/// Integration tests for CTFE PEG builder and grammar runtime builtins.
use caap_core::{frontend::parse, Evaluator, MapKey, PhasePolicy, RuntimeValue};
use std::rc::Rc;

mod common;

// ── ctfe-peg-* builtins ───────────────────────────────────────────────────────

#[test]
fn test_ctfe_peg_lit_creates_expr() {
    let graph = parse(r#"(ctfe_peg_lit "hello")"#).unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    let result = ev.run().unwrap();
    assert!(
        matches!(result, RuntimeValue::HostObject(_)),
        "expected peg-expr host object, got {result:?}"
    );
}

#[test]
fn test_ctfe_peg_builder_build_then_parse() {
    // Build grammar: word <- /[a-z]+/  then parse "hello"
    let graph = parse(
        r#"(ctfe_grammar_parse
             "hello"
             (ctfe_peg_builder_build
               (ctfe_peg_builder_rule
                 (ctfe_peg_builder "word")
                 "word"
                 (ctfe_peg_regex "[a-z]+"))))"#,
    )
    .unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    let result = ev.run().unwrap();
    let RuntimeValue::Map(m) = result else {
        panic!("expected map, got {result:?}");
    };
    let ok = m
        .borrow()
        .get(&MapKey::Str(Rc::from("ok")))
        .cloned()
        .unwrap();
    assert_eq!(ok, RuntimeValue::Bool(true));
}

#[test]
fn test_ctfe_peg_builder_with_choice_and_seq() {
    // Grammar:
    //   root <- item+
    //   item <- "(" root ")" / /[a-z]+/
    let graph = parse(
        r#"(ctfe_grammar_parse
             "(hello)"
             (ctfe_peg_builder_build
               (ctfe_peg_builder_rule
               (ctfe_peg_builder_rule
                 (ctfe_peg_builder "root")
                 "root" (ctfe_peg_plus (ctfe_peg_ref "item")))
                 "item" (ctfe_peg_choice (list_of
                          (ctfe_peg_seq (list_of
                            (ctfe_peg_lit "(")
                            (ctfe_peg_ref "root")
                            (ctfe_peg_lit ")")))
                          (ctfe_peg_regex "[a-z]+"))))))"#,
    )
    .unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    let result = ev.run().unwrap();
    let RuntimeValue::Map(m) = result else {
        panic!("expected map, got {result:?}");
    };
    let ok = m
        .borrow()
        .get(&MapKey::Str(Rc::from("ok")))
        .cloned()
        .unwrap();
    assert_eq!(ok, RuntimeValue::Bool(true));
}

#[test]
fn test_ctfe_peg_regex_invalid_pattern_errors() {
    let graph = parse(r#"(ctfe_peg_regex "[")"#).unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    let result = ev.run();
    assert!(
        result.is_err(),
        "invalid regex pattern should produce an eval error"
    );
}

#[test]
fn test_ctfe_peg_builder_exposes_separator_keyword_and_named_constructors() {
    let result = run_compile_time(
        r#"(ctfe_grammar_parse
             "let alpha,beta"
             (ctfe_peg_builder_build
               (ctfe_peg_builder_rule
                 (ctfe_peg_builder "root")
                 "root"
                 (ctfe_peg_seq
                   (list_of
                     (ctfe_peg_keyword "let")
                     (ctfe_peg_named
                       "names"
                       (ctfe_peg_sep_plus
                         (ctfe_peg_plus (ctfe_peg_char_class "a-z"))
                         (ctfe_peg_lit ","))))))))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(true)));
}

#[test]
fn test_ctfe_peg_builder_exposes_parametric_and_imported_refs() {
    let result = run_compile_time(
        r#"(bind ident_grammar
             (ctfe_peg_builder_build
               (ctfe_peg_builder_rule
                 (ctfe_peg_builder "ident")
                 "ident"
                 (ctfe_peg_plus (ctfe_peg_char_class "a-z"))))
             (bind main_builder
               (ctfe_peg_builder_import
                 (ctfe_peg_builder "root")
                 "id"
                 ident_grammar)
               (bind grammar
                 (ctfe_peg_builder_build
                   (ctfe_peg_builder_rule
                     (ctfe_peg_builder_parametric_rule
                       main_builder
                       "wrapped"
                       (list_of "inner")
                       (ctfe_peg_seq
                         (list_of
                           (ctfe_peg_lit "(")
                           (ctfe_peg_param "inner")
                           (ctfe_peg_lit ")"))))
                     "root"
                     (ctfe_peg_seq
                       (list_of
                         (ctfe_peg_soft_keyword "name")
                         (ctfe_peg_call
                           "wrapped"
                           (list_of (ctfe_peg_imported_ref "id" "ident")))))))
                 (ctfe_grammar_parse "name (alpha)" grammar))))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(true)));
}

