//! Static grammar analysis: the rule-reference graph, reachability, left-
//! recursion SCCs, and arity checks, cached on the grammar via
//! [`analyze_and_store`] / [`analyze_cached_grammar`].

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::analysis_rule::{RuleIssueSummary, RuleScanSummary};
use crate::expr::PegExpr;
use crate::grammar::Grammar;
use crate::graph_ops::{build_reverse_graph, find_left_recursive_sccs};
use crate::parser_analysis::{
    collect_dead_choice_alts_from_expr, collect_nullable_repetitions_from_expr,
    collect_overlapping_prefixes_from_expr, collect_prefix_shadowed_alts_from_expr,
    extract_calls_from_expr, extract_left_refs_from_expr, extract_params_used_from_expr,
    extract_refs_from_expr, fixed_text_in_expr, has_bare_commit_in_expr, is_nullable_in_expr,
    is_nullable_with_rules_in_expr, is_productive_with_rules_in_expr,
};

// ── Analysis result ────────────────────────────────────────────────────────

/// A parametric rule call with the wrong number of arguments.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParamArityMismatch {
    /// Rule that contains the mismatched call.
    pub caller: String,
    /// Rule being called.
    pub callee: String,
    /// Number of parameters declared by the callee.
    pub expected: usize,
    /// Number of arguments supplied by the caller.
    pub got: usize,
}

/// Full static analysis of a grammar's rule graph.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GrammarAnalysis {
    /// Number of rules in the grammar.
    pub rule_count: usize,
    /// Whether the configured start rule exists.
    pub has_start_rule: bool,
    /// Whether two rules share a name.
    pub has_duplicate_rule_names: bool,

    /// Outgoing rule references for each rule: `refs[rule] = {referenced_names}`.
    pub refs: HashMap<String, Vec<String>>,

    /// Rules reachable from the start rule (BFS over `refs`).
    pub reachable: Vec<String>,

    /// Rules defined in the grammar but never reachable from the start rule.
    pub unreachable: Vec<String>,

    /// Rule names whose definitions reference an unknown rule.
    pub missing_refs: Vec<(String, String)>,

    /// Rule names that belong to a reference cycle (left-recursive heuristic).
    pub left_recursive: Vec<String>,

    /// Duplicate rule names.
    pub duplicates: Vec<String>,

    /// Parametric rule calls whose argument count doesn't match the callee's
    /// declared parameter list.
    #[serde(default)]
    pub param_arity_mismatches: Vec<ParamArityMismatch>,

    /// `(rule, param)` pairs where a `$param` is used but not declared.
    #[serde(default)]
    pub undeclared_params: Vec<(String, String)>,

    /// `(rule, param)` pairs where a declared parameter is never used in the body.
    #[serde(default)]
    pub unused_params: Vec<(String, String)>,

    /// `(rule, kind)` where `kind` is `"cut"` or `"eager"` and the commit
    /// appears outside any Choice context (so it cannot prune alternatives).
    #[serde(default)]
    pub non_choice_commits: Vec<(String, String)>,

    /// `(rule, kind)` where the rule uses a repetition over a nullable expression.
    #[serde(default)]
    pub nullable_repetition: Vec<(String, String)>,

    /// `(rule, dead_alt_index, live_alt_index)` — 1-based; dead alternative is
    /// shadowed by an earlier alternative with identical fixed text.
    #[serde(default)]
    pub dead_choice_alternatives: Vec<(String, usize, usize)>,

    /// `(rule, dead_alt_index, live_alt_index, prefix)` — dead alternative whose
    /// fixed text starts with the live alternative's shorter fixed text.
    #[serde(default)]
    pub prefix_shadowed_choice_alternatives: Vec<(String, usize, usize, String)>,

    /// `(rule, alt1_index, alt2_index, common_prefix)` — two alternatives share a
    /// common prefix of ≥3 characters; factoring would improve performance.
    #[serde(default)]
    pub overlapping_prefixes: Vec<(String, usize, usize, String)>,

    /// Rules that can never successfully match any input string.
    #[serde(default)]
    pub unproductive: Vec<String>,

    /// `(rule, message)` pairs for rules whose stored expression could not be
    /// parsed from source.
    #[serde(default)]
    pub invalid_rules: Vec<(String, String)>,

    /// Strongly connected components that form left-recursive cycles.
    /// Each inner `Vec` is a sorted list of rule names in the same SCC.
    /// Computed from the left-refs graph (only refs reachable from the left edge).
    #[serde(default)]
    pub left_recursive_sccs: Vec<Vec<String>>,

    /// Non-fatal analysis warnings.
    pub warnings: Vec<String>,
    /// Analysis errors that make the grammar invalid.
    pub errors: Vec<String>,
}

