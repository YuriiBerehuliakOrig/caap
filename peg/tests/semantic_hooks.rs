//! Scenario: the **semantic-hook / driver protocol** — `@action`, `@?pred`,
//! guards, named-binding interplay, and context-dependent (auto-scope) parsing
//! behave correctly with and without an attached driver.

use caap_peg as peg;

// ── SemanticAction (runtime required, reference-behavior parity) ──────────────────────

#[test]
fn semantic_action_without_driver_errors() {
    let grammar = peg::Grammar::trusted_new("start <- @upper(\"hello\")").with_start_rule("start");
    let err = peg::parse("hello", &grammar).expect_err("parse should fail");
    assert!(err.message.contains("requires a driver"));
}

#[test]
fn semantic_action_with_no_handler_passes_value_through() {
    let grammar = peg::Grammar::trusted_new("start <- @upper(\"hello\")").with_start_rule("start");
    // No "upper" action registered → SemanticAction effect → Proceed → unchanged.
    let driver = peg::ParseDriverBuilder::new().build();
    let value = peg::ParseRequest::new(&grammar)
        .driver(&driver)
        .run("hello")
        .expect("parse should succeed");
    assert_eq!(value, peg::ParseValue::Text("hello".into()));
}

// ── @action via the driver protocol ───────────────────────────────────────

#[test]
fn semantic_action_transforms_value() {
    let grammar = peg::Grammar::trusted_new("start <- @upper(/[a-z]+/)").with_start_rule("start");
    let driver = peg::ParseDriverBuilder::new()
        .action("upper", |value, _view| match value {
            peg::ParseValue::Text(s) => peg::ParseValue::Text(s.to_uppercase().into()),
            other => other,
        })
        .build();
    let value = peg::ParseRequest::new(&grammar)
        .driver(&driver)
        .run("hello")
        .expect("parse should succeed");
    assert_eq!(value, peg::ParseValue::Text("HELLO".into()));
}

#[test]
fn semantic_action_panic_is_reported_as_parse_error() {
    let grammar = peg::Grammar::trusted_new("start <- @boom(/[a-z]+/)").with_start_rule("start");
    let driver = peg::ParseDriverBuilder::new()
        .action("boom", |_value, _view| panic!("action failed"))
        .build();

    let error = peg::ParseRequest::new(&grammar)
        .driver(&driver)
        .run("hello")
        .unwrap_err();

    assert_eq!(error.code.as_deref(), Some("driver_fail"));
    assert!(error.message.contains("driver hook panicked"));
    assert!(error.message.contains("action failed"));
}

#[test]
fn semantic_predicate_panic_is_reported_as_parse_error() {
    let grammar = peg::Grammar::trusted_new("start <- @?boom \"a\"").with_start_rule("start");
    let driver = peg::ParseDriverBuilder::new()
        .predicate("boom", |_view| panic!("predicate failed"))
        .build();

    let error = peg::ParseRequest::new(&grammar)
        .driver(&driver)
        .run("a")
        .unwrap_err();

    assert_eq!(error.code.as_deref(), Some("driver_fail"));
    assert!(error.message.contains("driver hook panicked"));
    assert!(error.message.contains("predicate failed"));
}

#[test]
fn semantic_action_receives_rich_view() {
    let grammar = peg::Grammar::trusted_new("start <- @ctx(key:/[a-z]+/)").with_start_rule("start");
    let driver = peg::ParseDriverBuilder::new()
        .action("ctx", |_value, view| {
            assert_eq!(view.matched_text, "abc");
            assert_eq!(view.span, Some((0, 3)));
            assert_eq!(view.pos, 0);
            assert_eq!(view.start_rule, "start");
            assert_eq!(view.grammar().start_rule, "start");
            assert_eq!(view.grammar().rule_count, 1);
            assert!(view.config().memo);
            assert_eq!(view.config().output_mode, "value");
            assert_eq!(view.state().param_depth, 0);
            assert!(view.rule_stack.contains(&"start"));
            assert!(view.named().contains_key("key"));
            assert!(!view.items().is_empty());
            peg::ParseValue::Text(format!("{}:{}", view.start_rule, view.matched_text).into())
        })
        .build();
    let value = peg::ParseRequest::new(&grammar)
        .driver(&driver)
        .run("abc")
        .expect("parse should succeed");
    assert_eq!(value, peg::ParseValue::Text("start:abc".into()));
}

