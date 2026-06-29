//! A guided tour of the core entry points: grammar construction (text + the
//! `GrammarBuilder` DSL), the `ParseRequest` terminals (`run`, `spans`,
//! `run_output`/`.ast()`, `run_prefix`, `run_profiled`), `ParseValue`
//! inspection, and typed extraction.
//!
//! Run with: `cargo run --example tour`

use caap_peg::builder::{char_class, choice, lit, plus, rule_ref, seq, GrammarBuilder};
use caap_peg::{Grammar, ParseOutput, ParseRequest, ParseValue, ParserConfig};

fn main() {
    // ── 1. Build a grammar from PEG text ────────────────────────────────────
    let g = Grammar::trusted_new("sum <- /[0-9]+/ (('+' / '-') /[0-9]+/)*").with_start_rule("sum");

    // ── 2. The one-liner: caap_peg::parse / ParseRequest::run ───────────────
    let value = ParseRequest::new(&g).run("1+2-3").expect("parses");
    println!("run            -> {value:?}");

    // ── 3. Span-wrapped result ──────────────────────────────────────────────
    let spanned = ParseRequest::new(&g).spans().run("1+2").expect("parses");
    println!("spans          -> is_spanned={}", spanned.is_spanned());

    // ── 4. AST output mode (a concrete syntax tree) ─────────────────────────
    let out = ParseRequest::new(&g)
        .ast()
        .run_output("1+2")
        .expect("parses");
    if let ParseOutput::Ast(node) = out {
        println!(
            "ast            -> root '{}' span [{},{}], {} children",
            node.rule,
            node.span.start,
            node.span.end,
            node.children.len()
        );
    }

    // ── 5. Prefix parse: consume only a leading slice ───────────────────────
    let word = Grammar::trusted_new("w <- /[a-z]+/").with_start_rule("w");
    let prefix = ParseRequest::new(&word).run_prefix("abc 123", 0);
    println!(
        "run_prefix     -> consumed={} eof={} ok={}",
        prefix.consumed,
        prefix.eof,
        prefix.ok()
    );

    // ── 6. Profiled parse: per-rule call / memo stats ───────────────────────
    let (_v, profile) = ParseRequest::new(&g)
        .run_profiled("1+2+3+4")
        .expect("parses");
    println!(
        "run_profiled   -> total_calls={} memo_hit_rate={:.0}% hottest={:?}",
        profile.total_calls(),
        profile.memo_hit_rate() * 100.0,
        profile.hottest(1).first().map(|(rule, _)| *rule)
    );

    // ── 7. Custom config (max_steps, memo off, …) ───────────────────────────
    let cfg = ParserConfig::default()
        .with_memo(false)
        .with_max_steps(1024);
    let _ = ParseRequest::new(&g)
        .config(cfg)
        .run("1+2")
        .expect("parses");
    println!("config         -> memo off + max_steps=1024 OK");

    // ── 8. Inspect a ParseValue (text / node / field) ───────────────────────
    let bound = Grammar::trusted_new("kv <- key:/[a-z]+/ '=' val:/[0-9]+/").with_start_rule("kv");
    let v = ParseRequest::new(&bound).run("x=42").expect("parses");
    if let Some(key) = v.field("key").and_then(ParseValue::text) {
        println!("field          -> key = {key:?}");
    }
    // Typed extraction: parse the `val` binding straight into an i64.
    let n: i64 = v.parse_field("val").expect("val is an integer");
    println!("parse_field    -> val = {n}");

    // ── 9. The GrammarBuilder DSL (programmatic, no text) ───────────────────
    let built = GrammarBuilder::new()
        .start("expr")
        .rule(
            "expr",
            choice(vec![
                seq(vec![lit("("), rule_ref("expr"), lit(")")]),
                plus(char_class("0-9").unwrap()),
            ]),
        )
        .build();
    let v = ParseRequest::new(&built).run("(((42)))").expect("parses");
    println!(
        "builder DSL    -> {:?}",
        matches!(v, ParseValue::Node(..) | ParseValue::Text(_))
    );
}
