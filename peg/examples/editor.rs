//! Editor primitives from a parsed tree — semantic tokens (highlighting),
//! folding ranges, selection ranges, and the document outline — the building
//! blocks an LSP/editor needs, driven by a grammar defined at runtime. Pair with
//! `ast_diff::changed_ranges` to recompute only the regions an edit touched.
//!
//! Run with: `cargo run --example editor`

use caap_peg::editor::{
    document_symbols, folding_ranges, selection_ranges, semantic_tokens, RuleKinds, Symbol,
    SymbolRule, SymbolRules,
};
use caap_peg::{parse_ast, Grammar};

fn main() {
    // A tiny assignment language. Newlines are whitespace under the default
    // trivia skipper, so statements need no explicit separator — a node simply
    // spans the lines its tokens cover.
    let g = Grammar::trusted_new(
        "doc    <- assign+\n\
         assign <- name '=' expr\n\
         expr   <- num (op num)*\n\
         name   <- /[a-z]+/\n\
         num    <- /[0-9]+/\n\
         op     <- '+' / '-'",
    )
    .with_start_rule("doc");

    let src = "x = 1 + 2\ny = 10 - 3";
    let tree = parse_ast(&g, src, None).expect("parses");

    // ── Semantic tokens (highlighting): map rules → token types ─────────────
    let kinds: RuleKinds = [("name", "variable"), ("num", "number"), ("op", "operator")]
        .iter()
        .map(|(r, k)| (r.to_string(), k.to_string()))
        .collect();
    println!("semantic tokens:");
    for t in semantic_tokens(&tree, &kinds, src) {
        println!(
            "  [{:>2}..{:<2}] {:<8} {:?}",
            t.span.start,
            t.span.end,
            t.kind,
            &src[t.span.start..t.span.end]
        );
    }

    // ── Folding ranges: multi-line nodes ────────────────────────────────────
    println!("\nfolding ranges (1-based lines):");
    for f in folding_ranges(&tree, src) {
        println!("  lines {}..{}", f.start_line, f.end_line);
    }

    // ── Selection ranges: expand-selection at a cursor ──────────────────────
    let cursor = 14; // inside "10" on line 2
    println!("\nselection ranges at byte {cursor} (innermost first):");
    for span in selection_ranges(&tree, cursor) {
        println!(
            "  [{:>2}..{:<2}] {:?}",
            span.start,
            span.end,
            &src[span.start..span.end.min(src.len())]
        );
    }

    // ── Document symbols / outline ───────────────────────────────────────────
    // A richer grammar with named, nestable definitions.
    let decls = Grammar::trusted_new(
        "file       <- item+\n\
         item       <- struct_def / fn_def\n\
         struct_def <- 'struct' name '{' field* '}'\n\
         fn_def     <- 'fn' name '(' ')'\n\
         field      <- name name\n\
         name       <- /[A-Za-z_][A-Za-z0-9_]*/",
    )
    .with_start_rule("file");
    let code = "struct Point { x int y int } fn main ( )";
    let tree = parse_ast(&decls, code, None).expect("parses");

    let sym = |kind: &str, name_rule: &str| SymbolRule {
        kind: kind.to_string(),
        name_rule: name_rule.to_string(),
    };
    let rules: SymbolRules = [
        ("struct_def".to_string(), sym("struct", "name")),
        ("fn_def".to_string(), sym("function", "name")),
        ("field".to_string(), sym("field", "name")),
    ]
    .into_iter()
    .collect();

    println!("\ndocument outline of {code:?}:");
    print_symbols(&document_symbols(&tree, code, &rules), 0);
}

fn print_symbols(syms: &[Symbol], depth: usize) {
    for s in syms {
        println!(
            "  {}{} {} [{}..{}]",
            "  ".repeat(depth),
            s.kind,
            s.name,
            s.span.start,
            s.span.end
        );
        print_symbols(&s.children, depth + 1);
    }
}
