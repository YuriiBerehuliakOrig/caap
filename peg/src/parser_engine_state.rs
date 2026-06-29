use std::collections::{HashMap, HashSet};

use crate::driver::ParserConfigContext;
use crate::expr::PegExpr;
use crate::invalid_rules::InvalidRulePolicy;
use crate::parser_compile::{ChoiceDispatch, CompiledGrammar, MemoRuntimeConfig};
use crate::skip::BoxedSkipStrategy;
use crate::types::ParseValue;

// ── ParseOutcome ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) enum ParseOutcome {
    Success {
        pos: usize,
        value: ParseValue,
        cut: bool,
    },
    Failure {
        pos: usize,
        cut: bool,
    },
}

impl ParseOutcome {
    pub(crate) fn success(pos: usize, value: ParseValue) -> Self {
        Self::Success {
            pos,
            value,
            cut: false,
        }
    }

    pub(crate) fn success_with_cut(pos: usize) -> Self {
        Self::Success {
            pos,
            value: ParseValue::Nil,
            cut: true,
        }
    }

    pub(crate) fn failure(pos: usize) -> Self {
        Self::Failure { pos, cut: false }
    }

    pub(crate) fn failure_with_cut(pos: usize) -> Self {
        Self::Failure { pos, cut: true }
    }

    pub(crate) fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }
}

pub(crate) fn outcome_end(outcome: &ParseOutcome) -> usize {
    match outcome {
        ParseOutcome::Success { pos, .. } | ParseOutcome::Failure { pos, .. } => *pos,
    }
}

// ── Read-extent tracking (incremental subtree reuse) ───────────────────────

/// The half-open byte interval `[lo, hi)` of input a parse *examined* while
/// producing a result — which is generally a **superset** of the matched span
/// `[start, end)`: positive/negative lookahead and trivia push `hi` past `end`,
/// and lookbehind pushes `lo` below `start`.
///
/// This is the soundness datum for incremental subtree reuse: a memoized result
/// at `[start, end)` may only be replayed across an edit when the edit region is
/// disjoint from its examined interval — not merely from its matched span. An
/// edit inside the examined-but-not-matched tail (e.g. a `&"…"` lookahead) can
/// flip the result even though the matched bytes are untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReadExtent {
    pub(crate) lo: usize,
    pub(crate) hi: usize,
}

impl ReadExtent {
    /// An empty extent anchored at `pos` (examines nothing yet).
    pub(crate) fn empty(pos: usize) -> Self {
        Self { lo: pos, hi: pos }
    }

    /// Widen to include the half-open byte interval `[lo, hi)`.
    #[inline]
    pub(crate) fn note(&mut self, lo: usize, hi: usize) {
        if lo < self.lo {
            self.lo = lo;
        }
        if hi > self.hi {
            self.hi = hi;
        }
    }

    /// Fold another extent into this one (union of examined bytes).
    #[inline]
    pub(crate) fn merge(&mut self, other: ReadExtent) {
        self.note(other.lo, other.hi);
    }
}

// ── Evaluator sub-types ────────────────────────────────────────────────────

/// Immutable references to compiled grammar data for the duration of one parse.
pub(crate) struct ParseCtx<'a> {
    pub(crate) rules: &'a HashMap<String, PegExpr>,
    pub(crate) rule_params: &'a HashMap<String, Vec<String>>,
    pub(crate) text: &'a str,
    pub(crate) grammar_start: &'a str,
    pub(crate) metadata_keys: &'a [String],
    pub(crate) imports: &'a HashMap<String, CompiledGrammar>,
    /// Import aliases pre-sorted at compile time (see `CompiledGrammar`).
    pub(crate) import_aliases: &'a [String],
    pub(crate) left_recursive_rules: &'a HashSet<String>,
    pub(crate) lr_min_step: &'a HashMap<String, usize>,
    pub(crate) rule_dispatch: &'a HashMap<String, ChoiceDispatch>,
    pub(crate) rule_scc: &'a HashMap<String, usize>,
}