#[test]
fn test_ctfe_peg_builder_import_rejects_empty_alias() {
    let graph = parse(
        r#"(bind ident_grammar
             (ctfe_peg_builder_build
               (ctfe_peg_builder_rule
                 (ctfe_peg_builder "ident")
                 "ident"
                 (ctfe_peg_plus (ctfe_peg_char_class "a-z"))))
             (ctfe_peg_builder_import
               (ctfe_peg_builder "root")
               ""
               ident_grammar))"#,
    )
    .unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    let error = ev.run().unwrap_err().to_string();
    assert!(
        error.contains("alias must be non-empty"),
        "expected empty alias diagnostic, got {error}"
    );
}

#[test]
fn test_ctfe_peg_builder_action_node_fires_driver_transform() {
    // A `@mark(...)` semantic action built programmatically via `ctfe_peg_action`
    // is dispatched to the CAAP `actions` driver hook at parse time. (The old
    // behavior-table builtins were removed with the Parse Effects Protocol
    // migration; named actions are now the only transform mechanism.)
    let result = run_compile_time(
        r#"(ctfe_grammar_parse
             "x"
             (ctfe_peg_builder_build
               (ctfe_peg_builder_rule
                 (ctfe_peg_builder "root")
                 "root"
                 (ctfe_peg_action "mark" (ctfe_peg_lit "x"))))
             (map_of
               "actions"
               (map_of
                 "mark"
                 (lambda (ctx)
                   (map_of
                     "kind" "marked"
                     "children" (list_of (get ctx "value" null)))))))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(true)));
    let value = map_field(&result, "value").expect("parse result should carry value");
    assert_eq!(
        map_field(&value, "kind"),
        Some(RuntimeValue::Str("marked".into()))
    );
}

#[test]
fn test_ctfe_peg_builder_exposes_remaining_advanced_expr_objects() {
    let result = run_compile_time(
        r#"(list_of
             (ctfe_peg_dot)
             (ctfe_peg_cut)
             (ctfe_peg_newline)
             (ctfe_peg_indent)
             (ctfe_peg_dedent)
             (ctfe_peg_eager (ctfe_peg_lit "x"))
             (ctfe_peg_no_trivia (ctfe_peg_lit "x"))
             (ctfe_peg_capture "span" (ctfe_peg_lit "x"))
             (ctfe_peg_expected "expected x" (ctfe_peg_lit "x"))
             (ctfe_peg_grammar_scope "demo" (ctfe_peg_lit "x"))
             (ctfe_peg_interspersed (ctfe_peg_lit "x") (ctfe_peg_lit ","))
             (ctfe_peg_island "{" "}" false)
             (ctfe_peg_raw_block "{{" "}}" "brace")
             (ctfe_peg_token_ref "ident" null))"#,
    );
    let RuntimeValue::List(items) = result else {
        panic!("expected list result, got {result:?}");
    };
    assert_eq!(items.borrow().len(), 14);
}

// ── ctfe-grammar-* builtins ───────────────────────────────────────────────────

#[test]
fn test_ctfe_grammar_new_returns_host_object() {
    let graph = parse(r#"(ctfe_grammar_new "start <- /[a-z]+/")"#).unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    let result = ev.run().unwrap();
    assert!(
        matches!(result, RuntimeValue::HostObject(_)),
        "expected grammar host object, got {result:?}"
    );
}

#[test]
fn test_ctfe_grammar_builtins_are_compile_time_only() {
    let graph = parse(r#"(ctfe_grammar_new "start <- /[a-z]+/")"#).unwrap();
    let mut ev = Evaluator::new(graph);
    let error = ev
        .run()
        .expect_err("ctfe grammar construction must not run in runtime phase");
    assert!(format!("{error}").contains("not available in phase runtime"));
}

#[test]
fn test_ctfe_grammar_parse_success() {
    let graph = parse(
        r#"(ctfe_grammar_parse
             "hello"
             (ctfe_grammar_set_start
               (ctfe_grammar_new "start <- /[a-z]+/")
               "start"))"#,
    )
    .unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    let result = ev.run().unwrap();
    let RuntimeValue::Map(m) = result else {
        panic!("expected map, got {result:?}");
    };
    let ok = m
        .borrow()
        .get(&MapKey::Str(Rc::from("ok")))
        .cloned()
        .unwrap();
    assert_eq!(ok, RuntimeValue::Bool(true));
}

#[test]
fn test_ctfe_grammar_parse_failure() {
    let graph = parse(
        r#"(ctfe_grammar_parse
             "123"
             (ctfe_grammar_set_start
               (ctfe_grammar_new "start <- /[a-z]+/")
               "start"))"#,
    )
    .unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    let result = ev.run().unwrap();
    let RuntimeValue::Map(m) = result else {
        panic!("expected map, got {result:?}");
    };
    let ok = m
        .borrow()
        .get(&MapKey::Str(Rc::from("ok")))
        .cloned()
        .unwrap();
    assert_eq!(ok, RuntimeValue::Bool(false));
    assert!(m.borrow().contains_key(&MapKey::Str(Rc::from("error"))));
}

