//! Editor primitives derived from a parsed [`AstNode`] — the building blocks an
//! LSP/editor consumes: **semantic tokens** (highlighting), **folding ranges**,
//! and **selection ranges** (expand-selection). These are pure functions of the
//! syntax tree, so they pair naturally with incremental parsing: after an edit,
//! re-derive them only over the regions [`crate::ast_diff::changed_ranges`]
//! reports as changed.
//!
//! This is the same value proposition as a tree-sitter highlighting/structure
//! layer, but driven by a grammar defined at runtime.
//!
//! ```
//! use caap_peg::{parse_ast, Grammar};
//! use caap_peg::editor::{semantic_tokens, RuleKinds};
//!
//! let g = Grammar::trusted_new("sum <- num (op num)*\nnum <- /[0-9]+/\nop <- '+' / '-'")
//!     .with_start_rule("sum");
//! let tree = parse_ast(&g, "1+2", None).unwrap();
//! let kinds: RuleKinds = [("num", "number"), ("op", "operator")]
//!     .iter().map(|(r, k)| (r.to_string(), k.to_string())).collect();
//! let tokens = semantic_tokens(&tree, &kinds, "1+2");
//! assert_eq!(tokens.len(), 3); // 1 + 2  ->  number operator number
//! ```

use std::collections::HashMap;

use crate::ast::{AstNode, AstSpan, Source};

// ── Semantic tokens ─────────────────────────────────────────────────────────

/// A classified span for syntax highlighting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticToken {
    /// The byte span the token covers.
    pub span: AstSpan,
    /// The token type (the value from the [`RuleKinds`] map).
    pub kind: String,
}

/// Maps AST rule names to token-type strings (e.g. `"num" -> "number"`).
pub type RuleKinds = HashMap<String, String>;

/// Non-overlapping, start-sorted semantic tokens. The **outermost** mapped node
/// claims its whole span (and suppresses any mapped descendants), so mapping a
/// `string` rule highlights the entire string as one token rather than its
/// inner pieces. Unmapped nodes are transparent — recursion descends through
/// them to find mapped subtrees.
///
/// Each token span is **trimmed** of leading/trailing whitespace against
/// `source`, because a rule's AST span absorbs the trivia skipped at its edges
/// and highlighting should not colour that whitespace. A span that is entirely
/// whitespace yields no token.
pub fn semantic_tokens(root: &AstNode, kinds: &RuleKinds, source: &str) -> Vec<SemanticToken> {
    let mut out = Vec::new();
    collect_tokens(root, kinds, source, &mut out);
    out
}

fn collect_tokens(node: &AstNode, kinds: &RuleKinds, source: &str, out: &mut Vec<SemanticToken>) {
    if let Some(kind) = kinds.get(&node.rule) {
        if let Some(span) = trim_span(&node.span, source) {
            out.push(SemanticToken {
                span,
                kind: kind.clone(),
            });
        }
        return; // claim this region; do not descend into a mapped node
    }
    for child in node.children.iter() {
        collect_tokens(child, kinds, source, out);
    }
}

/// Shrink a span to its non-whitespace content (rule spans include the trivia
/// skipped at their edges). `None` if the span is empty or all whitespace.
fn trim_span(span: &AstSpan, source: &str) -> Option<AstSpan> {
    let bytes = source.as_bytes();
    let mut start = span.start.min(source.len());
    let mut end = span.end.min(source.len());
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    (start < end).then(|| AstSpan::new(start, end))
}

// ── Folding ranges ────────────────────────────────────────────────────────

/// A foldable region, as inclusive **1-based** line numbers (matching
/// [`Source::line_col`]). Convert to your editor's 0-based convention at the
/// boundary.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct FoldRange {
    /// First line of the region.
    pub start_line: usize,
    /// Last line of the region.
    pub end_line: usize,
}

/// Folding ranges for every node that spans more than one line, deduplicated and
/// sorted (outermost/earliest first). `source` is the text the tree was parsed
/// from (needed to map byte spans to lines).
pub fn folding_ranges(root: &AstNode, source: &str) -> Vec<FoldRange> {
    let src = Source::new(source);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let (start_line, _) = src.line_col(node.span.start);
        let (end_line, _) = src.line_col(node.span.end.min(source.len()));
        if end_line > start_line {
            let range = FoldRange {
                start_line,
                end_line,
            };
            if seen.insert(range.clone()) {
                out.push(range);
            }
        }
        for child in node.children.iter() {
            stack.push(child);
        }
    }
    out.sort_by_key(|r| (r.start_line, std::cmp::Reverse(r.end_line)));
    out
}

