/// Integration tests for all newly ported modules:
/// values, behaviors, signature, validation, diagnostics, mutation diff.
use caap_peg_port as peg;

// ── values ─────────────────────────────────────────────────────────────────

#[test]
fn extract_span_from_parse_result() {
    let grammar = peg::Grammar::new("root <- .").with_start_rule("root");
    let result = peg::parse_with_spans("x", &grammar, None).expect("parse ok");
    let span = peg::extract_span(&result);
    assert_eq!(span, Some((0, 1)));
}

#[test]
fn strip_spans_removes_wrapper_from_parse_result() {
    let grammar = peg::Grammar::new("root <- .").with_start_rule("root");
    let spanned = peg::parse_with_spans("a", &grammar, None).expect("parse ok");
    assert!(spanned.is_spanned());
    let stripped = peg::strip_spans(spanned);
    assert!(!stripped.is_spanned());
}

#[test]
fn unwrap_spanned_roundtrip() {
    let v = peg::ParseValue::Text("hi".into()).spanned(1, 3);
    let (inner, span) = peg::unwrap_spanned(v);
    assert_eq!(inner, peg::ParseValue::Text("hi".into()));
    assert_eq!(span, Some((1, 3)));
}

#[test]
fn contains_spanned_nested() {
    let v = peg::ParseValue::Node(
        "seq".into(),
        vec![peg::ParseValue::Text("x".into()).spanned(0, 1)],
    );
    assert!(peg::contains_spanned(&v));
    assert!(!peg::contains_spanned(&peg::ParseValue::Nil));
}

#[test]
fn sequence_builder_integration() {
    let mut b = peg::SequenceValueBuilder::new();
    b.add(peg::ParseValue::Text("a".into()));
    b.add(peg::ParseValue::Text("b".into()));
    match b.build() {
        peg::ParseValue::Node(name, items) => {
            assert_eq!(name, "sequence");
            assert_eq!(items.len(), 2);
        }
        v => panic!("unexpected: {v:?}"),
    }
}

// ── behaviors ─────────────────────────────────────────────────────────────

#[test]
fn transform_behavior_roundtrip() {
    let b = peg::TransformBehavior::new("trim").with_args(vec![peg::GrammarScalar::Bool(true)]);
    assert_eq!(b.name, "trim");
    assert_eq!(b.args, vec![peg::GrammarScalar::Bool(true)]);
}

#[test]
fn predicate_behavior_kind() {
    let entry = peg::BehaviorEntry::Predicate(peg::PredicateBehavior::new("is_keyword"));
    assert_eq!(entry.kind(), "predicate");
    assert!(entry.is_predicate());
    assert!(!entry.is_transform());
}

#[test]
fn diagnostic_behavior_label() {
    let b = peg::DiagnosticBehavior::new("expected ';'");
    assert_eq!(b.label, "expected ';'");
}

#[test]
fn trace_behavior_names() {
    let cap = peg::TraceBehavior::capture("lhs");
    assert_eq!(cap.kind, peg::TraceBehaviorKind::Capture);
    let act = peg::TraceBehavior::action("on_expr");
    assert_eq!(act.kind, peg::TraceBehaviorKind::Action);
}

#[test]
fn grammar_scalar_null_is_null() {
    assert!(peg::GrammarScalar::Null.is_null());
    assert!(!peg::GrammarScalar::Int(0).is_null());
}

// ── signature ─────────────────────────────────────────────────────────────

#[test]
fn grammar_signature_changes_on_rule_edit() {
    let mut g = peg::Grammar::new("root <- 'x'").with_start_rule("root");
    let sig1 = peg::grammar_signature(&g);
    g.set_rule("root", "'y'");
    let sig2 = peg::grammar_signature(&g);
    assert_ne!(sig1, sig2);
}

#[test]
fn node_signature_for_choice_vs_sequence() {
    let choice = peg::PegNode::Choice(vec![peg::PegNode::Literal("a".into())]);
    let seq = peg::PegNode::Sequence(vec![peg::PegNode::Literal("a".into())]);
    assert_ne!(peg::node_signature(&choice), peg::node_signature(&seq));
}

#[test]
fn nodes_structurally_equal_for_identical_trees() {
    let a = peg::PegNode::Sequence(vec![
        peg::PegNode::Literal("x".into()),
        peg::PegNode::Ref("y".into()),
    ]);
    let b = peg::PegNode::Sequence(vec![
        peg::PegNode::Literal("x".into()),
        peg::PegNode::Ref("y".into()),
    ]);
    assert!(peg::nodes_structurally_equal(&a, &b));
}

// ── validation ────────────────────────────────────────────────────────────

#[test]
fn validate_grammar_valid() {
    let g = peg::Grammar::new("root <- 'hello'").with_start_rule("root");
    let report = peg::validate_grammar(&g);
    assert!(report.ok());
    assert_eq!(report.error_count(), 0);
}

#[test]
fn validate_grammar_missing_start_rule() {
    let g = peg::Grammar::new("a <- 'x'").with_start_rule("root");
    let report = peg::validate_grammar(&g);
    assert!(!report.ok());
    assert!(report
        .errors()
        .any(|i| i.code.as_deref() == Some("missing_start_rule")));
}

#[test]
fn validate_grammar_missing_ref() {
    let g = peg::Grammar::new("root <- foo").with_start_rule("root");
    let report = peg::validate_grammar(&g);
    assert!(!report.ok());
    assert!(report
        .errors()
        .any(|i| i.code.as_deref() == Some("missing_ref")));
}

#[test]
fn validate_grammar_unreachable_rule_is_warning_not_error() {
    let g = peg::Grammar::new("root <- 'x'\norphan <- 'y'").with_start_rule("root");
    let report = peg::validate_grammar(&g);
    assert!(report.ok()); // still valid
    assert!(report
        .warnings()
        .any(|i| i.code.as_deref() == Some("unreachable_rule")));
}

