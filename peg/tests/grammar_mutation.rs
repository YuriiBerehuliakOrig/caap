//! Scenario: **grammar mutation & incremental edits** — add/remove rules,
//! `diff_grammars`, incremental-edit delta math, and analysis-cache
//! invalidation on structural edits.

use caap_peg as peg;

fn edit(start: usize, old_end: usize, replacement: impl Into<String>) -> peg::IncrementalEdit {
    peg::IncrementalEdit::new(start, old_end, replacement).expect("test edit must be valid")
}

// ── mutation diff ─────────────────────────────────────────────────────────

#[test]
fn diff_grammars_no_change_reports_empty() {
    let g = peg::Grammar::trusted_new("root <- 'x'").with_start_rule("root");
    let diff = peg::diff_grammars(&g, &g);
    assert!(diff.is_empty());
    assert!(!diff.has_rule_changes());
}

#[test]
fn diff_grammars_add_rule() {
    let base = peg::Grammar::trusted_new("root <- 'x'").with_start_rule("root");
    let mut target = base.clone();
    peg::add_rule(&mut target, "extra", "'y'").unwrap();
    let diff = peg::diff_grammars(&base, &target);
    assert_eq!(diff.added_rules, vec!["extra"]);
    assert!(diff.has_rule_changes());
}

#[test]
fn diff_grammars_remove_rule() {
    let base = peg::Grammar::trusted_new("root <- 'x'\nextra <- 'y'").with_start_rule("root");
    let target = peg::Grammar::trusted_new("root <- 'x'").with_start_rule("root");
    let diff = peg::diff_grammars(&base, &target);
    assert_eq!(diff.removed_rules, vec!["extra"]);
}

#[test]
fn diff_grammars_change_start_rule() {
    let base = peg::Grammar::trusted_new("a <- 'x'\nb <- 'y'").with_start_rule("a");
    let target = peg::Grammar::trusted_new("a <- 'x'\nb <- 'y'").with_start_rule("b");
    let diff = peg::diff_grammars(&base, &target);
    assert!(diff.start_changed);
}

// ── incremental edit ──────────────────────────────────────────────────────

#[test]
fn incremental_edit_delta_calculation() {
    let insert = edit(5, 5, "abc");
    assert_eq!(insert.delta(), Some(3));

    let delete = edit(0, 4, "");
    assert_eq!(delete.delta(), Some(-4));

    let replace = edit(1, 3, "xy");
    assert_eq!(replace.delta(), Some(0));
}

#[test]
fn incremental_edit_rejects_invalid_range() {
    assert!(peg::IncrementalEdit::new(10, 5, "x").is_none());
    assert!(peg::IncrementalEdit::new(0, 0, "").is_some());
}

// `extract_refs_from_source` / `is_source_nullable` were removed in favour of the
// internal expr-based analysis (`extract_refs_from_expr` / `expr_is_nullable`);
// their behaviour is now unit-tested inside `src/parser_analysis.rs`.

// ── mutation & cache invalidation ───────────────────────────────────────────
#[test]
fn mutation_can_add_and_remove_rule() {
    let mut grammar = peg::Grammar::trusted_new("a <- [a]");
    assert!(peg::add_rule(&mut grammar, "b", "[b]").is_ok());
    assert!(grammar.get_rule("b").is_some());

    let removed = peg::remove_rule(&mut grammar, "a").expect("remove returns ok");
    assert!(removed);
    assert!(!grammar.text.contains("a <-"));
}
#[test]
fn registry_roundtrips_json_rules() {
    let grammar = peg::load_json_grammar(
        "{\"start_rule\":\"root\",\"rules\":[{\"name\":\"root\",\"source\":\"[a]\"}]}",
    )
    .expect("json grammar loads");

    assert_eq!(grammar.start_rule, "root");
    assert_eq!(grammar.rule_count(), 1);
}
#[test]
fn with_rules_resets_analysis_cache() {
    let mut grammar = peg::Grammar::trusted_new("a <- [a]");
    let _ = peg::analyze_and_store(&mut grammar);
    assert!(grammar.state.analysis_state.is_some());
    let grammar = grammar.with_rules(vec![peg::GrammarRule::trusted_from_source(
        "a",
        "[b]",
        Vec::new(),
    )]);
    assert!(grammar.state.analysis_state.is_none());
}
#[test]
fn with_start_rule_resets_analysis_cache() {
    let grammar = peg::Grammar::trusted_new("a <- [a]").with_start_rule("a");
    let mut grammar = grammar;
    let _ = peg::analyze_and_store(&mut grammar);
    assert!(grammar.state.analysis_state.is_some());

    let grammar = grammar.with_start_rule("b");
    assert!(grammar.state.analysis_state.is_none());
}
