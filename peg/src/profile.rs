//! Parse profiling — per-rule call/memo statistics for grammar authors.
//!
//! Opt-in via [`crate::ParseRequest::run_profiled`]. When profiling is off the
//! collector is never allocated and the hot path pays nothing.
//!
//! ```
//! use caap_peg::{Grammar, ParseRequest};
//!
//! let grammar = Grammar::trusted_new("root <- [a-z]+").with_start_rule("root");
//! let (_value, profile) = ParseRequest::new(&grammar)
//!     .run_profiled("abcdef")
//!     .unwrap();
//! assert!(profile.total_calls() >= 1);
//! for (rule, stats) in profile.hottest(3) {
//!     println!("{rule}: {} calls, {:.0}% memo hits", stats.calls, stats.memo_hit_rate() * 100.0);
//! }
//! ```

use std::collections::HashMap;

/// Per-rule parse statistics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleStats {
    /// Times the rule was entered (memo hits + seed hits + body executions).
    pub calls: u64,
    /// Calls served from the in-run packrat memo (no re-parse).
    pub memo_hits: u64,
    /// Calls served from the cross-run position-cache seed.
    pub seed_hits: u64,
    /// Calls that actually executed the rule body (the real parsing work).
    pub body_runs: u64,
    /// Body executions that ended in failure.
    pub failures: u64,
}

impl RuleStats {
    /// Fraction of calls answered without executing the body (memo + seed hits),
    /// in `0.0..=1.0`. `0.0` when the rule was never called.
    pub fn memo_hit_rate(&self) -> f64 {
        if self.calls == 0 {
            return 0.0;
        }
        (self.memo_hits + self.seed_hits) as f64 / self.calls as f64
    }
}

/// Aggregate profile of one parse run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParseProfile {
    /// Per-rule statistics keyed by canonical rule name.
    pub rules: HashMap<String, RuleStats>,
    /// Total `parse_expr` steps consumed (the work budget meter).
    pub expr_steps: u64,
    /// Furthest byte position reached (the best-error frontier).
    pub furthest: usize,
}

impl ParseProfile {
    /// Total rule calls across all rules.
    pub fn total_calls(&self) -> u64 {
        self.rules.values().map(|s| s.calls).sum()
    }

    /// Total body executions across all rules (the re-parse work the memo did
    /// not save).
    pub fn total_body_runs(&self) -> u64 {
        self.rules.values().map(|s| s.body_runs).sum()
    }

    /// Overall memo+seed hit rate across all calls (`0.0..=1.0`).
    pub fn memo_hit_rate(&self) -> f64 {
        let calls = self.total_calls();
        if calls == 0 {
            return 0.0;
        }
        let hits: u64 = self.rules.values().map(|s| s.memo_hits + s.seed_hits).sum();
        hits as f64 / calls as f64
    }

    /// The `n` rules with the most body executions (the genuine hot spots),
    /// highest first. Ties broken by rule name for determinism.
    pub fn hottest(&self, n: usize) -> Vec<(&str, &RuleStats)> {
        let mut entries: Vec<(&str, &RuleStats)> =
            self.rules.iter().map(|(k, v)| (k.as_str(), v)).collect();
        entries.sort_by(|a, b| b.1.body_runs.cmp(&a.1.body_runs).then_with(|| a.0.cmp(b.0)));
        entries.truncate(n);
        entries
    }
}

/// Internal accumulator used during a profiled parse. Keyed by the borrowed
/// `&'a str` rule name to avoid per-increment allocations; converted to the
/// owned [`ParseProfile`] when the run finishes.
#[derive(Default)]
pub(crate) struct ProfileCollector<'a> {
    rules: HashMap<&'a str, RuleStats>,
}

impl<'a> ProfileCollector<'a> {
    pub(crate) fn new() -> Self {
        Self {
            rules: HashMap::new(),
        }
    }

    #[inline]
    fn entry(&mut self, rule: &'a str) -> &mut RuleStats {
        self.rules.entry(rule).or_default()
    }

    #[inline]
    pub(crate) fn record_call(&mut self, rule: &'a str) {
        self.entry(rule).calls += 1;
    }

    #[inline]
    pub(crate) fn record_memo_hit(&mut self, rule: &'a str) {
        self.entry(rule).memo_hits += 1;
    }

    #[inline]
    pub(crate) fn record_seed_hit(&mut self, rule: &'a str) {
        self.entry(rule).seed_hits += 1;
    }

    #[inline]
    pub(crate) fn record_body_run(&mut self, rule: &'a str, failed: bool) {
        let stats = self.entry(rule);
        stats.body_runs += 1;
        if failed {
            stats.failures += 1;
        }
    }

    pub(crate) fn finish(self, expr_steps: u64, furthest: usize) -> ParseProfile {
        ParseProfile {
            rules: self
                .rules
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            expr_steps,
            furthest,
        }
    }
}
