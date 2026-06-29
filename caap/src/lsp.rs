//! Public bootstrap helper for embedders (the LSP/DAP) that need to invoke
//! stdlib session commands without reimplementing the CLI orchestration.
//!
//! The session boots a stdlib `bootstrap.caap`, which registers the
//! `caap.session.commands` capability map (`analyze_source`,
//! `analyze_source_with_root`, …). Commands are invoked by capability key:
//!
//! ```ignore
//! let session = BootstrapSession::new(
//!     vec!["stdlib/bootstrap.caap".into()],
//!     vec![/* extra compile-time read roots */],
//! )?;
//! let value = session.invoke_named_command(
//!     "analyze_source",
//!     "/abs/path/to/file.caap",
//!     None,
//!     vec![],
//! )?;
//! ```
//!
//! The bootstrap is run once and the preloaded session is reused across calls
//! (only the command form is evaluated each time), so editor latency stays low.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::Value;

use crate::compiler::{Compiler, CompilerHost, DiagnosticSink};
use crate::diagnostics::Diagnostic;
use crate::error::{CaapError, CaapResult};
use crate::frontend::parse;
use crate::semantic::PhasePolicy;
use crate::unit::Unit;
use crate::values::{MapKey, RuntimeValue};

/// The authoritative session-command capability map a tooling-aware stdlib
/// registers: `{version, analyze_source, analyze_source_with_root, …}`. The LSP
/// resolves commands through it (one lookup, discoverable, versionable).
const CAPABILITY_MAP_NAME: &str = "caap.session.commands";

/// The bootstrap capability granted to the command unit. stdlib's analyze
/// reads the file header through the `fs` host service (to detect a
/// `(surface KIT)` declaration), which is gated on `sys.fs.read`; granting the
/// root `sys` authority — the same the CLI grants a program it runs — lets that
/// read succeed. Without it the analyze quietly falls back to its default path
/// and surface-headed files (e.g. the C-like dialect) fail to parse.
const SESSION_CAPABILITY: &str = "sys";

/// Long-lived analysis session: owns a `CompilerHost` configured with the
/// default runtime + compile-time system libraries plus user-provided
/// compile-time read roots.
///
/// To keep editor latency low, the session lazily builds **one** compiler
/// session with the stdlib bootstrap already executed and reuses it across
/// calls — running just the requested command form each time instead of
/// re-running the whole bootstrap (~seconds) on every keystroke. The cache is
/// only used when a call's extra read roots are already covered by the
/// session's configured roots; otherwise it falls back to a fresh,
/// per-call host (the original behavior).
pub struct BootstrapSession {
    bootstrap_paths: Vec<PathBuf>,
    read_roots: Vec<PathBuf>,
    /// Lazily-initialized preloaded session (stdlib bootstrap already run).
    /// `RefCell` because callers hold `&self`; the LSP drives this on a single
    /// thread.
    cached: RefCell<Option<Compiler>>,
    /// Diagnostics emitted by the compiler during command evaluation, collected
    /// via the session's diagnostic sink. Surfaced to the editor as real
    /// semantic diagnostics (with source spans).
    diagnostics: Rc<RefCell<Vec<Diagnostic>>>,
    /// Memoized result of probing whether the booted stdlib exposes a usable
    /// session-command surface (the `caap.session.commands` capability map).
    /// `None` until first probed. Lets the LSP degrade quietly when it is absent
    /// instead of evaluating an undefined call per file.
    commands_available: Cell<Option<bool>>,
}

impl BootstrapSession {
    pub fn new(bootstrap_paths: Vec<PathBuf>, read_roots: Vec<PathBuf>) -> CaapResult<Self> {
        Ok(Self {
            bootstrap_paths,
            read_roots,
            cached: RefCell::new(None),
            diagnostics: Rc::new(RefCell::new(Vec::new())),
            commands_available: Cell::new(None),
        })
    }

