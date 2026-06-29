//! Shared fixtures for the integration test suite. Each `tests/*.rs` is its own
//! crate, so helpers live here and are pulled in with `mod common;`. Not every
//! binary uses every helper, hence the module-wide `allow(dead_code)`.
#![allow(dead_code)]

use std::cell::RefCell;

use caap_core::frontend::parse;
use caap_core::{Compiler, CompilerHost, PhasePolicy, RuntimeValue, Unit};

/// A fresh compiler session (owned; the host is cloned into it).
pub fn session() -> Compiler {
    CompilerHost::new().new_session()
}

fn stdlib_bootstrap_path() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../stdlib/bootstrap.caap")
        .display()
        .to_string()
}

thread_local! {
    static BOOTSTRAPPED: RefCell<Option<Compiler>> = const { RefCell::new(None) };
}

/// A compiler with `stdlib/bootstrap.caap` already executed.
///
/// Re-running the bootstrap costs ~1s; doing it per test is the suite's main
/// bottleneck. Here it runs ONCE per test thread and every call returns a cheap
/// `Rc`-copy-on-write clone, so tests skip the bootstrap. The evaluated body must
/// therefore NOT re-run `(ctfe_compiler_execute_bootstrap_file …)` — the expander,
/// loader and type layer are already registered in the clone; just look them up
/// (`(ctfe_compiler_lookup_value compiler "stdlib.expand")`, `"stdlib.load"`, …).
///
/// Isolation: each clone is COW, so registering values / declaring roots / loading
/// modules in a test does not leak into sibling tests. Tests that assert on the
/// bootstrap/session LIFECYCLE itself must keep using a fresh [`session`].
pub fn bootstrapped_session() -> Compiler {
    BOOTSTRAPPED.with(|cell| {
        if cell.borrow().is_none() {
            let mut compiler = CompilerHost::new().new_session();
            let src = format!(
                "(ctfe_compiler_execute_bootstrap_file compiler {:?})",
                stdlib_bootstrap_path()
            );
            let graph = parse(&src).expect("parse cached bootstrap");
            let unit = Unit::from_graph("cached_stdlib_bootstrap", graph).expect("bootstrap unit");
            compiler
                .evaluation()
                .evaluate(&unit, PhasePolicy::CompileTime, [])
                .expect("cached stdlib bootstrap");
            *cell.borrow_mut() = Some(compiler);
        }
        cell.borrow().as_ref().unwrap().clone()
    })
}

thread_local! {
    static CODEGEN_BOOTSTRAPPED: RefCell<Option<Compiler>> = const { RefCell::new(None) };
}

/// A compiler with `stdlib/bootstrap.caap` AND `stdlib/boot/native_emit.caap`
/// already executed (the composed native/codegen layer).
///
/// On top of the ~1s base bootstrap, `boot/native_emit.caap` eagerly `(load …)`s
/// the ~6400-line codegen layer (render/ir/prep/emit.llvm/emit.wasm/surface/driver/
/// clike) and registers peval — ~7-8s EACH if a native test re-runs the two-stage
/// composition. Here that composed session is built ONCE per test thread and every
/// call returns a cheap `Rc`-copy-on-write clone, so native tests skip BOTH stages.
/// The evaluated body must therefore NOT re-run either
/// `(ctfe_compiler_execute_bootstrap_file …)` — the codegen modules and their
/// registered values (`stdlib.frontend.clike`, `stdlib.syntax.render`,
/// `stdlib.backend.emit.llvm`, `stdlib.llvm.emit`, …) are already present in the
/// clone; just look them up (`(ctfe_compiler_lookup_value compiler "…")`).
///
/// Isolation: like [`bootstrapped_session`], each clone is COW so registering
/// values / declaring roots / loading modules in a test does not leak into
/// siblings; peval is already registered, and its registration is idempotent.
/// Tests that assert on the bootstrap/session LIFECYCLE itself must keep using a
/// fresh [`session`].
pub fn codegen_bootstrapped_session() -> Compiler {
    CODEGEN_BOOTSTRAPPED.with(|cell| {
        if cell.borrow().is_none() {
            let mut compiler = CompilerHost::new().new_session();
            let src = format!(
                "(do
                   (ctfe_compiler_execute_bootstrap_file compiler {:?})
                   (ctfe_compiler_execute_bootstrap_file compiler {:?}))",
                stdlib_bootstrap_path(),
                stdlib_path("boot/native_emit.caap")
            );
            let graph = parse(&src).expect("parse cached codegen bootstrap");
            let unit = Unit::from_graph("cached_codegen_bootstrap", graph)
                .expect("codegen bootstrap unit");
            compiler
                .evaluation()
                .evaluate(&unit, PhasePolicy::CompileTime, [])
                .expect("cached stdlib codegen bootstrap");
            *cell.borrow_mut() = Some(compiler);
        }
        cell.borrow().as_ref().unwrap().clone()
    })
}

