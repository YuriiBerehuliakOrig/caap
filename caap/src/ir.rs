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

/// Recursive literal data type mirroring Python's `IRLiteralData`.
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
        Ok(Self {
            id,
            identifier: Rc::from(identifier),
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
    pub args: Vec<NodeId>,
}

impl CallNode {
    pub fn new(id: NodeId, callee: NodeId, args: Vec<NodeId>) -> Self {
        Self { id, callee, args }
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
}
