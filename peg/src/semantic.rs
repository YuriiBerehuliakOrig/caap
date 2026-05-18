use crate::behaviors::GrammarScalar;
use crate::types::{ParseValue, ParserConfig, ParserOutputMode};
use std::collections::HashMap;

// ── SemanticRuntime trait ──────────────────────────────────────────────────

/// Trait for executing user-defined semantic actions and predicates during
/// parsing.
///
/// Implementors can be passed to the parser to wire up host-language logic to
/// grammar rules — the Rust equivalent of Python's `SemanticRuntime`.
///
/// The legacy methods receive:
/// * `name`    — the action/predicate name as declared in the grammar
/// * `value`   — the parse result produced so far at this position
/// * `span`    — `(start, end)` byte offsets of the matched text, if available
/// * `named`   — named sub-values from `Named` bindings in the current rule
///
/// New runtimes should prefer `invoke_*_with_context`, which mirrors Python's
/// richer semantic runtime payload while preserving the older Rust API.
pub trait SemanticRuntime {
    /// Invoke a named transform action; must return a new `ParseValue`.
    fn invoke_action(
        &self,
        name: &str,
        value: ParseValue,
        span: Option<(usize, usize)>,
        named: &HashMap<String, ParseValue>,
    ) -> ParseValue;

    /// Invoke a named predicate; returns `true` to accept, `false` to fail.
    fn invoke_predicate(
        &self,
        name: &str,
        value: &ParseValue,
        span: Option<(usize, usize)>,
        named: &HashMap<String, ParseValue>,
    ) -> bool;

    /// Invoke an action with the full semantic context.
    fn invoke_action_with_context(
        &self,
        name: &str,
        value: ParseValue,
        context: &SemanticContext<'_>,
    ) -> ParseValue {
        self.invoke_action(name, value, context.span, &context.named)
    }

    /// Invoke a predicate with the full semantic context.
    fn invoke_predicate_with_context(
        &self,
        name: &str,
        value: &ParseValue,
        context: &SemanticContext<'_>,
    ) -> bool {
        self.invoke_predicate(name, value, context.span, &context.named)
    }
}

/// Rich semantic-call payload, intentionally close to Python's runtime context.
#[derive(Clone, Debug)]
pub struct SemanticContext<'a> {
    pub span: Option<(usize, usize)>,
    pub matched_text: &'a str,
    pub args: Vec<GrammarScalar>,
    pub items: Vec<ParseValue>,
    pub named: HashMap<String, ParseValue>,
    pub grammar_start: &'a str,
    pub grammar: GrammarContext,
    pub config: ParserConfigContext,
    pub state: ParserStateContext,
    pub rule_stack: Vec<String>,
    pub pos: usize,
}

/// Rust-safe projection of grammar data exposed to semantic hooks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarContext {
    pub start_rule: String,
    pub rule_count: usize,
    pub import_aliases: Vec<String>,
    pub metadata_keys: Vec<String>,
}

impl GrammarContext {
    pub fn new(
        start_rule: impl Into<String>,
        rule_count: usize,
        import_aliases: Vec<String>,
        metadata_keys: Vec<String>,
    ) -> Self {
        Self {
            start_rule: start_rule.into(),
            rule_count,
            import_aliases,
            metadata_keys,
        }
    }
}

/// Rust-safe projection of parser config exposed to semantic hooks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParserConfigContext {
    pub return_spans: bool,
    pub memo: bool,
    pub max_steps: usize,
    pub include_invalid_rules: bool,
    pub output_mode: String,
}

impl ParserConfigContext {
    pub fn from_config(config: &ParserConfig) -> Self {
        let output_mode = match config.output_mode {
            ParserOutputMode::Value => "value",
            ParserOutputMode::Ast => "ast",
        };
        Self {
            return_spans: config.return_spans,
            memo: config.memo,
            max_steps: config.max_steps,
            include_invalid_rules: config.include_invalid_rules,
            output_mode: output_mode.to_string(),
        }
    }
}

