//! Graph algorithms for grammar analysis.

use std::collections::{HashMap, HashSet};

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
                if let Some(&dep_ll) = self.lowlinks.get(&dep) {
                    if let Some(ll) = self.lowlinks.get_mut(&node) {
                        *ll = (*ll).min(dep_ll);
                    }
                }
            } else if self.on_stack.contains(&dep) {
                if let Some(&dep_idx) = self.indices.get(&dep) {
                    if let Some(ll) = self.lowlinks.get_mut(&node) {
                        *ll = (*ll).min(dep_idx);
                    }
                }
            }
        }

        let Some(&node_lowlink) = self.lowlinks.get(&node) else {
            return;
        };
        let Some(&node_index) = self.indices.get(&node) else {
            return;
        };
        if node_lowlink == node_index {
            let mut component = Vec::new();
            while let Some(w) = self.stack.pop() {
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
}