#[test]
fn test_ctfe_grammar_rule_get_projects_one_rule() {
    let result = run_compile_time(
        r#"(bind grammar
             (ctfe_grammar_set_start
               (ctfe_grammar_new
                 "root <- item+\nitem <- /[a-z]+/")
               "root")
             (bind item (ctfe_grammar_rule_get grammar "item")
               (bind missing (ctfe_grammar_rule_get grammar "missing" "fallback")
                 (list_of
                   (get item "name")
                   (get item "source")
                   (get item "index")
                   (get (get item "params") 0 "none")
                   missing))))"#,
    );
    let RuntimeValue::List(items) = result else {
        panic!("expected list, got {result:?}");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("item".into()));
    assert_eq!(items[1], RuntimeValue::Str("/[a-z]+/".into()));
    assert_eq!(items[2], RuntimeValue::Int(1));
    assert_eq!(items[3], RuntimeValue::Str("none".into()));
    assert_eq!(items[4], RuntimeValue::Str("fallback".into()));
}

#[test]
fn test_ctfe_grammar_parse_options_expose_packrat_policy_and_step_budget() {
    let long_text = "a".repeat(5000);
    let source = format!(
        r#"(bind grammar
             (ctfe_grammar_set_start
               (ctfe_grammar_new "root <- /a+/")
               "root")
             (bind large
               (ctfe_grammar_parse
                 {:?}
                 grammar
                 (map_of "max_steps" 8192
                         "memo" true
                         "memo_policy" (map_of "global_budget" 128)))
               (bind no_memo_left_recursion
                 (ctfe_grammar_parse
                   "a"
                   (ctfe_grammar_set_start
                     (ctfe_grammar_new "root <- root \"a\" / \"a\"")
                     "root")
                   (map_of "memo" false
                           "max_steps" 64))
                 (list_of
                   (get large "ok")
                   (get no_memo_left_recursion "ok")
                   (string_contains
                     (get no_memo_left_recursion "error")
                     "memoization cannot be disabled")))))"#,
        long_text
    );
    let result = run_compile_time(&source);
    let RuntimeValue::List(items) = result else {
        panic!("expected list, got {result:?}");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Bool(false));
    assert_eq!(items[2], RuntimeValue::Bool(true));
}

#[test]
fn test_ctfe_grammar_extend_adds_rules() {
    // Base grammar only knows `name_stmt`.  After extend it also handles `fn`.
    let graph = parse(
        r#"(ctfe_grammar_parse
             "fn foo { return x }"
             (ctfe_grammar_extend
               (ctfe_grammar_set_start
                 (ctfe_grammar_new
                   "program     <- stmt+
                    stmt        <- lambda_stmt / name_stmt
                    lambda_stmt <- \"fn\" name \"{\" lambda_body \"}\"
                    lambda_body <- stmt*
                    name_stmt   <- name
                    name        <- /[a-z_][a-z0-9_]*/")
                 "program")
               (list_of
                 (list_of "return_stmt" "\"return\" name")
                 (list_of "stmt"
                   "lambda_stmt / return_stmt / name_stmt"))))"#,
    )
    .unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    let result = ev.run().unwrap();
    let RuntimeValue::Map(m) = result else {
        panic!("expected map, got {result:?}");
    };
    let ok = m
        .borrow()
        .get(&MapKey::Str(Rc::from("ok")))
        .cloned()
        .unwrap();
    assert_eq!(
        ok,
        RuntimeValue::Bool(true),
        "extended grammar should parse"
    );
}

#[test]
fn test_ctfe_grammar_set_start_changes_root() {
    // With start rule "b", only "b" should match at the top level.
    let graph = parse(
        r#"(ctfe_grammar_parse
             "bb"
             (ctfe_grammar_set_start
               (ctfe_grammar_new "a <- /x+/\nb <- /b+/")
               "b"))"#,
    )
    .unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    let result = ev.run().unwrap();
    let RuntimeValue::Map(m) = result else {
        panic!("expected map, got {result:?}");
    };
    let ok = m
        .borrow()
        .get(&MapKey::Str(Rc::from("ok")))
        .cloned()
        .unwrap();
    assert_eq!(ok, RuntimeValue::Bool(true));
}

#[test]
fn test_ctfe_grammar_describe_exposes_compiled_rules_as_data() {
    let report = run_compile_time(
        r#"(ctfe_grammar_describe
             (ctfe_grammar_set_start
               (ctfe_grammar_new "root <- item+\nitem(name) <- $name")
               "root"))"#,
    );
    assert_eq!(
        map_field(&report, "start_rule"),
        Some(RuntimeValue::Str("root".into()))
    );
    assert_eq!(map_field(&report, "rule_count"), Some(RuntimeValue::Int(2)));

    let rules = list_items(&map_field(&report, "rules").expect("rules field"));
    assert_eq!(rules.len(), 2);
    assert_eq!(
        map_field(&rules[0], "name"),
        Some(RuntimeValue::Str("root".into()))
    );
    assert_eq!(
        map_field(&rules[1], "name"),
        Some(RuntimeValue::Str("item".into()))
    );
    assert_eq!(
        list_items(&map_field(&rules[1], "params").expect("params field")),
        vec![RuntimeValue::Str("name".into())]
    );
}