// ── Selection ranges ────────────────────────────────────────────────────────

/// The chain of node spans containing byte position `pos`, **innermost first**
/// (each contained in the next). This is the LSP "selection range" /
/// expand-selection primitive: the first entry is the tightest node under the
/// cursor, the last is the whole document. Empty if `pos` is outside the tree.
pub fn selection_ranges(root: &AstNode, pos: usize) -> Vec<AstSpan> {
    let mut chain = Vec::new();
    let mut node = root;
    loop {
        if contains(&node.span, pos) {
            chain.push(node.span.clone());
        }
        match node.children.iter().find(|c| contains(&c.span, pos)) {
            Some(child) => node = child,
            None => break,
        }
    }
    chain.reverse(); // innermost first
    chain
}

fn contains(span: &AstSpan, pos: usize) -> bool {
    span.start <= pos && pos < span.end
}

// ── Document symbols (outline) ──────────────────────────────────────────────

/// A node in the document outline / symbol tree (the LSP `documentSymbol`
/// primitive): a named, kinded region with nested child symbols.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Symbol {
    /// The symbol's name (extracted from a designated child rule; `""` if none).
    pub name: String,
    /// The symbol kind (the `kind` from its [`SymbolRule`], e.g. `"function"`).
    pub kind: String,
    /// The full byte span of the symbol's definition.
    pub span: AstSpan,
    /// Symbols nested inside this one (e.g. fields of a struct, methods of a class).
    pub children: Vec<Symbol>,
}

/// How a rule becomes a [`Symbol`]: the kind to assign and the child rule whose
/// matched text names it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolRule {
    /// The symbol kind to emit (e.g. `"struct"`, `"function"`, `"field"`).
    pub kind: String,
    /// The descendant rule whose (trimmed) text gives the symbol's name. The
    /// search does not cross into a nested symbol, so each symbol gets its own
    /// nearest name.
    pub name_rule: String,
}

/// Maps AST rule names to their [`SymbolRule`].
pub type SymbolRules = HashMap<String, SymbolRule>;

/// Build the document outline: a tree of [`Symbol`]s for every AST node whose
/// rule is a symbol rule, in source order, with symbols found inside a symbol
/// nested as its children. Transparent (non-symbol) nodes are descended through.
pub fn document_symbols(root: &AstNode, source: &str, rules: &SymbolRules) -> Vec<Symbol> {
    let mut out = Vec::new();
    gather_symbols(root, source, rules, &mut out);
    out
}

fn gather_symbols(node: &AstNode, source: &str, rules: &SymbolRules, out: &mut Vec<Symbol>) {
    if let Some(spec) = rules.get(&node.rule) {
        let mut children = Vec::new();
        for child in node.children.iter() {
            gather_symbols(child, source, rules, &mut children);
        }
        out.push(Symbol {
            name: symbol_name(node, &spec.name_rule, rules, source).unwrap_or_default(),
            kind: spec.kind.clone(),
            span: node.span.clone(),
            children,
        });
    } else {
        for child in node.children.iter() {
            gather_symbols(child, source, rules, out);
        }
    }
}

