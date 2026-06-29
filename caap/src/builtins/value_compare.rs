use std::cmp::Ordering;

use crate::values::RuntimeValue;

/// Ordered comparison for the `lt`/`gt`/`le`/`ge` predicates. Comparable pairs:
/// Int/Int, Float/Float, Int↔Float (numeric mix), Str/Str (lexicographic).
/// Anything else is a TYPE ERROR, not a silent `false` (principle #11) — bools,
/// nulls, collections, callables and refs have no defined ordering. The total
/// order used by `sequence_sort_by` lives separately in [`runtime_value_cmp`]
/// and is intentionally unaffected: sorting heterogeneous keys stays total.
pub(super) fn compare_lt(
    a: &RuntimeValue,
    b: &RuntimeValue,
) -> Result<bool, crate::values::EvalSignal> {
    match (a, b) {
        (RuntimeValue::Int(x), RuntimeValue::Int(y)) => Ok(x < y),
        (RuntimeValue::Float(x), RuntimeValue::Float(y)) => Ok(x < y),
        (RuntimeValue::Str(x), RuntimeValue::Str(y)) => Ok(x.as_ref() < y.as_ref()),
        (RuntimeValue::Int(x), RuntimeValue::Float(y)) => Ok((*x as f64) < *y),
        (RuntimeValue::Float(x), RuntimeValue::Int(y)) => Ok(*x < (*y as f64)),
        _ => Err(crate::values::eval_err(format!(
            "ordered comparison: cannot compare {} with {}",
            crate::values::canonical_type_tag(a),
            crate::values::canonical_type_tag(b),
        ))),
    }
}

/// Structural ("deep") equality — the kernel twin of the stdlib `deep_eq`
/// facade. Scalars compare by value, closures/builtins/host objects/refs by
/// identity (exactly like `eq`); lists and tuples element-wise in order; maps
/// by key SET (order-insensitive) with deep-equal values. No numeric coercion
/// (1 ≠ 1.0) and IEEE float semantics, mirroring `eq` on atoms. Cyclic
/// structures terminate: a pointer pair already being compared is taken as
/// equal (coinductive reading), which CAAP-side implementations cannot do.
pub(super) fn deep_equal(a: &RuntimeValue, b: &RuntimeValue) -> bool {
    deep_equal_inner(a, b, &mut Vec::new())
}

