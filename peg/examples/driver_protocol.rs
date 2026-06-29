//! The Parse Effects Protocol — the host control surface that backs `@action`,
//! `@?pred`, `@!guard`, plus global observation/control. Built fluently with
//! `ParseDriverBuilder` and attached via `ParseRequest::driver`.
//!
//! Run with: `cargo run --example driver_protocol`

use std::cell::RefCell;
use std::rc::Rc;

use caap_peg::{Directive, Grammar, ParseDriverBuilder, ParseEffect, ParseRequest, ParseValue};

fn main() {
    // ── 1. @action — transform the matched value ────────────────────────────
    let g = Grammar::trusted_new("start <- @upper(/[a-z]+/)").with_start_rule("start");
    let driver = ParseDriverBuilder::new()
        .action("upper", |value, _view| match value {
            ParseValue::Text(s) => ParseValue::Text(s.to_uppercase().into()),
            other => other,
        })
        .build();
    let v = ParseRequest::new(&g).driver(&driver).run("hello").unwrap();
    println!("@action        -> {v:?}");

    // ── 2. @?pred — accept/reject by a host predicate ───────────────────────
    let g = Grammar::trusted_new("start <- @?even /[0-9]+/").with_start_rule("start");
    let driver = ParseDriverBuilder::new()
        // Bare `@?name` predicates run before the value; here we just gate on a flag.
        .predicate("even", |_view| true)
        .build();
    let v = ParseRequest::new(&g).driver(&driver).run("42");
    println!("@?pred         -> {:?}", v.map(|x| format!("{x:?}")));

    // ── 3. @!guard — match, then let the host accept/reject ─────────────────
    let g = Grammar::trusted_new("start <- @!short(/[a-z]+/)").with_start_rule("start");
    let driver = ParseDriverBuilder::new()
        .guard("short", |value, _view| match value {
            // Reject identifiers longer than 3 chars; the choice backtracks.
            ParseValue::Text(s) if s.len() > 3 => Directive::Reject,
            _ => Directive::Proceed,
        })
        .build();
    println!(
        "@!guard        -> 'abc' ok={}, 'abcd' ok={}",
        ParseRequest::new(&g).driver(&driver).run("abc").is_ok(),
        ParseRequest::new(&g).driver(&driver).run("abcd").is_ok(),
    );

    // ── 4. accept_if — the boolean-guard convenience ────────────────────────
    let g = Grammar::trusted_new("start <- @!nonempty(/[a-z]*/)").with_start_rule("start");
    let driver = ParseDriverBuilder::new()
        .accept_if(
            "nonempty",
            |value, _| matches!(value, ParseValue::Text(s) if !s.is_empty()),
        )
        .build();
    println!(
        "accept_if      -> 'x' ok={}",
        ParseRequest::new(&g).driver(&driver).run("x").is_ok()
    );

    // ── 5. on_event — observe every effect (tracing) ────────────────────────
    let g = Grammar::trusted_new("doc <- word+\nword <- /[a-z]+/").with_start_rule("doc");
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let log_cb = Rc::clone(&log);
    let driver = ParseDriverBuilder::new()
        .on_event(move |effect, _view| {
            if let ParseEffect::RuleEnter { rule, .. } = effect {
                log_cb.borrow_mut().push(rule.to_string());
            }
        })
        .build();
    let _ = ParseRequest::new(&g).driver(&driver).run("ab cd");
    println!("on_event       -> rules entered: {:?}", log.borrow());

    // ── 6. intercept — global control over any effect ───────────────────────
    // Reject the first alternative of a choice whenever it matched the literal "a".
    let g = Grammar::trusted_new("start <- 'a' / 'b'").with_start_rule("start");
    let driver = ParseDriverBuilder::new()
        .intercept(|effect, _view| match effect {
            ParseEffect::AltMatched {
                value: ParseValue::Text(s),
                ..
            } if &**s == "a" => Directive::Reject,
            _ => Directive::Proceed,
        })
        .build();
    let v = ParseRequest::new(&g).driver(&driver).run("a");
    println!("intercept      -> 'a' forced to fail: ok={}", v.is_ok());

    // ── 7. with_auto_scope — generated @?in_<rule> / @?not_in_<rule> ─────────
    // `@?in_block` succeeds only when `block` is on the rule stack.
    let g = Grammar::trusted_new(
        "prog  <- item+\n\
         item  <- block / loose\n\
         block <- '{' inner* '}'\n\
         inner <- @?in_block 'x'\n\
         loose <- @?not_in_block 'y'",
    )
    .with_start_rule("prog");
    let driver = ParseDriverBuilder::new().with_auto_scope().build();
    println!(
        "with_auto_scope-> '{{x}}y' ok={}",
        ParseRequest::new(&g).driver(&driver).run("{x}y").is_ok()
    );
}
