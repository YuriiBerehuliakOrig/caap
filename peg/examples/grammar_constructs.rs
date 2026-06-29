//! A catalogue of every grammar construct the text syntax supports, each with a
//! tiny parse. Mirrors `docs/peg/grammar-syntax.md`.
//!
//! Run with: `cargo run --example grammar_constructs`

use caap_peg::{Grammar, ParseRequest, ParseValue};

/// Parse `text` with a one-rule grammar `root <- {body}` and print the outcome.
fn demo(label: &str, body: &str, text: &str) {
    let g = Grammar::trusted_new(format!("root <- {body}")).with_start_rule("root");
    match ParseRequest::new(&g).run(text) {
        Ok(v) => println!("{label:<26} {text:<14?} -> {}", brief(&v)),
        Err(e) => println!("{label:<26} {text:<14?} -> ERR: {}", e.message),
    }
}

fn brief(v: &ParseValue) -> String {
    match v {
        ParseValue::Text(t) => format!("Text({t:?})"),
        ParseValue::Nil => "Nil".into(),
        ParseValue::Number(n) => format!("Number({n})"),
        ParseValue::Node(tag, kids) => format!("Node({tag}, {} kids)", kids.len()),
        ParseValue::Named(n, _) => format!("Named({n})"),
        ParseValue::SpannedValue { start, end, .. } => format!("Spanned[{start},{end}]"),
    }
}

fn main() {
    println!("── Terminals ──");
    demo("literal", "'hello'", "hello");
    demo("regex", "/[a-z]+/", "abc");
    demo("char class", "[a-z0-9_]+", "a1_z");
    demo("char class (negated)", "[^,]+", "abc");
    demo("dot (any char)", "...", "xyz");
    demo("case-insensitive literal", "i\"select\"", "SeLeCt");

    println!("\n── Structural combinators ──");
    demo("sequence", "'a' 'b' 'c'", "abc");
    demo("ordered choice", "'cat' / 'car'", "car");
    demo("optional ?", "'a' 'b'?", "a");
    demo("zero-or-more *", "'a'*", "aaaa");
    demo("one-or-more +", "'a'+", "aaa");
    demo("counted {m,n}", "'a'{2,3}", "aaa");
    demo("positive lookahead &", "&'a' 'a'", "a");
    demo("negative lookahead !", "!'b' 'a'", "a");
    demo("lookbehind &<", "'ab' &<'b'", "ab");
    demo("cut ~", "'[' ~ ']'", "[]");
    demo("eager !!", "!!'a'", "a");

    println!("\n── Precedence climbing ──");
    demo(
        "prec (infix levels)",
        "prec(/[0-9]+/, infixl(\"+\", \"-\"), infixl(\"*\", \"/\"))",
        "1+2*3",
    );

    println!("\n── Repetition with separators ──");
    demo("sep_plus (drop seps)", "sep_plus(/[a-z]+/, ',')", "a,b,c");
    demo(
        "interspersed (keep seps)",
        "interspersed(/[a-z]+/, ',')",
        "a,b,c",
    );

    println!("\n── Bindings & captures ──");
    demo("named binding", "k:/[a-z]+/", "abc");
    demo("capture", "capture(\"w\", /[a-z]+/)", "abc");
    demo(
        "backref (matched tags)",
        "t:/[a-z]+/ '>' backref(\"t\")",
        "div>div",
    );

    println!("\n── Delimiter-bounded text ──");
    demo("island", "island(\"<\", \">\")", "<hi>");
    demo(
        "raw_block (nested)",
        "raw_block(\"(\", \")\", \"parens\")",
        "(a(b)c)",
    );

    println!("\n── Keywords ──");
    demo("hard keyword kw()", "kw(\"if\")", "if");
    demo("kw not a prefix", "kw(\"if\")", "iffy"); // expected to fail

    println!("\n── Trivia control ──");
    demo("no_trivia / tight", "no_trivia('a' 'b')", "ab");
    demo(
        "with_trivia override",
        "with_trivia(\"whitespace\", 'a' 'b')",
        "a b",
    );

    println!("\n── Error labelling & recovery ──");
    demo(
        "expected(\"msg\", e)",
        "expected(\"a digit\", /[0-9]/)",
        "5",
    );
    // Grammar-level recovery: skip to the sync literal, localising the bad region.
    demo("recover fallback", "'good' / recover(\".\")", "garbage.");
}
