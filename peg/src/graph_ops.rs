//! Graph algorithms for incremental grammar analysis.
//!
//! Ported from `peg/analysis/incremental_graph_ops.py`.

use std::collections::{HashMap, HashSet, VecDeque};

// ── Tarjan SCC ────────────────────────────────────────────────────────────────

struct TarjanCtx {
    index_counter: usize,
    indices: HashMap<String, usize>,
    lowlinks: HashMap<String, usize>,
    stack: Vec<String>,
    on_stack: HashSet<String>,
    components: Vec<Vec<String>>,
}

impl TarjanCtx {
    fn new() -> Self {
        Self {
            index_counter: 0,
            indices: HashMap::new(),
            lowlinks: HashMap::new(),
            stack: Vec::new(),
            on_stack: HashSet::new(),
            components: Vec::new(),
        }
    }

    fn visit(&mut self, node: &str, graph: &HashMap<String, HashSet<String>>) {
        if self.indices.contains_key(node) {
            return;
        }
        self.strongconnect(node.to_string(), graph);
    }

    fn strongconnect(&mut self, node: String, graph: &HashMap<String, HashSet<String>>) {
        self.indices.insert(node.clone(), self.index_counter);
        self.lowlinks.insert(node.clone(), self.index_counter);
        self.index_counter += 1;
        self.stack.push(node.clone());
        self.on_stack.insert(node.clone());

        let deps: Vec<String> = graph
            .get(&node)
            .map(|s| {
                let mut v: Vec<String> = s
                    .iter()
                    .filter(|d| graph.contains_key(*d))
                    .cloned()
                    .collect();
                v.sort_unstable();
                v
            })
            .unwrap_or_default();

        for dep in deps {
            if !self.indices.contains_key(&dep) {
                self.strongconnect(dep.clone(), graph);
                let dep_ll = self.lowlinks[&dep];
                let ll = self.lowlinks.get_mut(&node).unwrap();
                *ll = (*ll).min(dep_ll);
            } else if self.on_stack.contains(&dep) {
                let dep_idx = self.indices[&dep];
                let ll = self.lowlinks.get_mut(&node).unwrap();
                *ll = (*ll).min(dep_idx);
            }
        }

        if self.lowlinks[&node] == self.indices[&node] {
            let mut component = Vec::new();
            loop {
                let w = self.stack.pop().unwrap();
                self.on_stack.remove(&w);
                let done = w == node;
                component.push(w);
                if done {
                    break;
                }
            }
            component.sort_unstable();
            self.components.push(component);
        }
    }
}

/// Compute strongly connected components using Tarjan's algorithm.
///
/// Processes nodes in sorted order for deterministic output.
/// Each component is returned as a sorted `Vec<String>`.
pub fn strongly_connected_components(graph: &HashMap<String, HashSet<String>>) -> Vec<Vec<String>> {
    let mut ctx = TarjanCtx::new();
    let mut nodes: Vec<&String> = graph.keys().collect();
    nodes.sort_unstable();
    for node in nodes {
        ctx.visit(node, graph);
    }
    ctx.components
}

/// Return only the left-recursive SCCs (self-loops or multi-node cycles),
/// sorted by `(len, members)`.
pub fn find_left_recursive_sccs(left_refs: &HashMap<String, HashSet<String>>) -> Vec<Vec<String>> {
    let components = strongly_connected_components(left_refs);
    let mut lr: Vec<Vec<String>> = components
        .into_iter()
        .filter(|c| _is_left_recursive_component(c, left_refs))
        .collect();
    lr.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    lr
}

fn _is_left_recursive_component(
    component: &[String],
    graph: &HashMap<String, HashSet<String>>,
) -> bool {
    if component.len() > 1 {
        return true;
    }
    let rule = &component[0];
    graph.get(rule).is_some_and(|deps| deps.contains(rule))
}

// ── Graph utilities ───────────────────────────────────────────────────────────

/// Build the reverse graph: for each edge `a → b`, add `b → a`.
///
/// Every node that appears as a key in `graph` is guaranteed to appear as a key
/// in the output even if it has no incoming edges.
pub fn build_reverse_graph(
    graph: &HashMap<String, HashSet<String>>,
) -> HashMap<String, HashSet<String>> {
    let mut reverse: HashMap<String, HashSet<String>> = HashMap::new();
    for (owner, targets) in graph {
        reverse.entry(owner.clone()).or_default();
        for target in targets {
            reverse
                .entry(target.clone())
                .or_default()
                .insert(owner.clone());
        }
    }
    reverse
}

