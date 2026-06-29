//! Scenario: **static analysis & validation** findings on a grammar without
//! parsing input — parametric-rule arity, undeclared/unused params, bare
//! cut/eager, soft-keyword matching, reference graph/reachability/nullability,
//! duplicate rules, and the validation report's error/warning classification.

use caap_peg as peg;

// ── Param arity mismatch ───────────────────────────────────────────────────

#[test]
fn analysis_detects_param_arity_mismatch() {
    // `wrap` expects 1 param, but `root` calls it with 0 args.
    let g = peg::Grammar::trusted_new("wrap(x) <- $x\nroot <- wrap()").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(
        !a.param_arity_mismatches.is_empty(),
        "should detect arity mismatch"
    );
    let m = &a.param_arity_mismatches[0];
    assert_eq!(m.callee, "wrap");
    assert_eq!(m.expected, 1);
    assert_eq!(m.got, 0);
}

#[test]
fn analysis_detects_too_many_args() {
    let g =
        peg::Grammar::trusted_new("wrap(x) <- $x\nroot <- wrap('a', 'b')").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(!a.param_arity_mismatches.is_empty());
    let m = &a.param_arity_mismatches[0];
    assert_eq!(m.expected, 1);
    assert_eq!(m.got, 2);
}

#[test]
fn analysis_no_arity_error_for_correct_call() {
    let g = peg::Grammar::trusted_new("wrap(x) <- $x\nroot <- wrap('a')").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(a.param_arity_mismatches.is_empty());
}

// ── Undeclared params ──────────────────────────────────────────────────────

#[test]
fn analysis_detects_undeclared_param() {
    // `root` uses `$y` but only declares `x`.
    let g = peg::Grammar::trusted_new("root(x) <- $x $y").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(
        a.undeclared_params
            .iter()
            .any(|(r, p)| r == "root" && p == "y"),
        "should flag undeclared param 'y': {:?}",
        a.undeclared_params
    );
}

#[test]
fn analysis_no_undeclared_for_correct_params() {
    let g = peg::Grammar::trusted_new("root(x, y) <- $x $y").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(a.undeclared_params.is_empty());
}

// ── Unused params ──────────────────────────────────────────────────────────

#[test]
fn analysis_detects_unused_param() {
    // `root` declares `y` but never uses it.
    let g = peg::Grammar::trusted_new("root(x, y) <- $x").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(
        a.unused_params.iter().any(|(r, p)| r == "root" && p == "y"),
        "should flag unused param 'y': {:?}",
        a.unused_params
    );
}

#[test]
fn analysis_no_unused_when_all_params_used() {
    let g = peg::Grammar::trusted_new("root(x) <- $x").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(a.unused_params.is_empty());
}

// ── Non-choice commits ─────────────────────────────────────────────────────

#[test]
fn analysis_detects_bare_cut() {
    // Cut at top-level of a sequence, not inside any choice.
    let g = peg::Grammar::trusted_new("root <- 'a' ~ 'b'").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(
        a.non_choice_commits
            .iter()
            .any(|(r, k)| r == "root" && k == "cut"),
        "should detect bare cut: {:?}",
        a.non_choice_commits
    );
}

#[test]
fn analysis_no_bare_cut_inside_choice() {
    // Cut inside a choice alternative is expected and valid.
    let g = peg::Grammar::trusted_new("root <- ('a' ~ 'b') / 'c'").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(
        a.non_choice_commits.is_empty(),
        "cut inside choice should not be flagged: {:?}",
        a.non_choice_commits
    );
}

#[test]
fn analysis_detects_bare_eager() {
    let g = peg::Grammar::trusted_new("root <- 'a' !!('b')").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    // eager outside choice flagged
    assert!(
        a.non_choice_commits
            .iter()
            .any(|(r, k)| r == "root" && k == "eager"),
        "should detect bare eager: {:?}",
        a.non_choice_commits
    );
}

// ── Validation picks up new checks ────────────────────────────────────────

#[test]
fn validation_errors_on_param_arity_mismatch() {
    let g = peg::Grammar::trusted_new("wrap(x) <- $x\nroot <- wrap()").with_start_rule("root");
    let r = peg::validate_grammar(&g);
    assert!(!r.ok());
    assert!(r
        .errors()
        .any(|i| i.code.as_deref() == Some("param_arity_mismatch")));
}

#[test]
fn validation_errors_on_undeclared_param() {
    let g = peg::Grammar::trusted_new("root(x) <- $x $y").with_start_rule("root");
    let r = peg::validate_grammar(&g);
    assert!(!r.ok());
    assert!(r
        .errors()
        .any(|i| i.code.as_deref() == Some("undeclared_param")));
}

#[test]
fn validation_warns_on_unused_param() {
    let g = peg::Grammar::trusted_new("root(x, y) <- $x").with_start_rule("root");
    let r = peg::validate_grammar(&g);
    assert!(r
        .warnings()
        .any(|i| i.code.as_deref() == Some("unused_param")));
}

#[test]
fn validation_warns_on_non_choice_commit() {
    let g = peg::Grammar::trusted_new("root <- 'a' ~ 'b'").with_start_rule("root");
    let r = peg::validate_grammar(&g);
    assert!(r
        .warnings()
        .any(|i| i.code.as_deref() == Some("non_choice_commit")));
}

