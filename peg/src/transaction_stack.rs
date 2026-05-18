use crate::grammar::Grammar;
use crate::mutation::{diff_grammars, GrammarDiff, MutationOutcome};
use crate::transaction::{GrammarTransaction, TransactionError};

// ── GrammarTransactionStack ────────────────────────────────────────────────

/// Nested savepoint manager over a grammar snapshot.
///
/// The stack maintains a `committed` base grammar and a vector of open
/// `GrammarTransaction` levels. Operations are forwarded to the top-of-stack
/// transaction. When a level is committed it is merged into its parent (or
/// into `committed` if it was the last level). Rolling back simply discards
/// the top level.
///
/// # Usage
///
/// ```
/// # use caap_peg_port::{Grammar, GrammarTransactionStack};
/// let grammar = Grammar::new("root <- 'x'").with_start_rule("root");
/// let mut stack = GrammarTransactionStack::new(grammar);
///
/// stack.begin();
/// stack.mutate(|tx| { tx.add_rule("extra", "'y'")?; Ok(()) }).unwrap();
/// let grammar = stack.commit().unwrap();
/// assert!(grammar.get_rule("extra").is_some());
/// ```
pub struct GrammarTransactionStack {
    committed: Grammar,
    stack: Vec<GrammarTransaction>,
    strict: bool,
    last_outcome: Option<MutationOutcome>,
}

impl GrammarTransactionStack {
    /// Create a strict stack (validation on commit).
    pub fn new(grammar: Grammar) -> Self {
        Self::with_options(grammar, true)
    }

    pub fn with_options(grammar: Grammar, strict: bool) -> Self {
        Self {
            committed: grammar,
            stack: Vec::new(),
            strict,
            last_outcome: None,
        }
    }

    /// Number of open transaction levels.
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Whether at least one transaction level is open.
    pub fn has_open(&self) -> bool {
        !self.stack.is_empty()
    }

    /// The outcome of the last completed operation (commit or rollback).
    pub fn last_outcome(&self) -> Option<&MutationOutcome> {
        self.last_outcome.as_ref()
    }

    /// Reference to the current grammar (top of stack or committed).
    pub fn current(&self) -> &Grammar {
        if let Some(tx) = self.stack.last() {
            tx.current()
        } else {
            &self.committed
        }
    }

    /// Push a new transaction level scoped to the current grammar.
    pub fn begin(&mut self) {
        let base = self.current().clone();
        self.stack
            .push(GrammarTransaction::with_options(&base, self.strict));
    }

    /// Commit the top-of-stack transaction, merging into its parent.
    ///
    /// If the committed level was the only open one, `self.committed` is
    /// updated and the returned grammar is the new committed state.
    pub fn commit(&mut self) -> Result<Grammar, TransactionError> {
        let tx = self
            .stack
            .pop()
            .ok_or_else(|| TransactionError::AlreadyFinalized("no open transaction".to_string()))?;

        let outcome = tx.commit()?;
        let committed_grammar = outcome.grammar.clone();

        if let Some(parent) = self.stack.last_mut() {
            let mut merged = committed_grammar.clone();
            merged.thaw();
            parent.replace_working(merged);
            parent.append_operations(outcome.grammar_ops_placeholder());
        } else {
            self.committed = committed_grammar.clone();
        }

        self.last_outcome = Some(outcome);
        Ok(committed_grammar)
    }

    /// Roll back the top-of-stack transaction, discarding its changes.
    pub fn rollback(&mut self) -> Grammar {
        if self.stack.is_empty() {
            return self.committed.clone();
        }
        let tx = self.stack.pop().unwrap();
        let _base = tx.rollback();
        let current = self.current().clone();
        self.last_outcome = Some(MutationOutcome {
            grammar: current.clone(),
            diff: GrammarDiff::default(),
            committed: false,
        });
        current
    }

    /// Roll back all open levels, returning the committed base.
    pub fn rollback_all(&mut self) -> Grammar {
        self.stack.clear();
        self.committed.clone()
    }

    /// Run `f` inside a scoped transaction that auto-commits on success and
    /// auto-rolls-back on error.
    ///
    /// - If no transaction is open, a new one is created, used, committed,
    ///   and the committed grammar is stored in `self.committed`.
    /// - If a transaction is already open, `f` operates on the top-of-stack
    ///   transaction without nesting (consistent with Python's `stack_mutate`).
    pub fn mutate<F>(&mut self, f: F) -> Result<Grammar, TransactionError>
    where
        F: FnOnce(&mut GrammarTransaction) -> Result<(), TransactionError>,
    {
        if self.stack.is_empty() {
            let base = self.committed.clone();
            let mut tx = GrammarTransaction::with_options(&base, self.strict);
            f(&mut tx)?;
            let outcome = tx.commit()?;
            let grammar = outcome.grammar.clone();
            self.committed = grammar.clone();
            self.last_outcome = Some(outcome);
            Ok(grammar)
        } else {
            // Operate on the existing top-of-stack without committing
            let tx = self.stack.last_mut().unwrap();
            f(tx)?;
            let grammar = tx.current().clone();
            self.last_outcome = Some(MutationOutcome {
                grammar: grammar.clone(),
                diff: diff_grammars(&self.committed, &grammar),
                committed: false,
            });
            Ok(grammar)
        }
    }
}

