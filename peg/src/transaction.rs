use serde_json::Value;
use thiserror::Error;

use crate::grammar::Grammar;
use crate::mutation::{diff_grammars, GrammarDiff, MutationOutcome};
use crate::validation::{validate_grammar, ValidationReport};

// ── Error ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransactionError {
    #[error("transaction is already {0}; no further operations are allowed")]
    AlreadyFinalized(String),
    #[error("grammar validation failed: {0}")]
    ValidationFailed(String),
    #[error("rule not found: {0}")]
    RuleNotFound(String),
    #[error("duplicate rule: {0}")]
    DuplicateRule(String),
    #[error("cannot remove start rule: {0}")]
    ProtectedStartRule(String),
}

// ── Operation log ──────────────────────────────────────────────────────────

/// A single mutation recorded in the transaction operation log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionOp {
    pub index: usize,
    pub name: String,
    pub args: Vec<String>,
}

// ── State ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
enum TxState {
    Open,
    Committed,
    RolledBack,
}

impl TxState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Committed => "committed",
            Self::RolledBack => "rolled_back",
        }
    }
}

// ── GrammarTransaction ─────────────────────────────────────────────────────

/// Transactional wrapper around a cloned grammar snapshot.
///
/// A transaction begins in the `Open` state. All mutations are applied to the
/// internal working copy. Calling `commit()` validates (if `strict`), seals
/// the working grammar, and returns a `MutationOutcome`. Calling `rollback()`
/// discards all changes and returns the original base grammar.
///
/// Both `commit` and `rollback` consume `self`.
pub struct GrammarTransaction {
    base: Grammar,
    pub(crate) working: Grammar,
    state: TxState,
    strict: bool,
    op_log: Vec<TransactionOp>,
    op_index: usize,
}

impl GrammarTransaction {
    /// Create a strict transaction (validation on commit).
    pub fn new(grammar: &Grammar) -> Self {
        Self::with_options(grammar, true)
    }

    /// Create a transaction with configurable strict validation.
    pub fn with_options(grammar: &Grammar, strict: bool) -> Self {
        let mut working = grammar.clone();
        working.thaw();
        Self {
            base: grammar.clone(),
            working,
            state: TxState::Open,
            strict,
            op_log: Vec::new(),
            op_index: 0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.state == TxState::Open
    }

    pub fn is_committed(&self) -> bool {
        self.state == TxState::Committed
    }

    pub fn is_rolled_back(&self) -> bool {
        self.state == TxState::RolledBack
    }

    pub fn state_str(&self) -> &'static str {
        self.state.as_str()
    }

    /// Reference to the current working grammar.
    pub fn current(&self) -> &Grammar {
        &self.working
    }

    /// Independent clone of the working grammar.
    pub fn snapshot(&self) -> Grammar {
        self.working.clone()
    }

    /// Diff between the base grammar and the current working state.
    pub fn diff(&self) -> GrammarDiff {
        diff_grammars(&self.base, &self.working)
    }

    /// Recorded operations so far.
    pub fn operation_log(&self) -> &[TransactionOp] {
        &self.op_log
    }

    // ── Validation ─────────────────────────────────────────────────────

    /// Validate the current working grammar without finalizing.
    pub fn validate(&self) -> ValidationReport {
        validate_grammar(&self.working)
    }

    // ── Mutations ──────────────────────────────────────────────────────

    /// Add a new rule. Errors if the rule already exists.
    pub fn add_rule(&mut self, name: &str, source: &str) -> Result<(), TransactionError> {
        self.ensure_open()?;
        if self.working.get_rule(name).is_some() {
            return Err(TransactionError::DuplicateRule(name.to_string()));
        }
        self.working.set_rule(name, source);
        self.record_op("add_rule", vec![name.to_string(), source.to_string()]);
        Ok(())
    }

    /// Replace an existing rule body. Errors if the rule does not exist.
    pub fn replace_rule(&mut self, name: &str, source: &str) -> Result<(), TransactionError> {
        self.ensure_open()?;
        if self.working.get_rule(name).is_none() {
            return Err(TransactionError::RuleNotFound(name.to_string()));
        }
        self.working.set_rule(name, source);
        self.record_op("replace_rule", vec![name.to_string(), source.to_string()]);
        Ok(())
    }

    /// Remove a rule. Cannot remove the start rule.
    pub fn remove_rule(&mut self, name: &str) -> Result<(), TransactionError> {
        self.ensure_open()?;
        if self.working.get_rule(name).is_none() {
            return Err(TransactionError::RuleNotFound(name.to_string()));
        }
        if name == self.working.start_rule {
            return Err(TransactionError::ProtectedStartRule(name.to_string()));
        }
        self.working.remove_rule(name);
        self.record_op("remove_rule", vec![name.to_string()]);
        Ok(())
    }

