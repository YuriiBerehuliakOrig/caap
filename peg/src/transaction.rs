//! Transactional grammar editing (the `transaction` feature): apply a sequence
//! of mutations to a working copy, then [`commit`](GrammarTransaction::commit)
//! (validate + seal) or [`rollback`](GrammarTransaction::rollback) atomically.

use serde_json::Value;
use thiserror::Error;

use crate::error::ParseError;
use crate::grammar::Grammar;
use crate::mutation::{diff_grammars, GrammarDiff, MutationOutcome};
use crate::validation::{validate_grammar, ValidationReport};

// ── Error ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Error)]
/// Why a transaction operation failed.
pub enum TransactionError {
    #[error("transaction is already {0}; no further operations are allowed")]
    /// The transaction was already committed or rolled back.
    AlreadyFinalized(String),
    #[error("grammar validation failed: {0}")]
    /// Validation failed on commit.
    ValidationFailed(String),
    #[error("rule not found: {0}")]
    /// A referenced rule does not exist.
    RuleNotFound(String),
    #[error("duplicate rule: {0}")]
    /// A rule with that name already exists.
    DuplicateRule(String),
    #[error("cannot remove start rule: {0}")]
    /// Attempted to remove the current start rule.
    ProtectedStartRule(String),
    #[error("invalid rule source: {0}")]
    /// Rule source failed to parse.
    InvalidRuleSource(ParseError),
    #[error("grammar version overflow: {0}")]
    /// The grammar version counter overflowed.
    VersionOverflow(ParseError),
    #[error("transaction operation index overflow")]
    /// The operation-log index overflowed.
    OperationIndexOverflow,
}

// ── Operation log ──────────────────────────────────────────────────────────

