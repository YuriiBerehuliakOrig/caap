//! Incremental grammar-analysis refresh.
//!
//! Ports `peg/analysis/incremental_scan.py` and `peg/analysis/incremental_refresh.py`.
//! Provides fine-grained per-rule caching so only structurally changed rules (and their
//! transitive dependents) are re-analysed on each grammar mutation.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::analysis::{
    GrammarAnalysis, GrammarAnalysisState, ParamArityMismatch, RuleIssueSummary, RuleScanSummary,
};
use crate::grammar::Grammar;
use crate::graph_ops::{
    build_reverse_graph, collect_reachable_rules, collect_transitive_dependents, merge_graphs,
    refresh_left_recursive_sccs,
};
use crate::parser::{
    collect_dead_choice_alts_from_source, collect_nullable_repetitions_from_source,
    collect_overlapping_prefixes_from_source, collect_prefix_shadowed_alts_from_source,
    compute_lr_min_step_from_source, extract_calls_from_source, extract_left_refs_from_source,
    extract_params_used_from_source, extract_refs_from_source, has_bare_commit_from_source,
    is_source_nullable_with_rules, is_source_productive_with_rules, source_fixed_text,
};

// ── Helpers ────────────────────────────────────────────────────────────────

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 14_695_981_039_346_656_037;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1_099_511_628_211);
    }
    h
}

fn vec_to_set(v: &HashMap<String, Vec<String>>) -> HashMap<String, HashSet<String>> {
    v.iter()
        .map(|(k, vs)| (k.clone(), vs.iter().cloned().collect()))
        .collect()
}

fn set_to_sorted_vec(m: HashMap<String, HashSet<String>>) -> HashMap<String, Vec<String>> {
    m.into_iter()
        .map(|(k, mut s)| {
            let mut v: Vec<String> = s.drain().collect();
            v.sort_unstable();
            (k, v)
        })
        .collect()
}

// ── Structural change detection ─────────────────────────────────────────────

/// Compute which rules changed structurally compared to the previous analysis.
///
/// A rule is considered changed if it was added, removed, or if its source hash
/// or declared parameter list differs from the previous state.
pub fn collect_structural_changes(
    defined_rules: &HashSet<String>,
    prev_rule_sigs: &HashMap<String, u64>,
    prev_param_sigs: &HashMap<String, Vec<String>>,
    curr_rule_sigs: &HashMap<String, u64>,
    curr_param_sigs: &HashMap<String, Vec<String>>,
) -> HashSet<String> {
    let prev_set: HashSet<&String> = prev_rule_sigs.keys().collect();
    let curr_set: HashSet<&String> = defined_rules.iter().collect();
    // Added and removed rules.
    let mut changed: HashSet<String> = prev_set
        .symmetric_difference(&curr_set)
        .map(|s| (*s).clone())
        .collect();
    // Rules present in both: compare signatures.
    for name in defined_rules
        .iter()
        .filter(|n| prev_rule_sigs.contains_key(*n))
    {
        if curr_rule_sigs.get(name) != prev_rule_sigs.get(name)
            || curr_param_sigs.get(name) != prev_param_sigs.get(name)
        {
            changed.insert(name.clone());
        }
    }
    changed
}

// ── Per-rule scan ──────────────────────────────────────────────────────────

/// Scan a single rule and return its structural summary.
pub fn scan_rule_summary(
    grammar: &Grammar,
    name: &str,
    defined_rules: &HashSet<String>,
) -> RuleScanSummary {
    let rule = match grammar.get_rule(name) {
        Some(r) => r,
        None => return RuleScanSummary::default(),
    };

    let mut refs: Vec<String> = extract_refs_from_source(&rule.source);
    refs.sort_unstable();
    refs.dedup();

    let mut missing_refs: Vec<(String, String)> = refs
        .iter()
        .filter(|r| !r.is_empty() && !defined_rules.contains(*r))
        .map(|r| (name.to_string(), r.clone()))
        .collect();
    missing_refs.sort_unstable();

    // Build param-count map for arity checking (first-occurrence-wins).
    let mut rule_param_counts: HashMap<&str, usize> = HashMap::new();
    for r in &grammar.rules {
        rule_param_counts
            .entry(r.name.as_str())
            .or_insert(r.params.len());
    }

    let calls = extract_calls_from_source(&rule.source);
    let mut param_arity_mismatches: Vec<(String, String, usize, usize)> = calls
        .iter()
        .filter_map(|(callee, got)| {
            let expected = *rule_param_counts.get(callee.as_str())?;
            if *got != expected {
                Some((name.to_string(), callee.clone(), expected, *got))
            } else {
                None
            }
        })
        .collect();
    param_arity_mismatches.sort_unstable();
    param_arity_mismatches.dedup();

    let used_params = extract_params_used_from_source(&rule.source);
    let declared: HashSet<&str> = rule.params.iter().map(|s| s.as_str()).collect();
    let used_set: HashSet<&str> = used_params.iter().map(|s| s.as_str()).collect();

    let mut undeclared_params: Vec<(String, String)> = used_params
        .iter()
        .filter(|p| !declared.contains(p.as_str()))
        .map(|p| (name.to_string(), p.clone()))
        .collect();
    undeclared_params.sort_unstable();

    let mut unused_params: Vec<(String, String)> = rule
        .params
        .iter()
        .filter(|p| !used_set.contains(p.as_str()))
        .map(|p| (name.to_string(), p.clone()))
        .collect();
    unused_params.sort_unstable();

    RuleScanSummary {
        refs,
        missing_refs,
        param_arity_mismatches,
        undeclared_params,
        unused_params,
    }
}