    /// Change the start rule. The target rule must already exist.
    pub fn set_start(&mut self, name: &str) -> Result<(), TransactionError> {
        self.ensure_open()?;
        if self.working.get_rule(name).is_none() {
            return Err(TransactionError::RuleNotFound(name.to_string()));
        }
        self.working.start_rule = name.to_string();
        self.record_op("set_start", vec![name.to_string()]);
        Ok(())
    }

    /// Set a metadata value for an owner key.
    pub fn set_metadata_value(
        &mut self,
        owner: &str,
        key: &str,
        value: Value,
    ) -> Result<(), TransactionError> {
        self.ensure_open()?;
        self.working.set_metadata_value(owner, key, value);
        self.record_op(
            "set_metadata_value",
            vec![owner.to_string(), key.to_string()],
        );
        Ok(())
    }

    // ── Finalize ───────────────────────────────────────────────────────

    /// Validate (when `strict`), seal, and finalize the grammar.
    ///
    /// Consumes `self`. Returns the diff and the sealed grammar inside a
    /// `MutationOutcome`.
    pub fn commit(mut self) -> Result<MutationOutcome, TransactionError> {
        self.ensure_open()?;
        if self.strict {
            let report = validate_grammar(&self.working);
            if !report.ok() {
                let msgs: Vec<String> = report.errors().map(|i| i.message.clone()).collect();
                return Err(TransactionError::ValidationFailed(msgs.join("; ")));
            }
        }
        let diff = diff_grammars(&self.base, &self.working);
        self.working.seal();
        self.working.version = self.working.version.saturating_add(1);
        self.state = TxState::Committed;
        Ok(MutationOutcome {
            grammar: self.working,
            diff,
            committed: true,
        })
    }

    /// Commit without running validation checks.
    ///
    /// Use when the caller guarantees grammar validity externally.
    pub fn commit_unchecked(mut self) -> MutationOutcome {
        let diff = diff_grammars(&self.base, &self.working);
        self.working.seal();
        self.working.version = self.working.version.saturating_add(1);
        self.state = TxState::Committed;
        MutationOutcome {
            grammar: self.working,
            diff,
            committed: true,
        }
    }

    /// Discard all changes and return the original base grammar.
    ///
    /// Consumes `self`.
    pub fn rollback(mut self) -> Grammar {
        self.state = TxState::RolledBack;
        self.base
    }

    // ── Stack integration (crate-internal) ────────────────────────────

    /// Replace the working grammar (used by `GrammarTransactionStack` when
    /// merging a committed child into its parent).
    pub(crate) fn replace_working(&mut self, grammar: Grammar) {
        self.working = grammar;
        self.working.thaw();
    }

