//! Sys-facade + session-command scenarios: facade verification against the
//! live caap-sys catalog, grants/declaration-only behaviour, sys call-site
//! type checking, and the cross-module analyze session command.
use caap_core::{frontend::parse, CompilerHost, PhasePolicy, RuntimeValue, Unit};

mod common;

use common::{corpus_path, eval_err_msg, eval_ok, stdlib_bootstrap, stdlib_path, with_stdlib_root};

/// verify_facade (pure): a facade's TYPED declaration is diffed against a host
/// catalog. Covers the drift cases — incl. param-type and return-type mismatch.
#[test]
fn stdlib_verify_facade_diffs_declaration_against_catalog() {
    let verify = stdlib_path("sys/verify.caap");
    // run a body with verify_facade / op / p in scope
    let run = |body: &str| -> String {
        with_stdlib_root(&format!(
            "(bind (
               (m      (load {verify:?}))
               (verify (get m \"verify_facade\" null))
               (op     (get m \"op\" null))
               (p      (get m \"p\" null))
             )
               {body})"
        ))
    };
    // one catalog op `a : (value: any) -> null`, impure
    let cat = "(list_of (assoc (map_of) \"public\" \"a\" \"effect\" \"impure\" \"signature\" \
        (assoc (map_of) \"params\" (list_of (assoc (map_of) \"name\" \"value\" \"type\" \"any\")) \"result\" \"null\")))";

    assert_eq!(
        eval_ok("vf_ok", &run(&format!(
            "(verify {cat} (list_of (op \"a\" (list_of (p \"value\" \"any\")) \"null\" \"impure\")) \"io\")"))),
        RuntimeValue::Bool(true)
    );
    assert!(eval_err_msg(
        "vf_phantom",
        &run(&format!(
            "(verify {cat} (list_of (op \"b\" (list_of) \"null\" \"impure\")) \"io\")"
        ))
    )
    .contains("not provided"));
    // param TYPE mismatch (the thing arity-only could not catch)
    assert!(eval_err_msg("vf_type", &run(&format!(
        "(verify {cat} (list_of (op \"a\" (list_of (p \"value\" \"int\")) \"null\" \"impure\")) \"io\")")))
        .contains("type"));
    // return-type mismatch
    assert!(eval_err_msg("vf_result", &run(&format!(
        "(verify {cat} (list_of (op \"a\" (list_of (p \"value\" \"any\")) \"int\" \"impure\")) \"io\")")))
        .contains("returns"));
    // coverage: a runtime op the facade forgot
    let cat2 = "(list_of \
        (assoc (map_of) \"public\" \"a\" \"effect\" \"impure\" \"signature\" (assoc (map_of) \"params\" (list_of) \"result\" \"null\")) \
        (assoc (map_of) \"public\" \"b\" \"effect\" \"impure\" \"signature\" (assoc (map_of) \"params\" (list_of) \"result\" \"null\")))";
    assert!(eval_err_msg(
        "vf_cover",
        &run(&format!(
            "(verify {cat2} (list_of (op \"a\" (list_of) \"null\" \"impure\")) \"io\")"
        ))
    )
    .contains("missing declaration"));
}

/// Build a session WITH the default system libraries (so the host catalog is
/// populated), bootstrap stdlib, declare the sys facades, and run `body`.
fn eval_with_sys(name: &str, body: &str) -> Result<RuntimeValue, String> {
    let mut compiler = CompilerHost::with_default_system_libraries(vec![])
        .expect("system libraries")
        .new_session();
    let bootstrap = stdlib_bootstrap();
    let verify = stdlib_path("sys/verify.caap");
    let io = stdlib_path("sys/io.caap");
    let os = stdlib_path("sys/os.caap");
    let base = stdlib_path("");
    let src = format!(
        "(do
           (ctfe_compiler_execute_bootstrap_file compiler {bootstrap:?})
           (bind (
             (api     (ctfe_compiler_lookup_value compiler \"stdlib.load\"))
             (load    (get api \"load\" null))
             (declare (get api \"declare\" null))
             (declare_root (get api \"declare_root\" null))
           )
             (do
               (declare_root \"stdlib\" {base:?})
               (declare \"stdlib.sys.verify\" {verify:?})
               (declare \"sys.io\" {io:?})
               (declare \"sys.os\" {os:?})
               {body})))"
    );
    let graph = parse(&src).expect("parse");
    let unit = Unit::from_graph(name, graph).expect("unit");
    compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .map_err(|e| e.to_string())
}

/// Like eval_with_sys, but first runs boot/sys_grants.caap WITH the sys.io
/// capability — the embedder's opt-in step that makes facades callable.
fn eval_with_sys_grants(name: &str, body: &str) -> Result<RuntimeValue, String> {
    let grants = stdlib_path("boot/sys_grants.caap");
    eval_with_sys(
        name,
        &format!(
            "(do
               (ctfe_compiler_execute_bootstrap_file compiler {grants:?} (list_of \"sys\"))
               {body})"
        ),
    )
}

/// THE milestone: a stdlib-built facade prints for real — in BOTH phases.
/// Sys exports are dual-phase (the per-phase sandbox policy, not a phase gate,
/// decides behaviour), so stage 1 calls println directly at compile time, and
/// stage 2 proves the same wrapper also runs in a runtime-phase evaluation
/// (received as an initial binding on the same session).
#[test]
fn stdlib_sys_io_is_callable_with_grants() {
    let mut compiler = CompilerHost::with_default_system_libraries(vec![])
        .expect("system libraries")
        .new_session();
    let bootstrap = stdlib_bootstrap();
    let grants = stdlib_path("boot/sys_grants.caap");
    let verify = stdlib_path("sys/verify.caap");
    let io = stdlib_path("sys/io.caap");
    let base = stdlib_path("");

    // Stage 1 (compile time): bootstrap, mint grants, load the facade, hand the
    // wrappers out as plain values.
    let src = format!(
        "(do
           (ctfe_compiler_execute_bootstrap_file compiler {bootstrap:?})
           (ctfe_compiler_execute_bootstrap_file compiler {grants:?} (list_of \"sys\"))
           (bind (
             (api     (ctfe_compiler_lookup_value compiler \"stdlib.load\"))
             (declare (get api \"declare\" null))
             (declare_root (get api \"declare_root\" null))
             (load_module (get api \"load_module\" null))
           )
             (do
               (declare_root \"stdlib\" {base:?})
               (declare \"stdlib.sys.verify\" {verify:?})
               (declare \"sys.io\" {io:?})
               (bind (
                 (m (load_module \"sys.io\"))
                 (println (get m \"println\" null))
               )
                 (do
                   (println \"hello from stdlib sys.io (compile time)\")
                   (list_of println (get m \"write\" null)))))))"
    );
    let graph = parse(&src).expect("parse stage 1");
    let unit = Unit::from_graph("sys_io_stage1", graph).expect("unit stage 1");
    let value = compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect("stage 1 (build wrappers) failed");
    let RuntimeValue::List(items) = value else {
        panic!("expected list, got {value:?}")
    };
    let (println_fn, write_fn) = {
        let items = items.borrow();
        (items[0].clone(), items[1].clone())
    };
    assert!(
        !matches!(println_fn, RuntimeValue::Null),
        "println wrapper must exist under grants"
    );

    // Stage 2 (runtime): call the wrappers — hello world, through stdlib.
    let graph = parse(
        "(do
           (io_println \"hello from stdlib sys.io\")
           (io_write \"written via stdlib\\n\"))",
    )
    .expect("parse stage 2");
    let unit = Unit::from_graph("sys_io_stage2", graph).expect("unit stage 2");
    let result = compiler
        .evaluation()
        .evaluate(
            &unit,
            PhasePolicy::Runtime,
            [
                ("io_println".to_string(), println_fn),
                ("io_write".to_string(), write_fn),
            ],
        )
        .expect("runtime call through the facade failed");
    assert_eq!(result, RuntimeValue::Null, "write returns null");
}

/// A facade called directly at compile time: os.platform is pure and
/// dual-phase, so the granted wrapper returns a non-empty string in CTFE.
#[test]
fn stdlib_sys_os_platform_callable_at_compile_time() {
    let v = eval_with_sys_grants(
        "sys_os_ct",
        "(bind (
           (load_module (get api \"load_module\" null))
           (platform (get (load_module \"sys.os\") \"platform\" null))
         )
           (gt (size (platform)) 0))",
    )
    .expect("os.platform must be callable at compile time");
    assert_eq!(v, RuntimeValue::Bool(true));
}

/// Without a grant the facade is in declaration-only mode: it still loads and
/// its typed surface is verified, but each wrapper is a NON-null self-describing
/// throwing stub — calling `println` raises "… requires a sys grant …" rather
/// than returning a silent null that would later fail as "not callable".
#[test]
fn stdlib_sys_io_is_declaration_only_without_grants() {
    let v = eval_with_sys(
        "sys_io_decl_only",
        "(bind ((load_module (get api \"load_module\" null))
                (println (get (load_module \"sys.io\") \"println\" null)))
           (do
             (if (eq println null)
               (runtime_error \"println must be a stub, not null\") null)
             (try (do (println \"x\") false)
               (catch e (string_contains (value_to_string e) \"requires a sys grant\")))))",
    )
    .expect("ungranted facade must still load");
    assert_eq!(v, RuntimeValue::Bool(true));
}

/// Integration: verify_sys — the ONE startup entry point — checks every declared
/// facade against the REAL caap-sys catalog (typed params, results, effects,
/// coverage). This is exactly the call an embedder makes after bootstrap.
#[test]
fn stdlib_verify_sys_checks_all_facades_against_caap_sys() {
    let sys_dir = stdlib_path("sys");
    let v = eval_with_sys(
        "sys_verify_all",
        &format!(
            "(bind (
               (load_module (get api \"load_module\" null))
               (verify_sys (get (load_module \"stdlib.sys.verify\") \"verify_sys\" null))
             )
               (verify_sys {sys_dir:?}))"
        ),
    )
    .expect("verify_sys must pass against the live catalog");
    assert_eq!(v, RuntimeValue::Bool(true));
}

/// sys facades file their TYPED surface with the type pass (declare_ops!):
/// a wrong argument type at an importer's call site is a load-time error —
/// system calls are checked like any defn.
#[test]
fn stdlib_sys_call_sites_are_type_checked() {
    let bad = corpus_path("fixtures/bad_sys_arg.caap");
    let msg = eval_with_sys("bad_sys_arg", &format!("(load {bad:?})"))
        .expect_err("a mistyped sys call must fail the load");
    assert!(
        msg.contains("`write` arg 1: expected string, got int"),
        "msg: {msg}"
    );
}

/// The catalog effect rides the declared signature: const refuses an impure
/// sys call by NAME (not just \"cannot prove\").
#[test]
fn stdlib_const_rejects_impure_sys_call() {
    let bad = corpus_path("fixtures/const_sys_impure.caap");
    let msg = eval_with_sys("const_sys_impure", &format!("(load {bad:?})"))
        .expect_err("const over println must be refused");
    assert!(
        msg.contains("const requires a pure expression (found impure `println`)"),
        "msg: {msg}"
    );
}

/// Integration negative: a truncated facade (one op dropped) fails against the
/// real catalog — proving startup verification catches real drift.
#[test]
fn stdlib_sys_io_facade_drift_is_caught() {
    let err = eval_with_sys(
        "sys_io_drift",
        "(bind (
           (load_module (get api \"load_module\" null))
           (verify (get (load_module \"stdlib.sys.verify\") \"verify_facade\" null))
           (ops (get (load_module \"sys.io\") \"ops\" null))
         )
           (verify (host_service_library_catalog \"io\" \"runtime\")
                   (sequence_take ops 9) \"io\"))",
    )
    .expect_err("dropping an op must fail verification");
    assert!(err.contains("missing declaration"), "err: {err}");
}

/// B3 — cross-module go-to-definition: analyze_source_with_root resolves
/// each use-imported name to its DEFINITION SITE in the sibling module
/// (file path + span), declaring the project root so a.b -> root/b.caap.
#[test]
fn stdlib_analyze_with_root_resolves_imports() {
    let app = corpus_path("fixtures/proj/app.caap");
    let root = corpus_path("fixtures/proj");
    let v = eval_ok(
        "analyze_root",
        &with_stdlib_root(&format!(
            "(bind ((caps (ctfe_compiler_lookup_value compiler
                            \"caap.session.commands\")))
               (bind ((r ((get caps \"analyze_source_with_root\" null)
                          {app:?} {root:?})))
                 (bind ((imps (get r \"imports\" (list_of))))
                   (list_of
                     (size imps)
                     (get (get imps 0 (map_of)) \"name\" \"\")
                     (get (get imps 0 (map_of)) \"module\" \"\")
                     (get (get (get imps 0 (map_of)) \"span\" (map_of))
                          \"start_line\" 0)))))"
        )),
    );
    let RuntimeValue::List(items) = v else {
        panic!("got {v:?}")
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1), "one import resolved");
    assert_eq!(
        items[1],
        RuntimeValue::Str("helper".into()),
        "imported name"
    );
    assert_eq!(
        items[2],
        RuntimeValue::Str("proj.lib".into()),
        "source module"
    );
    assert_eq!(
        items[3],
        RuntimeValue::Int(3),
        "definition span points at helper's line in lib.caap"
    );
}
