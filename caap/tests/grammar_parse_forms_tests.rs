//! `ctfe_grammar_parse_forms` — the one-call grammar-parse API: merge grammar
//! units, parse TEXT, get lowered surface forms back as data. A parse failure
//! of the text is DATA ({ok:false, error:…}), not an evaluation error, so
//! language-building callers can branch on it.

use caap_core::{frontend::parse, RuntimeValue, Unit};

mod common;

fn run_grammar(case: &str, authoring: &str, body: &str) -> RuntimeValue {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!("caap-gpf-{case}-{}.caap", std::process::id()));
    std::fs::write(&path, "(int_add 1 2)\n").unwrap();
    let escaped = authoring
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    let source = format!(
        "(bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
          (do
            (ctfe_unit_syntax_authoring_source_apply! unit \"{escaped}\")
            {body}))",
        path.display()
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph(case, graph).unwrap();
    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();
    let _ = std::fs::remove_file(&path);
    value
}

fn run(case: &str, body: &str) -> RuntimeValue {
    run_grammar(
        case,
        "add rule word = /[A-Za-z]+/ -> surface.symbol\nreplace rule form = word\n",
        body,
    )
}

fn unwrap_list(value: RuntimeValue) -> Vec<RuntimeValue> {
    let RuntimeValue::List(items) = value else {
        panic!("expected list, got {value:?}")
    };
    let items = items.borrow();
    items.clone()
}

#[test]
fn parses_text_under_the_merged_grammar_and_returns_forms() {
    let value = run(
        "ok",
        r#"(bind result (ctfe_grammar_parse_forms compiler (list_of unit) "hello world")
            (list_of
              (get result "ok" false)
              (size (get result "forms" (list_of)))))"#,
    );
    let RuntimeValue::List(items) = value else {
        panic!("expected list, got {value:?}")
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true), "parse must succeed");
    assert_eq!(items[1], RuntimeValue::Int(2), "two word forms");
}

#[test]
fn a_text_parse_failure_is_data_not_an_error() {
    let value = run(
        "fail",
        r#"(bind result (ctfe_grammar_parse_forms compiler (list_of unit) "123!!!")
            (list_of
              (get result "ok" true)
              (value_type (get result "error" null))))"#,
    );
    let RuntimeValue::List(items) = value else {
        panic!("expected list, got {value:?}")
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(false), "ok must be false");
    assert_eq!(
        items[1],
        RuntimeValue::Str("string".into()),
        "error carried"
    );
}

#[test]
fn an_unknown_start_rule_is_a_hard_error() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!("caap-gpf-start-{}.caap", std::process::id()));
    std::fs::write(&path, "(int_add 1 2)\n").unwrap();
    let source = format!(
        "(bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
          (ctfe_grammar_parse_forms compiler (list_of unit) \"x\" \"nonexistent_rule\"))",
        path.display()
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("start", graph).unwrap();
    let result = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, []);
    let _ = std::fs::remove_file(&path);
    assert!(result.is_err(), "setup problems stay hard errors");
}

// ── fix #1: `rule` names the producing rule, not the hook kind ────────────

#[test]
fn forms_carry_the_producing_rule_name() {
    let items = unwrap_list(run_grammar(
        "rule-name",
        "add rule ident = /[a-z]+/ -> surface.symbol\n\
         add rule num = /[0-9]+/ -> surface.integer\n\
         add rule st = items:ident items:num -> surface.list\n\
         replace rule form = st\n",
        r#"(bind result (ctfe_grammar_parse_forms compiler (list_of unit) "x 1")
            (bind st_form (get (get result "forms") 0)
              (list_of
                (get st_form "rule")
                (get (get (get st_form "items") 0) "rule")
                (get (get (get st_form "items") 1) "rule"))))"#,
    ));
    assert_eq!(items[0], RuntimeValue::Str("st".into()), "list rule name");
    assert_eq!(items[1], RuntimeValue::Str("ident".into()));
    assert_eq!(items[2], RuntimeValue::Str("num".into()));
}

// ── fix #2: spans start at the token, never at leading trivia ─────────────

#[test]
fn spans_start_at_the_token_not_the_leading_trivia() {
    // "   alpha ; gap\n  beta" — alpha at 3, beta at 17 (after the `;` line
    // comment of the DEFAULT convention and the indent).
    let items = unwrap_list(run(
        "span-trim",
        r#"(bind result (ctfe_grammar_parse_forms compiler (list_of unit) "   alpha ; gap\n  beta")
            (bind forms (get result "forms")
              (list_of
                (get (get (get forms 0) "span") "start")
                (get (get (get forms 0) "span") "end")
                (get (get (get forms 1) "span") "start"))))"#,
    ));
    assert_eq!(items[0], RuntimeValue::Int(3), "alpha starts at its byte");
    assert_eq!(items[1], RuntimeValue::Int(8));
    assert_eq!(items[2], RuntimeValue::Int(17), "beta starts at its byte");
}

// ── fix #3: honest delimiter — real bracket literal or null ───────────────

