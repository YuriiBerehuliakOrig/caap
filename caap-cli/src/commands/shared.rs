use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use caap_core::compiler::{CompilerHost, DiagnosticSink};
use caap_core::diagnostics::render_diagnostic;
use caap_core::error::{CaapError, CaapResult};
use caap_core::frontend::parse;
use caap_core::host::HostSystemPolicy;
use caap_core::semantic::PhasePolicy;
use caap_core::unit::Unit;
use caap_core::values::RuntimeValue;

/// The capability the CLI grants the bootstrap and the program. The CLI runs
/// trusted local code the user chose — the same trust as invoking any other
/// interpreter on a script — so it grants the root `sys` authority and leaves
/// attenuation to embedders (LSP/DAP) and to the bootstrap's own policy.
const CLI_CAPABILITY: &str = "sys";

/// Build the launch command the CLI evaluates. The protocol with the
/// bootstrap is one name: after the bootstrap executes, `cli.main` (if
/// registered) is called as `(cli.main program args)` and decides what
/// "running a program" means. Without `cli.main` the program is evaluated as
/// a bootstrap-style script itself — the bare-policy degenerate case (its
/// result value still flows back, and a failure re-raises the first captured
/// diagnostic). `cli.program` and `cli.args` are registered up front so even
/// that fallback can read its argv.
pub(super) fn launch_command_source(
    bootstrap: &str,
    program: &str,
    args: &[String],
) -> CaapResult<String> {
    let bootstrap_literal = caap_string_literal(bootstrap)?;
    let program_literal = caap_string_literal(program)?;
    let args_list = caap_string_list(args)?;
    let capability_list = caap_string_list(&[CLI_CAPABILITY.to_string()])?;
    Ok(format!(
        "(do \
           (ctfe_compiler_register_value compiler \"cli.program\" {program_literal}) \
           (ctfe_compiler_register_value compiler \"cli.args\" {args_list}) \
           (ctfe_compiler_execute_bootstrap_file compiler {bootstrap_literal} {capability_list}) \
           (bind ((cli_main (ctfe_compiler_lookup_value compiler \"cli.main\" null))) \
             (if (eq cli_main null) \
               (bind ((capture (ctfe_compiler_evaluate_bootstrap_file \
                                 compiler {program_literal} (map_of) {capability_list}))) \
                 (bind ((diagnostics (get capture \"diagnostics\" (list_of)))) \
                   (if (lt 0 (size diagnostics)) \
                     (bind ((first_diagnostic (get diagnostics 0 null))) \
                       (runtime_error \
                         (if (eq (value_type first_diagnostic) \"map\") \
                           (get first_diagnostic \"message\" \"program execution failed\") \
                           \"program execution failed\"))) \
                     (get capture \"result\" null)))) \
               (cli_main {program_literal} {args_list}))))"
    ))
}

/// Evaluate the launch command in a fresh compiler session. The compile-time
/// host policy is `allow_all`: the executing program IS the toolchain (it may
/// write artifacts and spawn `clang`), so sandboxing it would sandbox the
/// build itself.
pub(super) fn evaluate_launch_command(
    source: &str,
    diagnostic_sink: DiagnosticSink,
) -> CaapResult<RuntimeValue> {
    let mut host = CompilerHost::with_default_system_libraries(Vec::<PathBuf>::new())?;
    host.compile_time_services_mut()?
        .set_system_policy(HostSystemPolicy::allow_all());
    let mut compiler = host.new_session();
    compiler.set_diagnostic_sink(diagnostic_sink);
    let graph = parse(source)?;
    let unit = Unit::from_graph("cli.launch", graph)?;
    let value = compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])?;
    Ok(value)
}

pub(super) fn caap_string_literal(value: &str) -> CaapResult<String> {
    serde_json::to_string(value)
        .map_err(|error| CaapError::compiler(format!("failed to quote CAAP string: {error}")))
}

pub(super) fn caap_string_list(values: &[String]) -> CaapResult<String> {
    let mut items = Vec::with_capacity(values.len());
    for value in values {
        items.push(caap_string_literal(value)?);
    }
    Ok(format!("(list_of {})", items.join(" ")))
}

pub(super) fn canonical_path_string(path: &str) -> CaapResult<String> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| CaapError::host(format!("failed to resolve {}: {error}", path)))?;
    canonical
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| CaapError::host(format!("path is not valid UTF-8: {}", canonical.display())))
}

pub(super) fn read_input(path: &str) -> CaapResult<String> {
    if path == "-" {
        let mut source = String::new();
        io::stdin()
            .read_to_string(&mut source)
            .map_err(|error| CaapError::host(format!("failed to read stdin: {error}")))?;
        return Ok(source);
    }
    fs::read_to_string(path).map_err(|error| {
        CaapError::host(format!(
            "failed to read {}: {error}",
            Path::new(path).display()
        ))
    })
}

/// Build a diagnostic sink that renders and writes each diagnostic to stderr
/// immediately when emitted — before any runtime stdout output from the
/// program. A diagnostic can reach the session twice (streamed at emit time,
/// then re-published with a collected batch), so exact re-renders are skipped.
pub(super) fn make_streaming_diagnostic_sink() -> DiagnosticSink {
    use std::cell::Cell;
    use std::collections::BTreeSet;
    let first = Rc::new(Cell::new(true));
    let source_cache: Rc<RefCell<BTreeMap<String, Option<String>>>> =
        Rc::new(RefCell::new(BTreeMap::new()));
    let seen: Rc<RefCell<BTreeSet<String>>> = Rc::new(RefCell::new(BTreeSet::new()));
    DiagnosticSink::new(move |diagnostic| {
        let rendered = {
            let mut cache = source_cache.borrow_mut();
            if let Some(path) = diagnostic.span.as_ref().and_then(|s| s.path.as_ref()) {
                let source = cache
                    .entry(path.clone())
                    .or_insert_with(|| fs::read_to_string(path).ok());
                render_diagnostic(diagnostic, source.as_deref())
            } else {
                render_diagnostic(diagnostic, None)
            }
        };
        if !seen.borrow_mut().insert(rendered.clone()) {
            return;
        }
        let mut stderr = std::io::stderr();
        if first.get() {
            first.set(false);
        } else {
            let _ = writeln!(stderr);
        }
        let _ = writeln!(stderr, "{rendered}");
    })
}
