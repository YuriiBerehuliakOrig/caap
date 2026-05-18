use crate::analysis::GrammarAnalysisState;
use crate::types::MemoPolicy;

/// Resolve the four memo-limit values from a `MemoPolicy` and caller-provided defaults.
///
/// Returns `(memo_limit, node_limit, effective_window, prune_cadence)` — all `None`
/// when memoisation is disabled.
///
/// Errors if memoisation is disabled while the grammar contains left-recursive rules
/// (the BFS seed mechanism requires at least one memoised base-case per SCC).
pub fn resolve_memo_limits(
    memo_policy: Option<&MemoPolicy>,
    memoize: Option<bool>,
    default_cache_limit: Option<usize>,
    default_node_memo_window: Option<usize>,
    analysis_state: Option<&GrammarAnalysisState>,
) -> Result<(Option<usize>, Option<usize>, Option<usize>, Option<usize>), String> {
    let global_budget = memo_policy.and_then(|p| p.global_budget);
    let session_budget = memo_policy.and_then(|p| p.session_budget);
    let region_window = memo_policy.and_then(|p| p.region_window);
    let prune_cadence = memo_policy.and_then(|p| p.prune_cadence);

    let memo_disabled = memoize == Some(false) || global_budget == Some(0);
    if memo_disabled {
        let state = analysis_state.ok_or_else(|| {
            "grammar analysis state is required before resolving memo limits".to_string()
        })?;
        let mut lr_rules: Vec<&str> = state
            .analysis
            .left_recursive
            .iter()
            .map(|s| s.as_str())
            .collect();
        lr_rules.sort_unstable();
        if !lr_rules.is_empty() {
            return Err(format!(
                "Memoization cannot be disabled for left-recursive rules: {:?}",
                lr_rules
            ));
        }
        return Ok((None, None, None, None));
    }

    let memo_limit = global_budget.or(default_cache_limit);
    let node_limit = resolve_node_memo_limit(session_budget, memo_limit);
    let effective_window = if node_limit.is_none() {
        None
    } else {
        resolve_node_memo_window(region_window, default_node_memo_window)
    };

    Ok((memo_limit, node_limit, effective_window, prune_cadence))
}

fn resolve_node_memo_limit(
    session_budget: Option<usize>,
    memo_limit: Option<usize>,
) -> Option<usize> {
    match session_budget {
        Some(0) => None,
        Some(v) => Some(v),
        None => memo_limit,
    }
}

fn resolve_node_memo_window(
    region_window: Option<usize>,
    default_node_memo_window: Option<usize>,
) -> Option<usize> {
    region_window.or(default_node_memo_window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::GrammarAnalysis;
    use std::collections::HashMap;

    fn make_analysis(lr: &[&str]) -> GrammarAnalysis {
        GrammarAnalysis {
            rule_count: 0,
            has_start_rule: false,
            has_duplicate_rule_names: false,
            refs: HashMap::new(),
            reachable: vec![],
            unreachable: vec![],
            missing_refs: vec![],
            left_recursive: lr.iter().map(|s| s.to_string()).collect(),
            duplicates: vec![],
            param_arity_mismatches: vec![],
            undeclared_params: vec![],
            unused_params: vec![],
            non_choice_commits: vec![],
            nullable_repetition: vec![],
            dead_choice_alternatives: vec![],
            prefix_shadowed_choice_alternatives: vec![],
            overlapping_prefixes: vec![],
            unproductive: vec![],
            left_recursive_sccs: vec![],
            warnings: vec![],
            errors: vec![],
        }
    }

    fn empty_state() -> GrammarAnalysisState {
        GrammarAnalysisState {
            analysis: make_analysis(&[]),
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

    fn lr_state(rules: &[&str]) -> GrammarAnalysisState {
        GrammarAnalysisState {
            analysis: make_analysis(rules),
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

    #[test]
    fn no_policy_uses_defaults() {
        let (memo, node, window, cadence) =
            resolve_memo_limits(None, None, Some(1000), Some(512), None).unwrap();
        assert_eq!(memo, Some(1000));
        assert_eq!(node, Some(1000));
        assert_eq!(window, Some(512));
        assert_eq!(cadence, None);
    }

    #[test]
    fn global_budget_overrides_default() {
        let policy = MemoPolicy {
            global_budget: Some(200),
            session_budget: None,
            region_window: None,
            prune_cadence: None,
        };
        let (memo, node, window, _) =
            resolve_memo_limits(Some(&policy), None, Some(1000), Some(512), None).unwrap();
        assert_eq!(memo, Some(200));
        assert_eq!(node, Some(200));
        assert_eq!(window, Some(512));
    }

    #[test]
    fn session_budget_overrides_node_limit() {
        let policy = MemoPolicy {
            global_budget: Some(500),
            session_budget: Some(50),
            region_window: None,
            prune_cadence: None,
        };
        let (memo, node, _, _) =
            resolve_memo_limits(Some(&policy), None, None, None, None).unwrap();
        assert_eq!(memo, Some(500));
        assert_eq!(node, Some(50));
    }

    #[test]
    fn session_budget_zero_disables_node_limit() {
        let policy = MemoPolicy {
            global_budget: Some(500),
            session_budget: Some(0),
            region_window: None,
            prune_cadence: None,
        };
        let (_, node, window, _) =
            resolve_memo_limits(Some(&policy), None, None, None, None).unwrap();
        assert_eq!(node, None);
        assert_eq!(window, None);
    }

    #[test]
    fn memo_disabled_no_lr_ok() {
        let state = empty_state();
        let (memo, node, window, cadence) =
            resolve_memo_limits(None, Some(false), None, None, Some(&state)).unwrap();
        assert_eq!((memo, node, window, cadence), (None, None, None, None));
    }

    #[test]
    fn memo_disabled_with_lr_errors() {
        let state = lr_state(&["expr"]);
        let err = resolve_memo_limits(None, Some(false), None, None, Some(&state)).unwrap_err();
        assert!(err.contains("expr"), "error should mention the LR rule");
    }

    #[test]
    fn memo_disabled_no_state_errors() {
        let err = resolve_memo_limits(None, Some(false), None, None, None).unwrap_err();
        assert!(err.contains("analysis state"));
    }

    #[test]
    fn global_budget_zero_disables_memo() {
        let policy = MemoPolicy {
            global_budget: Some(0),
            session_budget: None,
            region_window: None,
            prune_cadence: None,
        };
        let state = empty_state();
        let (memo, node, window, cadence) =
            resolve_memo_limits(Some(&policy), None, None, None, Some(&state)).unwrap();
        assert_eq!((memo, node, window, cadence), (None, None, None, None));
    }

    #[test]
    fn prune_cadence_is_passed_through() {
        let policy = MemoPolicy {
            global_budget: None,
            session_budget: None,
            region_window: None,
            prune_cadence: Some(100),
        };
        let (_, _, _, cadence) =
            resolve_memo_limits(Some(&policy), None, None, None, None).unwrap();
        assert_eq!(cadence, Some(100));
    }
}