// We need a small helper to extract operation log from MutationOutcome for
// forwarding to parent transactions.  Since `MutationOutcome` doesn't carry
// ops (it's only diff + grammar), we pass an empty slice.
trait MutationOutcomeOps {
    fn grammar_ops_placeholder(&self) -> &[crate::transaction::TransactionOp];
}
impl MutationOutcomeOps for MutationOutcome {
    fn grammar_ops_placeholder(&self) -> &[crate::transaction::TransactionOp] {
        &[]
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::Grammar;

    fn base() -> Grammar {
        Grammar::new("root <- 'x'").with_start_rule("root")
    }

    #[test]
    fn new_stack_has_no_open_levels() {
        let s = GrammarTransactionStack::new(base());
        assert_eq!(s.depth(), 0);
        assert!(!s.has_open());
    }

    #[test]
    fn begin_increases_depth() {
        let mut s = GrammarTransactionStack::new(base());
        s.begin();
        assert_eq!(s.depth(), 1);
        s.begin();
        assert_eq!(s.depth(), 2);
    }

    #[test]
    fn mutate_no_open_tx_commits_directly() {
        let mut s = GrammarTransactionStack::new(base());
        let g = s
            .mutate(|tx| {
                tx.add_rule("extra", "'y'")?;
                Ok(())
            })
            .unwrap();
        assert!(g.get_rule("extra").is_some());
        assert_eq!(s.depth(), 0);
        // committed grammar was updated
        assert!(s.current().get_rule("extra").is_some());
    }

    #[test]
    fn mutate_with_open_tx_does_not_auto_commit() {
        let mut s = GrammarTransactionStack::new(base());
        s.begin();
        s.mutate(|tx| {
            tx.add_rule("pending", "'p'")?;
            Ok(())
        })
        .unwrap();
        // stack still has 1 level open
        assert_eq!(s.depth(), 1);
        // The mutated grammar is visible via current()
        assert!(s.current().get_rule("pending").is_some());
        // But committed has not changed
        assert!(s.committed.get_rule("pending").is_none());
    }

    #[test]
    fn begin_commit_updates_committed() {
        let mut s = GrammarTransactionStack::new(base());
        s.begin();
        s.mutate(|tx| {
            tx.add_rule("extra", "'y'")?;
            Ok(())
        })
        .unwrap();
        let g = s.commit().unwrap();
        assert!(g.get_rule("extra").is_some());
        assert!(s.committed.get_rule("extra").is_some());
        assert_eq!(s.depth(), 0);
    }

    #[test]
    fn rollback_discards_changes() {
        let mut s = GrammarTransactionStack::new(base());
        s.begin();
        s.mutate(|tx| {
            tx.add_rule("temp", "'t'")?;
            Ok(())
        })
        .unwrap();
        let g = s.rollback();
        assert!(g.get_rule("temp").is_none());
        assert_eq!(s.depth(), 0);
    }

    #[test]
    fn rollback_all_clears_all_levels() {
        let mut s = GrammarTransactionStack::new(base());
        s.begin();
        s.begin();
        s.begin();
        assert_eq!(s.depth(), 3);
        let g = s.rollback_all();
        assert_eq!(s.depth(), 0);
        assert!(g.get_rule("extra").is_none());
    }

    #[test]
    fn nested_commit_merges_into_parent() {
        let mut s = GrammarTransactionStack::new(base());
        s.begin(); // outer
        s.begin(); // inner
        s.mutate(|tx| {
            tx.add_rule("inner_rule", "'i'")?;
            Ok(())
        })
        .unwrap();
        s.commit().unwrap(); // commit inner → merges into outer
        assert_eq!(s.depth(), 1);
        assert!(s.current().get_rule("inner_rule").is_some());
        s.commit().unwrap(); // commit outer → updates committed
        assert!(s.committed.get_rule("inner_rule").is_some());
    }

    #[test]
    fn rollback_empty_stack_returns_committed() {
        let mut s = GrammarTransactionStack::new(base());
        let g = s.rollback();
        assert_eq!(g.start_rule, base().start_rule);
    }

    #[test]
    fn commit_empty_stack_errors() {
        let mut s = GrammarTransactionStack::new(base());
        assert!(matches!(
            s.commit(),
            Err(TransactionError::AlreadyFinalized(_))
        ));
    }

    #[test]
    fn last_outcome_is_set_after_mutate() {
        let mut s = GrammarTransactionStack::new(base());
        assert!(s.last_outcome().is_none());
        s.mutate(|tx| {
            tx.add_rule("x", "'x'")?;
            Ok(())
        })
        .unwrap();
        assert!(s.last_outcome().is_some());
    }

    #[test]
    fn mutate_error_leaves_stack_unchanged() {
        let mut s = GrammarTransactionStack::new(base());
        let result = s.mutate(|tx| {
            tx.add_rule("a", "'a'")?;
            tx.replace_rule("nonexistent", "'z'")?; // this will error
            Ok(())
        });
        assert!(result.is_err());
        // No pending open transactions from the failed mutate
        assert_eq!(s.depth(), 0);
    }
}
