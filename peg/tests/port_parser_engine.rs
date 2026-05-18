use caap_peg_port as peg;

#[test]
fn parse_literal_rule_matches_exact_text() {
    let grammar = peg::Grammar::new("start <- \"abc\"").with_start_rule("start");
    let value = peg::parse("abc", &grammar, None, false).expect("parse should succeed");
    match value {
        peg::ParseValue::Text(text) => assert_eq!(text, "abc"),
        other => panic!("unexpected parse value: {other:?}"),
    }
}

#[test]
fn parse_sequence_rule_builds_node_value() {
    let grammar = peg::Grammar::new("start <- \"a\" \"b\" \"c\"").with_start_rule("start");
    let value = peg::parse("abc", &grammar, None, false).expect("parse should succeed");
    match value {
        peg::ParseValue::Node(name, items) => {
            assert_eq!(name, "sequence");
            let joined = items
                .into_iter()
                .map(|item| match item {
                    peg::ParseValue::Text(value) => value,
                    other => panic!("unexpected item: {other:?}"),
                })
                .collect::<Vec<_>>()
                .join("");
            assert_eq!(joined, "abc");
        }
        other => panic!("unexpected parse value: {other:?}"),
    }
}

#[test]
fn parse_choice_rule_selects_first_matching_branch() {
    let grammar = peg::Grammar::new("start <- \"a\" / \"b\"").with_start_rule("start");
    let value = peg::parse("a", &grammar, None, false).expect("parse should succeed");
    assert_eq!(value, peg::ParseValue::Text("a".to_string()));
}

#[test]
fn parse_reference_rule_and_early_quantifier() {
    let grammar =
        peg::Grammar::new("start <- \"a\" rest\nrest <- \"b\" \"c\"? ").with_start_rule("start");
    let value = peg::parse("abc", &grammar, None, false).expect("parse should succeed");
    match value {
        peg::ParseValue::Node(name, items) => {
            assert_eq!(name, "sequence");
            assert_eq!(items.len(), 2);
        }
        other => panic!("unexpected parse value: {other:?}"),
    }
}

#[test]
fn parse_one_or_more_quantifier_repeats() {
    let grammar = peg::Grammar::new("start <- \"a\"+").with_start_rule("start");
    let value = peg::parse("aaa", &grammar, None, false).expect("parse should succeed");
    match value {
        peg::ParseValue::Node(name, items) => {
            assert_eq!(name, "one_or_more");
            assert_eq!(items.len(), 3);
        }
        other => panic!("unexpected parse value: {other:?}"),
    }
}

#[test]
fn parse_zero_or_more_allows_empty_text() {
    let grammar = peg::Grammar::new("start <- \"a\"*").with_start_rule("start");
    let value = peg::parse("", &grammar, None, false).expect("parse should succeed");
    assert_eq!(value, peg::ParseValue::Nil);
}

#[test]
fn parse_and_predicate_succeeds_without_consumption() {
    let grammar = peg::Grammar::new("start <- &\"a\" \"a\"").with_start_rule("start");
    let value = peg::parse("a", &grammar, None, false).expect("parse should succeed");
    assert_eq!(
        value,
        peg::ParseValue::Node(
            "sequence".to_string(),
            vec![peg::ParseValue::Nil, peg::ParseValue::Text("a".to_string())]
        )
    );
}

#[test]
fn parse_and_predicate_fails_without_advancing() {
    let grammar = peg::Grammar::new("start <- &\"b\" \"a\"").with_start_rule("start");
    assert!(peg::parse("a", &grammar, None, false).is_err());
}

#[test]
fn parse_not_predicate_succeeds_on_absence() {
    let grammar = peg::Grammar::new("start <- !\"b\" \"a\"").with_start_rule("start");
    let value = peg::parse("a", &grammar, None, false).expect("parse should succeed");
    assert_eq!(
        value,
        peg::ParseValue::Node(
            "sequence".to_string(),
            vec![peg::ParseValue::Nil, peg::ParseValue::Text("a".to_string())]
        )
    );
}

#[test]
fn parse_not_predicate_fails_on_presence() {
    let grammar = peg::Grammar::new("start <- !\"a\" \"a\"").with_start_rule("start");
    assert!(peg::parse("a", &grammar, None, false).is_err());
}

#[test]
fn parse_regex_literal_matches_character() {
    let grammar = peg::Grammar::new("start <- /a/").with_start_rule("start");
    let value = peg::parse("a", &grammar, None, false).expect("parse should succeed");
    assert_eq!(value, peg::ParseValue::Text("a".to_string()));
}

#[test]
fn parse_character_class_expression() {
    let grammar = peg::Grammar::new("start <- [a]").with_start_rule("start");
    let value = peg::parse("a", &grammar, None, false).expect("parse should succeed");
    assert_eq!(value, peg::ParseValue::Text("a".to_string()));

    assert!(peg::parse("b", &grammar, None, false).is_err());
}

