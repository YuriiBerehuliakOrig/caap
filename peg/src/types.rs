//! Core runtime data types: the [`ParseValue`] result tree, the [`ParserConfig`]
//! that tunes a run, lexer [`LexToken`]s, and the incremental-parse cache and
//! edit types.

use crate::error::ParseError;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const EXACT_PARSE_CACHE_ENTRY_LIMIT: usize = 32;

// ── Parse value ────────────────────────────────────────────────────────────

/// The runtime value produced by a successful parse.
///
/// Variants cover the common structural cases: nothing, raw text, an integer,
/// an annotated node with children, a named binding, and a span-decorated value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ParseValue {
    /// No value — a position-only match (lookahead, trivia, empty sequence).
    Nil,
    /// Matched source text.
    Text(Arc<str>),
    /// An integer value. The grammar engine never produces this directly (all
    /// terminals yield `Text`); it exists for host semantic actions that
    /// transform matched text into a number.
    Number(i64),
    /// A tagged node: a rule/construct name plus its child values.
    Node(Arc<str>, Arc<Vec<ParseValue>>),
    /// A named sub-value produced by a `name:expr` binding in a grammar rule.
    ///
    /// The inner value is `Arc`-wrapped (not `Box`) so cloning a `Named` — e.g.
    /// on a packrat memo hit — is an O(1) refcount bump rather than a deep copy.
    Named(Arc<str>, Arc<ParseValue>),
    /// A value decorated with the byte span `[start, end)` it covered.
    SpannedValue {
        /// `Arc`-wrapped for O(1) clones (see [`ParseValue::Named`]).
        value: Arc<ParseValue>,
        /// Inclusive start byte offset.
        start: usize,
        /// Exclusive end byte offset.
        end: usize,
    },
}

impl ParseValue {
    /// Wrap `self` in a `SpannedValue` covering `[start, end)`.
    pub fn spanned(self, start: usize, end: usize) -> Self {
        Self::SpannedValue {
            value: Arc::new(self),
            start,
            end,
        }
    }

    /// Move a value out of an `Arc<ParseValue>` without copying when uniquely
    /// owned (the common case for fresh parse output), cloning only if shared.
    pub fn unwrap_arc(value: Arc<ParseValue>) -> ParseValue {
        Arc::try_unwrap(value).unwrap_or_else(|shared| (*shared).clone())
    }

    /// Whether this is the [`Nil`](ParseValue::Nil) value.
    pub fn is_nil(&self) -> bool {
        matches!(self, Self::Nil)
    }

    /// Whether this is a [`SpannedValue`](ParseValue::SpannedValue).
    pub fn is_spanned(&self) -> bool {
        matches!(self, Self::SpannedValue { .. })
    }

    /// Unwrap the innermost non-spanned value.
    pub fn inner(&self) -> &Self {
        match self {
            Self::SpannedValue { value, .. } => value.inner(),
            other => other,
        }
    }

    /// Return the name and inner value if this is a `Named` variant.
    pub fn as_named(&self) -> Option<(&str, &Self)> {
        match self {
            Self::Named(name, value) => Some((name.as_ref(), value)),
            _ => None,
        }
    }

    /// Collect all `Named` children of a `Node` or `Named` wrapper into a map.
    pub fn named_bindings(&self) -> std::collections::HashMap<String, &Self> {
        let mut map = std::collections::HashMap::new();
        match self {
            Self::Named(name, value) => {
                map.insert(name.to_string(), value.as_ref());
            }
            Self::Node(_, children) => {
                for child in children.iter() {
                    if let Self::Named(name, value) = child {
                        map.insert(name.to_string(), value.as_ref());
                    }
                }
            }
            Self::SpannedValue { value, .. } => return value.named_bindings(),
            _ => {}
        }
        map
    }
}

// ── Prefix parse result ────────────────────────────────────────────────────

/// Result of `parse_prefix`: the value matched, how much input was consumed,
/// whether the match reached EOF, and any diagnostic messages.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompletedPrefixParse {
    /// The matched value, or `None` if the prefix did not parse.
    pub value: Option<ParseValue>,
    /// Bytes consumed from the start position.
    pub consumed: usize,
    /// Whether the match reached end-of-input.
    pub eof: bool,
    /// Diagnostic messages (empty on success).
    pub errors: Vec<String>,
}

