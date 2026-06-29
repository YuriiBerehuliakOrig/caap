//! Per-rule analysis summaries ([`RuleScanSummary`], [`RuleIssueSummary`])
//! cached so unchanged rules need not be re-scanned on every grammar analysis.

use serde::{Deserialize, Serialize};

// ── Per-rule scan / issue summaries ───────────────────────────────────────

/// Structural information extracted from a single rule body.
/// Cached so unchanged rules need not be re-parsed on every analysis.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuleScanSummary {
    /// All rule names referenced in the body.
    pub refs: Vec<String>,
    /// `(caller, callee)` pairs where `callee` is not defined in the grammar.
    pub missing_refs: Vec<(String, String)>,
    /// `(caller, callee, expected, got)` param-arity mismatches.
    pub param_arity_mismatches: Vec<(String, String, usize, usize)>,
    /// `(rule, param)` pairs: param used in body but not declared.
    pub undeclared_params: Vec<(String, String)>,
    /// `(rule, param)` pairs: declared param never used.
    pub unused_params: Vec<(String, String)>,
}

/// Analysis issues detected in a single rule body.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuleIssueSummary {
    /// `(rule, detail)` — a repetition over a nullable body.
    pub nullable_repetition: Vec<(String, String)>,
    /// `(rule, alt_index, shadowing_index)` — an unreachable alternative.
    pub dead_choice_alternatives: Vec<(String, usize, usize)>,
    /// `(rule, alt_index, shadowing_index, prefix)` — a prefix-shadowed alternative.
    pub prefix_shadowed_choice_alternatives: Vec<(String, usize, usize, String)>,
    /// `(rule, detail)` — a `~` cut outside a choice.
    pub non_choice_commits: Vec<(String, String)>,
    /// `(rule, alt_a, alt_b, prefix)` — alternatives sharing a literal prefix.
    pub overlapping_prefixes: Vec<(String, usize, usize, String)>,
}
