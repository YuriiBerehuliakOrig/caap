//! Scenario: the public `ParseRequest` API surface and incremental caching —
//! span-wrapped results, AST output mode, prefix parsing, sequential edits, and
//! the cache invalidation rules (grammar / import / runtime-signature changes).

use caap_peg as peg;

fn edit(start: usize, old_end: usize, replacement: impl Into<String>) -> peg::IncrementalEdit {
    peg::IncrementalEdit::new(start, old_end, replacement).expect("test edit must be valid")
}

#[test]
fn parse_with_spans_returns_span_value_when_requested() {
    let grammar = peg::Grammar::trusted_new("raw <- .").with_start_rule("raw");
    let parsed = peg::ParseRequest::new(&grammar).spans().run("h").unwrap();
    match parsed {
        peg::ParseValue::SpannedValue { value, .. } => {
            assert!(matches!(*value, peg::ParseValue::Text(_)))
        }
        other => panic!("unexpected parse value: {other:?}"),
    }
}

#[test]
fn parse_output_ast_mode_returns_ast_node() {
    let grammar = peg::Grammar::trusted_new("start <- \"x\"").with_start_rule("start");
    let output = peg::ParseRequest::new(&grammar)
        .ast()
        .run_output("x")
        .expect("parse output ok");
    match output {
        peg::ParseOutput::Ast(node) => {
            assert_eq!(node.rule, "start");
            assert_eq!(node.span.start, 0);
            assert_eq!(node.span.end, 1);
        }
        other => panic!("expected AST output, got {other:?}"),
    }
}

#[test]
fn parse_prefix_reports_consumed_prefix_for_early_rule() {
    let grammar = peg::Grammar::trusted_new("words <- .");
    let grammar = grammar.with_start_rule("words");
    let prefix = peg::ParseRequest::new(&grammar).run_prefix("a b c", 0);
    assert!(prefix.consumed > 0);
    assert!(prefix.errors.is_empty() || prefix.value.is_some());
}

#[test]
fn incremental_cache_reuses_previous_result() {
    let mut cache = peg::ParseCache::default();
    let grammar = peg::Grammar::trusted_new("start <- \"abc\"").with_start_rule("start");
    let first = peg::ParseRequest::new(&grammar)
        .run_incremental("abc", &mut cache)
        .expect("first incremental parse should succeed");
    let second = peg::ParseRequest::new(&grammar)
        .run_incremental("abc", &mut cache)
        .expect("second incremental parse should reuse cache");
    assert_eq!(format!("{:?}", first), format!("{:?}", second));
    assert_eq!(cache.entries.len(), 1);
}

#[test]
fn parse_incremental_cache_invalidates_on_grammar_change() {
    let mut cache = peg::ParseCache::default();
    let grammar = peg::Grammar::trusted_new("start <- \"ab\"").with_start_rule("start");
    let other_grammar = peg::Grammar::trusted_new("start <- \"ac\"").with_start_rule("start");

    let first = peg::ParseRequest::new(&grammar)
        .run_incremental("ab", &mut cache)
        .expect("first incremental parse should succeed");
    let second = peg::ParseRequest::new(&other_grammar)
        .run_incremental("ac", &mut cache)
        .expect("changed grammar incremental parse should succeed");

    assert_eq!(first.as_ref(), &peg::ParseValue::Text("ab".into()));
    assert_eq!(second.as_ref(), &peg::ParseValue::Text("ac".into()));
    assert_eq!(cache.entries.len(), 2);
}

#[test]
fn incremental_cache_invalidates_on_inline_import_change() {
    let mut cache = peg::ParseCache::default();
    let import_v1 = peg::Grammar::trusted_new("rule <- \"x\"")
        .with_start_rule("rule")
        .with_metadata(
            "__grammar__",
            [("version".to_string(), serde_json::json!(1))]
                .into_iter()
                .collect(),
        );
    let import_v2 = peg::Grammar::trusted_new("rule <- \"x\"")
        .with_start_rule("rule")
        .with_metadata(
            "__grammar__",
            [("version".to_string(), serde_json::json!(2))]
                .into_iter()
                .collect(),
        );
    let grammar_v1 = peg::Grammar::trusted_new("start <- other::rule")
        .with_start_rule("start")
        .with_import("other", import_v1);
    let grammar_v2 = peg::Grammar::trusted_new("start <- other::rule")
        .with_start_rule("start")
        .with_import("other", import_v2);

    let first = peg::ParseRequest::new(&grammar_v1)
        .run_incremental("x", &mut cache)
        .expect("first imported grammar parse should succeed");
    let second = peg::ParseRequest::new(&grammar_v2)
        .run_incremental("x", &mut cache)
        .expect("changed import metadata parse should succeed");

    assert_eq!(first.as_ref(), &peg::ParseValue::Text("x".into()));
    assert_eq!(second.as_ref(), &peg::ParseValue::Text("x".into()));
    assert_eq!(cache.entries.len(), 2);
}

