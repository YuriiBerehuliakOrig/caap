//! Pass/diag/misc scenarios: declarative text rewriting, lib loading, the
//! semantic name/arity checker, user-defined passes and transforms, and
//! sub-expression-located diagnostics.
use caap_core::{frontend::parse, PhasePolicy, RuntimeValue, Unit};

mod common;

use common::{corpus_path, eval_err_msg, eval_ok, stdlib_bootstrap, stdlib_path, with_stdlib_root};

/// Declarative rewriting, end to end: the pattern, the template AND the input
/// are all written as TEXT (frontend/surface template — the kit's own s-expr
/// grammar parses kernel text, `?x` names are pattern variables), the rule is
/// applied by lib/ir rewrite, and the result evaluates. (int_add ?x 0) -> ?x
/// over (int_add (int_mul 6 7) 0) leaves (int_mul 6 7) = 42.
#[test]
fn stdlib_text_rules_rewrite_and_evaluate() {
    let mut compiler = common::session();
    let bootstrap = stdlib_bootstrap();
    let emit = stdlib_path("boot/native_emit.caap");
    let src = format!(
        "(do
           (ctfe_compiler_execute_bootstrap_file compiler {bootstrap:?})
           (ctfe_compiler_execute_bootstrap_file compiler {emit:?})
           (bind (
             (sk (ctfe_compiler_lookup_value compiler \"stdlib.frontend.surface\"))
             (ir (ctfe_compiler_lookup_value compiler \"stdlib.syntax.ir\"))
           )
             (bind (
               (tpl (get sk \"template\" null))
               (rl  (get ir \"rule\" null))
               (rw  (get ir \"rewrite\" null))
             )
               (ctfe_eval_node
                 (rw (tpl \"(int_add (int_mul 6 7) 0)\" (map_of))
                     (list_of
                       (rl (tpl \"(int_add ?x 0)\" (map_of))
                           (tpl \"?x\" (map_of)))))))))"
    );
    let graph = parse(&src).expect("parse");
    let unit = Unit::from_graph("text_rules", graph).expect("unit");
    let v = compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect("text-rule rewrite");
    assert_eq!(v, RuntimeValue::Int(42), "x+0 -> x keeps (int_mul 6 7)");
}

/// Tier 2: lib/result loads and its functions work (direct functional check).
#[test]
fn stdlib_lib_result_works() {
    let v = eval_ok(
        "lib_result",
        &with_stdlib_root(
            "(bind ((r (load_module \"stdlib.lib.collections.result\")))
               (list_of
                 ((get r \"unwrap\" null) ((get r \"ok\" null) 42))
                 ((get r \"unwrap_or\" null) ((get r \"err\" null) \"e\" \"m\") 9)))",
        ),
    );
    let RuntimeValue::List(items) = v else {
        panic!("expected list, got {v:?}")
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(42), "unwrap (ok 42)");
    assert_eq!(items[1], RuntimeValue::Int(9), "unwrap_or err -> default");
}

/// The semantic checker: a misspelled name fails at LOAD time, naming the
/// module and the unknown identifier — not at first call.
#[test]
fn stdlib_check_rejects_unknown_name_at_load() {
    let bad = corpus_path("fixtures/typo.caap");
    let msg = eval_err_msg("stdlib_typo", &with_stdlib_root(&format!("(load {bad:?})")));
    assert!(msg.contains("failed load-time checks"), "msg: {msg}");
    assert!(
        msg.contains("unknown name `sequence_fold_lett`"),
        "msg: {msg}"
    );
    assert!(
        msg.contains("stdlib.fixtures.typo"),
        "names the module: {msg}"
    );
    assert!(
        msg.contains("typo.caap:"),
        "carries the form's path:line:col: {msg}"
    );
}

/// The semantic checker: calling a user lambda with the wrong number of
/// arguments fails at LOAD time with the expected/actual counts.
#[test]
fn stdlib_check_rejects_wrong_arity_at_load() {
    let bad = corpus_path("fixtures/bad_arity.caap");
    let msg = eval_err_msg(
        "stdlib_bad_arity",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(msg.contains("failed load-time checks"), "msg: {msg}");
    assert!(msg.contains("`add2` expects 2 arg(s), got 1"), "msg: {msg}");
}

/// USER-DEFINED load-time passes: fixtures/lint_pass.caap registers a
/// `no_int_div` analysis through stdlib.semantics.passes.registry; the loader then runs it
/// on every later load exactly like the built-in phases. A well-typed module
/// using int_div fails its load with the pass's finding, located at the
/// offending sub-expression.
#[test]
fn stdlib_user_pass_rejects_a_module_at_load() {
    let lint = corpus_path("fixtures/lint_pass.caap");
    let bad = corpus_path("fixtures/divides.caap");
    let msg = eval_err_msg(
        "user_pass",
        &with_stdlib_root(&format!("(do (load {lint:?}) (load {bad:?}))")),
    );
    assert!(
        msg.contains("divides.caap:5:4: [no_int_div] integer division is forbidden here"),
        "user pass finding with sub-expression location: {msg}"
    );
}

/// USER-DEFINED whole-module TRANSFORMS: lower_plus registers a rewrite of
/// the project vocabulary `plus` -> int_add. It runs after expansion and
/// BEFORE the check gate, so a module written in that vocabulary — which
/// fails its load outright without the transform — checks, types and
/// evaluates with it.
#[test]
fn stdlib_user_transform_lowers_project_vocabulary() {
    let lower = corpus_path("fixtures/lower_plus.caap");
    let user = corpus_path("fixtures/uses_plus.caap");

    // without the transform: `plus` is an unknown name, the load fails
    let msg = eval_err_msg(
        "plus_without",
        &with_stdlib_root(&format!("(load {user:?})")),
    );
    assert!(
        msg.contains("unknown name `plus`"),
        "vocabulary alone must not load: {msg}"
    );

    // with it: the rewrite lands before the gate; f() = 42
    let v = eval_ok(
        "plus_with",
        &with_stdlib_root(&format!(
            "(do (load {lower:?})
                 (bind ((m (load {user:?}))) ((get m \"f\" null))))"
        )),
    );
    assert_eq!(v, RuntimeValue::Int(42), "transformed module evaluates");
}

/// Pre-eval diagnostics point at the OFFENDING SUB-EXPRESSION, not just the
/// top-level form: expand preserves origin spans, so the checker locates an
/// unknown name and the type pass an arg mismatch at their own column.
#[test]
fn stdlib_diagnostics_are_located_at_the_sub_expression() {
    // bad_sig_call: (add8 1 "x") on line 4 — the offending "x" is well past
    // column 1 (the form start), so a sub-expression span is being used.
    let bad = corpus_path("fixtures/bad_sig_call.caap");
    let msg = eval_err_msg(
        "subform_type",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains("bad_sig_call.caap:4:33: `add8` arg 2: expected u8, got string"),
        "type mismatch at the arg's own column: {msg}"
    );

    // typo: the unknown name is located at its own column, not the form's.
    let typo = corpus_path("fixtures/typo.caap");
    let msg = eval_err_msg(
        "subform_name",
        &with_stdlib_root(&format!("(load {typo:?})")),
    );
    assert!(
        msg.contains("typo.caap:4:27: unknown name `sequence_fold_lett`"),
        "unknown name at its own column: {msg}"
    );
}
