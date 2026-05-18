use serde::{Deserialize, Serialize};

/// Primitive scalar value used in behavior arguments.
///
/// Mirrors Python's `GrammarDataValue` restricted to the types that behaviors accept.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GrammarScalar {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl Eq for GrammarScalar {}

impl GrammarScalar {
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

impl std::fmt::Display for GrammarScalar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Null => write!(f, "null"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int(n) => write!(f, "{n}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Str(s) => write!(f, "{s}"),
        }
    }
}

/// Applies a named transformation to the parse value after a node matches.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransformBehavior {
    pub name: String,
    pub args: Vec<GrammarScalar>,
}

impl TransformBehavior {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            args: Vec::new(),
        }
    }

    pub fn with_args(mut self, args: Vec<GrammarScalar>) -> Self {
        self.args = args;
        self
    }
}

/// Guards a rule match with a named semantic predicate; the match is rejected when
/// the predicate returns `false`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PredicateBehavior {
    pub name: String,
    pub args: Vec<GrammarScalar>,
}

impl PredicateBehavior {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            args: Vec::new(),
        }
    }

    pub fn with_args(mut self, args: Vec<GrammarScalar>) -> Self {
        self.args = args;
        self
    }
}

/// Attaches a diagnostic label to a rule so failures produce a human-readable message.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticBehavior {
    pub label: String,
}

impl DiagnosticBehavior {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
        }
    }
}

/// Which kind of trace event a `TraceBehavior` represents.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TraceBehaviorKind {
    Capture,
    Action,
}

impl TraceBehaviorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Action => "action",
        }
    }
}

/// Emits a trace event (capture or action) when a rule matches.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TraceBehavior {
    pub kind: TraceBehaviorKind,
    pub label: String,
}

impl TraceBehavior {
    pub fn capture(label: impl Into<String>) -> Self {
        Self {
            kind: TraceBehaviorKind::Capture,
            label: label.into(),
        }
    }

    pub fn action(label: impl Into<String>) -> Self {
        Self {
            kind: TraceBehaviorKind::Action,
            label: label.into(),
        }
    }
}

/// A single behavior entry attached to a PEG node.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BehaviorEntry {
    Transform(TransformBehavior),
    Predicate(PredicateBehavior),
    Diagnostic(DiagnosticBehavior),
    Trace(TraceBehavior),
}

impl BehaviorEntry {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Transform(_) => "transform",
            Self::Predicate(_) => "predicate",
            Self::Diagnostic(_) => "diagnostic",
            Self::Trace(_) => "trace",
        }
    }

    pub fn is_transform(&self) -> bool {
        matches!(self, Self::Transform(_))
    }

    pub fn is_predicate(&self) -> bool {
        matches!(self, Self::Predicate(_))
    }

    pub fn as_transform(&self) -> Option<&TransformBehavior> {
        if let Self::Transform(b) = self {
            Some(b)
        } else {
            None
        }
    }

    pub fn as_predicate(&self) -> Option<&PredicateBehavior> {
        if let Self::Predicate(b) = self {
            Some(b)
        } else {
            None
        }
    }
}

/// Generate the synthetic rule name used to record a trace behavior in the grammar.
pub fn trace_rule_name(node_id: u64, index: usize, entry: &TraceBehavior) -> String {
    format!(
        "__beh::{}::{}::{}::{}",
        entry.kind.as_str(),
        node_id,
        index,
        entry.label
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_behavior_new() {
        let b = TransformBehavior::new("upper");
        assert_eq!(b.name, "upper");
        assert!(b.args.is_empty());
    }

    #[test]
    fn transform_behavior_with_args() {
        let b = TransformBehavior::new("pad").with_args(vec![GrammarScalar::Int(4)]);
        assert_eq!(b.args, vec![GrammarScalar::Int(4)]);
    }

    #[test]
    fn predicate_behavior_kind_label() {
        let entry = BehaviorEntry::Predicate(PredicateBehavior::new("is_keyword"));
        assert_eq!(entry.kind(), "predicate");
        assert!(entry.is_predicate());
        assert!(!entry.is_transform());
    }

    #[test]
    fn diagnostic_behavior_stores_label() {
        let b = DiagnosticBehavior::new("expected identifier");
        assert_eq!(b.label, "expected identifier");
    }

    #[test]
    fn trace_behavior_capture_kind() {
        let b = TraceBehavior::capture("my_capture");
        assert_eq!(b.kind, TraceBehaviorKind::Capture);
        assert_eq!(b.label, "my_capture");
    }

    #[test]
    fn trace_behavior_action_kind() {
        let b = TraceBehavior::action("on_match");
        assert_eq!(b.kind, TraceBehaviorKind::Action);
    }

    #[test]
    fn trace_rule_name_format() {
        let b = TraceBehavior::capture("x");
        let name = trace_rule_name(42, 0, &b);
        assert_eq!(name, "__beh::capture::42::0::x");
    }

    #[test]
    fn grammar_scalar_display() {
        assert_eq!(GrammarScalar::Null.to_string(), "null");
        assert_eq!(GrammarScalar::Bool(true).to_string(), "true");
        assert_eq!(GrammarScalar::Int(-3).to_string(), "-3");
        assert_eq!(GrammarScalar::Str("hi".into()).to_string(), "hi");
    }

    #[test]
    fn behavior_entry_as_transform_returns_inner() {
        let tb = TransformBehavior::new("trim");
        let entry = BehaviorEntry::Transform(tb.clone());
        assert_eq!(entry.as_transform(), Some(&tb));
        assert!(entry.as_predicate().is_none());
    }

    #[test]
    fn grammar_scalar_eq() {
        assert_eq!(GrammarScalar::Int(1), GrammarScalar::Int(1));
        assert_ne!(GrammarScalar::Int(1), GrammarScalar::Int(2));
        assert_ne!(GrammarScalar::Str("a".into()), GrammarScalar::Int(1));
    }
}