    /// Diagnostics the compiler emitted during the most recent
    /// `invoke_named_command`, draining the buffer.
    pub fn drain_diagnostics(&self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics.borrow_mut())
    }

    fn diagnostic_sink(&self) -> DiagnosticSink {
        let buffer = Rc::clone(&self.diagnostics);
        DiagnosticSink::new(move |diagnostic| buffer.borrow_mut().push(diagnostic.clone()))
    }

    /// Resolve `command_name` (a `caap.session.commands` capability key, e.g.
    /// `analyze_source`) in the bootstrapped compiler and invoke it with
    /// `input_path` and an optional `module_root`. Returns the runtime value the
    /// command produced (or a CAAP error if bootstrap / lookup / evaluation
    /// failed).
    ///
    /// `module_root` enables `--root`-style discovery in the command
    /// (`analyze_source_with_root`): when provided, the command scans that
    /// directory for local `(module "...")` declarations so import targets that
    /// live next to the file under analysis are resolvable.
    ///
    /// `extra_read_roots` are merged with the session's configured roots for
    /// the duration of this single call so the compiler is allowed to read
    /// files from `module_root` (and any sibling directory it scans).
    pub fn invoke_named_command(
        &self,
        command_name: &str,
        input_path: &str,
        module_root: Option<&str>,
        extra_read_roots: Vec<PathBuf>,
    ) -> CaapResult<RuntimeValue> {
        // Fresh diagnostics for this command. (Bootstrap-time or stdlib
        // diagnostics that linger are filtered out by file path on the LSP
        // side.)
        self.diagnostics.borrow_mut().clear();
        // Fast path: when every extra read root is already covered by the
        // session's configured roots (read roots match by ancestor prefix),
        // reuse the preloaded session and run only the command form.
        if self.extras_covered(&extra_read_roots) {
            return self.invoke_cached(command_name, input_path, module_root);
        }
        self.invoke_uncached(command_name, input_path, module_root, extra_read_roots)
    }

    /// `true` when each extra read root is a descendant of (or equal to) one of
    /// the session's configured roots, so the preloaded host already permits
    /// reading those files.
    fn extras_covered(&self, extra_read_roots: &[PathBuf]) -> bool {
        extra_read_roots
            .iter()
            .all(|extra| self.read_roots.iter().any(|root| extra.starts_with(root)))
    }

    /// Reuse the preloaded session, evaluating only the command form.
    fn invoke_cached(
        &self,
        command_name: &str,
        input_path: &str,
        module_root: Option<&str>,
    ) -> CaapResult<RuntimeValue> {
        self.ensure_session()?;
        // Discard any diagnostics emitted while building the session (stdlib
        // bootstrap); only this command's diagnostics should surface.
        self.diagnostics.borrow_mut().clear();
        let source = self.command_only_source(command_name, input_path, module_root)?;
        let mut cached = self.cached.borrow_mut();
        let compiler = cached.as_mut().ok_or_else(|| {
            CaapError::compiler("bootstrap session cache was not initialized after ensure_session")
        })?;
        // Run the command as a capability-granted bootstrap unit so stdlib's
        // analyze may read file headers through the `fs` host service.
        Ok(compiler.bootstrap().execute_text_with_capability_scope(
            source,
            "caap.lsp.bootstrap_command",
            [SESSION_CAPABILITY.to_string()],
        )?)
    }

    /// Original behavior: build a fresh host (with the per-call extra roots
    /// merged) and run bootstrap + command together.
    fn invoke_uncached(
        &self,
        command_name: &str,
        input_path: &str,
        module_root: Option<&str>,
        extra_read_roots: Vec<PathBuf>,
    ) -> CaapResult<RuntimeValue> {
        let host = self.host_with_roots(extra_read_roots)?;
        let mut compiler = host.new_session();
        compiler.set_diagnostic_sink(self.diagnostic_sink());
        let source = self.synthesize_command_source(command_name, input_path, module_root)?;
        Ok(compiler.bootstrap().execute_text_with_capability_scope(
            source,
            "caap.lsp.bootstrap_command",
            [SESSION_CAPABILITY.to_string()],
        )?)
    }

    /// Build (once) the preloaded session: a host with the configured read
    /// roots and the stdlib bootstrap files executed. Subsequent commands run
    /// against this session, so the bootstrap is paid for only once.
    fn ensure_session(&self) -> CaapResult<()> {
        if self.cached.borrow().is_some() {
            return Ok(());
        }
        let host = self.host_with_roots(Vec::new())?;
        let mut compiler = host.new_session();
        compiler.set_diagnostic_sink(self.diagnostic_sink());
        let source = self.bootstrap_only_source()?;
        // Load the bootstrap under the capability scope too: stdlib's loader
        // captures the bridge of the unit that loads it, so a later analyze can
        // only read file headers through the `fs` host service when the loader
        // itself was loaded with that authority granted.
        compiler.bootstrap().execute_text_with_capability_scope(
            source,
            "caap.lsp.bootstrap_preload",
            [SESSION_CAPABILITY.to_string()],
        )?;
        *self.cached.borrow_mut() = Some(compiler);
        Ok(())
    }

    fn host_with_roots(&self, extra_read_roots: Vec<PathBuf>) -> CaapResult<CompilerHost> {
        let mut roots = self.read_roots.clone();
        for root in extra_read_roots {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        let mut host = CompilerHost::new();
        host.register_default_runtime_system_libraries()?;
        host.register_default_compile_time_system_libraries_with_read_roots(roots)?;
        Ok(host)
    }

    /// `(do <execute-bootstrap-file ...> <command call>)` — used by the
    /// fresh-host fallback path.
    fn synthesize_command_source(
        &self,
        command_name: &str,
        input_path: &str,
        module_root: Option<&str>,
    ) -> CaapResult<String> {
        let mut forms = self.bootstrap_forms()?;
        forms.push(self.command_form(command_name, input_path, module_root)?);
        Ok(format!("(do {})", forms.join(" ")))
    }

    /// Just the `(ctfe-compiler-execute-bootstrap-file ...)` forms — run once to
    /// preload the cached session.
    fn bootstrap_only_source(&self) -> CaapResult<String> {
        let forms = self.bootstrap_forms()?;
        Ok(format!("(do {})", forms.join(" ")))
    }

    /// Just the command lookup + call — run against the already-preloaded
    /// cached session.
    fn command_only_source(
        &self,
        command_name: &str,
        input_path: &str,
        module_root: Option<&str>,
    ) -> CaapResult<String> {
        Ok(format!(
            "(do {})",
            self.command_form(command_name, input_path, module_root)?
        ))
    }

    fn bootstrap_forms(&self) -> CaapResult<Vec<String>> {
        self.bootstrap_paths
            .iter()
            .map(|path| {
                Ok(format!(
                    "(ctfe_compiler_execute_bootstrap_file compiler {})",
                    caap_string_literal(path_to_str(path)?)?
                ))
            })
            .collect()
    }

    fn command_form(
        &self,
        command_name: &str,
        input_path: &str,
        module_root: Option<&str>,
    ) -> CaapResult<String> {
        // Match the invoked command's arity: pass the module root as a second
        // argument only when one is supplied. The 1-ary `analyze_source` takes a
        // single `path`; `analyze_source_with_root` takes `path root`. Emitting a
        // trailing arg unconditionally would overflow the 1-ary lambda.
        let call = match module_root {
            Some(root) => format!(
                "(command {} {})",
                caap_string_literal(input_path)?,
                caap_string_literal(root)?,
            ),
            None => format!("(command {})", caap_string_literal(input_path)?),
        };
        // `command_name` is a capability key (e.g. `analyze_source`) into the
        // `caap.session.commands` map. `ctfe_compiler_lookup_value` errors on a
        // missing name unless given a default, so pass an explicit `null` (the
        // map's presence is gated by `supports_command` before we get here).
        Ok(format!(
            "(bind ((command (get (ctfe_compiler_lookup_value compiler {caps} null) {key} null))) \
             {call})",
            caps = caap_string_literal(CAPABILITY_MAP_NAME)?,
            key = caap_string_literal(command_name)?,
        ))
    }

    /// Whether the booted session can serve the given capability key (e.g.
    /// `analyze_source`) via the `caap.session.commands` map. Memoized: the first
    /// call builds the session (the LSP pays that cost on the first analyze
    /// anyway); afterwards it is a cheap cached read. When this returns `false`
    /// (a bootstrap that ships no command map), the LSP skips augmentation
    /// rather than evaluating
    /// an undefined-call error per file.
    pub fn supports_command(&self, command_name: &str) -> bool {
        if let Some(known) = self.commands_available.get() {
            return known;
        }
        let available = self.probe_command(command_name).unwrap_or(false);
        self.commands_available.set(Some(available));
        available
    }

    fn probe_command(&self, command_name: &str) -> CaapResult<bool> {
        self.ensure_session()?;
        // Don't let the probe's evaluation leak into the next command's drain.
        self.diagnostics.borrow_mut().clear();
        let source = format!(
            "(do (not (eq (get (ctfe_compiler_lookup_value compiler {caps} null) {key} null) null)))",
            caps = caap_string_literal(CAPABILITY_MAP_NAME)?,
            key = caap_string_literal(command_name)?,
        );
        let graph = parse(&source)?;
        let unit = Unit::from_graph("caap.lsp.capability_probe", graph)?;
        let mut cached = self.cached.borrow_mut();
        let compiler = cached.as_mut().ok_or_else(|| {
            CaapError::compiler("bootstrap session cache was not initialized after ensure_session")
        })?;
        let value = compiler
            .evaluation()
            .evaluate(&unit, PhasePolicy::CompileTime, [])?;
        self.diagnostics.borrow_mut().clear();
        Ok(matches!(value, RuntimeValue::Bool(true)))
    }
}