impl CompletedPrefixParse {
    /// Whether a value was produced with no errors.
    pub fn ok(&self) -> bool {
        self.value.is_some() && self.errors.is_empty()
    }

    /// A failed prefix parse carrying a single diagnostic message.
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            value: None,
            consumed: 0,
            eof: false,
            errors: vec![message.into()],
        }
    }
}

// ── Parse cache ────────────────────────────────────────────────────────────

/// A whole-input parse result cached for exact-text reuse, keyed by the text,
/// grammar, and runtime signatures.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CachedResult {
    /// Hash of the input text.
    pub text_hash: u64,
    /// Signature of the grammar that produced the result.
    pub grammar_signature: u64,
    /// Signature of the runtime config that produced the result.
    pub runtime_signature: u64,
    /// The cached root value.
    pub output: Arc<ParseValue>,
}

/// A successful rule parse result persisted across incremental parse runs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PositionMemoEntry {
    /// End position (exclusive) of the matched span.
    pub end: usize,
    /// The cached rule value.
    pub value: Arc<ParseValue>,
    /// Whether the original parse committed via `cut` at or below this rule.
    /// `#[serde(default)]` keeps deserialisation backward-compatible with
    /// caches written before this field existed (legacy entries default to
    /// `false`, matching the previous always-false replay behaviour).
    #[serde(default)]
    pub cut: bool,
    /// Lowest byte index the parse *examined* to produce this result. Lookbehind
    /// can push it below the match start; otherwise it equals the start. `None`
    /// in legacy caches → callers fall back to the match start. This is the
    /// soundness datum for incremental reuse: an edit overlapping the examined
    /// interval (not merely the matched span) must invalidate the entry.
    #[serde(default)]
    pub read_lo: Option<usize>,
    /// One past the highest byte index the parse examined. Lookahead/trivia can
    /// push it past `end`; otherwise it equals `end`. `None` in legacy caches →
    /// callers fall back to `end`.
    #[serde(default)]
    pub read_hi: Option<usize>,
}

impl PositionMemoEntry {
    /// The examined byte interval `[lo, hi)`, always a superset of the matched
    /// span `[start, end)`. Falls back to the matched span for legacy entries
    /// that predate read-extent tracking.
    pub fn examined(&self, start: usize) -> (usize, usize) {
        let lo = self.read_lo.unwrap_or(start).min(start.min(self.end));
        let hi = self.read_hi.unwrap_or(self.end).max(start.max(self.end));
        (lo, hi)
    }
}

/// Position-level incremental memo for a single (grammar, runtime) context.
///
/// Stored inside `ParseCache` and reused across calls to `parse_incremental_many`.
/// On each edit, surviving entries are position-shifted; overlapping entries are
/// dropped.  Only successful rule outcomes are stored (failures are cheap to
/// re-derive and can become stale when surrounding text changes).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PositionCache {
    /// The text used to build this cache (needed to compute edit shifts).
    pub text: String,
    /// Signature of the grammar this cache was built against.
    pub grammar_hash: u64,
    /// Signature of the runtime config this cache was built against.
    pub runtime_signature: u64,
    /// Outer key: rule name.  Inner key: start position.
    pub memo:
        std::collections::HashMap<String, std::collections::HashMap<usize, PositionMemoEntry>>,
}

impl PositionCache {
    /// An empty position cache bound to the given text and signatures.
    pub fn new(text: impl Into<String>, grammar_hash: u64, runtime_signature: u64) -> Self {
        Self {
            text: text.into(),
            grammar_hash,
            runtime_signature,
            memo: std::collections::HashMap::new(),
        }
    }

    /// Merge a batch of exported rule-memo entries into this cache.
    pub fn absorb(&mut self, exported: impl IntoIterator<Item = ExportedMemoEntry>) {
        self.absorb_with_limit(exported, None);
    }

    /// Merge exported rule-memo entries and enforce a deterministic global cap.
    pub fn absorb_with_limit(
        &mut self,
        exported: impl IntoIterator<Item = ExportedMemoEntry>,
        global_limit: Option<usize>,
    ) {
        for entry in exported {
            self.memo.entry(entry.rule).or_default().insert(
                entry.start,
                PositionMemoEntry {
                    end: entry.end,
                    value: Arc::new(entry.value),
                    cut: entry.cut,
                    read_lo: Some(entry.read_lo),
                    read_hi: Some(entry.read_hi),
                },
            );
        }
        self.enforce_global_limit(global_limit);
    }