/// Compile-time-evaluate `src` through a fresh session, returning its value.
pub fn eval_ct(src: &str) -> RuntimeValue {
    eval_ct_unit("test", src)
}

/// Like [`eval_ct`] but with an explicit unit id (when the test asserts on it).
pub fn eval_ct_unit(unit_id: &str, src: &str) -> RuntimeValue {
    let mut compiler = session();
    let graph = parse(src).expect("parse source");
    let unit = Unit::from_graph(unit_id, graph).expect("build unit");
    compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect("compile-time evaluation")
}

/// Compile-time-evaluate `src`, returning the error message on failure.
pub fn eval_ct_err(src: &str) -> String {
    let mut compiler = session();
    let graph = parse(src).expect("parse source");
    let unit = Unit::from_graph("test", graph).expect("build unit");
    compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect_err("expected compile-time evaluation error")
        .to_string()
}

/// Run `src` on a bare compile-time evaluator (no session), returning its value.
pub fn run_ct(src: &str) -> RuntimeValue {
    let graph = parse(src).expect("parse source");
    let mut ev = caap_core::Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    ev.run().expect("compile-time run")
}

/// Like [`run_ct`] but returns the error message on failure.
pub fn run_ct_err(src: &str) -> String {
    let graph = parse(src).expect("parse source");
    let mut ev = caap_core::Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    ev.run()
        .expect_err("expected compile-time run error")
        .to_string()
}

// --- stdlib scenario helpers (shared across the stdlib_*_tests.rs scenario files) ---

pub fn stdlib_bootstrap() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../stdlib/bootstrap.caap")
        .display()
        .to_string()
}

pub fn stdlib_path(rel: &str) -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../stdlib")
        .join(rel)
        .display()
        .to_string()
}

/// The test corpus lives at the workspace root, not in the library tree:
/// runnable example modules under `examples/`, deliberately broken fixtures
/// under `tests/`. A `"fixtures/…"` argument resolves into `tests/…`; anything
/// else (`"examples/…"`) resolves as-is under the workspace root. Module names
/// keep their `stdlib.examples.*` / `stdlib.fixtures.*` spelling — the
/// preambles below declare roots mapping those prefixes to these dirs.
pub fn corpus_path(rel: &str) -> String {
    let mapped = match rel.strip_prefix("fixtures") {
        Some(rest) => format!("tests{rest}"),
        None => rel.to_string(),
    };
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(mapped)
        .display()
        .to_string()
}