// ── @?pred requires a driver ──────────────────────────────────────────────

#[test]
fn semantic_predicate_without_driver_errors() {
    let grammar = peg::Grammar::trusted_new("start <- @?check \"x\"").with_start_rule("start");
    let err = peg::parse("x", &grammar).expect_err("parse should fail");
    assert!(err.message.contains("requires a driver"));
}

#[test]
fn semantic_predicate_with_no_handler_passes() {
    let grammar = peg::Grammar::trusted_new("start <- @?check \"x\"").with_start_rule("start");
    // No "check" predicate registered → SemanticPredicate effect → Proceed.
    let driver = peg::ParseDriverBuilder::new().build();
    let value = peg::ParseRequest::new(&grammar)
        .driver(&driver)
        .run("x")
        .expect("parse should succeed");
    match value {
        peg::ParseValue::Node(name, items) => {
            assert_eq!(&*name, "sequence");
            assert_eq!(items.len(), 2);
            assert_eq!(items[0], peg::ParseValue::Nil);
            assert_eq!(items[1], peg::ParseValue::Text("x".into()));
        }
        other => panic!("expected sequence Node, got {other:?}"),
    }
}

// ── @?pred that rejects ───────────────────────────────────────────────────

#[test]
fn semantic_predicate_that_rejects_fails_parse() {
    let grammar =
        peg::Grammar::trusted_new("start <- @?always_false \"x\"").with_start_rule("start");
    let driver = peg::ParseDriverBuilder::new()
        .predicate("always_false", |_view| false)
        .build();
    assert!(peg::ParseRequest::new(&grammar)
        .driver(&driver)
        .run("x")
        .is_err());
}

// ── Combined: named binding + @action ─────────────────────────────────────

#[test]
fn semantic_action_receives_named_bindings() {
    let grammar = peg::Grammar::trusted_new("start <- @tag(key:/[a-z]+/)").with_start_rule("start");
    let driver = peg::ParseDriverBuilder::new()
        .action("tag", |_value, view| {
            view.named()
                .get("key")
                .cloned()
                .unwrap_or(peg::ParseValue::Nil)
        })
        .build();
    let value = peg::ParseRequest::new(&grammar)
        .driver(&driver)
        .run("abc")
        .expect("parse should succeed");
    assert_eq!(value, peg::ParseValue::Text("abc".into()));
}

// ── Context-Dependent Parsing ──────────────────────────────────────────────

