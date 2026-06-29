//! Scenario: the `GrammarRegistry` store — sealed-snapshot registration,
//! namespaced ids and scoped views, ambiguous-lookup rejection, atomic replace,
//! metadata/alias-preserving snapshots, and JSON grammar loading.

use std::collections::HashMap;

use caap_peg as peg;

#[test]
fn registry_register_stores_sealed_snapshot_without_mutating_input() {
    let source = peg::Grammar::trusted_new("start <- \"a\"").with_start_rule("start");
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
            peg::Grammar::trusted_new("start <- \"a\"").with_start_rule("start"),
            false,
            Some("core"),
            &[],
            None,
        )
        .expect("register core");
    registry
        .register_with_options(
            "Expr",
            peg::Grammar::trusted_new("start <- \"b\"").with_start_rule("start"),
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
            peg::Grammar::trusted_new("start <- \"a\"").with_start_rule("start"),
            false,
            Some("core"),
            &[],
            None,
        )
        .expect("register core");
    registry
        .register_with_options(
            "Expr",
            peg::Grammar::trusted_new("start <- \"b\"").with_start_rule("start"),
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
            peg::Grammar::trusted_new("start <- \"a\"").with_start_rule("start"),
            false,
            Some("core"),
            &[],
            None,
        )
        .expect("register core");
    registry
        .register_with_options(
            "Expr",
            peg::Grammar::trusted_new("start <- \"b\"").with_start_rule("start"),
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
            peg::Grammar::trusted_new("start <- \"c\"").with_start_rule("start"),
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
        identifier: peg::GrammarId::new(Some("core".to_string()), "Expr").unwrap(),
        grammar: peg::Grammar::trusted_new("start <- \"x\"").with_start_rule("start"),
        aliases: vec![peg::GrammarId::new(Some("core".to_string()), "Alias").unwrap()],
        origin: Some("unit-test".to_string()),
    };

    registry
        .register_entry(entry, false, None)
        .expect("register_entry should succeed");

    let snapshot = registry.snapshot_entries(None);
    let stored = snapshot
        .get(&peg::GrammarId::new(Some("core".to_string()), "Expr").unwrap())
        .expect("snapshot entry");

    assert_eq!(stored.origin.as_deref(), Some("unit-test"));
    assert_eq!(
        stored.aliases,
        vec![peg::GrammarId::new(Some("core".to_string()), "Alias").unwrap()]
    );
}

#[test]
fn registry_replace_rejects_invalid_grammar_without_partial_swap() {
    let mut registry = peg::GrammarRegistry::new();
    registry
        .register(
            "left",
            peg::Grammar::trusted_new("start <- \"a\"").with_start_rule("start"),
        )
        .expect("seed registry");

    let mut broken = peg::Grammar::trusted_new("start <- missing");
    broken = broken.with_start_rule("start");

    let replacement = HashMap::from([("broken".to_string(), broken)]);
    assert!(matches!(
        registry.replace(replacement, None),
        Err(peg::RegistryError::InvalidGrammar(_))
    ));

    let names = registry.list(None);
    assert_eq!(names, vec!["left"]);
}
