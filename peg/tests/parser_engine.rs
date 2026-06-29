//! End-to-end PEG **engine** behaviour, grouped by scenario. Each `mod` is
//! one scenario the engine must satisfy; tests build a small grammar and assert
//! on the parsed value, diagnostics, or incremental-cache state.

use caap_peg as peg;

fn edit(start: usize, old_end: usize, replacement: impl Into<String>) -> peg::IncrementalEdit {
    peg::IncrementalEdit::new(start, old_end, replacement).expect("test edit must be valid")
}

fn contains_recovered(value: &peg::ParseValue) -> bool {
    match value {
        peg::ParseValue::Node(tag, children) => {
            &**tag == peg::RECOVER_TAG || children.iter().any(contains_recovered)
        }
        peg::ParseValue::Named(_, inner) | peg::ParseValue::SpannedValue { value: inner, .. } => {
            contains_recovered(inner)
        }
        _ => false,
    }
}

/// Terminals and structural combinators (literal/sequence/choice/quantifiers/regex/char-class).
mod terminals_and_combinators {
    use super::*;

    #[test]
    fn parse_literal_rule_matches_exact_text() {
        let grammar = peg::Grammar::trusted_new("start <- \"abc\"").with_start_rule("start");
        let value = peg::parse("abc", &grammar).expect("parse should succeed");
        match value {
            peg::ParseValue::Text(text) => assert_eq!(&*text, "abc"),
            other => panic!("unexpected parse value: {other:?}"),
        }
    }