#[test]
fn context_dependent_parsing_with_builder() {
    use std::sync::{Arc, Mutex};

    // Grammar uses @?in_lambda_body / @?not_in_lambda_body — resolved automatically
    // by with_auto_scope(), no manual predicate registration required.
    let grammar = peg::Grammar::trusted_new(
        "program     <- stmt+\n\
         stmt        <- lambda_stmt / return_stmt / module_stmt / name_stmt\n\
         lambda_stmt <- \"fn\" name \"{\" lambda_body \"}\"\n\
         lambda_body <- stmt*\n\
         return_stmt <- @?in_lambda_body @return_tag(\"return\" name)\n\
         module_stmt <- @?not_in_lambda_body @module_tag(\"module\" name)\n\
         name_stmt   <- name\n\
         name        <- /[a-z_][a-z0-9_]*/",
    )
    .with_start_rule("program");

    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let log_return = Arc::clone(&log);
    let log_module = Arc::clone(&log);

    let built = peg::ParseDriverBuilder::new()
        .with_auto_scope()
        .action("return_tag", move |v, _view| {
            log_return.lock().unwrap().push("return_tag".to_string());
            v
        })
        .action("module_tag", move |v, _view| {
            log_module.lock().unwrap().push("module_tag".to_string());
            v
        })
        .build();
    let driver: &dyn peg::ParseDriver = &built;

    // Case 1: "return x" outside lambda — auto_scope: in_lambda_body fails → name_stmt
    log.lock().unwrap().clear();
    peg::ParseRequest::new(&grammar)
        .driver(driver)
        .run("return x")
        .expect("name_stmt fallback");
    assert!(!log.lock().unwrap().contains(&"return_tag".to_string()));

    // Case 2: "return x" inside lambda — auto_scope: in_lambda_body passes → return_stmt
    log.lock().unwrap().clear();
    peg::ParseRequest::new(&grammar)
        .driver(driver)
        .run("fn foo { return x }")
        .expect("return valid inside lambda");
    assert!(log.lock().unwrap().contains(&"return_tag".to_string()));

    // Case 3: "module main" outside lambda — auto_scope: not_in_lambda_body passes → module_stmt
    log.lock().unwrap().clear();
    peg::ParseRequest::new(&grammar)
        .driver(driver)
        .run("module main")
        .expect("module valid at top level");
    assert!(log.lock().unwrap().contains(&"module_tag".to_string()));

    // Case 4: "fn foo { module m }" inside lambda — auto_scope: not_in_lambda_body fails → name_stmt
    log.lock().unwrap().clear();
    peg::ParseRequest::new(&grammar)
        .driver(driver)
        .run("fn foo { module m }")
        .expect("name_stmt fallback inside lambda");
    assert!(!log.lock().unwrap().contains(&"module_tag".to_string()));
}

#[test]
fn context_dependent_parsing_lambda_vs_toplevel() {
    use std::sync::{Arc, Mutex};

    // Mini language: `return` only valid inside lambda body; `module` only valid outside.
    // Both predicates are zero-width — on failure the Choice backtracks to name_stmt.
    let grammar = peg::Grammar::trusted_new(
        "program     <- stmt+\n\
         stmt        <- lambda_stmt / return_stmt / module_stmt / name_stmt\n\
         lambda_stmt <- \"fn\" name \"{\" lambda_body \"}\"\n\
         lambda_body <- stmt*\n\
         return_stmt <- @?in_lambda @return_tag(\"return\" name)\n\
         module_stmt <- @?not_in_lambda @module_tag(\"module\" name)\n\
         name_stmt   <- name\n\
         name        <- /[a-z_][a-z0-9_]*/",
    )
    .with_start_rule("program");

    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let log_for_action = Arc::clone(&log);

    let log_module = std::sync::Arc::clone(&log);
    let built = peg::ParseDriverBuilder::new()
        .action("return_tag", move |v, _view| {
            log_for_action
                .lock()
                .unwrap()
                .push("return_tag".to_string());
            v
        })
        .action("module_tag", move |v, _view| {
            log_module.lock().unwrap().push("module_tag".to_string());
            v
        })
        .predicate("in_lambda", |view| view.rule_stack.contains(&"lambda_body"))
        .predicate("not_in_lambda", |view| {
            !view.rule_stack.contains(&"lambda_body")
        })
        .build();
    let driver: &dyn peg::ParseDriver = &built;

    // Case 1: "return x" outside lambda — @?in_lambda fails → name_stmt fallback
    log.lock().unwrap().clear();
    peg::ParseRequest::new(&grammar)
        .driver(driver)
        .run("return x")
        .expect("should parse via name_stmt fallback");
    assert!(
        !log.lock().unwrap().contains(&"return_tag".to_string()),
        "return_tag must NOT fire outside lambda"
    );

    // Case 2: "return x" inside lambda — @?in_lambda succeeds → return_stmt matches
    log.lock().unwrap().clear();
    peg::ParseRequest::new(&grammar)
        .driver(driver)
        .run("fn foo { return x }")
        .expect("return is valid inside lambda");
    assert!(
        log.lock().unwrap().contains(&"return_tag".to_string()),
        "return_tag must fire inside lambda"
    );

    // Case 3: "module main" outside lambda — @?not_in_lambda succeeds → module_stmt matches
    log.lock().unwrap().clear();
    peg::ParseRequest::new(&grammar)
        .driver(driver)
        .run("module main")
        .expect("module is valid at top level");
    assert!(
        log.lock().unwrap().contains(&"module_tag".to_string()),
        "module_tag must fire outside lambda"
    );

    // Case 4: "fn foo { module m }" inside lambda — @?not_in_lambda fails → name_stmt fallback
    log.lock().unwrap().clear();
    peg::ParseRequest::new(&grammar)
        .driver(driver)
        .run("fn foo { module m }")
        .expect("should parse via name_stmt fallback inside lambda");
    assert!(
        !log.lock().unwrap().contains(&"module_tag".to_string()),
        "module_tag must NOT fire inside lambda"
    );
}

