//! Scenario tests for the segmental reader (now the default for both the run and
//! compile paths): forms are read one at a time so a top-level `extend_syntax`
//! directive can grow the grammar mid-source, and the assembled whole-program
//! graph is evaluated with top-level names hoisted — so forward references
//! between top-level definitions resolve the same on `eval_source` and the
//! Unit/compile path.

use caap_core::frontend::{eval_source, parse_segmental};
use caap_core::{Evaluator, RuntimeValue};

// ── whole-program evaluation over segmental reading ─────────────────────────

/// A multi-form program yields the value of its last form.
#[test]
fn evaluates_multi_form_program() {
    let src = "(bind x 10)\n(int_add x 5)";
    assert_eq!(eval_source(src).expect("eval"), RuntimeValue::Int(15));
}

/// An earlier definition is visible to a later form.
#[test]
fn earlier_definitions_are_visible_to_later_forms() {
    let src = "(bind a 3)\n(bind b 4)\n(int_add a b)";
    assert_eq!(eval_source(src).expect("eval"), RuntimeValue::Int(7));
}

// ── forward references (hoisting parity with the compile path) ───────────────

/// A top-level function may reference another defined *later* in source order:
/// mutual recursion resolves because top-level names are hoisted.
#[test]
fn mutually_recursive_top_level_functions_resolve() {
    let src = "(bind is_even (lambda (n) (if (eq n 0) true (is_odd (int_sub n 1)))))\n\
               (bind is_odd (lambda (n) (if (eq n 0) false (is_even (int_sub n 1)))))\n\
               (is_even 10)";
    assert_eq!(eval_source(src).expect("eval"), RuntimeValue::Bool(true));
}

/// A closure referring to a top-level value defined after it still resolves when
/// invoked (the value is bound by call time).
#[test]
fn closure_sees_later_top_level_definition() {
    let src = "(bind get (lambda () answer))\n(bind answer 42)\n(get)";
    assert_eq!(eval_source(src).expect("eval"), RuntimeValue::Int(42));
}

/// Using a top-level value *before its defining form runs* fails — a
/// fundamental eval-order limit (the value does not exist yet), not a parsing
/// one. This fails identically on the run and compile paths.
#[test]
fn use_before_a_definition_runs_is_an_error() {
    let src = "(bind a (int_add helper 1))\n(bind helper 5)\na";
    let error = eval_source(src).expect_err("use-before-definition must fail");
    assert!(
        error.to_string().contains("helper"),
        "expected an error naming `helper`, got: {error}"
    );
}

// ── in-stream grammar mutation (extend_syntax directive) ─────────────────────

/// A top-level `extend_syntax` directive grows the grammar so a later form reads
/// under it: extending `boolean` to accept `yes` makes a bare `yes` a boolean
/// (lowered to `false`, since the text is not `"true"`).
#[test]
fn extend_syntax_changes_how_a_later_form_is_read() {
    let src = "(extend_syntax \"boolean\" \"'true' / 'false' / 'yes'\")\nyes";
    assert_eq!(eval_source(src).expect("eval"), RuntimeValue::Bool(false));
}

/// Without the extension, a bare `yes` is an ordinary symbol — an unbound name.
#[test]
fn without_extension_a_bare_word_is_an_unbound_symbol() {
    assert!(eval_source("yes").is_err());
}

// ── scoped grammar blocks (define_grammar / begin_scope / end_scope) ─────────

/// Two blocks in one source each read with their own named grammar; the program
/// value is the last block's form.
#[test]
fn two_blocks_use_different_named_grammars() {
    let src = "(define_grammar \"a\" \"null\" \"'null' / 'nil'\")\n\
               (define_grammar \"b\" \"null\" \"'null' / 'none'\")\n\
               (begin_scope \"a\")\n(eq nil null)\n(end_scope)\n\
               (begin_scope \"b\")\n(eq none null)\n(end_scope)";
    assert_eq!(eval_source(src).expect("eval"), RuntimeValue::Bool(true));
}

/// A scoped grammar reverts at `end_scope`: syntax from inside the block does not
/// leak out.
#[test]
fn scoped_grammar_reverts_after_end_scope() {
    let src = "(define_grammar \"g\" \"null\" \"'null' / 'nil'\")\n\
               (begin_scope \"g\")\n(eq nil null)\n(end_scope)\n\
               (eq nil null)"; // `nil` is an ordinary symbol again here
    assert!(eval_source(src).is_err());
}

/// `begin_scope` of an unregistered grammar is an error.
#[test]
fn begin_scope_unknown_grammar_is_an_error() {
    let error = eval_source("(begin_scope \"missing\")\n(end_scope)")
        .expect_err("unknown grammar must fail");
    assert!(
        error.to_string().contains("unknown grammar"),
        "got: {error}"
    );
}

/// An unterminated scope (missing `end_scope`) is an error.
#[test]
fn unbalanced_begin_scope_is_an_error() {
    let src = "(define_grammar \"g\" \"null\" \"'null'\")\n(begin_scope \"g\")\n1";
    let error = eval_source(src).expect_err("unbalanced scope must fail");
    assert!(error.to_string().contains("end_scope"), "got: {error}");
}

// ── parse_segmental directly (the reader both paths share) ───────────────────

/// `parse_segmental` applies the directive at read time and consumes it; the
/// produced graph evaluates to the boolean.
#[test]
fn parse_segmental_applies_extend_syntax_directive() {
    let src = "(extend_syntax \"boolean\" \"'true' / 'false' / 'yes'\")\nyes";
    let graph = parse_segmental(src).expect("segmental parse");
    let mut ev = Evaluator::new(graph);
    assert_eq!(ev.run().expect("eval"), RuntimeValue::Bool(false));
}

/// Without a directive, `parse_segmental` produces the same program as the
/// whole-file path.
#[test]
fn parse_segmental_matches_whole_file_without_directives() {
    let graph = parse_segmental("(bind x 6)\n(int_add x 1)").expect("segmental parse");
    let mut ev = Evaluator::new(graph);
    assert_eq!(ev.run().expect("eval"), RuntimeValue::Int(7));
}
