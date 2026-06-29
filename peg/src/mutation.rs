//! In-place grammar mutation ([`add_rule`], [`replace_rule`], …), the
//! [`GrammarMutation`] description type, and [`diff_grammars`] for comparing two
//! grammar versions.

use serde::{Deserialize, Serialize};

use crate::error::ParseError;
use crate::grammar::{Grammar, GrammarRule};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// The kind of edit a [`GrammarMutation`] performs.
pub enum MutationKind {
    /// Add a new rule.
    AddRule,
    /// Replace an existing rule's source.
    ReplaceRule,
    /// Remove a rule.
    RemoveRule,
    /// Change the grammar's start rule.
    SetStartRule,
    /// Replace the whole grammar text.
    SetText,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// Why a grammar mutation was rejected.
pub enum MutationError {
    /// The grammar is sealed against edits.
    GrammarSealed,
    /// A rule with that name already exists.
    DuplicateRule,
    /// The named rule does not exist.
    MissingRule,
    /// A rule name was empty.
    EmptyName,
    /// Required rule source was absent.
    MissingSource,
    /// The requested start rule does not exist.
    MissingStartRule,
    /// Attempted to remove the current start rule.
    ProtectedStartRule,
    /// Rule source failed to parse.
    InvalidRuleSource(ParseError),
    /// The grammar version counter overflowed.
    VersionOverflow(ParseError),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// A described grammar edit: a kind plus the affected rule name and source.
pub struct GrammarMutation {
    /// The kind of edit.
    pub kind: MutationKind,
    /// The target rule name, where applicable.
    pub name: Option<String>,
    /// New rule/grammar source, where applicable.
    pub source: Option<String>,
}

/// Add a new rule `name` with `source`.
pub fn add_rule(grammar: &mut Grammar, name: &str, source: &str) -> Result<(), MutationError> {
    apply(grammar, MutationKind::AddRule, name, Some(source))
}

/// Replace rule `name`'s source.
pub fn replace_rule(grammar: &mut Grammar, name: &str, source: &str) -> Result<(), MutationError> {
    apply(grammar, MutationKind::ReplaceRule, name, Some(source))
}

/// Remove rule `name` (refuses the current start rule).
pub fn remove_rule(grammar: &mut Grammar, name: &str) -> Result<bool, MutationError> {
    if grammar.is_sealed() {
        return Err(MutationError::GrammarSealed);
    }
    if name == grammar.start_rule {
        return Err(MutationError::ProtectedStartRule);
    }
    grammar
        .try_remove_rule(name)
        .map_err(MutationError::VersionOverflow)
}

/// Set the grammar's start rule to an existing rule.
pub fn set_start_rule(grammar: &mut Grammar, name: &str) -> Result<(), MutationError> {
    if grammar.is_sealed() {
        return Err(MutationError::GrammarSealed);
    }
    if name.trim().is_empty() {
        return Err(MutationError::EmptyName);
    }
    if grammar.get_rule(name).is_none() {
        return Err(MutationError::MissingStartRule);
    }
    grammar
        .bump_version()
        .map_err(MutationError::VersionOverflow)?;
    grammar.start_rule = name.to_string();
    grammar.clear_analysis_cache();
    Ok(())
}

/// Apply a mutation of `kind` to `grammar`.
pub fn apply(
    grammar: &mut Grammar,
    kind: MutationKind,
    name: &str,
    source: Option<&str>,
) -> Result<(), MutationError> {
    if grammar.is_sealed() {
        return Err(MutationError::GrammarSealed);
    }
    if kind != MutationKind::SetText && name.trim().is_empty() {
        return Err(MutationError::EmptyName);
    }

    match kind {
        MutationKind::AddRule => {
            if grammar.get_rule(name).is_some() {
                return Err(MutationError::DuplicateRule);
            }
            let source = source.ok_or(MutationError::MissingSource)?;
            let rule =
                GrammarRule::try_from_source(name.to_string(), source.to_string(), Vec::new())
                    .map_err(MutationError::InvalidRuleSource)?;
            grammar
                .bump_version()
                .map_err(MutationError::VersionOverflow)?;
            grammar.rules.push(rule);
            grammar.text = super::grammar::rules_to_text(&grammar.rules);
            grammar.clear_analysis_cache();
            Ok(())
        }
        MutationKind::ReplaceRule => {
            if let Some(index) = grammar.rules.iter().position(|rule| rule.name == name) {
                let source = source.ok_or(MutationError::MissingSource)?;
                let rule = GrammarRule::try_from_source(
                    name.to_string(),
                    source.to_string(),
                    grammar.rules[index].params.clone(),
                )
                .map_err(MutationError::InvalidRuleSource)?;
                grammar
                    .bump_version()
                    .map_err(MutationError::VersionOverflow)?;
                grammar.rules[index] = rule;
                grammar.text = super::grammar::rules_to_text(&grammar.rules);
                grammar.clear_analysis_cache();
                return Ok(());
            }
            Err(MutationError::MissingRule)
        }
        MutationKind::SetText => {
            let text = source.ok_or(MutationError::MissingSource)?.to_string();
            let rules = super::grammar::try_parse_rules_from_text(&text)
                .map_err(MutationError::InvalidRuleSource)?;
            grammar
                .bump_version()
                .map_err(MutationError::VersionOverflow)?;
            grammar.text = text;
            grammar.rules = rules;
            grammar.clear_analysis_cache();
            Ok(())
        }
        MutationKind::SetStartRule => set_start_rule(grammar, name),
        MutationKind::RemoveRule => {
            if remove_rule(grammar, name)? {
                Ok(())
            } else {
                Err(MutationError::MissingRule)
            }
        }
    }
}

// ── Grammar diff ───────────────────────────────────────────────────────────

/// What changed between two grammar versions.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct GrammarDiff {
    /// Names of newly added rules.
    pub added_rules: Vec<String>,
    /// Names of removed rules.
    pub removed_rules: Vec<String>,
    /// Rules whose body source changed (structural diff).
    pub changed_rules: Vec<String>,
    /// Rules whose parameter list changed.
    #[serde(default)]
    pub changed_params: Vec<String>,
    /// Rules whose associated metadata changed.
    #[serde(default)]
    pub changed_metadata: Vec<String>,
    /// Whether the start rule changed.
    pub start_changed: bool,
    /// Whether the `__grammar__` metadata entry changed.
    #[serde(default)]
    pub grammar_metadata_changed: bool,
    /// Whether grammar-level metadata changed (subset of changed_metadata).
    #[serde(default)]
    pub metadata_changed: bool,
    /// Whether `hard_keywords` option changed.
    #[serde(default)]
    pub hard_keywords_changed: bool,
    /// Whether `soft_keywords` option changed.
    #[serde(default)]
    pub soft_keywords_changed: bool,
    /// Whether trivia/indentation skip config changed (mirrors skip_config_changed).
    #[serde(default)]
    pub skip_config_changed: bool,
    /// Whether lexer or strict_actions config changed (mirrors runtime_config_changed).
    #[serde(default)]
    pub runtime_config_changed: bool,
}

impl GrammarDiff {
    /// Whether any rule was added, removed, or changed.
    pub fn has_rule_changes(&self) -> bool {
        !self.added_rules.is_empty()
            || !self.removed_rules.is_empty()
            || !self.changed_rules.is_empty()
    }

