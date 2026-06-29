//! Static grammar validation: [`validate_grammar`] produces a
//! [`ValidationReport`] of [`ValidationIssue`]s (errors and warnings) found by
//! analysing a grammar without parsing input.

use serde::{Deserialize, Serialize};

use crate::analysis::{analyze_grammar, GrammarAnalysis};
use crate::grammar::Grammar;
use crate::parser_analysis::{has_char_terminal_in_expr, has_token_ref_in_expr};
use crate::parser_imports::{extract_import_aliases_from_expr, metadata_import_targets};

// ── Severity ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// The severity of a [`ValidationIssue`].
pub enum Severity {
    /// A correctness error.
    Error,
    /// A non-fatal warning.
    Warning,
}

impl Severity {
    /// The severity as a lowercase string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── Individual issue ───────────────────────────────────────────────────────

/// A single validation finding attached to a grammar.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ValidationIssue {
    /// The finding message.
    pub message: String,
    /// Error or warning.
    pub severity: Severity,
    /// Optional rule name the issue concerns.
    pub rule: Option<String>,
    /// Optional short machine-readable code (e.g. `"missing_ref"`).
    pub code: Option<String>,
}

impl ValidationIssue {
    fn error(message: impl Into<String>, rule: Option<&str>, code: &str) -> Self {
        Self {
            message: message.into(),
            severity: Severity::Error,
            rule: rule.map(str::to_string),
            code: Some(code.to_string()),
        }
    }

    fn warning(message: impl Into<String>, rule: Option<&str>, code: &str) -> Self {
        Self {
            message: message.into(),
            severity: Severity::Warning,
            rule: rule.map(str::to_string),
            code: Some(code.to_string()),
        }
    }
}

impl std::fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rule_part = self
            .rule
            .as_deref()
            .map(|r| format!(" (rule '{r}')"))
            .unwrap_or_default();
        write!(f, "[{}]{} {}", self.severity, rule_part, self.message)
    }
}

// ── Validation report ──────────────────────────────────────────────────────

/// Aggregated validation results for a single grammar.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
    /// Optional human-readable label for the grammar.
    pub label: Option<String>,
    /// All findings, errors and warnings.
    pub issues: Vec<ValidationIssue>,
    /// The underlying grammar analysis.
    pub analysis: GrammarAnalysis,
}

impl ValidationReport {
    /// Whether the grammar passed (no errors).
    pub fn ok(&self) -> bool {
        !self.issues.iter().any(|i| i.severity == Severity::Error)
    }

    /// Iterate the error-severity issues.
    pub fn errors(&self) -> impl Iterator<Item = &ValidationIssue> {
        self.issues.iter().filter(|i| i.severity == Severity::Error)
    }

    /// Iterate the warning-severity issues.
    pub fn warnings(&self) -> impl Iterator<Item = &ValidationIssue> {
        self.issues
            .iter()
            .filter(|i| i.severity == Severity::Warning)
    }

    /// Number of errors.
    pub fn error_count(&self) -> usize {
        self.errors().count()
    }

    /// Number of warnings.
    pub fn warning_count(&self) -> usize {
        self.warnings().count()
    }
}

impl std::fmt::Display for ValidationReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = self.label.as_deref().unwrap_or("<grammar>");
        let status = if self.ok() { "ok" } else { "invalid" };
        writeln!(
            f,
            "{label}: {status} ({} error(s), {} warning(s))",
            self.error_count(),
            self.warning_count()
        )?;
        for issue in &self.issues {
            writeln!(f, "  {issue}")?;
        }
        Ok(())
    }
}

// ── Entry point ────────────────────────────────────────────────────────────

/// Options controlling validation behaviour.
///
/// Mirrors the `lint`/`strict`/`emit_warnings` parameters of `validate_grammar()`.
#[derive(Clone, Debug, Default)]
pub struct ValidationOptions {
    /// When `true`, all issues are reported as warnings (lint mode).
    pub lint: bool,
    /// When `true`, returns `Err` if there are any errors.
    pub strict: bool,
    /// Optional human-readable label for the report.
    pub label: Option<String>,
}

impl ValidationOptions {
    /// Default validation options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Treat all issues as warnings (lint mode).
    pub fn with_lint(mut self) -> Self {
        self.lint = true;
        self
    }

    /// Return `Err` if any errors are found.
    pub fn with_strict(mut self) -> Self {
        self.strict = true;
        self
    }

