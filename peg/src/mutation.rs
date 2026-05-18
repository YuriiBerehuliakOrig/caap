use serde::{Deserialize, Serialize};

use crate::grammar::{Grammar, GrammarRule};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MutationKind {
    AddRule,
    ReplaceRule,
    RemoveRule,
    SetStartRule,
    SetText,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MutationError {
    GrammarSealed,
    DuplicateRule,
    MissingRule,
    EmptyName,
    MissingStartRule,
    ProtectedStartRule,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GrammarMutation {
    pub kind: MutationKind,
    pub name: Option<String>,
    pub source: Option<String>,
}

pub fn add_rule(grammar: &mut Grammar, name: &str, source: &str) -> Result<(), MutationError> {
    apply(grammar, MutationKind::AddRule, name, Some(source))
}

pub fn replace_rule(grammar: &mut Grammar, name: &str, source: &str) -> Result<(), MutationError> {
    apply(grammar, MutationKind::ReplaceRule, name, Some(source))
}

pub fn remove_rule(grammar: &mut Grammar, name: &str) -> Result<bool, MutationError> {
    if grammar.is_sealed() {
        return Err(MutationError::GrammarSealed);
    }
    if name == grammar.start_rule {
        return Err(MutationError::ProtectedStartRule);
    }
    Ok(grammar.remove_rule(name))
}

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
    grammar.start_rule = name.to_string();
    grammar.version = grammar.version.saturating_add(1);
    grammar.clear_analysis_cache();
    Ok(())
}

pub fn apply(
    grammar: &mut Grammar,
    kind: MutationKind,
    name: &str,
    source: Option<&str>,
) -> Result<(), MutationError> {
    if grammar.is_sealed() {
        return Err(MutationError::GrammarSealed);
    }
    if name.trim().is_empty() {
        return Err(MutationError::EmptyName);
    }

    match kind {
        MutationKind::AddRule => {
            if grammar.get_rule(name).is_some() {
                return Err(MutationError::DuplicateRule);
            }
            grammar.rules.push(GrammarRule::from_source(
                name.to_string(),
                source.unwrap_or("").to_string(),
                Vec::new(),
            ));
            grammar.version = grammar.version.saturating_add(1);
            grammar.text = super::grammar::rules_to_text(&grammar.rules);
            grammar.clear_analysis_cache();
            Ok(())
        }
        MutationKind::ReplaceRule => {
            if let Some(rule) = grammar.rules.iter_mut().find(|rule| rule.name == name) {
                rule.set_source(source.unwrap_or(""));
                grammar.version = grammar.version.saturating_add(1);
                grammar.text = super::grammar::rules_to_text(&grammar.rules);
                grammar.clear_analysis_cache();
                return Ok(());
            }
            Err(MutationError::MissingRule)
        }
        MutationKind::SetText => {
            grammar.text = source.unwrap_or("").to_string();
            let rules = super::grammar::parse_rules_from_text(&grammar.text);
            grammar.rules = rules;
            grammar.version = grammar.version.saturating_add(1);
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
    pub added_rules: Vec<String>,
    pub removed_rules: Vec<String>,
    /// Rules whose body source changed (structural diff).
    pub changed_rules: Vec<String>,
    /// Rules whose parameter list changed.
    #[serde(default)]
    pub changed_params: Vec<String>,
    /// Rules whose associated metadata changed.
    #[serde(default)]
    pub changed_metadata: Vec<String>,
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
    /// Whether trivia/indentation skip config changed (mirrors Python skip_config_changed).
    #[serde(default)]
    pub skip_config_changed: bool,
    /// Whether lexer or strict_actions config changed (mirrors Python runtime_config_changed).
    #[serde(default)]
    pub runtime_config_changed: bool,
}

impl GrammarDiff {
    pub fn has_rule_changes(&self) -> bool {
        !self.added_rules.is_empty()
            || !self.removed_rules.is_empty()
            || !self.changed_rules.is_empty()
    }

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
pub struct MutationOutcome {
    pub grammar: Grammar,
    pub diff: GrammarDiff,
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

    let hard_keywords_changed = meta_keyword_list(base_meta, "hard_keywords")
        != meta_keyword_list(target_meta, "hard_keywords");
    let soft_keywords_changed = meta_keyword_list(base_meta, "soft_keywords")
        != meta_keyword_list(target_meta, "soft_keywords");

    let start_changed = base.start_rule != target.start_rule;

    // metadata_changed = any metadata entry differs (mirrors Python's `skip_config_changed` + `grammar_metadata_changed`)
    let metadata_changed = !changed_metadata.is_empty();

    // skip_config_changed: trivia or indentation changed in __grammar__ metadata.
    let skip_config_changed = {
        let base_g = base.metadata.get("__grammar__");
        let target_g = target.metadata.get("__grammar__");
        let trivia_changed = meta_str_opt(base_g, "trivia") != meta_str_opt(target_g, "trivia");
        let indent_changed = meta_bool(base_g, "indentation") != meta_bool(target_g, "indentation");
        trivia_changed || indent_changed
    };

    // runtime_config_changed: lexer or strict_actions changed.
    let runtime_config_changed = {
        let base_g = base.metadata.get("__grammar__");
        let target_g = target.metadata.get("__grammar__");
        let lexer_changed = meta_str_opt(base_g, "lexer") != meta_str_opt(target_g, "lexer");
        let strict_changed =
            meta_bool(base_g, "strict_actions") != meta_bool(target_g, "strict_actions");
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

fn meta_str_opt(
    meta: Option<&std::collections::HashMap<String, serde_json::Value>>,
    key: &str,
) -> Option<String> {
    meta.and_then(|m| m.get(key))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn meta_bool(
    meta: Option<&std::collections::HashMap<String, serde_json::Value>>,
    key: &str,
) -> bool {
    meta.and_then(|m| m.get(key))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn meta_keyword_list(
    meta: Option<&std::collections::HashMap<String, serde_json::Value>>,
    key: &str,
) -> Vec<String> {
    meta.and_then(|m| m.get(key))
        .and_then(|v| v.as_array())
        .map(|arr| {
            let mut kws: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            kws.sort_unstable();
            kws
        })
        .unwrap_or_default()
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::Grammar;

    #[test]
    fn add_and_remove_rule_lifecycle() {
        let mut grammar = Grammar::new("a <- [a]");
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
    fn cannot_modify_sealed_grammar() {
        let mut grammar = Grammar::new("a <- [a]");
        grammar.seal();
        assert!(matches!(
            add_rule(&mut grammar, "b", "[b]"),
            Err(MutationError::GrammarSealed)
        ));
    }

    #[test]
    fn cannot_remove_start_rule() {
        let mut grammar = Grammar::new("start <- [a]");
        assert!(matches!(set_start_rule(&mut grammar, "start"), Ok(())));
        assert!(matches!(
            remove_rule(&mut grammar, "start"),
            Err(MutationError::ProtectedStartRule)
        ));
    }

    #[test]
    fn set_start_rule_requires_rule_exist() {
        let mut grammar = Grammar::new("start <- [a]");
        assert!(matches!(
            set_start_rule(&mut grammar, "missing"),
            Err(MutationError::MissingStartRule)
        ));
    }

    #[test]
    fn diff_grammars_detects_added_rule() {
        let base = Grammar::new("a <- 'x'").with_start_rule("a");
        let mut target = base.clone();
        crate::mutation::add_rule(&mut target, "b", "'y'").unwrap();
        let diff = diff_grammars(&base, &target);
        assert_eq!(diff.added_rules, vec!["b"]);
        assert!(diff.removed_rules.is_empty());
        assert!(diff.has_rule_changes());
    }

    #[test]
    fn diff_grammars_detects_removed_rule() {
        let base = Grammar::new("a <- 'x'\nb <- 'y'").with_start_rule("a");
        let target = Grammar::new("a <- 'x'").with_start_rule("a");
        let diff = diff_grammars(&base, &target);
        assert_eq!(diff.removed_rules, vec!["b"]);
    }

    #[test]
    fn diff_grammars_detects_changed_rule() {
        let base = Grammar::new("a <- 'x'").with_start_rule("a");
        let mut target = base.clone();
        crate::mutation::replace_rule(&mut target, "a", "'z'").unwrap();
        let diff = diff_grammars(&base, &target);
        assert_eq!(diff.changed_rules, vec!["a"]);
    }

    #[test]
    fn diff_grammars_detects_start_rule_change() {
        let base = Grammar::new("a <- 'x'\nb <- 'y'").with_start_rule("a");
        let target = Grammar::new("a <- 'x'\nb <- 'y'").with_start_rule("b");
        let diff = diff_grammars(&base, &target);
        assert!(diff.start_changed);
    }

    #[test]
    fn diff_grammars_empty_for_identical_grammars() {
        let g = Grammar::new("a <- 'x'").with_start_rule("a");
        let diff = diff_grammars(&g, &g);
        assert!(diff.is_empty());
    }
}