    /// Look up a rule result at `pos`.
    pub fn get(&self, rule: &str, pos: usize) -> Option<&PositionMemoEntry> {
        self.memo.get(rule)?.get(&pos)
    }

    /// Apply a sequential list of incremental edits to shift/invalidate entries.
    ///
    /// Each edit is `(edit_start, old_end, new_len)`. Reuse is decided on the
    /// **examined** interval `[read_lo, read_hi)` (a superset of the matched
    /// span): an entry whose examined interval overlaps `[edit_start, old_end)`
    /// is discarded, because the edit could change what that subtree saw —
    /// including bytes a lookahead/lookbehind read but did not consume. Entries
    /// whose examined interval is entirely after the edit are shifted by `delta`
    /// (both the matched span and the examined interval).
    pub fn apply_edits(&mut self, edits: &[(usize, usize, usize)]) {
        for &(edit_start, old_end, new_len) in edits {
            let Some(removed_len) = old_end.checked_sub(edit_start) else {
                self.memo.clear();
                return;
            };
            let delta = new_len as isize - removed_len as isize;
            let mut next_memo: std::collections::HashMap<
                String,
                std::collections::HashMap<usize, PositionMemoEntry>,
            > = std::collections::HashMap::new();

            for (rule, by_pos) in &self.memo {
                let mut shifted: std::collections::HashMap<usize, PositionMemoEntry> =
                    std::collections::HashMap::new();
                for (&start, entry) in by_pos {
                    let (read_lo, read_hi) = entry.examined(start);
                    // Invalidate if the *examined* interval overlaps the edit.
                    if read_lo < old_end && read_hi > edit_start {
                        continue;
                    }
                    if read_lo >= old_end {
                        // Entirely after the edit — shift span + examined interval.
                        let Some(new_start) =
                            shift_position_after_edit(start, removed_len, new_len)
                        else {
                            continue;
                        };
                        let Some(new_end) =
                            shift_position_after_edit(entry.end, removed_len, new_len)
                        else {
                            continue;
                        };
                        shifted.insert(new_start, entry.shifted_to(new_end, delta));
                    } else {
                        // Entirely before the edit — keep as-is.
                        shifted.insert(start, entry.clone());
                    }
                }
                if !shifted.is_empty() {
                    next_memo.insert(rule.clone(), shifted);
                }
            }
            self.memo = next_memo;
        }
    }

    /// Total number of cached entries across all rules.
    pub fn entry_count(&self) -> usize {
        self.memo.values().map(|m| m.len()).sum()
    }

    fn enforce_global_limit(&mut self, global_limit: Option<usize>) {
        let Some(limit) = global_limit else {
            return;
        };
        let count = self.entry_count();
        if count <= limit {
            return;
        }
        if limit == 0 {
            self.memo.clear();
            return;
        }
        // Drop `to_drop` entries without sorting — any eviction order is correct
        // because evicted entries are simply recomputed on the next parse pass.
        // Walking the HashMap directly is O(n) vs. the previous O(n log n) sort.
        let mut to_drop = count - limit;
        self.memo.retain(|_, by_pos| {
            if to_drop == 0 {
                return true;
            }
            let available = by_pos.len();
            if available <= to_drop {
                to_drop -= available;
                false
            } else {
                let keys: Vec<usize> = by_pos.keys().copied().take(to_drop).collect();
                for k in keys {
                    by_pos.remove(&k);
                }
                to_drop = 0;
                true
            }
        });
    }
}

fn shift_position_after_edit(position: usize, removed_len: usize, new_len: usize) -> Option<usize> {
    position.checked_sub(removed_len)?.checked_add(new_len)
}

impl PositionMemoEntry {
    fn shifted_to(&self, end: usize, delta: isize) -> Self {
        Self {
            end,
            value: Arc::clone(&self.value),
            cut: self.cut,
            read_lo: self.read_lo.and_then(|lo| shift_offset_signed(lo, delta)),
            read_hi: self.read_hi.and_then(|hi| shift_offset_signed(hi, delta)),
        }
    }
}

fn shift_offset_signed(offset: usize, delta: isize) -> Option<usize> {
    if delta >= 0 {
        offset.checked_add(delta as usize)
    } else {
        offset.checked_sub(delta.unsigned_abs())
    }
}