/// Merge two graphs by unioning edge sets for each node.
pub fn merge_graphs(
    left: &HashMap<String, HashSet<String>>,
    right: &HashMap<String, HashSet<String>>,
) -> HashMap<String, HashSet<String>> {
    let mut merged: HashMap<String, HashSet<String>> =
        left.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    for (name, targets) in right {
        merged
            .entry(name.clone())
            .or_default()
            .extend(targets.iter().cloned());
    }
    merged
}

/// BFS from `seeds` through `reverse_graph`, collecting all transitive dependents.
pub fn collect_transitive_dependents(
    seeds: &HashSet<String>,
    reverse_graph: &HashMap<String, HashSet<String>>,
) -> HashSet<String> {
    if seeds.is_empty() {
        return HashSet::new();
    }
    let mut affected: HashSet<String> = seeds.clone();
    let mut queue: VecDeque<String> = seeds.iter().cloned().collect();
    while let Some(current) = queue.pop_front() {
        if let Some(dependents) = reverse_graph.get(&current) {
            for dep in dependents {
                if affected.insert(dep.clone()) {
                    queue.push_back(dep.clone());
                }
            }
        }
    }
    affected
}

/// BFS through both forward and reverse edges, collecting the connected region.
///
/// Only visits nodes present in `defined_rules`.
pub fn collect_bidirectional_region(
    seeds: &HashSet<String>,
    forward: &HashMap<String, HashSet<String>>,
    reverse: &HashMap<String, HashSet<String>>,
    defined_rules: &HashSet<String>,
) -> HashSet<String> {
    let mut region: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = seeds.iter().cloned().collect();
    while let Some(current) = queue.pop_front() {
        if !region.insert(current.clone()) {
            continue;
        }
        for neighbor in forward
            .get(&current)
            .into_iter()
            .flat_map(|s| s.iter())
            .chain(reverse.get(&current).into_iter().flat_map(|s| s.iter()))
        {
            if !region.contains(neighbor) && defined_rules.contains(neighbor) {
                queue.push_back(neighbor.clone());
            }
        }
    }
    region
}

/// DFS from `start` through `refs`, collecting all reachable rules in `defined_rules`.
pub fn collect_reachable_rules(
    start: &str,
    refs: &HashMap<String, HashSet<String>>,
    defined_rules: &HashSet<String>,
) -> HashSet<String> {
    let mut reachable: HashSet<String> = HashSet::new();
    if !defined_rules.contains(start) {
        return reachable;
    }
    let mut stack = vec![start.to_string()];
    while let Some(current) = stack.pop() {
        if reachable.contains(&current) || !defined_rules.contains(&current) {
            continue;
        }
        reachable.insert(current.clone());
        if let Some(targets) = refs.get(&current) {
            stack.extend(targets.iter().cloned());
        }
    }
    reachable
}