impl<'a> ParseCtx<'a> {
    pub(crate) fn from_compiled(compiled: &'a CompiledGrammar, text: &'a str) -> Self {
        Self {
            rules: &compiled.rules,
            rule_params: &compiled.rule_params,
            text,
            grammar_start: &compiled.start_rule,
            metadata_keys: &compiled.metadata_keys,
            imports: &compiled.imports,
            import_aliases: &compiled.import_aliases_sorted,
            left_recursive_rules: &compiled.left_recursive_rules,
            lr_min_step: &compiled.lr_min_step,
            rule_dispatch: &compiled.rule_dispatch,
            rule_scc: &compiled.rule_scc,
        }
    }

    /// Copy the borrowed references into a fresh `ParseCtx` with the same
    /// lifetime — used to spin up an isolated sub-parse evaluator.
    pub(crate) fn reborrow(&self) -> ParseCtx<'a> {
        ParseCtx {
            rules: self.rules,
            rule_params: self.rule_params,
            text: self.text,
            grammar_start: self.grammar_start,
            metadata_keys: self.metadata_keys,
            imports: self.imports,
            import_aliases: self.import_aliases,
            left_recursive_rules: self.left_recursive_rules,
            lr_min_step: self.lr_min_step,
            rule_dispatch: self.rule_dispatch,
            rule_scc: self.rule_scc,
        }
    }
}

/// Parse configuration that is fixed at call time and never mutated during evaluation.
pub(crate) struct EvalCfg<'a> {
    pub(crate) use_memo: bool,
    pub(crate) memo_rule_limit: Option<usize>,
    pub(crate) invalid_rule_policy: InvalidRulePolicy,
    pub(crate) trivia: Option<BoxedSkipStrategy>,
    pub(crate) driver: Option<&'a dyn crate::driver::ParseDriver>,
    pub(crate) pos_seed: Option<&'a crate::types::PositionCache>,
    pub(crate) lex_tokens: Option<std::sync::Arc<Vec<crate::types::LexToken>>>,
    pub(crate) config_context: ParserConfigContext,
    /// When `true`, the evaluator tracks per-rule read extents (the byte interval
    /// each subtree examined) so they can be exported to the incremental position
    /// cache. Off for plain `parse()` — the machinery (frame stack, regex
    /// extent DFA) is then entirely skipped, so a non-incremental parse pays
    /// nothing for it.
    pub(crate) track_read_extent: bool,
    /// When `true`, the evaluator accumulates per-rule profiling counters. Off
    /// for ordinary parses so the hot path pays nothing.
    pub(crate) profile_enabled: bool,
}

impl<'a> EvalCfg<'a> {
    pub(crate) fn new(
        invalid_rule_policy: InvalidRulePolicy,
        trivia: Option<BoxedSkipStrategy>,
        memo: MemoRuntimeConfig,
        config_context: ParserConfigContext,
    ) -> Self {
        Self {
            use_memo: memo.enabled,
            memo_rule_limit: memo.rule_limit,
            invalid_rule_policy,
            trivia,
            driver: None,
            pos_seed: None,
            lex_tokens: None,
            config_context,
            track_read_extent: false,
            profile_enabled: false,
        }
    }

    /// Build a configuration for an isolated sub-parse: the cheap, cloneable
    /// fields are copied; the host hook (`driver`), the trace, the
    /// position seed, and any token stream are dropped so the sub-parse cannot
    /// re-enter the driver or share mutable run state.
    pub(crate) fn fork_isolated(&self) -> Self {
        Self {
            use_memo: self.use_memo,
            memo_rule_limit: self.memo_rule_limit,
            invalid_rule_policy: self.invalid_rule_policy.clone(),
            trivia: self.trivia.clone(),
            driver: None,
            pos_seed: None,
            lex_tokens: None,
            config_context: self.config_context.clone(),
            track_read_extent: false,
            profile_enabled: false,
        }
    }
}