/// One successful rule outcome exported from a parse run into a [`PositionCache`].
///
/// Carries both the matched span `[start, end)` and the examined byte interval
/// `[read_lo, read_hi)` (a superset of the matched span) so the cache can decide
/// reuse soundly: an edit overlapping the examined interval invalidates the entry.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportedMemoEntry {
    /// Rule that produced the outcome.
    pub rule: String,
    /// Inclusive start byte offset of the matched span.
    pub start: usize,
    /// Exclusive end byte offset of the matched span.
    pub end: usize,
    /// Whether the parse committed via `cut` at or below this rule.
    pub cut: bool,
    /// Lowest byte index examined (≤ `start`).
    pub read_lo: usize,
    /// One past the highest byte index examined (≥ `end`).
    pub read_hi: usize,
    /// The matched value.
    pub value: ParseValue,
}

impl ExportedMemoEntry {
    /// Build an entry whose examined interval equals its matched span — for
    /// callers (and tests) that have no lookahead/lookbehind extent to record.
    pub fn span(
        rule: impl Into<String>,
        start: usize,
        end: usize,
        cut: bool,
        value: ParseValue,
    ) -> Self {
        Self {
            rule: rule.into(),
            start,
            end,
            cut,
            read_lo: start,
            read_hi: end,
            value,
        }
    }
}

/// Parse cache used by `parse_incremental_many`.
///
/// `entries` provides bounded whole-input exact-text-match reuse.
/// `pos_cache` provides position-level incremental reuse that survives edits.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParseCache {
    /// Bounded whole-input exact-text-match results.
    pub entries: Vec<CachedResult>,
    /// Position-level incremental memo that survives edits.
    pub pos_cache: Option<PositionCache>,
}

impl ParseCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of whole-input exact-match entries held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no whole-input entries are held.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn insert_exact_result(&mut self, entry: CachedResult) {
        self.entries.push(entry);
        let excess = self
            .entries
            .len()
            .saturating_sub(EXACT_PARSE_CACHE_ENTRY_LIMIT);
        if excess > 0 {
            self.entries.drain(0..excess);
        }
    }

    /// Number of position-level memo entries currently in the cache.
    pub fn position_entry_count(&self) -> usize {
        self.pos_cache
            .as_ref()
            .map(|cache| cache.entry_count())
            .unwrap_or(0)
    }
}

// ── Lex token ─────────────────────────────────────────────────────────────

/// A single token produced by an external lexer, for use with `tok()` expressions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LexToken {
    /// Token kind, e.g. "NAME", "NUMBER", "OP".
    pub kind: String,
    /// The matched text slice from the input.
    pub text: String,
    /// Byte offset where the token starts.
    pub start: usize,
    /// Byte offset where the token ends (exclusive).
    pub end: usize,
}

impl LexToken {
    /// Build a token of `kind` covering `text` over the byte span `[start, end)`.
    pub fn new(kind: impl Into<String>, text: impl Into<String>, start: usize, end: usize) -> Self {
        Self {
            kind: kind.into(),
            text: text.into(),
            start,
            end,
        }
    }
}

// ── Parser configuration ───────────────────────────────────────────────────

/// Fine-grained memoisation budget for a parse run.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct MemoPolicy {
    /// Cap on total memo entries across the whole parse run.
    global_budget: Option<usize>,
}

impl MemoPolicy {
    /// Build a policy with an optional cap on total memo entries.
    pub fn new(global_budget: Option<usize>) -> Result<Self, ParseError> {
        Ok(Self { global_budget })
    }

    /// The cap on total memo entries, if any.
    pub fn global_budget(&self) -> Option<usize> {
        self.global_budget
    }
}

impl<'de> Deserialize<'de> for MemoPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct MemoPolicyData {
            global_budget: Option<usize>,
        }

        let data = MemoPolicyData::deserialize(deserializer)?;
        Self::new(data.global_budget).map_err(serde::de::Error::custom)
    }
}

/// Preferred result shape for entry points that can return more than a value.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub enum ParserOutputMode {
    /// Produce a [`ParseValue`] (default).
    #[default]
    Value,
    /// Produce an [`AstNode`](crate::ast::AstNode) tree.
    Ast,
}