/// Rust-safe projection of parser state exposed to semantic hooks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParserStateContext {
    pub trivia_on: bool,
    pub rule_stack: Vec<String>,
    pub param_depth: usize,
    pub memo_entries: usize,
    pub indentation_enabled: bool,
    pub bracket_depth: usize,
}

impl<'a> SemanticContext<'a> {
    pub fn new(
        source: &'a str,
        span: Option<(usize, usize)>,
        value: &ParseValue,
        args: Vec<GrammarScalar>,
        grammar_start: &'a str,
        grammar: GrammarContext,
        config: ParserConfigContext,
        state: ParserStateContext,
        rule_stack: Vec<String>,
        pos: usize,
    ) -> Self {
        let matched_text = span
            .and_then(|(start, end)| source.get(start..end))
            .unwrap_or("");
        let items = match value {
            ParseValue::Node(_, children) => children.clone(),
            ParseValue::SpannedValue { value, .. } => match value.as_ref() {
                ParseValue::Node(_, children) => children.clone(),
                inner => vec![inner.clone()],
            },
            other if other.is_nil() => Vec::new(),
            other => vec![other.clone()],
        };
        let named = value
            .named_bindings()
            .into_iter()
            .map(|(k, v)| (k, v.clone()))
            .collect();
        Self {
            span,
            matched_text,
            args,
            items,
            named,
            grammar_start,
            grammar,
            config,
            state,
            rule_stack,
            pos,
        }
    }
}

// ── Null runtime ──────────────────────────────────────────────────────────

/// A [`SemanticRuntime`] that passes values through unchanged (actions are
/// identity, predicates always accept).
pub struct NullSemanticRuntime;

impl SemanticRuntime for NullSemanticRuntime {
    fn invoke_action(
        &self,
        _name: &str,
        value: ParseValue,
        _span: Option<(usize, usize)>,
        _named: &HashMap<String, ParseValue>,
    ) -> ParseValue {
        value
    }

    fn invoke_predicate(
        &self,
        _name: &str,
        _value: &ParseValue,
        _span: Option<(usize, usize)>,
        _named: &HashMap<String, ParseValue>,
    ) -> bool {
        true
    }
}

// ── Closure-based runtime ─────────────────────────────────────────────────

/// A [`SemanticRuntime`] backed by two `Box<dyn Fn>` closures, useful for
/// testing or embedding simple transforms without a full registry.
pub struct ClosureSemanticRuntime {
    action: Box<
        dyn Fn(
            &str,
            ParseValue,
            Option<(usize, usize)>,
            &HashMap<String, ParseValue>,
        ) -> ParseValue,
    >,
    predicate: Box<
        dyn Fn(&str, &ParseValue, Option<(usize, usize)>, &HashMap<String, ParseValue>) -> bool,
    >,
}

impl ClosureSemanticRuntime {
    /// Build a runtime from explicit action and predicate closures.
    pub fn new<A, P>(action: A, predicate: P) -> Self
    where
        A: Fn(&str, ParseValue, Option<(usize, usize)>, &HashMap<String, ParseValue>) -> ParseValue
            + 'static,
        P: Fn(&str, &ParseValue, Option<(usize, usize)>, &HashMap<String, ParseValue>) -> bool
            + 'static,
    {
        Self {
            action: Box::new(action),
            predicate: Box::new(predicate),
        }
    }

    /// Build a runtime where every action is an identity and every predicate
    /// returns `true` (useful as a no-op base when only one side is needed).
    pub fn passthrough() -> Self {
        Self::new(|_, v, _, _| v, |_, _, _, _| true)
    }
}

impl SemanticRuntime for ClosureSemanticRuntime {
    fn invoke_action(
        &self,
        name: &str,
        value: ParseValue,
        span: Option<(usize, usize)>,
        named: &HashMap<String, ParseValue>,
    ) -> ParseValue {
        (self.action)(name, value, span, named)
    }

    fn invoke_predicate(
        &self,
        name: &str,
        value: &ParseValue,
        span: Option<(usize, usize)>,
        named: &HashMap<String, ParseValue>,
    ) -> bool {
        (self.predicate)(name, value, span, named)
    }
}