#[test]
fn validate_grammar_cycle_is_warning() {
    let g = peg::Grammar::new("a <- b\nb <- a").with_start_rule("a");
    let report = peg::validate_grammar(&g);
    assert!(report
        .warnings()
        .any(|i| i.code.as_deref() == Some("left_recursive")));
}

#[test]
fn validate_grammar_with_label_sets_label() {
    let g = peg::Grammar::new("root <- 'x'").with_start_rule("root");
    let report = peg::validate_grammar_with_label(&g, Some("my_grammar"));
    assert_eq!(report.label.as_deref(), Some("my_grammar"));
}

// ── analysis ──────────────────────────────────────────────────────────────

#[test]
fn analysis_ref_graph_populated() {
    let g = peg::Grammar::new("root <- item\nitem <- 'x'").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    let root_refs = a.refs.get("root").cloned().unwrap_or_default();
    assert!(root_refs.contains(&"item".to_string()));
}

#[test]
fn analysis_reachable_includes_transitively_reached() {
    let g = peg::Grammar::new("root <- a\na <- b\nb <- 'x'").with_start_rule("root");
    let a = peg::analyze_grammar(&g);
    assert!(a.reachable.contains(&"root".to_string()));
    assert!(a.reachable.contains(&"a".to_string()));
    assert!(a.reachable.contains(&"b".to_string()));
}

#[test]
fn nullable_computation_via_fixed_point() {
    let g = peg::Grammar::new("root <- 'x'?\ninner <- 'y'?").with_start_rule("root");
    let nullable = peg::compute_nullable_rules(&g);
    assert!(nullable.contains("root"));
    assert!(nullable.contains("inner"));
}

// ── diagnostics ───────────────────────────────────────────────────────────

#[test]
fn diagnostics_snapshot_round_trip() {
    let mut snap = peg::ParserDiagnosticsSnapshot::new();
    snap.record_visit("root", 0, 0, false);
    snap.record_visit("root", 1, 1, false);
    snap.record_visit("item", 2, 0, false);
    assert_eq!(snap.total_visits, 3);
    let (hottest, _) = snap.hottest_rule().unwrap();
    assert_eq!(hottest, "root");
}

#[test]
fn diagnostics_memo_hits_counted() {
    let mut snap = peg::ParserDiagnosticsSnapshot::new();
    snap.record_visit("rule", 0, 0, false);
    snap.record_visit("rule", 0, 0, true);
    let stat = snap.rule_stats.get("rule").unwrap();
    assert_eq!(stat.memo_hits, 1);
    assert_eq!(stat.positions_tried, 1); // memo hit doesn't count
}

// ── mutation diff ─────────────────────────────────────────────────────────

#[test]
fn diff_grammars_no_change_reports_empty() {
    let g = peg::Grammar::new("root <- 'x'").with_start_rule("root");
    let diff = peg::diff_grammars(&g, &g);
    assert!(diff.is_empty());
    assert!(!diff.has_rule_changes());
}

#[test]
fn diff_grammars_add_rule() {
    let base = peg::Grammar::new("root <- 'x'").with_start_rule("root");
    let mut target = base.clone();
    peg::add_rule(&mut target, "extra", "'y'").unwrap();
    let diff = peg::diff_grammars(&base, &target);
    assert_eq!(diff.added_rules, vec!["extra"]);
    assert!(diff.has_rule_changes());
}

#[test]
fn diff_grammars_remove_rule() {
    let base = peg::Grammar::new("root <- 'x'\nextra <- 'y'").with_start_rule("root");
    let target = peg::Grammar::new("root <- 'x'").with_start_rule("root");
    let diff = peg::diff_grammars(&base, &target);
    assert_eq!(diff.removed_rules, vec!["extra"]);
}

#[test]
fn diff_grammars_change_start_rule() {
    let base = peg::Grammar::new("a <- 'x'\nb <- 'y'").with_start_rule("a");
    let target = peg::Grammar::new("a <- 'x'\nb <- 'y'").with_start_rule("b");
    let diff = peg::diff_grammars(&base, &target);
    assert!(diff.start_changed);
}

// ── incremental edit ──────────────────────────────────────────────────────

#[test]
fn incremental_edit_delta_calculation() {
    let insert = peg::IncrementalEdit::new_unchecked(5, 5, "abc");
    assert_eq!(insert.delta(), 3);

    let delete = peg::IncrementalEdit::new_unchecked(0, 4, "");
    assert_eq!(delete.delta(), -4);

    let replace = peg::IncrementalEdit::new_unchecked(1, 3, "xy");
    assert_eq!(replace.delta(), 0);
}

#[test]
fn incremental_edit_rejects_invalid_range() {
    assert!(peg::IncrementalEdit::new(10, 5, "x").is_none());
    assert!(peg::IncrementalEdit::new(0, 0, "").is_some());
}

// ── parser reference extraction ───────────────────────────────────────────

#[test]
fn extract_refs_from_simple_source() {
    let refs = peg::extract_refs_from_source("a b c");
    assert!(refs.contains(&"a".to_string()));
    assert!(refs.contains(&"b".to_string()));
    assert!(refs.contains(&"c".to_string()));
}

#[test]
fn extract_refs_no_refs_for_literals() {
    let refs = peg::extract_refs_from_source("'hello'");
    assert!(refs.is_empty());
}

#[test]
fn is_source_nullable_for_optional() {
    assert!(peg::is_source_nullable("'x'?"));
    assert!(peg::is_source_nullable("'x'*"));
    assert!(!peg::is_source_nullable("'x'"));
    assert!(!peg::is_source_nullable("'x'+"));
}