fn deep_equal_inner(
    a: &RuntimeValue,
    b: &RuntimeValue,
    visiting: &mut Vec<(usize, usize)>,
) -> bool {
    use std::rc::Rc;
    crate::eval::grow_stack(|| match (a, b) {
        (RuntimeValue::List(x), RuntimeValue::List(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let pair = (Rc::as_ptr(x) as usize, Rc::as_ptr(y) as usize);
            if visiting.contains(&pair) {
                return true;
            }
            visiting.push(pair);
            let result = {
                let (x, y) = (x.borrow(), y.borrow());
                x.len() == y.len()
                    && x.iter()
                        .zip(y.iter())
                        .all(|(a, b)| deep_equal_inner(a, b, visiting))
            };
            visiting.pop();
            result
        }
        (RuntimeValue::Map(x), RuntimeValue::Map(y)) => {
            if Rc::ptr_eq(x, y) {
                return true;
            }
            let pair = (Rc::as_ptr(x) as usize, Rc::as_ptr(y) as usize);
            if visiting.contains(&pair) {
                return true;
            }
            visiting.push(pair);
            let result = {
                let (x, y) = (x.borrow(), y.borrow());
                x.len() == y.len()
                    && x.iter().all(|(key, value)| {
                        y.get(key)
                            .is_some_and(|other| deep_equal_inner(value, other, visiting))
                    })
            };
            visiting.pop();
            result
        }
        (RuntimeValue::Tuple(x), RuntimeValue::Tuple(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|(a, b)| deep_equal_inner(a, b, visiting))
        }
        _ => a == b,
    })
}

fn all_pairs(
    args: &[RuntimeValue],
    holds: impl Fn(&RuntimeValue, &RuntimeValue) -> Result<bool, crate::values::EvalSignal>,
) -> Result<bool, crate::values::EvalSignal> {
    for window in args.windows(2) {
        if !holds(&window[0], &window[1])? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn all_lt(args: &[RuntimeValue]) -> Result<bool, crate::values::EvalSignal> {
    all_pairs(args, compare_lt)
}

pub(super) fn all_gt(args: &[RuntimeValue]) -> Result<bool, crate::values::EvalSignal> {
    all_pairs(args, |a, b| compare_lt(b, a))
}

pub(super) fn all_le(args: &[RuntimeValue]) -> Result<bool, crate::values::EvalSignal> {
    all_pairs(args, |a, b| Ok(!compare_lt(b, a)?))
}

pub(super) fn all_ge(args: &[RuntimeValue]) -> Result<bool, crate::values::EvalSignal> {
    all_pairs(args, |a, b| Ok(!compare_lt(a, b)?))
}

pub fn runtime_value_cmp(a: &RuntimeValue, b: &RuntimeValue) -> Ordering {
    match (a, b) {
        (RuntimeValue::Null, RuntimeValue::Null) => Ordering::Equal,
        (RuntimeValue::Bool(x), RuntimeValue::Bool(y)) => x.cmp(y),
        (RuntimeValue::Int(x), RuntimeValue::Int(y)) => x.cmp(y),
        (RuntimeValue::Float(x), RuntimeValue::Float(y)) => x.total_cmp(y),
        (RuntimeValue::Str(x), RuntimeValue::Str(y)) => x.as_ref().cmp(y.as_ref()),
        (RuntimeValue::Int(x), RuntimeValue::Float(y)) => (*x as f64).total_cmp(y),
        (RuntimeValue::Float(x), RuntimeValue::Int(y)) => x.total_cmp(&(*y as f64)),
        _ => sort_type_rank(a).cmp(&sort_type_rank(b)),
    }
}

fn sort_type_rank(value: &RuntimeValue) -> u8 {
    match value {
        RuntimeValue::Null => 0,
        RuntimeValue::Bool(_) => 1,
        RuntimeValue::Int(_) => 2,
        RuntimeValue::Float(_) => 3,
        RuntimeValue::Str(_) => 4,
        _ => 255,
    }
}

/// Total structural order over ALL value kinds — the `value_compare` builtin and
/// the order twin of [`deep_equal`]. Unlike [`runtime_value_cmp`] (the shallow
/// sort key used by `sequence_sort_by`, deliberately unchanged), this orders
/// first by a stable per-kind rank ([`deep_type_rank`]) and then *structurally*
/// within a kind: scalars by value, bytes/strings lexicographically, lists and
/// tuples lexicographically by element, maps by their key/value pairs in sorted
/// key order (so the order is insensitive to map insertion order — matching
/// [`deep_equal`], which compares maps by key SET). Int and Float are SEPARATE
/// ranks (no numeric coercion: 1 and 1.0 are ordered apart, never equal),
/// keeping the IFF with [`deep_equal`]: `deep_compare(a, b) == Equal` exactly
/// when `deep_equal(a, b)`. Identity-typed values (closures/macros/builtins/
/// host objects/refs) have no structural content, so they order by kind rank and
/// then by pointer address — deterministic within a process but, like `eq` on
/// them, NOT meaningful across runs. Cyclic structures terminate: a pointer pair
/// already on the comparison stack is taken as `Equal` (coinductive, mirroring
/// [`deep_equal`]).
pub fn deep_compare(a: &RuntimeValue, b: &RuntimeValue) -> Ordering {
    deep_compare_inner(a, b, &mut Vec::new())
}

fn deep_compare_inner(
    a: &RuntimeValue,
    b: &RuntimeValue,
    visiting: &mut Vec<(usize, usize)>,
) -> Ordering {
    use std::rc::Rc;
    crate::eval::grow_stack(|| {
        let rank = deep_type_rank(a).cmp(&deep_type_rank(b));
        if rank != Ordering::Equal {
            return rank;
        }
        match (a, b) {
            (RuntimeValue::Null, RuntimeValue::Null)
            | (RuntimeValue::UninitializedTopLevel, RuntimeValue::UninitializedTopLevel) => {
                Ordering::Equal
            }
            (RuntimeValue::Bool(x), RuntimeValue::Bool(y)) => x.cmp(y),
            (RuntimeValue::Int(x), RuntimeValue::Int(y)) => x.cmp(y),
            (RuntimeValue::Float(x), RuntimeValue::Float(y)) => x.total_cmp(y),
            (RuntimeValue::Str(x), RuntimeValue::Str(y)) => x.as_ref().cmp(y.as_ref()),
            (RuntimeValue::Bytes(x), RuntimeValue::Bytes(y)) => x.as_ref().cmp(y.as_ref()),
            (RuntimeValue::Tuple(x), RuntimeValue::Tuple(y)) => compare_slices(x, y, visiting),
            (RuntimeValue::List(x), RuntimeValue::List(y)) => {
                if Rc::ptr_eq(x, y) {
                    return Ordering::Equal;
                }
                let pair = (Rc::as_ptr(x) as usize, Rc::as_ptr(y) as usize);
                if visiting.contains(&pair) {
                    return Ordering::Equal;
                }
                visiting.push(pair);
                let (xb, yb) = (x.borrow(), y.borrow());
                let result = compare_slices(&xb, &yb, visiting);
                drop((xb, yb));
                visiting.pop();
                result
            }
            (RuntimeValue::Map(x), RuntimeValue::Map(y)) => {
                if Rc::ptr_eq(x, y) {
                    return Ordering::Equal;
                }
                let pair = (Rc::as_ptr(x) as usize, Rc::as_ptr(y) as usize);
                if visiting.contains(&pair) {
                    return Ordering::Equal;
                }
                visiting.push(pair);
                let result = compare_maps(&x.borrow(), &y.borrow(), visiting);
                visiting.pop();
                result
            }
            // Identity-typed kinds (same rank): no structural content, so order by
            // pointer address for a deterministic-within-a-run total order.
            _ => identity_addr(a).cmp(&identity_addr(b)),
        }
    })
}

fn compare_slices(
    x: &[RuntimeValue],
    y: &[RuntimeValue],
    visiting: &mut Vec<(usize, usize)>,
) -> Ordering {
    for (a, b) in x.iter().zip(y.iter()) {
        let ord = deep_compare_inner(a, b, visiting);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    x.len().cmp(&y.len())
}

fn compare_maps(
    x: &indexmap::IndexMap<crate::values::MapKey, RuntimeValue>,
    y: &indexmap::IndexMap<crate::values::MapKey, RuntimeValue>,
    visiting: &mut Vec<(usize, usize)>,
) -> Ordering {
    // Compare by entries in sorted-key order so the result is insensitive to
    // insertion order — keeping the IFF with `deep_equal` (key-SET comparison).
    let mut xe: Vec<_> = x.iter().collect();
    let mut ye: Vec<_> = y.iter().collect();
    xe.sort_by(|a, b| a.0.cmp(b.0));
    ye.sort_by(|a, b| a.0.cmp(b.0));
    for ((xk, xv), (yk, yv)) in xe.iter().zip(ye.iter()) {
        let key_ord = xk.cmp(yk);
        if key_ord != Ordering::Equal {
            return key_ord;
        }
        let val_ord = deep_compare_inner(xv, yv, visiting);
        if val_ord != Ordering::Equal {
            return val_ord;
        }
    }
    xe.len().cmp(&ye.len())
}

/// Stable per-kind rank for the TOTAL structural order: null < bool < int <
/// float < string < bytes < tuple < list < map < (identity-typed kinds). Int and
/// Float are distinct ranks (no coercion). Distinct from [`sort_type_rank`],
/// which lumps every non-scalar at 255 for the shallow `sequence_sort_by` order.
fn deep_type_rank(value: &RuntimeValue) -> u8 {
    match value {
        RuntimeValue::Null => 0,
        RuntimeValue::Bool(_) => 1,
        RuntimeValue::Int(_) => 2,
        RuntimeValue::Float(_) => 3,
        RuntimeValue::Str(_) => 4,
        RuntimeValue::Bytes(_) => 5,
        RuntimeValue::Tuple(_) => 6,
        RuntimeValue::List(_) => 7,
        RuntimeValue::Map(_) => 8,
        RuntimeValue::Closure(_) => 9,
        RuntimeValue::Macro(_) => 10,
        RuntimeValue::Builtin(_) => 11,
        RuntimeValue::HostFunction(_) => 12,
        RuntimeValue::HostObject(_) => 13,
        RuntimeValue::Ref(_) => 14,
        RuntimeValue::UninitializedTopLevel => 15,
    }
}

/// Pointer address of an identity-typed value, used only as a within-run
/// tiebreaker for the total order (these kinds carry no structural content and,
/// like `eq` on them, are not cross-run meaningful).
fn identity_addr(value: &RuntimeValue) -> usize {
    use std::rc::Rc;
    match value {
        RuntimeValue::Closure(p) => Rc::as_ptr(p) as usize,
        RuntimeValue::Macro(p) => Rc::as_ptr(p) as usize,
        RuntimeValue::Builtin(p) => Rc::as_ptr(p) as usize,
        RuntimeValue::HostFunction(p) => Rc::as_ptr(p) as usize,
        RuntimeValue::HostObject(p) => Rc::as_ptr(p) as *const () as usize,
        RuntimeValue::Ref(p) => Rc::as_ptr(p) as usize,
        _ => 0,
    }
}

/// Structural hash to an `i64` — the `value_hash` builtin. Structurally equal
/// values ([`deep_equal`]) hash equal: it walks the value the same way
/// `deep_equal` reads it (scalars by value, bytes/strings by content, lists and
/// tuples element-wise in order, maps by key/value pairs folded order-
/// independently so insertion order is irrelevant), running everything through an
/// FNV-1a 64-bit accumulator that mixes in a per-kind tag so e.g. `1` and the
/// one-element list `[1]` do not collide. Deterministic across runs for the data
/// kinds; identity-typed values (closures/refs/host objects) contribute only
/// their kind tag — `deep_equal` distinguishes them by identity, so hashing them
/// equal is sound (equal-implies-equal-hash holds; it only loosens the hash,
/// never the order). Cycle-safe: a pointer already on the walk stack contributes
/// a sentinel.
pub fn deep_hash(value: &RuntimeValue) -> i64 {
    let mut state = FNV_OFFSET;
    deep_hash_inner(value, &mut state, &mut Vec::new());
    state as i64
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv_mix(state: &mut u64, bytes: &[u8]) {
    for &byte in bytes {
        *state ^= u64::from(byte);
        *state = state.wrapping_mul(FNV_PRIME);
    }
}

fn deep_hash_inner(value: &RuntimeValue, state: &mut u64, visiting: &mut Vec<usize>) {
    use std::rc::Rc;
    crate::eval::grow_stack(|| {
        // Tag every value with its kind rank so differently-shaped values that
        // would otherwise fold to the same bytes (e.g. 1 vs [1]) stay distinct.
        fnv_mix(state, &[deep_type_rank(value)]);
        match value {
            RuntimeValue::Null | RuntimeValue::UninitializedTopLevel => {}
            RuntimeValue::Bool(b) => fnv_mix(state, &[u8::from(*b)]),
            RuntimeValue::Int(i) => fnv_mix(state, &i.to_le_bytes()),
            RuntimeValue::Float(f) => fnv_mix(state, &f.to_bits().to_le_bytes()),
            RuntimeValue::Str(s) => fnv_mix(state, s.as_bytes()),
            RuntimeValue::Bytes(b) => fnv_mix(state, b),
            RuntimeValue::Tuple(items) => {
                for item in items.iter() {
                    deep_hash_inner(item, state, visiting);
                }
            }
            RuntimeValue::List(items) => {
                let ptr = Rc::as_ptr(items) as usize;
                if visiting.contains(&ptr) {
                    fnv_mix(state, b"<cycle>");
                    return;
                }
                visiting.push(ptr);
                for item in items.borrow().iter() {
                    deep_hash_inner(item, state, visiting);
                }
                visiting.pop();
            }
            RuntimeValue::Map(map) => {
                let ptr = Rc::as_ptr(map) as usize;
                if visiting.contains(&ptr) {
                    fnv_mix(state, b"<cycle>");
                    return;
                }
                visiting.push(ptr);
                // Fold each entry into a SEPARATE FNV accumulator and XOR the
                // results: XOR is commutative, so the map hash is independent of
                // entry order (matching `deep_equal`'s key-SET comparison).
                let mut combined: u64 = 0;
                for (key, item) in map.borrow().iter() {
                    let mut entry = FNV_OFFSET;
                    hash_map_key(key, &mut entry);
                    deep_hash_inner(item, &mut entry, visiting);
                    combined ^= entry;
                }
                fnv_mix(state, &combined.to_le_bytes());
                visiting.pop();
            }
            // Identity-typed kinds: only the kind tag (already mixed) contributes.
            RuntimeValue::Closure(_)
            | RuntimeValue::Macro(_)
            | RuntimeValue::Builtin(_)
            | RuntimeValue::HostFunction(_)
            | RuntimeValue::HostObject(_)
            | RuntimeValue::Ref(_) => {}
        }
    })
}

fn hash_map_key(key: &crate::values::MapKey, state: &mut u64) {
    use crate::values::MapKey;
    // Mirror the RuntimeValue kind tags so a {1: ...} map key hashes the same
    // whether it arrived as a MapKey or a scalar RuntimeValue would.
    match key {
        MapKey::Null => fnv_mix(state, &[0]),
        MapKey::Bool(b) => {
            fnv_mix(state, &[1]);
            fnv_mix(state, &[u8::from(*b)]);
        }
        MapKey::Int(i) => {
            fnv_mix(state, &[2]);
            fnv_mix(state, &i.to_le_bytes());
        }
        MapKey::Str(s) => {
            fnv_mix(state, &[4]);
            fnv_mix(state, s.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::values::MapKey;

    #[test]
    fn deep_equal_terminates_on_cyclic_structures() {
        use std::cell::RefCell;
        use std::rc::Rc;
        // a = [1, a]; b = [1, b] — structurally indistinguishable cycles:
        // coinductively equal, and (the real point) the comparison terminates.
        let a = Rc::new(RefCell::new(vec![RuntimeValue::Int(1)]));
        a.borrow_mut().push(RuntimeValue::List(Rc::clone(&a)));
        let b = Rc::new(RefCell::new(vec![RuntimeValue::Int(1)]));
        b.borrow_mut().push(RuntimeValue::List(Rc::clone(&b)));
        assert!(deep_equal(
            &RuntimeValue::List(Rc::clone(&a)),
            &RuntimeValue::List(Rc::clone(&b))
        ));
        // c = [2, c] differs in the scalar — unequal despite the cycle.
        let c = Rc::new(RefCell::new(vec![RuntimeValue::Int(2)]));
        c.borrow_mut().push(RuntimeValue::List(Rc::clone(&c)));
        assert!(!deep_equal(&RuntimeValue::List(a), &RuntimeValue::List(c)));
    }

    #[test]
    fn compare_lt_supports_mixed_numeric_values() {
        assert!(compare_lt(&RuntimeValue::Int(1), &RuntimeValue::Float(1.5)).unwrap());
        assert!(compare_lt(&RuntimeValue::Float(1.5), &RuntimeValue::Int(2)).unwrap());
    }

    #[test]
    fn compare_lt_rejects_incomparable_types() {
        assert!(compare_lt(&RuntimeValue::Int(1), &RuntimeValue::Str("a".into())).is_err());
        assert!(compare_lt(&RuntimeValue::Bool(true), &RuntimeValue::Bool(false)).is_err());
        assert!(compare_lt(&RuntimeValue::Null, &RuntimeValue::Int(0)).is_err());
    }

    #[test]
    fn runtime_value_cmp_uses_total_float_ordering() {
        assert_eq!(
            runtime_value_cmp(
                &RuntimeValue::Float(f64::NAN),
                &RuntimeValue::Float(f64::NAN)
            ),
            Ordering::Equal
        );
        assert_eq!(
            runtime_value_cmp(&RuntimeValue::Float(f64::NAN), &RuntimeValue::Int(0)),
            Ordering::Greater
        );
        assert_eq!(
            runtime_value_cmp(
                &RuntimeValue::Float(f64::NEG_INFINITY),
                &RuntimeValue::Float(f64::INFINITY)
            ),
            Ordering::Less
        );
    }

    // ── value_compare / value_hash (the deep total order + structural hash) ──

    use std::cell::RefCell;
    use std::rc::Rc;

    fn list(items: Vec<RuntimeValue>) -> RuntimeValue {
        RuntimeValue::List(Rc::new(RefCell::new(items)))
    }

    fn map(entries: Vec<(MapKey, RuntimeValue)>) -> RuntimeValue {
        RuntimeValue::Map(Rc::new(RefCell::new(entries.into_iter().collect())))
    }

    /// A heterogeneous, nested sample exercising every data kind and ordering
    /// across kinds (null < bool < int < float < string < bytes < tuple < list
    /// < map). Returned roughly in ascending order to pin the cross-kind ranks.
    fn mixed_sample() -> Vec<RuntimeValue> {
        vec![
            RuntimeValue::Null,
            RuntimeValue::Bool(false),
            RuntimeValue::Bool(true),
            RuntimeValue::Int(-5),
            RuntimeValue::Int(0),
            RuntimeValue::Int(7),
            RuntimeValue::Float(-1.0),
            RuntimeValue::Float(3.5),
            RuntimeValue::Str("apple".into()),
            RuntimeValue::Str("banana".into()),
            RuntimeValue::Bytes(Rc::from(&b"ab"[..])),
            RuntimeValue::Bytes(Rc::from(&b"ac"[..])),
            RuntimeValue::Tuple(Rc::from(
                vec![RuntimeValue::Int(1), RuntimeValue::Int(2)].as_slice(),
            )),
            list(vec![RuntimeValue::Int(1)]),
            list(vec![RuntimeValue::Int(1), list(vec![RuntimeValue::Int(2)])]),
            map(vec![(MapKey::Str("k".into()), RuntimeValue::Int(1))]),
        ]
    }

    #[test]
    fn value_compare_orders_across_kinds_by_rank() {
        let sample = mixed_sample();
        for window in sample.windows(2) {
            assert_eq!(
                deep_compare(&window[0], &window[1]),
                Ordering::Less,
                "{:?} should sort before {:?}",
                window[0],
                window[1],
            );
            assert_eq!(
                deep_compare(&window[1], &window[0]),
                Ordering::Greater,
                "antisymmetry: {:?} vs {:?}",
                window[1],
                window[0],
            );
        }
    }

    #[test]
    fn value_compare_is_antisymmetric_reflexive_and_transitive() {
        let sample = mixed_sample();
        for a in &sample {
            assert_eq!(deep_compare(a, a), Ordering::Equal, "reflexive on {a:?}");
            for b in &sample {
                // Antisymmetry: cmp(a,b) is the reverse of cmp(b,a).
                assert_eq!(
                    deep_compare(a, b),
                    deep_compare(b, a).reverse(),
                    "antisymmetry on {a:?} / {b:?}",
                );
                for c in &sample {
                    // Transitivity of strict order.
                    if deep_compare(a, b) == Ordering::Less && deep_compare(b, c) == Ordering::Less
                    {
                        assert_eq!(
                            deep_compare(a, c),
                            Ordering::Less,
                            "transitivity {a:?} < {b:?} < {c:?}",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn value_compare_lists_lexicographically_and_by_length() {
        // [1, 2] < [1, 3]; the shorter prefix [1] < [1, 0].
        assert_eq!(
            deep_compare(
                &list(vec![RuntimeValue::Int(1), RuntimeValue::Int(2)]),
                &list(vec![RuntimeValue::Int(1), RuntimeValue::Int(3)]),
            ),
            Ordering::Less,
        );
        assert_eq!(
            deep_compare(
                &list(vec![RuntimeValue::Int(1)]),
                &list(vec![RuntimeValue::Int(1), RuntimeValue::Int(0)]),
            ),
            Ordering::Less,
        );
    }

    #[test]
    fn value_compare_maps_are_insertion_order_insensitive() {
        // Same key/value SET, different insertion order — must compare Equal,
        // matching deep_equal's key-set semantics.
        let m1 = map(vec![
            (MapKey::Str("a".into()), RuntimeValue::Int(1)),
            (MapKey::Str("b".into()), RuntimeValue::Int(2)),
        ]);
        let m2 = map(vec![
            (MapKey::Str("b".into()), RuntimeValue::Int(2)),
            (MapKey::Str("a".into()), RuntimeValue::Int(1)),
        ]);
        assert_eq!(deep_compare(&m1, &m2), Ordering::Equal);
        assert!(deep_equal(&m1, &m2));
        // A differing value breaks the tie deterministically.
        let m3 = map(vec![
            (MapKey::Str("a".into()), RuntimeValue::Int(1)),
            (MapKey::Str("b".into()), RuntimeValue::Int(9)),
        ]);
        assert_eq!(deep_compare(&m1, &m3), Ordering::Less);
    }

    #[test]
    fn value_compare_equal_iff_value_eq() {
        // The keystone invariant: deep_compare == Equal exactly when deep_equal.
        let sample = mixed_sample();
        // Add a couple of structurally-equal-but-distinct clones to exercise the
        // "Equal yet different objects" branch.
        let mut all = sample.clone();
        all.push(list(vec![RuntimeValue::Int(1)])); // == sample[13]
        all.push(map(vec![(MapKey::Str("k".into()), RuntimeValue::Int(1))])); // == sample[15]
        for a in &all {
            for b in &all {
                assert_eq!(
                    deep_compare(a, b) == Ordering::Equal,
                    deep_equal(a, b),
                    "value_compare==0 must match value_eq for {a:?} / {b:?}",
                );
            }
        }
    }

    #[test]
    fn value_compare_distinguishes_int_and_float() {
        // No numeric coercion: 1 and 1.0 are ordered apart and never equal,
        // consistent with deep_equal (1 != 1.0).
        assert_ne!(
            deep_compare(&RuntimeValue::Int(1), &RuntimeValue::Float(1.0)),
            Ordering::Equal,
        );
        assert!(!deep_equal(
            &RuntimeValue::Int(1),
            &RuntimeValue::Float(1.0)
        ));
    }

    #[test]
    fn value_compare_terminates_on_cyclic_structures() {
        // a = [1, a]; b = [1, b] — cyclic, structurally indistinguishable.
        let a = Rc::new(RefCell::new(vec![RuntimeValue::Int(1)]));
        a.borrow_mut().push(RuntimeValue::List(Rc::clone(&a)));
        let b = Rc::new(RefCell::new(vec![RuntimeValue::Int(1)]));
        b.borrow_mut().push(RuntimeValue::List(Rc::clone(&b)));
        assert_eq!(
            deep_compare(&RuntimeValue::List(a), &RuntimeValue::List(b)),
            Ordering::Equal,
        );
    }

    #[test]
    fn value_hash_equal_for_structurally_equal_values() {
        // Distinct objects, same structure → same hash (the core contract).
        let a = list(vec![
            RuntimeValue::Int(1),
            RuntimeValue::Str("x".into()),
            map(vec![(MapKey::Int(2), RuntimeValue::Bool(true))]),
        ]);
        let b = list(vec![
            RuntimeValue::Int(1),
            RuntimeValue::Str("x".into()),
            map(vec![(MapKey::Int(2), RuntimeValue::Bool(true))]),
        ]);
        assert!(deep_equal(&a, &b));
        assert_eq!(deep_hash(&a), deep_hash(&b));

        // Map hash is insertion-order independent.
        let m1 = map(vec![
            (MapKey::Str("a".into()), RuntimeValue::Int(1)),
            (MapKey::Str("b".into()), RuntimeValue::Int(2)),
        ]);
        let m2 = map(vec![
            (MapKey::Str("b".into()), RuntimeValue::Int(2)),
            (MapKey::Str("a".into()), RuntimeValue::Int(1)),
        ]);
        assert_eq!(deep_hash(&m1), deep_hash(&m2));
    }

    #[test]
    fn value_hash_differs_for_distinct_values() {
        // A handful of distinct values should not collide. (Hashing can collide
        // in principle, but these concrete cases must stay distinct.)
        let values = [
            RuntimeValue::Null,
            RuntimeValue::Bool(true),
            RuntimeValue::Int(1),
            RuntimeValue::Float(1.0),
            RuntimeValue::Str("1".into()),
            list(vec![RuntimeValue::Int(1)]),
            RuntimeValue::Tuple(Rc::from(vec![RuntimeValue::Int(1)].as_slice())),
            map(vec![(MapKey::Int(1), RuntimeValue::Int(1))]),
        ];
        for (i, a) in values.iter().enumerate() {
            for (j, b) in values.iter().enumerate() {
                if i != j {
                    assert_ne!(
                        deep_hash(a),
                        deep_hash(b),
                        "hash collision between distinct {a:?} and {b:?}",
                    );
                }
            }
        }
        // 1 (int) and [1] (one-element list) must not collide despite both
        // folding the integer 1 — the kind tag separates them.
        assert_ne!(
            deep_hash(&RuntimeValue::Int(1)),
            deep_hash(&list(vec![RuntimeValue::Int(1)])),
        );
    }

    #[test]
    fn value_hash_terminates_on_cyclic_structures() {
        let a = Rc::new(RefCell::new(vec![RuntimeValue::Int(1)]));
        a.borrow_mut().push(RuntimeValue::List(Rc::clone(&a)));
        // The point is termination; the value just needs to be well-defined.
        let _ = deep_hash(&RuntimeValue::List(a));
    }
}
