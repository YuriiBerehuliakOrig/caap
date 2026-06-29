//! [`AstNode`] — the concrete syntax tree built from a parse, plus [`AstSpan`]
//! byte ranges, the [`Source`] text helper, and the `parse_ast` / tolerant
//! tree builders.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ── AstSpan ────────────────────────────────────────────────────────────────

/// Byte-offset span in the source text `[start, end)`.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct AstSpan {
    /// Inclusive start byte offset.
    pub start: usize,
    /// Exclusive end byte offset.
    pub end: usize,
}

impl AstSpan {
    /// A span over the byte range `[start, end)`.
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Length in bytes (saturating, so a reversed span reads as `0`).
    pub fn length(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Whether the span covers no bytes.
    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }
}

// ── Source ─────────────────────────────────────────────────────────────────

/// Source text with optional name and pre-computed line offset table.
///
/// `line_offsets[i]` is the byte offset of the start of line `i + 1`.
/// The first entry is always `0`.
#[derive(Clone, Debug)]
pub struct Source {
    /// The source text.
    pub text: String,
    /// Optional source name (e.g. a file path) for diagnostics.
    pub name: Option<String>,
    line_offsets: Vec<usize>,
}

impl Source {
    /// Build a `Source` from text, precomputing its line-offset table.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let line_offsets = Self::compute_offsets(&text);
        Self {
            text,
            name: None,
            line_offsets,
        }
    }

    /// Attach a source name (e.g. a file path) for diagnostics.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    fn compute_offsets(text: &str) -> Vec<usize> {
        let mut offsets = vec![0usize];
        for (i, ch) in text.char_indices() {
            if ch == '\n' {
                offsets.push(i + 1);
            }
        }
        offsets
    }

    /// Return `(line, col)` for a byte position (both 1-based).
    ///
    /// Uses binary search over the pre-computed `line_offsets`.
    pub fn line_col(&self, pos: usize) -> (usize, usize) {
        // partition_point gives the first index where offset > pos
        let idx = self.line_offsets.partition_point(|&offset| offset <= pos);
        let line_idx = if idx > 0 { idx - 1 } else { 0 };
        let col = pos - self.line_offsets[line_idx] + 1;
        (line_idx + 1, col)
    }

    /// Slice the source text using a span.
    pub fn slice(&self, span: &AstSpan) -> &str {
        let end = span.end.min(self.text.len());
        &self.text[span.start..end]
    }
}

// ── AstCapture ─────────────────────────────────────────────────────────────

/// A named binding captured during a parse rule match.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AstCapture {
    /// The capture label (the `name` in a `capture("name", …)`).
    pub label: String,
    /// The captured subtree.
    pub node: Box<AstNode>,
}

impl AstCapture {
    /// Build a capture binding `label` to `node`.
    pub fn new(label: impl Into<String>, node: AstNode) -> Self {
        Self {
            label: label.into(),
            node: Box::new(node),
        }
    }
}

// ── AstNode ────────────────────────────────────────────────────────────────

/// A single node in the concrete syntax tree produced by the PEG parser.
///
/// Source access is intentionally separated from the node: call
/// `source.slice(&node.span)` or `node.text_from(&source)` to retrieve text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AstNode {
    /// The rule (or construct) name this node was produced by.
    pub rule: String,
    /// The byte span this node covers.
    pub span: AstSpan,
    /// Children are held behind an [`Arc`] so structurally identical subtrees can
    /// be physically shared between successive parses (see
    /// [`crate::ast_diff::reparse_ast_incremental`]). Clone is cheap; mutation
    /// goes through [`Arc::make_mut`]-style copy-on-write (`to_vec` + `into`).
    pub children: Arc<[AstNode]>,
    /// Named captures attached to this node.
    pub captures: Vec<AstCapture>,
    /// The semantic action name attached by the grammar, if any.
    pub action: String,
    /// `true` for synthetic error nodes produced by [`parse_ast_tolerant`] over
    /// input the grammar could not match. Always `false` for matched rules.
    #[serde(default)]
    pub error: bool,
}

/// The reserved rule name used for synthetic error nodes.
pub const ERROR_RULE: &str = "<error>";

impl AstNode {
    /// A leaf node for `rule` over `span` (no children, captures, or action).
    pub fn new(rule: impl Into<String>, span: AstSpan) -> Self {
        Self {
            rule: rule.into(),
            span,
            children: Arc::from([]),
            captures: Vec::new(),
            action: String::new(),
            error: false,
        }
    }

    /// A synthetic error node covering `[start, end)` (rule [`ERROR_RULE`]).
    pub fn error(start: usize, end: usize) -> Self {
        Self {
            rule: ERROR_RULE.to_string(),
            span: AstSpan::new(start, end),
            children: Arc::from([]),
            captures: Vec::new(),
            action: String::new(),
            error: true,
        }
    }