#[test]
fn test_ctfe_grammar_analyze_reports_reachability_and_choice_conflicts() {
    let report = run_compile_time(
        r#"(ctfe_grammar_analyze
             (ctfe_grammar_new "root <- \"a\" / \"ab\"\nunused <- \"z\""))"#,
    );
    assert_eq!(map_field(&report, "rule_count"), Some(RuntimeValue::Int(2)));
    assert_eq!(
        list_items(&map_field(&report, "unreachable").expect("unreachable field")),
        vec![RuntimeValue::Str("unused".into())]
    );
    assert_eq!(
        list_items(
            &map_field(&report, "prefix_shadowed_choice_alternatives")
                .expect("prefix shadow field")
        )
        .len(),
        1
    );
    assert_eq!(
        list_items(&map_field(&report, "errors").expect("errors field")).len(),
        1
    );
}

#[test]
fn test_ctfe_grammar_conflicts_normalizes_ambiguity_diagnostics() {
    let report = run_compile_time(
        r#"(ctfe_grammar_conflicts
             (ctfe_grammar_new "root <- \"a\" / \"ab\"\nunused <- \"z\""))"#,
    );
    assert_eq!(
        map_field(&report, "has_conflicts"),
        Some(RuntimeValue::Bool(true))
    );

    let conflicts = list_items(&map_field(&report, "conflicts").expect("conflicts field"));
    assert!(
        conflicts.iter().any(|conflict| {
            map_field(conflict, "kind")
                == Some(RuntimeValue::Str(
                    "prefix_shadowed_choice_alternative".into(),
                ))
                && map_field(conflict, "severity") == Some(RuntimeValue::Str("error".into()))
        }),
        "expected prefix shadowing conflict in {conflicts:?}"
    );
    assert!(
        conflicts.iter().any(|conflict| {
            map_field(conflict, "kind") == Some(RuntimeValue::Str("unreachable_rule".into()))
                && map_field(conflict, "severity") == Some(RuntimeValue::Str("warning".into()))
        }),
        "expected unreachable rule warning in {conflicts:?}"
    );
}

#[test]
fn test_ctfe_lexer_tokenize_uses_longest_match_and_skips_tokens() {
    let tokens = run_compile_time(
        r#"(ctfe_lexer_tokenize
             "== x"
             (list_of
               (map_of "kind" "EQ" "pattern" "=")
               (map_of "kind" "EQEQ" "pattern" "==")
               (map_of "kind" "WS" "pattern" "[ \t\n]+" "skip" true)
               (map_of "kind" "NAME" "pattern" "[a-z]+")))"#,
    );
    let tokens = list_items(&tokens);
    assert_eq!(tokens.len(), 2);
    assert_eq!(
        map_field(&tokens[0], "kind"),
        Some(RuntimeValue::Str("EQEQ".into()))
    );
    assert_eq!(
        map_field(&tokens[0], "text"),
        Some(RuntimeValue::Str("==".into()))
    );
    assert_eq!(
        map_field(&tokens[1], "kind"),
        Some(RuntimeValue::Str("NAME".into()))
    );
    assert_eq!(map_field(&tokens[1], "start"), Some(RuntimeValue::Int(3)));
}

#[test]
fn test_ctfe_grammar_parse_tokens_runs_tok_grammar_from_caap_tokens() {
    let result = run_compile_time(
        r#"(bind grammar
             (ctfe_grammar_new "root <- tok(NAME) tok(OP,'+') tok(NUMBER)")
             (bind tokens
               (ctfe_lexer_tokenize
                 "x + 1"
                 (list_of
                   (map_of "kind" "WS" "pattern" "[ \t\n]+" "skip" true)
                   (map_of "kind" "NAME" "pattern" "[a-z]+")
                   (map_of "kind" "OP" "pattern" "[+]")
                   (map_of "kind" "NUMBER" "pattern" "[0-9]+")))
               (ctfe_grammar_parse_tokens "x + 1" grammar tokens)))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(true)));
}

#[test]
fn test_ctfe_grammar_parse_tokens_accepts_manual_token_maps() {
    let result = run_compile_time(
        r#"(ctfe_grammar_parse_tokens
             "name"
             (ctfe_grammar_new "root <- tok(NAME)")
             (list_of (ctfe_lex_token "NAME" "name" 0 4)))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(true)));
}

fn run_compile_time(src: &str) -> RuntimeValue {
    common::run_ct(src)
}

fn run_compile_time_err(src: &str) -> String {
    common::run_ct_err(src)
}

fn map_field(value: &RuntimeValue, key: &str) -> Option<RuntimeValue> {
    let RuntimeValue::Map(m) = value else {
        return None;
    };
    m.borrow().get(&MapKey::Str(Rc::from(key))).cloned()
}

