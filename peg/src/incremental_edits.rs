//! Incremental edit normalization, sequencing, and cache-interval transplant helpers.
//!
//! Ported from `peg/engine/incremental_edits.py` and `peg/engine/incremental_rewrite.py`.

use crate::types::IncrementalEdit;
use thiserror::Error;

// ── IncrementalEditStep ───────────────────────────────────────────────────────

/// A single applied edit: the byte range it affected and the net byte-length delta.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncrementalEditStep {
    /// Inclusive start byte offset of the affected range.
    pub start: usize,
    /// Exclusive end byte offset of the affected range.
    pub old_end: usize,
    /// Net byte-length change (inserted − removed).
    pub delta: isize,
}

// ── SpanShift ─────────────────────────────────────────────────────────────────

/// Records a point in the text after which all offsets shift by `delta` bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpanShift {
    /// Offset at/after which the shift applies.
    pub shift_from: usize,
    /// Bytes to add to offsets at/after `shift_from`.
    pub delta: isize,
}

// ── BoundaryTransplant ────────────────────────────────────────────────────────

/// A simpler transplant plan built from old/new text boundaries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundaryTransplant {
    /// Length of the unchanged prefix shared by old and new text.
    pub prefix: usize,
    /// End of the edited region in the old text.
    pub old_edit_end: usize,
    /// Net byte-length change across the edit.
    pub delta: isize,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error, Clone, PartialEq)]
/// Why edit normalization/sequencing failed.
pub enum IncrementalEditError {
    #[error("invalid edit range at edit[{index}]: start={start} old_end={old_end} len={len}")]
    /// An edit's byte range was invalid for the text.
    InvalidRange {
        /// Index of the offending edit.
        index: usize,
        /// Edit start offset.
        start: usize,
        /// Edit old-end offset.
        old_end: usize,
        /// Text length.
        len: usize,
    },
    #[error(
        "edit[{index}] length delta is too large: inserted={inserted_len} removed={removed_len}"
    )]
    /// An edit's length delta overflowed.
    DeltaOverflow {
        /// Index of the offending edit.
        index: usize,
        /// Inserted byte count.
        inserted_len: usize,
        /// Removed byte count.
        removed_len: usize,
    },
    #[error("edit[{index}] shifted offset overflows: offset={offset} delta={delta}")]
    /// A shifted offset overflowed.
    OffsetOverflow {
        /// Index of the offending edit.
        index: usize,
        /// The offset being shifted.
        offset: usize,
        /// The shift delta.
        delta: isize,
    },
    #[error(
        "overlapping snapshot edits: edit[{cur_index}] [{cur_start},{cur_end}) overlaps \
         edit[{prev_index}] [{prev_start},{prev_end})"
    )]
    /// Two snapshot edits overlapped.
    OverlappingEdits {
        /// Index of the current edit.
        cur_index: usize,
        /// Current edit start.
        cur_start: usize,
        /// Current edit end.
        cur_end: usize,
        /// Index of the previous edit.
        prev_index: usize,
        /// Previous edit start.
        prev_start: usize,
        /// Previous edit end.
        prev_end: usize,
    },
}

// ── Edit normalization / compilation ─────────────────────────────────────────

/// Validate and return a copy of `edits` (no-op for well-formed inputs).
pub fn normalize_incremental_edits(edits: &[IncrementalEdit]) -> Vec<IncrementalEdit> {
    edits.to_vec()
}

