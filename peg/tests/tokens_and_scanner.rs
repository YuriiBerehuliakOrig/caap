//! Scenario: **token-stream parsing** — `tok()`/`LexToken` matching and the
//! built-in `Scanner` feeding the token path (including precedence over an
//! external token stream).

use caap_peg as peg;

// ── LexToken / TokenRef ────────────────────────────────────────────────────

#[test]
fn lex_token_matches_by_kind() {
    let grammar = peg::Grammar::trusted_new("start <- tok(NAME)").with_start_rule("start");
    let tokens = vec![peg::LexToken::new("NAME", "hello", 0, 5)];
    let value = peg::ParseRequest::new(&grammar)
        .tokens(tokens)
        .run("hello")
        .expect("should parse");
    assert_eq!(value, peg::ParseValue::Text("hello".into()));
}

#[test]
fn lex_token_parse_enforces_max_steps_budget() {
    let grammar = peg::Grammar::trusted_new("start <- tok(NAME)").with_start_rule("start");
    let tokens = vec![peg::LexToken::new("NAME", "hello", 0, 5)];
    let config = peg::ParserConfig::default().with_max_steps(4);

    let error = peg::ParseRequest::new(&grammar)
        .config(config)
        .tokens(tokens)
        .run("hello")
        .expect_err("tokenized parse should honor max_steps");

    assert!(error.message.contains("input exceeds configured max_steps"));
}

#[test]
fn lex_token_parse_rejects_mismatched_token_text() {
    let grammar = peg::Grammar::trusted_new("start <- tok(NAME)").with_start_rule("start");
    let tokens = vec![peg::LexToken::new("NAME", "hullo", 0, 5)];

    let error = peg::ParseRequest::new(&grammar)
        .tokens(tokens)
        .run("hello")
        .expect_err("tokenized parse should validate token text");

    assert!(error.message.contains("text does not match input slice"));
}

#[test]
fn lex_token_parse_rejects_unsorted_or_overlapping_tokens() {
    let grammar = peg::Grammar::trusted_new("start <- tok(A) tok(B)").with_start_rule("start");
    let tokens = vec![
        peg::LexToken::new("B", "b", 1, 2),
        peg::LexToken::new("A", "a", 0, 1),
    ];

    let error = peg::ParseRequest::new(&grammar)
        .tokens(tokens)
        .run("ab")
        .expect_err("tokenized parse should validate token order");

    assert!(error.message.contains("overlaps or is out of order"));
}

#[test]
fn lex_token_parse_does_not_match_from_inside_token() {
    let grammar = peg::Grammar::trusted_new("start <- 'h' tok(NAME)").with_start_rule("start");
    let tokens = vec![peg::LexToken::new("NAME", "hello", 0, 5)];

    let error = peg::ParseRequest::new(&grammar)
        .tokens(tokens)
        .run("hello")
        .expect_err("tok() must only match at token boundaries");

    assert!(
        error.message.contains("expected"),
        "expected parser failure, got {error:?}"
    );
}

#[test]
fn lex_token_fails_on_wrong_kind() {
    let grammar = peg::Grammar::trusted_new("start <- tok(NUMBER)").with_start_rule("start");
    let tokens = vec![peg::LexToken::new("NAME", "hello", 0, 5)];
    let result = peg::ParseRequest::new(&grammar).tokens(tokens).run("hello");
    assert!(result.is_err());
}

#[test]
fn lex_token_matches_by_kind_and_text() {
    let grammar = peg::Grammar::trusted_new("start <- tok(NAME,'hello')").with_start_rule("start");
    let tokens = vec![peg::LexToken::new("NAME", "hello", 0, 5)];
    let value = peg::ParseRequest::new(&grammar)
        .tokens(tokens)
        .run("hello")
        .expect("should parse");
    assert_eq!(value, peg::ParseValue::Text("hello".into()));
}

#[test]
fn lex_token_fails_on_wrong_text() {
    let grammar = peg::Grammar::trusted_new("start <- tok(NAME,'world')").with_start_rule("start");
    let tokens = vec![peg::LexToken::new("NAME", "hello", 0, 5)];
    let result = peg::ParseRequest::new(&grammar).tokens(tokens).run("hello");
    assert!(result.is_err());
}

#[test]
fn lex_token_sequence_parses_multiple_tokens() {
    let grammar = peg::Grammar::trusted_new("start <- tok(NAME) tok(OP) tok(NUMBER)")
        .with_start_rule("start");
    let tokens = vec![
        peg::LexToken::new("NAME", "x", 0, 1),
        peg::LexToken::new("OP", "+", 2, 3),
        peg::LexToken::new("NUMBER", "1", 4, 5),
    ];
    let value = peg::ParseRequest::new(&grammar)
        .tokens(tokens)
        .run("x + 1")
        .expect("should parse");
    assert!(matches!(value, peg::ParseValue::Node(ref n, _) if &**n == "sequence"));
}

#[test]
fn tok_without_lex_tokens_returns_error() {
    let grammar = peg::Grammar::trusted_new("start <- tok(NAME)").with_start_rule("start");
    let result = peg::parse("hello", &grammar);
    assert!(result.is_err());
    let msg = result.unwrap_err().message;
    assert!(
        msg.contains("tok()")
            || msg.contains("token list")
            || msg.contains("parse_with_lex_tokens"),
        "message: {msg}"
    );
}

// ── Built-in scanner (Scanner + ParseRequest::scan) ────────────────────────

#[test]
fn scanner_feeds_tok_grammar_end_to_end() {
    // No external lexer: the Scanner produces the token stream that tok(...) consumes.
    let scanner = peg::Scanner::new()
        .token("NUMBER", r"[0-9]+")
        .unwrap()
        .literal("PLUS", "+")
        .skip(r"\s+")
        .unwrap();
    let grammar = peg::Grammar::trusted_new("sum <- tok(NUMBER) (tok(PLUS) tok(NUMBER))*")
        .with_start_rule("sum");
    let value = peg::ParseRequest::new(&grammar)
        .scan(&scanner)
        .run("1 + 22 + 333")
        .expect("scan + parse should succeed");
    assert!(matches!(value, peg::ParseValue::Node(..)));
}

#[test]
fn scanner_scan_failure_surfaces_as_parse_error() {
    let scanner = peg::Scanner::new().token("NUMBER", r"[0-9]+").unwrap();
    let grammar = peg::Grammar::trusted_new("n <- tok(NUMBER)").with_start_rule("n");
    let err = peg::ParseRequest::new(&grammar)
        .scan(&scanner)
        .run("12x")
        .expect_err("an untokenisable byte should fail the run");
    assert!(
        err.message.contains("no token rule matches"),
        "{}",
        err.message
    );
}

#[test]
fn explicit_tokens_take_precedence_over_a_scanner() {
    // A scanner is attached, but an explicit token stream wins — proving the
    // documented precedence (the scanner is not consulted).
    let scanner = peg::Scanner::new().literal("BANG", "!"); // would fail on "hi"
    let grammar = peg::Grammar::trusted_new("start <- tok(NAME)").with_start_rule("start");
    let value = peg::ParseRequest::new(&grammar)
        .scan(&scanner)
        .tokens(vec![peg::LexToken::new("NAME", "hi", 0, 2)])
        .run("hi")
        .expect("explicit tokens should be used, not the scanner");
    assert_eq!(value, peg::ParseValue::Text("hi".into()));
}
