//! Scenario: **result-value & introspection helpers** — span extract/strip/
//! unwrap, `SequenceValueBuilder`, `GrammarScalar`, grammar signatures, and
//! parser-diagnostics snapshots.

use caap_peg as peg;

// ── values ─────────────────────────────────────────────────────────────────

#[test]
fn extract_span_from_parse_result() {
    let grammar = peg::Grammar::trusted_new("root <- .").with_start_rule("root");
    let result = peg::ParseRequest::new(&grammar)
        .spans()
        .run("x")
        .expect("parse ok");
    let span = peg::extract_span(&result);
    assert_eq!(span, Some((0, 1)));
}

#[test]
fn strip_spans_removes_wrapper_from_parse_result() {
    let grammar = peg::Grammar::trusted_new("root <- .").with_start_rule("root");
    let spanned = peg::ParseRequest::new(&grammar)
        .spans()
        .run("a")
        .expect("parse ok");
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
        std::sync::Arc::new(vec![peg::ParseValue::Text("x".into()).spanned(0, 1)]),
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
            assert_eq!(&*name, "sequence");
            assert_eq!(items.len(), 2);
        }
        v => panic!("unexpected: {v:?}"),
    }
}

// ── driver scalar ───────────────────────────────────────────────────────────

#[test]
fn grammar_scalar_null_is_null() {
    assert!(peg::GrammarScalar::Null.is_null());
    assert!(!peg::GrammarScalar::Int(0).is_null());
}

// ── signature ─────────────────────────────────────────────────────────────

#[test]
fn grammar_signature_changes_on_rule_edit() {
    let mut g = peg::Grammar::trusted_new("root <- 'x'").with_start_rule("root");
    let sig1 = peg::grammar_signature(&g);
    g.set_rule("root", "'y'");
    let sig2 = peg::grammar_signature(&g);
    assert_ne!(sig1, sig2);
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
