//! Deterministic hashing for compiled grammars (cache keys, identity checks).

use crate::grammar::Grammar;
use crate::signature::grammar_signature;

/// Return a stable hex hash string for a grammar's compiled structure.
///
/// Equivalent to Python's `compiled_grammar_hash`: two grammars with the same
/// rules, start rule, and metadata produce the same hash.
pub fn compiled_grammar_hash(grammar: &Grammar) -> String {
    format!("{:016x}", grammar_signature(grammar))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::Grammar;

    #[test]
    fn hash_is_stable_for_same_grammar() {
        let g = Grammar::new("start <- 'x'").with_start_rule("start");
        assert_eq!(compiled_grammar_hash(&g), compiled_grammar_hash(&g));
    }

    #[test]
    fn hash_changes_when_rule_source_changes() {
        let g1 = Grammar::new("start <- 'x'").with_start_rule("start");
        let g2 = Grammar::new("start <- 'y'").with_start_rule("start");
        assert_ne!(compiled_grammar_hash(&g1), compiled_grammar_hash(&g2));
    }

    #[test]
    fn hash_changes_when_metadata_changes() {
        let g1 = Grammar::new("start <- 'x'")
            .with_start_rule("start")
            .with_metadata(
                "__grammar__",
                [("mode".to_string(), serde_json::json!("strict"))]
                    .into_iter()
                    .collect(),
            );
        let g2 = Grammar::new("start <- 'x'")
            .with_start_rule("start")
            .with_metadata(
                "__grammar__",
                [("mode".to_string(), serde_json::json!("loose"))]
                    .into_iter()
                    .collect(),
            );
        assert_ne!(compiled_grammar_hash(&g1), compiled_grammar_hash(&g2));
    }
}
