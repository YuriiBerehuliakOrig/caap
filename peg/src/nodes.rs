use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    Action,
    And,
    Behavior,
    Capture,
    Choice,
    Cut,
    Dedent,
    Eager,
    Expected,
    GrammarScope,
    GrammarScopeEnter,
    GrammarScopeExit,
    InlineAction,
    Indent,
    Island,
    Literal,
    Named,
    Newline,
    NoTrivia,
    Not,
    OneOrMore,
    Optional,
    Ref,
    Regex,
    RawBlock,
    SepOneOrMore,
    Sequence,
    ZeroOrMore,
    Token,
    TokenRef,
    Parameter,
    ImportedRef,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PegNode {
    Action(Vec<PegNode>),
    And(Box<PegNode>),
    Behavior(String, Box<PegNode>),
    Capture(String, Box<PegNode>),
    Choice(Vec<PegNode>),
    Cut(Box<PegNode>),
    Dedent,
    Eager(Box<PegNode>),
    Expected(String),
    GrammarScope {
        name: String,
        body: Vec<PegNode>,
    },
    GrammarScopeEnter,
    GrammarScopeExit,
    InlineAction(String, Vec<String>),
    Indent,
    Island(String),
    Literal(String),
    Named {
        name: String,
        node: Box<PegNode>,
    },
    Newline,
    NoTrivia,
    Not(Box<PegNode>),
    OneOrMore(Box<PegNode>),
    Optional(Box<PegNode>),
    Ref(String),
    Regex(String),
    RawBlock(String),
    SepOneOrMore {
        separator: Box<PegNode>,
        element: Box<PegNode>,
    },
    Sequence(Vec<PegNode>),
    ZeroOrMore(Box<PegNode>),
    Token(String),
    TokenRef(String),
    Parameter(String),
    ImportedRef(String),
}

impl PegNode {
    pub fn kind(&self) -> NodeKind {
        match self {
            Self::Action(_) => NodeKind::Action,
            Self::And(_) => NodeKind::And,
            Self::Behavior(_, _) => NodeKind::Behavior,
            Self::Capture(_, _) => NodeKind::Capture,
            Self::Choice(_) => NodeKind::Choice,
            Self::Cut(_) => NodeKind::Cut,
            Self::Dedent => NodeKind::Dedent,
            Self::Eager(_) => NodeKind::Eager,
            Self::Expected(_) => NodeKind::Expected,
            Self::GrammarScope { .. } => NodeKind::GrammarScope,
            Self::GrammarScopeEnter => NodeKind::GrammarScopeEnter,
            Self::GrammarScopeExit => NodeKind::GrammarScopeExit,
            Self::InlineAction(_, _) => NodeKind::InlineAction,
            Self::Indent => NodeKind::Indent,
            Self::Island(_) => NodeKind::Island,
            Self::Literal(_) => NodeKind::Literal,
            Self::Named { .. } => NodeKind::Named,
            Self::Newline => NodeKind::Newline,
            Self::NoTrivia => NodeKind::NoTrivia,
            Self::Not(_) => NodeKind::Not,
            Self::OneOrMore(_) => NodeKind::OneOrMore,
            Self::Optional(_) => NodeKind::Optional,
            Self::Ref(_) => NodeKind::Ref,
            Self::Regex(_) => NodeKind::Regex,
            Self::RawBlock(_) => NodeKind::RawBlock,
            Self::SepOneOrMore { .. } => NodeKind::SepOneOrMore,
            Self::Sequence(_) => NodeKind::Sequence,
            Self::ZeroOrMore(_) => NodeKind::ZeroOrMore,
            Self::Token(_) => NodeKind::Token,
            Self::TokenRef(_) => NodeKind::TokenRef,
            Self::Parameter(_) => NodeKind::Parameter,
            Self::ImportedRef(_) => NodeKind::ImportedRef,
        }
    }

    pub fn literal(value: impl Into<String>) -> Self {
        Self::Literal(value.into())
    }

    pub fn reference(name: impl Into<String>) -> Self {
        Self::Ref(name.into())
    }

    pub fn sequence(items: Vec<Self>) -> Self {
        Self::Sequence(items)
    }

    pub fn choice(items: Vec<Self>) -> Self {
        Self::Choice(items)
    }

    pub fn one_or_more(node: Self) -> Self {
        Self::OneOrMore(Box::new(node))
    }

    pub fn zero_or_more(node: Self) -> Self {
        Self::ZeroOrMore(Box::new(node))
    }

    pub fn optional(node: Self) -> Self {
        Self::Optional(Box::new(node))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PegGrammarTree {
    pub rules: Vec<(String, PegNode)>,
    pub start: String,
}

impl PegGrammarTree {
    pub fn new(start: impl Into<String>) -> Self {
        Self {
            rules: Vec::new(),
            start: start.into(),
        }
    }

    pub fn add_rule(mut self, name: impl Into<String>, node: PegNode) -> Self {
        self.rules.push((name.into(), node));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_mapping_is_stable() {
        let literal = PegNode::literal("x");
        assert_eq!(literal.kind(), NodeKind::Literal);

        let seq = PegNode::sequence(vec![PegNode::reference("a"), PegNode::literal("b")]);
        assert_eq!(seq.kind(), NodeKind::Sequence);
    }

    #[test]
    fn builds_tree_with_rules() {
        let tree = PegGrammarTree::new("start").add_rule("start", PegNode::literal("[a]"));
        assert_eq!(tree.start, "start");
        assert_eq!(tree.rules.len(), 1);
    }
}
