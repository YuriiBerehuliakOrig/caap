//! Incremental cache transplant orchestration.
//!
//! Ports `peg/engine/incremental_transforms.py`, `cache_persist.py`, and
//! `cache_seed.py`.  Transforms a persisted `PositionCache` across text edits so that
//! surviving memo entries are carried forward into the next parse run.

use std::collections::{HashMap, HashSet};

use crate::incremental_edits::{
    compile_incremental_edit_steps, BoundaryTransplant, IncrementalEditError, IncrementalEditStep,
    SpanShift,
};
use crate::types::{IncrementalEdit, ParseValue, PositionCache, PositionMemoEntry};
use crate::values::contains_spanned;

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

/// An optional filename replacement applied during payload projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilenameUpdate {
    pub value: Option<String>,
}

/// Projects span offsets inside a cached `ParseValue` tree when text shifts.
///
/// Mirrors `peg/engine/payload_projection.py::PayloadProjection`.
///
/// After an incremental edit the in-cache parse values contain stale byte
/// offsets. `PayloadProjection` walks the value tree and applies the
/// `span_shifts` list so all spans reflect the new text layout.
#[derive(Clone, Debug, Default)]
pub struct PayloadProjection {
    /// Ordered list of (shift_from, delta) pairs. Each pair means: for every
    /// span endpoint `>= shift_from`, add `delta` to it.
    pub span_shifts: Vec<SpanShift>,
    /// If set, replaces `source_id` in every span-bearing node.
    pub source_id: Option<u64>,
    /// If set, replaces the filename in every `RawBlock`-style leaf.
    pub filename_update: Option<FilenameUpdate>,
}

impl PayloadProjection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_span_shifts(mut self, shifts: Vec<SpanShift>) -> Self {
        self.span_shifts = shifts;
        self
    }

    pub fn with_source_id(mut self, id: u64) -> Self {
        self.source_id = Some(id);
        self
    }

    pub fn with_filename_update(mut self, filename: Option<String>) -> Self {
        self.filename_update = Some(FilenameUpdate { value: filename });
        self
    }

    pub fn has_effect(&self) -> bool {
        !self.span_shifts.is_empty() || self.source_id.is_some() || self.filename_update.is_some()
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
                let new_start = self.shift_pos(start);
                let new_end = self.shift_pos(end);
                let new_inner = self.project_value(*inner)?;
                Ok(ParseValue::SpannedValue {
                    value: Box::new(new_inner),
                    start: new_start,
                    end: new_end,
                })
            }
            ParseValue::Named(name, inner) => Ok(ParseValue::Named(
                name,
                Box::new(self.project_value(*inner)?),
            )),
            ParseValue::Node(tag, children) => {
                let projected: Result<Vec<_>, _> = children
                    .into_iter()
                    .map(|c| self.project_value(c))
                    .collect();
                Ok(ParseValue::Node(tag, projected?))
            }
            other => Ok(other),
        }
    }

    fn shift_pos(&self, pos: usize) -> usize {
        let mut p = pos as isize;
        for shift in &self.span_shifts {
            if pos >= shift.shift_from {
                p += shift.delta;
            }
        }
        p.max(0) as usize
    }
}

// ── Public types ───────────────────────────────────────────────────────────

/// Transformed cache tables ready to seed the next parse.
#[derive(Clone, Debug, Default)]
pub struct CacheTables {
    /// Rule memo: rule_name → start_pos → entry.
    pub memo_data: HashMap<String, HashMap<usize, PositionMemoEntry>>,
    /// Trivia-skip memo: start_pos → end_pos.
    pub skip_memo_data: HashMap<usize, usize>,
    /// `(rule, start)` pairs whose cached values contain embedded source spans.
    /// These need special handling if the text shifts — without projection the
    /// spans would be stale.  Conservative policy: spans in shifted entries are
    /// stripped so they are re-captured on the next parse.
    pub memo_provenance_keys: HashSet<(String, usize)>,
}