    /// Whether any node in this subtree is an error node.
    pub fn has_errors(&self) -> bool {
        walk(self).any(|n| n.error)
    }

    /// Builder: set the node's children.
    pub fn with_children(mut self, children: Vec<AstNode>) -> Self {
        self.children = children.into();
        self
    }

    /// Builder: set the node's captures.
    pub fn with_captures(mut self, captures: Vec<AstCapture>) -> Self {
        self.captures = captures;
        self
    }

    /// Builder: set the node's semantic-action name.
    pub fn with_action(mut self, action: impl Into<String>) -> Self {
        self.action = action.into();
        self
    }

    /// Retrieve the source text for this node's span.
    pub fn text_from<'s>(&self, source: &'s Source) -> &'s str {
        source.slice(&self.span)
    }
}

/// Depth-first pre-order walk of an AST tree.
pub fn walk(node: &AstNode) -> impl Iterator<Item = &AstNode> {
    let mut stack: Vec<&AstNode> = vec![node];
    std::iter::from_fn(move || {
        let n = stack.pop()?;
        for child in n.children.iter().rev() {
            stack.push(child);
        }
        Some(n)
    })
}

// ── parse_ast ──────────────────────────────────────────────────────────────

/// Internal trace node used during AST construction.
#[derive(Clone, Debug)]
struct TraceNode {
    rule: String,
    start: usize,
    end: usize,
    children: Vec<TraceNode>,
}

/// A rule-lifecycle event collected during the parse, folded into the tree.
#[derive(Clone, Copy)]
enum TraceKind {
    Enter,
    Exit,
    Fail,
}

struct TraceEvent {
    kind: TraceKind,
    rule: String,
    pos: usize,
}

/// A [`crate::driver::ParseDriver`] that records the rule lifecycle (enter /
/// exit / fail) so the AST builder can fold it into a tree. The replacement for
/// the former `with_trace` event stream.
#[derive(Default)]
struct AstCollector {
    events: std::cell::RefCell<Vec<TraceEvent>>,
}

impl crate::driver::ParseDriver for AstCollector {
    fn handle(
        &self,
        effect: &crate::driver::ParseEffect<'_>,
        _view: &crate::driver::ParseView<'_>,
    ) -> crate::driver::Directive {
        use crate::driver::ParseEffect::*;
        let event = match effect {
            RuleEnter { rule, pos } => Some(TraceEvent {
                kind: TraceKind::Enter,
                rule: rule.to_string(),
                pos: *pos,
            }),
            // The exit "position" is the rule's end, matching the old trace.
            RuleExit { rule, end, .. } => Some(TraceEvent {
                kind: TraceKind::Exit,
                rule: rule.to_string(),
                pos: *end,
            }),
            RuleFail { rule, pos } => Some(TraceEvent {
                kind: TraceKind::Fail,
                rule: rule.to_string(),
                pos: *pos,
            }),
            _ => None,
        };
        if let Some(event) = event {
            self.events.borrow_mut().push(event);
        }
        crate::driver::Directive::Proceed
    }
}

/// Parse `text` with `grammar` and build an `AstNode` tree from the rule trace.
///
/// This mirrors `peg/ast/_core.py`'s `parse_ast()`. The grammar is run with
/// tracing enabled; enter/exit events are folded into a tree; duplicate
/// consecutive siblings are pruned; the best root is selected.
pub fn parse_ast(
    grammar: &crate::grammar::Grammar,
    text: &str,
    start_rule: Option<&str>,
) -> Result<AstNode, crate::error::ParseError> {
    parse_ast_with_max_steps(grammar, text, start_rule, None)
}

/// Like `parse_ast` but accepts a custom `max_steps` budget for large inputs.
pub fn parse_ast_with_max_steps(
    grammar: &crate::grammar::Grammar,
    text: &str,
    start_rule: Option<&str>,
    max_steps: Option<usize>,
) -> Result<AstNode, crate::error::ParseError> {
    let (start, collected, result) = collect_ast_events(grammar, text, start_rule, max_steps);
    result?;
    let roots = build_trace_forest(&collected);
    let root = select_root_node(&roots, &start, text.len()).ok_or_else(|| {
        crate::error::ParseError::new("AST parse produced no trace", 0, text.len())
    })?;
    Ok(build_ast_tree(&dedupe_trace_tree(root.clone())))
}

