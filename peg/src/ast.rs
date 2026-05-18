use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

// ── AstSpan ────────────────────────────────────────────────────────────────

/// Byte-offset span in the source text `[start, end)`.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct AstSpan {
    pub start: usize,
    pub end: usize,
}

impl AstSpan {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn length(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

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
    pub text: String,
    pub name: Option<String>,
    line_offsets: Vec<usize>,
}

impl Source {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let line_offsets = Self::compute_offsets(&text);
        Self {
            text,
            name: None,
            line_offsets,
        }
    }

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
    pub label: String,
    pub node: Box<AstNode>,
}

impl AstCapture {
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
    pub rule: String,
    pub span: AstSpan,
    pub children: Vec<AstNode>,
    pub captures: Vec<AstCapture>,
    pub action: String,
}

impl AstNode {
    pub fn new(rule: impl Into<String>, span: AstSpan) -> Self {
        Self {
            rule: rule.into(),
            span,
            children: Vec::new(),
            captures: Vec::new(),
            action: String::new(),
        }
    }

    pub fn with_children(mut self, children: Vec<AstNode>) -> Self {
        self.children = children;
        self
    }

    pub fn with_captures(mut self, captures: Vec<AstCapture>) -> Self {
        self.captures = captures;
        self
    }

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
    use crate::parser::PEGParser;
    use crate::types::{ParseEvent, ParserConfig};

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

    let events: Arc<Mutex<Vec<ParseEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let ev = events.clone();
    let steps = max_steps.unwrap_or_else(|| text.len().saturating_add(65536).max(65536));
    let config = ParserConfig::default()
        .with_max_steps(steps)
        .with_trace(move |e| {
            ev.lock().unwrap().push(e.clone());
        });

    let parser = PEGParser;
    parser.parse(&effective_grammar, text, &config)?;

    let collected = events.lock().unwrap().clone();
    let roots = build_trace_forest(&collected);
    let root =
        select_root_node(&roots, &effective_grammar.start_rule, text.len()).ok_or_else(|| {
            crate::error::ParseError::new("AST parse produced no trace", 0, text.len())
        })?;
    let deduped = dedupe_trace_tree(root.clone());
    Ok(build_ast_tree(&deduped))
}

fn build_trace_forest(events: &[crate::types::ParseEvent]) -> Vec<TraceNode> {
    let mut stack: Vec<TraceNode> = Vec::new();
    let mut roots: Vec<TraceNode> = Vec::new();
    for event in events {
        match event.kind {
            "enter" => {
                stack.push(TraceNode {
                    rule: event.rule.clone(),
                    start: event.pos,
                    end: event.pos,
                    children: Vec::new(),
                });
            }
            "exit" => {
                if let Some(mut node) = stack.pop() {
                    node.end = event.pos;
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(node);
                    } else {
                        roots.push(node);
                    }
                }
            }
            "fail" => {
                stack.pop();
            }
            _ => {}
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
    let mut children = Vec::new();
    let mut captures = Vec::new();
    let mut action = String::new();
    for child in &node.children {
        let built = build_ast_tree(child);
        match parse_behavior_trace_rule(&child.rule) {
            Some(("capture", label)) => captures.push(AstCapture::new(label, built)),
            Some(("action", label)) => action = label.to_string(),
            _ => children.push(built),
        }
    }
    AstNode {
        rule: node.rule.clone(),
        span: AstSpan::new(node.start, node.end),
        children,
        captures,
        action,
    }
}

fn parse_behavior_trace_rule(rule: &str) -> Option<(&str, &str)> {
    let mut parts = rule.splitn(5, "::");
    if parts.next()? != "__beh" {
        return None;
    }
    let kind = parts.next()?;
    let _node_id = parts.next()?;
    let _index = parts.next()?;
    let label = parts.next()?;
    Some((kind, label))
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
        let grammar = Grammar::new("word <- [a-z]+").with_start_rule("word");
        let tree = parse_ast(&grammar, "abc", None).expect("parse_ast succeeds");
        assert_eq!(tree.rule, "word");
        assert_eq!(tree.span, AstSpan::new(0, 3));
    }
}