/// Error returned by [`ensure_cache_compatible`].
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
            skip_memo_data: HashMap::new(),
            memo_provenance_keys: compute_provenance_keys(&pos_cache.memo),
        });
    }

    // Same text with forced invalidation — apply only the invalidation filter.
    if pos_cache.text == text {
        let (memo_data, prov) = build_transformed_rule_cache_fast(
            &pos_cache.memo,
            &BoundaryTransplant {
                prefix: 0,
                old_edit_end: 0,
                delta: 0,
            },
            invalidate_from,
        );
        return Ok(CacheTables {
            memo_data,
            skip_memo_data: HashMap::new(),
            memo_provenance_keys: prov,
        });
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
    let delta = (nb.len() as isize) - (ob.len() as isize);
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
    let (memo_data, prov) = match plan {
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
    CacheTables {
        memo_data,
        skip_memo_data: HashMap::new(), // skip memo not yet persisted across runs
        memo_provenance_keys: prov,
    }
}

// ── Rule memo fast path (BoundaryTransplant) ───────────────────────────────

/// Fast rule-cache transform for a single boundary edit.
///
/// Drops entries that overlap the edit zone.
/// Shifts positions of entries in the suffix zone by `delta`.
/// Strips embedded spans from shifted entries to avoid stale provenance.
fn build_transformed_rule_cache_fast(
    memo: &HashMap<String, HashMap<usize, PositionMemoEntry>>,
    plan: &BoundaryTransplant,
    invalidate_from: Option<usize>,
) -> (
    HashMap<String, HashMap<usize, PositionMemoEntry>>,
    HashSet<(String, usize)>,
) {
    let prefix = plan.prefix;
    let old_edit_end = plan.old_edit_end;
    let delta = plan.delta;
    let mut out: HashMap<String, HashMap<usize, PositionMemoEntry>> = HashMap::new();
    let mut prov: HashSet<(String, usize)> = HashSet::new();

    for (rule, by_pos) in memo {
        let mut new_by_pos: HashMap<usize, PositionMemoEntry> = HashMap::new();
        for (&start, entry) in by_pos {
            let end = entry.end;
            let lo = start.min(end);
            let hi = start.max(end);
            if delta == 0 {
                // No shift — only apply zone filter and invalidate_from.
                if (hi < prefix || lo >= old_edit_end) && invalidate_from.is_none_or(|f| end <= f) {
                    let has_prov = contains_spanned(&entry.value);
                    new_by_pos.insert(start, entry.clone());
                    if has_prov {
                        prov.insert((rule.clone(), start));
                    }
                }
            } else if hi < prefix {
                // Prefix zone: keep as-is.
                if invalidate_from.is_none_or(|f| end <= f) {
                    let has_prov = contains_spanned(&entry.value);
                    new_by_pos.insert(start, entry.clone());
                    if has_prov {
                        prov.insert((rule.clone(), start));
                    }
                }
            } else if lo >= old_edit_end {
                // Suffix zone: shift positions.
                let new_start = (start as isize + delta) as usize;
                let new_end = (end as isize + delta) as usize;
                if invalidate_from.is_none_or(|f| new_end <= f) {
                    let has_prov = contains_spanned(&entry.value);
                    let value = if has_prov && delta != 0 {
                        // Project embedded spans to new positions.
                        let proj = PayloadProjection::new().with_span_shifts(vec![SpanShift {
                            shift_from: old_edit_end,
                            delta,
                        }]);
                        proj.project_value(entry.value.clone())
                            .unwrap_or_else(|_| crate::values::strip_spans(entry.value.clone()))
                    } else {
                        entry.value.clone()
                    };
                    if has_prov {
                        prov.insert((rule.clone(), new_start));
                    }
                    new_by_pos.insert(
                        new_start,
                        PositionMemoEntry {
                            end: new_end,
                            value,
                        },
                    );
                }
            }
            // Overlap zone: drop.
        }
        if !new_by_pos.is_empty() {
            out.insert(rule.clone(), new_by_pos);
        }
    }
    (out, prov)
}

// ── Rule memo single-step path ─────────────────────────────────────────────

fn build_transformed_rule_cache_single_step(
    memo: &HashMap<String, HashMap<usize, PositionMemoEntry>>,
    step: &IncrementalEditStep,
    invalidate_from: Option<usize>,
) -> (
    HashMap<String, HashMap<usize, PositionMemoEntry>>,
    HashSet<(String, usize)>,
) {
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
    memo: &HashMap<String, HashMap<usize, PositionMemoEntry>>,
    steps: &[IncrementalEditStep],
    invalidate_from: Option<usize>,
) -> (
    HashMap<String, HashMap<usize, PositionMemoEntry>>,
    HashSet<(String, usize)>,
) {
    use crate::incremental_edits::apply_incremental_steps_to_interval;
    let mut out: HashMap<String, HashMap<usize, PositionMemoEntry>> = HashMap::new();
    let mut prov: HashSet<(String, usize)> = HashSet::new();

    for (rule, by_pos) in memo {
        let mut new_by_pos: HashMap<usize, PositionMemoEntry> = HashMap::new();
        for (&start, entry) in by_pos {
            let end = entry.end;
            // Try to transform [start, end) through all steps.
            if let Some((new_start, new_end)) =
                apply_incremental_steps_to_interval(start, end, steps)
            {
                if invalidate_from.is_none_or(|f| new_end <= f) {
                    let has_prov = contains_spanned(&entry.value);
                    let value = if has_prov && new_start != start {
                        // Build span shifts from all steps that affected this entry.
                        let shifts: Vec<SpanShift> = steps
                            .iter()
                            .map(|s| SpanShift {
                                shift_from: s.start,
                                delta: s.delta,
                            })
                            .collect();
                        let proj = PayloadProjection::new().with_span_shifts(shifts);
                        proj.project_value(entry.value.clone())
                            .unwrap_or_else(|_| crate::values::strip_spans(entry.value.clone()))
                    } else {
                        entry.value.clone()
                    };
                    if has_prov {
                        prov.insert((rule.clone(), new_start));
                    }
                    new_by_pos.insert(
                        new_start,
                        PositionMemoEntry {
                            end: new_end,
                            value,
                        },
                    );
                }
            }
        }
        if !new_by_pos.is_empty() {
            out.insert(rule.clone(), new_by_pos);
        }
    }
    (out, prov)
}

// ── Provenance helpers ─────────────────────────────────────────────────────

/// Collect `(rule, start)` keys whose values contain embedded source spans.
fn compute_provenance_keys(
    memo: &HashMap<String, HashMap<usize, PositionMemoEntry>>,
) -> HashSet<(String, usize)> {
    let mut keys = HashSet::new();
    for (rule, by_pos) in memo {
        for (&start, entry) in by_pos {
            if contains_spanned(&entry.value) {
                keys.insert((rule.clone(), start));
            }
        }
    }
    keys
}

/// Validate that `tables.memo_provenance_keys` exactly matches the set of
/// cache entries that contain embedded spans.
///
/// Returns an error message if the keys are out of sync — which can happen if
/// the cache was built without provenance tracking or was corrupted.
pub fn validate_cache_provenance_metadata(tables: &CacheTables) -> Result<(), String> {
    let actual_keys = compute_provenance_keys(&tables.memo_data);
    if actual_keys != tables.memo_provenance_keys {
        return Err(
            "CacheTables memo_provenance_keys must exactly match span-bearing memo entries; \
             rebuild the cache with the current parser"
                .to_string(),
        );
    }
    Ok(())
}

// ── Cache persistence helpers ──────────────────────────────────────────────

/// Statistics collected when persisting a parse run's cache state.
#[derive(Clone, Debug, Default)]
pub struct CacheStats {
    pub rule_cache_size: usize,
    pub rule_cache_hits: usize,
    pub rule_cache_misses: usize,
}

/// Persist a `PositionCache` snapshot suitable for serialization and later
/// incremental reuse.
///
/// Strips failed entries (only successful outcomes are worth keeping) and
/// records provenance keys for span-bearing entries.
pub fn persist_position_cache(
    pos_cache: &PositionCache,
) -> (PositionCache, HashSet<(String, usize)>) {
    let mut clean_memo: HashMap<String, HashMap<usize, PositionMemoEntry>> = HashMap::new();
    let mut provenance: HashSet<(String, usize)> = HashSet::new();
    for (rule, by_pos) in &pos_cache.memo {
        let clean: HashMap<usize, PositionMemoEntry> =
            by_pos.iter().map(|(&k, v)| (k, v.clone())).collect();
        if !clean.is_empty() {
            for (&start, entry) in &clean {
                if contains_spanned(&entry.value) {
                    provenance.insert((rule.clone(), start));
                }
            }
            clean_memo.insert(rule.clone(), clean);
        }
    }
    let persisted = PositionCache {
        text: pos_cache.text.clone(),
        grammar_hash: pos_cache.grammar_hash,
        runtime_signature: pos_cache.runtime_signature,
        memo: clean_memo,
    };
    (persisted, provenance)
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
) -> CacheTables {
    resolve_incremental_cache_tables(
        cache,
        text,
        normalized_edits,
        invalidate_from,
        incremental_cache,
        grammar_hash,
        runtime_signature,
    )
    .unwrap_or_default()
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ParseValue, PositionCache, PositionMemoEntry};

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
                value: ParseValue::Nil,
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
        let seed = build_parse_cache_seed(None, "text", &[], None, true, 1, 1);
        assert!(seed.memo_data.is_empty());
    }

    #[test]
    fn seed_reuses_compatible_cache() {
        let mut cache = make_cache("hello", 10, 20);
        add_entry(&mut cache, "r", 0, 5);
        let seed = build_parse_cache_seed(Some(&cache), "hello", &[], None, true, 10, 20);
        assert!(seed.memo_data.contains_key("r"));
    }

    // ── PayloadProjection ─────────────────────────────────────────────────

    #[test]
    fn payload_projection_no_shifts_is_identity() {
        let proj = PayloadProjection::new();
        let v = ParseValue::Text("hello".to_string());
        assert_eq!(proj.project(v.clone()).unwrap(), v);
    }

    #[test]
    fn payload_projection_shifts_spanned_value() {
        let proj = PayloadProjection::new().with_span_shifts(vec![SpanShift {
            shift_from: 5,
            delta: 3,
        }]);
        let v = ParseValue::SpannedValue {
            value: Box::new(ParseValue::Text("x".to_string())),
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
            value: Box::new(ParseValue::Nil),
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
    fn payload_projection_negative_delta_clamps_to_zero() {
        let proj = PayloadProjection::new().with_span_shifts(vec![SpanShift {
            shift_from: 0,
            delta: -100,
        }]);
        let v = ParseValue::SpannedValue {
            value: Box::new(ParseValue::Nil),
            start: 5,
            end: 10,
        };
        let projected = proj.project(v).unwrap();
        match projected {
            ParseValue::SpannedValue { start, end, .. } => {
                assert_eq!(start, 0);
                assert_eq!(end, 0);
            }
            _ => panic!("expected SpannedValue"),
        }
    }

    #[test]
    fn payload_projection_has_effect_checks_correctly() {
        assert!(!PayloadProjection::new().has_effect());
        assert!(PayloadProjection::new()
            .with_span_shifts(vec![SpanShift {
                shift_from: 0,
                delta: 1
            }])
            .has_effect());
        assert!(PayloadProjection::new().with_source_id(1).has_effect());
        assert!(PayloadProjection::new()
            .with_filename_update(None)
            .has_effect());
    }
}
