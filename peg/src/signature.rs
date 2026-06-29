use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use crate::grammar::Grammar;

/// A stable structural hash for a grammar, suitable for cache invalidation.
///
/// Rules are hashed in sorted-name order so insertion order doesn't matter.
pub fn grammar_signature(grammar: &Grammar) -> u64 {
    let mut h = StableHasher::default();
    let mut seen = HashSet::new();
    hash_grammar(grammar, &mut h, &mut seen);
    h.finish()
}

pub(crate) struct StableHasher(u64);

impl StableHasher {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
}

impl Default for StableHasher {
    fn default() -> Self {
        Self(Self::FNV_OFFSET)
    }
}

impl Hasher for StableHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::FNV_PRIME);
        }
    }
}

fn hash_grammar(grammar: &Grammar, h: &mut StableHasher, seen: &mut HashSet<usize>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::Grammar;

    #[test]
    fn grammar_signature_is_stable() {
        let g = Grammar::trusted_new("root <- [a-z]+").with_start_rule("root");
        let s1 = grammar_signature(&g);
        let s2 = grammar_signature(&g);
        assert_eq!(s1, s2);
    }

    #[test]
    fn grammar_signature_differs_on_rule_change() {
        let g1 = Grammar::trusted_new("root <- [a-z]+").with_start_rule("root");
        let g2 = Grammar::trusted_new("root <- [A-Z]+").with_start_rule("root");
        assert_ne!(grammar_signature(&g1), grammar_signature(&g2));
    }

    #[test]
    fn grammar_signature_differs_on_start_rule_change() {
        let g1 = Grammar::trusted_new("a <- 'x'\nb <- 'y'").with_start_rule("a");
        let g2 = Grammar::trusted_new("a <- 'x'\nb <- 'y'").with_start_rule("b");
        assert_ne!(grammar_signature(&g1), grammar_signature(&g2));
    }

    #[test]
    fn grammar_signature_independent_of_insertion_order() {
        let mut g1 = Grammar::trusted_new("a <- 'x'").with_start_rule("a");
        g1.set_rule("b", "'y'");

        let mut g2 = Grammar::trusted_new("b <- 'y'").with_start_rule("a");
        g2.set_rule("a", "'x'");

        assert_eq!(grammar_signature(&g1), grammar_signature(&g2));
    }
}
