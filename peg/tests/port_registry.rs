use std::collections::HashMap;

use caap_peg_port as peg;

#[test]
fn registry_register_stores_sealed_snapshot_without_mutating_input() {
    let source = peg::Grammar::new("start <- \"a\"").with_start_rule("start");
    let original_sealed = source.is_sealed();
    let mut registry = peg::GrammarRegistry::new();

    registry
        .register("sample", source.clone())
        .expect("register should succeed");

    assert!(!original_sealed);
    assert!(!source.is_sealed());

    let stored = registry.get("sample", None).expect("stored grammar exists");
    assert!(stored.is_sealed());
}

#[test]
fn registry_supports_namespaced_identifiers_without_collision() {
    let mut registry = peg::GrammarRegistry::new();
    registry
        .register_with_options(
            "Expr",
            peg::Grammar::new("start <- \"a\"").with_start_rule("start"),
            false,
            Some("core"),
            &[],
            None,
        )
        .expect("register core");
    registry
        .register_with_options(
            "Expr",
            peg::Grammar::new("start <- \"b\"").with_start_rule("start"),
            false,
            Some("plugin"),
            &[],
            None,
        )
        .expect("register plugin");

    assert_eq!(registry.list(None), vec!["core.Expr", "plugin.Expr"]);
    assert_eq!(
        registry.get("core.Expr", None).unwrap().rules[0].source,
        "\"a\""
    );
    assert_eq!(
        registry.get("plugin.Expr", None).unwrap().rules[0].source,
        "\"b\""
    );
}

#[test]
fn registry_rejects_ambiguous_unqualified_lookup() {
    let mut registry = peg::GrammarRegistry::new();
    registry
        .register_with_options(
            "Expr",
            peg::Grammar::new("start <- \"a\"").with_start_rule("start"),
            false,
            Some("core"),
            &[],
            None,
        )
        .expect("register core");
    registry
        .register_with_options(
            "Expr",
            peg::Grammar::new("start <- \"b\"").with_start_rule("start"),
            false,
            Some("plugin"),
            &[],
            None,
        )
        .expect("register plugin");

    let err = registry
        .get("Expr", None)
        .expect_err("expected ambiguous lookup");
    assert!(matches!(err, peg::RegistryError::AmbiguousLookup { .. }));
}

#[test]
fn scoped_registry_replace_only_swaps_one_namespace() {
    let mut registry = peg::GrammarRegistry::new();
    registry
        .register_with_options(
            "Expr",
            peg::Grammar::new("start <- \"a\"").with_start_rule("start"),
            false,
            Some("core"),
            &[],
            None,
        )
        .expect("register core");
    registry
        .register_with_options(
            "Expr",
            peg::Grammar::new("start <- \"b\"").with_start_rule("start"),
            false,
            Some("plugin"),
            &[],
            None,
        )
        .expect("register plugin");

    let mut scoped = registry.scope("core").expect("scope core");
    scoped
        .replace(HashMap::from([(
            "Expr".to_string(),
            peg::Grammar::new("start <- \"c\"").with_start_rule("start"),
        )]))
        .expect("scoped replace");

    assert_eq!(
        registry.get("core.Expr", None).unwrap().rules[0].source,
        "\"c\""
    );
    assert_eq!(
        registry.get("plugin.Expr", None).unwrap().rules[0].source,
        "\"b\""
    );
}

#[test]
fn registry_entry_snapshot_preserves_metadata_and_aliases() {
    let mut registry = peg::GrammarRegistry::new();
    let entry = peg::RegistryEntry {
        identifier: peg::GrammarId::new(Some("core".to_string()), "Expr"),
        grammar: peg::Grammar::new("start <- \"x\"").with_start_rule("start"),
        aliases: vec![peg::GrammarId::new(Some("core".to_string()), "Alias")],
        origin: Some("unit-test".to_string()),
    };

    registry
        .register_entry(entry, false, None)
        .expect("register_entry should succeed");

    let snapshot = registry.snapshot_entries(None);
    let stored = snapshot
        .get(&peg::GrammarId::new(Some("core".to_string()), "Expr"))
        .expect("snapshot entry");

    assert_eq!(stored.origin.as_deref(), Some("unit-test"));
    assert_eq!(
        stored.aliases,
        vec![peg::GrammarId::new(Some("core".to_string()), "Alias")]
    );
}

#[test]
fn registry_replace_rejects_invalid_grammar_without_partial_swap() {
    let mut registry = peg::GrammarRegistry::new();
    registry
        .register(
            "left",
            peg::Grammar::new("start <- \"a\"").with_start_rule("start"),
        )
        .expect("seed registry");

    let mut broken = peg::Grammar::new("start <- missing");
    broken = broken.with_start_rule("start");

    let replacement = HashMap::from([("broken".to_string(), broken)]);
    assert!(matches!(
        registry.replace(replacement, None),
        Err(peg::RegistryError::InvalidGrammar(_))
    ));

    let names = registry.list(None);
    assert_eq!(names, vec!["left"]);
}

#[test]
fn sexpr_grammar_roundtrips_through_parser() {
    let sexpr = r#"
    (grammar calculator
      (start expr)
      (rule expr (choice (ref number) (ref paren)))
      (rule paren (seq (lit "(") (ref expr) (lit ")")))
      (rule number (regex "[0-9]+"))
      (trivia "whitespace")
    )
    "#;

    let grammar = peg::load_grammar_from_sexpr(sexpr).expect("sexpr grammar loads");
    assert_eq!(grammar.start_rule, "expr");
    assert_eq!(grammar.rules.len(), 3);

    let value = peg::parse("42", &grammar, None, false).expect("number parses");
    assert_eq!(value, peg::ParseValue::Text("42".to_string()));

    let value2 = peg::parse("(42)", &grammar, None, false).expect("paren parses");
    assert!(matches!(value2, peg::ParseValue::Node(_, _)));
}

#[test]
fn sexpr_grammar_and_predicates() {
    let sexpr = r#"
    (grammar pred-test
      (start root)
      (rule root (seq (and (lit "a")) (lit "a")))
    )
    "#;
    let grammar = peg::load_grammar_from_sexpr(sexpr).expect("sexpr grammar loads");
    let value = peg::parse("a", &grammar, None, false).expect("parses");
    assert!(matches!(value, peg::ParseValue::Node(_, _)));
}