/// Apply `edits` sequentially to `old_text`, returning the list of edit steps
/// (start, old_end, delta) and the resulting text.
///
/// Each edit is applied to the *current* text after all previous edits.
pub fn compile_incremental_edit_steps(
    old_text: &str,
    edits: &[IncrementalEdit],
) -> Result<(Vec<IncrementalEditStep>, String), IncrementalEditError> {
    let mut steps = Vec::with_capacity(edits.len());
    let mut current = old_text.to_string();
    for (idx, edit) in edits.iter().enumerate() {
        let len = current.len();
        if edit.start() > len || edit.old_end() > len || edit.start() > edit.old_end() {
            return Err(IncrementalEditError::InvalidRange {
                index: idx,
                start: edit.start(),
                old_end: edit.old_end(),
                len,
            });
        }
        let delta = checked_len_delta(edit.replacement().len(), edit.old_end() - edit.start())
            .ok_or(IncrementalEditError::DeltaOverflow {
                index: idx,
                inserted_len: edit.replacement().len(),
                removed_len: edit.old_end() - edit.start(),
            })?;
        steps.push(IncrementalEditStep {
            start: edit.start(),
            old_end: edit.old_end(),
            delta,
        });
        let mut new_text = current[..edit.start()].to_string();
        new_text.push_str(edit.replacement());
        new_text.push_str(&current[edit.old_end()..]);
        current = new_text;
    }
    Ok((steps, current))
}

/// Convert snapshot edits (all offsets relative to the original text) into
/// sequential edits (each offset relative to the text after prior edits applied).
///
/// Snapshot edits may be given in any order; they are sorted and validated
/// (no overlapping ranges) before sequentialisation.
pub fn snapshot_edits_to_sequential(
    base_text: &str,
    edits: &[IncrementalEdit],
) -> Result<Vec<IncrementalEdit>, IncrementalEditError> {
    if edits.is_empty() {
        return Ok(vec![]);
    }
    let normalized = normalize_incremental_edits(edits);
    let sorted = _sort_snapshot_edits(&normalized);
    _validate_snapshot_edit_ranges(base_text.len(), &sorted)?;

    let mut sequential = Vec::with_capacity(sorted.len());
    let mut delta: isize = 0;
    for (index, edit) in &sorted {
        let seq_start = shift_offset_by_delta(edit.start(), delta).ok_or(
            IncrementalEditError::OffsetOverflow {
                index: *index,
                offset: edit.start(),
                delta,
            },
        )?;
        let seq_old_end = shift_offset_by_delta(edit.old_end(), delta).ok_or(
            IncrementalEditError::OffsetOverflow {
                index: *index,
                offset: edit.old_end(),
                delta,
            },
        )?;
        sequential.push(
            IncrementalEdit::new(seq_start, seq_old_end, edit.replacement().to_string()).ok_or(
                IncrementalEditError::InvalidRange {
                    index: *index,
                    start: seq_start,
                    old_end: seq_old_end,
                    len: base_text.len(),
                },
            )?,
        );
        let edit_delta = checked_len_delta(edit.replacement().len(), edit.old_end() - edit.start())
            .ok_or(IncrementalEditError::DeltaOverflow {
                index: *index,
                inserted_len: edit.replacement().len(),
                removed_len: edit.old_end() - edit.start(),
            })?;
        delta = delta
            .checked_add(edit_delta)
            .ok_or(IncrementalEditError::DeltaOverflow {
                index: *index,
                inserted_len: edit.replacement().len(),
                removed_len: edit.old_end() - edit.start(),
            })?;
    }
    Ok(sequential)
}

fn checked_len_delta(inserted_len: usize, removed_len: usize) -> Option<isize> {
    if inserted_len >= removed_len {
        isize::try_from(inserted_len - removed_len).ok()
    } else {
        isize::try_from(removed_len - inserted_len)
            .ok()
            .and_then(isize::checked_neg)
    }
}

pub(crate) fn shift_offset_by_delta(offset: usize, delta: isize) -> Option<usize> {
    if delta >= 0 {
        offset.checked_add(delta as usize)
    } else {
        offset.checked_sub(delta.unsigned_abs())
    }
}

fn _sort_snapshot_edits(edits: &[IncrementalEdit]) -> Vec<(usize, &IncrementalEdit)> {
    let mut indexed: Vec<(usize, &IncrementalEdit)> = edits.iter().enumerate().collect();
    indexed.sort_by_key(|(i, e)| (e.start(), e.old_end(), *i));
    indexed
}