/// Incrementally refresh left-recursive SCCs after grammar changes.
///
/// Preserves SCCs that are unchanged (not in `region`), then recomputes SCCs
/// only for the affected region.
pub fn refresh_left_recursive_sccs(
    defined_rules: &HashSet<String>,
    previous_sccs: &[Vec<String>],
    previous_left_refs: &HashMap<String, HashSet<String>>,
    previous_reverse_left_refs: &HashMap<String, HashSet<String>>,
    affected: &HashSet<String>,
    left_refs: &HashMap<String, HashSet<String>>,
    reverse_left_refs: &HashMap<String, HashSet<String>>,
    structural_changes: &HashSet<String>,
) -> Vec<Vec<String>> {
    if structural_changes.is_empty() {
        return previous_sccs.to_vec();
    }
    let seeds = if !affected.is_empty() {
        affected.clone()
    } else {
        structural_changes
            .intersection(defined_rules)
            .cloned()
            .collect()
    };
    let merged_fwd = merge_graphs(previous_left_refs, left_refs);
    let merged_rev = merge_graphs(previous_reverse_left_refs, reverse_left_refs);
    let region = collect_bidirectional_region(&seeds, &merged_fwd, &merged_rev, defined_rules);
    let region_set: HashSet<&String> = region.iter().collect();

    // Keep SCCs that are entirely within defined_rules and not touched by the region.
    let mut preserved: Vec<Vec<String>> = previous_sccs
        .iter()
        .filter(|c| {
            let as_set: HashSet<&String> = c.iter().collect();
            as_set.is_subset(&defined_rules.iter().collect()) && as_set.is_disjoint(&region_set)
        })
        .cloned()
        .collect();

    // Recompute SCCs for the affected region only.
    let region_graph: HashMap<String, HashSet<String>> = {
        let mut m = HashMap::new();
        let mut sorted_region: Vec<&String> = region.iter().collect();
        sorted_region.sort_unstable();
        for name in sorted_region {
            let targets: HashSet<String> = left_refs
                .get(name)
                .map(|s| s.intersection(&region).cloned().collect())
                .unwrap_or_default();
            m.insert(name.clone(), targets);
        }
        m
    };
    let recomputed = find_left_recursive_sccs(&region_graph);

    preserved.extend(recomputed);
    preserved.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    preserved
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(edges: &[(&str, &[&str])]) -> HashMap<String, HashSet<String>> {
        edges
            .iter()
            .map(|(k, vs)| (k.to_string(), vs.iter().map(|v| v.to_string()).collect()))
            .collect()
    }

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // ── strongly_connected_components ─────────────────────────────────────────

    #[test]
    fn scc_single_node_no_self_loop() {
        let g = graph(&[("a", &[])]);
        let sccs = strongly_connected_components(&g);
        assert_eq!(sccs, vec![vec!["a".to_string()]]);
    }

    #[test]
    fn scc_self_loop() {
        let g = graph(&[("a", &["a"])]);
        let sccs = strongly_connected_components(&g);
        assert_eq!(sccs, vec![vec!["a".to_string()]]);
    }

    #[test]
    fn scc_simple_cycle() {
        let g = graph(&[("a", &["b"]), ("b", &["a"])]);
        let sccs = strongly_connected_components(&g);
        assert_eq!(sccs.len(), 1);
        assert_eq!(sccs[0], vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn scc_two_separate_nodes() {
        let g = graph(&[("a", &["b"]), ("b", &[])]);
        let sccs = strongly_connected_components(&g);
        // a → b (no cycle): two singleton SCCs, b comes first in topological order
        assert_eq!(sccs.len(), 2);
        let all_names: HashSet<String> = sccs.iter().flatten().cloned().collect();
        assert!(all_names.contains("a") && all_names.contains("b"));
    }

    #[test]
    fn scc_diamond_no_cycle() {
        // a → b, a → c, b → d, c → d (no cycles)
        let g = graph(&[("a", &["b", "c"]), ("b", &["d"]), ("c", &["d"]), ("d", &[])]);
        let sccs = strongly_connected_components(&g);
        // All singletons
        assert_eq!(sccs.len(), 4);
    }

    // ── find_left_recursive_sccs ──────────────────────────────────────────────

    #[test]
    fn lr_sccs_detects_self_loop() {
        let g = graph(&[("expr", &["expr"]), ("atom", &[])]);
        let lr = find_left_recursive_sccs(&g);
        assert_eq!(lr.len(), 1);
        assert_eq!(lr[0], vec!["expr".to_string()]);
    }

    #[test]
    fn lr_sccs_detects_mutual_recursion() {
        let g = graph(&[("a", &["b"]), ("b", &["a"])]);
        let lr = find_left_recursive_sccs(&g);
        assert_eq!(lr.len(), 1);
        assert_eq!(lr[0], vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn lr_sccs_no_recursion() {
        let g = graph(&[("a", &["b"]), ("b", &[])]);
        let lr = find_left_recursive_sccs(&g);
        assert!(lr.is_empty());
    }

    // ── build_reverse_graph ───────────────────────────────────────────────────

    #[test]
    fn reverse_graph_basic() {
        let g = graph(&[("a", &["b", "c"]), ("b", &["c"])]);
        let rev = build_reverse_graph(&g);
        // b and c should point back to a; c should also point back to b
        assert!(rev["b"].contains("a"));
        assert!(rev["c"].contains("a"));
        assert!(rev["c"].contains("b"));
        // a has no incoming edges (key still present)
        assert!(rev.contains_key("a"));
        assert!(rev["a"].is_empty());
    }

    // ── merge_graphs ──────────────────────────────────────────────────────────

    #[test]
    fn merge_graphs_unions_edges() {
        let left = graph(&[("a", &["b"]), ("b", &[])]);
        let right = graph(&[("a", &["c"]), ("d", &[])]);
        let merged = merge_graphs(&left, &right);
        assert!(merged["a"].contains("b"));
        assert!(merged["a"].contains("c"));
        assert!(merged.contains_key("b"));
        assert!(merged.contains_key("d"));
    }

    // ── collect_transitive_dependents ─────────────────────────────────────────

    #[test]
    fn transitive_dependents_empty_seeds() {
        let rev = graph(&[("a", &["b"])]);
        let result = collect_transitive_dependents(&HashSet::new(), &rev);
        assert!(result.is_empty());
    }

    #[test]
    fn transitive_dependents_basic() {
        // reverse graph: c ← b ← a (so dependents of c include b and a)
        let rev = graph(&[("c", &["b"]), ("b", &["a"]), ("a", &[])]);
        let seeds = set(&["c"]);
        let result = collect_transitive_dependents(&seeds, &rev);
        assert!(result.contains("c"));
        assert!(result.contains("b"));
        assert!(result.contains("a"));
    }

    // ── collect_bidirectional_region ──────────────────────────────────────────

    #[test]
    fn bidirectional_region_follows_both_directions() {
        let forward = graph(&[("a", &["b"]), ("b", &[])]);
        let reverse = graph(&[("b", &["a"]), ("a", &[])]);
        let defined = set(&["a", "b", "c"]);
        let seeds = set(&["b"]);
        let region = collect_bidirectional_region(&seeds, &forward, &reverse, &defined);
        assert!(region.contains("a"));
        assert!(region.contains("b"));
        assert!(!region.contains("c"));
    }

    // ── collect_reachable_rules ───────────────────────────────────────────────

    #[test]
    fn reachable_rules_dfs() {
        let refs = graph(&[("a", &["b", "c"]), ("b", &["d"]), ("c", &[]), ("d", &[])]);
        let defined = set(&["a", "b", "c", "d"]);
        let reachable = collect_reachable_rules("a", &refs, &defined);
        assert_eq!(reachable, set(&["a", "b", "c", "d"]));
    }

    #[test]
    fn reachable_rules_stops_at_undefined() {
        let refs = graph(&[("a", &["b", "x"]), ("b", &[])]);
        let defined = set(&["a", "b"]); // "x" not defined
        let reachable = collect_reachable_rules("a", &refs, &defined);
        assert!(reachable.contains("a"));
        assert!(reachable.contains("b"));
        assert!(!reachable.contains("x"));
    }

    #[test]
    fn reachable_rules_unknown_start() {
        let refs = graph(&[("a", &[])]);
        let defined = set(&["a"]);
        let reachable = collect_reachable_rules("z", &refs, &defined);
        assert!(reachable.is_empty());
    }

    // ── refresh_left_recursive_sccs ───────────────────────────────────────────

    #[test]
    fn refresh_lr_sccs_no_structural_changes() {
        let previous_sccs = vec![vec!["a".to_string()]];
        let empty: HashMap<String, HashSet<String>> = HashMap::new();
        let result = refresh_left_recursive_sccs(
            &set(&["a"]),
            &previous_sccs,
            &empty,
            &empty,
            &HashSet::new(),
            &empty,
            &empty,
            &HashSet::new(), // no structural changes
        );
        assert_eq!(result, previous_sccs);
    }

    #[test]
    fn refresh_lr_sccs_detects_new_recursion() {
        // "a" was not recursive before; now a → a (self-loop added)
        let previous_sccs: Vec<Vec<String>> = vec![];
        let empty: HashMap<String, HashSet<String>> = HashMap::new();
        let left_refs = graph(&[("a", &["a"])]);
        let rev_left_refs = build_reverse_graph(&left_refs);
        let structural_changes = set(&["a"]);
        let defined = set(&["a"]);

        let result = refresh_left_recursive_sccs(
            &defined,
            &previous_sccs,
            &empty,
            &empty,
            &HashSet::new(),
            &left_refs,
            &rev_left_refs,
            &structural_changes,
        );
        assert_eq!(result, vec![vec!["a".to_string()]]);
    }
}
