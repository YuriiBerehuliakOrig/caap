use serde::{Deserialize, Serialize};

use crate::analysis::analyze_grammar;
use crate::error::ParseError;
use crate::grammar::Grammar;
use crate::types::{ParseCache, ParseValue};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NodeSpec {
    Nil,
    Text(String),
    Number(i64),
    Node {
        name: String,
        children: Vec<NodeSpec>,
    },
    Spanned {
        start: usize,
        end: usize,
        value: Box<NodeSpec>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GrammarCompilation {
    pub rule_count: usize,
    pub root_rule: String,
    pub output: String,
}

pub fn compile_grammar(grammar: &Grammar) -> Result<GrammarCompilation, ParseError> {
    let analysis = analyze_grammar(grammar);
    if !analysis.errors.is_empty() {
        return Err(ParseError::new(
            format!("invalid grammar: {}", analysis.errors.join(", ")),
            0,
            grammar.text.len(),
        ));
    }

    if grammar.rules.is_empty() {
        return Err(ParseError::new(
            "grammar has no rules",
            0,
            grammar.text.len(),
        ));
    }
    let output = format!("compiled:{}", grammar.version);
    Ok(GrammarCompilation {
        rule_count: grammar.rules.len(),
        root_rule: grammar.start_rule.clone(),
        output,
    })
}

pub fn project_value(value: ParseValue, cache: &mut ParseCache) -> NodeSpec {
    let _ = cache; // kept for API compatibility
    match value {
        ParseValue::Nil => NodeSpec::Nil,
        ParseValue::Text(value) => NodeSpec::Text(value),
        ParseValue::Number(value) => NodeSpec::Number(value),
        ParseValue::Node(name, items) => NodeSpec::Node {
            name,
            children: items.into_iter().map(project_value_from_inner).collect(),
        },
        ParseValue::Named(_, inner) => project_value(*inner, cache),
        ParseValue::SpannedValue { value, start, end } => NodeSpec::Spanned {
            start,
            end,
            value: Box::new(project_value_from_inner(*value)),
        },
    }
}

fn project_value_from_inner(value: ParseValue) -> NodeSpec {
    match value {
        ParseValue::Nil => NodeSpec::Nil,
        ParseValue::Text(value) => NodeSpec::Text(value),
        ParseValue::Number(value) => NodeSpec::Number(value),
        ParseValue::Node(name, items) => NodeSpec::Node {
            name,
            children: items.into_iter().map(project_value_from_inner).collect(),
        },
        ParseValue::Named(_, inner) => project_value_from_inner(*inner),
        ParseValue::SpannedValue { value, start, end } => NodeSpec::Spanned {
            start,
            end,
            value: Box::new(project_value_from_inner(*value)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ParseValue;

    #[test]
    fn compile_non_empty_grammar() {
        let grammar = Grammar::new("root <- .");
        let comp = compile_grammar(&grammar).unwrap();
        assert_eq!(comp.rule_count, 1);
    }

    #[test]
    fn compile_rejects_missing_start_rule() {
        let grammar = Grammar::new("a <- .").with_start_rule("root");
        assert!(compile_grammar(&grammar).is_err());
    }

    #[test]
    fn compile_rejects_duplicate_rules() {
        let grammar = Grammar {
            start_rule: "a".to_string(),
            text: "a <- x\na <- y".to_string(),
            metadata: std::collections::HashMap::new(),
            rules: vec![
                crate::grammar::GrammarRule::from_source("a", "x", Vec::new()),
                crate::grammar::GrammarRule::from_source("a", "y", Vec::new()),
            ],
            imports: std::collections::HashMap::new(),
            version: 1,
            state: crate::grammar::GrammarState {
                sealed: false,
                analysis_state: None,
                version: 0,
            },
        };
        assert!(compile_grammar(&grammar).is_err());
    }

    #[test]
    fn project_ast() {
        let cache = &mut ParseCache::default();
        let value = ParseValue::Node("x".to_string(), vec![ParseValue::Text("y".to_string())]);
        let spec = project_value(value, cache);
        match spec {
            NodeSpec::Node { name, children } => {
                assert_eq!(name, "x");
                assert_eq!(children.len(), 1);
            }
            _ => panic!("expected node"),
        }
    }
}