// ── Per-rule issue collection ──────────────────────────────────────────────

/// Collect analysis issues for a single rule.
pub fn collect_rule_issues_summary(
    grammar: &Grammar,
    name: &str,
    nullable_rules: &HashSet<String>,
    fixed_rules: &HashMap<String, Option<String>>,
) -> RuleIssueSummary {
    let rule = match grammar.get_rule(name) {
        Some(r) => r,
        None => return RuleIssueSummary::default(),
    };

    let mut nullable_repetition: Vec<(String, String)> =
        collect_nullable_repetitions_from_source(&rule.source, nullable_rules)
            .into_iter()
            .map(|kind| (name.to_string(), kind))
            .collect();
    nullable_repetition.sort_unstable();

    let mut dead_choice_alternatives: Vec<(String, usize, usize)> =
        collect_dead_choice_alts_from_source(&rule.source, fixed_rules)
            .into_iter()
            .map(|(dead, live)| (name.to_string(), dead, live))
            .collect();
    dead_choice_alternatives.sort_unstable();

    let mut prefix_shadowed_choice_alternatives: Vec<(String, usize, usize, String)> =
        collect_prefix_shadowed_alts_from_source(&rule.source, fixed_rules)
            .into_iter()
            .map(|(dead, live, pfx)| (name.to_string(), dead, live, pfx))
            .collect();
    prefix_shadowed_choice_alternatives.sort_unstable();

    let non_choice_commits: Vec<(String, String)> = has_bare_commit_from_source(&rule.source)
        .map(|kind| vec![(name.to_string(), kind.to_string())])
        .unwrap_or_default();

    let mut overlapping_prefixes: Vec<(String, usize, usize, String)> =
        collect_overlapping_prefixes_from_source(&rule.source, fixed_rules)
            .into_iter()
            .map(|(a1, a2, pfx)| (name.to_string(), a1, a2, pfx))
            .collect();
    overlapping_prefixes.sort_unstable();

    RuleIssueSummary {
        nullable_repetition,
        dead_choice_alternatives,
        prefix_shadowed_choice_alternatives,
        non_choice_commits,
        overlapping_prefixes,
    }
}

// ── Generic incremental property solver ────────────────────────────────────

/// Incrementally re-solve a per-rule property (nullable, productive, fixed-text …)
/// using BFS over the reverse-reference graph.
///
/// Only `affected` rules are reset to `initial` and re-evaluated; unaffected rules
/// keep their `previous_values`.  Propagation only touches rules in `affected` — this
/// is correct because `affected` is assumed to already be the full transitive closure.
pub fn solve_rule_property_incrementally<V, F>(
    grammar: &Grammar,
    previous_values: &HashMap<String, V>,
    reverse_refs: &HashMap<String, Vec<String>>,
    affected: &HashSet<String>,
    evaluator: F,
    initial: V,
) -> HashMap<String, V>
where
    V: Clone + PartialEq,
    F: Fn(&str, &HashMap<String, V>) -> V,
{
    // Unique rule names (first-occurrence-wins).
    let mut seen: HashSet<&str> = HashSet::new();
    let rule_names: HashSet<String> = grammar
        .rules
        .iter()
        .filter(|r| seen.insert(r.name.as_str()))
        .map(|r| r.name.clone())
        .collect();

    // Seed values: carry over previous, reset affected.
    let mut values: HashMap<String, V> = rule_names
        .iter()
        .map(|n| {
            let v = if affected.contains(n) {
                initial.clone()
            } else {
                previous_values
                    .get(n)
                    .cloned()
                    .unwrap_or_else(|| initial.clone())
            };
            (n.clone(), v)
        })
        .collect();

    let mut queue: VecDeque<String> = VecDeque::new();
    let mut pending: HashSet<String> = HashSet::new();
    let mut sorted_affected: Vec<&String> = affected.iter().collect();
    sorted_affected.sort_unstable();
    for name in sorted_affected {
        queue.push_back(name.clone());
        pending.insert(name.clone());
    }

    while let Some(name) = queue.pop_front() {
        pending.remove(&name);
        let source = match grammar.get_rule(&name) {
            Some(r) => r.source.clone(),
            None => continue,
        };
        let new_value = evaluator(&source, &values);
        if new_value == *values.get(&name).unwrap() {
            continue;
        }
        values.insert(name.clone(), new_value);

        // Propagate to dependents that are also in `affected`.
        if let Some(deps) = reverse_refs.get(&name) {
            let mut sorted_deps: Vec<&String> = deps
                .iter()
                .filter(|d| {
                    rule_names.contains(*d) && affected.contains(*d) && !pending.contains(*d)
                })
                .collect();
            sorted_deps.sort_unstable();
            for dep in sorted_deps {
                queue.push_back(dep.clone());
                pending.insert(dep.clone());
            }
        }
    }

    values
}