// ── Cached analysis state ──────────────────────────────────────────────────

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// Cached analysis plus the signatures used to detect when it is stale.
pub struct GrammarAnalysisState {
    /// The cached analysis.
    pub analysis: GrammarAnalysis,
    /// Grammar version the analysis was computed at.
    pub analysis_version: u64,
    /// FNV-1a hash of each rule's source text at analysis time.
    #[serde(default)]
    pub rule_signatures: HashMap<String, u64>,
    /// `rule_name → declared parameter names` at analysis time.
    #[serde(default)]
    pub param_signatures: HashMap<String, Vec<String>>,
    /// Reverse-reference graph: `reverse_refs[b]` = sorted rules that reference `b`.
    #[serde(default)]
    pub reverse_refs: HashMap<String, Vec<String>>,
    /// Left-reference graph: `left_refs[a]` = rules reachable from the left edge of `a`.
    #[serde(default)]
    pub left_refs: HashMap<String, Vec<String>>,
    /// Reverse left-reference graph.
    #[serde(default)]
    pub reverse_left_refs: HashMap<String, Vec<String>>,
    /// Nullable result per rule (`true` = can match empty input).
    #[serde(default)]
    pub nullable_rules: HashMap<String, bool>,
    /// Productive result per rule (`true` = can match some input).
    #[serde(default)]
    pub productive_rules: HashMap<String, bool>,
    /// Fixed-text result per rule (`Some("text")` = always produces that text).
    #[serde(default)]
    pub fixed_rules: HashMap<String, Option<String>>,
    /// Per-rule structural scan results (cached for incremental re-use).
    #[serde(default)]
    pub rule_scans: HashMap<String, RuleScanSummary>,
    /// Per-rule analysis issue results (cached for incremental re-use).
    #[serde(default)]
    pub node_issues: HashMap<String, RuleIssueSummary>,
    /// Minimum proven character advance per LR-growth step for each LR rule.
    #[serde(default)]
    pub lr_min_step: HashMap<String, usize>,
}

// ── Core analysis ──────────────────────────────────────────────────────────

/// Build the outgoing-reference map for `grammar`.
///
/// When a grammar contains duplicate rule names (which `GrammarRule::try_from_source`
/// allows but the parser compile step rejects at runtime), analysis silently uses
/// the first occurrence so that static checks can still report all other issues
/// in the grammar.
fn build_refs_map(grammar: &Grammar) -> HashMap<String, Vec<String>> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut refs: HashMap<String, Vec<String>> = HashMap::new();
    for rule in &grammar.rules {
        if seen.insert(rule.name.clone()) {
            refs.insert(rule.name.clone(), extract_refs_from_expr(rule.expr()));
        }
    }
    refs
}

/// Perform a full static analysis of `grammar` and return the results.
pub fn analyze_grammar(grammar: &Grammar) -> GrammarAnalysis {
    let refs = build_refs_map(grammar);
    analyze_grammar_with_refs(grammar, refs)
}