fn _validate_snapshot_edit_ranges(
    base_len: usize,
    sorted: &[(usize, &IncrementalEdit)],
) -> Result<(), IncrementalEditError> {
    let mut prev_start = 0usize;
    let mut prev_end = 0usize;
    let mut prev_index: Option<usize> = None;
    for &(index, edit) in sorted {
        if edit.start() > base_len || edit.old_end() > base_len || edit.start() > edit.old_end() {
            return Err(IncrementalEditError::InvalidRange {
                index,
                start: edit.start(),
                old_end: edit.old_end(),
                len: base_len,
            });
        }
        if let Some(pi) = prev_index {
            if edit.start() < prev_end {
                return Err(IncrementalEditError::OverlappingEdits {
                    cur_index: index,
                    cur_start: edit.start(),
                    cur_end: edit.old_end(),
                    prev_index: pi,
                    prev_start,
                    prev_end,
                });
            }
        }
        prev_start = edit.start();
        prev_end = edit.old_end();
        prev_index = Some(index);
    }
    Ok(())
}

// ── Cache interval classification ─────────────────────────────────────────────

/// Classify interval `[start, end)` relative to edit region `[prefix, old_edit_end)`.
///
/// - `"prefix"` — interval is entirely before the edit region (`hi <= prefix`)
/// - `"suffix"` — interval is entirely after the edit region (`lo >= old_edit_end`)
/// - `None` — interval overlaps the edit region; entry cannot be transplanted
pub fn cache_interval_zone(
    start: usize,
    end: usize,
    prefix: usize,
    old_edit_end: usize,
) -> Option<&'static str> {
    let lo = start.min(end);
    let hi = start.max(end);
    if hi <= prefix {
        Some("prefix")
    } else if lo >= old_edit_end {
        Some("suffix")
    } else {
        None
    }
}

// ── Incremental step transforms ───────────────────────────────────────────────

/// Apply edit steps to `[start, end)`, producing the shifted interval plus any
/// `SpanShift`s needed to update provenance spans inside the entry.
///
/// Returns `None` if the interval overlaps any edit and cannot be transplanted.
pub fn apply_incremental_steps_to_entry(
    start: usize,
    end: usize,
    steps: &[IncrementalEditStep],
) -> Option<(usize, usize, Vec<SpanShift>)> {
    let mut cur_start = start;
    let mut cur_end = end;
    let mut span_shifts: Vec<SpanShift> = Vec::new();
    for step in steps {
        let zone = cache_interval_zone(cur_start, cur_end, step.start, step.old_end)?;
        if zone == "suffix" {
            if step.delta != 0 {
                span_shifts.push(SpanShift {
                    shift_from: step.old_end,
                    delta: step.delta,
                });
            }
            cur_start = shift_offset_by_delta(cur_start, step.delta)?;
            cur_end = shift_offset_by_delta(cur_end, step.delta)?;
        }
    }
    Some((cur_start, cur_end, span_shifts))
}

/// Apply edit steps to `[start, end)`, returning the shifted interval only.
///
/// Returns `None` if the interval overlaps any edit.
pub fn apply_incremental_steps_to_interval(
    start: usize,
    end: usize,
    steps: &[IncrementalEditStep],
) -> Option<(usize, usize)> {
    let mut cur_start = start;
    let mut cur_end = end;
    for step in steps {
        let zone = cache_interval_zone(cur_start, cur_end, step.start, step.old_end)?;
        if zone == "suffix" {
            cur_start = shift_offset_by_delta(cur_start, step.delta)?;
            cur_end = shift_offset_by_delta(cur_end, step.delta)?;
        }
    }
    Some((cur_start, cur_end))
}

// ── Boundary transplant helpers ───────────────────────────────────────────────