/// Configuration for a single parse invocation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParserConfig {
    /// When `true`, wrap the root result in a `SpannedValue`.
    pub return_spans: bool,
    /// Enable Packrat-style memoisation.
    pub memo: bool,
    /// Maximum input size (in bytes) to accept; also used as a step budget.
    pub max_steps: usize,
    /// When `true`, rules whose names match `invalid_rule_prefixes` are exposed
    /// to the caller instead of silently excluded.
    pub include_invalid_rules: bool,
    /// Override the default `["invalid_"]` prefix list for filtering.
    pub invalid_rule_prefixes: Option<Vec<String>>,
    /// Fine-grained memo budget/window configuration.
    pub memo_policy: Option<MemoPolicy>,
    /// Preferred output shape for APIs that can return more than `ParseValue`.
    #[serde(default)]
    pub output_mode: ParserOutputMode,
    /// Maximum expression-evaluation nesting depth before the parse fails with a
    /// `recursion_limit` error instead of overflowing the stack. Bounds recursive
    /// descent on deeply-nested input (a denial-of-service guard).
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
}

/// Default expression-nesting depth limit (`ParserConfig::max_depth`). Generous
/// for real grammars (cf. serde_json's default of 128) yet safely below the
/// stack-overflow threshold on common thread-stack sizes.
fn default_max_depth() -> usize {
    1024
}

impl Default for ParserConfig {
    fn default() -> Self {
        Self {
            return_spans: false,
            memo: true,
            max_steps: 4096,
            include_invalid_rules: false,
            invalid_rule_prefixes: None,
            memo_policy: None,
            output_mode: ParserOutputMode::Value,
            max_depth: default_max_depth(),
        }
    }
}

impl ParserConfig {
    /// Enable span-wrapping of the root result.
    pub fn with_spans(mut self) -> Self {
        self.return_spans = true;
        self
    }

    /// Toggle packrat memoisation.
    pub fn with_memo(mut self, memo: bool) -> Self {
        self.memo = memo;
        self
    }

    /// Set the maximum input size / step budget.
    pub fn with_max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// Set the maximum expression-nesting depth (recursion guard).
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Set the preferred output mode (value vs AST).
    pub fn with_output_mode(mut self, output_mode: ParserOutputMode) -> Self {
        self.output_mode = output_mode;
        self
    }
}

// ── Incremental edits ──────────────────────────────────────────────────────

/// A single text edit described as a half-open byte range to replace.
///
/// Invariants enforced at construction: `start ≤ old_end`, both non-negative.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IncrementalEdit {
    start: usize,
    old_end: usize,
    replacement: String,
}

impl IncrementalEdit {
    /// Create a new edit, returning `None` when `start > old_end`.
    pub fn new(start: usize, old_end: usize, replacement: impl Into<String>) -> Option<Self> {
        if start > old_end {
            return None;
        }
        Some(Self {
            start,
            old_end,
            replacement: replacement.into(),
        })
    }

    /// Inclusive start byte offset of the replaced range.
    pub fn start(&self) -> usize {
        self.start
    }

    /// Exclusive end byte offset of the replaced range.
    pub fn old_end(&self) -> usize {
        self.old_end
    }

    /// The replacement text.
    pub fn replacement(&self) -> &str {
        &self.replacement
    }

    /// Consume the edit, returning its replacement text.
    pub fn into_replacement(self) -> String {
        self.replacement
    }

    /// The number of bytes removed by this edit.
    pub fn removed_len(&self) -> usize {
        self.old_end - self.start
    }

    /// The number of bytes inserted by this edit.
    pub fn inserted_len(&self) -> usize {
        self.replacement.len()
    }

    /// Net byte-length change: positive = text grew, negative = text shrank.
    ///
    /// Returns `None` when the byte-length difference cannot be represented as
    /// an `isize`; callers that shift `usize` offsets must handle that case
    /// explicitly instead of relying on wrapping casts.
    pub fn delta(&self) -> Option<isize> {
        if self.inserted_len() >= self.removed_len() {
            isize::try_from(self.inserted_len() - self.removed_len()).ok()
        } else {
            isize::try_from(self.removed_len() - self.inserted_len())
                .ok()
                .and_then(isize::checked_neg)
        }
    }
}

impl<'de> Deserialize<'de> for IncrementalEdit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct IncrementalEditData {
            start: usize,
            old_end: usize,
            replacement: String,
        }

        let data = IncrementalEditData::deserialize(deserializer)?;
        Self::new(data.start, data.old_end, data.replacement).ok_or_else(|| {
            serde::de::Error::custom("incremental edit start must be less than or equal to old_end")
        })
    }
}

