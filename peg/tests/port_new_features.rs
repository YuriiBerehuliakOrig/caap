use caap_peg_port as peg;

// ── Capture ────────────────────────────────────────────────────────────────

#[test]
fn capture_wraps_result_in_spanned_value() {
    // Use /[a-z]+/ (full-word regex) so the inner value is a single Text node.
    let grammar = peg::Grammar::new("start <- capture(\"src\", /[a-z]+/)").with_start_rule("start");
    let value = peg::parse("hello", &grammar, None, false).expect("parse should succeed");
    match value {
        peg::ParseValue::SpannedValue { start, end, value } => {
            assert_eq!(start, 0);
            assert_eq!(end, 5);
            assert!(matches!(*value, peg::ParseValue::Text(_)));
        }
        other => panic!("expected SpannedValue, got {other:?}"),
    }
}

#[test]
fn capture_fails_when_inner_fails() {
    let grammar = peg::Grammar::new("start <- capture(\"src\", \"x\")").with_start_rule("start");
    assert!(peg::parse("y", &grammar, None, false).is_err());
}

// ── Island ────────────────────────────────────────────────────────────────

#[test]
fn island_matches_content_between_delimiters() {
    let grammar = peg::Grammar::new("start <- island(\"<\", \">\")").with_start_rule("start");
    let value = peg::parse("<hello>", &grammar, None, false).expect("parse should succeed");
    match value {
        peg::ParseValue::Text(t) => assert_eq!(t, "hello"),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn island_include_delims_returns_full_text() {
    let grammar = peg::Grammar::new("start <- island(\"<\", \">\", true)").with_start_rule("start");
    let value = peg::parse("<hello>", &grammar, None, false).expect("parse should succeed");
    match value {
        peg::ParseValue::Text(t) => assert_eq!(t, "<hello>"),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn island_fails_when_start_delimiter_absent() {
    let grammar = peg::Grammar::new("start <- island(\"<\", \">\")").with_start_rule("start");
    assert!(peg::parse("hello", &grammar, None, false).is_err());
}

#[test]
fn island_does_not_nest() {
    // With non-nested island, the first `>` closes the match.
    let grammar = peg::Grammar::new("start <- island(\"<\", \">\")").with_start_rule("start");
    // Input: "<a<b>" — island finds closing ">" after "a<b"
    let value = peg::parse("<a<b>", &grammar, None, false).expect("parse should succeed");
    match value {
        peg::ParseValue::Text(t) => assert_eq!(t, "a<b"),
        other => panic!("expected Text, got {other:?}"),
    }
}

// ── RawBlock ──────────────────────────────────────────────────────────────

#[test]
fn raw_block_matches_balanced_delimiters() {
    let grammar = peg::Grammar::new("start <- raw_block(\"(\", \")\")").with_start_rule("start");
    let value = peg::parse("(hello)", &grammar, None, false).expect("parse should succeed");
    match value {
        peg::ParseValue::Text(t) => assert_eq!(t, "hello"),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn raw_block_handles_nested_delimiters() {
    let grammar = peg::Grammar::new("start <- raw_block(\"(\", \")\")").with_start_rule("start");
    let value = peg::parse("(a(b)c)", &grammar, None, false).expect("parse should succeed");
    match value {
        peg::ParseValue::Text(t) => assert_eq!(t, "a(b)c"),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn raw_block_errors_on_unterminated() {
    let grammar = peg::Grammar::new("start <- raw_block(\"(\", \")\")").with_start_rule("start");
    assert!(peg::parse("(unterminated", &grammar, None, false).is_err());
}

// ── SemanticAction (runtime required, Python parity) ──────────────────────

#[test]
fn semantic_action_without_runtime_errors() {
    let grammar = peg::Grammar::new("start <- @upper(\"hello\")").with_start_rule("start");
    let err = peg::parse("hello", &grammar, None, false).expect_err("parse should fail");
    assert!(err.message.contains("semantic runtime"));
}

#[test]
fn semantic_action_with_null_runtime_passes_value_through() {
    let grammar = peg::Grammar::new("start <- @upper(\"hello\")").with_start_rule("start");
    let rt = peg::NullSemanticRuntime;
    let value = peg::parse_with_semantic("hello", &grammar, None, false, Some(&rt))
        .expect("parse should succeed");
    assert_eq!(value, peg::ParseValue::Text("hello".into()));
}

// ── SemanticAction (with ClosureSemanticRuntime) ──────────────────────────

#[test]
fn semantic_action_with_runtime_transforms_value() {
    // Use /[a-z]+/ so the action receives a single Text node.
    let grammar = peg::Grammar::new("start <- @upper(/[a-z]+/)").with_start_rule("start");
    let rt = peg::ClosureSemanticRuntime::new(
        |name, value, _span, _named| {
            if name == "upper" {
                match value {
                    peg::ParseValue::Text(s) => peg::ParseValue::Text(s.to_uppercase()),
                    other => other,
                }
            } else {
                value
            }
        },
        |_, _, _, _| true,
    );
    let value = peg::parse_with_semantic("hello", &grammar, None, false, Some(&rt))
        .expect("parse should succeed");
    assert_eq!(value, peg::ParseValue::Text("HELLO".into()));
}

#[test]
fn semantic_action_receives_rich_context() {
    let grammar = peg::Grammar::new("start <- @ctx(key:/[a-z]+/)").with_start_rule("start");
    let rt = peg::ContextualSemanticRuntime::new(
        |name, _value, context| {
            assert_eq!(name, "ctx");
            assert_eq!(context.matched_text, "abc");
            assert_eq!(context.span, Some((0, 3)));
            assert_eq!(context.pos, 0);
            assert_eq!(context.grammar_start, "start");
            assert_eq!(context.grammar.start_rule, "start");
            assert_eq!(context.grammar.rule_count, 1);
            assert!(context.config.memo);
            assert_eq!(context.config.output_mode, "value");
            assert_eq!(context.state.param_depth, 0);
            assert!(context.state.rule_stack.contains(&"start".to_string()));
            assert!(context.named.contains_key("key"));
            assert!(!context.items.is_empty());
            peg::ParseValue::Text(format!(
                "{}:{}",
                context.grammar_start, context.matched_text
            ))
        },
        |_, _, _| true,
    );
    let value = peg::parse_with_semantic("abc", &grammar, None, false, Some(&rt))
        .expect("parse should succeed");
    assert_eq!(value, peg::ParseValue::Text("start:abc".into()));
}

// ── SemanticPredicate (runtime required, Python parity) ───────────────────

#[test]
fn semantic_predicate_without_runtime_errors() {
    let grammar = peg::Grammar::new("start <- @?check \"x\"").with_start_rule("start");
    let err = peg::parse("x", &grammar, None, false).expect_err("parse should fail");
    assert!(err.message.contains("semantic runtime"));
}

#[test]
fn semantic_predicate_with_null_runtime_passes() {
    let grammar = peg::Grammar::new("start <- @?check \"x\"").with_start_rule("start");
    let rt = peg::NullSemanticRuntime;
    let value = peg::parse_with_semantic("x", &grammar, None, false, Some(&rt))
        .expect("parse should succeed");
    match value {
        peg::ParseValue::Node(name, items) => {
            assert_eq!(name, "sequence");
            assert_eq!(items.len(), 2);
            assert_eq!(items[0], peg::ParseValue::Nil);
            assert_eq!(items[1], peg::ParseValue::Text("x".into()));
        }
        other => panic!("expected sequence Node, got {other:?}"),
    }
}

#[test]
fn behavior_predicate_receives_args_from_spec_runtime() {
    use serde_json::json;
    let grammar = peg::SpecCompiler::new()
        .compile(&json!([
            "grammar",
            "g",
            "start",
            [[
                "rule",
                "start",
                [
                    "behavior",
                    [["predicate", "is_mode", "strict"]],
                    ["lit", "x"]
                ]
            ]]
        ]))
        .expect("compile should succeed");
    let rt = peg::ContextualSemanticRuntime::new(
        |_, value, _| value,
        |name, _value, context| {
            name == "is_mode"
                && context.args == vec![peg::GrammarScalar::Str("strict".into())]
                && context.matched_text == "x"
        },
    );
    let value = peg::parse_with_semantic("x", &grammar, None, false, Some(&rt))
        .expect("behavior predicate should accept");
    assert_eq!(value, peg::ParseValue::Text("x".into()));
}

// ── SemanticPredicate (with runtime that rejects) ─────────────────────────

#[test]
fn semantic_predicate_with_rejecting_runtime_fails_parse() {
    let grammar = peg::Grammar::new("start <- @?always_false \"x\"").with_start_rule("start");
    let rt = peg::ClosureSemanticRuntime::new(
        |_, v, _, _| v,
        |_name, _value, _span, _named| false, // always reject
    );
    assert!(peg::parse_with_semantic("x", &grammar, None, false, Some(&rt)).is_err());
}

// ── Combined: named binding + semantic action ─────────────────────────────

#[test]
fn semantic_action_receives_named_bindings() {
    // Grammar: `start <- @tag(key:/[a-z]+/)` — binds 'key' then calls @tag action.
    let grammar = peg::Grammar::new("start <- @tag(key:/[a-z]+/)").with_start_rule("start");
    let rt = peg::ClosureSemanticRuntime::new(
        |name, _value, _span, named| {
            if name == "tag" {
                named.get("key").cloned().unwrap_or(peg::ParseValue::Nil)
            } else {
                _value
            }
        },
        |_, _, _, _| true,
    );
    let value = peg::parse_with_semantic("abc", &grammar, None, false, Some(&rt))
        .expect("parse should succeed");
    assert_eq!(value, peg::ParseValue::Text("abc".into()));
}

// ── Eager ─────────────────────────────────────────────────────────────────

#[test]
fn eager_succeeds_on_match() {
    let grammar = peg::Grammar::new("start <- eager(\"x\")").with_start_rule("start");
    let value = peg::parse("x", &grammar, None, false).expect("eager match should succeed");
    assert_eq!(value, peg::ParseValue::Text("x".into()));
}

#[test]
fn eager_escalates_failure_to_error() {
    let grammar = peg::Grammar::new("start <- eager(\"x\")").with_start_rule("start");
    // On mismatch, Eager returns a ParseError (not a soft failure).
    assert!(peg::parse("y", &grammar, None, false).is_err());
}

#[test]
fn eager_double_bang_syntax() {
    // `!!expr` is shorthand for eager(expr).
    let grammar = peg::Grammar::new("start <- !!\"x\"").with_start_rule("start");
    assert!(peg::parse("y", &grammar, None, false).is_err());
    peg::parse("x", &grammar, None, false).expect("should succeed on match");
}

// ── HardKeyword ───────────────────────────────────────────────────────────

#[test]
fn hard_keyword_matches_standalone_word() {
    let grammar = peg::Grammar::new("start <- kw(\"if\")").with_start_rule("start");
    let value = peg::parse("if", &grammar, None, false).expect("kw match should succeed");
    assert_eq!(value, peg::ParseValue::Text("if".into()));
}

#[test]
fn hard_keyword_rejects_prefix_of_longer_identifier() {
    // "iffy" starts with "if" but is not the keyword.
    let grammar = peg::Grammar::new("start <- kw(\"if\")").with_start_rule("start");
    assert!(peg::parse("iffy", &grammar, None, false).is_err());
}

// ── ImportedRef ───────────────────────────────────────────────────────────

#[test]
fn imported_ref_without_registry_errors() {
    let grammar = peg::Grammar::new("start <- other::rule").with_start_rule("start");
    let err = peg::parse("x", &grammar, None, false).unwrap_err();
    assert!(
        err.message.contains("registry"),
        "error should mention registry: {}",
        err.message
    );
}

#[test]
fn imported_ref_resolves_from_registry() {
    let mut registry = peg::GrammarRegistry::new();
    registry
        .register(
            "other",
            peg::Grammar::new("rule <- \"x\"").with_start_rule("rule"),
        )
        .expect("registry registration should succeed");

    let parser = peg::PEGParser;
    let grammar = peg::Grammar::new("start <- other::rule").with_start_rule("start");
    let value = parser
        .parse_with_registry(&grammar, "x", &peg::ParserConfig::default(), &registry)
        .expect("registry import should parse");
    assert_eq!(value, peg::ParseValue::Text("x".into()));
}

#[test]
fn imported_ref_resolves_registry_target_from_metadata_alias() {
    use serde_json::json;
    let mut registry = peg::GrammarRegistry::new();
    registry
        .register(
            "math.core",
            peg::Grammar::new("rule <- \"x\"").with_start_rule("rule"),
        )
        .expect("registry registration should succeed");

    let grammar = peg::SpecCompiler::new()
        .compile(&json!([
            "grammar", "g", "start",
            [["rule", "start", ["imported_ref", "m", "rule"]]],
            ["imports", {"m": "math.core"}]
        ]))
        .expect("compile should succeed");

    let parser = peg::PEGParser;
    let value = parser
        .parse_with_registry(&grammar, "x", &peg::ParserConfig::default(), &registry)
        .expect("metadata import should resolve through registry");
    assert_eq!(value, peg::ParseValue::Text("x".into()));
}

#[test]
fn grammar_scope_resolves_from_registry() {
    let mut registry = peg::GrammarRegistry::new();
    registry
        .register(
            "other",
            peg::Grammar::new("rule <- \"x\"").with_start_rule("rule"),
        )
        .expect("registry registration should succeed");

    let parser = peg::PEGParser;
    let grammar = peg::Grammar::new("start <- scope(\"other\", rule)").with_start_rule("start");
    let value = parser
        .parse_with_registry(&grammar, "x", &peg::ParserConfig::default(), &registry)
        .expect("registry scope should parse");
    assert_eq!(value, peg::ParseValue::Text("x".into()));
}

// ── Parameter / Call ─────────────────────────────────────────────────────

#[test]
fn parametric_call_binds_argument() {
    // Grammar: wrap(x) <- "(" $x ")"
    //          start   <- wrap("hello")
    use peg::GrammarRule;
    let grammar = {
        let mut g = peg::Grammar::new("start <- wrap(\"hello\")").with_start_rule("start");
        g.rules.push(GrammarRule::from_source(
            "wrap",
            "\"(\" $x \")\"",
            vec!["x".to_string()],
        ));
        g
    };
    let value =
        peg::parse("(hello)", &grammar, None, false).expect("parametric call should succeed");
    match value {
        peg::ParseValue::Node(name, items) => {
            assert_eq!(name, "sequence");
            assert_eq!(items.len(), 3);
        }
        other => panic!("expected sequence Node, got {other:?}"),
    }
}

#[test]
fn parametric_call_fails_when_arg_mismatches() {
    use peg::GrammarRule;
    let grammar = {
        let mut g = peg::Grammar::new("start <- wrap(\"hello\")").with_start_rule("start");
        g.rules.push(GrammarRule::from_source(
            "wrap",
            "\"(\" $x \")\"",
            vec!["x".to_string()],
        ));
        g
    };
    assert!(peg::parse("(world)", &grammar, None, false).is_err());
}

// ── Behavior ─────────────────────────────────────────────────────────────

#[test]
fn behavior_transform_invoked_via_runtime() {
    use peg::BehaviorEntry;
    // Build a grammar where a behavior node wraps a literal — tested
    // by wiring it from code since there's no text-syntax for behavior() yet.
    // Instead, test via @action which is the SemanticAction equivalent.
    // (Full Behavior text syntax needs spec_compiler integration.)
    //
    // For now: verify that BehaviorEntry types are accessible and constructible.
    let _entry = BehaviorEntry::Transform(peg::TransformBehavior::new("upper"));
    let _pred = BehaviorEntry::Predicate(peg::PredicateBehavior::new("check"));
    let _diag = BehaviorEntry::Diagnostic(peg::DiagnosticBehavior::new("expected identifier"));
    let _trace = BehaviorEntry::Trace(peg::TraceBehavior::capture("my_capture"));
}

// ── LexToken / TokenRef ────────────────────────────────────────────────────

#[test]
fn lex_token_matches_by_kind() {
    let grammar = peg::Grammar::new("start <- tok(NAME)").with_start_rule("start");
    let tokens = vec![peg::LexToken::new("NAME", "hello", 0, 5)];
    let value =
        peg::parse_with_lex_tokens("hello", &grammar, tokens, None, false).expect("should parse");
    assert_eq!(value, peg::ParseValue::Text("hello".to_string()));
}

#[test]
fn lex_token_fails_on_wrong_kind() {
    let grammar = peg::Grammar::new("start <- tok(NUMBER)").with_start_rule("start");
    let tokens = vec![peg::LexToken::new("NAME", "hello", 0, 5)];
    let result = peg::parse_with_lex_tokens("hello", &grammar, tokens, None, false);
    assert!(result.is_err());
}

#[test]
fn lex_token_matches_by_kind_and_text() {
    let grammar = peg::Grammar::new("start <- tok(NAME,'hello')").with_start_rule("start");
    let tokens = vec![peg::LexToken::new("NAME", "hello", 0, 5)];
    let value =
        peg::parse_with_lex_tokens("hello", &grammar, tokens, None, false).expect("should parse");
    assert_eq!(value, peg::ParseValue::Text("hello".to_string()));
}

#[test]
fn lex_token_fails_on_wrong_text() {
    let grammar = peg::Grammar::new("start <- tok(NAME,'world')").with_start_rule("start");
    let tokens = vec![peg::LexToken::new("NAME", "hello", 0, 5)];
    let result = peg::parse_with_lex_tokens("hello", &grammar, tokens, None, false);
    assert!(result.is_err());
}

#[test]
fn lex_token_sequence_parses_multiple_tokens() {
    let grammar =
        peg::Grammar::new("start <- tok(NAME) tok(OP) tok(NUMBER)").with_start_rule("start");
    let tokens = vec![
        peg::LexToken::new("NAME", "x", 0, 1),
        peg::LexToken::new("OP", "+", 2, 3),
        peg::LexToken::new("NUMBER", "1", 4, 5),
    ];
    let value =
        peg::parse_with_lex_tokens("x + 1", &grammar, tokens, None, false).expect("should parse");
    assert!(matches!(value, peg::ParseValue::Node(ref n, _) if n == "sequence"));
}

#[test]
fn tok_without_lex_tokens_returns_error() {
    let grammar = peg::Grammar::new("start <- tok(NAME)").with_start_rule("start");
    let result = peg::parse("hello", &grammar, None, false);
    assert!(result.is_err());
    let msg = result.unwrap_err().message;
    assert!(
        msg.contains("tok()")
            || msg.contains("token list")
            || msg.contains("parse_with_lex_tokens"),
        "message: {msg}"
    );
}