/// First descendant of `node` whose rule is `name_rule`, without crossing into a
/// nested symbol (so a symbol takes its own name, not a child symbol's).
fn symbol_name(
    node: &AstNode,
    name_rule: &str,
    rules: &SymbolRules,
    source: &str,
) -> Option<String> {
    for child in node.children.iter() {
        if child.rule == name_rule {
            return trim_span(&child.span, source).map(|s| source[s.start..s.end].to_string());
        }
        if rules.contains_key(&child.rule) {
            continue; // a nested symbol names itself; don't steal its identifier
        }
        if let Some(name) = symbol_name(child, name_rule, rules, source) {
            return Some(name);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::parse_ast;
    use crate::grammar::Grammar;

    fn kinds(pairs: &[(&str, &str)]) -> RuleKinds {
        pairs
            .iter()
            .map(|(r, k)| (r.to_string(), k.to_string()))
            .collect()
    }

    #[test]
    fn semantic_tokens_classify_leaves() {
        let g = Grammar::trusted_new("sum <- num (op num)*\nnum <- /[0-9]+/\nop <- '+' / '-'")
            .with_start_rule("sum");
        let tree = parse_ast(&g, "1+2-3", None).unwrap();
        let toks = semantic_tokens(
            &tree,
            &kinds(&[("num", "number"), ("op", "operator")]),
            "1+2-3",
        );
        let view: Vec<&str> = toks.iter().map(|t| t.kind.as_str()).collect();
        assert_eq!(view, ["number", "operator", "number", "operator", "number"]);
        // Sorted, non-overlapping.
        assert!(toks.windows(2).all(|w| w[0].span.end <= w[1].span.start));
    }

    #[test]
    fn outermost_mapped_node_claims_its_span() {
        // Mapping `item` (interior) suppresses its mapped `num` descendants.
        let g = Grammar::trusted_new("doc <- item+\nitem <- num num\nnum <- /[0-9]/")
            .with_start_rule("doc");
        let tree = parse_ast(&g, "12", None).unwrap();
        let toks = semantic_tokens(&tree, &kinds(&[("item", "pair"), ("num", "number")]), "12");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, "pair");
        assert_eq!(toks[0].span, AstSpan::new(0, 2));
    }

    #[test]
    fn folding_ranges_cover_multiline_nodes() {
        let g = Grammar::trusted_new("doc <- line+\nline <- /[a-z]+/ nl?\nnl <- /\\n/")
            .with_start_rule("doc");
        let src = "aaa\nbbb\nccc";
        let tree = parse_ast(&g, src, None).unwrap();
        let folds = folding_ranges(&tree, src);
        // The whole `doc` spans lines 1..3.
        assert!(folds.iter().any(|f| f.start_line == 1 && f.end_line == 3));
    }

    #[test]
    fn selection_ranges_expand_from_cursor() {
        let g = Grammar::trusted_new("sum <- num (op num)*\nnum <- /[0-9]+/\nop <- '+' / '-'")
            .with_start_rule("sum");
        let tree = parse_ast(&g, "10+20", None).unwrap();
        let chain = selection_ranges(&tree, 1); // inside the first "10"
        assert!(!chain.is_empty());
        // Innermost first, each strictly within the next.
        assert!(chain
            .windows(2)
            .all(|w| { w[1].start <= w[0].start && w[0].end <= w[1].end }));
        // Last is the whole input.
        assert_eq!(chain.last().unwrap(), &AstSpan::new(0, 5));
    }

    #[test]
    fn selection_ranges_empty_outside_tree() {
        let g = Grammar::trusted_new("w <- /[a-z]+/").with_start_rule("w");
        let tree = parse_ast(&g, "abc", None).unwrap();
        assert!(selection_ranges(&tree, 99).is_empty());
    }

    #[test]
    fn document_symbols_build_a_nested_outline() {
        let g = Grammar::trusted_new(
            "file       <- item+\n\
             item       <- struct_def / fn_def\n\
             struct_def <- 'struct' name '{' field* '}'\n\
             fn_def     <- 'fn' name '(' ')'\n\
             field      <- name name\n\
             name       <- /[A-Za-z_][A-Za-z0-9_]*/",
        )
        .with_start_rule("file");
        let src = "struct Point { x int y int } fn main ( )";
        let tree = parse_ast(&g, src, None).unwrap();

        let rule = |kind: &str, name_rule: &str| SymbolRule {
            kind: kind.to_string(),
            name_rule: name_rule.to_string(),
        };
        let rules: SymbolRules = [
            ("struct_def".to_string(), rule("struct", "name")),
            ("fn_def".to_string(), rule("function", "name")),
            ("field".to_string(), rule("field", "name")),
        ]
        .into_iter()
        .collect();

        let syms = document_symbols(&tree, src, &rules);
        assert_eq!(syms.len(), 2);

        assert_eq!(syms[0].kind, "struct");
        assert_eq!(syms[0].name, "Point"); // not "x" — nested fields don't steal it
        let fields: Vec<&str> = syms[0].children.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(fields, ["x", "y"]); // field names, in order
        assert!(syms[0].children.iter().all(|s| s.kind == "field"));

        assert_eq!(syms[1].kind, "function");
        assert_eq!(syms[1].name, "main");
        assert!(syms[1].children.is_empty());
    }
}