/// A sequential edit produced by `snapshot_edits_to_sequential`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompletedEdit {
    /// Replacement text inserted at `span`.
    pub text: String,
    /// The `(start, end)` byte range replaced, in original-text coordinates.
    pub span: (usize, usize),
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_value_inner_unwraps_nested_spanned() {
        let v = ParseValue::Text("hi".into()).spanned(0, 2).spanned(0, 2);
        assert_eq!(v.inner(), &ParseValue::Text("hi".into()));
    }

    #[test]
    fn parse_value_is_nil() {
        assert!(ParseValue::Nil.is_nil());
        assert!(!ParseValue::Text("x".into()).is_nil());
    }

    #[test]
    fn parse_value_is_spanned() {
        let v = ParseValue::Nil.spanned(0, 1);
        assert!(v.is_spanned());
    }

    #[test]
    fn parser_config_with_spans() {
        let c = ParserConfig::default().with_spans();
        assert!(c.return_spans);
    }

    #[test]
    fn parser_config_with_max_steps() {
        let c = ParserConfig::default().with_max_steps(100);
        assert_eq!(c.max_steps, 100);
    }

    #[test]
    fn memo_policy_deserialize_rejects_inactive_fields() {
        let err =
            serde_json::from_str::<MemoPolicy>(r#"{"global_budget":null,"session_budget":64}"#)
                .unwrap_err();
        assert!(err.to_string().contains("session_budget"));
    }

    #[test]
    fn position_cache_absorb_enforces_global_limit() {
        let mut cache = PositionCache::new("abc", 1, 1);
        cache.absorb_with_limit(
            [
                ExportedMemoEntry::span("b", 0, 1, false, ParseValue::Text("b0".into())),
                ExportedMemoEntry::span("a", 0, 1, false, ParseValue::Text("a0".into())),
                ExportedMemoEntry::span("a", 1, 2, false, ParseValue::Text("a1".into())),
            ],
            Some(2),
        );

        // The limit is enforced: exactly 2 entries survive. Which entries are
        // evicted is unspecified (retain uses HashMap order, not insertion order).
        assert_eq!(cache.entry_count(), 2);
    }

    #[test]
    fn position_cache_zero_global_limit_disables_storage() {
        let mut cache = PositionCache::new("abc", 1, 1);
        cache.absorb_with_limit(
            [ExportedMemoEntry::span(
                "a",
                0,
                1,
                false,
                ParseValue::Text("a".into()),
            )],
            Some(0),
        );

        assert_eq!(cache.entry_count(), 0);
    }

    #[test]
    fn position_cache_edit_shift_drops_entries_that_overflow() {
        let mut cache = PositionCache::new("abc", 1, 1);
        cache.absorb([ExportedMemoEntry::span(
            "a",
            usize::MAX - 1,
            usize::MAX,
            false,
            ParseValue::Text("a".into()),
        )]);

        cache.apply_edits(&[(0, 0, 2)]);

        assert_eq!(cache.entry_count(), 0);
    }

    #[test]
    fn apply_edits_invalidates_edit_in_examined_but_unmatched_region() {
        // Entry matched [0,1) but examined [0,5) (e.g. a `&"…"` lookahead read 4
        // bytes it did not consume). An edit at byte 3 — inside the examined
        // region but outside the matched span — must invalidate it.
        let mut cache = PositionCache::new("xZZZZ", 1, 1);
        cache.memo.entry("a".to_string()).or_default().insert(
            0,
            PositionMemoEntry {
                end: 1,
                value: Arc::new(ParseValue::Nil),
                cut: false,
                read_lo: Some(0),
                read_hi: Some(5),
            },
        );
        // Replace bytes [3,4) (1 byte → 1 byte, delta 0).
        cache.apply_edits(&[(3, 4, 1)]);
        assert_eq!(
            cache.entry_count(),
            0,
            "entry whose lookahead read the edited byte must be dropped"
        );
    }

    #[test]
    fn apply_edits_keeps_entry_when_edit_is_outside_examined_region() {
        // Entry matched [0,1), examined [0,3); an edit at byte 6 is disjoint from
        // the examined interval, so the entry survives unchanged.
        let mut cache = PositionCache::new("ab    cd", 1, 1);
        cache.memo.entry("a".to_string()).or_default().insert(
            0,
            PositionMemoEntry {
                end: 1,
                value: Arc::new(ParseValue::Nil),
                cut: false,
                read_lo: Some(0),
                read_hi: Some(3),
            },
        );
        cache.apply_edits(&[(6, 7, 1)]);
        assert_eq!(cache.entry_count(), 1, "disjoint edit must keep the entry");
        let entry = cache.get("a", 0).expect("entry survives");
        assert_eq!((entry.read_lo, entry.read_hi), (Some(0), Some(3)));
    }

    #[test]
    fn apply_edits_shifts_examined_interval_for_suffix_entry() {
        // An insertion before a suffix entry shifts both its matched span and its
        // examined interval by the same delta.
        let mut cache = PositionCache::new("0123456789", 1, 1);
        cache.memo.entry("a".to_string()).or_default().insert(
            5,
            PositionMemoEntry {
                end: 7,
                value: Arc::new(ParseValue::Nil),
                cut: false,
                read_lo: Some(5),
                read_hi: Some(9),
            },
        );
        // Insert 2 bytes at position 0: [0,0) → 2 bytes, delta +2.
        cache.apply_edits(&[(0, 0, 2)]);
        let entry = cache.get("a", 7).expect("suffix entry shifted to +2");
        assert_eq!(entry.end, 9);
        assert_eq!((entry.read_lo, entry.read_hi), (Some(7), Some(11)));
    }

    #[test]
    fn position_cache_malformed_edit_clears_cache() {
        let mut cache = PositionCache::new("abc", 1, 1);
        cache.absorb([ExportedMemoEntry::span(
            "a",
            1,
            2,
            false,
            ParseValue::Text("a".into()),
        )]);

        cache.apply_edits(&[(3, 1, 0)]);

        assert_eq!(cache.entry_count(), 0);
    }

    #[test]
    fn incremental_edit_delta_insert() {
        let e = IncrementalEdit::new(3, 3, "hello").unwrap();
        assert_eq!(e.removed_len(), 0);
        assert_eq!(e.inserted_len(), 5);
        assert_eq!(e.delta(), Some(5));
    }

    #[test]
    fn incremental_edit_delta_delete() {
        let e = IncrementalEdit::new(1, 4, "").unwrap();
        assert_eq!(e.removed_len(), 3);
        assert_eq!(e.delta(), Some(-3));
    }

    #[test]
    fn incremental_edit_new_rejects_invalid_range() {
        assert!(IncrementalEdit::new(5, 3, "x").is_none());
    }

    #[test]
    fn incremental_edit_new_accepts_empty_range() {
        let e = IncrementalEdit::new(3, 3, "").unwrap();
        assert_eq!(e.start(), 3);
        assert_eq!(e.old_end(), 3);
        assert_eq!(e.replacement(), "");
        assert_eq!(e.delta(), Some(0));
    }

    #[test]
    fn incremental_edit_deserialize_rejects_invalid_range() {
        let err =
            serde_json::from_str::<IncrementalEdit>(r#"{"start":5,"old_end":3,"replacement":"x"}"#)
                .unwrap_err();
        assert!(err
            .to_string()
            .contains("start must be less than or equal to old_end"));
    }

    #[test]
    fn parse_cache_len_and_empty() {
        let c = ParseCache::new();
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn parse_cache_exact_results_are_bounded() {
        let mut cache = ParseCache::new();
        for index in 0..(EXACT_PARSE_CACHE_ENTRY_LIMIT + 3) {
            cache.insert_exact_result(CachedResult {
                text_hash: index as u64,
                grammar_signature: 1,
                runtime_signature: 1,
                output: Arc::new(ParseValue::Number(index as i64)),
            });
        }

        assert_eq!(cache.len(), EXACT_PARSE_CACHE_ENTRY_LIMIT);
        assert_eq!(cache.entries.first().map(|entry| entry.text_hash), Some(3));
        assert_eq!(
            cache.entries.last().map(|entry| entry.text_hash),
            Some((EXACT_PARSE_CACHE_ENTRY_LIMIT + 2) as u64)
        );
    }

    #[test]
    fn completed_prefix_parse_ok() {
        let p = CompletedPrefixParse {
            value: Some(ParseValue::Text("x".into())),
            consumed: 1,
            eof: true,
            errors: vec![],
        };
        assert!(p.ok());
    }

    #[test]
    fn completed_prefix_parse_not_ok_when_no_value() {
        let p = CompletedPrefixParse {
            value: None,
            consumed: 0,
            eof: false,
            errors: vec!["failed".into()],
        };
        assert!(!p.ok());
    }
}