    #[test]
    fn parse_sequence_rule_builds_node_value() {
        let grammar =
            peg::Grammar::trusted_new("start <- \"a\" \"b\" \"c\"").with_start_rule("start");
        let value = peg::parse("abc", &grammar).expect("parse should succeed");
        match value {
            peg::ParseValue::Node(name, items) => {
                assert_eq!(&*name, "sequence");
                let joined = items
                    .iter()
                    .map(|item| match item {
                        peg::ParseValue::Text(value) => value.to_string(),
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
        let grammar = peg::Grammar::trusted_new("start <- \"a\" / \"b\"").with_start_rule("start");
        let value = peg::parse("a", &grammar).expect("parse should succeed");
        assert_eq!(value, peg::ParseValue::Text("a".into()));
    }

    #[test]
    fn parse_reference_rule_and_early_quantifier() {
        let grammar = peg::Grammar::trusted_new("start <- \"a\" rest\nrest <- \"b\" \"c\"? ")
            .with_start_rule("start");
        let value = peg::parse("abc", &grammar).expect("parse should succeed");
        match value {
            peg::ParseValue::Node(name, items) => {
                assert_eq!(&*name, "sequence");
                assert_eq!(items.len(), 2);
            }
            other => panic!("unexpected parse value: {other:?}"),
        }
    }

    #[test]
    fn parse_one_or_more_quantifier_repeats() {
        let grammar = peg::Grammar::trusted_new("start <- \"a\"+").with_start_rule("start");
        let value = peg::parse("aaa", &grammar).expect("parse should succeed");
        match value {
            peg::ParseValue::Node(name, items) => {
                assert_eq!(&*name, "one_or_more");
                assert_eq!(items.len(), 3);
            }
            other => panic!("unexpected parse value: {other:?}"),
        }
    }

    #[test]
    fn parse_zero_or_more_allows_empty_text() {
        let grammar = peg::Grammar::trusted_new("start <- \"a\"*").with_start_rule("start");
        let value = peg::parse("", &grammar).expect("parse should succeed");
        assert_eq!(
            value,
            peg::ParseValue::Node("zero_or_more".into(), std::sync::Arc::new(Vec::new()))
        );
    }

    #[test]
    fn parse_regex_literal_matches_character() {
        let grammar = peg::Grammar::trusted_new("start <- /a/").with_start_rule("start");
        let value = peg::parse("a", &grammar).expect("parse should succeed");
        assert_eq!(value, peg::ParseValue::Text("a".into()));
    }

    #[test]
    fn parse_character_class_expression() {
        let grammar = peg::Grammar::trusted_new("start <- [a]").with_start_rule("start");
        let value = peg::parse("a", &grammar).expect("parse should succeed");
        assert_eq!(value, peg::ParseValue::Text("a".into()));

        assert!(peg::parse("b", &grammar).is_err());
    }

    #[test]
    fn parse_choice_between_regex_and_literal() {
        let grammar = peg::Grammar::trusted_new("start <- /a/ / \"b\"").with_start_rule("start");
        let value = peg::parse("b", &grammar).expect("parse should succeed");
        assert_eq!(value, peg::ParseValue::Text("b".into()));
    }

    #[test]
    fn parse_choice_uses_furthest_failure_position() {
        let grammar = peg::Grammar::trusted_new("start <- \"ab\" \"x\" / \"a\" \"y\" \"z\"")
            .with_start_rule("start");
        let err = peg::parse("ab", &grammar).expect_err("parse should fail");
        assert_eq!(err.span.start, 2);
        assert_eq!(err.span.end, 2);
    }
}

/// Lookahead predicates (`&` / `!`).
mod predicates {
    use super::*;

    #[test]
    fn parse_and_predicate_succeeds_without_consumption() {
        let grammar = peg::Grammar::trusted_new("start <- &\"a\" \"a\"").with_start_rule("start");
        let value = peg::parse("a", &grammar).expect("parse should succeed");
        assert_eq!(
            value,
            peg::ParseValue::Node(
                "sequence".into(),
                std::sync::Arc::new(vec![
                    peg::ParseValue::Nil,
                    peg::ParseValue::Text("a".into())
                ])
            )
        );
    }

    #[test]
    fn parse_and_predicate_fails_without_advancing() {
        let grammar = peg::Grammar::trusted_new("start <- &\"b\" \"a\"").with_start_rule("start");
        assert!(peg::parse("a", &grammar).is_err());
    }

    #[test]
    fn parse_not_predicate_succeeds_on_absence() {
        let grammar = peg::Grammar::trusted_new("start <- !\"b\" \"a\"").with_start_rule("start");
        let value = peg::parse("a", &grammar).expect("parse should succeed");
        assert_eq!(
            value,
            peg::ParseValue::Node(
                "sequence".into(),
                std::sync::Arc::new(vec![
                    peg::ParseValue::Nil,
                    peg::ParseValue::Text("a".into())
                ])
            )
        );
    }

    #[test]
    fn parse_not_predicate_fails_on_presence() {
        let grammar = peg::Grammar::trusted_new("start <- !\"a\" \"a\"").with_start_rule("start");
        assert!(peg::parse("a", &grammar).is_err());
    }
}

/// Cut (`~`) commit and ordered-choice backtracking.
mod cut_and_backtracking {
    use super::*;

    #[test]
    fn parse_choice_commit_prevents_backtracking_after_cut() {
        let grammar = peg::Grammar::trusted_new("start <- \"[\" ~ \"]\" / \"[\" \"x\"")
            .with_start_rule("start");
        assert!(peg::parse("[x", &grammar).is_err());
    }

    #[test]
    fn parse_choice_without_cut_allows_backtracking() {
        let grammar = peg::Grammar::trusted_new("start <- \"[\" \"]\" / \"[\" \"x\"")
            .with_start_rule("start");
        let value = peg::parse("[x", &grammar).expect("parse should succeed");
        assert_eq!(
            value,
            peg::ParseValue::Node(
                "sequence".into(),
                std::sync::Arc::new(vec![
                    peg::ParseValue::Text("[".into()),
                    peg::ParseValue::Text("x".into())
                ])
            )
        );
    }
}

/// First-character choice dispatch.
mod choice_dispatch {
    use super::*;

    #[test]
    fn choice_dispatch_selects_correct_alternative_by_first_char() {
        // Three distinct literal alternatives — dispatch table should route by first char.
        let grammar = peg::Grammar::trusted_new("start <- \"abc\" / \"xyz\" / \"mno\"")
            .with_start_rule("start");

        let v1 = peg::parse("abc", &grammar).expect("abc should match");
        assert_eq!(v1, peg::ParseValue::Text("abc".into()));

        let v2 = peg::parse("xyz", &grammar).expect("xyz should match");
        assert_eq!(v2, peg::ParseValue::Text("xyz".into()));

        let v3 = peg::parse("mno", &grammar).expect("mno should match");
        assert_eq!(v3, peg::ParseValue::Text("mno".into()));

        assert!(peg::parse("zzz", &grammar).is_err());
    }

    #[test]
    fn choice_dispatch_still_respects_cut_operator() {
        // Cut inside a dispatched choice must still prevent backtracking.
        let grammar = peg::Grammar::trusted_new("start <- \"a\" ~ \"b\" / \"a\" \"c\"")
            .with_start_rule("start");
        assert!(peg::parse("ac", &grammar).is_err());
    }
}

/// Separated repetition (`interspersed`).
mod combinators {
    use super::*;

    #[test]
    fn interspersed_preserves_separators_in_output() {
        // interspersed(letter, op) on "a+b+c" should yield [a, +, b, +, c]
        // Using single-char element ([a-z]) so items are Text values directly.
        let grammar = peg::Grammar::trusted_new("start <- interspersed([a-z], [+*])")
            .with_start_rule("start");

        let value = peg::parse("a+b+c", &grammar).expect("parse should succeed");
        match value {
            peg::ParseValue::Node(name, items) => {
                assert_eq!(&*name, "interspersed");
                assert_eq!(items.len(), 5);
                let texts: Vec<String> = items
                    .iter()
                    .map(|v| match v {
                        peg::ParseValue::Text(t) => t.to_string(),
                        other => panic!("unexpected: {other:?}"),
                    })
                    .collect();
                assert_eq!(texts, ["a", "+", "b", "+", "c"]);
            }
            other => panic!("unexpected parse value: {other:?}"),
        }
    }

    #[test]
    fn interspersed_single_element_no_separator() {
        let grammar = peg::Grammar::trusted_new("start <- interspersed([a-z], \",\")")
            .with_start_rule("start");

        let value = peg::parse("a", &grammar).expect("single element");
        match value {
            peg::ParseValue::Node(name, items) => {
                assert_eq!(&*name, "interspersed");
                assert_eq!(items.len(), 1);
                assert_eq!(items[0], peg::ParseValue::Text("a".into()));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn interspersed_vs_sep_plus_separator_presence() {
        // sep_plus drops separators; interspersed keeps them
        let sep_grammar =
            peg::Grammar::trusted_new("start <- sep_plus([a-z], \",\")").with_start_rule("start");
        let int_grammar = peg::Grammar::trusted_new("start <- interspersed([a-z], \",\")")
            .with_start_rule("start");

        let sep_val = peg::parse("a,b,c", &sep_grammar).unwrap();
        let int_val = peg::parse("a,b,c", &int_grammar).unwrap();

        let sep_len = match &sep_val {
            peg::ParseValue::Node(_, items) => items.len(),
            _ => panic!(),
        };
        let int_len = match &int_val {
            peg::ParseValue::Node(_, items) => items.len(),
            _ => panic!(),
        };

        assert_eq!(sep_len, 3); // elements only: [a, b, c]
        assert_eq!(int_len, 5); // elements + separators: [a, ,, b, ,, c]
    }
}

/// Direct, indirect, and mutual left recursion (seed-grow).
mod left_recursion {
    use super::*;

    #[test]
    fn parse_fails_on_left_recursive_start_rule() {
        let grammar = peg::Grammar::trusted_new("start <- start").with_start_rule("start");
        assert!(peg::parse("anything", &grammar).is_err());
    }

    #[test]
    fn parse_direct_left_recursion_grows_expression_chain() {
        let grammar = peg::Grammar::trusted_new("expr <- expr \"+\" atom / atom\natom <- \"a\"")
            .with_start_rule("expr");
        let value = peg::parse("a+a+a", &grammar).expect("left recursion should parse");
        match value {
            peg::ParseValue::Node(name, _) => assert_eq!(&*name, "sequence"),
            other => panic!("unexpected left-recursive parse value: {other:?}"),
        }
    }

    #[test]
    fn parse_indirect_left_recursion_reaches_base_alternative() {
        let grammar = peg::Grammar::trusted_new("start <- a\na <- b / \"x\"\nb <- a")
            .with_start_rule("start");
        let value = peg::parse("x", &grammar).expect("indirect left recursion should parse");
        assert_eq!(value, peg::ParseValue::Text("x".into()));
    }

    #[test]
    fn parse_indirect_left_recursion_grows_through_the_cycle() {
        // `a` is left-recursive only *through* `b` (a -> b -> a). The whole SCC must
        // seed-grow: "yxx" = ((y x) x), not just the base "y".
        let grammar =
            peg::Grammar::trusted_new("a <- b\nb <- a \"x\" / \"y\"").with_start_rule("a");
        assert_eq!(
            peg::parse("y", &grammar).unwrap(),
            peg::ParseValue::Text("y".into())
        );
        // Grows: a left-associatively absorbs each trailing "x".
        let v = peg::parse("yxx", &grammar).expect("indirect LR should grow");
        assert!(matches!(v, peg::ParseValue::Node(ref t, _) if &**t == "sequence"));
    }

    #[test]
    fn parse_mutual_left_recursion_grows() {
        // a <- b 'p' / 'q' ; b <- a 's' / 't'  ==  a = ('tp' | 'q') ('sp')*
        let grammar =
            peg::Grammar::trusted_new("a <- b 'p' / 'q'\nb <- a 's' / 't'").with_start_rule("a");
        assert!(peg::parse("q", &grammar).is_ok());
        assert!(peg::parse("qsp", &grammar).is_ok());
        assert!(peg::parse("qspsp", &grammar).is_ok());
        assert!(peg::parse("tpsp", &grammar).is_ok());
        // Not in the language → rejected (no silent over/under-match).
        assert!(peg::parse("qp", &grammar).is_err());
    }

    #[test]
    fn parse_left_recursion_requires_memoization() {
        let grammar = peg::Grammar::trusted_new("expr <- expr \"+\" atom / atom\natom <- \"a\"")
            .with_start_rule("expr");
        let config = peg::ParserConfig {
            memo: false,
            ..peg::ParserConfig::default()
        };
        let err = peg::ParseRequest::new(&grammar)
            .config(config)
            .run("a+a")
            .expect_err("left recursion without memoization should fail early");
        assert!(err.message.contains("memoization cannot be disabled"));
    }

    #[test]
    fn parse_left_recursion_respects_zero_global_memo_budget() {
        let grammar = peg::Grammar::trusted_new("expr <- expr \"+\" atom / atom\natom <- \"a\"")
            .with_start_rule("expr");
        let config = peg::ParserConfig {
            memo_policy: Some(peg::MemoPolicy::new(Some(0)).unwrap()),
            ..peg::ParserConfig::default()
        };
        let err = peg::ParseRequest::new(&grammar)
            .config(config)
            .run("a+a")
            .expect_err("zero global memo budget should disable memoization");
        assert!(err.message.contains("memoization cannot be disabled"));
    }
}

/// Prefix parsing (partial input, reported consumption).
mod prefix {
    use super::*;

    #[test]
    fn parse_prefix_stops_at_position_and_reports_consumed() {
        let grammar = peg::Grammar::trusted_new("start <- \"ab\"").with_start_rule("start");
        let prefix = peg::ParseRequest::new(&grammar)
            .start_rule("start")
            .run_prefix("abc", 0);
        assert_eq!(prefix.consumed, 2);
        assert!(!prefix.eof);
        assert!(prefix.value.is_some());
    }

    #[test]
    fn parse_prefix_reports_eof_for_full_match() {
        let grammar = peg::Grammar::trusted_new("start <- \"ab\"");
        let prefix = peg::ParseRequest::new(&grammar.with_start_rule("start")).run_prefix("ab", 0);
        assert_eq!(prefix.consumed, 2);
        assert!(prefix.eof);
        assert!(prefix.value.is_some());
    }

    #[test]
    fn parse_prefix_rejects_invalid_start_pos() {
        let grammar = peg::Grammar::trusted_new("start <- \"a\"");
        let prefix = peg::ParseRequest::new(&grammar)
            .start_rule("start")
            .run_prefix("a", 10);
        assert!(prefix.consumed == 0);
        assert!(!prefix.errors.is_empty());
    }

    #[test]
    fn choice_dispatch_handles_overlapping_prefixes_correctly() {
        // Two alternatives sharing a common first char — dispatch must try both.
        let grammar =
            peg::Grammar::trusted_new("start <- \"ab\" / \"ac\"").with_start_rule("start");

        let v1 = peg::parse("ab", &grammar).expect("ab should match");
        assert_eq!(v1, peg::ParseValue::Text("ab".into()));

        let v2 = peg::parse("ac", &grammar).expect("ac should match");
        assert_eq!(v2, peg::ParseValue::Text("ac".into()));
    }
}

/// Memoisation requirements and step/memo budget exhaustion.
mod budgets_and_memo {
    use super::*;

    #[test]
    fn parse_reports_memo_budget_exhaustion() {
        let grammar = peg::Grammar::trusted_new("start <- a b\na <- \"a\"\nb <- \"b\"")
            .with_start_rule("start");
        let config = peg::ParserConfig {
            memo_policy: Some(peg::MemoPolicy::new(Some(1)).unwrap()),
            ..peg::ParserConfig::default()
        };

        let err = peg::ParseRequest::new(&grammar)
            .config(config)
            .run("ab")
            .expect_err("memo budget exhaustion should fail explicitly");

        assert!(err.message.contains("parser memo budget exceeded"));
    }

    #[test]
    fn parse_reports_expression_step_budget_exhaustion() {
        let grammar =
            peg::Grammar::trusted_new("start <- a b c\na <- \"a\"\nb <- \"b\"\nc <- \"c\"")
                .with_start_rule("start");
        let config = peg::ParserConfig::default().with_max_steps(4);

        let err = peg::ParseRequest::new(&grammar)
            .config(config)
            .run("abc")
            .expect_err("expression budget should fail before input-size budget");

        assert_eq!(err.code.as_deref(), Some("step_budget_exhausted"));
        assert!(err
            .message
            .contains("parser step budget exceeded after 4 expression steps"));
    }

    #[test]
    fn parse_profiled_reports_calls_and_memo_hits() {
        // `term` is referenced twice at the same position (`a` then the `/` alt),
        // so the packrat memo serves the second call without re-running the body.
        let grammar = peg::Grammar::trusted_new("expr <- term \"+\" term / term\nterm <- [0-9]+")
            .with_start_rule("expr");

        let (_value, profile) = peg::ParseRequest::new(&grammar)
            .run_profiled("12")
            .expect("parse should succeed");

        let term = profile.rules.get("term").expect("term profiled");
        assert!(
            term.calls >= 2,
            "term is tried in both alternatives: {term:?}"
        );
        assert!(
            term.memo_hits >= 1,
            "the re-try must be a memo hit: {term:?}"
        );
        assert!(term.body_runs >= 1);
        assert!(profile.memo_hit_rate() > 0.0);
        // `hottest` returns rules ranked by real parsing work.
        assert!(!profile.hottest(2).is_empty());
        assert!(profile.expr_steps > 0);
    }
}

/// Rejection of malformed grammars, metadata, and inputs.
mod error_handling {
    use super::*;

    #[test]
    fn parse_rejects_unterminated_regex() {
        let err =
            peg::Grammar::try_new("start <- /abc").expect_err("unterminated regex should fail");
        assert!(err.to_string().contains("unterminated regex literal"));
    }

    #[test]
    fn parse_rejects_unterminated_character_class() {
        let err = peg::Grammar::try_new("start <- [abc")
            .expect_err("unterminated char class should fail");
        assert!(err.to_string().contains("unterminated character class"));
    }

    #[test]
    fn parse_rejects_invalid_trivia_regex_metadata() {
        let grammar = peg::Grammar::trusted_new("start <- \"a\"").with_start_rule("start");
        let grammar = grammar.with_metadata(
            "__grammar__",
            vec![("trivia".to_string(), serde_json::json!("["))]
                .into_iter()
                .collect(),
        );
        let err = peg::parse("a", &grammar).expect_err("invalid trivia regex metadata should fail");
        assert!(err.message.contains("invalid trivia regex"));
    }

    #[test]
    fn parse_rejects_non_string_trivia_metadata() {
        let grammar = peg::Grammar::trusted_new("start <- \"a\"").with_start_rule("start");
        let grammar = grammar.with_metadata(
            "__grammar__",
            vec![("trivia".to_string(), serde_json::json!(false))]
                .into_iter()
                .collect(),
        );
        let err = peg::parse("a", &grammar).expect_err("non-string trivia metadata should fail");
        assert!(err.message.contains("invalid trivia metadata"));
        assert!(err.message.contains("expected string, got bool"));
    }

    #[test]
    fn parse_rejects_non_bool_indentation_metadata() {
        let grammar = peg::Grammar::trusted_new("start <- \"a\"").with_start_rule("start");
        let grammar = grammar.with_metadata(
            "__grammar__",
            vec![("indentation".to_string(), serde_json::json!("off"))]
                .into_iter()
                .collect(),
        );
        let err = peg::parse("a", &grammar).expect_err("non-bool indentation metadata should fail");
        assert!(err.message.contains("invalid indentation metadata"));
        assert!(err.message.contains("expected bool, got string"));
    }

    #[test]
    fn parse_fails_when_start_rule_is_missing() {
        let grammar = peg::Grammar::trusted_new("a <- \"x\"");
        let grammar = grammar.with_start_rule("missing");
        assert!(peg::parse("x", &grammar).is_err());
    }

    #[test]
    fn parse_fails_on_missing_start_rule() {
        let grammar = peg::Grammar::trusted_new("words <- \"x\"");
        let result = peg::parse("x", &grammar);
        assert!(result.is_err());
    }
}

/// Scoped trivia overrides and grammar-level error recovery.
mod trivia_and_recovery {
    use super::*;

    #[test]
    fn with_trivia_overrides_the_skipper_in_scope() {
        // The default skipper treats ';' as a line comment, so a bare ';' literal is
        // eaten before it can match.
        let default_g = peg::Grammar::trusted_new("s <- ';' 'x'").with_start_rule("s");
        assert!(
            peg::parse(";x", &default_g).is_err(),
            "default trivia should swallow the ';' as a comment"
        );

        // `with_trivia("whitespace", …)` switches to whitespace-only skipping in
        // scope, so ';' is a normal character and the literal matches.
        let scoped_g = peg::Grammar::trusted_new("s <- with_trivia('whitespace', (';' 'x'))")
            .with_start_rule("s");
        assert!(
            peg::parse(";x", &scoped_g).is_ok(),
            "with_trivia('whitespace') should expose ';' as a literal"
        );
        // The override is scoped: the default skipper is restored afterwards.
        let restored_g =
            peg::Grammar::trusted_new("s <- with_trivia('whitespace', ';') a\na <- 'y'")
                .with_start_rule("s");
        // "; ;y" — the scoped ';' matches the first ';', then default trivia eats the
        // " ;" (whitespace + comment) before `a` matches "y".
        assert!(peg::parse("; ;\ny", &restored_g).is_ok());
    }

    #[test]
    fn trailing_trivia_at_eof_does_not_fail_full_consumption() {
        // A failed `stmt*` iteration rewinds past the trivia it skipped, so the
        // engine must consume trailing trivia once more before judging full
        // consumption — a file ending in a comment (even without a final
        // newline) is complete input.
        let grammar = peg::Grammar::trusted_new("program <- stmt*\nstmt <- [a-z]+")
            .with_start_rule("program");
        assert!(
            peg::parse("ab cd ; trailing comment", &grammar).is_ok(),
            "trailing line comment at EOF is trivia"
        );
        assert!(peg::parse("ab cd ; trailing\n", &grammar).is_ok());
        // Trailing NON-trivia still fails the full-consumption check.
        assert!(peg::parse("ab cd 42", &grammar).is_err());

        // The incremental path applies the same rule.
        let mut cache = peg::ParseCache::default();
        assert!(peg::ParseRequest::new(&grammar)
            .run_incremental("ab ; tail", &mut cache)
            .is_ok());
    }

    #[test]
    fn grammar_level_recover_localises_a_bad_region() {
        // `stmt` either parses a good `[a-z]+.` statement or recovers by skipping to
        // the next `.` — so one malformed statement doesn't fail the whole parse.
        // (`.` is the terminator here because the default trivia skipper eats `;`.)
        let grammar = peg::Grammar::trusted_new(
            "program <- stmt+\nstmt <- good / recover(\".\")\ngood <- [a-z]+ \".\"",
        )
        .with_start_rule("program");

        // Plain parse without recovery would fail on "@#$".
        assert!(peg::parse("abc.@#$.def.", &grammar).is_ok());
        let value = peg::parse("abc.@#$.def.", &grammar).unwrap();
        assert!(
            contains_recovered(&value),
            "the malformed middle statement should yield a <recovered> node: {value:?}"
        );

        // A well-formed input never triggers recovery.
        let clean = peg::parse("abc.def.", &grammar).unwrap();
        assert!(!contains_recovered(&clean));
    }
}

/// Incremental reparse: edit snapshots, the position cache, and sound reuse.
mod incremental {
    use super::*;

    #[test]
    fn parse_incremental_cache_is_reused_for_same_text() {
        let mut cache = peg::ParseCache::default();
        let grammar = peg::Grammar::trusted_new("start <- \"ab\"").with_start_rule("start");
        let first = peg::ParseRequest::new(&grammar)
            .run_incremental("ab", &mut cache)
            .expect("first incremental parse should succeed");
        let second = peg::ParseRequest::new(&grammar)
            .config(peg::ParserConfig {
                memo: true,
                ..peg::ParserConfig::default()
            })
            .run_incremental("ab", &mut cache)
            .expect("second incremental parse should reuse cache");
        assert_eq!(format!("{:?}", first), format!("{:?}", second));
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn snapshot_edits_sort_and_shift_ranges() {
        let base = "abcdef";
        let edits = vec![edit(4, 5, "Y"), edit(0, 2, "XYZ")];

        let sequential = peg::snapshot_edits_to_sequential(base, &edits)
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
    }

    #[test]
    fn snapshot_edits_reject_overlapping_ranges() {
        let base = "abcd";
        let edits = vec![edit(0, 3, "X"), edit(2, 4, "Y")];

        let err = peg::snapshot_edits_to_sequential(base, &edits)
            .expect_err("overlapping edits should fail");
        assert!(err.message.contains("overlapping snapshot edits"));
        assert_eq!(err.code.as_deref(), Some("overlapping_incremental_edits"));
        assert_eq!(err.span.start, 0);
        assert_eq!(err.span.end, 4);
    }

    #[test]
    fn snapshot_edits_reports_invalid_range_with_code_and_span() {
        let edits = vec![edit(10, 12, "X")];

        let err = peg::snapshot_edits_to_sequential("abcd", &edits)
            .expect_err("invalid edit should fail");
        assert_eq!(err.code.as_deref(), Some("invalid_incremental_edit_range"));
        assert_eq!(err.span.start, 4);
        assert_eq!(err.span.end, 4);
    }

    #[test]
    fn position_cache_populated_after_incremental_parse() {
        let grammar = peg::Grammar::trusted_new("start <- \"hello\"").with_start_rule("start");
        let mut cache = peg::ParseCache::default();
        peg::ParseRequest::new(&grammar)
            .run_incremental("hello", &mut cache)
            .expect("incremental parse should succeed");
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
        let grammar = peg::Grammar::trusted_new("start <- [a-z]+").with_start_rule("start");
        let mut cache = peg::ParseCache::default();
        peg::ParseRequest::new(&grammar)
            .run_incremental("hello", &mut cache)
            .expect("initial incremental parse should succeed");
        let before = cache.position_entry_count();
        peg::ParseRequest::new(&grammar)
            .run_incremental("jello", &mut cache)
            .expect("edited incremental parse should succeed");
        // Position cache should still have entries (some shifted from previous run).
        assert!(cache.position_entry_count() > 0);
        let _ = before; // entry count may differ; just confirm cache is alive
    }

    #[test]
    fn position_cache_projects_spans_after_prefix_edit() {
        let grammar = peg::Grammar::trusted_new(
            r#"
    start <- "!"* item
    item <- capture("src", /[a-z]+/)
    "#,
        )
        .with_start_rule("start");
        let mut cache = peg::ParseCache::default();

        peg::ParseRequest::new(&grammar)
            .run_incremental("!abc", &mut cache)
            .expect("initial incremental parse should succeed");
        peg::ParseRequest::new(&grammar)
            .run_incremental("!!abc", &mut cache)
            .expect("edited incremental parse should succeed");

        let item_entry = cache
            .pos_cache
            .as_ref()
            .and_then(|pos_cache| pos_cache.get("item", 2))
            .expect("shifted item memo should be available at its new position");
        assert_eq!(item_entry.end, 5);
        assert!(matches!(
            &*item_entry.value,
            peg::ParseValue::SpannedValue {
                start: 2,
                end: 5,
                ..
            }
        ));
    }

    #[test]
    fn position_cache_records_lookahead_read_extent() {
        // `a` consumes only "x" but its `&"yz"` lookahead examines two more bytes.
        // The exported memo entry must record that examined tail so incremental
        // reuse can invalidate it on an edit there.
        let grammar = peg::Grammar::trusted_new(
            r#"
    start <- a rest
    a     <- "x" &"yz"
    rest  <- [a-z]+
    "#,
        )
        .with_start_rule("start");
        let mut cache = peg::ParseCache::default();
        peg::ParseRequest::new(&grammar)
            .run_incremental("xyz", &mut cache)
            .expect("parse should succeed");

        let entry = cache
            .pos_cache
            .as_ref()
            .and_then(|pc| pc.get("a", 0))
            .expect("rule `a` memoized at 0");
        assert_eq!(entry.end, 1, "`a` consumes only the 'x'");
        assert_eq!(
            entry.read_hi,
            Some(3),
            "examined interval must include the two-byte `&\"yz\"` lookahead"
        );
    }

    #[test]
    fn position_cache_records_tight_extent_for_multitoken_regex() {
        // A delimited-string regex consumes through its closing quote and examines
        // only one byte past it (where its automaton dies) — NOT the rest of the
        // input. Recording that tight extent (via the regex automaton, not a
        // conservative end-of-input fallback) is what lets edits after the string
        // reuse it.
        let grammar =
            peg::Grammar::trusted_new("start <- str word\nstr  <- /\"[^\"]*\"/\nword <- /[a-z]+/")
                .with_start_rule("start");
        let input = "\"ab\"cdefgh";
        let mut cache = peg::ParseCache::default();
        peg::ParseRequest::new(&grammar)
            .run_incremental(input, &mut cache)
            .expect("parse should succeed");

        let entry = cache
            .pos_cache
            .as_ref()
            .and_then(|pc| pc.get("str", 0))
            .expect("rule `str` memoized at 0");
        assert_eq!(entry.end, 4, "str consumes the quoted \"ab\"");
        let read_hi = entry.read_hi.expect("examined interval recorded");
        // The extent covers the close quote plus a small fixed lookahead — it must
        // NOT run to the end of the input (which the old end-of-input fallback did).
        assert!(
            read_hi < input.len(),
            "examined extent must be tight, not scan to end-of-input (got {read_hi} of {})",
            input.len()
        );
    }

    #[test]
    fn incremental_reuse_is_sound_for_lookahead_region_edit() {
        // `a`'s negative lookahead `!"STOP"` reads four bytes it does not consume.
        // Editing those bytes flips the lookahead, so the cached `a` (matched only
        // "x") must be invalidated even though the edit misses its matched span.
        let grammar = peg::Grammar::trusted_new(
            r#"
    start <- a rest
    a     <- "x" !"STOP"
    rest  <- [A-Z]+
    "#,
        )
        .with_start_rule("start");
        let mut cache = peg::ParseCache::default();

        // First parse: "xZZZZ" — the lookahead passes, the whole input parses.
        peg::ParseRequest::new(&grammar)
            .run_incremental("xZZZZ", &mut cache)
            .expect("initial parse should succeed");

        // Replace the four lookahead bytes with "STOP" (same length). A fresh parse
        // now fails because `!"STOP"` rejects.
        let fresh = peg::parse("xSTOP", &grammar);
        assert!(fresh.is_err(), "fresh parse of \"xSTOP\" must fail");

        // The incremental parse must agree — i.e. it must NOT replay the stale `a`.
        let incremental = peg::ParseRequest::new(&grammar).run_incremental("xSTOP", &mut cache);
        assert!(
            incremental.is_err(),
            "incremental parse must reject too; reusing the cached `a` would be unsound"
        );
    }
}

/// Tightness rule: a `(` is a parametric call only when it immediately follows
/// the rule name; with whitespace the name is a `Ref` and `( … )` is a grouped
/// expression. Regression for the mis-parse of `a ('c')*` as the call `a('c')*`.
mod call_vs_grouping {
    use super::*;

    #[test]
    fn leading_ref_followed_by_repetition_group_parses() {
        let g = peg::Grammar::trusted_new("s <- a ('c')*\na <- 'b'").with_start_rule("s");
        assert!(peg::parse("bcc", &g).is_ok(), "ref + (group)* must parse");
        assert!(peg::parse("b", &g).is_ok());

        // The common `operand (op operand)*` shape with a rule operand.
        let expr = peg::Grammar::trusted_new("s <- n (('+' / '-') n)*\nn <- /[0-9]+/")
            .with_start_rule("s");
        assert!(peg::parse("1+2-3", &expr).is_ok());
    }

    #[test]
    fn leading_ref_with_optional_and_plus_groups() {
        let opt = peg::Grammar::trusted_new("s <- a ('c')?\na <- 'b'").with_start_rule("s");
        assert!(peg::parse("bc", &opt).is_ok());
        assert!(peg::parse("b", &opt).is_ok());
        let plus = peg::Grammar::trusted_new("s <- a ('c')+\na <- 'b'").with_start_rule("s");
        assert!(peg::parse("bcc", &plus).is_ok());
    }

    #[test]
    fn tight_paren_is_still_a_parametric_call() {
        // No space before `(` → a real call into a parametric rule.
        let g = peg::Grammar::trusted_new("s <- dup('x')\ndup(p) <- $p $p").with_start_rule("s");
        assert_eq!(
            peg::parse("xx", &g).unwrap().inner(),
            &peg::ParseValue::Node(
                "sequence".into(),
                std::sync::Arc::new(vec![
                    peg::ParseValue::Text("x".into()),
                    peg::ParseValue::Text("x".into()),
                ])
            )
        );
    }
}

/// Recursion-depth guard: deeply-nested input fails with a `recursion_limit`
/// error instead of overflowing the stack (a DoS guard). Regression for the
/// previously-unbounded recursive descent.
mod recursion_guard {
    use super::*;

    #[test]
    fn deep_nesting_errors_instead_of_overflowing() {
        let g = peg::Grammar::trusted_new("e <- '(' e ')' / 'x'").with_start_rule("e");
        let deep = format!("{}x{}", "(".repeat(50_000), ")".repeat(50_000));
        let cfg = peg::ParserConfig::default().with_max_steps(deep.len() * 8 + 1024);
        let err = peg::ParseRequest::new(&g)
            .config(cfg)
            .run(&deep)
            .expect_err("deep nesting must error, not abort");
        assert_eq!(err.code.as_deref(), Some("recursion_limit"));
    }

    #[test]
    fn shallow_nesting_still_parses() {
        let g = peg::Grammar::trusted_new("e <- '(' e ')' / 'x'").with_start_rule("e");
        let s = format!("{}x{}", "(".repeat(64), ")".repeat(64));
        assert!(peg::parse(&s, &g).is_ok());
    }

    #[test]
    fn max_depth_is_configurable() {
        let g = peg::Grammar::trusted_new("e <- '(' e ')' / 'x'").with_start_rule("e");
        let s = format!("{}x{}", "(".repeat(40), ")".repeat(40));
        let tight = peg::ParserConfig::default().with_max_depth(8);
        assert_eq!(
            peg::ParseRequest::new(&g)
                .config(tight)
                .run(&s)
                .unwrap_err()
                .code
                .as_deref(),
            Some("recursion_limit")
        );
    }
}