#[test]
fn contextual_rules_added_to_base_grammar() {
    use peg::builder::{self, GrammarBuilder};
    use std::sync::{Arc, Mutex};

    // Base grammar — built programmatically; no string parsing, no escaping.
    let base = GrammarBuilder::new()
        .start("program")
        .rule("program", builder::plus(builder::rule_ref("stmt")))
        .rule(
            "stmt",
            builder::choice(vec![
                builder::rule_ref("lambda_stmt"),
                builder::rule_ref("name_stmt"),
            ]),
        )
        .rule(
            "lambda_stmt",
            builder::seq(vec![
                builder::lit("fn"),
                builder::rule_ref("name"),
                builder::lit("{"),
                builder::rule_ref("lambda_body"),
                builder::lit("}"),
            ]),
        )
        .rule("lambda_body", builder::star(builder::rule_ref("stmt")))
        .rule("name_stmt", builder::rule_ref("name"))
        .rule("name", builder::regex("[a-z_][a-z0-9_]*").unwrap())
        .build();

    // Context extension — defined separately, applied on top of the base.
    // Adds `return` (lambda-only) and `module` (top-level-only), then patches
    // the `stmt` dispatch rule to include the new context-sensitive alternatives.
    let grammar = base.extend(&[
        (
            "return_stmt",
            "@?in_lambda_body @return_tag(\"return\" name)",
        ),
        (
            "module_stmt",
            "@?not_in_lambda_body @module_tag(\"module\" name)",
        ),
        (
            "stmt",
            "lambda_stmt / return_stmt / module_stmt / name_stmt",
        ),
    ]);

    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let log_return = Arc::clone(&log);
    let log_module = Arc::clone(&log);

    let built = peg::ParseDriverBuilder::new()
        .with_auto_scope()
        .action("return_tag", move |v, _view| {
            log_return.lock().unwrap().push("return_tag".to_string());
            v
        })
        .action("module_tag", move |v, _view| {
            log_module.lock().unwrap().push("module_tag".to_string());
            v
        })
        .build();
    let driver: &dyn peg::ParseDriver = &built;

    log.lock().unwrap().clear();
    peg::ParseRequest::new(&grammar)
        .driver(driver)
        .run("return x")
        .expect("name_stmt fallback");
    assert!(!log.lock().unwrap().contains(&"return_tag".to_string()));

    log.lock().unwrap().clear();
    peg::ParseRequest::new(&grammar)
        .driver(driver)
        .run("fn foo { return x }")
        .expect("return valid inside lambda");
    assert!(log.lock().unwrap().contains(&"return_tag".to_string()));

    log.lock().unwrap().clear();
    peg::ParseRequest::new(&grammar)
        .driver(driver)
        .run("module main")
        .expect("module valid at top level");
    assert!(log.lock().unwrap().contains(&"module_tag".to_string()));

    log.lock().unwrap().clear();
    peg::ParseRequest::new(&grammar)
        .driver(driver)
        .run("fn foo { module m }")
        .expect("name_stmt fallback inside lambda");
    assert!(!log.lock().unwrap().contains(&"module_tag".to_string()));
}