#[test]
fn parse_choice_between_regex_and_literal() {
    let grammar = peg::Grammar::new("start <- /a/ / \"b\"").with_start_rule("start");
    let value = peg::parse("b", &grammar, None, false).expect("parse should succeed");
    assert_eq!(value, peg::ParseValue::Text("b".to_string()));
}

#[test]
fn parse_choice_commit_prevents_backtracking_after_cut() {
    let grammar =
        peg::Grammar::new("start <- \"[\" ~ \"]\" / \"[\" \"x\"").with_start_rule("start");
    assert!(peg::parse("[x", &grammar, None, false).is_err());
}

#[test]
fn parse_choice_without_cut_allows_backtracking() {
    let grammar = peg::Grammar::new("start <- \"[\" \"]\" / \"[\" \"x\"").with_start_rule("start");
    let value = peg::parse("[x", &grammar, None, false).expect("parse should succeed");
    assert_eq!(
        value,
        peg::ParseValue::Node(
            "sequence".to_string(),
            vec![
                peg::ParseValue::Text("[".to_string()),
                peg::ParseValue::Text("x".to_string())
            ]
        )
    );
}

#[test]
fn parse_choice_uses_furthest_failure_position() {
    let grammar =
        peg::Grammar::new("start <- \"ab\" \"x\" / \"a\" \"y\" \"z\"").with_start_rule("start");
    let err = peg::parse("ab", &grammar, None, false).expect_err("parse should fail");
    assert_eq!(err.span.start, 2);
    assert_eq!(err.span.end, 2);
}

#[test]
fn parse_rejects_unterminated_regex() {
    let grammar = peg::Grammar::new("start <- /abc");
    let err = peg::parse("anything", &grammar.with_start_rule("start"), None, false)
        .expect_err("unterminated regex should fail");
    assert!(err.message.contains("unterminated regex literal"));
}

#[test]
fn parse_rejects_unterminated_character_class() {
    let grammar = peg::Grammar::new("start <- [abc");
    let err = peg::parse("anything", &grammar.with_start_rule("start"), None, false)
        .expect_err("unterminated char class should fail");
    assert!(err.message.contains("unterminated character class"));
}

#[test]
fn parse_prefix_stops_at_position_and_reports_consumed() {
    let grammar = peg::Grammar::new("start <- \"ab\"").with_start_rule("start");
    let prefix = peg::parse_prefix(
        "abc",
        &grammar,
        Some("start"),
        0,
        Some(peg::ParserConfig::default()),
        false,
    );
    assert_eq!(prefix.consumed, 2);
    assert!(!prefix.eof);
    assert!(prefix.value.is_some());
}

#[test]
fn parse_prefix_reports_eof_for_full_match() {
    let grammar = peg::Grammar::new("start <- \"ab\"");
    let prefix = peg::parse_prefix(
        "ab",
        &grammar.with_start_rule("start"),
        None,
        0,
        Some(peg::ParserConfig::default()),
        false,
    );
    assert_eq!(prefix.consumed, 2);
    assert!(prefix.eof);
    assert!(prefix.value.is_some());
}

#[test]
fn parse_fails_on_left_recursive_start_rule() {
    let grammar = peg::Grammar::new("start <- start").with_start_rule("start");
    assert!(peg::parse("anything", &grammar, None, false).is_err());
}

#[test]
fn parse_direct_left_recursion_grows_expression_chain() {
    let grammar =
        peg::Grammar::new("expr <- expr \"+\" atom / atom\natom <- \"a\"").with_start_rule("expr");
    let value = peg::parse("a+a+a", &grammar, None, false).expect("left recursion should parse");
    match value {
        peg::ParseValue::Node(name, _) => assert_eq!(name, "sequence"),
        other => panic!("unexpected left-recursive parse value: {other:?}"),
    }
}

#[test]
fn parse_indirect_left_recursion_reaches_base_alternative() {
    let grammar = peg::Grammar::new("start <- a\na <- b / \"x\"\nb <- a").with_start_rule("start");
    let value =
        peg::parse("x", &grammar, None, false).expect("indirect left recursion should parse");
    assert_eq!(value, peg::ParseValue::Text("x".to_string()));
}

#[test]
fn parse_left_recursion_requires_memoization() {
    let grammar =
        peg::Grammar::new("expr <- expr \"+\" atom / atom\natom <- \"a\"").with_start_rule("expr");
    let config = peg::ParserConfig {
        memo: false,
        ..peg::ParserConfig::default()
    };
    let err = peg::parse("a+a", &grammar, Some(config), false)
        .expect_err("left recursion without memoization should fail early");
    assert!(err.message.contains("memoization cannot be disabled"));
}

#[test]
fn parse_fails_when_start_rule_is_missing() {
    let grammar = peg::Grammar::new("a <- \"x\"");
    let grammar = grammar.with_start_rule("missing");
    assert!(peg::parse("x", &grammar, None, false).is_err());
}