/// Error-tolerant parse: **always** returns a best-effort tree, never an error.
///
/// When the grammar cannot match the whole input, the matched prefix is returned
/// with a synthetic [`ERROR_RULE`] node ([`AstNode::error`]) covering the
/// unmatched tail, and the root's `error` flag set. If nothing matched at all,
/// a single error node spanning the whole input is returned. Built on the same
/// rule-lifecycle stream (enter/exit/fail) as [`parse_ast`] — useful for editors
/// and IDE tooling that must produce a tree even from invalid source.
pub fn parse_ast_tolerant(
    grammar: &crate::grammar::Grammar,
    text: &str,
    start_rule: Option<&str>,
) -> AstNode {
    let (start, collected, _result) = collect_ast_events(grammar, text, start_rule, None);
    let text_len = text.len();
    let roots = build_trace_forest(&collected);
    match select_root_node(&roots, &start, text_len) {
        None => AstNode::error(0, text_len),
        Some(root) => {
            let mut node = build_ast_tree(&dedupe_trace_tree(root.clone()));
            if node.span.end < text_len {
                let mut kids = node.children.to_vec();
                kids.push(AstNode::error(node.span.end, text_len));
                node.children = kids.into();
                node.span.end = text_len;
                node.error = true;
            }
            node
        }
    }
}

/// Run the parse with an `AstCollector` driver, returning the effective start
/// rule, the collected lifecycle events, and the (possibly failed) parse result.
fn collect_ast_events(
    grammar: &crate::grammar::Grammar,
    text: &str,
    start_rule: Option<&str>,
    max_steps: Option<usize>,
) -> (
    String,
    Vec<TraceEvent>,
    Result<(), crate::error::ParseError>,
) {
    use crate::parser_engine::PEGParser;
    use crate::types::ParserConfig;

    let effective_grammar: std::borrow::Cow<crate::grammar::Grammar> =
        if let Some(rule) = start_rule {
            if rule != grammar.start_rule {
                let mut g = grammar.clone();
                g.start_rule = rule.to_string();
                std::borrow::Cow::Owned(g)
            } else {
                std::borrow::Cow::Borrowed(grammar)
            }
        } else {
            std::borrow::Cow::Borrowed(grammar)
        };

    let steps = max_steps.unwrap_or_else(|| text.len().saturating_add(65536).max(65536));
    let config = ParserConfig::default().with_max_steps(steps);

    let collector = AstCollector::default();
    let result = PEGParser
        .parse_with_driver(&effective_grammar, text, &config, Some(&collector))
        .map(|_| ());
    (
        effective_grammar.start_rule.clone(),
        collector.events.into_inner(),
        result,
    )
}

fn build_trace_forest(events: &[TraceEvent]) -> Vec<TraceNode> {
    let mut stack: Vec<TraceNode> = Vec::new();
    let mut roots: Vec<TraceNode> = Vec::new();
    for event in events {
        match event.kind {
            TraceKind::Enter => {
                stack.push(TraceNode {
                    rule: event.rule.clone(),
                    start: event.pos,
                    end: event.pos,
                    children: Vec::new(),
                });
            }
            TraceKind::Exit => {
                if let Some(mut node) = stack.pop() {
                    node.end = event.pos;
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(node);
                    } else {
                        roots.push(node);
                    }
                }
            }
            TraceKind::Fail => {
                stack.pop();
            }
        }
    }
    roots
}

fn select_root_node<'a>(
    roots: &'a [TraceNode],
    start_rule: &str,
    text_len: usize,
) -> Option<&'a TraceNode> {
    roots.iter().max_by_key(|node| {
        let is_start = if node.rule == start_rule { 1usize } else { 0 };
        let full_span = if node.start == 0 && node.end == text_len {
            1usize
        } else {
            0
        };
        let span_size = node.end.saturating_sub(node.start);
        let desc = descendant_count(node);
        (is_start, full_span, span_size, desc)
    })
}

fn descendant_count(node: &TraceNode) -> usize {
    node.children.iter().map(|c| 1 + descendant_count(c)).sum()
}

fn dedupe_trace_tree(mut node: TraceNode) -> TraceNode {
    let deduped_children: Vec<TraceNode> = {
        let mut result: Vec<TraceNode> = Vec::new();
        let mut last_sig: Option<(String, usize, usize)> = None;
        for child in node.children.drain(..) {
            let deduped = dedupe_trace_tree(child);
            let sig = (deduped.rule.clone(), deduped.start, deduped.end);
            if Some(&sig) == last_sig.as_ref() {
                continue;
            }
            last_sig = Some(sig);
            result.push(deduped);
        }
        result
    };
    node.children = deduped_children;
    node
}

