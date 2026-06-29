//! Randomised panic-safety: every entry point that consumes untrusted input must
//! return `Ok`/`Err` (or a best-effort tree), never panic, for *arbitrary* bytes.
//!
//! Runs on stable in `cargo test`. The `fuzz/` crate carries the same idea as a
//! coverage-guided libFuzzer target for deeper, longer campaigns.

use caap_peg as peg;
use proptest::prelude::*;

/// A representative grammar exercising many terminal/combinator kinds, so fuzzed
/// input drives a broad slice of the engine.
fn sample_grammar() -> peg::Grammar {
    peg::Grammar::trusted_new(
        "doc   <- item+\n\
         item  <- num / word / group / sym\n\
         group <- '(' item* ')'\n\
         num   <- /[0-9]+/\n\
         word  <- /[a-z]+/\n\
         sym   <- [+*/-]",
    )
    .with_start_rule("doc")
}

proptest! {
    // Parsing arbitrary input against a fixed grammar never panics.
    #[test]
    fn parsing_arbitrary_input_never_panics(input in ".{0,256}") {
        let grammar = sample_grammar();
        let _ = peg::parse(&input, &grammar); // Ok or Err — must not panic.
    }

    // Deeply-nested input never overflows the stack: the recursion guard turns it
    // into a recoverable error. (A stack overflow would abort the test process, so
    // this also pins the guard against regression.)
    #[test]
    fn deep_nesting_never_overflows(depth in 0usize..20_000) {
        let g = peg::Grammar::trusted_new("e <- '(' e ')' / 'x'").with_start_rule("e");
        let s = format!("{}x{}", "(".repeat(depth), ")".repeat(depth));
        let cfg = peg::ParserConfig::default().with_max_steps(s.len().saturating_mul(8) + 1024);
        let _ = peg::ParseRequest::new(&g).config(cfg).run(&s); // Ok or recursion_limit Err.
    }

    // The error-tolerant AST builder always yields a tree, never panics, and its
    // root span never exceeds the input length.
    #[test]
    fn tolerant_ast_is_total(input in ".{0,256}") {
        let grammar = sample_grammar();
        let tree = peg::parse_ast_tolerant(&grammar, &input, None);
        prop_assert!(tree.span.end <= input.len());
    }

    // Compiling arbitrary text as a grammar never panics (it is the fallible
    // `try_new`, so it returns Err on nonsense rather than aborting).
    #[test]
    fn compiling_arbitrary_grammar_source_never_panics(src in ".{0,128}") {
        let _ = peg::Grammar::try_new(&src);
    }

    // The built-in scanner never panics on arbitrary input (Err on an
    // untokenisable byte, never an abort).
    #[test]
    fn scanning_arbitrary_input_never_panics(input in ".{0,256}") {
        let scanner = peg::Scanner::new()
            .token("NUM", r"[0-9]+")
            .unwrap()
            .token("WORD", r"[a-z]+")
            .unwrap()
            .skip(r"\s+")
            .unwrap();
        let _ = scanner.scan(&input);
    }
}
