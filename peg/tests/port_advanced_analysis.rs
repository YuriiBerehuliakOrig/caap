use caap_peg_port as peg;

// ── Param arity mismatch ───────────────────────────────────────────────────

#[test]
fn analysis_detects_param_arity_mismatch() {
    // `wrap` expects 1 param, but `root` calls it with 0 args.
    let g = peg::Grammar::new("wrap(x) <- $x\nroot <- wrap()").with_start_rule("root");
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
    let g = peg::Grammar::new("wrap(x) <- $x\nroot <- wrap('a', 'b')").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(!a.param_arity_mismatches.is_empty());
    let m = &a.param_arity_mismatches[0];
    assert_eq!(m.expected, 1);
    assert_eq!(m.got, 2);
}

#[test]
fn analysis_no_arity_error_for_correct_call() {
    let g = peg::Grammar::new("wrap(x) <- $x\nroot <- wrap('a')").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(a.param_arity_mismatches.is_empty());
}

// ── Undeclared params ──────────────────────────────────────────────────────

#[test]
fn analysis_detects_undeclared_param() {
    // `root` uses `$y` but only declares `x`.
    let g = peg::Grammar::new("root(x) <- $x $y").with_start_rule("root");
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
    let g = peg::Grammar::new("root(x, y) <- $x $y").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(a.undeclared_params.is_empty());
}

// ── Unused params ──────────────────────────────────────────────────────────

#[test]
fn analysis_detects_unused_param() {
    // `root` declares `y` but never uses it.
    let g = peg::Grammar::new("root(x, y) <- $x").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(
        a.unused_params.iter().any(|(r, p)| r == "root" && p == "y"),
        "should flag unused param 'y': {:?}",
        a.unused_params
    );
}

#[test]
fn analysis_no_unused_when_all_params_used() {
    let g = peg::Grammar::new("root(x) <- $x").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(a.unused_params.is_empty());
}

// ── Non-choice commits ─────────────────────────────────────────────────────

#[test]
fn analysis_detects_bare_cut() {
    // Cut at top-level of a sequence, not inside any choice.
    let g = peg::Grammar::new("root <- 'a' ~ 'b'").with_start_rule("root");
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
    let g = peg::Grammar::new("root <- ('a' ~ 'b') / 'c'").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(
        a.non_choice_commits.is_empty(),
        "cut inside choice should not be flagged: {:?}",
        a.non_choice_commits
    );
}

#[test]
fn analysis_detects_bare_eager() {
    let g = peg::Grammar::new("root <- 'a' !!('b')").with_start_rule("root");
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
    let g = peg::Grammar::new("wrap(x) <- $x\nroot <- wrap()").with_start_rule("root");
    let r = peg::validate_grammar(&g);
    assert!(!r.ok());
    assert!(r
        .errors()
        .any(|i| i.code.as_deref() == Some("param_arity_mismatch")));
}

#[test]
fn validation_errors_on_undeclared_param() {
    let g = peg::Grammar::new("root(x) <- $x $y").with_start_rule("root");
    let r = peg::validate_grammar(&g);
    assert!(!r.ok());
    assert!(r
        .errors()
        .any(|i| i.code.as_deref() == Some("undeclared_param")));
}

#[test]
fn validation_warns_on_unused_param() {
    let g = peg::Grammar::new("root(x, y) <- $x").with_start_rule("root");
    let r = peg::validate_grammar(&g);
    assert!(r
        .warnings()
        .any(|i| i.code.as_deref() == Some("unused_param")));
}

#[test]
fn validation_warns_on_non_choice_commit() {
    let g = peg::Grammar::new("root <- 'a' ~ 'b'").with_start_rule("root");
    let r = peg::validate_grammar(&g);
    assert!(r
        .warnings()
        .any(|i| i.code.as_deref() == Some("non_choice_commit")));
}

// ── GrammarScope stub ──────────────────────────────────────────────────────

#[test]
fn grammar_scope_in_text_parses_ok() {
    // scope() syntax parses without error; execution fails at runtime (needs registry).
    let g = peg::Grammar::new("root <- scope('other', 'x')").with_start_rule("root");
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
    let g = peg::Grammar::new("root <- soft_keyword('as')").with_start_rule("root");
    let config = peg::ParserConfig::default();
    let result = peg::PEGParser.parse(&g, "assert", &config);
    assert!(
        result.is_err(),
        "soft_keyword should reject 'assert' for keyword 'as'"
    );
}

#[test]
fn soft_keyword_matches_exact_word() {
    let g = peg::Grammar::new("root <- soft_keyword('as')").with_start_rule("root");
    let config = peg::ParserConfig::default();
    let result = peg::PEGParser.parse(&g, "as", &config);
    assert!(result.is_ok(), "soft_keyword should match exact word 'as'");
}

// ── extract_* helpers ──────────────────────────────────────────────────────

#[test]
fn extract_calls_finds_parametric_calls() {
    // Use generic non-keyword rule names to test Call extraction.
    let calls = peg::extract_calls_from_source("wrap('a') combine(item, 'x')");
    assert!(
        calls.iter().any(|(r, n)| r == "wrap" && *n == 1),
        "should find wrap/1: {:?}",
        calls
    );
    assert!(
        calls.iter().any(|(r, n)| r == "combine" && *n == 2),
        "should find combine/2: {:?}",
        calls
    );
}

#[test]
fn extract_params_used_finds_params() {
    let params = peg::extract_params_used_from_source("$x $y $x");
    // Deduped and sorted.
    assert_eq!(params, vec!["x".to_string(), "y".to_string()]);
}

#[test]
fn has_bare_commit_detects_cut_in_sequence() {
    let result = peg::has_bare_commit_from_source("'a' ~ 'b'");
    assert_eq!(result, Some("cut"));
}

#[test]
fn has_bare_commit_none_for_cut_in_choice() {
    let result = peg::has_bare_commit_from_source("('a' ~ 'b') / 'c'");
    assert_eq!(result, None);
}

#[test]
fn has_bare_commit_none_for_no_commit() {
    let result = peg::has_bare_commit_from_source("'a' 'b'");
    assert_eq!(result, None);
}