    /// Append operations from a committed child transaction into this log.
    pub(crate) fn append_operations(&mut self, ops: &[TransactionOp]) {
        for op in ops {
            self.op_index += 1;
            self.op_log.push(TransactionOp {
                index: self.op_index,
                name: op.name.clone(),
                args: op.args.clone(),
            });
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────

    fn ensure_open(&self) -> Result<(), TransactionError> {
        if self.state != TxState::Open {
            Err(TransactionError::AlreadyFinalized(
                self.state.as_str().to_string(),
            ))
        } else {
            Ok(())
        }
    }

    fn record_op(&mut self, name: &str, args: Vec<String>) {
        self.op_index += 1;
        self.op_log.push(TransactionOp {
            index: self.op_index,
            name: name.to_string(),
            args,
        });
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
    fn new_transaction_is_open() {
        let tx = GrammarTransaction::new(&base());
        assert!(tx.is_open());
        assert!(!tx.is_committed());
    }

    #[test]
    fn add_rule_then_commit() {
        let mut tx = GrammarTransaction::new(&base());
        tx.add_rule("extra", "'y'").unwrap();
        assert!(tx.current().get_rule("extra").is_some());
        let outcome = tx.commit().unwrap();
        assert!(outcome.committed);
        assert!(outcome.grammar.get_rule("extra").is_some());
        assert!(outcome.grammar.is_sealed());
        assert!(!outcome.diff.is_empty());
    }

    #[test]
    fn replace_rule() {
        let mut tx = GrammarTransaction::new(&base());
        tx.replace_rule("root", "'z'").unwrap();
        let outcome = tx.commit_unchecked();
        assert_eq!(outcome.grammar.get_rule("root").unwrap().source, "'z'");
        assert!(!outcome.diff.changed_rules.is_empty());
    }

    #[test]
    fn replace_rule_missing_errors() {
        let mut tx = GrammarTransaction::new(&base());
        assert!(matches!(
            tx.replace_rule("missing", "x"),
            Err(TransactionError::RuleNotFound(_))
        ));
    }

    #[test]
    fn remove_rule() {
        let g = Grammar::new("root <- 'x'\nextra <- 'y'").with_start_rule("root");
        let mut tx = GrammarTransaction::new(&g);
        tx.remove_rule("extra").unwrap();
        let outcome = tx.commit_unchecked();
        assert!(outcome.grammar.get_rule("extra").is_none());
        assert_eq!(outcome.diff.removed_rules, vec!["extra"]);
    }

    #[test]
    fn cannot_remove_start_rule() {
        let mut tx = GrammarTransaction::new(&base());
        assert!(matches!(
            tx.remove_rule("root"),
            Err(TransactionError::ProtectedStartRule(_))
        ));
    }

    #[test]
    fn set_start_rule() {
        let g = Grammar::new("root <- 'x'\nnew_start <- 'y'").with_start_rule("root");
        let mut tx = GrammarTransaction::new(&g);
        tx.set_start("new_start").unwrap();
        let outcome = tx.commit_unchecked();
        assert_eq!(outcome.grammar.start_rule, "new_start");
        assert!(outcome.diff.start_changed);
    }

    #[test]
    fn set_start_rule_missing_errors() {
        let mut tx = GrammarTransaction::new(&base());
        assert!(matches!(
            tx.set_start("ghost"),
            Err(TransactionError::RuleNotFound(_))
        ));
    }

    #[test]
    fn rollback_returns_original() {
        let g = base();
        let original_version = g.version;
        let mut tx = GrammarTransaction::new(&g);
        tx.add_rule("extra", "'y'").unwrap();
        let base_back = tx.rollback();
        assert!(base_back.get_rule("extra").is_none());
        assert_eq!(base_back.version, original_version);
    }

    #[test]
    fn cannot_operate_after_commit() {
        let tx = GrammarTransaction::new(&base());
        let _ = tx.commit_unchecked();
        // tx is consumed; Rust ownership prevents use-after-commit at compile time
    }

    #[test]
    fn operation_log_records_ops() {
        let mut tx = GrammarTransaction::new(&base());
        tx.add_rule("a", "'a'").unwrap();
        tx.add_rule("b", "'b'").unwrap();
        let log = tx.operation_log();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].name, "add_rule");
        assert_eq!(log[0].index, 1);
        assert_eq!(log[1].name, "add_rule");
        assert_eq!(log[1].index, 2);
    }

    #[test]
    fn diff_before_commit() {
        let mut tx = GrammarTransaction::new(&base());
        tx.add_rule("new_rule", "'z'").unwrap();
        let diff = tx.diff();
        assert_eq!(diff.added_rules, vec!["new_rule"]);
    }

    #[test]
    fn strict_commit_fails_on_missing_ref() {
        let g = Grammar::new("root <- missing_ref").with_start_rule("root");
        let tx = GrammarTransaction::new(&g); // strict = true
        let err = tx.commit().unwrap_err();
        assert!(matches!(err, TransactionError::ValidationFailed(_)));
    }

    #[test]
    fn non_strict_commit_succeeds_with_invalid_grammar() {
        let g = Grammar::new("root <- missing_ref").with_start_rule("root");
        let tx = GrammarTransaction::with_options(&g, false);
        let outcome = tx.commit().unwrap();
        assert!(outcome.committed);
    }

    #[test]
    fn snapshot_is_independent_clone() {
        let mut tx = GrammarTransaction::new(&base());
        let snap1 = tx.snapshot();
        tx.add_rule("extra", "'y'").unwrap();
        let snap2 = tx.snapshot();
        assert!(snap1.get_rule("extra").is_none());
        assert!(snap2.get_rule("extra").is_some());
    }

    #[test]
    fn set_metadata_value() {
        let mut tx = GrammarTransaction::new(&base());
        tx.set_metadata_value("root", "return_type", serde_json::json!("Expr"))
            .unwrap();
        let outcome = tx.commit_unchecked();
        let meta = outcome.grammar.metadata.get("root").expect("meta");
        assert_eq!(
            meta.get("return_type").and_then(|v| v.as_str()),
            Some("Expr")
        );
    }
}
