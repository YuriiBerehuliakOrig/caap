//! Scenario: each user-facing **grammar construct** parses and produces the
//! documented value — capture, island/raw-block, counted repetition,
//! case-insensitive literals, lookbehind, backreferences, operator precedence,
//! eager, and hard keywords.

use caap_peg as peg;

// ── Capture ────────────────────────────────────────────────────────────────

#[test]
fn capture_wraps_result_in_spanned_value() {
    // Use /[a-z]+/ (full-word regex) so the inner value is a single Text node.
    let grammar =
        peg::Grammar::trusted_new("start <- capture(\"src\", /[a-z]+/)").with_start_rule("start");
    let value = peg::parse("hello", &grammar).expect("parse should succeed");
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
    let grammar =
        peg::Grammar::trusted_new("start <- capture(\"src\", \"x\")").with_start_rule("start");
    assert!(peg::parse("y", &grammar).is_err());
}

// ── Island ────────────────────────────────────────────────────────────────

#[test]
fn island_matches_content_between_delimiters() {
    let grammar =
        peg::Grammar::trusted_new("start <- island(\"<\", \">\")").with_start_rule("start");
    let value = peg::parse("<hello>", &grammar).expect("parse should succeed");
    match value {
        peg::ParseValue::Text(t) => assert_eq!(&*t, "hello"),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn island_include_delims_returns_full_text() {
    let grammar =
        peg::Grammar::trusted_new("start <- island(\"<\", \">\", true)").with_start_rule("start");
    let value = peg::parse("<hello>", &grammar).expect("parse should succeed");
    match value {
        peg::ParseValue::Text(t) => assert_eq!(&*t, "<hello>"),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn island_fails_when_start_delimiter_absent() {
    let grammar =
        peg::Grammar::trusted_new("start <- island(\"<\", \">\")").with_start_rule("start");
    assert!(peg::parse("hello", &grammar).is_err());
}

#[test]
fn island_does_not_nest() {
    // With non-nested island, the first `>` closes the match.
    let grammar =
        peg::Grammar::trusted_new("start <- island(\"<\", \">\")").with_start_rule("start");
    // Input: "<a<b>" — island finds closing ">" after "a<b"
    let value = peg::parse("<a<b>", &grammar).expect("parse should succeed");
    match value {
        peg::ParseValue::Text(t) => assert_eq!(&*t, "a<b"),
        other => panic!("expected Text, got {other:?}"),
    }
}

// ── RawBlock ──────────────────────────────────────────────────────────────

#[test]
fn raw_block_matches_balanced_delimiters() {
    let grammar =
        peg::Grammar::trusted_new("start <- raw_block(\"(\", \")\")").with_start_rule("start");
    let value = peg::parse("(hello)", &grammar).expect("parse should succeed");
    match value {
        peg::ParseValue::Text(t) => assert_eq!(&*t, "hello"),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn raw_block_handles_nested_delimiters() {
    let grammar =
        peg::Grammar::trusted_new("start <- raw_block(\"(\", \")\")").with_start_rule("start");
    let value = peg::parse("(a(b)c)", &grammar).expect("parse should succeed");
    match value {
        peg::ParseValue::Text(t) => assert_eq!(&*t, "a(b)c"),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn raw_block_errors_on_unterminated() {
    let grammar =
        peg::Grammar::trusted_new("start <- raw_block(\"(\", \")\")").with_start_rule("start");
    assert!(peg::parse("(unterminated", &grammar).is_err());
}

#[test]
fn raw_block_rejects_identical_delimiters() {
    let grammar =
        peg::Grammar::trusted_new("start <- raw_block(\"|\", \"|\")").with_start_rule("start");
    let err = peg::parse("|x|", &grammar).expect_err("identical delimiters must error");
    assert_eq!(err.code.as_deref(), Some("raw_block_identical_delimiters"));
}

// ── Counted repetition e{m,n} ─────────────────────────────────────────────

#[test]
fn counted_repetition_enforces_bounds() {
    // Exactly 2–3 digits.
    let grammar = peg::Grammar::trusted_new("root <- /[0-9]/{2,3}").with_start_rule("root");
    assert!(peg::parse("1", &grammar).is_err(), "one digit < min");
    assert!(peg::parse("12", &grammar).is_ok(), "two digits in range");
    assert!(peg::parse("123", &grammar).is_ok(), "three digits in range");
    // Four digits: matches the first 3, then trailing input is unconsumed → err.
    assert!(
        peg::parse("1234", &grammar).is_err(),
        "four digits exceeds max"
    );
}

#[test]
fn counted_repetition_exact_and_open() {
    let exact = peg::Grammar::trusted_new("root <- 'a'{3}").with_start_rule("root");
    assert!(peg::parse("aa", &exact).is_err());
    assert!(peg::parse("aaa", &exact).is_ok());

    let open = peg::Grammar::trusted_new("root <- 'a'{2,}").with_start_rule("root");
    assert!(peg::parse("a", &open).is_err());
    assert!(peg::parse("aaaa", &open).is_ok());
}

// ── Case-insensitive literals ─────────────────────────────────────────────

#[test]
fn case_insensitive_literal_matches_any_casing() {
    let grammar = peg::Grammar::trusted_new("root <- i\"select\"").with_start_rule("root");
    for input in ["select", "SELECT", "SeLeCt"] {
        let value = peg::parse(input, &grammar).unwrap_or_else(|e| panic!("{input}: {e:?}"));
        // Yields the actual matched casing.
        assert_eq!(value.inner(), &peg::ParseValue::Text(input.into()));
    }
    assert!(peg::parse("other", &grammar).is_err());
}

// ── Lookbehind ─────────────────────────────────────────────────────────────

#[test]
fn lookbehind_positive_and_negative() {
    // `&<'b'`: 'c' only when preceded by 'b'.
    let ok = peg::Grammar::trusted_new("root <- 'ab' &<'b' 'c'").with_start_rule("root");
    assert!(peg::parse("abc", &ok).is_ok());

    let mismatch = peg::Grammar::trusted_new("root <- 'ab' &<'z' 'c'").with_start_rule("root");
    assert!(peg::parse("abc", &mismatch).is_err());

    // `!<'b'`: fails because 'c' *is* preceded by 'b'.
    let neg = peg::Grammar::trusted_new("root <- 'ab' !<'b' 'c'").with_start_rule("root");
    assert!(peg::parse("abc", &neg).is_err());
}

// ── Backreferences ─────────────────────────────────────────────────────────

#[test]
fn backref_matches_prior_capture() {
    let grammar = peg::Grammar::trusted_new("root <- x:id \"=\" backref(\"x\")\nid <- /[a-z]+/")
        .with_start_rule("root");
    assert!(
        peg::parse("foo=foo", &grammar).is_ok(),
        "same token matches"
    );
    assert!(
        peg::parse("foo=bar", &grammar).is_err(),
        "different token fails"
    );
}

#[test]
fn backref_capture_is_rolled_back_on_failed_alternative() {
    // `a` captures y='X' then fails; the choice backtracks to `b`. A correct
    // (transactional) capture store discards y, so `backref("y")` has nothing to
    // match and the parse fails. A leaky store would wrongly match the stale 'X'.
    let grammar = peg::Grammar::trusted_new(
        "root <- (a / b) backref(\"y\")\n\
         a <- y:'X' 'Z'\n\
         b <- 'X'",
    )
    .with_start_rule("root");
    assert!(
        peg::parse("XX", &grammar).is_err(),
        "rolled-back capture must not leak into a later backref"
    );
}

// ── Operator precedence / associativity ───────────────────────────────────

/// Render a `binop`/`unary_*`/operand tree as a fully-parenthesised string.
fn render_binop(value: &peg::ParseValue) -> String {
    let op_text = |v: &peg::ParseValue| match v {
        peg::ParseValue::Text(t) => t.to_string(),
        _ => "?".to_string(),
    };
    match value {
        peg::ParseValue::Node(name, kids) if &**name == "binop" => format!(
            "({}{}{})",
            render_binop(&kids[0]),
            op_text(&kids[1]),
            render_binop(&kids[2]),
        ),
        peg::ParseValue::Node(name, kids) if &**name == "unary_prefix" => {
            format!("({}{})", op_text(&kids[0]), render_binop(&kids[1]))
        }
        peg::ParseValue::Node(name, kids) if &**name == "unary_postfix" => {
            format!("({}{})", render_binop(&kids[0]), op_text(&kids[1]))
        }
        peg::ParseValue::Text(t) => t.to_string(),
        peg::ParseValue::SpannedValue { value, .. } => render_binop(value),
        other => format!("{other:?}"),
    }
}

#[test]
fn precedence_binds_higher_level_tighter() {
    let grammar = peg::Grammar::trusted_new(
        "expr <- prec(num, infixl('+', '-'), infixl('*', '/'))\nnum <- /[0-9]+/",
    )
    .with_start_rule("expr");
    let value = peg::parse("1+2*3", &grammar).expect("parse");
    assert_eq!(render_binop(&value), "(1+(2*3))");
}

#[test]
fn precedence_left_associates_same_level() {
    let grammar = peg::Grammar::trusted_new("expr <- prec(num, infixl('+', '-'))\nnum <- /[0-9]+/")
        .with_start_rule("expr");
    let value = peg::parse("1-2-3", &grammar).expect("parse");
    assert_eq!(render_binop(&value), "((1-2)-3)");
}

#[test]
fn precedence_right_associates() {
    let grammar = peg::Grammar::trusted_new("expr <- prec(num, infixr('^'))\nnum <- /[0-9]+/")
        .with_start_rule("expr");
    let value = peg::parse("2^3^2", &grammar).expect("parse");
    assert_eq!(render_binop(&value), "(2^(3^2))");
}

#[test]
fn precedence_prefix_operator() {
    // Prefix `-` declared higher than infix `+`, so it binds only its operand.
    let grammar =
        peg::Grammar::trusted_new("expr <- prec(num, infixl('+'), prefix('-'))\nnum <- /[0-9]+/")
            .with_start_rule("expr");
    let value = peg::parse("-1+2", &grammar).expect("parse");
    assert_eq!(render_binop(&value), "((-1)+2)");
}

#[test]
fn precedence_postfix_operator() {
    let grammar = peg::Grammar::trusted_new("expr <- prec(num, postfix('!'))\nnum <- /[0-9]+/")
        .with_start_rule("expr");
    assert_eq!(render_binop(&peg::parse("5!", &grammar).unwrap()), "(5!)");
    assert_eq!(
        render_binop(&peg::parse("5!!", &grammar).unwrap()),
        "((5!)!)"
    );
}

#[test]
fn precedence_non_associative_rejects_chain() {
    let grammar = peg::Grammar::trusted_new("expr <- prec(num, infixn('='))\nnum <- /[0-9]+/")
        .with_start_rule("expr");
    assert!(peg::parse("1=2", &grammar).is_ok(), "single application ok");
    assert!(
        peg::parse("1=2=3", &grammar).is_err(),
        "chaining is rejected"
    );
}

#[test]
fn precedence_ternary() {
    // `?`/`:` ternary, with `+` as a lower-precedence operand combinator.
    let grammar = peg::Grammar::trusted_new(
        "expr <- prec(num, ternary('?', ':'), infixl('+'))\nnum <- /[0-9]+/",
    )
    .with_start_rule("expr");
    let value = peg::parse("1?2:3", &grammar).expect("parse");
    match value {
        peg::ParseValue::Node(name, kids) => {
            assert_eq!(&*name, "ternary");
            assert_eq!(kids.len(), 3);
        }
        other => panic!("expected ternary node, got {other:?}"),
    }
    // Right-associative else branch.
    assert!(peg::parse("1?2:3?4:5", &grammar).is_ok());
}

#[test]
fn precedence_single_operand_has_no_binop() {
    let grammar = peg::Grammar::trusted_new("expr <- prec(num, infixl('+'))\nnum <- /[0-9]+/")
        .with_start_rule("expr");
    let value = peg::parse("42", &grammar).expect("parse");
    assert_eq!(value, peg::ParseValue::Text("42".into()));
}

// ── Eager ─────────────────────────────────────────────────────────────────

#[test]
fn eager_succeeds_on_match() {
    let grammar = peg::Grammar::trusted_new("start <- eager(\"x\")").with_start_rule("start");
    let value = peg::parse("x", &grammar).expect("eager match should succeed");
    assert_eq!(value, peg::ParseValue::Text("x".into()));
}

#[test]
fn eager_escalates_failure_to_error() {
    let grammar = peg::Grammar::trusted_new("start <- eager(\"x\")").with_start_rule("start");
    // On mismatch, Eager returns a ParseError (not a soft failure).
    assert!(peg::parse("y", &grammar).is_err());
}

#[test]
fn eager_double_bang_syntax() {
    // `!!expr` is shorthand for eager(expr).
    let grammar = peg::Grammar::trusted_new("start <- !!\"x\"").with_start_rule("start");
    assert!(peg::parse("y", &grammar).is_err());
    peg::parse("x", &grammar).expect("should succeed on match");
}

// ── HardKeyword ───────────────────────────────────────────────────────────

#[test]
fn hard_keyword_matches_standalone_word() {
    let grammar = peg::Grammar::trusted_new("start <- kw(\"if\")").with_start_rule("start");
    let value = peg::parse("if", &grammar).expect("kw match should succeed");
    assert_eq!(value, peg::ParseValue::Text("if".into()));
}

#[test]
fn hard_keyword_rejects_prefix_of_longer_identifier() {
    // "iffy" starts with "if" but is not the keyword.
    let grammar = peg::Grammar::trusted_new("start <- kw(\"if\")").with_start_rule("start");
    assert!(peg::parse("iffy", &grammar).is_err());
}