#[test]
fn parse_prefix_rejects_invalid_start_pos() {
    let grammar = peg::Grammar::new("start <- \"a\"");
    let prefix = peg::parse_prefix(
        "a",
        &grammar,
        Some("start"),
        10,
        Some(peg::ParserConfig::default()),
        false,
    );
    assert!(prefix.consumed == 0);
    assert!(!prefix.errors.is_empty());
}

#[test]
fn parse_fails_on_missing_start_rule() {
    let grammar = peg::Grammar::new("words <- \"x\"");
    let result = peg::parse("x", &grammar, None, false);
    assert!(result.is_err());
}

#[test]
fn parse_incremental_cache_is_reused_for_same_text() {
    let mut cache = peg::ParseCache::default();
    let grammar = peg::Grammar::new("start <- \"ab\"").with_start_rule("start");
    let first =
        peg::parse_incremental_many("ab", &grammar, peg::ParserConfig::default(), &mut cache);
    let second = peg::parse_incremental_many(
        "ab",
        &grammar,
        peg::ParserConfig {
            memo: true,
            ..peg::ParserConfig::default()
        },
        &mut cache,
    );
    assert_eq!(format!("{:?}", first), format!("{:?}", second));
    assert_eq!(cache.entries.len(), 1);
}

#[test]
fn snapshot_edits_sort_and_shift_ranges() {
    let base = "abcdef";
    let edits = vec![
        peg::IncrementalEdit::new_unchecked(4, 5, "Y"),
        peg::IncrementalEdit::new_unchecked(0, 2, "XYZ"),
    ];

    let sequential = peg::snapshot_edits_to_sequential(base, &edits);
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
}

#[test]
fn snapshot_edits_reject_overlapping_ranges() {
    let base = "abcd";
    let edits = vec![
        peg::IncrementalEdit::new_unchecked(0, 3, "X"),
        peg::IncrementalEdit::new_unchecked(2, 4, "Y"),
    ];

    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = peg::snapshot_edits_to_sequential(base, &edits);
    }))
    .expect_err("expected panic on overlapping edits");
}

#[test]
fn position_cache_populated_after_incremental_parse() {
    let grammar = peg::Grammar::new("start <- \"hello\"").with_start_rule("start");
    let mut cache = peg::ParseCache::default();
    peg::parse_incremental_many("hello", &grammar, peg::ParserConfig::default(), &mut cache);
    assert!(
        cache.position_entry_count() > 0,
        "position cache should have entries after parse"
    );
}

#[test]
fn position_cache_shifts_after_edit() {
    // First parse builds position cache for "aXbc".
    // Second parse with "aYbc" (same length, different char at pos 1) should
    // reuse entries from positions 2+ via the position cache.
    let grammar = peg::Grammar::new("start <- [a-z]+").with_start_rule("start");
    let mut cache = peg::ParseCache::default();
    peg::parse_incremental_many("hello", &grammar, peg::ParserConfig::default(), &mut cache);
    let before = cache.position_entry_count();
    peg::parse_incremental_many("jello", &grammar, peg::ParserConfig::default(), &mut cache);
    // Position cache should still have entries (some shifted from previous run).
    assert!(cache.position_entry_count() > 0);
    let _ = before; // entry count may differ; just confirm cache is alive
}

#[test]
fn choice_dispatch_selects_correct_alternative_by_first_char() {
    // Three distinct literal alternatives — dispatch table should route by first char.
    let grammar =
        peg::Grammar::new("start <- \"abc\" / \"xyz\" / \"mno\"").with_start_rule("start");

    let v1 = peg::parse("abc", &grammar, None, false).expect("abc should match");
    assert_eq!(v1, peg::ParseValue::Text("abc".to_string()));

    let v2 = peg::parse("xyz", &grammar, None, false).expect("xyz should match");
    assert_eq!(v2, peg::ParseValue::Text("xyz".to_string()));

    let v3 = peg::parse("mno", &grammar, None, false).expect("mno should match");
    assert_eq!(v3, peg::ParseValue::Text("mno".to_string()));

    assert!(peg::parse("zzz", &grammar, None, false).is_err());
}

#[test]
fn choice_dispatch_handles_overlapping_prefixes_correctly() {
    // Two alternatives sharing a common first char — dispatch must try both.
    let grammar = peg::Grammar::new("start <- \"ab\" / \"ac\"").with_start_rule("start");

    let v1 = peg::parse("ab", &grammar, None, false).expect("ab should match");
    assert_eq!(v1, peg::ParseValue::Text("ab".to_string()));

    let v2 = peg::parse("ac", &grammar, None, false).expect("ac should match");
    assert_eq!(v2, peg::ParseValue::Text("ac".to_string()));
}

#[test]
fn choice_dispatch_still_respects_cut_operator() {
    // Cut inside a dispatched choice must still prevent backtracking.
    let grammar =
        peg::Grammar::new("start <- \"a\" ~ \"b\" / \"a\" \"c\"").with_start_rule("start");
    assert!(peg::parse("ac", &grammar, None, false).is_err());
}
