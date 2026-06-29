//! Fuzz target: parse arbitrary input against a fixed, representative grammar.
//! The contract is panic-safety — every byte string yields `Ok` or `Err`, never
//! an abort.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    let grammar = caap_peg::Grammar::trusted_new(
        "doc   <- item+\n\
         item  <- num / word / group / sym\n\
         group <- '(' item* ')'\n\
         num   <- /[0-9]+/\n\
         word  <- /[a-z]+/\n\
         sym   <- [+*/-]",
    )
    .with_start_rule("doc");

    // Whole-input parse and the error-tolerant AST builder: neither may panic.
    let _ = caap_peg::parse(data, &grammar);
    let _ = caap_peg::parse_ast_tolerant(&grammar, data, None);
});
