use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ── Parse value ────────────────────────────────────────────────────────────

/// The runtime value produced by a successful parse.
///
/// Variants cover the common structural cases: nothing, raw text, an integer,
/// an annotated node with children, a named binding, and a span-decorated value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ParseValue {
    Nil,
    Text(String),
    Number(i64),
    Node(String, Vec<ParseValue>),
    /// A named sub-value produced by a `name:expr` binding in a grammar rule.
    Named(String, Box<ParseValue>),
    SpannedValue {
        value: Box<ParseValue>,
        start: usize,
        end: usize,
    },
}

impl ParseValue {
    /// Wrap `self` in a `SpannedValue` covering `[start, end)`.
    pub fn spanned(self, start: usize, end: usize) -> Self {
        Self::SpannedValue {
            value: Box::new(self),
            start,
            end,
        }
    }

    pub fn is_nil(&self) -> bool {
        matches!(self, Self::Nil)
    }

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
            Self::Named(name, value) => Some((name.as_str(), value)),
            _ => None,
        }
    }

    /// Collect all `Named` children of a `Node` or `Named` wrapper into a map.
    pub fn named_bindings(&self) -> std::collections::HashMap<String, &Self> {
        let mut map = std::collections::HashMap::new();
        match self {
            Self::Named(name, value) => {
                map.insert(name.clone(), value.as_ref());
            }
            Self::Node(_, children) => {
                for child in children {
                    if let Self::Named(name, value) = child {
                        map.insert(name.clone(), value.as_ref());
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
    pub value: Option<ParseValue>,
    pub consumed: usize,
    pub eof: bool,
    pub errors: Vec<String>,
}

impl CompletedPrefixParse {
    pub fn ok(&self) -> bool {
        self.value.is_some() && self.errors.is_empty()
    }
}

// ── Parse cache ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CachedResult {
    pub text_hash: u64,
    pub grammar_signature: u64,
    pub runtime_signature: u64,
    pub output: ParseValue,
}

/// A successful rule parse result persisted across incremental parse runs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PositionMemoEntry {
    /// End position (exclusive) of the matched span.
    pub end: usize,
    pub value: ParseValue,
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
    pub grammar_hash: u64,
    pub runtime_signature: u64,
    /// Outer key: rule name.  Inner key: start position.
    pub memo:
        std::collections::HashMap<String, std::collections::HashMap<usize, PositionMemoEntry>>,
}

impl PositionCache {
    pub fn new(text: impl Into<String>, grammar_hash: u64, runtime_signature: u64) -> Self {
        Self {
            text: text.into(),
            grammar_hash,
            runtime_signature,
            memo: std::collections::HashMap::new(),
        }
    }

    /// Merge a batch of exported rule-memo entries into this cache.
    pub fn absorb(
        &mut self,
        exported: impl IntoIterator<Item = (String, usize, usize, ParseValue)>,
    ) {
        for (rule, start, end, value) in exported {
            self.memo
                .entry(rule)
                .or_default()
                .insert(start, PositionMemoEntry { end, value });
        }
    }

    /// Look up a rule result at `pos`.
    pub fn get(&self, rule: &str, pos: usize) -> Option<&PositionMemoEntry> {
        self.memo.get(rule)?.get(&pos)
    }

    /// Apply a sequential list of incremental edits to shift/invalidate entries.
    ///
    /// Each edit is `(edit_start, old_end, new_len)`.  Entries whose `[start, end)`
    /// overlaps `[edit_start, old_end)` are discarded.  Entries that start at or
    /// after `old_end` are shifted by `delta`.
    pub fn apply_edits(&mut self, edits: &[(usize, usize, usize)]) {
        for &(edit_start, old_end, new_len) in edits {
            let delta = new_len as isize - (old_end - edit_start) as isize;
            let mut next_memo: std::collections::HashMap<
                String,
                std::collections::HashMap<usize, PositionMemoEntry>,
            > = std::collections::HashMap::new();

            for (rule, by_pos) in &self.memo {
                let mut shifted: std::collections::HashMap<usize, PositionMemoEntry> =
                    std::collections::HashMap::new();
                for (&start, entry) in by_pos {
                    let end = entry.end;
                    // Invalidate if the entry's span overlaps the edit region.
                    if start < old_end && end > edit_start {
                        continue;
                    }
                    if start >= old_end {
                        // Entry is entirely after the edit — shift.
                        let new_start = (start as isize + delta) as usize;
                        let new_end = (end as isize + delta) as usize;
                        shifted.insert(
                            new_start,
                            PositionMemoEntry {
                                end: new_end,
                                value: entry.value.clone(),
                            },
                        );
                    } else {
                        // Entry is entirely before the edit — keep as-is.
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
}

/// Parse cache used by `parse_incremental_many`.
///
/// `entries` provides fast exact-text-match reuse (backward compatible).
/// `pos_cache` provides position-level incremental reuse that survives edits.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParseCache {
    pub entries: Vec<CachedResult>,
    pub pos_cache: Option<PositionCache>,
}

impl ParseCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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
    pub fn new(kind: impl Into<String>, text: impl Into<String>, start: usize, end: usize) -> Self {
        Self {
            kind: kind.into(),
            text: text.into(),
            start,
            end,
        }
    }
}

// ── Parse trace events ────────────────────────────────────────────────────

/// A single parser trace event emitted when a named rule is entered, exited, or fails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseEvent {
    /// `"enter"`, `"exit"`, or `"fail"`.
    pub kind: &'static str,
    pub rule: String,
    pub pos: usize,
}

/// Optional per-rule trace callback. Called on every rule entry, successful exit, and failure.
pub type TraceCallback = Arc<dyn Fn(&ParseEvent) + Send + Sync>;

/// Newtype wrapper so `TraceCallback` can live in a `#[derive(Clone)]` struct.
#[derive(Clone)]
pub struct TraceCallbackHolder(pub TraceCallback);

impl std::fmt::Debug for TraceCallbackHolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TraceCallback")
    }
}

impl PartialEq for TraceCallbackHolder {
    fn eq(&self, _other: &Self) -> bool {
        false
    }
}

impl Eq for TraceCallbackHolder {}

// ── Parser configuration ───────────────────────────────────────────────────

/// Configuration for a single parse invocation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoPolicy {
    /// Cap on total memo entries across the whole parse run.
    pub global_budget: Option<usize>,
    /// Cap on per-session (node-level) memo entries.
    pub session_budget: Option<usize>,
    /// Sliding-window size for node-memo lookups (in bytes).
    pub region_window: Option<usize>,
    /// How often to prune stale memo entries (every N rule calls).
    pub prune_cadence: Option<usize>,
}

impl MemoPolicy {
    pub fn new(
        global_budget: Option<usize>,
        session_budget: Option<usize>,
        region_window: Option<usize>,
        prune_cadence: Option<usize>,
    ) -> Result<Self, String> {
        for (name, val) in [
            ("global_budget", global_budget),
            ("session_budget", session_budget),
            ("region_window", region_window),
        ] {
            if val == Some(0) && name == "prune_cadence" {
                return Err(format!(
                    "MemoPolicy.{name} must be a positive integer when provided"
                ));
            }
        }
        if prune_cadence == Some(0) {
            return Err("MemoPolicy.prune_cadence must be a positive integer when provided".into());
        }
        Ok(Self {
            global_budget,
            session_budget,
            region_window,
            prune_cadence,
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub enum ParserOutputMode {
    #[default]
    Value,
    Ast,
}

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
    /// Optional per-rule trace callback. Not serialized.
    #[serde(skip)]
    pub trace: Option<TraceCallbackHolder>,
    /// Preferred output shape for APIs that can return more than `ParseValue`.
    #[serde(default)]
    pub output_mode: ParserOutputMode,
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
            trace: None,
            output_mode: ParserOutputMode::Value,
        }
    }
}

impl ParserConfig {
    /// Return a new config with selected fields overridden.
    pub fn with_updates(
        &self,
        return_spans: bool,
        include_invalid_rules: Option<bool>,
        invalid_rule_prefixes: Option<Vec<String>>,
    ) -> Self {
        Self {
            return_spans,
            memo: self.memo,
            max_steps: self.max_steps,
            include_invalid_rules: include_invalid_rules.unwrap_or(self.include_invalid_rules),
            invalid_rule_prefixes: invalid_rule_prefixes
                .or_else(|| self.invalid_rule_prefixes.clone()),
            memo_policy: None,
            trace: self.trace.clone(),
            output_mode: self.output_mode.clone(),
        }
    }

    pub fn with_trace(mut self, callback: impl Fn(&ParseEvent) + Send + Sync + 'static) -> Self {
        self.trace = Some(TraceCallbackHolder(Arc::new(callback)));
        self
    }

    pub fn with_spans(mut self) -> Self {
        self.return_spans = true;
        self
    }

    pub fn with_memo(mut self, memo: bool) -> Self {
        self.memo = memo;
        self
    }

    pub fn with_max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    pub fn with_output_mode(mut self, output_mode: ParserOutputMode) -> Self {
        self.output_mode = output_mode;
        self
    }
}

// ── Incremental edits ──────────────────────────────────────────────────────

/// A single text edit described as a half-open byte range to replace.
///
/// Invariants enforced at construction: `start ≤ old_end`, both non-negative.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IncrementalEdit {
    pub start: usize,
    pub old_end: usize,
    pub replacement: String,
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

    /// Panics if the invariant is violated; use in tests / trusted contexts only.
    pub fn new_unchecked(start: usize, old_end: usize, replacement: impl Into<String>) -> Self {
        assert!(start <= old_end, "IncrementalEdit: start > old_end");
        Self {
            start,
            old_end,
            replacement: replacement.into(),
        }
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
    pub fn delta(&self) -> isize {
        self.inserted_len() as isize - self.removed_len() as isize
    }
}

/// A sequential edit produced by `snapshot_edits_to_sequential`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompletedEdit {
    pub text: String,
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
    fn incremental_edit_delta_insert() {
        let e = IncrementalEdit::new_unchecked(3, 3, "hello");
        assert_eq!(e.removed_len(), 0);
        assert_eq!(e.inserted_len(), 5);
        assert_eq!(e.delta(), 5);
    }

    #[test]
    fn incremental_edit_delta_delete() {
        let e = IncrementalEdit::new_unchecked(1, 4, "");
        assert_eq!(e.removed_len(), 3);
        assert_eq!(e.delta(), -3);
    }

    #[test]
    fn incremental_edit_new_rejects_invalid_range() {
        assert!(IncrementalEdit::new(5, 3, "x").is_none());
    }

    #[test]
    fn incremental_edit_new_accepts_empty_range() {
        let e = IncrementalEdit::new(3, 3, "").unwrap();
        assert_eq!(e.delta(), 0);
    }

    #[test]
    fn parse_cache_len_and_empty() {
        let c = ParseCache::new();
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
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