#[test]
fn list_delimiters_are_honest_and_atoms_carry_none() {
    let items = unwrap_list(run_grammar(
        "delims",
        "add rule num = /[0-9]+/ -> surface.integer\n\
         add rule plist = \"(\" items:num* \")\" -> surface.list\n\
         add rule blist = \"[\" items:num* \"]\" -> surface.list\n\
         add rule clist = \"{\" items:num* \"}\" -> surface.list\n\
         add rule bare = items:num items:num -> surface.list\n\
         replace rule form = plist | blist | clist | bare\n",
        r#"(bind result (ctfe_grammar_parse_forms compiler (list_of unit) "(1) [2] {3} 4 5")
            (bind forms (get result "forms")
              (list_of
                (get (get forms 0) "delimiter")
                (get (get forms 1) "delimiter")
                (get (get forms 2) "delimiter")
                (get (get forms 3) "delimiter")
                (get (get (get (get forms 0) "items") 0) "delimiter"))))"#,
    ));
    assert_eq!(items[0], RuntimeValue::Str("paren".into()));
    assert_eq!(items[1], RuntimeValue::Str("bracket".into()));
    assert_eq!(items[2], RuntimeValue::Str("brace".into()));
    assert_eq!(items[3], RuntimeValue::Null, "no bracket literal → null");
    assert_eq!(items[4], RuntimeValue::Null, "atoms carry no delimiter");
}

// ── fix #4: `set comment = …` controls the comment convention ─────────────

#[test]
fn set_comment_directive_replaces_the_default_convention() {
    // Under `//`, `;` is an ordinary token and a trailing comment at EOF
    // (no final newline) is trivia.
    let items = unwrap_list(run_grammar(
        "comment-custom",
        "set comment = \"//\"\n\
         add rule ident = /[a-z]+/ -> surface.symbol\n\
         add rule semi = /;/ -> surface.symbol\n\
         add rule st = items:ident items:semi -> surface.list\n\
         replace rule form = st\n",
        r#"(bind result (ctfe_grammar_parse_forms compiler (list_of unit) "x; // trailing")
            (bind st_form (get (get result "forms") 0)
              (list_of
                (get result "ok" false)
                (size (get result "forms"))
                (get (get (get st_form "items") 1) "value"))))"#,
    ));
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Int(1));
    assert_eq!(
        items[2],
        RuntimeValue::Str(";".into()),
        "`;` is a token, not a comment"
    );
}

#[test]
fn set_comment_none_disables_comments_entirely() {
    let items = unwrap_list(run_grammar(
        "comment-none",
        "set comment = none\n\
         add rule word = /[A-Za-z]+/ -> surface.symbol\n\
         replace rule form = word\n",
        r#"(bind result (ctfe_grammar_parse_forms compiler (list_of unit) "x ; y")
            (list_of (get result "ok" true)))"#,
    ));
    assert_eq!(
        items[0],
        RuntimeValue::Bool(false),
        "`;` must be a parse failure when comments are off"
    );
}

#[test]
fn the_default_semicolon_convention_stays_without_a_directive() {
    let items = unwrap_list(run(
        "comment-default",
        r#"(bind result (ctfe_grammar_parse_forms compiler (list_of unit) "x ; comment\ny ; trailing at eof")
            (list_of (get result "ok" false) (size (get result "forms"))))"#,
    ));
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Int(2), "comments stay trivia");
}

// ── C7: optional real source path for the returned spans ──────────────────

#[test]
fn an_explicit_path_argument_reaches_the_form_spans() {
    let items = unwrap_list(run(
        "real-path",
        r#"(bind with_path (ctfe_grammar_parse_forms compiler (list_of unit) "hello" null "demo.clike")
            (bind without (ctfe_grammar_parse_forms compiler (list_of unit) "hello")
              (list_of
                (get (get (get (get with_path "forms") 0) "span") "path")
                (get (get (get (get without "forms") 0) "span") "path"))))"#,
    ));
    assert_eq!(
        items[0],
        RuntimeValue::Str("demo.clike".into()),
        "explicit path wins"
    );
    assert_eq!(
        items[1],
        RuntimeValue::Str("<ctfe_grammar_parse_forms>".into()),
        "default stays the synthetic marker"
    );
}

// ── fix #5: multicapture — same-name labels concatenate in source order ───

#[test]
fn same_label_captures_concatenate_across_groups_and_repeats() {
    let items = unwrap_list(run_grammar(
        "multicap",
        "add rule num = /[0-9]+/ -> surface.integer\n\
         add rule csv = \"(\" items:num (\",\" items:num)* \")\" -> surface.list\n\
         replace rule form = csv\n",
        r#"(bind result (ctfe_grammar_parse_forms compiler (list_of unit) "(1,2,3)")
            (bind csv_form (get (get result "forms") 0)
              (list_of
                (size (get csv_form "items"))
                (get (get (get csv_form "items") 0) "value")
                (get (get (get csv_form "items") 1) "value")
                (get (get (get csv_form "items") 2) "value")
                (get csv_form "delimiter"))))"#,
    ));
    assert_eq!(items[0], RuntimeValue::Int(3), "labels concatenated");
    assert_eq!(items[1], RuntimeValue::Int(1));
    assert_eq!(items[2], RuntimeValue::Int(2));
    assert_eq!(items[3], RuntimeValue::Int(3));
    assert_eq!(
        items[4],
        RuntimeValue::Str("paren".into()),
        "unlabeled literals dropped, bracket still honest"
    );
}