pub(crate) struct PegEvaluatorInit<'a> {
    pub(crate) ctx: ParseCtx<'a>,
    pub(crate) cfg: EvalCfg<'a>,
    pub(crate) indentation_enabled: bool,
}

/// Mutable state accumulated during a single parse run.
pub(crate) struct EvalState<'a> {
    /// Memo cache keyed by `(rule, pos, facet)` — `&'a str` avoids per-lookup
    /// String allocations; `facet` is the host driver's state digest
    /// ([`crate::driver::MemoFacet`]), `0` for state-independent (pure) results.
    /// Keying on the facet keeps packrat sound when a rule's outcome depends on
    /// host state: a different state digest is a different key, so a stale result
    /// is never replayed, while a matching digest still reuses the cache.
    pub(crate) rule_memo: HashMap<(&'a str, usize, u64), ParseOutcome>,
    pub(crate) diag: crate::parser_engine_diag::DiagnosticsState,
    pub(crate) layout: crate::parser_engine_diag::LayoutState,
    /// Lazily computed line offsets (only when an error needs line/col info).
    pub(crate) line_offsets: Option<Vec<usize>>,
    /// Stack of parameter bindings for parametric rule calls.
    pub(crate) params: Vec<HashMap<String, PegExpr>>,
    pub(crate) rule_stack: Vec<&'a str>,
    pub(crate) trivia_on: bool,
    pub(crate) expr_steps: usize,
    /// Current expression-evaluation nesting depth (the recursion guard counter).
    /// Incremented on entry to `parse_expr`, decremented on exit, so it tracks the
    /// live recursive-descent stack depth.
    pub(crate) expr_depth: usize,
    /// Captured text of `name:` bindings, consulted by `backref("name")`.
    /// Last-write-wins; not rolled back on backtracking (intended for linear
    /// capture-then-reference patterns such as heredocs / matched tags).
    pub(crate) captures: HashMap<String, String>,
    /// Stack of read-extent accumulators, one frame per active rule body. Leaf
    /// matchers widen the top frame via `note_read`; on rule exit the frame is
    /// popped and merged into its parent, so each rule's frame ends up holding
    /// the union of every byte its subtree examined. Empty outside any rule.
    pub(crate) read_stack: Vec<ReadExtent>,
    /// Read extent of the most recently completed rule body, published by
    /// `parse_rule_body` for `parse_rule_inner` to memoize alongside the result.
    pub(crate) last_read_extent: ReadExtent,
    /// Per-`(rule, pos, facet)` examined interval, parallel to `rule_memo`.
    /// Consulted on a memo / position-seed hit so the reused subtree's examined
    /// bytes still bubble into the caller's frame (the caller's outcome depends
    /// on what the child looked at, even when the child itself is not re-run).
    pub(crate) read_memo: HashMap<(&'a str, usize, u64), ReadExtent>,
    /// Per-rule profiling counters; `Some` only when profiling is enabled.
    pub(crate) profile: Option<crate::profile::ProfileCollector<'a>>,
    /// `(scc_index, pos)` pairs whose left-recursive SCC is currently being
    /// grown (seed-grow). Lets only the SCC *head* drive growth while the other
    /// involved rules reuse the head's seed — the key to supporting indirect /
    /// mutual left recursion, not just direct.
    pub(crate) lr_growing: HashSet<(usize, usize)>,
    /// Skippers built lazily for `with_trivia("spec", …)` regions, keyed by spec
    /// (owned, so no lifetime juggling). `None` value = the `"none"` spec.
    pub(crate) trivia_cache: HashMap<String, Option<BoxedSkipStrategy>>,
    /// Stack of active `with_trivia` specs; the top overrides the grammar's
    /// default skipper inside the scoped region.
    pub(crate) trivia_override_stack: Vec<String>,
}

// ── Utility helpers ────────────────────────────────────────────────────────

pub(crate) fn fnv_hash_64(input: &[u8]) -> u64 {
    const BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = BASIS;
    for byte in input {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}