// ── GrammarScope stub ──────────────────────────────────────────────────────

#[test]
fn grammar_scope_in_text_parses_ok() {
    // scope() syntax parses without error; execution fails at runtime (needs registry).
    let g = peg::Grammar::trusted_new("root <- scope('other', 'x')").with_start_rule("root");
    let config = peg::ParserConfig::default();
    // Compiling the grammar should succeed (the syntax is valid).
    // Parsing will fail because no registry is available.
    let result = peg::PEGParser.parse(&g, "x", &config);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.message.contains("registry"), "err: {}", err.message);
}

// ── SoftKeyword word-boundary ──────────────────────────────────────────────

#[test]
fn soft_keyword_rejects_prefix_of_longer_word() {
    // `soft_keyword('as')` should not match 'assert' because 'assert' has a
    // word char after 'as'.
    let g = peg::Grammar::trusted_new("root <- soft_keyword('as')").with_start_rule("root");
    let config = peg::ParserConfig::default();
    let result = peg::PEGParser.parse(&g, "assert", &config);
    assert!(
        result.is_err(),
        "soft_keyword should reject 'assert' for keyword 'as'"
    );
}

#[test]
fn soft_keyword_matches_exact_word() {
    let g = peg::Grammar::trusted_new("root <- soft_keyword('as')").with_start_rule("root");
    let config = peg::ParserConfig::default();
    let result = peg::PEGParser.parse(&g, "as", &config);
    assert!(result.is_ok(), "soft_keyword should match exact word 'as'");
}

// Source-level analysis helpers (`extract_calls_from_source`, `has_bare_commit_from_source`,
// `extract_params_used_from_source`, …) were removed in favour of the internal
// expr-based analysis; their behaviour is now unit-tested inside
// `src/parser_analysis.rs`.

// ── validation report ──────────────────────────────────────────────────────
// ── validation ────────────────────────────────────────────────────────────

#[test]
fn validate_grammar_valid() {
    let g = peg::Grammar::trusted_new("root <- 'hello'").with_start_rule("root");
    let report = peg::validate_grammar(&g);
    assert!(report.ok());
    assert_eq!(report.error_count(), 0);
}

#[test]
fn validate_grammar_missing_start_rule() {
    let g = peg::Grammar::trusted_new("a <- 'x'").with_start_rule("root");
    let report = peg::validate_grammar(&g);
    assert!(!report.ok());
    assert!(report
        .errors()
        .any(|i| i.code.as_deref() == Some("missing_start_rule")));
}

#[test]
fn validate_grammar_missing_ref() {
    let g = peg::Grammar::trusted_new("root <- foo").with_start_rule("root");
    let report = peg::validate_grammar(&g);
    assert!(!report.ok());
    assert!(report
        .errors()
        .any(|i| i.code.as_deref() == Some("missing_ref")));
}

#[test]
fn validate_grammar_unreachable_rule_is_warning_not_error() {
    let g = peg::Grammar::trusted_new("root <- 'x'\norphan <- 'y'").with_start_rule("root");
    let report = peg::validate_grammar(&g);
    assert!(report.ok()); // still valid
    assert!(report
        .warnings()
        .any(|i| i.code.as_deref() == Some("unreachable_rule")));
}

#[test]
fn validate_grammar_cycle_is_warning() {
    let g = peg::Grammar::trusted_new("a <- b\nb <- a").with_start_rule("a");
    let report = peg::validate_grammar(&g);
    assert!(report
        .warnings()
        .any(|i| i.code.as_deref() == Some("left_recursive")));
}

#[test]
fn validate_grammar_with_label_sets_label() {
    let g = peg::Grammar::trusted_new("root <- 'x'").with_start_rule("root");
    let report = peg::validate_grammar_with_label(&g, Some("my_grammar"));
    assert_eq!(report.label.as_deref(), Some("my_grammar"));
}

// ── analysis ──────────────────────────────────────────────────────────────

#[test]
fn analysis_ref_graph_populated() {
    let g = peg::Grammar::trusted_new("root <- item\nitem <- 'x'").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    let root_refs = a.refs.get("root").cloned().unwrap_or_default();
    assert!(root_refs.contains(&"item".to_string()));
}

#[test]
fn analysis_reachable_includes_transitively_reached() {
    let g = peg::Grammar::trusted_new("root <- a\na <- b\nb <- 'x'").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(a.reachable.contains(&"root".to_string()));
    assert!(a.reachable.contains(&"a".to_string()));
    assert!(a.reachable.contains(&"b".to_string()));
}

#[test]
fn nullable_computation_via_fixed_point() {
    let g = peg::Grammar::trusted_new("root <- 'x'?\ninner <- 'y'?").with_start_rule("root");
    let nullable = peg::compute_nullable_rules(&g);
    assert!(nullable.contains("root"));
    assert!(nullable.contains("inner"));
}

// ── duplicate rules ─────────────────────────────────────────────────────────
#[test]
fn analysis_detects_duplicate_rules() {
    // Two rules share the name `a` — duplicate rule names are preserved by the
    // text parser, so the analyzer flags them.
    let grammar = peg::Grammar::trusted_new("a <- [a]\na <- [b]").with_start_rule("root");

    let analysis = peg::analyze_grammar(&grammar);
    assert!(analysis.has_duplicate_rule_names);
    assert!(!analysis.errors.is_empty());
}