fn build_ast_tree(node: &TraceNode) -> AstNode {
    // The rule-lifecycle trace carries only rule enter/exit, so every trace child
    // is an AST child. (`captures`/`action` are populated by the value-building
    // path, not this trace-folding one — they stay empty here.)
    let children: Vec<AstNode> = node.children.iter().map(build_ast_tree).collect();
    AstNode {
        rule: node.rule.clone(),
        span: AstSpan::new(node.start, node.end),
        children: children.into(),
        captures: Vec::new(),
        action: String::new(),
        error: false,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ast_span_properties() {
        let s = AstSpan::new(3, 8);
        assert_eq!(s.start, 3);
        assert_eq!(s.end, 8);
        assert_eq!(s.length(), 5);
        assert!(!s.is_empty());
        assert!(AstSpan::new(5, 5).is_empty());
    }

    #[test]
    fn source_line_col_single_line() {
        let src = Source::new("hello world");
        assert_eq!(src.line_col(0), (1, 1));
        assert_eq!(src.line_col(6), (1, 7));
    }

    #[test]
    fn source_line_col_multiline() {
        let src = Source::new("abc\ndef\nghi");
        assert_eq!(src.line_col(0), (1, 1));
        assert_eq!(src.line_col(3), (1, 4)); // the '\n' itself
        assert_eq!(src.line_col(4), (2, 1));
        assert_eq!(src.line_col(7), (2, 4));
        assert_eq!(src.line_col(8), (3, 1));
    }

    #[test]
    fn source_slice() {
        let src = Source::new("hello world");
        assert_eq!(src.slice(&AstSpan::new(6, 11)), "world");
    }

    #[test]
    fn source_with_name() {
        let src = Source::new("x").with_name("main.caap");
        assert_eq!(src.name.as_deref(), Some("main.caap"));
    }

    #[test]
    fn ast_node_builder() {
        let child = AstNode::new("leaf", AstSpan::new(2, 4));
        let cap = AstCapture::new("lhs", child.clone());
        let node = AstNode::new("root", AstSpan::new(0, 10))
            .with_children(vec![child])
            .with_captures(vec![cap])
            .with_action("build_root");
        assert_eq!(node.rule, "root");
        assert_eq!(node.children.len(), 1);
        assert_eq!(node.captures.len(), 1);
        assert_eq!(node.action, "build_root");
    }

    #[test]
    fn ast_node_text_from() {
        let src = Source::new("(+ 1 2)");
        let node = AstNode::new("expr", AstSpan::new(1, 6));
        assert_eq!(node.text_from(&src), "+ 1 2");
    }

    #[test]
    fn ast_capture_new() {
        let inner = AstNode::new("item", AstSpan::new(0, 3));
        let cap = AstCapture::new("value", inner.clone());
        assert_eq!(cap.label, "value");
        assert_eq!(*cap.node, inner);
    }

    #[test]
    fn walk_visits_all_nodes_preorder() {
        let leaf1 = AstNode::new("a", AstSpan::new(0, 1));
        let leaf2 = AstNode::new("b", AstSpan::new(1, 2));
        let root = AstNode::new("root", AstSpan::new(0, 2)).with_children(vec![leaf1, leaf2]);
        let names: Vec<&str> = walk(&root).map(|n| n.rule.as_str()).collect();
        assert_eq!(names, vec!["root", "a", "b"]);
    }

    #[test]
    fn parse_ast_builds_tree_for_simple_grammar() {
        use crate::grammar::Grammar;
        let grammar = Grammar::trusted_new("word <- [a-z]+").with_start_rule("word");
        let tree = parse_ast(&grammar, "abc", None).expect("parse_ast succeeds");
        assert_eq!(tree.rule, "word");
        assert_eq!(tree.span, AstSpan::new(0, 3));
    }

    #[test]
    fn parse_ast_tolerant_returns_clean_tree_on_full_match() {
        use crate::grammar::Grammar;
        let grammar = Grammar::trusted_new("word <- [a-z]+").with_start_rule("word");
        let tree = parse_ast_tolerant(&grammar, "abc", None);
        assert_eq!(tree.rule, "word");
        assert!(!tree.has_errors());
        assert_eq!(tree.span, AstSpan::new(0, 3));
    }

    #[test]
    fn parse_ast_tolerant_marks_unmatched_tail() {
        use crate::grammar::Grammar;
        // `[a-z]+` matches the "abc" prefix; "123" is the unmatched tail.
        let grammar = Grammar::trusted_new("word <- [a-z]+").with_start_rule("word");
        let tree = parse_ast_tolerant(&grammar, "abc123", None);
        assert!(tree.error, "root should be flagged as containing an error");
        assert_eq!(tree.span, AstSpan::new(0, 6));
        let err = tree
            .children
            .iter()
            .find(|c| c.error)
            .expect("an error child for the tail");
        assert_eq!(err.rule, ERROR_RULE);
        assert_eq!(err.span, AstSpan::new(3, 6));
    }

    #[test]
    fn parse_ast_tolerant_full_error_when_nothing_matches() {
        use crate::grammar::Grammar;
        let grammar = Grammar::trusted_new("digits <- [0-9]+").with_start_rule("digits");
        let tree = parse_ast_tolerant(&grammar, "abc", None);
        assert!(tree.error);
        assert_eq!(tree.rule, ERROR_RULE);
        assert_eq!(tree.span, AstSpan::new(0, 3));
    }
}