/// Evaluate a stdlib program at compile time (the path the loader uses) and
/// return the result. The program is wrapped so `compiler` is in scope and the
/// expander is already bootstrapped; `body` is spliced as the final expression.
pub fn eval_with_expander(body: &str) -> RuntimeValue {
    let mut compiler = bootstrapped_session();
    let source = format!(
        "(bind (
             (ex       (ctfe_compiler_lookup_value compiler \"stdlib.expand\"))
             (expand   (get ex \"expand\" null))
             (expand_wd (get ex \"expand_with_diagnostics\" null))
             (slit     (lambda (v) (syntax_literal v)))
             (snm      (lambda (n) (syntax_name n)))
             (scall    (lambda (callee args) (syntax_call callee args)))
           )
             {body})"
    );
    let graph = parse(&source).expect("parse failed");
    let unit = Unit::from_graph("stdlib_expander", graph).expect("unit failed");
    compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect("compile-time evaluation failed")
}

/// Like eval_with_expander but returns the error message (for diagnostics tests).
pub fn eval_with_expander_err(body: &str) -> String {
    let mut compiler = bootstrapped_session();
    let source = format!(
        "(bind (
             (ex     (ctfe_compiler_lookup_value compiler \"stdlib.expand\"))
             (expand (get ex \"expand\" null))
             (slit   (lambda (v) (syntax_literal v)))
             (snm    (lambda (n) (syntax_name n)))
             (scall  (lambda (callee args) (syntax_call callee args)))
           )
             {body})"
    );
    let graph = parse(&source).expect("parse failed");
    let unit = Unit::from_graph("stdlib_expander_err", graph).expect("unit failed");
    compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect_err("expected an expansion error")
        .to_string()
}

/// Like with_examples_root, but roots for the whole tree: library modules
/// ("stdlib.lib.*") resolve into stdlib/, while the corpus prefixes
/// ("stdlib.examples.*" / "stdlib.fixtures.*") resolve here in the test
/// corpus. The specific prefixes are declared FIRST — the loader picks the
/// first matching root.
pub fn with_stdlib_root(body: &str) -> String {
    let base = stdlib_path("");
    let examples = corpus_path("examples");
    let fixtures = corpus_path("fixtures");
    format!(
        "(bind (
             (api          (ctfe_compiler_lookup_value compiler \"stdlib.load\"))
             (load         (get api \"load\" null))
             (load_module  (get api \"load_module\" null))
             (declare_root (get api \"declare_root\" null))
           )
             (do
               (declare_root \"stdlib.examples\" {examples:?})
               (declare_root \"stdlib.fixtures\" {fixtures:?})
               (declare_root \"stdlib\" {base:?})
               {body}))"
    )
}

pub fn eval_ok(name: &str, source: &str) -> RuntimeValue {
    let mut compiler = bootstrapped_session();
    let graph = parse(source).expect("parse failed");
    let unit = Unit::from_graph(name, graph).expect("unit failed");
    compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect("compile-time evaluation failed")
}

pub fn eval_err_msg(name: &str, source: &str) -> String {
    let mut compiler = bootstrapped_session();
    let graph = parse(source).expect("parse failed");
    let unit = Unit::from_graph(name, graph).expect("unit failed");
    compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect_err("expected an error")
        .to_string()
}

/// Run `body` (already wrapped by [`with_stdlib_root`]) on a fresh session that
/// carries the compile-time `fs` service, with the bootstrap and the body in
/// ONE `sys`-granted unit — the same shape as the CLI launch command.
///
/// Why not reuse the cached [`bootstrapped_session`]? The loader's surface
/// helpers (`surface_of` / `surface_read`) read the file header through the
/// `fs` service, whose capability resolves against the unit executing the
/// bootstrap. A cached-and-cloned bootstrap loses that context, so the header
/// read is denied and every file looks headerless — surface dispatch can't be
/// exercised. Running bootstrap+body together under one `sys` grant fixes that.
/// This pays the bootstrap cost (~1s); used only by the surface-dispatch tests.
fn eval_fs_unit(name: &str, body: &str) -> Result<RuntimeValue, String> {
    use caap_core::{CompilerHost, HostSystemPolicy};
    let mut host = CompilerHost::with_default_system_libraries(Vec::<std::path::PathBuf>::new())
        .expect("host with default system libraries");
    host.compile_time_services_mut()
        .expect("compile-time services")
        .set_system_policy(HostSystemPolicy::allow_all());
    let mut compiler = host.new_session();
    let src = format!(
        "(do (ctfe_compiler_execute_bootstrap_file compiler {:?} (list_of \"sys\")) {body})",
        stdlib_bootstrap_path()
    );
    compiler
        .bootstrap()
        .execute_text_with_capabilities(src, name, ["sys".to_string()])
        .map_err(|signal| signal.to_string())
}

/// [`eval_ok`] but on an `fs`-enabled, `sys`-granted session that can read
/// surface headers (see [`eval_fs_unit`]).
pub fn eval_ok_fs(name: &str, body: &str) -> RuntimeValue {
    eval_fs_unit(name, body).expect("compile-time evaluation failed")
}

/// [`eval_err_msg`] but on an `fs`-enabled, `sys`-granted session (see
/// [`eval_fs_unit`]).
pub fn eval_err_msg_fs(name: &str, body: &str) -> String {
    eval_fs_unit(name, body).expect_err("expected an error")
}