/// Build a `BoundaryTransplant` from old/new text lengths and the edit boundary.
pub fn boundary_transplant_plan(
    old_len: usize,
    new_len: usize,
    prefix: usize,
    old_edit_end: usize,
) -> Option<BoundaryTransplant> {
    Some(BoundaryTransplant {
        prefix,
        old_edit_end,
        delta: checked_len_delta(new_len, old_len)?,
    })
}

/// Transplant a cached entry `[start, end)` using a boundary plan.
///
/// Returns `(new_start, new_end, span_shifts)` or `None` on overlap.
pub fn transplant_cached_entry_with_boundary(
    start: usize,
    end: usize,
    boundary: &BoundaryTransplant,
) -> Option<(usize, usize, Vec<SpanShift>)> {
    let zone = cache_interval_zone(start, end, boundary.prefix, boundary.old_edit_end)?;
    if zone == "prefix" || boundary.delta == 0 {
        return Some((start, end, vec![]));
    }
    let new_start = shift_offset_by_delta(start, boundary.delta)?;
    let new_end = shift_offset_by_delta(end, boundary.delta)?;
    Some((
        new_start,
        new_end,
        vec![SpanShift {
            shift_from: boundary.old_edit_end,
            delta: boundary.delta,
        }],
    ))
}

/// Transplant an interval `[start, end)` using a boundary plan (no span tracking).
pub fn transplant_interval_with_boundary(
    start: usize,
    end: usize,
    boundary: &BoundaryTransplant,
) -> Option<(usize, usize)> {
    let zone = cache_interval_zone(start, end, boundary.prefix, boundary.old_edit_end)?;
    if zone == "prefix" || boundary.delta == 0 {
        return Some((start, end));
    }
    Some((
        shift_offset_by_delta(start, boundary.delta)?,
        shift_offset_by_delta(end, boundary.delta)?,
    ))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::IncrementalEdit;

    fn edit(start: usize, old_end: usize, replacement: impl Into<String>) -> IncrementalEdit {
        IncrementalEdit::new(start, old_end, replacement).expect("test edit must be valid")
    }

    // ── compile_incremental_edit_steps ────────────────────────────────────────

    #[test]
    fn compile_steps_single_insert() {
        let edits = vec![edit(3, 3, "XY")];
        let (steps, text) = compile_incremental_edit_steps("hello", &edits).unwrap();
        assert_eq!(text, "helXYlo");
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].delta, 2);
    }

    #[test]
    fn compile_steps_single_delete() {
        let edits = vec![edit(1, 4, "")];
        let (steps, text) = compile_incremental_edit_steps("hello", &edits).unwrap();
        assert_eq!(text, "ho");
        assert_eq!(steps[0].delta, -3);
    }

    #[test]
    fn compile_steps_replace() {
        let edits = vec![edit(0, 2, "AB")];
        let (steps, text) = compile_incremental_edit_steps("hello", &edits).unwrap();
        assert_eq!(text, "ABllo");
        assert_eq!(steps[0].delta, 0);
    }

    #[test]
    fn compile_steps_sequential_multiple() {
        // "abcde" → delete 'b' → "acde" → insert 'X' at 2 → "acXde"
        let edits = vec![edit(1, 2, ""), edit(2, 2, "X")];
        let (steps, text) = compile_incremental_edit_steps("abcde", &edits).unwrap();
        assert_eq!(text, "acXde");
        assert_eq!(steps[0].delta, -1);
        assert_eq!(steps[1].delta, 1);
    }

    #[test]
    fn compile_steps_invalid_range_returns_error() {
        let edits = vec![edit(3, 10, "x")];
        let err = compile_incremental_edit_steps("hi", &edits).unwrap_err();
        assert!(matches!(err, IncrementalEditError::InvalidRange { .. }));
    }

    // ── snapshot_edits_to_sequential ──────────────────────────────────────────

    #[test]
    fn snapshot_sequential_single() {
        let edits = vec![edit(1, 3, "XY")];
        let seq = snapshot_edits_to_sequential("hello", &edits).unwrap();
        assert_eq!(seq.len(), 1);
        assert_eq!(seq[0].start(), 1);
        assert_eq!(seq[0].old_end(), 3);
    }

    #[test]
    fn snapshot_sequential_sorts_and_shifts() {
        // Two non-overlapping snapshot edits given in reverse order:
        // edit[1] = (3,4,"Z"), edit[0] = (0,1,"AB")
        // sorted: (0,1,"AB") then (3,4,"Z")
        // sequential:
        //   (0,1,"AB") → delta=+1
        //   (3+1,4+1,"Z") = (4,5,"Z") → delta=0
        let edits = vec![edit(3, 4, "Z"), edit(0, 1, "AB")];
        let seq = snapshot_edits_to_sequential("hello", &edits).unwrap();
        assert_eq!(seq.len(), 2);
        assert_eq!(seq[0].start(), 0);
        assert_eq!(seq[0].old_end(), 1);
        assert_eq!(seq[0].replacement(), "AB");
        assert_eq!(seq[1].start(), 4); // shifted by +1
        assert_eq!(seq[1].old_end(), 5);
    }

    #[test]
    fn snapshot_sequential_empty() {
        let seq = snapshot_edits_to_sequential("hello", &[]).unwrap();
        assert!(seq.is_empty());
    }

    #[test]
    fn snapshot_sequential_overlapping_returns_error() {
        let edits = vec![edit(0, 3, "X"), edit(2, 4, "Y")];
        let err = snapshot_edits_to_sequential("hello", &edits).unwrap_err();
        assert!(matches!(err, IncrementalEditError::OverlappingEdits { .. }));
    }

    // ── cache_interval_zone ───────────────────────────────────────────────────

    #[test]
    fn zone_prefix() {
        // interval [0,3), prefix=5, old_edit_end=8 -> hi=3 <= prefix=5 -> "prefix"
        assert_eq!(cache_interval_zone(0, 3, 5, 8), Some("prefix"));
    }

    #[test]
    fn zone_suffix() {
        // interval [10,15), lo=10 >= old_edit_end=8 → "suffix"
        assert_eq!(cache_interval_zone(10, 15, 5, 8), Some("suffix"));
    }

    #[test]
    fn zone_overlap_returns_none() {
        // interval [3,7) overlaps edit region [5,8)
        assert_eq!(cache_interval_zone(3, 7, 5, 8), None);
    }

    #[test]
    fn zone_touching_boundary() {
        // interval [0,5) ends exactly where the edit starts, so it is still prefix.
        assert_eq!(cache_interval_zone(0, 5, 5, 8), Some("prefix"));
        // interval [8,10) starts exactly where the edit ends, so it is suffix.
        assert_eq!(cache_interval_zone(8, 10, 5, 8), Some("suffix"));
    }

    // ── apply_incremental_steps_to_interval ───────────────────────────────────

    #[test]
    fn steps_to_interval_prefix_unchanged() {
        let steps = vec![IncrementalEditStep {
            start: 5,
            old_end: 8,
            delta: 3,
        }];
        // interval [0,4) is prefix → unchanged
        assert_eq!(
            apply_incremental_steps_to_interval(0, 4, &steps),
            Some((0, 4))
        );
    }

    #[test]
    fn steps_to_interval_suffix_shifted() {
        let steps = vec![IncrementalEditStep {
            start: 5,
            old_end: 8,
            delta: 2,
        }];
        // interval [10,15) is suffix → shifted by +2
        assert_eq!(
            apply_incremental_steps_to_interval(10, 15, &steps),
            Some((12, 17))
        );
    }

    #[test]
    fn steps_to_interval_overlap_returns_none() {
        let steps = vec![IncrementalEditStep {
            start: 5,
            old_end: 8,
            delta: 1,
        }];
        // interval [4,9) overlaps edit → None
        assert_eq!(apply_incremental_steps_to_interval(4, 9, &steps), None);
    }

    #[test]
    fn steps_to_interval_drops_underflowed_shift() {
        let steps = vec![IncrementalEditStep {
            start: 0,
            old_end: 10,
            delta: -3,
        }];
        assert_eq!(apply_incremental_steps_to_interval(1, 2, &steps), None);
    }

    // ── apply_incremental_steps_to_entry ──────────────────────────────────────

    #[test]
    fn steps_to_entry_suffix_has_span_shift() {
        let steps = vec![IncrementalEditStep {
            start: 5,
            old_end: 8,
            delta: -2,
        }];
        let result = apply_incremental_steps_to_entry(10, 15, &steps);
        let (ns, ne, shifts) = result.unwrap();
        assert_eq!(ns, 8);
        assert_eq!(ne, 13);
        assert_eq!(shifts.len(), 1);
        assert_eq!(shifts[0].shift_from, 8); // old_end
        assert_eq!(shifts[0].delta, -2);
    }

    #[test]
    fn steps_to_entry_prefix_no_shifts() {
        let steps = vec![IncrementalEditStep {
            start: 5,
            old_end: 8,
            delta: 1,
        }];
        let (ns, ne, shifts) = apply_incremental_steps_to_entry(0, 3, &steps).unwrap();
        assert_eq!((ns, ne), (0, 3));
        assert!(shifts.is_empty());
    }

    // ── boundary transplant helpers ───────────────────────────────────────────

    #[test]
    fn boundary_plan_computes_delta() {
        let bp = boundary_transplant_plan(10, 14, 3, 8).unwrap();
        assert_eq!(bp.delta, 4);
        assert_eq!(bp.prefix, 3);
        assert_eq!(bp.old_edit_end, 8);
    }

    #[test]
    fn transplant_interval_prefix_unchanged() {
        let bp = boundary_transplant_plan(5, 7, 10, 15).unwrap();
        // interval [0,8) — hi=8 < prefix=10 → "prefix", no shift
        assert_eq!(transplant_interval_with_boundary(0, 8, &bp), Some((0, 8)));
    }

    #[test]
    fn transplant_interval_suffix_shifted() {
        let bp = boundary_transplant_plan(5, 7, 3, 6).unwrap();
        // interval [7,10) — lo=7 >= old_edit_end=6 → "suffix", delta=+2
        assert_eq!(transplant_interval_with_boundary(7, 10, &bp), Some((9, 12)));
    }

    #[test]
    fn transplant_entry_suffix_with_span_shift() {
        let bp = boundary_transplant_plan(5, 7, 3, 6).unwrap();
        let (ns, ne, shifts) = transplant_cached_entry_with_boundary(7, 10, &bp).unwrap();
        assert_eq!((ns, ne), (9, 12));
        assert_eq!(shifts.len(), 1);
        assert_eq!(shifts[0].shift_from, 6); // old_edit_end
        assert_eq!(shifts[0].delta, 2);
    }

    #[test]
    fn transplant_entry_no_delta_no_shift() {
        // delta=0 means no span shifts even if suffix
        let bp = boundary_transplant_plan(5, 5, 3, 6).unwrap();
        let (ns, ne, shifts) = transplant_cached_entry_with_boundary(7, 10, &bp).unwrap();
        assert_eq!((ns, ne), (7, 10));
        assert!(shifts.is_empty());
    }

    #[test]
    fn transplant_overlap_returns_none() {
        let bp = boundary_transplant_plan(5, 7, 3, 6).unwrap();
        // interval [4, 7) overlaps edit [3,6)
        assert_eq!(transplant_interval_with_boundary(4, 7, &bp), None);
    }

    #[test]
    fn boundary_plan_rejects_unrepresentable_delta() {
        assert!(boundary_transplant_plan(0, usize::MAX, 0, 0).is_none());
    }
}