/// A single mutation recorded in the transaction operation log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionOp {
    /// Sequential index in the operation log.
    pub index: usize,
    /// The operation name.
    pub name: String,
    /// String-rendered operation arguments.
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

    /// Whether the transaction is still open.
    pub fn is_open(&self) -> bool {
        self.state == TxState::Open
    }

    /// Whether the transaction was committed.
    pub fn is_committed(&self) -> bool {
        self.state == TxState::Committed
    }

    /// Whether the transaction was rolled back.
    pub fn is_rolled_back(&self) -> bool {
        self.state == TxState::RolledBack
    }

    /// The transaction state as a string.
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
        let op_index = self.next_op_index()?;
        self.working
            .try_set_rule(name, source)
            .map_err(grammar_mutation_error)?;
        self.record_op_at(
            op_index,
            "add_rule",
            vec![name.to_string(), source.to_string()],
        );
        Ok(())
    }

    /// Replace an existing rule body. Errors if the rule does not exist.
    pub fn replace_rule(&mut self, name: &str, source: &str) -> Result<(), TransactionError> {
        self.ensure_open()?;
        if self.working.get_rule(name).is_none() {
            return Err(TransactionError::RuleNotFound(name.to_string()));
        }
        let op_index = self.next_op_index()?;
        self.working
            .try_set_rule(name, source)
            .map_err(grammar_mutation_error)?;
        self.record_op_at(
            op_index,
            "replace_rule",
            vec![name.to_string(), source.to_string()],
        );
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
        let op_index = self.next_op_index()?;
        self.working
            .try_remove_rule(name)
            .map_err(TransactionError::VersionOverflow)?;
        self.record_op_at(op_index, "remove_rule", vec![name.to_string()]);
        Ok(())
    }

    /// Change the start rule. The target rule must already exist.
    pub fn set_start(&mut self, name: &str) -> Result<(), TransactionError> {
        self.ensure_open()?;
        if self.working.get_rule(name).is_none() {
            return Err(TransactionError::RuleNotFound(name.to_string()));
        }
        let op_index = self.next_op_index()?;
        self.working
            .bump_version()
            .map_err(TransactionError::VersionOverflow)?;
        self.working.start_rule = name.to_string();
        self.working.clear_analysis_cache();
        self.record_op_at(op_index, "set_start", vec![name.to_string()]);
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
        let op_index = self.next_op_index()?;
        self.working
            .try_set_metadata_value(owner, key, value)
            .map_err(TransactionError::VersionOverflow)?;
        self.record_op_at(
            op_index,
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
        self.working
            .bump_version()
            .map_err(TransactionError::VersionOverflow)?;
        self.working.seal();
        self.state = TxState::Committed;
        Ok(MutationOutcome {
            grammar: self.working,
            diff,
            committed: true,
        })
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
    pub(crate) fn append_operations(
        &mut self,
        ops: &[TransactionOp],
    ) -> Result<(), TransactionError> {
        for op in ops {
            let op_index = self.next_op_index()?;
            self.op_log.push(TransactionOp {
                index: op_index,
                name: op.name.clone(),
                args: op.args.clone(),
            });
            self.op_index = op_index;
        }
        Ok(())
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

    fn next_op_index(&self) -> Result<usize, TransactionError> {
        self.op_index
            .checked_add(1)
            .ok_or(TransactionError::OperationIndexOverflow)
    }

    fn record_op_at(&mut self, index: usize, name: &str, args: Vec<String>) {
        self.op_index = index;
        self.op_log.push(TransactionOp {
            index,
            name: name.to_string(),
            args,
        });
    }
}

fn grammar_mutation_error(error: ParseError) -> TransactionError {
    if error.code.as_deref() == Some("grammar.version_overflow") {
        TransactionError::VersionOverflow(error)
    } else {
        TransactionError::InvalidRuleSource(error)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar::Grammar;

    fn base() -> Grammar {
        Grammar::trusted_new("root <- 'x'").with_start_rule("root")
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
        let outcome = tx.commit().unwrap();
        assert_eq!(outcome.grammar.get_rule("root").unwrap().source, "'z'");
        assert!(!outcome.diff.changed_rules.is_empty());
    }

    #[test]
    fn add_rule_rejects_invalid_source_without_recording_op() {
        let mut tx = GrammarTransaction::new(&base());
        let err = tx
            .add_rule("bad", "[a")
            .expect_err("invalid rule source is rejected");
        let TransactionError::InvalidRuleSource(parse_error) = err else {
            panic!("expected InvalidRuleSource");
        };
        assert!(parse_error.message.contains("unterminated character class"));
        assert!(tx.current().get_rule("bad").is_none());
        assert!(tx.operation_log().is_empty());
    }

    #[test]
    fn add_rule_rejects_operation_index_overflow_without_mutating() {
        let mut tx = GrammarTransaction::new(&base());
        tx.op_index = usize::MAX;

        let err = tx
            .add_rule("overflow", "'y'")
            .expect_err("operation index overflow is rejected");

        assert!(matches!(err, TransactionError::OperationIndexOverflow));
        assert!(tx.current().get_rule("overflow").is_none());
        assert!(tx.operation_log().is_empty());
        assert_eq!(tx.op_index, usize::MAX);
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
        let g = Grammar::trusted_new("root <- 'x'\nextra <- 'y'").with_start_rule("root");
        let mut tx = GrammarTransaction::new(&g);
        tx.remove_rule("extra").unwrap();
        let outcome = tx.commit().unwrap();
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
        let g = Grammar::trusted_new("root <- 'x'\nnew_start <- 'y'").with_start_rule("root");
        let mut tx = GrammarTransaction::new(&g);
        tx.set_start("new_start").unwrap();
        let outcome = tx.commit().unwrap();
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
        let _ = tx.commit().unwrap();
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
        let g = Grammar::trusted_new("root <- missing_ref").with_start_rule("root");
        let tx = GrammarTransaction::new(&g); // strict = true
        let err = tx.commit().unwrap_err();
        assert!(matches!(err, TransactionError::ValidationFailed(_)));
    }

    #[test]
    fn non_strict_commit_succeeds_with_invalid_grammar() {
        let g = Grammar::trusted_new("root <- missing_ref").with_start_rule("root");
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
        let outcome = tx.commit().unwrap();
        let meta = outcome.grammar.metadata.get("root").expect("meta");
        assert_eq!(
            meta.get("return_type").and_then(|v| v.as_str()),
            Some("Expr")
        );
    }
}