    /// Attach a human-readable label.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// Run all static validation checks on `grammar` and return a `ValidationReport`.
pub fn validate_grammar(grammar: &Grammar) -> ValidationReport {
    validate_grammar_with_label(grammar, None)
}

/// Like `validate_grammar` but with an optional human-readable label.
pub fn validate_grammar_with_label(grammar: &Grammar, label: Option<&str>) -> ValidationReport {
    let opts = ValidationOptions {
        label: label.map(str::to_string),
        ..Default::default()
    };
    validate_grammar_with_options(grammar, &opts).unwrap_or_else(|r| *r)
}

/// Full-featured validation entry point.
///
/// Returns `Ok(report)` when there are no errors (or `lint=true`).
/// Returns `Err(report)` when `strict=true` and there are errors.
pub fn validate_grammar_with_options(
    grammar: &Grammar,
    opts: &ValidationOptions,
) -> Result<ValidationReport, Box<ValidationReport>> {
    let analysis = analyze_grammar(grammar);
    let mut issues: Vec<ValidationIssue> = Vec::new();

    check_duplicates(&analysis, &mut issues);
    check_invalid_rule_sources(&analysis, &mut issues);
    check_start_rule(grammar, &analysis, &mut issues);
    check_missing_refs(&analysis, &mut issues);
    check_unreachable(&analysis, &mut issues);
    check_left_recursive(&analysis, &mut issues);
    check_nullable_repetition(&analysis, &mut issues);
    check_param_arity(&analysis, &mut issues);
    check_undeclared_params(&analysis, &mut issues);
    check_unused_params(&analysis, &mut issues);
    check_non_choice_commits(&analysis, &mut issues);
    check_dead_choice_alternatives(&analysis, &mut issues);
    check_prefix_shadowed_alternatives(&analysis, &mut issues);
    check_overlapping_prefixes(&analysis, &mut issues);
    check_unproductive_rules(&analysis, &mut issues);
    check_unused_actions(grammar, &analysis, &mut issues);
    check_grammar_config_metadata_types(grammar, &mut issues);
    check_indentation_external_lexer(grammar, &mut issues);
    check_metadata_owners(grammar, &analysis, &mut issues);
    check_recovery_metadata_keys(grammar, &mut issues);
    check_invalid_rule_prefixes_metadata(grammar, &mut issues);
    check_mixed_terminal_modes(grammar, &mut issues);
    check_import_aliases(grammar, &mut issues);

    // In lint mode, convert all errors to warnings.
    if opts.lint {
        for issue in &mut issues {
            issue.severity = Severity::Warning;
        }
    }

    let report = ValidationReport {
        label: opts.label.clone(),
        issues,
        analysis,
    };

    if opts.strict && !report.ok() {
        Err(Box::new(report))
    } else {
        Ok(report)
    }
}

// ── Individual checks ──────────────────────────────────────────────────────

fn check_duplicates(analysis: &GrammarAnalysis, issues: &mut Vec<ValidationIssue>) {
    for dup in &analysis.duplicates {
        issues.push(ValidationIssue::error(
            format!("rule '{dup}' is defined more than once"),
            Some(dup),
            "duplicate_rule",
        ));
    }
}

fn check_invalid_rule_sources(analysis: &GrammarAnalysis, issues: &mut Vec<ValidationIssue>) {
    for (rule, message) in &analysis.invalid_rules {
        issues.push(ValidationIssue::error(
            format!("rule '{rule}' has invalid source: {message}"),
            Some(rule),
            "invalid_rule_source",
        ));
    }
}

fn check_start_rule(
    grammar: &Grammar,
    analysis: &GrammarAnalysis,
    issues: &mut Vec<ValidationIssue>,
) {
    if !analysis.has_start_rule {
        issues.push(ValidationIssue::error(
            format!("start rule '{}' is not defined", grammar.start_rule),
            None,
            "missing_start_rule",
        ));
    }
}

fn check_missing_refs(analysis: &GrammarAnalysis, issues: &mut Vec<ValidationIssue>) {
    for (owner, target) in &analysis.missing_refs {
        issues.push(ValidationIssue::error(
            format!("rule '{owner}' references undefined rule '{target}'"),
            Some(owner),
            "missing_ref",
        ));
    }
}

fn check_unreachable(analysis: &GrammarAnalysis, issues: &mut Vec<ValidationIssue>) {
    for rule in &analysis.unreachable {
        issues.push(ValidationIssue::warning(
            format!("rule '{rule}' is unreachable from the start rule"),
            Some(rule),
            "unreachable_rule",
        ));
    }
}

fn check_left_recursive(analysis: &GrammarAnalysis, issues: &mut Vec<ValidationIssue>) {
    for rule in &analysis.left_recursive {
        issues.push(ValidationIssue::warning(
            format!("rule '{rule}' is part of a reference cycle (possible left recursion)"),
            Some(rule),
            "left_recursive",
        ));
    }
}

fn check_nullable_repetition(analysis: &GrammarAnalysis, issues: &mut Vec<ValidationIssue>) {
    for (rule, kind) in &analysis.nullable_repetition {
        issues.push(ValidationIssue::warning(
            format!(
                "rule '{rule}' has a potential nullable repetition trap: {kind}, \
                 which may cause an infinite loop"
            ),
            Some(rule),
            "nullable_repetition",
        ));
    }
}

fn check_dead_choice_alternatives(analysis: &GrammarAnalysis, issues: &mut Vec<ValidationIssue>) {
    for (rule, dead, live) in &analysis.dead_choice_alternatives {
        issues.push(ValidationIssue::error(
            format!(
                "rule '{rule}' has dead choice alternative #{dead}; \
                 alternative #{live} already shadows it with identical text"
            ),
            Some(rule),
            "dead_choice_alternative",
        ));
    }
}

fn check_prefix_shadowed_alternatives(
    analysis: &GrammarAnalysis,
    issues: &mut Vec<ValidationIssue>,
) {
    for (rule, dead, live, prefix) in &analysis.prefix_shadowed_choice_alternatives {
        issues.push(ValidationIssue::error(
            format!(
                "rule '{rule}' has choice alternative #{dead} shadowed by prefix \
                 alternative #{live} ('{prefix}')"
            ),
            Some(rule),
            "prefix_shadowed_choice",
        ));
    }
}

fn check_overlapping_prefixes(analysis: &GrammarAnalysis, issues: &mut Vec<ValidationIssue>) {
    for (rule, alt1, alt2, prefix) in &analysis.overlapping_prefixes {
        issues.push(ValidationIssue::warning(
            format!(
                "rule '{rule}' has alternatives #{alt1} and #{alt2} sharing common \
                 prefix '{prefix}'; consider factoring the prefix out for better performance"
            ),
            Some(rule),
            "overlapping_prefix_choice",
        ));
    }
}

fn check_unproductive_rules(analysis: &GrammarAnalysis, issues: &mut Vec<ValidationIssue>) {
    for rule in &analysis.unproductive {
        issues.push(ValidationIssue::error(
            format!("rule '{rule}' is unproductive and cannot match any input"),
            Some(rule),
            "unproductive_rule",
        ));
    }
}

fn check_param_arity(analysis: &GrammarAnalysis, issues: &mut Vec<ValidationIssue>) {
    for m in &analysis.param_arity_mismatches {
        issues.push(ValidationIssue::error(
            format!(
                "rule '{}' calls '{}' with {} argument(s) but '{}' expects {}",
                m.caller, m.callee, m.got, m.callee, m.expected
            ),
            Some(&m.caller),
            "param_arity_mismatch",
        ));
    }
}

fn check_undeclared_params(analysis: &GrammarAnalysis, issues: &mut Vec<ValidationIssue>) {
    for (rule, param) in &analysis.undeclared_params {
        issues.push(ValidationIssue::error(
            format!("rule '{rule}' uses undeclared parameter '${param}'"),
            Some(rule),
            "undeclared_param",
        ));
    }
}

fn check_unused_params(analysis: &GrammarAnalysis, issues: &mut Vec<ValidationIssue>) {
    for (rule, param) in &analysis.unused_params {
        issues.push(ValidationIssue::warning(
            format!("rule '{rule}' declares parameter '{param}' that is never used"),
            Some(rule),
            "unused_param",
        ));
    }
}

fn check_non_choice_commits(analysis: &GrammarAnalysis, issues: &mut Vec<ValidationIssue>) {
    for (rule, kind) in &analysis.non_choice_commits {
        let msg = if kind == "cut" {
            format!("rule '{rule}' uses cut outside a choice; it can abort parsing but cannot prune alternatives")
        } else {
            format!("rule '{rule}' uses eager outside a choice; it escalates failures but cannot prune alternatives")
        };
        issues.push(ValidationIssue::warning(
            msg,
            Some(rule),
            "non_choice_commit",
        ));
    }
}

/// Warn when metadata targets a rule name that doesn't exist in the grammar.
fn check_metadata_owners(
    grammar: &Grammar,
    analysis: &GrammarAnalysis,
    issues: &mut Vec<ValidationIssue>,
) {
    let rule_names: std::collections::HashSet<&str> = analysis
        .reachable
        .iter()
        .chain(analysis.unreachable.iter())
        .map(|s| s.as_str())
        .collect();

    let mut owners: Vec<&str> = grammar.metadata.keys().map(|k| k.as_str()).collect();
    owners.sort_unstable();

    for owner in owners {
        if owner == "__grammar__" {
            continue;
        }
        // Check both reachable/unreachable sets and the full rule list.
        let known = rule_names.contains(owner) || grammar.rules.iter().any(|r| r.name == owner);
        if !known {
            issues.push(ValidationIssue::error(
                format!("metadata targets unknown rule '{owner}'"),
                Some(owner),
                "unknown_metadata_owner",
            ));
        }
    }
}

const RECOVERY_METADATA_KEYS: &[&str] = &["recover_sync_tokens", "recover_sync_regex"];

/// Error when metadata uses unsupported recovery config keys.
fn check_recovery_metadata_keys(grammar: &Grammar, issues: &mut Vec<ValidationIssue>) {
    let mut owners: Vec<&str> = grammar.metadata.keys().map(|k| k.as_str()).collect();
    owners.sort_unstable();

    for owner in owners {
        if let Some(meta) = grammar.metadata.get(owner) {
            let bad_keys: Vec<&str> = RECOVERY_METADATA_KEYS
                .iter()
                .filter(|&&k| meta.contains_key(k))
                .copied()
                .collect();
            if !bad_keys.is_empty() {
                let rendered = bad_keys
                    .iter()
                    .map(|k| format!("'{k}'"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let rule_name = if owner == "__grammar__" {
                    None
                } else {
                    Some(owner)
                };
                issues.push(ValidationIssue::error(
                    format!(
                        "metadata for '{owner}' uses unsupported recovery config key(s) {rendered}; \
                         pass sync_tokens/sync_regex to recover_parse instead"
                    ),
                    rule_name,
                    "unsupported_recovery_metadata",
                ));
            }
        }
    }
}

/// Error when `invalid_rule_prefixes` metadata is not a list of non-empty strings.
fn check_invalid_rule_prefixes_metadata(grammar: &Grammar, issues: &mut Vec<ValidationIssue>) {
    let mut owners: Vec<&str> = grammar.metadata.keys().map(|k| k.as_str()).collect();
    owners.sort_unstable();

    for owner in owners {
        if let Some(meta) = grammar.metadata.get(owner) {
            if let Some(value) = meta.get("invalid_rule_prefixes") {
                let valid = match value {
                    serde_json::Value::Array(items) => items
                        .iter()
                        .all(|item| matches!(item, serde_json::Value::String(s) if !s.is_empty())),
                    _ => false,
                };
                if !valid {
                    let rule_name = if owner == "__grammar__" {
                        None
                    } else {
                        Some(owner)
                    };
                    issues.push(ValidationIssue::error(
                        format!(
                            "metadata for '{owner}' has invalid 'invalid_rule_prefixes'; \
                             expected a non-empty array of non-empty strings"
                        ),
                        rule_name,
                        "invalid_invalid_rule_prefixes",
                    ));
                }
            }
        }
    }
}

fn check_mixed_terminal_modes(grammar: &Grammar, issues: &mut Vec<ValidationIssue>) {
    let mut has_tok = false;
    let mut has_char = false;
    for rule in &grammar.rules {
        if has_token_ref_in_expr(rule.expr()) {
            has_tok = true;
        }
        if has_char_terminal_in_expr(rule.expr()) {
            has_char = true;
        }
        if has_tok && has_char {
            break;
        }
    }
    if has_tok && has_char {
        issues.push(ValidationIssue::error(
            "grammar mixes tok() token-stream refs with char-level terminals (regex, literal, dot); \
             use one mode consistently",
            None,
            "mixed_terminal_modes",
        ));
    }
}

/// Warn when a grammar action entry has no matching rule.
///
/// Equivalent to the `analysis.unused_actions` check.
fn check_unused_actions(
    grammar: &Grammar,
    _analysis: &GrammarAnalysis,
    issues: &mut Vec<ValidationIssue>,
) {
    let rule_names: std::collections::HashSet<&str> =
        grammar.rules.iter().map(|r| r.name.as_str()).collect();
    for action_name in grammar.metadata.keys() {
        if action_name == "__grammar__" {
            continue;
        }
        // Actions stored under a rule name that doesn't exist.
        if !rule_names.contains(action_name.as_str()) {
            // Only report if the metadata key looks like an action owner
            // (i.e. not a known non-rule key).
            issues.push(ValidationIssue::warning(
                format!("metadata entry '{action_name}' targets a non-existent rule"),
                Some(action_name),
                "unused_action",
            ));
        }
    }
}

/// Error when an indentation-aware grammar tries to use an external lexer.
fn check_indentation_external_lexer(grammar: &Grammar, issues: &mut Vec<ValidationIssue>) {
    let grammar_meta = grammar.metadata.get("__grammar__");
    let indentation_on = grammar_meta
        .and_then(|m| m.get("indentation"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let has_external_lexer = grammar_meta
        .and_then(|m| m.get("lexer"))
        .and_then(|v| v.as_str())
        .is_some();
    if indentation_on && has_external_lexer {
        issues.push(ValidationIssue::error(
            "indentation-aware grammars cannot use an external lexer",
            None,
            "indentation_external_lexer",
        ));
    }
}

fn check_grammar_config_metadata_types(grammar: &Grammar, issues: &mut Vec<ValidationIssue>) {
    let Some(grammar_meta) = grammar.metadata.get("__grammar__") else {
        return;
    };

    expect_metadata_string(grammar_meta, "trivia", issues);
    expect_metadata_string_or_null(grammar_meta, "lexer", issues);
    expect_metadata_bool(grammar_meta, "indentation", issues);
    expect_metadata_bool(grammar_meta, "strict_actions", issues);
    expect_metadata_string_array(grammar_meta, "hard_keywords", issues);
    expect_metadata_string_array(grammar_meta, "soft_keywords", issues);
}

fn expect_metadata_string(
    meta: &std::collections::HashMap<String, serde_json::Value>,
    key: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    let Some(value) = meta.get(key) else {
        return;
    };
    if !value.is_string() {
        push_invalid_grammar_metadata_type(issues, key, "string", value);
    }
}

fn expect_metadata_string_or_null(
    meta: &std::collections::HashMap<String, serde_json::Value>,
    key: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    let Some(value) = meta.get(key) else {
        return;
    };
    if !value.is_string() && !value.is_null() {
        push_invalid_grammar_metadata_type(issues, key, "string or null", value);
    }
}

fn expect_metadata_bool(
    meta: &std::collections::HashMap<String, serde_json::Value>,
    key: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    let Some(value) = meta.get(key) else {
        return;
    };
    if !value.is_boolean() {
        push_invalid_grammar_metadata_type(issues, key, "bool", value);
    }
}

fn expect_metadata_string_array(
    meta: &std::collections::HashMap<String, serde_json::Value>,
    key: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    let Some(value) = meta.get(key) else {
        return;
    };
    let valid = value
        .as_array()
        .map(|items| items.iter().all(|item| item.is_string()))
        .unwrap_or(false);
    if !valid {
        push_invalid_grammar_metadata_type(issues, key, "array of strings", value);
    }
}

fn push_invalid_grammar_metadata_type(
    issues: &mut Vec<ValidationIssue>,
    key: &str,
    expected: &'static str,
    value: &serde_json::Value,
) {
    issues.push(ValidationIssue::error(
        format!(
            "__grammar__.{key} metadata must be {expected}, got {}",
            metadata_value_type_name(value)
        ),
        None,
        "invalid_grammar_metadata_type",
    ));
}

fn metadata_value_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn check_import_aliases(grammar: &Grammar, issues: &mut Vec<ValidationIssue>) {
    let mut known: std::collections::HashSet<String> = grammar.imports.keys().cloned().collect();
    for alias in grammar.imports.keys() {
        if alias.is_empty() {
            issues.push(ValidationIssue::error(
                "inline grammar import alias must be non-empty",
                None,
                "invalid_import_alias",
            ));
        }
    }
    match metadata_import_targets(grammar) {
        Ok(imports) => {
            known.extend(imports.into_iter().map(|(alias, _)| alias));
        }
        Err(error) => {
            issues.push(ValidationIssue::error(
                error.message,
                None,
                "invalid_import_metadata",
            ));
        }
    }
    let mut rule_names: Vec<&str> = grammar.rules.iter().map(|r| r.name.as_str()).collect();
    rule_names.sort_unstable();
    for rule in &grammar.rules {
        for alias in extract_import_aliases_from_expr(rule.expr()) {
            if !known.contains(&alias) {
                issues.push(ValidationIssue::error(
                    format!(
                        "rule '{}' references unknown import alias '{alias}'; add it via Grammar::add_import()",
                        rule.name
                    ),
                    Some(&rule.name),
                    "unknown_import_alias",
                ));
            }
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::Grammar;

    fn grammar(start: &str, rules: &[(&str, &str)]) -> Grammar {
        let text = rules
            .iter()
            .map(|(n, s)| format!("{n} <- {s}"))
            .collect::<Vec<_>>()
            .join("\n");
        Grammar::trusted_new(&text).with_start_rule(start)
    }

    #[test]
    fn valid_grammar_reports_ok() {
        let g = grammar("root", &[("root", "'hello'")]);
        let r = validate_grammar(&g);
        assert!(r.ok());
        assert_eq!(r.error_count(), 0);
    }

    #[test]
    fn grammar_try_new_rejects_invalid_rule_source_before_validation() {
        let err = Grammar::try_new("root <- [a").unwrap_err();
        assert!(err.to_string().contains("unterminated character class"));
    }

    #[test]
    fn missing_start_rule_is_an_error() {
        let g = grammar("root", &[("a", "'x'")]);
        let r = validate_grammar(&g);
        assert!(!r.ok());
        assert!(r
            .errors()
            .any(|i| i.code.as_deref() == Some("missing_start_rule")));
    }

    #[test]
    fn duplicate_rule_is_an_error() {
        // Two rules named `a` — duplicates are preserved by the text parser.
        let g = Grammar::trusted_new("a <- 'x'\na <- 'y'").with_start_rule("a");
        let r = validate_grammar(&g);
        assert!(!r.ok());
        assert!(r
            .errors()
            .any(|i| i.code.as_deref() == Some("duplicate_rule")));
    }

    #[test]
    fn missing_ref_is_an_error() {
        let g = grammar("root", &[("root", "undefined_rule")]);
        let r = validate_grammar(&g);
        assert!(!r.ok());
        assert!(r.errors().any(|i| i.code.as_deref() == Some("missing_ref")));
    }

    #[test]
    fn unreachable_rule_is_a_warning() {
        let g = grammar("root", &[("root", "'x'"), ("orphan", "'y'")]);
        let r = validate_grammar(&g);
        assert!(r.ok()); // warnings don't fail
        assert!(r
            .warnings()
            .any(|i| i.code.as_deref() == Some("unreachable_rule")));
    }

    #[test]
    fn cycle_is_a_warning() {
        let g = grammar("a", &[("a", "b"), ("b", "a")]);
        let r = validate_grammar(&g);
        assert!(r
            .warnings()
            .any(|i| i.code.as_deref() == Some("left_recursive")));
    }

    #[test]
    fn nullable_repetition_is_a_warning() {
        // Optional (nullable) inside ZeroOrMore — `('x'?)*`
        let g = grammar("root", &[("root", "('x'?)*")]);
        let r = validate_grammar(&g);
        // The analysis detects nullable repetition and reports it as a warning.
        assert!(
            r.warnings()
                .any(|i| i.code.as_deref() == Some("nullable_repetition")),
            "expected nullable_repetition warning; got: {:?}",
            r.issues
        );
    }

    #[test]
    fn report_display_includes_label() {
        let g = grammar("root", &[("root", "'x'")]);
        let r = validate_grammar_with_label(&g, Some("my_grammar"));
        let text = r.to_string();
        assert!(text.contains("my_grammar"));
    }

    #[test]
    fn severity_display() {
        assert_eq!(Severity::Error.to_string(), "error");
        assert_eq!(Severity::Warning.to_string(), "warning");
    }

    #[test]
    fn issue_display_includes_rule_and_message() {
        let issue = ValidationIssue::error("rule 'x' undefined", Some("root"), "missing_ref");
        let text = issue.to_string();
        assert!(text.contains("error"));
        assert!(text.contains("root"));
    }

    #[test]
    fn unknown_metadata_owner_is_an_error() {
        use crate::mutation::add_rule;
        let mut g = Grammar::trusted_new("root <- 'x'").with_start_rule("root");
        // Add metadata targeting a rule that doesn't exist.
        g.set_metadata_value("ghost_rule", "some_key", serde_json::Value::Null);
        let r = validate_grammar(&g);
        assert!(!r.ok());
        assert!(r
            .errors()
            .any(|i| i.code.as_deref() == Some("unknown_metadata_owner")));
        let _ = add_rule; // suppress unused import warning
    }

    #[test]
    fn known_metadata_owner_does_not_trigger_error() {
        let mut g = Grammar::trusted_new("root <- 'x'").with_start_rule("root");
        g.set_metadata_value("root", "some_key", serde_json::Value::Bool(true));
        let r = validate_grammar(&g);
        assert!(!r
            .errors()
            .any(|i| i.code.as_deref() == Some("unknown_metadata_owner")));
    }

    #[test]
    fn grammar_metadata_owner_is_allowed() {
        let mut g = Grammar::trusted_new("root <- 'x'").with_start_rule("root");
        g.set_metadata_value(
            "__grammar__",
            "version",
            serde_json::Value::Number(1.into()),
        );
        let r = validate_grammar(&g);
        assert!(!r
            .errors()
            .any(|i| i.code.as_deref() == Some("unknown_metadata_owner")));
    }

    #[test]
    fn recovery_metadata_key_triggers_error() {
        let mut g = Grammar::trusted_new("root <- 'x'").with_start_rule("root");
        g.set_metadata_value(
            "__grammar__",
            "recover_sync_tokens",
            serde_json::Value::Array(vec![]),
        );
        let r = validate_grammar(&g);
        assert!(r
            .errors()
            .any(|i| i.code.as_deref() == Some("unsupported_recovery_metadata")));
    }

    #[test]
    fn invalid_rule_prefixes_not_an_array_is_an_error() {
        let mut g = Grammar::trusted_new("root <- 'x'").with_start_rule("root");
        g.set_metadata_value(
            "__grammar__",
            "invalid_rule_prefixes",
            serde_json::Value::String("bad".to_string()),
        );
        let r = validate_grammar(&g);
        assert!(!r.ok());
        assert!(r
            .errors()
            .any(|i| i.code.as_deref() == Some("invalid_invalid_rule_prefixes")));
    }

    #[test]
    fn invalid_rule_prefixes_array_of_strings_is_ok() {
        let mut g = Grammar::trusted_new("root <- 'x'").with_start_rule("root");
        g.set_metadata_value(
            "__grammar__",
            "invalid_rule_prefixes",
            serde_json::Value::Array(vec![serde_json::Value::String("_".to_string())]),
        );
        let r = validate_grammar(&g);
        assert!(!r
            .errors()
            .any(|i| i.code.as_deref() == Some("invalid_invalid_rule_prefixes")));
    }

    #[test]
    fn mixed_terminal_modes_triggers_error() {
        // Grammar that mixes tok() with a literal — should get mixed_terminal_modes error.
        let g = Grammar::trusted_new("start <- tok(NAME) 'hello'").with_start_rule("start");
        let r = validate_grammar(&g);
        assert!(
            r.errors()
                .any(|i| i.code.as_deref() == Some("mixed_terminal_modes")),
            "expected mixed_terminal_modes"
        );
    }

    #[test]
    fn pure_char_grammar_has_no_mixed_terminal_error() {
        let g = grammar("root", &[("root", "'hello' /[a-z]+/")]);
        let r = validate_grammar(&g);
        assert!(!r
            .errors()
            .any(|i| i.code.as_deref() == Some("mixed_terminal_modes")));
    }

    #[test]
    fn pure_token_grammar_has_no_mixed_terminal_error() {
        let g = Grammar::trusted_new("start <- tok(NAME) tok(OP)").with_start_rule("start");
        let r = validate_grammar(&g);
        assert!(!r
            .errors()
            .any(|i| i.code.as_deref() == Some("mixed_terminal_modes")));
    }

    #[test]
    fn unknown_import_alias_triggers_error() {
        let g = Grammar::trusted_new("start <- other::rule").with_start_rule("start");
        let r = validate_grammar(&g);
        assert!(
            r.errors()
                .any(|i| i.code.as_deref() == Some("unknown_import_alias")),
            "expected unknown_import_alias"
        );
    }

    #[test]
    fn known_import_alias_is_accepted() {
        let mut g = Grammar::trusted_new("start <- other::rule").with_start_rule("start");
        let import = Grammar::trusted_new("rule <- 'x'").with_start_rule("rule");
        g.add_import("other", import);
        let r = validate_grammar(&g);
        assert!(!r
            .errors()
            .any(|i| i.code.as_deref() == Some("unknown_import_alias")));
    }

    #[test]
    fn empty_inline_import_alias_is_rejected_at_mutation_boundary() {
        let mut g = Grammar::trusted_new("start <- root").with_start_rule("start");
        let import = Grammar::trusted_new("root <- 'x'").with_start_rule("root");
        let err = g.try_add_import("", import).unwrap_err();
        assert!(
            err.message.contains("import alias must be non-empty"),
            "expected invalid import alias error, got {}",
            err.message
        );
    }

    #[test]
    fn metadata_import_alias_is_accepted() {
        let mut g = Grammar::trusted_new("start <- other::rule").with_start_rule("start");
        g.set_metadata_value(
            "__grammar__",
            "imports",
            serde_json::json!({"other": "registry.other"}),
        );
        let r = validate_grammar(&g);
        assert!(!r
            .errors()
            .any(|i| i.code.as_deref() == Some("unknown_import_alias")));
    }

    #[test]
    fn invalid_import_metadata_is_an_error() {
        let mut g = Grammar::trusted_new("start <- other::rule").with_start_rule("start");
        g.set_metadata_value("__grammar__", "imports", serde_json::json!(["other"]));
        let r = validate_grammar(&g);
        assert!(r
            .errors()
            .any(|i| i.code.as_deref() == Some("invalid_import_metadata")));
    }

    // ── ValidationOptions ─────────────────────────────────────────────────

    #[test]
    fn lint_mode_converts_errors_to_warnings() {
        let g = Grammar::trusted_new("root <- 'x'").with_start_rule("nonexistent");
        let opts = ValidationOptions::new().with_lint();
        let r = validate_grammar_with_options(&g, &opts).unwrap();
        assert!(r.ok(), "lint mode should not produce errors");
        assert!(r
            .warnings()
            .any(|i| i.code.as_deref() == Some("missing_start_rule")));
    }

    #[test]
    fn strict_mode_returns_err_on_errors() {
        let g = grammar("root", &[("a", "'x'")]); // start=root but rule=a
        let opts = ValidationOptions::new().with_strict();
        let result = validate_grammar_with_options(&g, &opts);
        assert!(
            result.is_err(),
            "strict mode should return Err when errors present"
        );
    }

    #[test]
    fn strict_mode_returns_ok_on_valid_grammar() {
        let g = grammar("root", &[("root", "'x'")]);
        let opts = ValidationOptions::new().with_strict();
        assert!(validate_grammar_with_options(&g, &opts).is_ok());
    }

    #[test]
    fn validation_options_with_label() {
        let g = grammar("root", &[("root", "'x'")]);
        let opts = ValidationOptions::new().with_label("my_grammar");
        let r = validate_grammar_with_options(&g, &opts).unwrap();
        assert_eq!(r.label.as_deref(), Some("my_grammar"));
    }

    #[test]
    fn indentation_external_lexer_is_an_error() {
        let mut g = Grammar::trusted_new("root <- 'x'").with_start_rule("root");
        g.set_metadata_value("__grammar__", "indentation", serde_json::Value::Bool(true));
        g.set_metadata_value(
            "__grammar__",
            "lexer",
            serde_json::Value::String("mylex".to_string()),
        );
        let r = validate_grammar(&g);
        assert!(r
            .errors()
            .any(|i| i.code.as_deref() == Some("indentation_external_lexer")));
    }

    #[test]
    fn invalid_grammar_config_metadata_types_are_errors() {
        let mut g = Grammar::trusted_new("root <- 'x'").with_start_rule("root");
        g.set_metadata_value("__grammar__", "trivia", serde_json::json!(false));
        g.set_metadata_value("__grammar__", "lexer", serde_json::json!(123));
        g.set_metadata_value("__grammar__", "indentation", serde_json::json!("off"));
        g.set_metadata_value("__grammar__", "strict_actions", serde_json::json!("false"));
        g.set_metadata_value("__grammar__", "hard_keywords", serde_json::json!(["if", 1]));
        g.set_metadata_value("__grammar__", "soft_keywords", serde_json::json!("match"));

        let r = validate_grammar(&g);
        let invalid_type_errors = r
            .errors()
            .filter(|i| i.code.as_deref() == Some("invalid_grammar_metadata_type"))
            .count();
        assert_eq!(invalid_type_errors, 6);
        assert!(r.errors().any(|i| i
            .message
            .contains("__grammar__.indentation metadata must be bool")));
    }

    #[test]
    fn null_lexer_metadata_does_not_count_as_external_lexer() {
        let mut g = Grammar::trusted_new("root <- 'x'").with_start_rule("root");
        g.set_metadata_value("__grammar__", "indentation", serde_json::Value::Bool(true));
        g.set_metadata_value("__grammar__", "lexer", serde_json::Value::Null);

        let r = validate_grammar(&g);
        assert!(!r
            .errors()
            .any(|i| i.code.as_deref() == Some("indentation_external_lexer")));
    }
}