fn list_items(value: &RuntimeValue) -> Vec<RuntimeValue> {
    let RuntimeValue::List(items) = value else {
        panic!("expected list, got {value:?}");
    };
    items.borrow().clone()
}

#[test]
fn test_ctfe_grammar_parse_caap_semantic_predicate_and_action() {
    // A CAAP predicate that accepts -> parse succeeds.
    let result = run_compile_time(
        r#"(ctfe_grammar_parse "hello"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- @?yes /[a-z]+/") "root")
             (map_of "predicates" (map_of "yes" (lambda (ctx) true))))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(true)));

    // The same grammar with a predicate that rejects -> parse fails.
    let result = run_compile_time(
        r#"(ctfe_grammar_parse "hello"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- @?yes /[a-z]+/") "root")
             (map_of "predicates" (map_of "yes" (lambda (ctx) false))))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(false)));

    // A CAAP action transforms the matched value into a tagged node.
    let result = run_compile_time(
        r#"(ctfe_grammar_parse "hello"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- @mark(/[a-z]+/)") "root")
             (map_of "actions"
               (map_of "mark"
                 (lambda (ctx)
                   (map_of "kind" "marked"
                           "children" (list_of (get ctx "value" null)))))))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(true)));
    let value = map_field(&result, "value").unwrap();
    assert_eq!(
        map_field(&value, "kind"),
        Some(RuntimeValue::Str("marked".into()))
    );
}

#[test]
fn test_ctfe_grammar_parse_caap_predicate_receives_rule_stack() {
    // The predicate fires while `inner` is on the rule stack, so the context's
    // rule-stack is non-empty and the predicate accepts.
    let result = run_compile_time(
        r#"(ctfe_grammar_parse "hi"
             (ctfe_grammar_set_start
               (ctfe_grammar_new "root <- inner\ninner <- @?in_inner /[a-z]+/")
               "root")
             (map_of "predicates"
               (map_of "in_inner"
                 (lambda (ctx)
                   (lt 0 (size (get ctx "rule_stack" (list_of))))))))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(true)));
}

// ── Incremental parsing ───────────────────────────────────────────────────────

#[test]
fn test_ctfe_grammar_apply_edits_replaces_range() {
    let result = run_compile_time(
        r#"(ctfe_grammar_apply_edits "hello world"
             (list_of (map_of "start" 6 "old_end" 11 "replacement" "there")))"#,
    );
    assert_eq!(result, RuntimeValue::Str(Rc::from("hello there")));
}

#[test]
fn test_ctfe_grammar_apply_edits_rejects_start_after_old_end() {
    let error = run_compile_time_err(
        r#"(ctfe_grammar_apply_edits "abc"
             (list_of (map_of "start" 3 "old_end" 1 "replacement" "x")))"#,
    );
    assert!(
        error.contains("start") && error.contains("old_end"),
        "{error}"
    );
}

#[test]
fn test_ctfe_grammar_parse_incremental_reuses_cache_across_edits() {
    // A cache threaded across two parses returns the correct value for the
    // edited text (position-level reuse is internal; correctness is observable).
    let result = run_compile_time(
        r#"(bind grammar
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z][a-z]/") "root")
             (bind cache (ctfe_grammar_parse_cache)
               (bind first (ctfe_grammar_parse_incremental "ab" grammar cache)
                 (ctfe_grammar_parse_incremental "ac" grammar cache))))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(true)));
    assert_eq!(
        map_field(&result, "value"),
        Some(RuntimeValue::Str(Rc::from("ac")))
    );
}

#[test]
fn test_ctfe_grammar_parse_incremental_reports_failure_without_panicking() {
    let result = run_compile_time(
        r#"(ctfe_grammar_parse_incremental "123"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z]+/") "root")
             (ctfe_grammar_parse_cache))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(false)));
}

#[test]
fn test_ctfe_grammar_parse_incremental_rejects_non_cache_handle() {
    let error = run_compile_time_err(
        r#"(ctfe_grammar_parse_incremental "x"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z]+/") "root")
             "not-a-cache")"#,
    );
    assert!(error.contains("parse-cache"), "{error}");
}

// ── AST + incremental AST diff ────────────────────────────────────────────────

#[test]
fn test_ctfe_grammar_parse_ast_projects_tree() {
    let result = run_compile_time(
        r#"(ctfe_ast_to_map
             (get (ctfe_grammar_parse_ast "ab"
                    (ctfe_grammar_set_start
                      (ctfe_grammar_new "root <- letter+\nletter <- /[a-z]/") "root"))
                  "ast" null))"#,
    );
    assert_eq!(
        map_field(&result, "rule"),
        Some(RuntimeValue::Str(Rc::from("root")))
    );
    assert_eq!(map_field(&result, "error"), Some(RuntimeValue::Bool(false)));
    assert!(matches!(
        map_field(&result, "children"),
        Some(RuntimeValue::List(_))
    ));
}

