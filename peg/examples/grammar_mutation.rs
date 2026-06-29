//! Changing the grammar *during a session* — a grammar is an ordinary value, so
//! it can grow new rules between parses. This is the engine behind a REPL that
//! learns new syntax, a notebook that imports a DSL extension mid-document, or a
//! language server that hot-reloads a `.peg` file on save.
//!
//! The flow:
//!   1. Parse against a minimal grammar (addition only).
//!   2. Snapshot it, then mutate in place: `replace_rule` + `add_rule`.
//!   3. `diff_grammars` reports exactly what changed (added/changed rules).
//!   4. Re-parse — input the old grammar rejected now succeeds.
//!   5. `seal` the grammar; further edits are refused (`GrammarSealed`).
//!
//! Run with: `cargo run --example grammar_mutation`

use caap_peg::mutation::{add_rule, diff_grammars, replace_rule, MutationError};
use caap_peg::{Grammar, ParseRequest};

fn try_parse(g: &Grammar, input: &str) {
    match ParseRequest::new(g).run(input) {
        Ok(_) => println!("    OK     {input}"),
        Err(e) => println!("    REJECT {input:<9} ({})", e.message),
    }
}

fn main() {
    // ── 1. A minimal calculator: a number, optionally followed by `+ number`. ──
    let mut g = Grammar::trusted_new(
        "calc <- num ('+' num)*\n\
         num  <- /[0-9]+/",
    )
    .with_start_rule("calc");

    println!("v1 — addition only:");
    try_parse(&g, "1 + 2 + 3"); // OK
    try_parse(&g, "7 - 4"); // REJECT — no subtraction
    try_parse(&g, "6 * 9"); // REJECT — no multiplication

    // ── 2. Grow the grammar at runtime. Snapshot the old version first so we can
    //       describe the delta. `Grammar` is a plain value, so this is just a
    //       clone. ─────────────────────────────────────────────────────────────
    let v1 = g.clone();

    // Replace `calc` to fold over an operator rule, and introduce that rule.
    replace_rule(&mut g, "calc", "num (op num)*").expect("calc replaced");
    add_rule(&mut g, "op", "'+' / '-' / '*' / '/'").expect("op added");

    // ── 3. What changed? ───────────────────────────────────────────────────────
    let diff = diff_grammars(&v1, &g);
    println!(
        "\nmutation diff: added={:?} changed={:?}",
        diff.added_rules, diff.changed_rules
    );

    // ── 4. The same inputs now parse against the evolved grammar. ───────────────
    println!("\nv2 — full four-function operators:");
    try_parse(&g, "1 + 2 + 3"); // still OK
    try_parse(&g, "7 - 4"); // now OK
    try_parse(&g, "6 * 9"); // now OK
    try_parse(&g, "8 / 2 + 1"); // now OK

    // ── 5. Freeze it. A sealed grammar refuses further edits — useful once a
    //       grammar is published/shared and must not drift underneath callers. ──
    g.seal();
    match add_rule(&mut g, "pow", "'**'") {
        Err(MutationError::GrammarSealed) => {
            println!("\nsealed: further edits rejected (GrammarSealed) ✔");
        }
        other => println!("\nunexpected: {other:?}"),
    }
}