#[test]
fn parse_incremental_cache_invalidates_on_runtime_signature_change() {
    let mut cache = peg::ParseCache::default();
    let grammar = peg::Grammar::trusted_new("start <- \"ab\"").with_start_rule("start");

    let plain = peg::ParseRequest::new(&grammar)
        .run_incremental("ab", &mut cache)
        .expect("plain incremental parse should succeed");
    let with_spans = peg::ParseRequest::new(&grammar)
        .config(peg::ParserConfig::default().with_spans())
        .run_incremental("ab", &mut cache)
        .expect("spanned incremental parse should succeed");

    assert_eq!(plain.as_ref(), &peg::ParseValue::Text("ab".into()));
    assert!(matches!(
        with_spans.as_ref(),
        peg::ParseValue::SpannedValue { .. }
    ));
    assert_eq!(cache.entries.len(), 2);
}

#[test]
fn parse_incremental_cache_tracks_memo_policy_in_runtime_signature() {
    let mut cache = peg::ParseCache::default();
    let grammar = peg::Grammar::trusted_new("start <- \"ab\"").with_start_rule("start");

    let default_policy = peg::ParseRequest::new(&grammar)
        .run_incremental("ab", &mut cache)
        .expect("default policy parse should succeed");
    let limited_policy = peg::ParseRequest::new(&grammar)
        .config(peg::ParserConfig {
            memo_policy: Some(peg::MemoPolicy::new(Some(32)).unwrap()),
            ..peg::ParserConfig::default()
        })
        .run_incremental("ab", &mut cache)
        .expect("limited policy parse should succeed");

    assert_eq!(default_policy.as_ref(), &peg::ParseValue::Text("ab".into()));
    assert_eq!(limited_policy.as_ref(), &peg::ParseValue::Text("ab".into()));
    assert_eq!(cache.entries.len(), 2);
}

#[test]
fn parse_incremental_many_reports_parse_failure() {
    let mut cache = peg::ParseCache::default();
    let grammar = peg::Grammar::trusted_new("start <- \"abc\"").with_start_rule("start");
    let err = peg::ParseRequest::new(&grammar)
        .run_incremental("abx", &mut cache)
        .expect_err("incremental parse should fail instead of returning Nil");
    assert!(!err.message.is_empty());
    assert_eq!(cache.entries.len(), 0);
}

#[test]
fn parse_incremental_many_rejects_incomplete_input() {
    let mut cache = peg::ParseCache::default();
    let grammar = peg::Grammar::trusted_new("start <- \"abc\"").with_start_rule("start");
    let err = peg::ParseRequest::new(&grammar)
        .run_incremental("abcd", &mut cache)
        .expect_err("incremental parse should reject trailing input");
    assert_eq!(err.code.as_deref(), Some("incomplete_input"));
    assert_eq!(cache.entries.len(), 0);
}

#[test]
fn edits_to_sequential_are_filtered_by_offset() {
    let edits = vec![edit(4, 5, "Y"), edit(0, 2, "XYZ")];
    let sequential = peg::snapshot_edits_to_sequential("abcdef", &edits)
        .expect("snapshot edits should be valid");
    assert_eq!(
        sequential,
        vec![
            peg::CompletedEdit {
                text: "XYZ".to_string(),
                span: (0, 2),
            },
            peg::CompletedEdit {
                text: "Y".to_string(),
                span: (4 + 1, 5 + 1),
            },
        ]
    );
    let rebuilt = peg::apply_edits("abcdef", &sequential).expect("sequential edits apply");
    assert_eq!(rebuilt, "XYZcdeY");
}

#[test]
fn clone_grammar_resets_analysis_cache() {
    let mut base = peg::Grammar::trusted_new("a <- [a]");
    let _ = peg::analyze_and_store(&mut base);
    let cloned = peg::clone_grammar(
        &base,
        Some(peg::GrammarPatch {
            source: "b <- [b]".to_string(),
            start_rule: Some("b".to_string()),
        }),
    )
    .expect("grammar patch should parse");
    assert_eq!(cloned.start_rule, "b");
    assert!(cloned.state.analysis_state.is_none());
}