#[test]
fn test_ctfe_grammar_parse_ast_reports_failure() {
    let result = run_compile_time(
        r#"(ctfe_grammar_parse_ast "123"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z]+/") "root"))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(false)));
}

#[test]
fn test_ctfe_grammar_parse_ast_tolerant_marks_unmatched_tail() {
    let result = run_compile_time(
        r#"(ctfe_ast_to_map
             (ctfe_grammar_parse_ast_tolerant "ab!!"
               (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z]+/") "root")))"#,
    );
    assert_eq!(map_field(&result, "error"), Some(RuntimeValue::Bool(true)));
}

#[test]
fn test_ctfe_ast_changed_ranges_and_reparse_incremental() {
    let result = run_compile_time(
        r#"(bind grammar
             (ctfe_grammar_set_start
               (ctfe_grammar_new "root <- letter+\nletter <- /[a-z]/") "root")
             (bind old (get (ctfe_grammar_parse_ast "ab" grammar) "ast" null)
               (bind new (get (ctfe_grammar_parse_ast "ac" grammar) "ast" null)
                 (bind edit (map_of "start" 1 "old_end" 2 "new_end" 2)
                   (map_of
                     "ranges" (ctfe_ast_changed_ranges old new edit)
                     "reparsed_rule"
                       (get (ctfe_ast_to_map (ctfe_ast_reparse_incremental old new edit))
                            "rule" null))))))"#,
    );
    assert!(matches!(
        map_field(&result, "ranges"),
        Some(RuntimeValue::List(_))
    ));
    assert_eq!(
        map_field(&result, "reparsed_rule"),
        Some(RuntimeValue::Str(Rc::from("root")))
    );
}

#[test]
fn test_ctfe_ast_to_map_rejects_non_ast_handle() {
    let error = run_compile_time_err(r#"(ctfe_ast_to_map "not-an-ast")"#);
    assert!(error.contains("ast-node"), "{error}");
}

// ── Validation / mutation / diff ──────────────────────────────────────────────

#[test]
fn test_ctfe_grammar_validate_accepts_well_formed_grammar() {
    let result = run_compile_time(
        r#"(ctfe_grammar_validate
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z]+/") "root"))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(true)));
}

#[test]
fn test_ctfe_grammar_validate_flags_undefined_rule_reference() {
    let result =
        run_compile_time(r#"(ctfe_grammar_validate (ctfe_grammar_new "root <- missing"))"#);
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(false)));
    let issues = map_field(&result, "issues").expect("issues field");
    assert!(
        !list_items(&issues).is_empty(),
        "expected validation issues"
    );
}

#[test]
fn test_ctfe_grammar_diff_reports_added_and_removed_rules() {
    let result = run_compile_time(
        r#"(ctfe_grammar_diff
             (ctfe_grammar_new "root <- a\na <- /x/")
             (ctfe_grammar_new "root <- b\nb <- /y/"))"#,
    );
    let added = list_items(&map_field(&result, "added_rules").unwrap());
    let removed = list_items(&map_field(&result, "removed_rules").unwrap());
    assert!(
        added.contains(&RuntimeValue::Str(Rc::from("b"))),
        "{added:?}"
    );
    assert!(
        removed.contains(&RuntimeValue::Str(Rc::from("a"))),
        "{removed:?}"
    );
    assert!(list_items(&map_field(&result, "changed_rules").unwrap())
        .contains(&RuntimeValue::Str(Rc::from("root"))));
}

#[test]
fn test_ctfe_grammar_signature_is_stable_and_distinguishing() {
    let result = run_compile_time(
        r#"(bind g1 (ctfe_grammar_new "root <- /[a-z]+/")
             (bind g2 (ctfe_grammar_new "root <- /[a-z]+/")
               (bind g3 (ctfe_grammar_new "root <- /[0-9]+/")
                 (map_of
                   "same" (eq (ctfe_grammar_signature g1) (ctfe_grammar_signature g2))
                   "diff" (eq (ctfe_grammar_signature g1) (ctfe_grammar_signature g3))))))"#,
    );
    assert_eq!(map_field(&result, "same"), Some(RuntimeValue::Bool(true)));
    assert_eq!(map_field(&result, "diff"), Some(RuntimeValue::Bool(false)));
}

#[test]
fn test_ctfe_grammar_rule_graph_lists_references() {
    let result =
        run_compile_time(r#"(ctfe_grammar_rule_graph (ctfe_grammar_new "root <- a\na <- /x/"))"#);
    let entries = list_items(&result);
    assert!(!entries.is_empty());
    let root = entries
        .iter()
        .find(|e| map_field(e, "rule") == Some(RuntimeValue::Str(Rc::from("root"))))
        .expect("root entry");
    assert!(
        list_items(&map_field(root, "refs").unwrap()).contains(&RuntimeValue::Str(Rc::from("a")))
    );
}

#[test]
fn test_ctfe_grammar_nullable_rules_detects_optional_rule() {
    let result = run_compile_time(
        r#"(ctfe_grammar_nullable_rules (ctfe_grammar_new "root <- a?\na <- /x/"))"#,
    );
    assert!(list_items(&result).contains(&RuntimeValue::Str(Rc::from("root"))));
}

#[test]
fn test_ctfe_grammar_remove_rule_drops_rule() {
    let result = run_compile_time(
        r#"(ctfe_grammar_rule_get
             (ctfe_grammar_remove_rule (ctfe_grammar_new "root <- a\na <- /x/") "a")
             "a" "GONE")"#,
    );
    assert_eq!(result, RuntimeValue::Str(Rc::from("GONE")));
}

