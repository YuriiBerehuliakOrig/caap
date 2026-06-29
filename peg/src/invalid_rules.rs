//! Explicit invalid-rule filtering policy (mirrors `peg.invalid_rules`).

use serde_json::Value;

use crate::error::ParseError;
use crate::grammar::Grammar;

pub const DEFAULT_INVALID_RULE_PREFIXES: &[&str] = &["invalid_"];

/// Normalise caller-supplied prefix list; `None` yields the default prefix.
pub fn normalize_invalid_rule_prefixes(
    value: Option<&[String]>,
) -> Result<Vec<String>, ParseError> {
    match value {
        None => Ok(DEFAULT_INVALID_RULE_PREFIXES
            .iter()
            .map(|s| s.to_string())
            .collect()),
        Some(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                if item.is_empty() {
                    return Err(invalid_rule_prefix_error(
                        "invalid_rule_prefixes must contain only non-empty strings",
                    ));
                }
                out.push(item.clone());
            }
            Ok(out)
        }
    }
}

/// Read `invalid_rule_prefixes` from grammar metadata (`__grammar__` section).
pub fn normalize_metadata_invalid_rule_prefixes(
    value: Option<&Value>,
) -> Result<Vec<String>, ParseError> {
    match value {
        None => normalize_invalid_rule_prefixes(None),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Some(s) = item.as_str() else {
                    return Err(invalid_rule_prefix_error(
                        "invalid_rule_prefixes metadata must be a sequence of non-empty strings",
                    ));
                };
                if s.is_empty() {
                    return Err(invalid_rule_prefix_error(
                        "invalid_rule_prefixes metadata must contain only non-empty strings",
                    ));
                }
                out.push(s.to_string());
            }
            Ok(out)
        }
        Some(_) => Err(invalid_rule_prefix_error(
            "invalid_rule_prefixes metadata must be a sequence of non-empty strings",
        )),
    }
}

fn invalid_rule_prefix_error(message: impl Into<String>) -> ParseError {
    ParseError::new(message, 0, 0)
}

#[derive(Clone, Debug)]
pub struct InvalidRulePolicy {
    pub include_invalid_rules: bool,
    pub prefixes: Vec<String>,
}

impl InvalidRulePolicy {
    pub fn resolve(
        grammar: &Grammar,
        include_invalid_rules: bool,
        invalid_rule_prefixes: Option<&[String]>,
    ) -> Result<Self, ParseError> {
        let prefixes = if let Some(prefixes) = invalid_rule_prefixes {
            normalize_invalid_rule_prefixes(Some(prefixes))?
        } else if let Some(section) = grammar.metadata.get("__grammar__") {
            normalize_metadata_invalid_rule_prefixes(section.get("invalid_rule_prefixes"))?
        } else {
            normalize_invalid_rule_prefixes(None)?
        };

        Ok(Self {
            include_invalid_rules,
            prefixes,
        })
    }

    pub fn matches(&self, rule_name: &str) -> bool {
        if self.prefixes.is_empty() {
            return false;
        }
        self.prefixes
            .iter()
            .any(|prefix| rule_name.starts_with(prefix))
    }

    pub fn excludes(&self, rule_name: &str) -> bool {
        if self.include_invalid_rules {
            return false;
        }
        self.matches(rule_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_prefixes_include_invalid_underscore() {
        let p = normalize_invalid_rule_prefixes(None).unwrap();
        assert_eq!(p, vec!["invalid_".to_string()]);
    }

    #[test]
    fn empty_prefix_list_disables_filtering() {
        let p = normalize_invalid_rule_prefixes(Some(&[])).unwrap();
        assert!(p.is_empty());
    }
}
