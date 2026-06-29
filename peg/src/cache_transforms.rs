//! Incremental cache transplant orchestration.
//!
//! Ports `peg/engine/incremental_transforms.py`, `cache_persist.py`, and
//! `cache_seed.py`.  Transforms a persisted `PositionCache` across text edits so that
//! surviving memo entries are carried forward into the next parse run.

use std::collections::HashMap;
use std::sync::Arc;

use crate::incremental_edits::{
    compile_incremental_edit_steps, shift_offset_by_delta, BoundaryTransplant,
    IncrementalEditError, IncrementalEditStep, SpanShift,
};
use crate::types::{IncrementalEdit, ParseValue, PositionCache, PositionMemoEntry};
use crate::values::contains_spanned;

type RuleMemoTable = HashMap<String, HashMap<usize, PositionMemoEntry>>;

// ── PayloadProjection ──────────────────────────────────────────────────────

/// Error raised when a cached payload cannot be safely span-projected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnmappablePayloadError(pub String);

impl std::fmt::Display for UnmappablePayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UnmappablePayloadError {}

/// Projects span offsets inside a cached `ParseValue` tree when text shifts.
///
/// After an incremental edit the in-cache parse values contain stale byte
/// offsets. `PayloadProjection` walks the value tree and applies the
/// `span_shifts` list so all spans reflect the new text layout.
#[derive(Clone, Debug, Default)]
pub struct PayloadProjection {
    /// Ordered list of (shift_from, delta) pairs. Each pair means: for every
    /// span endpoint `>= shift_from`, add `delta` to it.
    pub span_shifts: Vec<SpanShift>,
}

impl PayloadProjection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_span_shifts(mut self, shifts: Vec<SpanShift>) -> Self {
        self.span_shifts = shifts;
        self
    }

    /// Apply span projection to a `ParseValue` tree.
    ///
    /// Returns `Err` if a `RawBlock` node is encountered while span-shifting
    /// (raw blocks carry absolute text ranges that cannot be safely adjusted).
    pub fn project(&self, value: ParseValue) -> Result<ParseValue, UnmappablePayloadError> {
        self.project_value(value)
    }

    fn project_value(&self, value: ParseValue) -> Result<ParseValue, UnmappablePayloadError> {
        match value {
            ParseValue::SpannedValue {
                value: inner,
                start,
                end,
            } => {
                let new_start = self.shift_pos(start).ok_or_else(|| {
                    UnmappablePayloadError("projected span start moved before byte 0".to_string())
                })?;
                let new_end = self.shift_pos(end).ok_or_else(|| {
                    UnmappablePayloadError("projected span end moved before byte 0".to_string())
                })?;
                let new_inner = self.project_value(ParseValue::unwrap_arc(inner))?;
                Ok(ParseValue::SpannedValue {
                    value: Arc::new(new_inner),
                    start: new_start,
                    end: new_end,
                })
            }
            ParseValue::Named(name, inner) => Ok(ParseValue::Named(
                name,
                Arc::new(self.project_value(ParseValue::unwrap_arc(inner))?),
            )),
            ParseValue::Node(tag, children) => {
                let projected: Result<Vec<_>, _> = children
                    .iter()
                    .map(|c| self.project_value(c.clone()))
                    .collect();
                Ok(ParseValue::Node(tag, Arc::new(projected?)))
            }
            other => Ok(other),
        }
    }

    fn shift_pos(&self, pos: usize) -> Option<usize> {
        let mut shifted = pos;
        for shift in &self.span_shifts {
            if pos >= shift.shift_from {
                shifted = shift_offset_by_delta(shifted, shift.delta)?;
            }
        }
        Some(shifted)
    }
}

// ── Public types ───────────────────────────────────────────────────────────

/// Transformed cache tables ready to seed the next parse.
#[derive(Clone, Debug, Default)]
pub struct CacheTables {
    /// Rule memo: rule_name → start_pos → entry.
    pub memo_data: HashMap<String, HashMap<usize, PositionMemoEntry>>,
}

/// Error returned by cache compatibility and metadata validation helpers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CacheCompatError {
    GrammarMismatch { cache_hash: u64, current_hash: u64 },
    RuntimeMismatch { cache_sig: u64, current_sig: u64 },
}