#[test]
fn test_ctfe_grammar_replace_rule_rebinds_rule_body() {
    let result = run_compile_time(
        r#"(ctfe_grammar_parse "123"
             (ctfe_grammar_set_start
               (ctfe_grammar_replace_rule
                 (ctfe_grammar_new "root <- /[a-z]+/") "root" "/[0-9]+/")
               "root"))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(true)));
}

#[test]
fn test_ctfe_grammar_replace_rule_rejects_missing_rule() {
    let error = run_compile_time_err(
        r#"(ctfe_grammar_replace_rule (ctfe_grammar_new "root <- /x/") "nope" "/y/")"#,
    );
    assert!(error.contains("MissingRule"), "{error}");
}

// ── Prefix / profiled parsing ─────────────────────────────────────────────────

#[test]
fn test_ctfe_grammar_parse_prefix_matches_leading_run() {
    let result = run_compile_time(
        r#"(ctfe_grammar_parse_prefix "abc123"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z]+/") "root"))"#,
    );
    assert_eq!(
        map_field(&result, "value"),
        Some(RuntimeValue::Str(Rc::from("abc")))
    );
    assert_eq!(map_field(&result, "consumed"), Some(RuntimeValue::Int(3)));
    assert_eq!(map_field(&result, "eof"), Some(RuntimeValue::Bool(false)));
}

#[test]
fn test_ctfe_grammar_parse_prefix_honors_start_pos() {
    let result = run_compile_time(
        r#"(ctfe_grammar_parse_prefix "01abc"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z]+/") "root")
             2)"#,
    );
    assert_eq!(map_field(&result, "consumed"), Some(RuntimeValue::Int(3)));
}

#[test]
fn test_ctfe_grammar_parse_profiled_returns_value_and_profile() {
    let result = run_compile_time(
        r#"(ctfe_grammar_parse_profiled "hello"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z]+/") "root"))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(true)));
    assert_eq!(
        map_field(&result, "value"),
        Some(RuntimeValue::Str(Rc::from("hello")))
    );
    let profile = map_field(&result, "profile").expect("profile field");
    assert!(matches!(
        map_field(&profile, "expr_steps"),
        Some(RuntimeValue::Int(_))
    ));
    assert!(matches!(
        map_field(&profile, "rules"),
        Some(RuntimeValue::List(_))
    ));
}

#[test]
fn test_ctfe_grammar_parse_profiled_reports_failure() {
    let result = run_compile_time(
        r#"(ctfe_grammar_parse_profiled "123"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z]+/") "root"))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(false)));
}

#[test]
fn test_ctfe_grammar_parse_prefix_rejects_non_grammar() {
    let error = run_compile_time_err(r#"(ctfe_grammar_parse_prefix "x" "not-a-grammar")"#);
    assert!(error.contains("grammar object"), "{error}");
}

// ── Cross-grammar registry ────────────────────────────────────────────────────

#[test]
fn test_ctfe_grammar_registry_resolves_cross_grammar_reference() {
    let result = run_compile_time(
        r#"(bind idents
             (ctfe_grammar_set_start (ctfe_grammar_new "rule <- /[a-z]+/") "rule")
             (bind reg
               (ctfe_grammar_registry_register (ctfe_grammar_registry) "other" idents)
               (bind main
                 (ctfe_grammar_set_start (ctfe_grammar_new "start <- other::rule") "start")
                 (ctfe_grammar_parse_with_registry "hello" main reg))))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(true)));
    assert_eq!(
        map_field(&result, "value"),
        Some(RuntimeValue::Str(Rc::from("hello")))
    );
}

#[test]
fn test_ctfe_grammar_registry_list_reports_registered_names() {
    let result = run_compile_time(
        r#"(ctfe_grammar_registry_list
             (ctfe_grammar_registry_register
               (ctfe_grammar_registry) "g1"
               (ctfe_grammar_set_start (ctfe_grammar_new "rule <- /x/") "rule")))"#,
    );
    assert!(
        list_items(&result).contains(&RuntimeValue::Str(Rc::from("g1"))),
        "{result:?}"
    );
}

#[test]
fn test_ctfe_grammar_registry_register_rejects_non_grammar() {
    let error = run_compile_time_err(
        r#"(ctfe_grammar_registry_register (ctfe_grammar_registry) "x" "not-a-grammar")"#,
    );
    assert!(error.contains("grammar object"), "{error}");
}

