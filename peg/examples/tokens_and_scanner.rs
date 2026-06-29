//! Token-stream parsing: the built-in `Scanner` (a declarative maximal-munch
//! lexer) produces `LexToken`s that a grammar matches with `tok(...)` — fed
//! either via `ParseRequest::scan` (scan + parse in one call) or an explicit
//! `tokens(...)` stream.
//!
//! Run with: `cargo run --example tokens_and_scanner`

use caap_peg::{Grammar, LexToken, ParseRequest, Scanner};

fn main() {
    // ── 1. Build a scanner: ordered token rules + a trivia skip ─────────────
    let scanner = Scanner::new()
        .token("NUMBER", r"[0-9]+")
        .unwrap()
        .literal("PLUS", "+")
        .literal("STAR", "*")
        .skip(r"\s+")
        .unwrap();

    // Scan to a raw token stream (maximal munch; whitespace dropped).
    let toks = scanner.scan("12 + 3*45").expect("scans");
    let view: Vec<(&str, &str)> = toks
        .iter()
        .map(|t| (t.kind.as_str(), t.text.as_str()))
        .collect();
    println!("scanner.scan   -> {view:?}");

    // ── 2. Parse a tok()-grammar by attaching the scanner ───────────────────
    let g =
        Grammar::trusted_new("sum <- tok(NUMBER) (tok(PLUS) tok(NUMBER))*").with_start_rule("sum");
    let value = ParseRequest::new(&g)
        .scan(&scanner)
        .run("1 + 22 + 333")
        .expect("parses");
    println!("ParseRequest::scan -> {value:?}");

    // ── 3. Or supply a pre-produced token stream directly ───────────────────
    // (e.g. tokens from an external lexer). `tok(KIND, "text")` can also pin text.
    let g = Grammar::trusted_new("start <- tok(NAME) tok(EQ) tok(NUMBER)").with_start_rule("start");
    let tokens = vec![
        LexToken::new("NAME", "x", 0, 1),
        LexToken::new("EQ", "=", 1, 2),
        LexToken::new("NUMBER", "42", 2, 4),
    ];
    let value = ParseRequest::new(&g)
        .tokens(tokens)
        .run("x=42")
        .expect("parses");
    println!("ParseRequest::tokens -> {value:?}");

    // ── 4. A scan failure surfaces as the parse error ───────────────────────
    let err = ParseRequest::new(&g)
        .scan(&scanner)
        .run("12 @ 3")
        .unwrap_err();
    println!("scan failure   -> {}", err.message);
}