impl std::fmt::Display for CacheCompatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GrammarMismatch { cache_hash, current_hash } => write!(
                f,
                "ParseCache was built for a different grammar (cache={cache_hash:x}, current={current_hash:x}); \
                 use a cache built from the same grammar or parse without a cache"
            ),
            Self::RuntimeMismatch { cache_sig, current_sig } => write!(
                f,
                "ParseCache was built for a different parser runtime configuration \
                 (cache={cache_sig:x}, current={current_sig:x}); \
                 rebuild the cache or reuse it with the same parser config"
            ),
        }
    }
}

impl std::error::Error for CacheCompatError {}

// ── Cache compatibility ────────────────────────────────────────────────────

/// Verify that `pos_cache` was built for the same grammar and runtime config.
///
/// Returns `Err` if either hash mismatches; callers should discard the cache
/// or re-build it in that case.
pub fn ensure_cache_compatible(
    pos_cache: &PositionCache,
    grammar_hash: u64,
    runtime_signature: u64,
) -> Result<(), CacheCompatError> {
    if pos_cache.grammar_hash != grammar_hash {
        return Err(CacheCompatError::GrammarMismatch {
            cache_hash: pos_cache.grammar_hash,
            current_hash: grammar_hash,
        });
    }
    if pos_cache.runtime_signature != runtime_signature {
        return Err(CacheCompatError::RuntimeMismatch {
            cache_sig: pos_cache.runtime_signature,
            current_sig: runtime_signature,
        });
    }
    Ok(())
}

// ── Main entry point ───────────────────────────────────────────────────────

/// Build [`CacheTables`] from an existing position cache + edit information.
///
/// If `cache` is `None` or incompatible, returns empty tables.
///
/// * `normalized_edits` — explicit incremental edits, in position order.
///   When non-empty these are compiled into edit steps and applied to the cache.
///   When empty, the edit is auto-detected via longest-common-prefix/suffix.
/// * `invalidate_from` — forcibly drop all entries whose end position exceeds
///   this byte offset (useful to invalidate a parse-error region).
/// * `incremental_cache` — when `false` and `normalized_edits` is empty, no
///   automatic edit detection is performed and empty tables are returned.
/// * `grammar_hash` / `runtime_signature` — must match the cache or it is discarded.
pub fn resolve_incremental_cache_tables(
    cache: Option<&PositionCache>,
    text: &str,
    normalized_edits: &[IncrementalEdit],
    invalidate_from: Option<usize>,
    incremental_cache: bool,
    grammar_hash: u64,
    runtime_signature: u64,
) -> Result<CacheTables, IncrementalEditError> {
    let Some(pos_cache) = cache else {
        return Ok(CacheTables::default());
    };

    // Compatibility check — discard on mismatch.
    if ensure_cache_compatible(pos_cache, grammar_hash, runtime_signature).is_err() {
        return Ok(CacheTables::default());
    }

    // Same text, no forced invalidation → reuse as-is.
    if pos_cache.text == text && invalidate_from.is_none() {
        return Ok(CacheTables {
            memo_data: pos_cache.memo.clone(),
        });
    }

    // Same text with forced invalidation — apply only the invalidation filter.
    if pos_cache.text == text {
        let memo_data = build_transformed_rule_cache_fast(
            &pos_cache.memo,
            &BoundaryTransplant {
                prefix: 0,
                old_edit_end: 0,
                delta: 0,
            },
            invalidate_from,
        );
        return Ok(CacheTables { memo_data });
    }

    // Determine the transplant plan.
    let plan: TransplantPlan = if !normalized_edits.is_empty() {
        let (steps, resulting_text) =
            compile_incremental_edit_steps(&pos_cache.text, normalized_edits)?;
        if resulting_text != text {
            // Edits don't produce the expected text — fall back to empty.
            return Ok(CacheTables::default());
        }
        TransplantPlan::Steps(steps)
    } else if incremental_cache {
        match boundary_plan_for_changed_text(&pos_cache.text, text) {
            Some(plan) => TransplantPlan::Boundary(plan),
            None => return Ok(CacheTables::default()),
        }
    } else {
        return Ok(CacheTables::default());
    };

    Ok(build_transformed_cache_tables(
        &pos_cache.memo,
        &plan,
        invalidate_from,
    ))
}