#[test]
fn test_ctfe_grammar_parse_with_registry_rejects_non_registry() {
    let error = run_compile_time_err(
        r#"(ctfe_grammar_parse_with_registry "x"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z]+/") "root")
             "not-a-registry")"#,
    );
    assert!(error.contains("grammar-registry"), "{error}");
}

// ── Driver: guards + auto-scope ───────────────────────────────────────────────

#[test]
fn test_ctfe_grammar_parse_guard_accepts_and_rejects() {
    let accept = run_compile_time(
        r#"(ctfe_grammar_parse "let"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- @!kw(/[a-z]+/)") "root")
             (map_of "guards" (map_of "kw" (lambda (ctx) (eq (get ctx "value" null) "let")))))"#,
    );
    assert_eq!(map_field(&accept, "ok"), Some(RuntimeValue::Bool(true)));
    let reject = run_compile_time(
        r#"(ctfe_grammar_parse "xyz"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- @!kw(/[a-z]+/)") "root")
             (map_of "guards" (map_of "kw" (lambda (ctx) (eq (get ctx "value" null) "let")))))"#,
    );
    assert_eq!(map_field(&reject, "ok"), Some(RuntimeValue::Bool(false)));
}

#[test]
fn test_ctfe_grammar_parse_auto_scope_in_rule_succeeds() {
    let result = run_compile_time(
        r#"(ctfe_grammar_parse "hi"
             (ctfe_grammar_set_start
               (ctfe_grammar_new "root <- inner\ninner <- @?in_inner /[a-z]+/") "root")
             (map_of "auto_scope" true))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(true)));
}

#[test]
fn test_ctfe_grammar_parse_auto_scope_not_in_rule_rejects() {
    let result = run_compile_time(
        r#"(ctfe_grammar_parse "hi"
             (ctfe_grammar_set_start
               (ctfe_grammar_new "root <- inner\ninner <- @?not_in_root /[a-z]+/") "root")
             (map_of "auto_scope" true))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(false)));
}

// ── Grammar metadata config ───────────────────────────────────────────────────

#[test]
fn test_ctfe_grammar_set_metadata_is_observable_in_diff() {
    let result = run_compile_time(
        r#"(bind g (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z]+/") "root")
             (get (ctfe_grammar_diff g (ctfe_grammar_set_metadata g "trivia" "whitespace"))
                  "grammar_metadata_changed" null))"#,
    );
    assert_eq!(result, RuntimeValue::Bool(true));
}

#[test]
fn test_ctfe_grammar_set_metadata_under_custom_owner_is_observable() {
    // A custom owner round-trips through the grammar diff's metadata flag.
    let result = run_compile_time(
        r#"(bind g (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z]+/") "root")
             (get (ctfe_grammar_diff g
                    (ctfe_grammar_set_metadata g "return_type" "Expr" "root"))
                  "metadata_changed" null))"#,
    );
    assert_eq!(result, RuntimeValue::Bool(true));
}

#[test]
fn test_ctfe_grammar_set_metadata_rejects_non_grammar() {
    let error = run_compile_time_err(r#"(ctfe_grammar_set_metadata "nope" "trivia" "whitespace")"#);
    assert!(error.contains("grammar object"), "{error}");
}

// ── Error-recovery parsing ────────────────────────────────────────────────────

#[test]
fn test_ctfe_grammar_parse_recover_collects_all_valid_segments() {
    let result = run_compile_time(
        r#"(ctfe_grammar_parse_recover "alpha;gamma"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z]+/") "root")
             (map_of "sync_tokens" (list_of ";")))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(true)));
    assert!(!list_items(&map_field(&result, "forms").unwrap()).is_empty());
    assert!(list_items(&map_field(&result, "errors").unwrap()).is_empty());
}

#[test]
fn test_ctfe_grammar_parse_recover_reports_errors_past_first_failure() {
    let result = run_compile_time(
        r#"(ctfe_grammar_parse_recover "alpha;1;gamma"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z]+/") "root")
             (map_of "sync_tokens" (list_of ";")))"#,
    );
    assert_eq!(map_field(&result, "ok"), Some(RuntimeValue::Bool(false)));
    let errors = list_items(&map_field(&result, "errors").unwrap());
    assert!(!errors.is_empty(), "expected recovery errors");
    // valid segments still parsed despite the bad one
    assert!(!list_items(&map_field(&result, "forms").unwrap()).is_empty());
    // error carries a message + span
    assert!(map_field(&errors[0], "message").is_some());
    assert!(matches!(
        map_field(&errors[0], "start"),
        Some(RuntimeValue::Int(_))
    ));
}

#[test]
fn test_ctfe_grammar_parse_recover_rejects_bad_sync_tokens() {
    let error = run_compile_time_err(
        r#"(ctfe_grammar_parse_recover "x"
             (ctfe_grammar_set_start (ctfe_grammar_new "root <- /[a-z]+/") "root")
             (map_of "sync_tokens" "notlist"))"#,
    );
    assert!(error.contains("sync_tokens"), "{error}");
}
