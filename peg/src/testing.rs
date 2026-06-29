//! Test helpers for grammar authors.
//!
//! Small assertion helpers that panic with a clear message on failure, for use
//! in `#[test]`s that exercise a grammar:
//!
//! ```
//! use caap_peg::{Grammar, ParseValue};
//! use caap_peg::testing::{assert_parses, assert_rejects, assert_valid};
//!
//! let grammar = Grammar::trusted_new("root <- [a-z]+").with_start_rule("root");
//! assert_valid(&grammar);
//! assert_parses(&grammar, "abc");
//! assert_rejects(&grammar, "123");
//! ```

use crate::error::ParseError;
use crate::grammar::Grammar;
use crate::parser_engine::PEGParser;
use crate::types::{ParseValue, ParserConfig};
use crate::validation::{validate_grammar, ValidationReport};

/// Assert the grammar has no validation **errors** (warnings are allowed) and
/// return the full report. Panics listing the errors otherwise — the "lint"
/// gate for a grammar.
pub fn assert_valid(grammar: &Grammar) -> ValidationReport {
    let report = validate_grammar(grammar);
    let errors: Vec<String> = report
        .errors()
        .map(|issue| match &issue.code {
            Some(code) => format!("[{code}] {}", issue.message),
            None => issue.message.clone(),
        })
        .collect();
    assert!(
        errors.is_empty(),
        "grammar has {} validation error(s):\n  - {}",
        errors.len(),
        errors.join("\n  - ")
    );
    report
}

/// Assert `input` parses against `grammar`, returning the value. Panics with the
/// parse error (message + line/col when available) otherwise.
pub fn assert_parses(grammar: &Grammar, input: &str) -> ParseValue {
    PEGParser
        .parse(grammar, input, &ParserConfig::default())
        .unwrap_or_else(|err| {
            panic!(
                "expected {input:?} to parse, but it failed: {}",
                render(&err)
            )
        })
}

/// Assert `input` is **rejected** by `grammar`. Panics (showing the value) if it
/// unexpectedly parsed.
pub fn assert_rejects(grammar: &Grammar, input: &str) {
    if let Ok(value) = PEGParser.parse(grammar, input, &ParserConfig::default()) {
        panic!("expected {input:?} to be rejected, but it parsed to: {value:?}");
    }
}

/// Assert `input` parses to exactly `expected`.
pub fn assert_parses_to(grammar: &Grammar, input: &str, expected: &ParseValue) {
    let value = assert_parses(grammar, input);
    assert_eq!(
        &value, expected,
        "parse of {input:?} did not match expected"
    );
}

fn render(err: &ParseError) -> String {
    match (err.line, err.col) {
        (Some(line), Some(col)) => format!("{} (at {line}:{col})", err.message),
        _ => format!("{} (at byte {})", err.message, err.span.start),
    }
}
