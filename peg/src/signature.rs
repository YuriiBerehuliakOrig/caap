use std::collections::{hash_map::DefaultHasher, HashSet};
use std::hash::{Hash, Hasher};

use crate::grammar::Grammar;
use crate::nodes::PegNode;

/// A stable structural hash for a grammar, suitable for cache invalidation.
///
/// Rules are hashed in sorted-name order so insertion order doesn't matter.
pub fn grammar_signature(grammar: &Grammar) -> u64 {
    let mut h = DefaultHasher::new();
    let mut seen = HashSet::new();
    hash_grammar(grammar, &mut h, &mut seen);
    h.finish()
}

fn hash_grammar(grammar: &Grammar, h: &mut DefaultHasher, seen: &mut HashSet<usize>) {
    let identity = grammar as *const Grammar as usize;
    if !seen.insert(identity) {
        "cycle".hash(h);
        return;
    }

    grammar.start_rule.hash(h);

    let mut names: Vec<&str> = grammar.rules.iter().map(|r| r.name.as_str()).collect();
    names.sort_unstable();

    for name in names {
        name.hash(h);
        if let Some(rule) = grammar.get_rule(name) {
            rule.source.hash(h);
        }
    }

    // Include metadata keys so grammar-metadata changes bust the cache too.
    let mut meta_keys: Vec<&str> = grammar.metadata.keys().map(String::as_str).collect();
    meta_keys.sort_unstable();
    for key in meta_keys {
        key.hash(h);
        if let Some(section) = grammar.metadata.get(key) {
            let mut sub_keys: Vec<&str> = section.keys().map(String::as_str).collect();
            sub_keys.sort_unstable();
            for sk in sub_keys {
                sk.hash(h);
                section[sk].to_string().hash(h);
            }
        }
    }

    let mut import_aliases: Vec<&String> = grammar.imports.keys().collect();
    import_aliases.sort_unstable();
    for alias in import_aliases {
        alias.hash(h);
        if let Some(imported) = grammar.imports.get(alias) {
            hash_grammar(imported, h, seen);
        }
    }

    seen.remove(&identity);
}

/// A stable structural hash of a single `PegNode` tree.
pub fn node_signature(node: &PegNode) -> u64 {
    let mut h = DefaultHasher::new();
    hash_node(node, &mut h);
    h.finish()
}

fn hash_node(node: &PegNode, h: &mut impl Hasher) {
    // Use the discriminant so each variant gets a unique prefix.
    std::mem::discriminant(node).hash(h);

    match node {
        PegNode::Literal(s)
        | PegNode::Regex(s)
        | PegNode::Ref(s)
        | PegNode::Token(s)
        | PegNode::TokenRef(s)
        | PegNode::Parameter(s)
        | PegNode::ImportedRef(s)
        | PegNode::Island(s)
        | PegNode::RawBlock(s)
        | PegNode::Expected(s) => s.hash(h),

        PegNode::InlineAction(name, args) => {
            name.hash(h);
            args.hash(h);
        }

        PegNode::Behavior(name, child) => {
            name.hash(h);
            hash_node(child, h);
        }

        PegNode::Named { name, node } => {
            name.hash(h);
            hash_node(node, h);
        }

        PegNode::Capture(label, child) => {
            label.hash(h);
            hash_node(child, h);
        }

        PegNode::GrammarScope { name, body } => {
            name.hash(h);
            for child in body {
                hash_node(child, h);
            }
        }

        PegNode::And(n)
        | PegNode::Not(n)
        | PegNode::OneOrMore(n)
        | PegNode::ZeroOrMore(n)
        | PegNode::Optional(n)
        | PegNode::Eager(n)
        | PegNode::Cut(n) => hash_node(n, h),

        PegNode::Sequence(items) | PegNode::Choice(items) | PegNode::Action(items) => {
            items.len().hash(h);
            for child in items {
                hash_node(child, h);
            }
        }

        PegNode::SepOneOrMore { separator, element } => {
            hash_node(separator, h);
            hash_node(element, h);
        }

        // Unit variants – discriminant alone is sufficient.
        PegNode::Dedent
        | PegNode::Indent
        | PegNode::Newline
        | PegNode::NoTrivia
        | PegNode::GrammarScopeEnter
        | PegNode::GrammarScopeExit => {}
    }
}

/// Return `true` if two node trees are structurally identical (same signature).
pub fn nodes_structurally_equal(a: &PegNode, b: &PegNode) -> bool {
    node_signature(a) == node_signature(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::Grammar;
    use crate::nodes::PegNode;

    #[test]
    fn grammar_signature_is_stable() {
        let g = Grammar::new("root <- [a-z]+").with_start_rule("root");
        let s1 = grammar_signature(&g);
        let s2 = grammar_signature(&g);
        assert_eq!(s1, s2);
    }

    #[test]
    fn grammar_signature_differs_on_rule_change() {
        let g1 = Grammar::new("root <- [a-z]+").with_start_rule("root");
        let g2 = Grammar::new("root <- [A-Z]+").with_start_rule("root");
        assert_ne!(grammar_signature(&g1), grammar_signature(&g2));
    }

    #[test]
    fn grammar_signature_differs_on_start_rule_change() {
        let g1 = Grammar::new("a <- 'x'\nb <- 'y'").with_start_rule("a");
        let g2 = Grammar::new("a <- 'x'\nb <- 'y'").with_start_rule("b");
        assert_ne!(grammar_signature(&g1), grammar_signature(&g2));
    }

    #[test]
    fn grammar_signature_independent_of_insertion_order() {
        let mut g1 = Grammar::new("a <- 'x'").with_start_rule("a");
        g1.set_rule("b", "'y'");

        let mut g2 = Grammar::new("b <- 'y'").with_start_rule("a");
        g2.set_rule("a", "'x'");

        assert_eq!(grammar_signature(&g1), grammar_signature(&g2));
    }

    #[test]
    fn node_signature_literal_stable() {
        let n = PegNode::Literal("hello".into());
        assert_eq!(node_signature(&n), node_signature(&n));
    }

    #[test]
    fn node_signature_differs_for_different_literals() {
        let a = PegNode::Literal("hello".into());
        let b = PegNode::Literal("world".into());
        assert_ne!(node_signature(&a), node_signature(&b));
    }

    #[test]
    fn node_signature_differs_for_different_kinds() {
        let a = PegNode::Literal("x".into());
        let b = PegNode::Regex("x".into());
        assert_ne!(node_signature(&a), node_signature(&b));
    }

    #[test]
    fn nodes_structurally_equal_returns_true_for_same() {
        let a = PegNode::Sequence(vec![PegNode::Literal("a".into()), PegNode::Ref("b".into())]);
        let b = PegNode::Sequence(vec![PegNode::Literal("a".into()), PegNode::Ref("b".into())]);
        assert!(nodes_structurally_equal(&a, &b));
    }

    #[test]
    fn nodes_structurally_equal_returns_false_for_different() {
        let a = PegNode::Choice(vec![PegNode::Literal("a".into())]);
        let b = PegNode::Sequence(vec![PegNode::Literal("a".into())]);
        assert!(!nodes_structurally_equal(&a, &b));
    }
}
