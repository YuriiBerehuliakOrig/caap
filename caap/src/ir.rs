/// Core CAAP IR node types.
///
/// Three node kinds form the complete IR:
///   NameNode   — identifier reference
///   LiteralNode — compile-time constant
///   CallNode    — function application (callee + ordered args)
use serde::{Deserialize, Serialize};
use std::rc::Rc;

use crate::error::{CaapError, CaapResult};
use crate::source::SourceSpan;

pub type NodeId = u32;

/// Recursive literal data type used by IR snapshots and runtime lowering.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum IrLiteralData {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Tuple(Vec<IrLiteralData>),
    Dict(Vec<(String, IrLiteralData)>), // sorted by key
}

impl IrLiteralData {
    pub fn dict(entries: impl IntoIterator<Item = (String, IrLiteralData)>) -> CaapResult<Self> {
        let mut entries: Vec<(String, IrLiteralData)> = entries.into_iter().collect();
        if let Some((idx, _)) = entries
            .iter()
            .enumerate()
            .find(|(_, (key, _))| key.is_empty())
        {
            return Err(CaapError::ir(format!(
                "IR literal dict key at index {idx} must be non-empty"
            )));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(Self::Dict(entries))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NameSpec {
    pub identifier: String,
    pub span: Option<SourceSpan>,
}

impl NameSpec {
    pub fn new(identifier: impl Into<String>) -> CaapResult<Self> {
        Self::with_span(identifier, None)
    }

    pub fn with_span(identifier: impl Into<String>, span: Option<SourceSpan>) -> CaapResult<Self> {
        let identifier = identifier.into();
        if identifier.is_empty() {
            return Err(CaapError::ir("name spec identifier must be non-empty"));
        }
        Ok(Self { identifier, span })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiteralSpec {
    pub value: IrLiteralData,
    pub span: Option<SourceSpan>,
}

impl LiteralSpec {
    pub fn new(value: IrLiteralData) -> Self {
        Self { value, span: None }
    }

    pub fn with_span(value: IrLiteralData, span: Option<SourceSpan>) -> Self {
        Self { value, span }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallSpec {
    pub callee: Box<ExprSpec>,
    pub args: Vec<ExprSpec>,
    pub span: Option<SourceSpan>,
}

impl CallSpec {
    pub fn new(callee: ExprSpec, args: Vec<ExprSpec>) -> Self {
        Self {
            callee: Box::new(callee),
            args,
            span: None,
        }
    }

    pub fn with_span(callee: ExprSpec, args: Vec<ExprSpec>, span: Option<SourceSpan>) -> Self {
        Self {
            callee: Box::new(callee),
            args,
            span,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ExprSpec {
    Name(NameSpec),
    Literal(LiteralSpec),
    Call(CallSpec),
}

impl ExprSpec {
    pub fn name(identifier: impl Into<String>) -> CaapResult<Self> {
        Ok(Self::Name(NameSpec::new(identifier)?))
    }

    pub fn name_with_span(
        identifier: impl Into<String>,
        span: Option<SourceSpan>,
    ) -> CaapResult<Self> {
        Ok(Self::Name(NameSpec::with_span(identifier, span)?))
    }

    pub fn literal(value: IrLiteralData) -> Self {
        Self::Literal(LiteralSpec::new(value))
    }

    pub fn literal_with_span(value: IrLiteralData, span: Option<SourceSpan>) -> Self {
        Self::Literal(LiteralSpec::with_span(value, span))
    }

    pub fn call(callee: ExprSpec, args: Vec<ExprSpec>) -> Self {
        Self::Call(CallSpec::new(callee, args))
    }

    pub fn call_with_span(callee: ExprSpec, args: Vec<ExprSpec>, span: Option<SourceSpan>) -> Self {
        Self::Call(CallSpec::with_span(callee, args, span))
    }

    pub fn span(&self) -> Option<&SourceSpan> {
        match self {
            Self::Name(spec) => spec.span.as_ref(),
            Self::Literal(spec) => spec.span.as_ref(),
            Self::Call(spec) => spec.span.as_ref(),
        }
    }
}
thread_local! {
    static INTERN_POOL: std::cell::RefCell<std::collections::HashSet<Rc<str>>> = std::cell::RefCell::new(std::collections::HashSet::new());
}

pub fn intern_string(s: &str) -> Rc<str> {
    INTERN_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        if let Some(existing) = pool.get(s) {
            existing.clone()
        } else {
            let rc: Rc<str> = Rc::from(s);
            pool.insert(rc.clone());
            rc
        }
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NameNode {
    pub id: NodeId,
    pub identifier: Rc<str>,
}

impl NameNode {
    pub fn new(id: NodeId, identifier: impl Into<String>) -> CaapResult<Self> {
        let identifier = identifier.into();
        if identifier.is_empty() {
            return Err(CaapError::ir("name node identifier must be non-empty"));
        }
        let interned = intern_string(&identifier);
        Ok(Self {
            id,
            identifier: interned,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiteralNode {
    pub id: NodeId,
    pub value: IrLiteralData,
}

impl LiteralNode {
    pub fn new(id: NodeId, value: IrLiteralData) -> Self {
        Self { id, value }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallNode {
    pub id: NodeId,
    pub callee: NodeId,
    pub args: Rc<[NodeId]>,
}

impl CallNode {
    pub fn new(id: NodeId, callee: NodeId, args: Vec<NodeId>) -> Self {
        Self {
            id,
            callee,
            args: args.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Node {
    Name(NameNode),
    Literal(LiteralNode),
    Call(CallNode),
}

impl Node {
    pub fn id(&self) -> NodeId {
        match self {
            Node::Name(n) => n.id,
            Node::Literal(n) => n.id,
            Node::Call(n) => n.id,
        }
    }

    pub fn children(&self) -> Vec<NodeId> {
        match self {
            Node::Name(_) | Node::Literal(_) => vec![],
            Node::Call(n) => {
                let mut ch = vec![n.callee];
                ch.extend_from_slice(&n.args);
                ch
            }
        }
    }

    pub fn has_children(&self) -> bool {
        matches!(self, Node::Call(_))
    }

    pub fn contains_child(&self, id: NodeId) -> bool {
        match self {
            Node::Name(_) | Node::Literal(_) => false,
            Node::Call(n) => n.callee == id || n.args.contains(&id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_node_identifier_clones_share_storage() {
        let node = NameNode::new(1, "shared").unwrap();
        let cloned = node.clone();
        assert!(Rc::ptr_eq(&node.identifier, &cloned.identifier));
        assert_eq!(node.identifier.as_ref(), "shared");
    }

    #[test]
    fn name_nodes_with_same_identifier_share_storage_interning() {
        let node1 = NameNode::new(1, "shared_name").unwrap();
        let node2 = NameNode::new(2, "shared_name").unwrap();
        assert!(Rc::ptr_eq(&node1.identifier, &node2.identifier));
        assert_eq!(node1.identifier.as_ref(), "shared_name");
    }

    // Minimal Semantic Kernel invariant — docs/principles.md #1 and the
    // "Substrate ↔ policy boundary" section of docs/builtins.md: the IR is
    // exactly Name | Literal | Call, and every language construct (if, lambda,
    // match, …) is a `Call` whose callee names it. These exhaustive matches have
    // no `_` arm, so adding a fourth node kind FAILS TO COMPILE — the kernel
    // cannot grow a new primitive without a deliberate review here.
    #[test]
    fn ir_kernel_stays_name_literal_call_only() {
        fn lock_node(node: &Node) {
            match node {
                Node::Name(_) => {}
                Node::Literal(_) => {}
                Node::Call(_) => {}
            }
        }
        fn lock_expr(expr: &ExprSpec) {
            match expr {
                ExprSpec::Name(_) => {}
                ExprSpec::Literal(_) => {}
                ExprSpec::Call(_) => {}
            }
        }
        // Exercise the locks so they participate in compilation.
        lock_node(&Node::Literal(LiteralNode::new(0, IrLiteralData::Null)));
        lock_expr(&ExprSpec::literal(IrLiteralData::Null));
    }
}