/// Core analysis — identical to `analyze_grammar` but uses a pre-built `refs` map so
/// the incremental path can skip re-parsing unchanged rules' sources.
fn analyze_grammar_with_refs(
    grammar: &Grammar,
    refs: HashMap<String, Vec<String>>,
) -> GrammarAnalysis {
    let rule_names: HashSet<String> = grammar.rules.iter().map(|r| r.name.clone()).collect();

    // ── Duplicate detection ────────────────────────────────────────────────
    let duplicates = find_duplicates(grammar);

    // ── Start-rule presence ────────────────────────────────────────────────
    let has_start_rule = rule_names.contains(&grammar.start_rule);

    // ── Missing references ─────────────────────────────────────────────────
    let mut missing_refs: Vec<(String, String)> = Vec::new();
    for (owner, targets) in &refs {
        for target in targets {
            if !rule_names.contains(target) {
                missing_refs.push((owner.clone(), target.clone()));
            }
        }
    }
    missing_refs.sort_unstable();

    // ── Reachability (BFS from start rule) ────────────────────────────────
    let (reachable, unreachable) = if has_start_rule {
        bfs_reachable(&grammar.start_rule, &refs, &rule_names)
    } else {
        (vec![], rule_names.iter().cloned().collect())
    };

    // ── Cycle / left-recursion detection ──────────────────────────────────
    let left_recursive = detect_cycles(&refs);

    // ── Parameter arity / undeclared / unused ────────────────────────────
    // Build a map: rule_name → declared param count (only for parametric rules).
    let mut rule_param_counts: HashMap<String, usize> = HashMap::new();
    let mut rule_param_names: HashMap<String, Vec<String>> = HashMap::new();
    for rule in &grammar.rules {
        if !rule.params.is_empty() {
            rule_param_counts.insert(rule.name.clone(), rule.params.len());
            rule_param_names.insert(rule.name.clone(), rule.params.clone());
        }
    }

    let mut param_arity_mismatches: Vec<ParamArityMismatch> = Vec::new();
    let mut undeclared_params: Vec<(String, String)> = Vec::new();
    let mut unused_params: Vec<(String, String)> = Vec::new();
    let mut non_choice_commits: Vec<(String, String)> = Vec::new();

    for rule in &grammar.rules {
        let calls = extract_calls_from_expr(rule.expr());
        for (callee, got) in calls {
            if let Some(&expected) = rule_param_counts.get(&callee) {
                if got != expected {
                    param_arity_mismatches.push(ParamArityMismatch {
                        caller: rule.name.clone(),
                        callee,
                        expected,
                        got,
                    });
                }
            }
        }

        if !rule.params.is_empty() {
            let used_params = extract_params_used_from_expr(rule.expr());
            let declared: HashSet<&str> = rule.params.iter().map(|s| s.as_str()).collect();
            let used: HashSet<&str> = used_params.iter().map(|s| s.as_str()).collect();
            for p in &used_params {
                if !declared.contains(p.as_str()) {
                    undeclared_params.push((rule.name.clone(), p.clone()));
                }
            }
            for p in &rule.params {
                if !used.contains(p.as_str()) {
                    unused_params.push((rule.name.clone(), p.clone()));
                }
            }
        }

        if let Some(kind) = has_bare_commit_in_expr(rule.expr()) {
            non_choice_commits.push((rule.name.clone(), kind.to_string()));
        }
    }

    param_arity_mismatches
        .sort_unstable_by(|a, b| a.caller.cmp(&b.caller).then(a.callee.cmp(&b.callee)));
    undeclared_params.sort_unstable();
    unused_params.sort_unstable();
    non_choice_commits.sort_unstable();

    // ── Fixed-text computation (fixed-point) ──────────────────────────────
    // A rule has a "fixed text" if every input that matches it produces the same
    // output string. Iterate until convergence.
    let mut fixed_rules: HashMap<String, Option<String>> = HashMap::new();
    for _ in 0..64 {
        let mut changed = false;
        for rule in &grammar.rules {
            if fixed_rules.contains_key(&rule.name) {
                continue;
            }
            if let Some(text) = fixed_text_in_expr(rule.expr(), &fixed_rules) {
                fixed_rules.insert(rule.name.clone(), Some(text));
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // ── Productive-rule computation (fixed-point) ─────────────────────────
    // A rule is "productive" if it can match on at least some input.
    let mut productive_rules: HashMap<String, bool> = HashMap::new();
    for _ in 0..64 {
        let mut changed = false;
        for rule in &grammar.rules {
            if productive_rules.get(&rule.name).copied() == Some(true) {
                continue;
            }
            if is_productive_with_rules_in_expr(rule.expr(), &productive_rules) {
                productive_rules.insert(rule.name.clone(), true);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // ── Nullable rules (with context) ─────────────────────────────────────
    let mut nullable_set: HashSet<String> = HashSet::new();
    for _ in 0..64 {
        let mut changed = false;
        for rule in &grammar.rules {
            if nullable_set.contains(&rule.name) {
                continue;
            }
            if is_nullable_with_rules_in_expr(rule.expr(), &nullable_set) {
                nullable_set.insert(rule.name.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // ── Left-reference graph and SCC-based left-recursion ─────────────────
    // Build the left-refs graph: only refs reachable from the left edge (more
    // precise than the full refs graph used by `detect_cycles`).
    let mut left_refs_graph: HashMap<String, HashSet<String>> = HashMap::new();
    for rule in &grammar.rules {
        let lr: HashSet<String> = extract_left_refs_from_expr(rule.expr(), &nullable_set)
            .into_iter()
            .filter(|r| rule_names.contains(r))
            .collect();
        left_refs_graph.insert(rule.name.clone(), lr);
    }
    let left_recursive_sccs = find_left_recursive_sccs(&left_refs_graph);

    // ── Node-level analysis (nullable_repetition, dead alts, etc.) ────────
    let mut nullable_repetition: Vec<(String, String)> = Vec::new();
    let mut dead_choice_alternatives: Vec<(String, usize, usize)> = Vec::new();
    let mut prefix_shadowed_choice_alternatives: Vec<(String, usize, usize, String)> = Vec::new();
    let mut overlapping_prefixes: Vec<(String, usize, usize, String)> = Vec::new();

    for rule in &grammar.rules {
        for kind in collect_nullable_repetitions_from_expr(rule.expr(), &nullable_set) {
            nullable_repetition.push((rule.name.clone(), kind));
        }
        for (dead, live) in collect_dead_choice_alts_from_expr(rule.expr(), &fixed_rules) {
            dead_choice_alternatives.push((rule.name.clone(), dead, live));
        }
        for (dead, live, prefix) in
            collect_prefix_shadowed_alts_from_expr(rule.expr(), &fixed_rules)
        {
            prefix_shadowed_choice_alternatives.push((rule.name.clone(), dead, live, prefix));
        }
        for (alt1, alt2, prefix) in
            collect_overlapping_prefixes_from_expr(rule.expr(), &fixed_rules)
        {
            overlapping_prefixes.push((rule.name.clone(), alt1, alt2, prefix));
        }
    }

    nullable_repetition.sort_unstable();
    dead_choice_alternatives.sort_unstable();
    prefix_shadowed_choice_alternatives.sort_unstable();
    overlapping_prefixes.sort_unstable();

    // ── Unproductive rules ────────────────────────────────────────────────
    let mut unproductive: Vec<String> = grammar
        .rules
        .iter()
        .filter(|r| !productive_rules.get(&r.name).copied().unwrap_or(false))
        .map(|r| r.name.clone())
        .collect();
    unproductive.sort_unstable();

    let mut invalid_rules: Vec<(String, String)> = grammar
        .rules
        .iter()
        .filter_map(|rule| match rule.expr() {
            PegExpr::Invalid(message) => Some((rule.name.clone(), message.clone())),
            _ => None,
        })
        .collect();
    invalid_rules.sort_unstable();

    // ── Errors & warnings ─────────────────────────────────────────────────
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for dup in &duplicates {
        errors.push(format!("duplicate rule: {dup}"));
    }
    if !has_start_rule {
        errors.push(format!("missing start rule: {}", grammar.start_rule));
    }
    for (owner, target) in &missing_refs {
        errors.push(format!("rule '{owner}' references unknown rule '{target}'"));
    }
    for m in &param_arity_mismatches {
        errors.push(format!(
            "rule '{}' calls '{}' with {} arg(s) but '{}' expects {}",
            m.caller, m.callee, m.got, m.callee, m.expected
        ));
    }
    for (owner, param) in &undeclared_params {
        errors.push(format!(
            "rule '{owner}' uses undeclared parameter '${param}'"
        ));
    }
    for lr in &left_recursive {
        warnings.push(format!("left-recursive rule: {lr}"));
    }
    for rule in &unreachable {
        warnings.push(format!("unreachable rule: {rule}"));
    }
    for (owner, param) in &unused_params {
        warnings.push(format!(
            "rule '{owner}' declares unused parameter '{param}'"
        ));
    }
    for (owner, kind) in &non_choice_commits {
        warnings.push(format!(
            "rule '{owner}' uses {kind} outside a choice; it can abort parsing but cannot prune alternatives"
        ));
    }
    for (owner, kind) in &nullable_repetition {
        warnings.push(format!(
            "rule '{owner}' has nullable repetition trap: {kind}"
        ));
    }
    for (owner, dead, live) in &dead_choice_alternatives {
        errors.push(format!(
            "rule '{owner}': alternative #{dead} is dead (shadowed by #{live})"
        ));
    }
    for (owner, dead, live, prefix) in &prefix_shadowed_choice_alternatives {
        errors.push(format!("rule '{owner}': alternative #{dead} is shadowed by prefix alternative #{live} ('{prefix}')"));
    }
    for (owner, alt1, alt2, prefix) in &overlapping_prefixes {
        warnings.push(format!(
            "rule '{owner}': alternatives #{alt1} and #{alt2} share prefix '{prefix}'"
        ));
    }
    for rule in &unproductive {
        errors.push(format!(
            "rule '{rule}' is unproductive (cannot match any input)"
        ));
    }
    for (rule, message) in &invalid_rules {
        errors.push(format!("rule '{rule}' has invalid source: {message}"));
    }

    GrammarAnalysis {
        rule_count: grammar.rules.len(),
        has_start_rule,
        has_duplicate_rule_names: !duplicates.is_empty(),
        refs,
        reachable,
        unreachable,
        missing_refs,
        left_recursive,
        left_recursive_sccs,
        duplicates,
        param_arity_mismatches,
        undeclared_params,
        unused_params,
        non_choice_commits,
        nullable_repetition,
        dead_choice_alternatives,
        prefix_shadowed_choice_alternatives,
        overlapping_prefixes,
        unproductive,
        invalid_rules,
        warnings,
        errors,
    }
}

// ── Incremental analysis helpers ───────────────────────────────────────────

fn fnv1a_hash(s: &str) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for byte in s.bytes() {
        h ^= byte as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

fn compute_rule_signatures(grammar: &Grammar) -> HashMap<String, u64> {
    grammar
        .rules
        .iter()
        .map(|r| (r.name.clone(), fnv1a_hash(&r.source)))
        .collect()
}

fn build_reverse_refs(refs: &HashMap<String, Vec<String>>) -> HashMap<String, Vec<String>> {
    let mut reverse: HashMap<String, Vec<String>> = HashMap::new();
    for (rule, targets) in refs {
        for target in targets {
            reverse
                .entry(target.clone())
                .or_default()
                .push(rule.clone());
        }
    }
    for v in reverse.values_mut() {
        v.sort_unstable();
        v.dedup();
    }
    reverse
}

fn transitive_dependents(
    changed: &HashSet<String>,
    reverse_refs: &HashMap<String, Vec<String>>,
) -> HashSet<String> {
    let mut affected = changed.clone();
    let mut queue: VecDeque<String> = changed.iter().cloned().collect();
    while let Some(name) = queue.pop_front() {
        if let Some(dependents) = reverse_refs.get(&name) {
            for dep in dependents {
                if affected.insert(dep.clone()) {
                    queue.push_back(dep.clone());
                }
            }
        }
    }
    affected
}

// ── Cache-aware analysis ───────────────────────────────────────────────────

/// Analyse `grammar`, reusing `previous` results for rules whose source is
/// unchanged.
pub fn analyze_cached_grammar(
    grammar: &Grammar,
    previous: Option<&GrammarAnalysisState>,
) -> GrammarAnalysisState {
    let current_sigs = compute_rule_signatures(grammar);

    // Short-circuit: same version AND same per-rule signatures → nothing changed.
    if let Some(prev) = previous {
        if prev.analysis_version == grammar.version && prev.rule_signatures == current_sigs {
            return prev.clone();
        }
    }

    let rule_names: HashSet<String> = grammar.rules.iter().map(|r| r.name.clone()).collect();

    // Determine which rules have new/changed sources.
    let changed: HashSet<String> = {
        let mut c = HashSet::new();
        if let Some(prev) = previous {
            for (name, sig) in &current_sigs {
                if prev.rule_signatures.get(name) != Some(sig) {
                    c.insert(name.clone());
                }
            }
            // Rules removed from the grammar also count as changed.
            for name in prev.rule_signatures.keys() {
                if !rule_names.contains(name) {
                    c.insert(name.clone());
                }
            }
        } else {
            // No previous state: every rule is "changed".
            c = rule_names.clone();
        }
        c
    };

    // Compute the transitive closure of rules affected by the changes.
    let affected = if let Some(prev) = previous {
        transitive_dependents(&changed, &prev.reverse_refs)
    } else {
        rule_names.clone()
    };

    // Build the refs map: re-extract for affected rules, reuse cache for unchanged.
    let refs: HashMap<String, Vec<String>> = {
        let prev_refs = previous.map(|p| &p.analysis.refs);
        let mut r: HashMap<String, Vec<String>> = HashMap::new();
        let mut seen: HashSet<String> = HashSet::new();
        for rule in &grammar.rules {
            if !seen.insert(rule.name.clone()) {
                continue; // duplicate rule — first occurrence wins
            }
            if affected.contains(&rule.name) {
                r.insert(rule.name.clone(), extract_refs_from_expr(rule.expr()));
            } else if let Some(cached) = prev_refs.and_then(|pr| pr.get(&rule.name)) {
                r.insert(rule.name.clone(), cached.clone());
            } else {
                r.insert(rule.name.clone(), extract_refs_from_expr(rule.expr()));
            }
        }
        r
    };

    let reverse_refs = build_reverse_refs(&refs);
    let analysis = analyze_grammar_with_refs(grammar, refs);

    // ── Left-refs graph for incremental SCC refresh ────────────────────────
    // Re-compute the nullable set needed to extract left-edge references.
    let mut nullable_for_lr: HashSet<String> = HashSet::new();
    for _ in 0..64 {
        let mut changed = false;
        for rule in &grammar.rules {
            if nullable_for_lr.contains(&rule.name) {
                continue;
            }
            if is_nullable_with_rules_in_expr(rule.expr(), &nullable_for_lr) {
                nullable_for_lr.insert(rule.name.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let defined_rule_names: HashSet<String> = analysis.refs.keys().cloned().collect();
    let left_refs: HashMap<String, Vec<String>> = {
        let mut seen: HashSet<String> = HashSet::new();
        grammar
            .rules
            .iter()
            .filter(|r| seen.insert(r.name.clone()))
            .map(|rule| {
                let lr = extract_left_refs_from_expr(rule.expr(), &nullable_for_lr)
                    .into_iter()
                    .filter(|r| defined_rule_names.contains(r))
                    .collect();
                (rule.name.clone(), lr)
            })
            .collect()
    };

    let left_refs_hs: HashMap<String, HashSet<String>> = left_refs
        .iter()
        .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
        .collect();
    let reverse_left_refs: HashMap<String, Vec<String>> = build_reverse_graph(&left_refs_hs)
        .into_iter()
        .map(|(k, mut v)| {
            let mut vv: Vec<String> = v.drain().collect();
            vv.sort_unstable();
            (k, vv)
        })
        .collect();

    GrammarAnalysisState {
        analysis,
        analysis_version: grammar.version,
        rule_signatures: current_sigs,
        param_signatures: grammar
            .rules
            .iter()
            .map(|r| (r.name.clone(), r.params.clone()))
            .collect(),
        reverse_refs,
        left_refs,
        reverse_left_refs,
        nullable_rules: HashMap::new(),
        productive_rules: HashMap::new(),
        fixed_rules: HashMap::new(),
        rule_scans: HashMap::new(),
        node_issues: HashMap::new(),
        lr_min_step: HashMap::new(),
    }
}

/// Analyse `grammar` and store the result in its cached analysis state.
pub fn analyze_and_store(grammar: &mut Grammar) -> GrammarAnalysis {
    let state = analyze_cached_grammar(grammar, grammar.state.analysis_state.as_ref());
    let analysis = state.analysis.clone();
    grammar.state.analysis_state = Some(state);
    grammar.state.version = grammar.version;
    analysis
}

// ── Nullability helpers (exported for validation use) ─────────────────────

/// Determine which rules in the grammar can match without consuming input.
///
/// Uses a fixed-point iteration so that indirect nullability through other rules is
/// captured, capped at 64 iterations to guarantee termination.
pub fn compute_nullable_rules(grammar: &Grammar) -> HashSet<String> {
    // Bootstrap with structural analysis (no ref-following).
    let mut nullable: HashSet<String> = grammar
        .rules
        .iter()
        .filter(|r| is_nullable_in_expr(r.expr()))
        .map(|r| r.name.clone())
        .collect();

    // Fixed-point: a rule becomes nullable when its source becomes nullable once
    // we substitute nullable decisions for Ref nodes.
    for _ in 0..64 {
        let mut changed = false;
        for rule in &grammar.rules {
            if nullable.contains(&rule.name) {
                continue;
            }
            if is_nullable_with_rules_in_expr(rule.expr(), &nullable) {
                nullable.insert(rule.name.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    nullable
}

/// Render a GraphViz DOT representation of rule references for debugging.
pub fn grammar_to_dot(grammar: &Grammar) -> String {
    let analysis = analyze_grammar(grammar);
    let mut lines = vec![
        "digraph grammar {".to_string(),
        "    rankdir=LR;".to_string(),
    ];
    let mut rule_names: Vec<&str> = analysis.refs.keys().map(|s| s.as_str()).collect();
    rule_names.sort_unstable();
    for src in &rule_names {
        let targets = &analysis.refs[*src];
        if targets.is_empty() {
            lines.push(format!("    \"{src}\";"));
        } else {
            let mut sorted_targets = targets.clone();
            sorted_targets.sort_unstable();
            for dst in &sorted_targets {
                lines.push(format!("    \"{src}\" -> \"{dst}\";"));
            }
        }
    }
    lines.push("}".to_string());
    lines.join("\n")
}

// ── Private helpers ────────────────────────────────────────────────────────

fn find_duplicates(grammar: &Grammar) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut dup_seen = HashSet::new();
    let mut dups = Vec::new();
    for rule in &grammar.rules {
        if !seen.insert(&rule.name) && dup_seen.insert(&rule.name) {
            dups.push(rule.name.clone());
        }
    }
    dups.sort_unstable();
    dups
}

fn bfs_reachable(
    start: &str,
    refs: &HashMap<String, Vec<String>>,
    all_rules: &HashSet<String>,
) -> (Vec<String>, Vec<String>) {
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(start.to_string());

    while let Some(rule) = queue.pop_front() {
        if !visited.insert(rule.clone()) {
            continue;
        }
        if let Some(targets) = refs.get(&rule) {
            for t in targets {
                if !visited.contains(t) {
                    queue.push_back(t.clone());
                }
            }
        }
    }

    let mut reachable: Vec<String> = visited.iter().cloned().collect();
    reachable.sort_unstable();

    let mut unreachable: Vec<String> = all_rules
        .iter()
        .filter(|r| !visited.contains(*r))
        .cloned()
        .collect();
    unreachable.sort_unstable();

    (reachable, unreachable)
}

/// Detect nodes that are part of reference cycles using iterative DFS.
///
/// This over-approximates left recursion (it catches all cycles, not only
/// left-most ones), which is safe—grammars with cycles always warrant a warning.
fn detect_cycles(refs: &HashMap<String, Vec<String>>) -> Vec<String> {
    enum Frame {
        Enter(String),
        Leave(String),
    }

    let mut in_cycle: HashSet<String> = HashSet::new();
    let mut permanently_visited: HashSet<String> = HashSet::new();

    for start in refs.keys() {
        if permanently_visited.contains(start) {
            continue;
        }

        let mut stack: Vec<Frame> = vec![Frame::Enter(start.clone())];
        let mut path: Vec<String> = Vec::new();
        let mut path_set: HashSet<String> = HashSet::new();

        while let Some(frame) = stack.pop() {
            match frame {
                Frame::Enter(name) => {
                    if permanently_visited.contains(&name) {
                        continue;
                    }
                    if path_set.contains(&name) {
                        // Back-edge: everything from `name`'s first occurrence to
                        // the current tip is part of a cycle.
                        let cycle_start = path.iter().position(|n| n == &name).unwrap_or(0);
                        for n in &path[cycle_start..] {
                            in_cycle.insert(n.clone());
                        }
                        in_cycle.insert(name.clone());
                        continue;
                    }

                    path.push(name.clone());
                    path_set.insert(name.clone());
                    stack.push(Frame::Leave(name.clone()));

                    if let Some(children) = refs.get(&name) {
                        for child in children {
                            stack.push(Frame::Enter(child.clone()));
                        }
                    }
                }
                Frame::Leave(name) => {
                    path.pop();
                    path_set.remove(&name);
                    permanently_visited.insert(name);
                }
            }
        }
    }

    let mut result: Vec<String> = in_cycle.into_iter().collect();
    result.sort_unstable();
    result
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::{Grammar, GrammarRule, GrammarState};
    use std::collections::HashMap;

    fn make_grammar(start: &str, rules: &[(&str, &str)]) -> Grammar {
        let text = rules
            .iter()
            .map(|(n, s)| format!("{n} <- {s}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut g = Grammar::trusted_new(&text).with_start_rule(start);
        g.start_rule = start.to_string();
        g
    }

    #[test]
    fn detects_missing_start_rule() {
        let g = Grammar::trusted_new("a <- [a]\nb <- [b]");
        let a = analyze_grammar(&g);
        assert!(!a.has_start_rule);
        assert!(a.errors.iter().any(|e| e.contains("missing start rule")));
    }

    #[test]
    fn detects_duplicate_rules() {
        let g = Grammar {
            start_rule: "a".to_string(),
            text: "a <- x\na <- y".to_string(),
            metadata: HashMap::new(),
            imports: HashMap::new(),
            rules: vec![
                GrammarRule::trusted_from_source("a", "x", Vec::new()),
                GrammarRule::trusted_from_source("a", "y", Vec::new()),
            ],
            version: 1,
            state: GrammarState {
                sealed: false,
                analysis_state: None,
                version: 0,
            },
            compiled: Default::default(),
        };
        let a = analyze_grammar(&g);
        assert!(a.has_duplicate_rule_names);
        assert!(a.errors.iter().any(|e| e.contains("duplicate rule: a")));
    }

    #[test]
    fn detects_missing_refs() {
        let g = make_grammar("root", &[("root", "a b")]);
        let a = analyze_grammar(&g);
        // 'a' and 'b' are not defined
        let missing: Vec<_> = a.missing_refs.iter().map(|(_, t)| t.as_str()).collect();
        assert!(missing.contains(&"a") || missing.contains(&"b"));
    }

    #[test]
    fn reachability_excludes_unreachable() {
        let g = make_grammar("root", &[("root", "'x'"), ("orphan", "'y'")]);
        let a = analyze_grammar(&g);
        assert!(a.reachable.contains(&"root".to_string()));
        assert!(a.unreachable.contains(&"orphan".to_string()));
    }

    #[test]
    fn no_errors_for_valid_grammar() {
        let g = make_grammar("root", &[("root", "'hello'")]);
        let a = analyze_grammar(&g);
        assert!(a.has_start_rule);
        assert!(a.errors.is_empty());
    }

    #[test]
    fn grammar_try_new_rejects_invalid_rule_source_before_analysis() {
        let err = Grammar::try_new("root <- [a").unwrap_err();
        assert!(err.to_string().contains("unterminated character class"));
    }

    #[test]
    fn detects_reference_cycle() {
        let g = make_grammar("a", &[("a", "b"), ("b", "a")]);
        let a = analyze_grammar(&g);
        assert!(!a.left_recursive.is_empty());
    }

    #[test]
    fn no_cycle_for_linear_chain() {
        let g = make_grammar("a", &[("a", "b"), ("b", "'x'")]);
        let a = analyze_grammar(&g);
        assert!(a.left_recursive.is_empty());
    }

    #[test]
    fn cache_is_reused_for_same_version() {
        let g = make_grammar("root", &[("root", "'x'")]);
        let state1 = analyze_cached_grammar(&g, None);
        let state2 = analyze_cached_grammar(&g, Some(&state1));
        assert_eq!(state1, state2);
    }

    #[test]
    fn cache_is_invalidated_on_version_change() {
        let mut g = make_grammar("root", &[("root", "'x'")]);
        let state1 = analyze_cached_grammar(&g, None);
        g.set_rule("extra", "'y'");
        let state2 = analyze_cached_grammar(&g, Some(&state1));
        assert_ne!(state1.analysis.rule_count, state2.analysis.rule_count);
    }

    #[test]
    fn nullable_rule_detected() {
        let g = make_grammar("root", &[("root", "'x'?")]);
        let nullable = compute_nullable_rules(&g);
        assert!(nullable.contains("root"));
    }

    #[test]
    fn non_nullable_rule_not_in_set() {
        let g = make_grammar("root", &[("root", "'x'")]);
        let nullable = compute_nullable_rules(&g);
        assert!(!nullable.contains("root"));
    }

    #[test]
    fn incremental_state_has_signatures_after_first_analysis() {
        let g = make_grammar("root", &[("root", "'x'")]);
        let state = analyze_cached_grammar(&g, None);
        assert!(!state.rule_signatures.is_empty());
        assert!(state.rule_signatures.contains_key("root"));
    }

    #[test]
    fn incremental_short_circuits_when_signatures_unchanged() {
        let g = make_grammar("root", &[("root", "'x'"), ("helper", "'y'")]);
        let state1 = analyze_cached_grammar(&g, None);
        // Pass same grammar with same version: signatures match → must get identical state.
        let state2 = analyze_cached_grammar(&g, Some(&state1));
        assert_eq!(state1, state2);
    }

    #[test]
    fn incremental_detects_single_changed_rule() {
        let mut g = make_grammar("root", &[("root", "'x'"), ("helper", "'y'")]);
        let state1 = analyze_cached_grammar(&g, None);
        // Change only the "helper" rule source.
        g.rules[1].trusted_set_source("'z'");
        g.bump_version().unwrap();
        let state2 = analyze_cached_grammar(&g, Some(&state1));
        // "root" signature should be preserved in state2 and equal state1's.
        assert_eq!(
            state1.rule_signatures.get("root"),
            state2.rule_signatures.get("root")
        );
        // "helper" signature must have changed.
        assert_ne!(
            state1.rule_signatures.get("helper"),
            state2.rule_signatures.get("helper")
        );
    }

    #[test]
    fn reverse_refs_populated_correctly() {
        // "root" references "helper"; so reverse_refs["helper"] should include "root".
        let g = make_grammar("root", &[("root", "helper"), ("helper", "'y'")]);
        let state = analyze_cached_grammar(&g, None);
        let dependents = state
            .reverse_refs
            .get("helper")
            .cloned()
            .unwrap_or_default();
        assert!(dependents.contains(&"root".to_string()));
    }

    #[test]
    fn left_recursive_sccs_detects_self_loop() {
        // "expr <- expr '+' atom" — expr is self-referencing at the left edge.
        let g = make_grammar("expr", &[("expr", "expr '+' atom / atom"), ("atom", "'x'")]);
        let a = analyze_grammar(&g);
        assert!(
            !a.left_recursive_sccs.is_empty(),
            "expected at least one LR SCC"
        );
        let all_lr: Vec<&str> = a
            .left_recursive_sccs
            .iter()
            .flatten()
            .map(String::as_str)
            .collect();
        assert!(all_lr.contains(&"expr"), "expr should be in LR sccs");
    }

    #[test]
    fn left_recursive_sccs_empty_for_non_recursive() {
        let g = make_grammar("root", &[("root", "'a' 'b'"), ("helper", "'c'")]);
        let a = analyze_grammar(&g);
        assert!(
            a.left_recursive_sccs.is_empty(),
            "no LR rules expected, got: {:?}",
            a.left_recursive_sccs
        );
    }

    #[test]
    fn analyze_and_store_populates_left_refs() {
        let mut g = make_grammar("expr", &[("expr", "expr '+' atom / atom"), ("atom", "'x'")]);
        analyze_and_store(&mut g);
        let state = g.state.analysis_state.as_ref().unwrap();
        // expr's left refs should include itself
        let lr = state.left_refs.get("expr").cloned().unwrap_or_default();
        assert!(
            lr.contains(&"expr".to_string()),
            "expected expr in left_refs[expr], got: {lr:?}"
        );
    }

    #[test]
    fn transitive_dependents_propagates_through_chain() {
        // a → b → c; changing "c" should mark a, b, c as affected.
        let mut r: HashMap<String, Vec<String>> = HashMap::new();
        r.insert("b".into(), vec!["a".into()]);
        r.insert("c".into(), vec!["b".into()]);
        let changed: HashSet<String> = ["c".to_string()].into();
        let affected = transitive_dependents(&changed, &r);
        assert!(affected.contains("a"));
        assert!(affected.contains("b"));
        assert!(affected.contains("c"));
    }
}