// ── LR min-step computation ────────────────────────────────────────────────

/// Compute the minimum proven character advance per LR-growth iteration for each LR rule.
///
/// For each SCC, walks every rule body looking for `Sequence([Ref(scc_member), Literal(sep), ...])`
/// patterns and takes the maximum proven separator length as the SCC's minimum step.
/// Defaults to 1 (conservative) when no provable minimum exists.
pub fn compute_lr_min_step(
    grammar: &Grammar,
    left_recursive_sccs: &[Vec<String>],
) -> HashMap<String, usize> {
    let mut result = HashMap::new();
    for scc in left_recursive_sccs {
        let scc_set: HashSet<&str> = scc.iter().map(|s| s.as_str()).collect();
        let scc_min = scc
            .iter()
            .filter_map(|rule_name| grammar.get_rule(rule_name))
            .map(|rule| compute_lr_min_step_from_source(&rule.source, &scc_set))
            .max()
            .unwrap_or(1);
        for rule_name in scc {
            result.insert(rule_name.clone(), scc_min);
        }
    }
    result
}

// ── Analysis assembly ──────────────────────────────────────────────────────

/// Build a `GrammarAnalysis` from per-rule incremental caches.
fn assemble_grammar_analysis(
    grammar: &Grammar,
    refs: &HashMap<String, Vec<String>>,
    rule_scans: &HashMap<String, RuleScanSummary>,
    productive_rules: &HashMap<String, bool>,
    _nullable_rules: &HashMap<String, bool>,
    left_recursive_sccs: &[Vec<String>],
    node_issues: &HashMap<String, RuleIssueSummary>,
) -> GrammarAnalysis {
    let defined_rules: HashSet<String> = refs.keys().cloned().collect();
    let refs_hs = vec_to_set(refs);
    let reachable_set = collect_reachable_rules(&grammar.start_rule, &refs_hs, &defined_rules);
    let left_recursive: HashSet<&str> = left_recursive_sccs
        .iter()
        .flatten()
        .map(|s| s.as_str())
        .collect();

    // Aggregate per-rule scan results.
    let mut all_missing_refs: Vec<(String, String)> = rule_scans
        .values()
        .flat_map(|s| s.missing_refs.iter().cloned())
        .collect();
    all_missing_refs.sort_unstable();
    all_missing_refs.dedup();

    let mut all_param_arity: Vec<ParamArityMismatch> = rule_scans
        .values()
        .flat_map(|s| {
            s.param_arity_mismatches
                .iter()
                .map(|(caller, callee, exp, got)| ParamArityMismatch {
                    caller: caller.clone(),
                    callee: callee.clone(),
                    expected: *exp,
                    got: *got,
                })
        })
        .collect();
    all_param_arity.sort_unstable_by(|a, b| a.caller.cmp(&b.caller).then(a.callee.cmp(&b.callee)));
    all_param_arity.dedup_by(|a, b| a.caller == b.caller && a.callee == b.callee);

    let mut undeclared_params: Vec<(String, String)> = rule_scans
        .values()
        .flat_map(|s| s.undeclared_params.iter().cloned())
        .collect();
    undeclared_params.sort_unstable();
    undeclared_params.dedup();

    let mut unused_params: Vec<(String, String)> = rule_scans
        .values()
        .flat_map(|s| s.unused_params.iter().cloned())
        .collect();
    unused_params.sort_unstable();
    unused_params.dedup();

    // Aggregate per-rule issue results.
    let mut nullable_repetition: Vec<(String, String)> = node_issues
        .values()
        .flat_map(|i| i.nullable_repetition.iter().cloned())
        .collect();
    nullable_repetition.sort_unstable();

    let mut dead_choice_alternatives: Vec<(String, usize, usize)> = node_issues
        .values()
        .flat_map(|i| i.dead_choice_alternatives.iter().cloned())
        .collect();
    dead_choice_alternatives.sort_unstable();

    let mut prefix_shadowed: Vec<(String, usize, usize, String)> = node_issues
        .values()
        .flat_map(|i| i.prefix_shadowed_choice_alternatives.iter().cloned())
        .collect();
    prefix_shadowed.sort_unstable();

    let mut non_choice_commits: Vec<(String, String)> = node_issues
        .values()
        .flat_map(|i| i.non_choice_commits.iter().cloned())
        .collect();
    non_choice_commits.sort_unstable();

    let mut overlapping_prefixes: Vec<(String, usize, usize, String)> = node_issues
        .values()
        .flat_map(|i| i.overlapping_prefixes.iter().cloned())
        .collect();
    overlapping_prefixes.sort_unstable();

    let mut unproductive: Vec<String> = productive_rules
        .iter()
        .filter(|(_, &v)| !v)
        .map(|(k, _)| k.clone())
        .collect();
    unproductive.sort_unstable();

    // Find duplicates (rules with same name).
    let mut seen_names: HashSet<&str> = HashSet::new();
    let mut dup_seen: HashSet<&str> = HashSet::new();
    let mut duplicates: Vec<String> = Vec::new();
    for r in &grammar.rules {
        if !seen_names.insert(r.name.as_str()) && dup_seen.insert(r.name.as_str()) {
            duplicates.push(r.name.clone());
        }
    }
    duplicates.sort_unstable();

    let has_start_rule = defined_rules.contains(&grammar.start_rule);
    let mut reachable: Vec<String> = reachable_set.iter().cloned().collect();
    reachable.sort_unstable();
    let mut unreachable: Vec<String> = defined_rules
        .iter()
        .filter(|r| !reachable_set.contains(*r))
        .cloned()
        .collect();
    unreachable.sort_unstable();
    let mut left_recursive_sorted: Vec<String> =
        left_recursive.iter().map(|s| (*s).to_string()).collect();
    left_recursive_sorted.sort_unstable();

    // Build errors / warnings.
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for dup in &duplicates {
        errors.push(format!("duplicate rule: {dup}"));
    }
    if !has_start_rule {
        errors.push(format!("missing start rule: {}", grammar.start_rule));
    }
    for (owner, target) in &all_missing_refs {
        errors.push(format!("rule '{owner}' references unknown rule '{target}'"));
    }
    for m in &all_param_arity {
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
    for (owner, dead, live) in &dead_choice_alternatives {
        errors.push(format!(
            "rule '{owner}': alternative #{dead} is dead (shadowed by #{live})"
        ));
    }
    for (owner, dead, live, prefix) in &prefix_shadowed {
        errors.push(format!(
            "rule '{owner}': alternative #{dead} is shadowed by prefix alternative #{live} ('{prefix}')"
        ));
    }
    for rule in &unproductive {
        errors.push(format!(
            "rule '{rule}' is unproductive (cannot match any input)"
        ));
    }
    for lr in &left_recursive_sorted {
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
    for (owner, a1, a2, prefix) in &overlapping_prefixes {
        warnings.push(format!(
            "rule '{owner}': alternatives #{a1} and #{a2} share prefix '{prefix}'"
        ));
    }

    GrammarAnalysis {
        rule_count: grammar.rules.len(),
        has_start_rule,
        has_duplicate_rule_names: !duplicates.is_empty(),
        refs: refs.clone(),
        reachable,
        unreachable,
        missing_refs: all_missing_refs,
        left_recursive: left_recursive_sorted,
        left_recursive_sccs: left_recursive_sccs.to_vec(),
        duplicates,
        param_arity_mismatches: all_param_arity,
        undeclared_params,
        unused_params,
        non_choice_commits,
        nullable_repetition,
        dead_choice_alternatives,
        prefix_shadowed_choice_alternatives: prefix_shadowed,
        overlapping_prefixes,
        unproductive,
        warnings,
        errors,
    }
}

// ── Full incremental refresh ───────────────────────────────────────────────

/// Empty state used as the base when no previous state exists.
fn empty_state() -> GrammarAnalysisState {
    use crate::analysis::GrammarAnalysis;
    GrammarAnalysisState {
        analysis: GrammarAnalysis {
            rule_count: 0,
            has_start_rule: false,
            has_duplicate_rule_names: false,
            refs: HashMap::new(),
            reachable: Vec::new(),
            unreachable: Vec::new(),
            missing_refs: Vec::new(),
            left_recursive: Vec::new(),
            left_recursive_sccs: Vec::new(),
            duplicates: Vec::new(),
            param_arity_mismatches: Vec::new(),
            undeclared_params: Vec::new(),
            unused_params: Vec::new(),
            non_choice_commits: Vec::new(),
            nullable_repetition: Vec::new(),
            dead_choice_alternatives: Vec::new(),
            prefix_shadowed_choice_alternatives: Vec::new(),
            overlapping_prefixes: Vec::new(),
            unproductive: Vec::new(),
            warnings: Vec::new(),
            errors: Vec::new(),
        },
        analysis_version: 0,
        rule_signatures: HashMap::new(),
        param_signatures: HashMap::new(),
        reverse_refs: HashMap::new(),
        left_refs: HashMap::new(),
        reverse_left_refs: HashMap::new(),
        nullable_rules: HashMap::new(),
        productive_rules: HashMap::new(),
        fixed_rules: HashMap::new(),
        rule_scans: HashMap::new(),
        node_issues: HashMap::new(),
        lr_min_step: HashMap::new(),
    }
}

/// Perform a full incremental refresh of the grammar analysis state.
///
/// Only structurally changed rules (and their transitive dependents) are re-analysed;
/// all other per-rule results are carried forward from `previous`.
pub fn refresh_grammar_analysis_state(
    grammar: &Grammar,
    previous: Option<&GrammarAnalysisState>,
) -> GrammarAnalysisState {
    let base = previous.unwrap_or(&EMPTY_STATE_SENTINEL);

    // Compute current rule signatures.
    let mut seen_sig: HashSet<&str> = HashSet::new();
    let curr_rule_sigs: HashMap<String, u64> = grammar
        .rules
        .iter()
        .filter(|r| seen_sig.insert(r.name.as_str()))
        .map(|r| (r.name.clone(), fnv1a(&r.source)))
        .collect();
    let mut seen_param: HashSet<&str> = HashSet::new();
    let curr_param_sigs: HashMap<String, Vec<String>> = grammar
        .rules
        .iter()
        .filter(|r| seen_param.insert(r.name.as_str()))
        .map(|r| (r.name.clone(), r.params.clone()))
        .collect();

    let defined_rules: HashSet<String> = curr_rule_sigs.keys().cloned().collect();

    // ── Structural change detection ────────────────────────────────────────
    let structural_changes = collect_structural_changes(
        &defined_rules,
        &base.rule_signatures,
        &base.param_signatures,
        &curr_rule_sigs,
        &curr_param_sigs,
    );

    // Short-circuit: nothing changed.
    if structural_changes.is_empty()
        && grammar.start_rule
            == base
                .analysis
                .reachable
                .first()
                .map(|s| s.as_str())
                .unwrap_or("")
    {
        // full short-circuit: start rule and signatures identical
        // (check start rule by comparing analysis.reachable is imperfect — use rule_count as proxy)
        if base.rule_signatures.len() == defined_rules.len() {
            if let Some(prev) = previous {
                return prev.clone();
            }
        }
    }

    // ── Build refs map (re-use cache for unaffected rules) ─────────────────
    // First pass: use only structural_changes to decide which to re-scan.
    let refs_hs_prev: HashMap<String, HashSet<String>> = vec_to_set(&base.analysis.refs);
    let prev_rev_refs_hs: HashMap<String, HashSet<String>> = vec_to_set(&base.reverse_refs);

    // refs for changed rules.
    let mut refs_hs: HashMap<String, HashSet<String>> = HashMap::new();
    for rule in grammar.rules.iter() {
        if refs_hs.contains_key(&rule.name) {
            continue; // first-occurrence-wins
        }
        if structural_changes.contains(&rule.name) {
            let r: HashSet<String> = extract_refs_from_source(&rule.source).into_iter().collect();
            refs_hs.insert(rule.name.clone(), r);
        } else if let Some(prev) = refs_hs_prev.get(&rule.name) {
            refs_hs.insert(rule.name.clone(), prev.clone());
        } else {
            let r: HashSet<String> = extract_refs_from_source(&rule.source).into_iter().collect();
            refs_hs.insert(rule.name.clone(), r);
        }
    }

    let rev_refs_hs = build_reverse_graph(&refs_hs);
    let union_rev_refs = merge_graphs(&prev_rev_refs_hs, &rev_refs_hs);
    let affected = collect_transitive_dependents(&structural_changes, &union_rev_refs);
    let affected_existing: HashSet<String> = affected
        .iter()
        .filter(|n| defined_rules.contains(*n))
        .cloned()
        .collect();

    // ── Re-scan affected rules ─────────────────────────────────────────────
    let mut rule_scans: HashMap<String, RuleScanSummary> = base
        .rule_scans
        .iter()
        .filter(|(n, _)| defined_rules.contains(*n) && !affected_existing.contains(*n))
        .map(|(n, s)| (n.clone(), s.clone()))
        .collect();
    let mut sorted_affected: Vec<&String> = affected_existing.iter().collect();
    sorted_affected.sort_unstable();
    for name in &sorted_affected {
        let scan = scan_rule_summary(grammar, name, &defined_rules);
        // Update refs_hs with freshly-scanned refs.
        refs_hs.insert((*name).clone(), scan.refs.iter().cloned().collect());
        rule_scans.insert((*name).clone(), scan);
    }

    // Recompute reverse refs from final refs.
    let rev_refs_hs = build_reverse_graph(&refs_hs);
    let refs: HashMap<String, Vec<String>> = set_to_sorted_vec(refs_hs.clone());
    let reverse_refs: HashMap<String, Vec<String>> = set_to_sorted_vec(rev_refs_hs.clone());

    // ── Nullable (incremental BFS) ─────────────────────────────────────────
    let nullable_rules: HashMap<String, bool> = {
        let prev_nullable: HashMap<String, bool> = base
            .nullable_rules
            .iter()
            .filter(|(n, _)| defined_rules.contains(*n))
            .map(|(n, &v)| (n.clone(), v))
            .collect();
        solve_rule_property_incrementally(
            grammar,
            &prev_nullable,
            &reverse_refs,
            &affected_existing,
            |source, values| {
                let nullable_set: HashSet<String> = values
                    .iter()
                    .filter_map(|(k, &v)| if v { Some(k.clone()) } else { None })
                    .collect();
                is_source_nullable_with_rules(source, &nullable_set)
            },
            false,
        )
    };

    // ── Productive (incremental BFS) ───────────────────────────────────────
    let productive_rules: HashMap<String, bool> = {
        let prev_productive: HashMap<String, bool> = base
            .productive_rules
            .iter()
            .filter(|(n, _)| defined_rules.contains(*n))
            .map(|(n, &v)| (n.clone(), v))
            .collect();
        solve_rule_property_incrementally(
            grammar,
            &prev_productive,
            &reverse_refs,
            &affected_existing,
            is_source_productive_with_rules,
            false,
        )
    };

    // ── Fixed-text (incremental BFS) ───────────────────────────────────────
    let fixed_rules: HashMap<String, Option<String>> = {
        let prev_fixed: HashMap<String, Option<String>> = base
            .fixed_rules
            .iter()
            .filter(|(n, _)| defined_rules.contains(*n))
            .map(|(n, v)| (n.clone(), v.clone()))
            .collect();
        solve_rule_property_incrementally(
            grammar,
            &prev_fixed,
            &reverse_refs,
            &affected_existing,
            source_fixed_text,
            None,
        )
    };

    // ── Left-refs and LR SCCs ──────────────────────────────────────────────
    let nullable_set: HashSet<String> = nullable_rules
        .iter()
        .filter_map(|(k, &v)| if v { Some(k.clone()) } else { None })
        .collect();

    let prev_left_refs_hs: HashMap<String, HashSet<String>> = vec_to_set(&base.left_refs);
    let mut left_refs_hs: HashMap<String, HashSet<String>> = base
        .left_refs
        .iter()
        .filter(|(n, _)| defined_rules.contains(*n) && !affected_existing.contains(*n))
        .map(|(n, v)| (n.clone(), v.iter().cloned().collect()))
        .collect();
    for name in &sorted_affected {
        let rule = match grammar.get_rule(name) {
            Some(r) => r,
            None => continue,
        };
        let lr: HashSet<String> = extract_left_refs_from_source(&rule.source, &nullable_set)
            .into_iter()
            .filter(|r| defined_rules.contains(r))
            .collect();
        left_refs_hs.insert((*name).clone(), lr);
    }
    let rev_left_refs_hs = build_reverse_graph(&left_refs_hs);
    let prev_rev_left_refs_hs: HashMap<String, HashSet<String>> =
        vec_to_set(&base.reverse_left_refs);
    let prev_left_recursive_sccs = base.analysis.left_recursive_sccs.clone();

    let left_recursive_sccs: Vec<Vec<String>> = refresh_left_recursive_sccs(
        &defined_rules,
        &prev_left_recursive_sccs,
        &prev_left_refs_hs,
        &prev_rev_left_refs_hs,
        &affected_existing,
        &left_refs_hs,
        &rev_left_refs_hs,
        &structural_changes,
    );

    let left_refs: HashMap<String, Vec<String>> = set_to_sorted_vec(left_refs_hs);
    let reverse_left_refs: HashMap<String, Vec<String>> = set_to_sorted_vec(rev_left_refs_hs);

    // ── Node issues ────────────────────────────────────────────────────────
    let mut node_issues: HashMap<String, RuleIssueSummary> = base
        .node_issues
        .iter()
        .filter(|(n, _)| defined_rules.contains(*n) && !affected_existing.contains(*n))
        .map(|(n, s)| (n.clone(), s.clone()))
        .collect();
    for name in &sorted_affected {
        let issues = collect_rule_issues_summary(grammar, name, &nullable_set, &fixed_rules);
        node_issues.insert((*name).clone(), issues);
    }

    // ── Assemble and return ────────────────────────────────────────────────
    let analysis = assemble_grammar_analysis(
        grammar,
        &refs,
        &rule_scans,
        &productive_rules,
        &nullable_rules,
        &left_recursive_sccs,
        &node_issues,
    );
    let lr_min_step = compute_lr_min_step(grammar, &left_recursive_sccs);

    GrammarAnalysisState {
        analysis,
        analysis_version: grammar.version as u64,
        rule_signatures: curr_rule_sigs,
        param_signatures: curr_param_sigs,
        reverse_refs,
        left_refs,
        reverse_left_refs,
        nullable_rules,
        productive_rules,
        fixed_rules,
        rule_scans,
        node_issues,
        lr_min_step,
    }
}

// Sentinel used as a zero-allocation empty base (avoids Option gymnastics in the hot path).
static EMPTY_STATE_SENTINEL: std::sync::LazyLock<GrammarAnalysisState> =
    std::sync::LazyLock::new(empty_state);

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::Grammar;

    fn make_grammar(start: &str, rules: &[(&str, &str)]) -> Grammar {
        let text = rules
            .iter()
            .map(|(n, s)| format!("{n} <- {s}"))
            .collect::<Vec<_>>()
            .join("\n");
        Grammar::new(&text).with_start_rule(start)
    }

    // ── collect_structural_changes ─────────────────────────────────────────

    #[test]
    fn structural_changes_detects_added_rule() {
        let prev_sigs: HashMap<String, u64> = [("a".to_string(), 1u64)].into();
        let curr_sigs: HashMap<String, u64> =
            [("a".to_string(), 1u64), ("b".to_string(), 2u64)].into();
        let defined: HashSet<String> = curr_sigs.keys().cloned().collect();
        let empty: HashMap<String, Vec<String>> = HashMap::new();
        let changed = collect_structural_changes(&defined, &prev_sigs, &empty, &curr_sigs, &empty);
        assert!(changed.contains("b"), "added rule 'b' should be in changes");
        assert!(
            !changed.contains("a"),
            "unchanged rule 'a' should not be in changes"
        );
    }

    #[test]
    fn structural_changes_detects_removed_rule() {
        let prev_sigs: HashMap<String, u64> =
            [("a".to_string(), 1u64), ("b".to_string(), 2u64)].into();
        let curr_sigs: HashMap<String, u64> = [("a".to_string(), 1u64)].into();
        let defined: HashSet<String> = curr_sigs.keys().cloned().collect();
        let empty: HashMap<String, Vec<String>> = HashMap::new();
        let changed = collect_structural_changes(&defined, &prev_sigs, &empty, &curr_sigs, &empty);
        assert!(
            changed.contains("b"),
            "removed rule 'b' should be in changes"
        );
    }

    #[test]
    fn structural_changes_detects_source_change() {
        let prev_sigs: HashMap<String, u64> = [("a".to_string(), 1u64)].into();
        let curr_sigs: HashMap<String, u64> = [("a".to_string(), 99u64)].into();
        let defined: HashSet<String> = curr_sigs.keys().cloned().collect();
        let empty: HashMap<String, Vec<String>> = HashMap::new();
        let changed = collect_structural_changes(&defined, &prev_sigs, &empty, &curr_sigs, &empty);
        assert!(
            changed.contains("a"),
            "rule with changed hash should be in changes"
        );
    }

    #[test]
    fn structural_changes_empty_when_nothing_changed() {
        let sigs: HashMap<String, u64> = [("a".to_string(), 1u64)].into();
        let defined: HashSet<String> = sigs.keys().cloned().collect();
        let empty: HashMap<String, Vec<String>> = HashMap::new();
        let changed = collect_structural_changes(&defined, &sigs, &empty, &sigs, &empty);
        assert!(changed.is_empty());
    }

    // ── scan_rule_summary ──────────────────────────────────────────────────

    #[test]
    fn scan_rule_detects_missing_ref() {
        let g = make_grammar("root", &[("root", "missing_rule")]);
        let defined: HashSet<String> = ["root".to_string()].into();
        let scan = scan_rule_summary(&g, "root", &defined);
        assert!(
            scan.missing_refs.iter().any(|(_, t)| t == "missing_rule"),
            "should detect missing ref: {:?}",
            scan.missing_refs
        );
    }

    #[test]
    fn scan_rule_no_missing_ref_when_defined() {
        let g = make_grammar("root", &[("root", "helper"), ("helper", "'x'")]);
        let defined: HashSet<String> = ["root".to_string(), "helper".to_string()].into();
        let scan = scan_rule_summary(&g, "root", &defined);
        assert!(scan.missing_refs.is_empty());
    }

    #[test]
    fn scan_rule_detects_param_arity_mismatch() {
        let g = make_grammar("root", &[("wrap(x)", "'[' $x ']'"), ("root", "wrap()")]);
        let defined: HashSet<String> = ["root".to_string(), "wrap".to_string()].into();
        let scan = scan_rule_summary(&g, "root", &defined);
        assert!(
            scan.param_arity_mismatches
                .iter()
                .any(|(_, c, exp, got)| c == "wrap" && *exp == 1 && *got == 0),
            "arity mismatch: {:?}",
            scan.param_arity_mismatches
        );
    }

    #[test]
    fn scan_rule_detects_undeclared_param() {
        let g = make_grammar("root", &[("root(x)", "$x $y")]);
        let defined: HashSet<String> = ["root".to_string()].into();
        let scan = scan_rule_summary(&g, "root", &defined);
        assert!(
            scan.undeclared_params.iter().any(|(_, p)| p == "y"),
            "undeclared: {:?}",
            scan.undeclared_params
        );
    }

    #[test]
    fn scan_rule_detects_unused_param() {
        let g = make_grammar("root", &[("root(x, y)", "$x")]);
        let defined: HashSet<String> = ["root".to_string()].into();
        let scan = scan_rule_summary(&g, "root", &defined);
        assert!(
            scan.unused_params.iter().any(|(_, p)| p == "y"),
            "unused: {:?}",
            scan.unused_params
        );
    }

    // ── collect_rule_issues_summary ────────────────────────────────────────

    #[test]
    fn rule_issues_detects_bare_cut() {
        let g = make_grammar("root", &[("root", "'a' ~ 'b'")]);
        let issues = collect_rule_issues_summary(&g, "root", &HashSet::new(), &HashMap::new());
        assert!(
            issues.non_choice_commits.iter().any(|(_, k)| k == "cut"),
            "{:?}",
            issues.non_choice_commits
        );
    }

    #[test]
    fn rule_issues_none_for_simple_rule() {
        let g = make_grammar("root", &[("root", "'hello'")]);
        let issues = collect_rule_issues_summary(&g, "root", &HashSet::new(), &HashMap::new());
        assert!(issues.non_choice_commits.is_empty());
        assert!(issues.nullable_repetition.is_empty());
    }

    // ── solve_rule_property_incrementally ──────────────────────────────────

    #[test]
    fn property_solver_propagates_nullable() {
        // `root <- opt` where `opt` becomes nullable → root should become nullable.
        let g = make_grammar("root", &[("root", "opt"), ("opt", "'x'?")]);
        let defined: HashSet<String> = ["root".to_string(), "opt".to_string()].into();
        let reverse_refs: HashMap<String, Vec<String>> =
            [("opt".to_string(), vec!["root".to_string()])].into();
        let affected: HashSet<String> = defined.clone();
        let result = solve_rule_property_incrementally(
            &g,
            &HashMap::new(),
            &reverse_refs,
            &affected,
            |source, values| {
                let ns: HashSet<String> = values
                    .iter()
                    .filter_map(|(k, &v)| if v { Some(k.clone()) } else { None })
                    .collect();
                is_source_nullable_with_rules(source, &ns)
            },
            false,
        );
        assert_eq!(result.get("opt"), Some(&true));
        assert_eq!(result.get("root"), Some(&true));
    }

    #[test]
    fn property_solver_unaffected_rules_reuse_previous() {
        let g = make_grammar("root", &[("root", "'x'"), ("stable", "'y'?")]);
        let prev: HashMap<String, bool> = [("stable".to_string(), true)].into();
        let affected: HashSet<String> = ["root".to_string()].into();
        let result = solve_rule_property_incrementally(
            &g,
            &prev,
            &HashMap::new(),
            &affected,
            |_, _| false,
            false,
        );
        // "stable" not in affected → carries through from prev
        assert_eq!(result.get("stable"), Some(&true));
    }

    // ── compute_lr_min_step ────────────────────────────────────────────────

    #[test]
    fn lr_min_step_defaults_to_one_for_self_loop() {
        let g = make_grammar("expr", &[("expr", "expr atom / atom"), ("atom", "'x'")]);
        let sccs = vec![vec!["expr".to_string()]];
        let steps = compute_lr_min_step(&g, &sccs);
        assert_eq!(steps.get("expr"), Some(&1));
    }

    #[test]
    fn lr_min_step_detects_literal_separator() {
        // `expr <- expr '+' atom / atom` — separator '+' has length 1.
        let g = make_grammar("expr", &[("expr", "expr '+' atom / atom"), ("atom", "'x'")]);
        let sccs = vec![vec!["expr".to_string()]];
        let steps = compute_lr_min_step(&g, &sccs);
        assert_eq!(steps.get("expr"), Some(&1));
    }

    // ── refresh_grammar_analysis_state ─────────────────────────────────────

    #[test]
    fn refresh_full_analysis_on_first_call() {
        let g = make_grammar("root", &[("root", "'hello'")]);
        let state = refresh_grammar_analysis_state(&g, None);
        assert!(state.analysis.has_start_rule);
        assert!(state.rule_signatures.contains_key("root"));
        assert!(state.rule_scans.contains_key("root"));
    }

    #[test]
    fn refresh_short_circuits_when_unchanged() {
        let g = make_grammar("root", &[("root", "'x'")]);
        let state1 = refresh_grammar_analysis_state(&g, None);
        let state2 = refresh_grammar_analysis_state(&g, Some(&state1));
        assert_eq!(state1, state2);
    }

    #[test]
    fn refresh_detects_added_rule() {
        let mut g = make_grammar("root", &[("root", "'x'")]);
        let state1 = refresh_grammar_analysis_state(&g, None);
        g.set_rule("helper", "'y'");
        g.version += 1;
        let state2 = refresh_grammar_analysis_state(&g, Some(&state1));
        assert!(state2.rule_scans.contains_key("helper"));
        assert_ne!(state1.analysis.rule_count, state2.analysis.rule_count);
    }

    #[test]
    fn refresh_preserves_unaffected_scans() {
        let mut g = make_grammar("root", &[("root", "'x'"), ("helper", "'y'")]);
        let state1 = refresh_grammar_analysis_state(&g, None);
        // Change only "helper".
        g.set_rule("helper", "'z'");
        g.version += 1;
        let state2 = refresh_grammar_analysis_state(&g, Some(&state1));
        // "root" scan should be reused (same signature).
        assert_eq!(
            state1.rule_signatures.get("root"),
            state2.rule_signatures.get("root")
        );
    }

    #[test]
    fn refresh_populates_nullable_rules() {
        let g = make_grammar("root", &[("root", "'x'?"), ("other", "'y'")]);
        let state = refresh_grammar_analysis_state(&g, None);
        assert_eq!(state.nullable_rules.get("root"), Some(&true));
        assert_eq!(state.nullable_rules.get("other"), Some(&false));
    }

    #[test]
    fn refresh_populates_lr_min_step_for_lr_grammar() {
        let g = make_grammar("expr", &[("expr", "expr '+' atom / atom"), ("atom", "'x'")]);
        let state = refresh_grammar_analysis_state(&g, None);
        assert!(
            state.lr_min_step.contains_key("expr"),
            "lr_min_step should have 'expr': {:?}",
            state.lr_min_step
        );
    }

    #[test]
    fn refresh_detects_left_recursive_scc() {
        let g = make_grammar("expr", &[("expr", "expr '+' atom / atom"), ("atom", "'x'")]);
        let state = refresh_grammar_analysis_state(&g, None);
        let sccs = &state.analysis.left_recursive_sccs;
        assert!(!sccs.is_empty(), "expected LR SCC for expr");
        let all: Vec<&str> = sccs.iter().flatten().map(String::as_str).collect();
        assert!(all.contains(&"expr"));
    }
}
