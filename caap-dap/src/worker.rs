//! The evaluation worker: builds a fresh compiler session, installs the debug
//! controller as a thread-local hook, and drives evaluation so every node flows
//! through the controller. Runs on its own thread because `Rc`/`RuntimeValue`
//! are not `Send`; only the channels (Send) connect it to the DAP loop.
//!
//! Three launch targets:
//! - `Bootstrap`: step the stdlib bootstrap itself (hook active throughout).
//! - `Source` / `Root`: load the stdlib **without** the hook, then install it
//!   and run the user's source file (`run_source`) or module
//!   (`run_from_root`) — so stepping starts in the user's code, and
//!   grammar-extended surface forms (which carry surface spans) are stepped at
//!   their surface positions, not the stdlib bootstrap. The run commands are
//!   resolved solely through stdlib's `caap.session.commands` capability map —
//!   see [`bind_session_command`].

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

use caap_core::compiler::{Compiler, CompilerHost};
use caap_core::error::CaapError;
use caap_core::frontend::parse;
use caap_core::semantic::PhasePolicy;
use caap_core::unit::Unit;

use crate::controller::{AbortEval, DebugController};
use crate::protocol::{DapEvent, DebugCommand};

/// What to debug.
#[derive(Clone, Debug)]
pub enum DebugTarget {
    /// Step the stdlib bootstrap load itself.
    Bootstrap,
    /// Run a single source file (the `run_source` session command).
    Source { path: PathBuf },
    /// Run a module from a root (the `run_from_root` session command), the path
    /// for grammar-extended programs that need module/syntax-import discovery.
    Root { root: PathBuf, entry: String },
}

/// Parameters for a debug launch.
#[derive(Clone, Debug)]
pub struct LaunchArgs {
    /// Stdlib bootstrap entry file (e.g. `<repo>/stdlib/bootstrap.caap`).
    pub bootstrap: PathBuf,
    /// Compile-time read roots (workspace, module root, program dir, …).
    pub read_roots: Vec<PathBuf>,
    pub target: DebugTarget,
    /// The user's program file; stepping/entry is confined to it.
    pub focus: Option<PathBuf>,
    pub stop_on_entry: bool,
}

pub fn spawn_worker(
    args: LaunchArgs,
    to_dap: Sender<DapEvent>,
    from_dap: Receiver<DebugCommand>,
    start: Receiver<()>,
) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("caap_ctfe_eval".to_string())
        .spawn(move || {
            let controller = Rc::new(RefCell::new(DebugController::new(
                to_dap.clone(),
                from_dap,
                args.stop_on_entry,
                args.focus.clone(),
            )));
            let to_dap_run = to_dap.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&args, &controller, &start, &to_dap_run)
            }));
            caap_core::debug::clear_hook();

            let terminated = match result {
                Ok(Ok(())) => DapEvent::Terminated { error: None },
                Ok(Err(message)) => DapEvent::Terminated {
                    error: Some(message),
                },
                Err(payload) => {
                    if payload.downcast_ref::<AbortEval>().is_some() {
                        DapEvent::Terminated { error: None }
                    } else {
                        DapEvent::Terminated {
                            error: Some("evaluator panicked".to_string()),
                        }
                    }
                }
            };
            let _ = to_dap.send(terminated);
        })
}

fn run(
    args: &LaunchArgs,
    controller: &Rc<RefCell<DebugController>>,
    start: &Receiver<()>,
    to_dap: &Sender<DapEvent>,
) -> Result<(), String> {
    let host =
        CompilerHost::with_default_system_libraries(read_roots(args)).map_err(|e| e.to_string())?;
    let mut compiler = host.new_session();

    let bootstrap = path_str(&args.bootstrap)?;
    let bootstrap_unit = format!(
        "(do (ctfe_compiler_execute_bootstrap_file compiler {}))",
        quote(bootstrap)?
    );

    match &args.target {
        DebugTarget::Bootstrap => {
            // Step the bootstrap itself.
            caap_core::debug::install_hook(controller.clone());
            let _ = start.recv();
            evaluate(&mut compiler, "caap.dap.bootstrap", &bootstrap_unit)
                .map_err(|e| e.to_string())
        }
        target => {
            // Load the stdlib silently (no hook), then step only the user code.
            output(to_dap, "Loading stdlib…\n");
            evaluate(&mut compiler, "caap.dap.bootstrap", &bootstrap_unit)
                .map_err(|e| e.to_string())?;
            caap_core::debug::install_hook(controller.clone());
            let _ = start.recv();
            let command = command_unit(target)?;
            output(to_dap, "Running…\n");
            evaluate(&mut compiler, "caap.dap.run", &command).map_err(|e| e.to_string())
        }
    }
}

/// Build the read roots: the configured roots plus the stdlib bootstrap's
/// directory so its files are always readable.
fn read_roots(args: &LaunchArgs) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(parent) = args.bootstrap.parent() {
        roots.push(parent.to_path_buf());
    }
    for root in &args.read_roots {
        if !roots.contains(root) {
            roots.push(root.clone());
        }
    }
    roots
}

fn command_unit(target: &DebugTarget) -> Result<String, String> {
    match target {
        DebugTarget::Bootstrap => unreachable!(),
        // Run the program: its surface forms (lowered to CTFE nodes that carry
        // surface spans) are evaluated, so stepping/breakpoints land on the
        // surface syntax. Program stdout is captured by the DAP server's
        // fd redirection, so runtime `io.println` does not corrupt the stream.
        DebugTarget::Source { path } => Ok(format!(
            "{}(command {}))",
            bind_session_command("run_source"),
            quote(path_str(path)?)?,
        )),
        DebugTarget::Root { root, entry } => Ok(format!(
            "{}(command {} {}))",
            bind_session_command("run_from_root"),
            quote(path_str(root)?)?,
            quote(entry)?,
        )),
    }
}

/// An open `(bind (…)` that binds `command` to a session command (`run_source`
/// or `run_from_root`); the caller closes it with the actual `(command …)` call.
///
/// Resolution goes through stdlib's authoritative capability map
/// `caap.session.commands` (`{version, run_source, run_from_root, …}`) — the
/// sole source. The v1-era `stdlib.module.*` registrations were removed with v1,
/// so there is no fallback: an absent map is a real contract violation (the
/// stdlib bootstrap was not run) and raises a clear error rather than silently
/// degrading. Kernel `bind` is sequential, so the second binding sees
/// `commands`; everything here is a kernel primitive, since this unit is
/// evaluated raw (without the stdlib expander).
fn bind_session_command(name: &str) -> String {
    format!(
        "(bind (\
           (commands (ctfe_compiler_lookup_value compiler \"caap.session.commands\" null)) \
           (command (if (eq commands null) \
                        (runtime_error \"caap.session.commands is absent — run the stdlib bootstrap before a DAP run target\") \
                        (get commands \"{name}\" null)))) "
    )
}

fn evaluate(compiler: &mut Compiler, unit_id: &str, source: &str) -> Result<(), CaapError> {
    let graph = parse(source)?;
    let unit = Unit::from_graph(unit_id, graph)?;
    compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])?;
    Ok(())
}

fn output(to_dap: &Sender<DapEvent>, text: &str) {
    let _ = to_dap.send(DapEvent::Output {
        category: "console".to_string(),
        text: text.to_string(),
    });
}

fn path_str(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn quote(value: &str) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("failed to quote string: {e}"))
}
