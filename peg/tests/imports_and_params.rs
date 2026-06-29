//! Scenario: **cross-grammar references and parametric rules** — `ImportedRef`,
//! `scope(...)`, registry resolution, and parametric `rule(arg)` calls.

use caap_peg as peg;

// ── ImportedRef ───────────────────────────────────────────────────────────

#[test]
fn imported_ref_without_registry_errors() {
    let grammar = peg::Grammar::trusted_new("start <- other::rule").with_start_rule("start");
    let err = peg::parse("x", &grammar).unwrap_err();
    assert!(
        err.message.contains("registry"),
        "error should mention registry: {}",
        err.message
    );
}

#[test]
fn inline_import_rejects_empty_alias() {
    let import = peg::Grammar::trusted_new("rule <- \"x\"").with_start_rule("rule");
    let err = peg::Grammar::trusted_new("start <- rule")
        .with_start_rule("start")
        .try_with_import("", import)
        .unwrap_err();
    assert!(
        err.message.contains("import alias must be non-empty"),
        "error should mention empty import alias: {}",
        err.message
    );
}

#[test]
fn imported_ref_resolves_from_registry() {
    let mut registry = peg::GrammarRegistry::new();
    registry
        .register(
            "other",
            peg::Grammar::trusted_new("rule <- \"x\"").with_start_rule("rule"),
        )
        .expect("registry registration should succeed");

    let grammar = peg::Grammar::trusted_new("start <- other::rule").with_start_rule("start");
    let value = peg::ParseRequest::new(&grammar)
        .registry(&registry)
        .run("x")
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
            peg::Grammar::trusted_new("rule <- \"x\"").with_start_rule("rule"),
        )
        .expect("registry registration should succeed");

    let grammar = peg::SpecCompiler::new()
        .compile(&json!([
            "grammar", "g", "start",
            [["rule", "start", ["imported_ref", "m", "rule"]]],
            ["imports", {"m": "math.core"}]
        ]))
        .expect("compile should succeed");

    let value = peg::ParseRequest::new(&grammar)
        .registry(&registry)
        .run("x")
        .expect("metadata import should resolve through registry");
    assert_eq!(value, peg::ParseValue::Text("x".into()));
}

#[test]
fn imported_ref_rejects_malformed_import_metadata() {
    use serde_json::json;
    let registry = peg::GrammarRegistry::new();
    let mut grammar = peg::SpecCompiler::new()
        .compile(&json!([
            "grammar",
            "g",
            "start",
            [["rule", "start", ["imported_ref", "m", "rule"]]]
        ]))
        .expect("compile should succeed");
    grammar.set_metadata_value("__grammar__", "imports", json!(["m"]));

    let err = peg::ParseRequest::new(&grammar)
        .registry(&registry)
        .run("x")
        .expect_err("malformed import metadata must fail");
    assert_eq!(err.code.as_deref(), Some("invalid_import_metadata"));
}

#[test]
fn imported_ref_rejects_registry_import_cycles() {
    use serde_json::json;
    let compiler = peg::SpecCompiler::new();
    let grammar_a = compiler
        .compile(&json!([
            "grammar", "a", "start",
            [["rule", "start", ["imported_ref", "b", "rule"]]],
            ["imports", {"b": "b"}]
        ]))
        .expect("grammar a should compile");
    let grammar_b = compiler
        .compile(&json!([
            "grammar", "b", "rule",
            [["rule", "rule", ["imported_ref", "a", "start"]]],
            ["imports", {"a": "a"}]
        ]))
        .expect("grammar b should compile");
    let mut registry = peg::GrammarRegistry::new();
    registry
        .register("a", grammar_a.clone())
        .expect("grammar a should register");
    registry
        .register("b", grammar_b)
        .expect("grammar b should register");

    let err = peg::ParseRequest::new(&grammar_a)
        .registry(&registry)
        .run("x")
        .expect_err("cyclic registry imports must fail");
    assert_eq!(err.code.as_deref(), Some("import_cycle"));
}

#[test]
fn grammar_scope_resolves_from_registry() {
    let mut registry = peg::GrammarRegistry::new();
    registry
        .register(
            "other",
            peg::Grammar::trusted_new("rule <- \"x\"").with_start_rule("rule"),
        )
        .expect("registry registration should succeed");

    let grammar =
        peg::Grammar::trusted_new("start <- scope(\"other\", rule)").with_start_rule("start");
    let value = peg::ParseRequest::new(&grammar)
        .registry(&registry)
        .run("x")
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
        let mut g = peg::Grammar::trusted_new("start <- wrap(\"hello\")").with_start_rule("start");
        g.rules.push(GrammarRule::trusted_from_source(
            "wrap",
            "\"(\" $x \")\"",
            vec!["x".to_string()],
        ));
        g
    };
    let value = peg::parse("(hello)", &grammar).expect("parametric call should succeed");
    match value {
        peg::ParseValue::Node(name, items) => {
            assert_eq!(&*name, "sequence");
            assert_eq!(items.len(), 3);
        }
        other => panic!("expected sequence Node, got {other:?}"),
    }
}

#[test]
fn parametric_call_fails_when_arg_mismatches() {
    use peg::GrammarRule;
    let grammar = {
        let mut g = peg::Grammar::trusted_new("start <- wrap(\"hello\")").with_start_rule("start");
        g.rules.push(GrammarRule::trusted_from_source(
            "wrap",
            "\"(\" $x \")\"",
            vec!["x".to_string()],
        ));
        g
    };
    assert!(peg::parse("(world)", &grammar).is_err());
}