fn path_to_str(path: &Path) -> CaapResult<&str> {
    path.to_str().ok_or_else(|| {
        CaapError::host(format!(
            "bootstrap path is not valid UTF-8: {}",
            path.display()
        ))
    })
}

fn caap_string_literal(value: &str) -> CaapResult<String> {
    serde_json::to_string(value)
        .map_err(|error| CaapError::compiler(format!("failed to quote CAAP string: {error}")))
}

/// Convert a `RuntimeValue` produced by a CAAP command into a JSON-shaped
/// `serde_json::Value` for embedders that want to traverse the result with
/// the standard serde APIs (e.g., LSP servers building protocol messages).
pub fn runtime_value_to_json(value: &RuntimeValue) -> Value {
    use serde_json::{Map, Number};
    match value {
        RuntimeValue::Null => Value::Null,
        RuntimeValue::Bool(value) => Value::Bool(*value),
        RuntimeValue::Int(value) => Value::Number((*value).into()),
        RuntimeValue::Float(value) => Number::from_f64(*value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        RuntimeValue::Str(value) => Value::String(value.to_string()),
        RuntimeValue::Tuple(items) => {
            Value::Array(items.iter().map(runtime_value_to_json).collect())
        }
        RuntimeValue::List(items) => {
            Value::Array(items.borrow().iter().map(runtime_value_to_json).collect())
        }
        RuntimeValue::Map(entries) => {
            let entries = entries.borrow();
            let mut map = Map::new();
            for (key, value) in entries.iter() {
                let key_string = match key {
                    MapKey::Str(value) => value.to_string(),
                    MapKey::Int(value) => value.to_string(),
                    MapKey::Bool(value) => value.to_string(),
                    MapKey::Null => "null".to_string(),
                };
                map.insert(key_string, runtime_value_to_json(value));
            }
            Value::Object(map)
        }
        other => Value::String(format!("{other}")),
    }
}
