use caap_peg_port as peg;

#[test]
fn parse_with_spans_returns_span_value_when_requested() {
    let grammar = peg::Grammar::new("raw <- .").with_start_rule("raw");
    let parsed = peg::parse("h", &grammar, None, true).unwrap();
    match parsed {
        peg::ParseValue::SpannedValue { value, .. } => {
            assert!(matches!(*value, peg::ParseValue::Text(_)))
        }
        other => panic!("unexpected parse value: {other:?}"),
    }
}

#[test]
fn parse_output_ast_mode_returns_ast_node() {
    let grammar = peg::Grammar::new("start <- \"x\"").with_start_rule("start");
    let config = peg::ParserConfig::default().with_output_mode(peg::ParserOutputMode::Ast);
    let output = peg::parse_output("x", &grammar, Some(config)).expect("parse output ok");
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
    let grammar = peg::Grammar::new("words <- .");
    let grammar = grammar.with_start_rule("words");
    let prefix = peg::parse_prefix(
        "a b c",
        &grammar,
        None,
        0,
        Some(peg::ParserConfig::default()),
        false,
    );
    assert!(prefix.consumed > 0);
    assert!(prefix.errors.is_empty() || prefix.value.is_some());
}

#[test]
fn incremental_cache_reuses_previous_result() {
    let mut cache = peg::ParseCache::default();
    let grammar = peg::Grammar::new("start <- \"abc\"").with_start_rule("start");
    let first =
        peg::parse_incremental_many("abc", &grammar, peg::ParserConfig::default(), &mut cache);
    let second =
        peg::parse_incremental_many("abc", &grammar, peg::ParserConfig::default(), &mut cache);
    assert_eq!(format!("{:?}", first), format!("{:?}", second));
    assert_eq!(cache.entries.len(), 1);
}

#[test]
fn parse_incremental_cache_invalidates_on_grammar_change() {
    let mut cache = peg::ParseCache::default();
    let grammar = peg::Grammar::new("start <- \"ab\"").with_start_rule("start");
    let other_grammar = peg::Grammar::new("start <- \"ac\"").with_start_rule("start");

    let first =
        peg::parse_incremental_many("ab", &grammar, peg::ParserConfig::default(), &mut cache);
    let second = peg::parse_incremental_many(
        "ac",
        &other_grammar,
        peg::ParserConfig::default(),
        &mut cache,
    );

    assert_eq!(first, peg::ParseValue::Text("ab".to_string()));
    assert_eq!(second, peg::ParseValue::Text("ac".to_string()));
    assert_eq!(cache.entries.len(), 2);
}

#[test]
fn incremental_cache_invalidates_on_inline_import_change() {
    let mut cache = peg::ParseCache::default();
    let import_v1 = peg::Grammar::new("rule <- \"x\"")
        .with_start_rule("rule")
        .with_metadata(
            "__grammar__",
            [("version".to_string(), serde_json::json!(1))]
                .into_iter()
                .collect(),
        );
    let import_v2 = peg::Grammar::new("rule <- \"x\"")
        .with_start_rule("rule")
        .with_metadata(
            "__grammar__",
            [("version".to_string(), serde_json::json!(2))]
                .into_iter()
                .collect(),
        );
    let grammar_v1 = peg::Grammar::new("start <- other::rule")
        .with_start_rule("start")
        .with_import("other", import_v1);
    let grammar_v2 = peg::Grammar::new("start <- other::rule")
        .with_start_rule("start")
        .with_import("other", import_v2);

    let first =
        peg::parse_incremental_many("x", &grammar_v1, peg::ParserConfig::default(), &mut cache);
    let second =
        peg::parse_incremental_many("x", &grammar_v2, peg::ParserConfig::default(), &mut cache);

    assert_eq!(first, peg::ParseValue::Text("x".to_string()));
    assert_eq!(second, peg::ParseValue::Text("x".to_string()));
    assert_eq!(cache.entries.len(), 2);
}

#[test]
fn parse_incremental_cache_invalidates_on_runtime_signature_change() {
    let mut cache = peg::ParseCache::default();
    let grammar = peg::Grammar::new("start <- \"ab\"").with_start_rule("start");

    let plain =
        peg::parse_incremental_many("ab", &grammar, peg::ParserConfig::default(), &mut cache);
    let with_spans = peg::parse_incremental_many(
        "ab",
        &grammar,
        peg::ParserConfig::default().with_updates(true, None, None),
        &mut cache,
    );

    assert_eq!(plain, peg::ParseValue::Text("ab".to_string()));
    assert!(matches!(with_spans, peg::ParseValue::SpannedValue { .. }));
    assert_eq!(cache.entries.len(), 2);
}

#[test]
fn edits_to_sequential_are_filtered_by_offset() {
    let edits = vec![
        peg::IncrementalEdit::new_unchecked(4, 5, "Y"),
        peg::IncrementalEdit::new_unchecked(0, 2, "XYZ"),
    ];
    let sequential = peg::snapshot_edits_to_sequential("abcdef", &edits);
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
    let rebuilt = peg::apply_edits("abcdef", &sequential);
    assert_eq!(rebuilt, "XYZcdeY");
}

#[test]
fn clone_grammar_resets_analysis_cache() {
    let mut base = peg::Grammar::new("a <- [a]");
    let _ = peg::analyze_and_store(&mut base);
    let cloned = peg::clone_grammar(
        &base,
        Some(peg::GrammarPatch {
            source: "b <- [b]".to_string(),
            start_rule: Some("b".to_string()),
        }),
    );
    assert_eq!(cloned.start_rule, "b");
    assert!(cloned.state.analysis_state.is_none());
}