// ── Context-aware closure runtime ─────────────────────────────────────────

pub struct ContextualSemanticRuntime {
    action: Box<dyn Fn(&str, ParseValue, &SemanticContext<'_>) -> ParseValue>,
    predicate: Box<dyn Fn(&str, &ParseValue, &SemanticContext<'_>) -> bool>,
}

impl ContextualSemanticRuntime {
    pub fn new<A, P>(action: A, predicate: P) -> Self
    where
        A: Fn(&str, ParseValue, &SemanticContext<'_>) -> ParseValue + 'static,
        P: Fn(&str, &ParseValue, &SemanticContext<'_>) -> bool + 'static,
    {
        Self {
            action: Box::new(action),
            predicate: Box::new(predicate),
        }
    }
}

impl SemanticRuntime for ContextualSemanticRuntime {
    fn invoke_action(
        &self,
        _name: &str,
        value: ParseValue,
        _span: Option<(usize, usize)>,
        _named: &HashMap<String, ParseValue>,
    ) -> ParseValue {
        value
    }

    fn invoke_predicate(
        &self,
        _name: &str,
        _value: &ParseValue,
        _span: Option<(usize, usize)>,
        _named: &HashMap<String, ParseValue>,
    ) -> bool {
        true
    }

    fn invoke_action_with_context(
        &self,
        name: &str,
        value: ParseValue,
        context: &SemanticContext<'_>,
    ) -> ParseValue {
        (self.action)(name, value, context)
    }

    fn invoke_predicate_with_context(
        &self,
        name: &str,
        value: &ParseValue,
        context: &SemanticContext<'_>,
    ) -> bool {
        (self.predicate)(name, value, context)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn null_runtime_action_is_identity() {
        let rt = NullSemanticRuntime;
        let value = ParseValue::Text("hello".into());
        let result = rt.invoke_action("upper", value.clone(), None, &HashMap::new());
        assert_eq!(result, value);
    }

    #[test]
    fn null_runtime_predicate_always_true() {
        let rt = NullSemanticRuntime;
        assert!(rt.invoke_predicate("check", &ParseValue::Nil, None, &HashMap::new()));
    }

    #[test]
    fn closure_runtime_action_applies_closure() {
        let rt = ClosureSemanticRuntime::new(
            |_name, value, _, _| match value {
                ParseValue::Text(s) => ParseValue::Text(s.to_uppercase()),
                other => other,
            },
            |_, _, _, _| true,
        );
        let result = rt.invoke_action(
            "upper",
            ParseValue::Text("hello".into()),
            None,
            &HashMap::new(),
        );
        assert_eq!(result, ParseValue::Text("HELLO".into()));
    }

    #[test]
    fn closure_runtime_predicate_applies_closure() {
        let rt = ClosureSemanticRuntime::new(
            |_, v, _, _| v,
            |_name, value, _, _| matches!(value, ParseValue::Text(_)),
        );
        assert!(rt.invoke_predicate(
            "is_text",
            &ParseValue::Text("x".into()),
            None,
            &HashMap::new()
        ));
        assert!(!rt.invoke_predicate("is_text", &ParseValue::Nil, None, &HashMap::new()));
    }

    #[test]
    fn closure_runtime_receives_named_bindings() {
        let rt = ClosureSemanticRuntime::new(
            |_name, _value, _, named| named.get("key").cloned().unwrap_or(ParseValue::Nil),
            |_, _, _, _| true,
        );
        let mut named = HashMap::new();
        named.insert("key".to_string(), ParseValue::Text("found".into()));
        let result = rt.invoke_action("get_key", ParseValue::Nil, None, &named);
        assert_eq!(result, ParseValue::Text("found".into()));
    }

    #[test]
    fn passthrough_runtime_is_identity() {
        let rt = ClosureSemanticRuntime::passthrough();
        let v = ParseValue::Number(42);
        assert_eq!(
            rt.invoke_action("noop", v.clone(), None, &HashMap::new()),
            v
        );
        assert!(rt.invoke_predicate("always", &v, None, &HashMap::new()));
    }
}