// ── Transplant plan ────────────────────────────────────────────────────────

enum TransplantPlan {
    Boundary(BoundaryTransplant),
    Steps(Vec<IncrementalEditStep>),
}

/// Detect the single changed region between `old_text` and `new_text` as a
/// `BoundaryTransplant`.  Returns `None` if the texts are identical.
fn boundary_plan_for_changed_text(old_text: &str, new_text: &str) -> Option<BoundaryTransplant> {
    if old_text == new_text {
        return Some(BoundaryTransplant {
            prefix: old_text.len(),
            old_edit_end: old_text.len(),
            delta: 0,
        });
    }
    let ob = old_text.as_bytes();
    let nb = new_text.as_bytes();
    let max_prefix = ob.len().min(nb.len());
    let prefix = ob.iter().zip(nb.iter()).take_while(|(a, b)| a == b).count();
    let prefix = prefix.min(max_prefix);

    let max_suffix = (ob.len() - prefix).min(nb.len() - prefix);
    let suffix = ob[ob.len() - max_suffix..]
        .iter()
        .rev()
        .zip(nb[nb.len() - max_suffix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();

    let old_edit_end = ob.len() - suffix;
    let delta = if nb.len() >= ob.len() {
        isize::try_from(nb.len() - ob.len()).ok()?
    } else {
        isize::try_from(ob.len() - nb.len())
            .ok()
            .and_then(isize::checked_neg)?
    };
    Some(BoundaryTransplant {
        prefix,
        old_edit_end,
        delta,
    })
}

// ── Table building ─────────────────────────────────────────────────────────

fn build_transformed_cache_tables(
    memo: &HashMap<String, HashMap<usize, PositionMemoEntry>>,
    plan: &TransplantPlan,
    invalidate_from: Option<usize>,
) -> CacheTables {
    let memo_data = match plan {
        TransplantPlan::Boundary(bp) => {
            build_transformed_rule_cache_fast(memo, bp, invalidate_from)
        }
        TransplantPlan::Steps(steps) if steps.len() == 1 => {
            build_transformed_rule_cache_single_step(memo, &steps[0], invalidate_from)
        }
        TransplantPlan::Steps(steps) => {
            build_transformed_rule_cache_general(memo, steps, invalidate_from)
        }
    };
    CacheTables { memo_data }
}

// ── Rule memo fast path (BoundaryTransplant) ───────────────────────────────

/// Fast rule-cache transform for a single boundary edit.
///
/// Drops entries that overlap the edit zone.
/// Shifts positions of entries in the suffix zone by `delta`.
/// Strips embedded spans from shifted entries to avoid stale provenance.
fn build_transformed_rule_cache_fast(
    memo: &RuleMemoTable,
    plan: &BoundaryTransplant,
    invalidate_from: Option<usize>,
) -> RuleMemoTable {
    let prefix = plan.prefix;
    let old_edit_end = plan.old_edit_end;
    let delta = plan.delta;
    let mut out: HashMap<String, HashMap<usize, PositionMemoEntry>> = HashMap::new();

    for (rule, by_pos) in memo {
        let mut new_by_pos: HashMap<usize, PositionMemoEntry> = HashMap::new();
        for (&start, entry) in by_pos {
            let end = entry.end;
            // Reuse is classified on the *examined* interval, not the matched
            // span: an edit inside a lookahead/lookbehind region (read but not
            // consumed) must still invalidate the entry.
            let (read_lo, read_hi) = entry.examined(start);
            match crate::incremental_edits::cache_interval_zone(
                read_lo,
                read_hi,
                prefix,
                old_edit_end,
            ) {
                // Before the edit — keep as-is (unless the invalidation filter
                // drops it).
                Some("prefix") if invalidate_from.is_none_or(|f| end <= f) => {
                    new_by_pos.insert(start, entry.clone());
                }
                Some("suffix") => {
                    // After the edit — shift the matched span and examined interval.
                    let Some(new_start) = shift_offset_by_delta(start, delta) else {
                        continue;
                    };
                    let Some(new_end) = shift_offset_by_delta(end, delta) else {
                        continue;
                    };
                    if invalidate_from.is_none_or(|f| new_end <= f) {
                        let value = project_shifted_value(
                            entry,
                            delta,
                            &[SpanShift {
                                shift_from: old_edit_end,
                                delta,
                            }],
                        );
                        new_by_pos.insert(new_start, shifted_entry(entry, new_end, value, delta));
                    }
                }
                // Overlap (or unexpected) — drop.
                _ => {}
            }
        }
        if !new_by_pos.is_empty() {
            out.insert(rule.clone(), new_by_pos);
        }
    }
    out
}

/// Shift an entry's value provenance spans by `delta` (only when it embeds spans
/// and `delta != 0`); otherwise reuse the value as-is.
fn project_shifted_value(
    entry: &PositionMemoEntry,
    delta: isize,
    shifts: &[SpanShift],
) -> Arc<ParseValue> {
    if delta != 0 && !shifts.is_empty() && contains_spanned(&entry.value) {
        let proj = PayloadProjection::new().with_span_shifts(shifts.to_vec());
        Arc::new(
            proj.project((*entry.value).clone())
                .unwrap_or_else(|_| crate::values::strip_spans((*entry.value).clone())),
        )
    } else {
        entry.value.clone()
    }
}

/// Build a shifted entry: new matched end + value, with the examined interval
/// shifted by `delta`. Entries that drop below 0 keep `None` (fall back to span).
fn shifted_entry(
    entry: &PositionMemoEntry,
    new_end: usize,
    value: Arc<ParseValue>,
    delta: isize,
) -> PositionMemoEntry {
    PositionMemoEntry {
        end: new_end,
        value,
        cut: entry.cut,
        read_lo: entry
            .read_lo
            .and_then(|lo| shift_offset_by_delta(lo, delta)),
        read_hi: entry
            .read_hi
            .and_then(|hi| shift_offset_by_delta(hi, delta)),
    }
}

// ── Rule memo single-step path ─────────────────────────────────────────────

fn build_transformed_rule_cache_single_step(
    memo: &RuleMemoTable,
    step: &IncrementalEditStep,
    invalidate_from: Option<usize>,
) -> RuleMemoTable {
    let s = step.start;
    let e = step.old_end;
    let d = step.delta;
    let boundary = BoundaryTransplant {
        prefix: s,
        old_edit_end: e,
        delta: d,
    };
    build_transformed_rule_cache_fast(memo, &boundary, invalidate_from)
}

// ── Rule memo general path (multi-step) ────────────────────────────────────

fn build_transformed_rule_cache_general(
    memo: &RuleMemoTable,
    steps: &[IncrementalEditStep],
    invalidate_from: Option<usize>,
) -> RuleMemoTable {
    use crate::incremental_edits::apply_incremental_steps_to_entry;
    let mut out: HashMap<String, HashMap<usize, PositionMemoEntry>> = HashMap::new();

    use crate::incremental_edits::apply_incremental_steps_to_interval;
    for (rule, by_pos) in memo {
        let mut new_by_pos: HashMap<usize, PositionMemoEntry> = HashMap::new();
        for (&start, entry) in by_pos {
            let end = entry.end;
            let (read_lo, read_hi) = entry.examined(start);
            // Reuse is gated on the examined interval being edit-disjoint; the
            // matched span (a subset) is then guaranteed transplantable too.
            let Some((new_read_lo, new_read_hi)) =
                apply_incremental_steps_to_interval(read_lo, read_hi, steps)
            else {
                continue;
            };
            if let Some((new_start, new_end, shifts)) =
                apply_incremental_steps_to_entry(start, end, steps)
            {
                if invalidate_from.is_none_or(|f| new_end <= f) {
                    let value = project_shifted_value_with_shifts(entry, &shifts);
                    new_by_pos.insert(
                        new_start,
                        PositionMemoEntry {
                            end: new_end,
                            value,
                            cut: entry.cut,
                            read_lo: Some(new_read_lo),
                            read_hi: Some(new_read_hi),
                        },
                    );
                }
            }
        }
        if !new_by_pos.is_empty() {
            out.insert(rule.clone(), new_by_pos);
        }
    }
    out
}

/// Like [`project_shifted_value`] but driven by an explicit shift list (the
/// multi-step path computes one shift per applied edit).
fn project_shifted_value_with_shifts(
    entry: &PositionMemoEntry,
    shifts: &[SpanShift],
) -> Arc<ParseValue> {
    if !shifts.is_empty() && contains_spanned(&entry.value) {
        let proj = PayloadProjection::new().with_span_shifts(shifts.to_vec());
        Arc::new(
            proj.project((*entry.value).clone())
                .unwrap_or_else(|_| crate::values::strip_spans((*entry.value).clone())),
        )
    } else {
        entry.value.clone()
    }
}

// ── Cache seed helpers (from cache_seed.py) ────────────────────────────────

/// Build a `CacheTables` suitable for seeding the next parse run.
///
/// Combines `resolve_incremental_cache_tables` with a compatibility gate.
pub fn build_parse_cache_seed(
    cache: Option<&PositionCache>,
    text: &str,
    normalized_edits: &[IncrementalEdit],
    invalidate_from: Option<usize>,
    incremental_cache: bool,
    grammar_hash: u64,
    runtime_signature: u64,
) -> Result<CacheTables, IncrementalEditError> {
    resolve_incremental_cache_tables(
        cache,
        text,
        normalized_edits,
        invalidate_from,
        incremental_cache,
        grammar_hash,
        runtime_signature,
    )
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{IncrementalEdit, ParseValue, PositionCache, PositionMemoEntry};

    fn edit(start: usize, old_end: usize, replacement: impl Into<String>) -> IncrementalEdit {
        IncrementalEdit::new(start, old_end, replacement).expect("test edit must be valid")
    }

    fn make_cache(text: &str, grammar_hash: u64, runtime_sig: u64) -> PositionCache {
        PositionCache {
            text: text.to_string(),
            grammar_hash,
            runtime_signature: runtime_sig,
            memo: HashMap::new(),
        }
    }

    fn add_entry(cache: &mut PositionCache, rule: &str, start: usize, end: usize) {
        cache.memo.entry(rule.to_string()).or_default().insert(
            start,
            PositionMemoEntry {
                end,
                value: Arc::new(ParseValue::Nil),
                cut: false,
                read_lo: Some(start),
                read_hi: Some(end),
            },
        );
    }

    /// Like [`add_entry`] but with an explicit examined interval wider than the
    /// matched span (e.g. a lookahead that read past `end`).
    fn add_entry_examined(
        cache: &mut PositionCache,
        rule: &str,
        start: usize,
        end: usize,
        read_lo: usize,
        read_hi: usize,
    ) {
        cache.memo.entry(rule.to_string()).or_default().insert(
            start,
            PositionMemoEntry {
                end,
                value: Arc::new(ParseValue::Nil),
                cut: false,
                read_lo: Some(read_lo),
                read_hi: Some(read_hi),
            },
        );
    }

    // ── ensure_cache_compatible ────────────────────────────────────────────

    #[test]
    fn compatible_cache_ok() {
        let cache = make_cache("hello", 42, 7);
        assert!(ensure_cache_compatible(&cache, 42, 7).is_ok());
    }

    #[test]
    fn incompatible_grammar_hash_err() {
        let cache = make_cache("hello", 42, 7);
        assert!(matches!(
            ensure_cache_compatible(&cache, 99, 7),
            Err(CacheCompatError::GrammarMismatch { .. })
        ));
    }

    #[test]
    fn incompatible_runtime_signature_err() {
        let cache = make_cache("hello", 42, 7);
        assert!(matches!(
            ensure_cache_compatible(&cache, 42, 99),
            Err(CacheCompatError::RuntimeMismatch { .. })
        ));
    }

    // ── boundary_plan_for_changed_text ────────────────────────────────────

    #[test]
    fn boundary_plan_identical_texts() {
        let plan = boundary_plan_for_changed_text("abc", "abc").unwrap();
        assert_eq!(plan.prefix, 3);
        assert_eq!(plan.old_edit_end, 3);
        assert_eq!(plan.delta, 0);
    }

    #[test]
    fn boundary_plan_insertion_in_middle() {
        // "abcd" → "abXcd": prefix=2, old_edit_end=2, delta=1
        let plan = boundary_plan_for_changed_text("abcd", "abXcd").unwrap();
        assert_eq!(plan.prefix, 2);
        assert_eq!(plan.old_edit_end, 2);
        assert_eq!(plan.delta, 1);
    }

    #[test]
    fn boundary_plan_deletion_in_middle() {
        // "abXcd" → "abcd": prefix=2, old_edit_end=3, delta=-1
        let plan = boundary_plan_for_changed_text("abXcd", "abcd").unwrap();
        assert_eq!(plan.prefix, 2);
        assert_eq!(plan.old_edit_end, 3);
        assert_eq!(plan.delta, -1);
    }

    // ── resolve_incremental_cache_tables ──────────────────────────────────

    #[test]
    fn returns_empty_when_no_cache() {
        let tables = resolve_incremental_cache_tables(None, "text", &[], None, true, 1, 1).unwrap();
        assert!(tables.memo_data.is_empty());
    }

    #[test]
    fn returns_empty_when_grammar_mismatch() {
        let cache = make_cache("text", 1, 1);
        let tables =
            resolve_incremental_cache_tables(Some(&cache), "text", &[], None, true, 99, 1).unwrap();
        assert!(tables.memo_data.is_empty());
    }

    #[test]
    fn reuses_exact_same_text() {
        let mut cache = make_cache("hello", 1, 1);
        add_entry(&mut cache, "rule", 0, 3);
        let tables =
            resolve_incremental_cache_tables(Some(&cache), "hello", &[], None, true, 1, 1).unwrap();
        assert!(tables.memo_data.contains_key("rule"));
    }

    #[test]
    fn drops_overlapping_entries_on_insertion() {
        let mut cache = make_cache("abcd", 1, 1);
        // Entry [0, 4) spans the whole string.
        add_entry(&mut cache, "rule", 0, 4);
        // After inserting 'X' at position 2: "abXcd"
        let tables =
            resolve_incremental_cache_tables(Some(&cache), "abXcd", &[], None, true, 1, 1).unwrap();
        // The entry [0, 4) overlaps the edit zone [2,2), so it should be dropped.
        let rule_entries = tables.memo_data.get("rule");
        assert!(
            rule_entries.is_none_or(|m| m.is_empty()),
            "overlapping entry should be dropped: {rule_entries:?}"
        );
    }

    #[test]
    fn drops_entry_when_edit_hits_examined_but_unmatched_region() {
        // Entry matched [0,1) but examined [0,4) (a lookahead read past `end`).
        // An edit at byte 2 is outside the matched span but inside the examined
        // interval — the transplant must drop it (soundness).
        let mut cache = make_cache("abcd", 1, 1);
        add_entry_examined(&mut cache, "rule", 0, 1, 0, 4);
        // Insert 'X' at position 2: "abXcd".
        let tables =
            resolve_incremental_cache_tables(Some(&cache), "abXcd", &[], None, true, 1, 1).unwrap();
        let rule_entries = tables.memo_data.get("rule");
        assert!(
            rule_entries.is_none_or(|m| m.is_empty()),
            "entry whose examined interval overlaps the edit must be dropped: {rule_entries:?}"
        );
    }

    #[test]
    fn explicit_single_edit_drops_examined_overlap() {
        // One explicit edit routes through the single-step transplant; the entry
        // matched [0,1) but examined [0,4), and the edit lands at byte 2.
        let mut cache = make_cache("abcd", 1, 1);
        add_entry_examined(&mut cache, "rule", 0, 1, 0, 4);
        let edits = [edit(2, 3, "Z")]; // replace 'c' → 'Z' (delta 0) ⇒ "abZd"
        let tables =
            resolve_incremental_cache_tables(Some(&cache), "abZd", &edits, None, true, 1, 1)
                .unwrap();
        assert!(
            tables.memo_data.get("rule").is_none_or(|m| m.is_empty()),
            "single-step explicit edit must drop the examined-overlapping entry"
        );
    }

    #[test]
    fn multi_step_edits_drop_examined_overlap() {
        // Two explicit edits force the general (multi-step) transplant path.
        // The entry matched [0,1) but examined [0,4); the first edit lands inside
        // that examined tail (byte 2) → it must be dropped.
        let mut cache = make_cache("abcdefghij", 1, 1);
        add_entry_examined(&mut cache, "rule", 0, 1, 0, 4);
        // Sequential single-char replacements: "abcdefghij" → "abZdefghij" → "abZdefQhij".
        let edits = [edit(2, 3, "Z"), edit(6, 7, "Q")];
        let tables =
            resolve_incremental_cache_tables(Some(&cache), "abZdefQhij", &edits, None, true, 1, 1)
                .unwrap();
        assert!(
            tables.memo_data.get("rule").is_none_or(|m| m.is_empty()),
            "general path must classify on the examined interval, not the match span"
        );
    }

    #[test]
    fn multi_step_edits_shift_examined_interval_of_suffix_entry() {
        // Two insertions at the front shift a trailing entry's matched span AND
        // its examined interval by the combined delta through the general path.
        let mut cache = make_cache("abcdefghij", 1, 1);
        add_entry_examined(&mut cache, "rule", 8, 9, 8, 10);
        // Insert 'X' then 'Y' at the front: "abcdefghij" → "Xabcdefghij" → "YXabcdefghij".
        let edits = [edit(0, 0, "X"), edit(0, 0, "Y")];
        let tables = resolve_incremental_cache_tables(
            Some(&cache),
            "YXabcdefghij",
            &edits,
            None,
            true,
            1,
            1,
        )
        .unwrap();
        let entry = tables
            .memo_data
            .get("rule")
            .and_then(|m| m.get(&10))
            .expect("suffix entry shifted by +2");
        assert_eq!(entry.end, 11);
        assert_eq!((entry.read_lo, entry.read_hi), (Some(10), Some(12)));
    }

    #[test]
    fn keeps_entry_when_edit_is_past_examined_region() {
        // Entry matched [0,1), examined [0,2); appending past byte 2 leaves it
        // untouched even though the matched span is tiny.
        let mut cache = make_cache("abcd", 1, 1);
        add_entry_examined(&mut cache, "rule", 0, 1, 0, 2);
        let tables =
            resolve_incremental_cache_tables(Some(&cache), "abcdX", &[], None, true, 1, 1).unwrap();
        assert!(
            tables
                .memo_data
                .get("rule")
                .and_then(|m| m.get(&0))
                .is_some(),
            "entry examined only [0,2) should survive an append at byte 4"
        );
    }

    #[test]
    fn keeps_prefix_entries_on_suffix_insertion() {
        let mut cache = make_cache("abcd", 1, 1);
        // Entry [0, 1) is purely in the prefix.
        add_entry(&mut cache, "rule", 0, 1);
        // Append 'X': "abcdX"
        let tables =
            resolve_incremental_cache_tables(Some(&cache), "abcdX", &[], None, true, 1, 1).unwrap();
        assert!(
            tables
                .memo_data
                .get("rule")
                .and_then(|m| m.get(&0))
                .is_some(),
            "prefix entry should survive"
        );
    }

    #[test]
    fn shifts_suffix_entries_on_insertion() {
        let mut cache = make_cache("abcd", 1, 1);
        // Entry [2, 4) is in the suffix relative to prefix=0, old_end=0.
        // After inserting at start ("Xabcd"): shift by +1.
        add_entry(&mut cache, "rule", 2, 4);
        let tables =
            resolve_incremental_cache_tables(Some(&cache), "Xabcd", &[], None, true, 1, 1).unwrap();
        // Insertion at byte 0 makes the edit zone [0,0), suffix starts at 0.
        // Entry [2,4) → [3, 5).
        let rule_m = tables.memo_data.get("rule");
        // Might be at shifted position 3.
        let found = rule_m.is_some_and(|m| m.contains_key(&3));
        assert!(found, "suffix entry should be shifted: {rule_m:?}");
    }

    #[test]
    fn incremental_cache_false_returns_empty_on_text_change() {
        let mut cache = make_cache("abcd", 1, 1);
        add_entry(&mut cache, "rule", 0, 4);
        let tables =
            resolve_incremental_cache_tables(Some(&cache), "abXcd", &[], None, false, 1, 1)
                .unwrap();
        assert!(tables.memo_data.is_empty());
    }

    #[test]
    fn invalidate_from_drops_late_entries() {
        let mut cache = make_cache("hello world", 1, 1);
        // Entry [6, 11) ends after position 5.
        add_entry(&mut cache, "rule", 6, 11);
        // Same text but invalidate from position 5.
        let tables =
            resolve_incremental_cache_tables(Some(&cache), "hello world", &[], Some(5), true, 1, 1)
                .unwrap();
        let rule_m = tables.memo_data.get("rule");
        assert!(
            rule_m.is_none_or(|m| m.is_empty()),
            "entry ending after invalidate_from should be dropped"
        );
    }

    // ── build_parse_cache_seed ────────────────────────────────────────────

    #[test]
    fn seed_returns_empty_on_none_cache() {
        let seed = build_parse_cache_seed(None, "text", &[], None, true, 1, 1).unwrap();
        assert!(seed.memo_data.is_empty());
    }

    #[test]
    fn seed_reuses_compatible_cache() {
        let mut cache = make_cache("hello", 10, 20);
        add_entry(&mut cache, "r", 0, 5);
        let seed = build_parse_cache_seed(Some(&cache), "hello", &[], None, true, 10, 20).unwrap();
        assert!(seed.memo_data.contains_key("r"));
    }

    #[test]
    fn seed_rejects_invalid_normalized_edit_range() {
        let cache = make_cache("hello", 10, 20);
        let edits = [edit(9, 9, "!")];
        let err =
            build_parse_cache_seed(Some(&cache), "hello!", &edits, None, true, 10, 20).unwrap_err();
        assert!(matches!(err, IncrementalEditError::InvalidRange { .. }));
    }

    // ── PayloadProjection ─────────────────────────────────────────────────

    #[test]
    fn payload_projection_no_shifts_is_identity() {
        let proj = PayloadProjection::new();
        let v = ParseValue::Text("hello".into());
        assert_eq!(proj.project(v.clone()).unwrap(), v);
    }

    #[test]
    fn payload_projection_shifts_spanned_value() {
        let proj = PayloadProjection::new().with_span_shifts(vec![SpanShift {
            shift_from: 5,
            delta: 3,
        }]);
        let v = ParseValue::SpannedValue {
            value: Arc::new(ParseValue::Text("x".into())),
            start: 5,
            end: 8,
        };
        let projected = proj.project(v).unwrap();
        match projected {
            ParseValue::SpannedValue { start, end, .. } => {
                assert_eq!(start, 8);
                assert_eq!(end, 11);
            }
            _ => panic!("expected SpannedValue"),
        }
    }

    #[test]
    fn payload_projection_no_shift_below_threshold() {
        let proj = PayloadProjection::new().with_span_shifts(vec![SpanShift {
            shift_from: 10,
            delta: 5,
        }]);
        let v = ParseValue::SpannedValue {
            value: Arc::new(ParseValue::Nil),
            start: 3,
            end: 8,
        };
        let projected = proj.project(v).unwrap();
        match projected {
            ParseValue::SpannedValue { start, end, .. } => {
                assert_eq!(start, 3);
                assert_eq!(end, 8);
            }
            _ => panic!("expected SpannedValue"),
        }
    }

    #[test]
    fn payload_projection_negative_delta_underflow_is_unmappable() {
        let proj = PayloadProjection::new().with_span_shifts(vec![SpanShift {
            shift_from: 0,
            delta: -100,
        }]);
        let v = ParseValue::SpannedValue {
            value: Arc::new(ParseValue::Nil),
            start: 5,
            end: 10,
        };
        assert!(proj.project(v).is_err());
    }
}
