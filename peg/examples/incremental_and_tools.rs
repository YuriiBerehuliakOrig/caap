//! Editor-grade & tooling capabilities: incremental reparse with a `ParseCache`,
//! edit sequencing, the AST + structural diff (`changed_ranges` /
//! `reparse_ast_incremental`), cross-grammar imports via a `GrammarRegistry`,
//! static analysis / validation / mutation / diff, grammar signatures, and the
//! JSON `SpecCompiler`.
//!
//! Run with: `cargo run --example incremental_and_tools`

use caap_peg::ast_diff::{changed_ranges, reparse_ast_incremental, AstEdit};
use caap_peg::{
    add_rule, analyze_grammar, apply_edits, diff_grammars, grammar_signature, parse_ast,
    parse_ast_tolerant, snapshot_edits_to_sequential, validate_grammar, walk_ast, Grammar,
    GrammarRegistry, IncrementalEdit, ParseCache, ParseRequest, SpecCompiler,
};
use std::sync::Arc;

fn doc_grammar() -> Grammar {
    Grammar::trusted_new("doc <- item+\nitem <- [a-z]+ \".\"").with_start_rule("doc")
}

fn main() {
    // ── 1. Incremental parse: reuse a cache across re-parses ────────────────
    let g = doc_grammar();
    let mut cache = ParseCache::default();
    let _ = ParseRequest::new(&g)
        .run_incremental("ab.cd.", &mut cache)
        .unwrap();
    let again = ParseRequest::new(&g)
        .run_incremental("ab.cd.", &mut cache)
        .unwrap();
    println!(
        "incremental    -> cache entries={}, shared Arc result={}",
        cache.entries.len(),
        Arc::strong_count(&again) > 1
    );

    // ── 2. Sequence + apply text edits ──────────────────────────────────────
    let edits = vec![
        IncrementalEdit::new(0, 2, "XY").unwrap(), // replace "ab" -> "XY"
        IncrementalEdit::new(3, 5, "Z").unwrap(),  // replace "cd" -> "Z"
    ];
    let sequential = snapshot_edits_to_sequential("ab.cd.", &edits).unwrap();
    let rebuilt = apply_edits("ab.cd.", &sequential).unwrap();
    println!("apply_edits    -> {rebuilt:?}");

    // ── 3. AST tree + tolerant parse ────────────────────────────────────────
    let tree = parse_ast(&g, "ab.cd.", None).unwrap();
    println!(
        "parse_ast      -> root '{}', {} nodes",
        tree.rule,
        walk_ast(&tree).count()
    );
    let tolerant = parse_ast_tolerant(&g, "ab.!!", None); // invalid tail
    println!("tolerant       -> has_errors={}", tolerant.has_errors());

    // ── 4. Structural diff + physical subtree reuse ─────────────────────────
    let old = parse_ast(&g, "ab.cd.", None).unwrap();
    let new = parse_ast(&g, "ab.ce.", None).unwrap(); // 'd' -> 'e' at byte 4
    let ranges = changed_ranges(&old, "ab.cd.", &new, "ab.ce.", &AstEdit::new(4, 5, 5));
    println!("changed_ranges -> {} changed region(s)", ranges.len());
    let merged = reparse_ast_incremental(&old, "ab.cd.", new, "ab.ce.", &AstEdit::new(4, 5, 5));
    let shared = Arc::ptr_eq(&old.children[0].children, &merged.children[0].children);
    println!("reparse reuse  -> first (untouched) item physically shared = {shared}");

    // ── 5. Cross-grammar imports via a registry ─────────────────────────────
    let mut registry = GrammarRegistry::new();
    registry
        .register(
            "ident",
            Grammar::trusted_new("rule <- /[a-z]+/").with_start_rule("rule"),
        )
        .unwrap();
    let main = Grammar::trusted_new("start <- ident::rule").with_start_rule("start");
    let v = ParseRequest::new(&main)
        .registry(&registry)
        .run("hello")
        .unwrap();
    println!("registry       -> imported rule matched: {v:?}");

    // ── 6. Static analysis ──────────────────────────────────────────────────
    let analysis = analyze_grammar(&g);
    println!(
        "analyze        -> {} rules, {} reachable, {} errors",
        analysis.rule_count,
        analysis.reachable.len(),
        analysis.errors.len()
    );

    // ── 7. Validation report (errors vs warnings, with codes) ───────────────
    let bad = Grammar::trusted_new("root <- missing_rule").with_start_rule("root");
    let report = validate_grammar(&bad);
    println!(
        "validate       -> ok={}, codes={:?}",
        report.ok(),
        report
            .errors()
            .filter_map(|i| i.code.clone())
            .collect::<Vec<_>>()
    );

    // ── 8. Mutation + diff + signature ──────────────────────────────────────
    let base = Grammar::trusted_new("root <- 'x'").with_start_rule("root");
    let mut target = base.clone();
    add_rule(&mut target, "extra", "'y'").unwrap();
    let diff = diff_grammars(&base, &target);
    println!("diff_grammars  -> added={:?}", diff.added_rules);
    println!(
        "signature      -> base != target: {}",
        grammar_signature(&base) != grammar_signature(&target)
    );

    // ── 9. Build a grammar from a JSON spec ─────────────────────────────────
    let spec = serde_json::json!([
        "grammar",
        "greeting",
        "root",
        [[
            "rule",
            "root",
            ["seq", [["lit", "hi "], ["regex", "[a-z]+"]]]
        ]]
    ]);
    let g = SpecCompiler::new().compile(&spec).expect("spec compiles");
    let v = ParseRequest::new(&g).run("hi there").unwrap();
    println!("SpecCompiler   -> {v:?}");
}
