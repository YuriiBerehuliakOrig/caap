//! docs/builtins.md ↔ builtin registry bijection (remediation plan P8).
//!
//! The reference claims full coverage of the registered builtin surface; a
//! 2026-06 audit found 28 registered builtins missing from it, caught only by
//! a manual `comm` over extracted names. This test makes that check part of
//! `cargo test`, in both directions, exactly like the classification bijection
//! in `builtins/mod.rs`:
//!   - every registered builtin name must appear as a backtick token in the doc;
//!   - every name in a table's *first* cell must be a registered builtin;
//!   - the advertised total in the header must equal the registry size.

use std::collections::BTreeSet;

use caap_core::{Evaluator, IRGraph};

fn doc_text() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/builtins.md");
    std::fs::read_to_string(path).expect("docs/builtins.md must exist")
}

fn registered_names() -> BTreeSet<String> {
    Evaluator::new(IRGraph::new())
        .builtin_names()
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// All `` `token` `` spans in `text`.
fn backtick_tokens(text: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('`') else { break };
        let token = &rest[..end];
        if !token.is_empty() && !token.contains('\n') {
            tokens.insert(token.to_string());
        }
        rest = &rest[end + 1..];
    }
    tokens
}

#[test]
fn every_registered_builtin_is_documented() {
    let doc_tokens = backtick_tokens(&doc_text());
    let missing: Vec<_> = registered_names()
        .into_iter()
        .filter(|name| !doc_tokens.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "registered builtins missing from docs/builtins.md: {missing:?}"
    );
}

#[test]
fn every_documented_table_entry_is_registered() {
    let registered = registered_names();
    let mut ghosts: Vec<String> = Vec::new();
    for line in doc_text().lines() {
        // Table data rows whose first cell names builtins: "| `a` `b` | … |".
        let Some(rest) = line.strip_prefix("| `") else {
            continue;
        };
        let Some(first_cell) = rest.split('|').next() else {
            continue;
        };
        for token in backtick_tokens(&format!("`{first_cell}")) {
            if !registered.contains(&token) {
                ghosts.push(token);
            }
        }
    }
    assert!(
        ghosts.is_empty(),
        "doc table rows name unregistered builtins: {ghosts:?}"
    );
}

#[test]
fn advertised_total_matches_registry_size() {
    let text = doc_text();
    let advertised: usize = text
        .split("**")
        .nth(1)
        .and_then(|s| s.strip_suffix(" builtins"))
        .and_then(|s| s.parse().ok())
        .expect("header must advertise '**N builtins**'");
    let actual = registered_names().len();
    assert_eq!(
        advertised, actual,
        "docs/builtins.md header says {advertised} but the registry has {actual}"
    );
}