    /// Whether the diff records no changes at all.
    pub fn is_empty(&self) -> bool {
        !self.has_rule_changes()
            && !self.start_changed
            && !self.metadata_changed
            && !self.grammar_metadata_changed
            && self.changed_params.is_empty()
            && self.changed_metadata.is_empty()
    }
}

/// The outcome of a committed mutation transaction.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// The result of a committed mutation transaction.
pub struct MutationOutcome {
    /// The resulting grammar.
    pub grammar: Grammar,
    /// What changed relative to the base.
    pub diff: GrammarDiff,
    /// Whether the transaction was committed.
    pub committed: bool,
}

/// Compare two grammars and describe what changed.
pub fn diff_grammars(base: &Grammar, target: &Grammar) -> GrammarDiff {
    use std::collections::{HashMap, HashSet};

    // ── Rule-level diff ────────────────────────────────────────────────────
    let base_rule_map: HashMap<&str, (&str, &[String])> = base
        .rules
        .iter()
        .map(|r| (r.name.as_str(), (r.source.as_str(), r.params.as_slice())))
        .collect();
    let target_rule_map: HashMap<&str, (&str, &[String])> = target
        .rules
        .iter()
        .map(|r| (r.name.as_str(), (r.source.as_str(), r.params.as_slice())))
        .collect();

    let base_names: HashSet<&str> = base_rule_map.keys().copied().collect();
    let target_names: HashSet<&str> = target_rule_map.keys().copied().collect();

    let mut added_rules: Vec<String> = target_names
        .difference(&base_names)
        .map(|s| s.to_string())
        .collect();
    added_rules.sort_unstable();

    let mut removed_rules: Vec<String> = base_names
        .difference(&target_names)
        .map(|s| s.to_string())
        .collect();
    removed_rules.sort_unstable();

    let shared: HashSet<&str> = base_names.intersection(&target_names).copied().collect();

    let mut changed_rules: Vec<String> = shared
        .iter()
        .filter(|&&name| base_rule_map[name].0 != target_rule_map[name].0)
        .map(|s| s.to_string())
        .collect();
    changed_rules.sort_unstable();

    let mut changed_params: Vec<String> = shared
        .iter()
        .filter(|&&name| base_rule_map[name].1 != target_rule_map[name].1)
        .map(|s| s.to_string())
        .collect();
    changed_params.sort_unstable();

    // ── Metadata diff ──────────────────────────────────────────────────────
    let all_meta_owners: HashSet<&str> = base
        .metadata
        .keys()
        .chain(target.metadata.keys())
        .map(|k| k.as_str())
        .collect();

    let mut changed_metadata: Vec<String> = all_meta_owners
        .iter()
        .filter(|&&owner| {
            let base_sig = grammar_signature_of_metadata(base.metadata.get(owner));
            let target_sig = grammar_signature_of_metadata(target.metadata.get(owner));
            base_sig != target_sig
        })
        .map(|s| s.to_string())
        .collect();
    changed_metadata.sort_unstable();

    let grammar_metadata_changed = changed_metadata.contains(&"__grammar__".to_string());

    // ── Keyword option diffs ───────────────────────────────────────────────
    let base_meta = base.metadata.get("__grammar__");
    let target_meta = target.metadata.get("__grammar__");

    let hard_keywords_changed = meta_keyword_signature(base_meta, "hard_keywords")
        != meta_keyword_signature(target_meta, "hard_keywords");
    let soft_keywords_changed = meta_keyword_signature(base_meta, "soft_keywords")
        != meta_keyword_signature(target_meta, "soft_keywords");

    let start_changed = base.start_rule != target.start_rule;

    // metadata_changed = any metadata entry differs.
    let metadata_changed = !changed_metadata.is_empty();

    // skip_config_changed: trivia or indentation changed in __grammar__ metadata.
    let skip_config_changed = {
        let base_g = base.metadata.get("__grammar__");
        let target_g = target.metadata.get("__grammar__");
        let trivia_changed =
            meta_field_signature(base_g, "trivia") != meta_field_signature(target_g, "trivia");
        let indent_changed = meta_field_signature(base_g, "indentation")
            != meta_field_signature(target_g, "indentation");
        trivia_changed || indent_changed
    };

    // runtime_config_changed: lexer or strict_actions changed.
    let runtime_config_changed = {
        let base_g = base.metadata.get("__grammar__");
        let target_g = target.metadata.get("__grammar__");
        let lexer_changed =
            meta_field_signature(base_g, "lexer") != meta_field_signature(target_g, "lexer");
        let strict_changed = meta_field_signature(base_g, "strict_actions")
            != meta_field_signature(target_g, "strict_actions");
        lexer_changed || strict_changed
    };

    GrammarDiff {
        added_rules,
        removed_rules,
        changed_rules,
        changed_params,
        changed_metadata,
        start_changed,
        grammar_metadata_changed,
        metadata_changed,
        hard_keywords_changed,
        soft_keywords_changed,
        skip_config_changed,
        runtime_config_changed,
    }
}

fn grammar_signature_of_metadata(
    meta: Option<&std::collections::HashMap<String, serde_json::Value>>,
) -> String {
    match meta {
        None => String::new(),
        Some(m) => {
            let mut pairs: Vec<String> = m.iter().map(|(k, v)| format!("{k}={v}")).collect();
            pairs.sort_unstable();
            pairs.join(";")
        }
    }
}

fn meta_field_signature(
    meta: Option<&std::collections::HashMap<String, serde_json::Value>>,
    key: &str,
) -> Option<String> {
    meta.and_then(|m| m.get(key)).map(value_signature)
}

fn meta_keyword_signature(
    meta: Option<&std::collections::HashMap<String, serde_json::Value>>,
    key: &str,
) -> Option<String> {
    let value = meta.and_then(|m| m.get(key))?;
    match value {
        serde_json::Value::Array(items) if items.iter().all(|item| item.as_str().is_some()) => {
            let mut words: Vec<&str> = items.iter().filter_map(|item| item.as_str()).collect();
            words.sort_unstable();
            Some(format!("strings:[{}]", words.join(",")))
        }
        _ => Some(value_signature(value)),
    }
}

fn value_signature(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(v) => format!("bool:{v}"),
        serde_json::Value::Number(v) => format!("number:{v}"),
        serde_json::Value::String(v) => format!("string:{v}"),
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(value_signature).collect();
            format!("array:[{}]", parts.join(","))
        }
        serde_json::Value::Object(map) => {
            let mut pairs: Vec<String> = map
                .iter()
                .map(|(key, value)| format!("{key}:{}", value_signature(value)))
                .collect();
            pairs.sort_unstable();
            format!("object:{{{}}}", pairs.join(","))
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::Grammar;

    #[test]
    fn add_and_remove_rule_lifecycle() {
        let mut grammar = Grammar::trusted_new("a <- [a]");
        assert!(grammar.remove_rule("a"));
        assert_eq!(grammar.rule_count(), 0);
        add_rule(&mut grammar, "b", "[b]").unwrap();
        assert!(grammar.get_rule("b").is_some());
        assert!(matches!(
            add_rule(&mut grammar, "b", "[b]"),
            Err(MutationError::DuplicateRule)
        ));
    }

    #[test]
    fn add_rule_rejects_invalid_source_without_mutating() {
        let mut grammar = Grammar::trusted_new("a <- [a]");
        let version = grammar.version;
        let text = grammar.text.clone();
        let err = add_rule(&mut grammar, "b", "[b").expect_err("invalid rule source is rejected");
        let MutationError::InvalidRuleSource(parse_error) = err else {
            panic!("expected InvalidRuleSource");
        };
        assert!(parse_error.message.contains("unterminated character class"));
        assert!(grammar.get_rule("b").is_none());
        assert_eq!(grammar.version, version);
        assert_eq!(grammar.text, text);
    }

    #[test]
    fn replace_rule_rejects_invalid_source_without_mutating() {
        let mut grammar = Grammar::trusted_new("a <- [a]");
        let version = grammar.version;
        let text = grammar.text.clone();
        let err =
            replace_rule(&mut grammar, "a", "[a").expect_err("invalid rule source is rejected");
        assert!(matches!(err, MutationError::InvalidRuleSource(_)));
        assert_eq!(grammar.get_rule("a").unwrap().source, "[a]");
        assert_eq!(grammar.version, version);
        assert_eq!(grammar.text, text);
    }

    #[test]
    fn set_text_rejects_malformed_rule_line_without_mutating() {
        let mut grammar = Grammar::trusted_new("a <- [a]");
        let version = grammar.version;
        let text = grammar.text.clone();
        let err = apply(&mut grammar, MutationKind::SetText, "", Some("bad line"))
            .expect_err("malformed rule line is rejected");
        assert!(matches!(err, MutationError::InvalidRuleSource(_)));
        assert_eq!(grammar.version, version);
        assert_eq!(grammar.text, text);
    }

    #[test]
    fn source_mutations_require_explicit_source() {
        let mut grammar = Grammar::trusted_new("a <- [a]");
        assert!(matches!(
            apply(&mut grammar, MutationKind::AddRule, "b", None),
            Err(MutationError::MissingSource)
        ));
        assert!(grammar.get_rule("b").is_none());

        assert!(matches!(
            apply(&mut grammar, MutationKind::ReplaceRule, "a", None),
            Err(MutationError::MissingSource)
        ));
        assert_eq!(grammar.get_rule("a").unwrap().source, "[a]");

        assert!(matches!(
            apply(&mut grammar, MutationKind::SetText, "", None),
            Err(MutationError::MissingSource)
        ));
        assert_eq!(grammar.text, "a <- [a]");
    }

    #[test]
    fn cannot_modify_sealed_grammar() {
        let mut grammar = Grammar::trusted_new("a <- [a]");
        grammar.seal();
        assert!(matches!(
            add_rule(&mut grammar, "b", "[b]"),
            Err(MutationError::GrammarSealed)
        ));
    }

    #[test]
    fn cannot_remove_start_rule() {
        let mut grammar = Grammar::trusted_new("start <- [a]");
        assert!(matches!(set_start_rule(&mut grammar, "start"), Ok(())));
        assert!(matches!(
            remove_rule(&mut grammar, "start"),
            Err(MutationError::ProtectedStartRule)
        ));
    }

    #[test]
    fn set_start_rule_requires_rule_exist() {
        let mut grammar = Grammar::trusted_new("start <- [a]");
        assert!(matches!(
            set_start_rule(&mut grammar, "missing"),
            Err(MutationError::MissingStartRule)
        ));
    }

    #[test]
    fn diff_grammars_detects_added_rule() {
        let base = Grammar::trusted_new("a <- 'x'").with_start_rule("a");
        let mut target = base.clone();
        crate::mutation::add_rule(&mut target, "b", "'y'").unwrap();
        let diff = diff_grammars(&base, &target);
        assert_eq!(diff.added_rules, vec!["b"]);
        assert!(diff.removed_rules.is_empty());
        assert!(diff.has_rule_changes());
    }

    #[test]
    fn diff_grammars_detects_removed_rule() {
        let base = Grammar::trusted_new("a <- 'x'\nb <- 'y'").with_start_rule("a");
        let target = Grammar::trusted_new("a <- 'x'").with_start_rule("a");
        let diff = diff_grammars(&base, &target);
        assert_eq!(diff.removed_rules, vec!["b"]);
    }

    #[test]
    fn diff_grammars_detects_changed_rule() {
        let base = Grammar::trusted_new("a <- 'x'").with_start_rule("a");
        let mut target = base.clone();
        crate::mutation::replace_rule(&mut target, "a", "'z'").unwrap();
        let diff = diff_grammars(&base, &target);
        assert_eq!(diff.changed_rules, vec!["a"]);
    }

    #[test]
    fn diff_grammars_detects_start_rule_change() {
        let base = Grammar::trusted_new("a <- 'x'\nb <- 'y'").with_start_rule("a");
        let target = Grammar::trusted_new("a <- 'x'\nb <- 'y'").with_start_rule("b");
        let diff = diff_grammars(&base, &target);
        assert!(diff.start_changed);
    }

    #[test]
    fn diff_grammars_empty_for_identical_grammars() {
        let g = Grammar::trusted_new("a <- 'x'").with_start_rule("a");
        let diff = diff_grammars(&g, &g);
        assert!(diff.is_empty());
    }

    #[test]
    fn diff_grammars_detects_runtime_metadata_value_changes() {
        let base = Grammar::trusted_new("a <- 'x'").with_start_rule("a");

        let mut strict_target = base.clone();
        strict_target.set_metadata_value("__grammar__", "strict_actions", serde_json::json!(false));
        let diff = diff_grammars(&base, &strict_target);
        assert!(diff.runtime_config_changed);

        let mut lexer_target = base.clone();
        lexer_target.set_metadata_value("__grammar__", "lexer", serde_json::json!(false));
        let diff = diff_grammars(&base, &lexer_target);
        assert!(diff.runtime_config_changed);
    }

    #[test]
    fn diff_grammars_detects_skip_metadata_value_changes() {
        let base = Grammar::trusted_new("a <- 'x'").with_start_rule("a");

        let mut trivia_target = base.clone();
        trivia_target.set_metadata_value("__grammar__", "trivia", serde_json::json!(false));
        let diff = diff_grammars(&base, &trivia_target);
        assert!(diff.skip_config_changed);

        let mut indent_target = base.clone();
        indent_target.set_metadata_value("__grammar__", "indentation", serde_json::json!(true));
        let diff = diff_grammars(&base, &indent_target);
        assert!(diff.skip_config_changed);
    }

    #[test]
    fn diff_grammars_keeps_keyword_order_semantic_but_detects_malformed_values() {
        let mut base = Grammar::trusted_new("a <- 'x'").with_start_rule("a");
        base.set_metadata_value(
            "__grammar__",
            "hard_keywords",
            serde_json::json!(["if", "else"]),
        );

        let mut reordered = Grammar::trusted_new("a <- 'x'").with_start_rule("a");
        reordered.set_metadata_value(
            "__grammar__",
            "hard_keywords",
            serde_json::json!(["else", "if"]),
        );
        let diff = diff_grammars(&base, &reordered);
        assert!(!diff.hard_keywords_changed);

        let mut malformed = Grammar::trusted_new("a <- 'x'").with_start_rule("a");
        malformed.set_metadata_value("__grammar__", "hard_keywords", serde_json::json!(["if", 1]));
        let diff = diff_grammars(&base, &malformed);
        assert!(diff.hard_keywords_changed);
    }
}
