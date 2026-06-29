/// Sequence + polymorphic container builtins.
///
/// Covers: sequence-range, sequence-each, sequence-map, sequence-filter,
/// sequence-fold-left, sequence-any, sequence-all, sequence-find,
/// sequence-find-reverse, sequence-count, sequence-reverse, sequence-slice,
/// sequence-take, sequence-drop, sequence-zip, sequence-flatten,
/// sequence-join, sequence-index-of,
/// get, get-strict, size, contains, map-keys, map-values, map-merge.
///
/// Note: `sequence-unique-by` accepts 1 or 2 args. With 1 arg it uses identity
/// keying (equivalent to the removed `sequence-distinct`). The builtins
/// `sequence-distinct`, `sequence-sort-by-desc`, and `for-range` have been
/// removed; use `sequence-unique-by`, `sequence-reverse`+`sequence-sort-by`,
/// and `sequence-each`+`sequence-range` respectively.
use indexmap::IndexMap;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::bridges::NodeBridgeValue;
use crate::compiler::UnitBridgeValue;
use crate::eval::{eval_args, Evaluator};
use crate::ir::Node;
use crate::values::{
    eval_err, is_truthy, ordered_runtime_map_entries, require_int_strict, require_map, require_str,
    MapKey, RuntimeValue,
};

// ── helper: require a sequence-like value ────────────────────────────────────

fn require_seq(
    v: &RuntimeValue,
    ctx: &str,
) -> Result<Vec<RuntimeValue>, crate::values::EvalSignal> {
    match v {
        RuntimeValue::List(l) => Ok(l.borrow().clone()),
        RuntimeValue::Tuple(items) => Ok(items.iter().cloned().collect()),
        other => Err(eval_err(format!("{ctx}: expected sequence, got {other}"))),
    }
}

pub fn register(ev: &mut Evaluator) {
    // ── sequence-range ────────────────────────────────────────────────────────
    ev.register_special(
        "sequence_range",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int", "int"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let start = require_int_strict(&args[0], "sequence_range")?;
            let end = require_int_strict(&args[1], "sequence_range")?;
            if end <= start {
                return Ok(RuntimeValue::List(Rc::new(RefCell::new(vec![]))));
            }
            ensure_sequence_len("sequence_range", range_len(start, end)?, ev)?;
            let list: Vec<RuntimeValue> = (start..end).map(RuntimeValue::Int).collect();
            Ok(RuntimeValue::List(Rc::new(RefCell::new(list))))
        },
    );

    // ── sequence-each ─────────────────────────────────────────────────────────
    ev.register_special(
        "sequence_each",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "callable"], "null"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_each")?;
            let cb = args[1].clone();
            for item in seq {
                ev.invoke_callback(&cb, vec![item])?;
            }
            Ok(RuntimeValue::Null)
        },
    );

    // ── sequence-each-indexed ─────────────────────────────────────────────────
    ev.register_special(
        "sequence_each_indexed",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "callable"], "null"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_each_indexed")?;
            let cb = args[1].clone();
            for (i, item) in seq.into_iter().enumerate() {
                ev.invoke_callback(&cb, vec![item, RuntimeValue::Int(i as i64)])?;
            }
            Ok(RuntimeValue::Null)
        },
    );

    // ── sequence-map ──────────────────────────────────────────────────────────
    ev.register_special(
        "sequence_map",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "callable"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_map")?;
            let cb = args[1].clone();
            ensure_sequence_len("sequence_map", seq.len(), ev)?;
            let mut result = Vec::with_capacity(seq.len());
            for item in seq {
                result.push(ev.invoke_callback(&cb, vec![item])?);
            }
            Ok(RuntimeValue::List(Rc::new(RefCell::new(result))))
        },
    );

    // ── sequence-filter ───────────────────────────────────────────────────────
    ev.register_special(
        "sequence_filter",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "callable"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_filter")?;
            let cb = args[1].clone();
            let mut result = Vec::new();
            for item in seq {
                let keep = ev.invoke_callback(&cb, vec![item.clone()])?;
                if is_truthy(&keep) {
                    result.push(item);
                }
            }
            ensure_sequence_len("sequence_filter", result.len(), ev)?;
            Ok(RuntimeValue::List(Rc::new(RefCell::new(result))))
        },
    );

    // ── sequence-fold-left ────────────────────────────────────────────────────
    ev.register_special(
        "sequence_fold_left",
        3,
        Some(3),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "any", "callable"], "any"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_fold_left")?;
            let mut acc = args[1].clone();
            let cb = args[2].clone();
            for item in seq {
                acc = ev.invoke_callback(&cb, vec![acc, item])?;
            }
            Ok(acc)
        },
    );

    // ── sequence-find ─────────────────────────────────────────────────────────
    ev.register_special(
        "sequence_find",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "callable"], "any"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_find")?;
            let cb = args[1].clone();
            for item in seq {
                let matched = ev.invoke_callback(&cb, vec![item.clone()])?;
                if is_truthy(&matched) {
                    return Ok(item);
                }
            }
            Ok(RuntimeValue::Null)
        },
    );

    // ── sequence-find-reverse ─────────────────────────────────────────────────
    ev.register_special(
        "sequence_find_reverse",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "callable"], "any"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_find_reverse")?;
            let cb = args[1].clone();
            for item in seq.into_iter().rev() {
                let matched = ev.invoke_callback(&cb, vec![item.clone()])?;
                if is_truthy(&matched) {
                    return Ok(item);
                }
            }
            Ok(RuntimeValue::Null)
        },
    );

    // ── sequence-any ─────────────────────────────────────────────────────────
    ev.register_special(
        "sequence_any",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "callable"], "bool"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_any")?;
            let cb = args[1].clone();
            for item in seq {
                if is_truthy(&ev.invoke_callback(&cb, vec![item])?) {
                    return Ok(RuntimeValue::Bool(true));
                }
            }
            Ok(RuntimeValue::Bool(false))
        },
    );

    // ── sequence-all ─────────────────────────────────────────────────────────
    ev.register_special(
        "sequence_all",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "callable"], "bool"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_all")?;
            let cb = args[1].clone();
            for item in seq {
                if !is_truthy(&ev.invoke_callback(&cb, vec![item])?) {
                    return Ok(RuntimeValue::Bool(false));
                }
            }
            Ok(RuntimeValue::Bool(true))
        },
    );

    // ── sequence-count ────────────────────────────────────────────────────────
    ev.register_special(
        "sequence_count",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "callable"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_count")?;
            let cb = args[1].clone();
            let mut count: i64 = 0;
            for item in seq {
                if is_truthy(&ev.invoke_callback(&cb, vec![item])?) {
                    count += 1;
                }
            }
            Ok(RuntimeValue::Int(count))
        },
    );

    // ── sequence-index-of ─────────────────────────────────────────────────────
    ev.register_special(
        "sequence_index_of",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "callable"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_index_of")?;
            let cb = args[1].clone();
            for (i, item) in seq.into_iter().enumerate() {
                if is_truthy(&ev.invoke_callback(&cb, vec![item])?) {
                    return Ok(RuntimeValue::Int(i as i64));
                }
            }
            Ok(RuntimeValue::Int(-1))
        },
    );

    // ── sequence-reverse ──────────────────────────────────────────────────────
    ev.register_special(
        "sequence_reverse",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["list"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let mut seq = require_seq(&args[0], "sequence_reverse")?;
            ensure_sequence_len("sequence_reverse", seq.len(), ev)?;
            seq.reverse();
            Ok(RuntimeValue::List(Rc::new(RefCell::new(seq))))
        },
    );

    // ── sequence-slice ────────────────────────────────────────────────────────
    ev.register_special(
        "sequence_slice",
        2,
        Some(3),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "int", "int"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_slice")?;
            let start =
                normalize_slice_index(require_int_strict(&args[1], "sequence_slice")?, seq.len());
            let end = optional_slice_end(args.get(2), seq.len(), "sequence_slice")?;
            let sliced = if end <= start {
                Vec::new()
            } else {
                seq[start..end].to_vec()
            };
            ensure_sequence_len("sequence_slice", sliced.len(), ev)?;
            Ok(RuntimeValue::List(Rc::new(RefCell::new(sliced))))
        },
    );

    // ── sequence-take ─────────────────────────────────────────────────────────
    ev.register_special(
        "sequence_take",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["list", "int"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_take")?;
            let n = require_non_negative_count(&args[1], "sequence_take")?;
            let taken: Vec<_> = seq.into_iter().take(n).collect();
            ensure_sequence_len("sequence_take", taken.len(), ev)?;
            Ok(RuntimeValue::List(Rc::new(RefCell::new(taken))))
        },
    );

    // ── sequence-drop ─────────────────────────────────────────────────────────
    ev.register_special(
        "sequence_drop",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["list", "int"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_drop")?;
            let n = require_non_negative_count(&args[1], "sequence_drop")?;
            let dropped: Vec<_> = seq.into_iter().skip(n).collect();
            ensure_sequence_len("sequence_drop", dropped.len(), ev)?;
            Ok(RuntimeValue::List(Rc::new(RefCell::new(dropped))))
        },
    );

    // ── sequence-flatten ──────────────────────────────────────────────────────
    ev.register_special(
        "sequence_flatten",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["list"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_flatten")?;
            let mut result = Vec::new();
            for item in seq {
                let inner = require_seq(&item, "sequence_flatten: inner item")?;
                let next = result
                    .len()
                    .checked_add(inner.len())
                    .ok_or_else(|| eval_err("sequence_flatten: list size overflow"))?;
                // Limit check on the cumulative length, but charge only this
                // iteration's INCREMENT — charging the running total each pass
                // would be quadratic and falsely trip the budget on legit data.
                let limit = ev.runtime_collection_limit();
                if next > limit {
                    return Err(eval_err(format!(
                        "sequence_flatten: list size limit {limit} exceeded"
                    )));
                }
                ev.charge_allocation(inner.len())?;
                result.extend(inner);
            }
            Ok(RuntimeValue::List(Rc::new(RefCell::new(result))))
        },
    );

    // ── sequence-join ─────────────────────────────────────────────────────────
    ev.register_special(
        "sequence_join",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "string"], "string"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_join")?;
            let sep = require_str(&args[1], "sequence_join")?.to_string();
            let mut parts = Vec::with_capacity(seq.len());
            for item in &seq {
                match item {
                    RuntimeValue::Str(s) => parts.push(s.to_string()),
                    other => {
                        return Err(eval_err(format!(
                            "sequence_join: expected string, got {other}"
                        )))
                    }
                }
            }
            let joined = parts.join(&sep);
            ensure_string_char_limit("sequence_join", joined.chars().count(), ev)?;
            Ok(RuntimeValue::Str(joined.into()))
        },
    );

    // ── get ───────────────────────────────────────────────────────────────────
    ev.register_eager(
        "get",
        2,
        Some(3),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["any", "any", "any"], "any"),
        |args| {
            let default = args.get(2).cloned().unwrap_or(RuntimeValue::Null);
            polymorphic_get(&args[0], &args[1], default, false)
        },
    );

    // ── get-strict ────────────────────────────────────────────────────────────
    ev.register_eager(
        "get_strict",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["any", "any"], "any"),
        |args| polymorphic_get(&args[0], &args[1], RuntimeValue::Null, true),
    );

    // ── size ──────────────────────────────────────────────────────────────────
    ev.register_eager(
        "size",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["any"], "int"),
        |args| polymorphic_size(&args[0]),
    );

    // ── contains ──────────────────────────────────────────────────────────────
    ev.register_special(
        "contains",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["any", "any"], "bool"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            match &args[0] {
                RuntimeValue::List(l) => Ok(RuntimeValue::Bool(l.borrow().contains(&args[1]))),
                RuntimeValue::Tuple(items) => Ok(RuntimeValue::Bool(items.contains(&args[1]))),
                RuntimeValue::Map(m) => {
                    let key = MapKey::try_from(&args[1])?;
                    Ok(RuntimeValue::Bool(m.borrow().contains_key(&key)))
                }
                RuntimeValue::Str(s) => {
                    let sub = require_str(&args[1], "contains: string substring")?;
                    Ok(RuntimeValue::Bool(s.contains(sub.as_ref())))
                }
                RuntimeValue::HostObject(object) => {
                    if let Some(node) = object.as_any().downcast_ref::<NodeBridgeValue>() {
                        let key = require_str(
                            &args[1],
                            "contains expects a string key for IR node containers",
                        )?;
                        return Ok(RuntimeValue::Bool(node_field_value(node, key)?.is_some()));
                    }
                    Err(eval_err(format!("contains does not support {}", args[0])))
                }
                other => Err(eval_err(format!("contains does not support {other}"))),
            }
        },
    );

    // ── map-keys ──────────────────────────────────────────────────────────────
    ev.register_special(
        "map_keys",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["map"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let map = require_map(&args[0], "map_keys")?;
            let borrow = map.borrow();
            let keys: Vec<RuntimeValue> = ordered_runtime_map_entries(&borrow)
                .into_iter()
                .map(|(key, _)| key.clone().into())
                .collect();
            ensure_sequence_len("map_keys", keys.len(), ev)?;
            Ok(RuntimeValue::List(Rc::new(RefCell::new(keys))))
        },
    );

    // ── map-values ────────────────────────────────────────────────────────────
    ev.register_special(
        "map_values",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["map"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let map = require_map(&args[0], "map_values")?;
            let borrow = map.borrow();
            let vals: Vec<RuntimeValue> = ordered_runtime_map_entries(&borrow)
                .into_iter()
                .map(|(_, value)| value.clone())
                .collect();
            ensure_sequence_len("map_values", vals.len(), ev)?;
            Ok(RuntimeValue::List(Rc::new(RefCell::new(vals))))
        },
    );

    // ── map-merge ─────────────────────────────────────────────────────────────
    ev.register_special(
        "map_merge",
        0,
        None,
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["*map"], "map"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let mut result: IndexMap<MapKey, RuntimeValue> = IndexMap::new();
            for v in &args {
                let map = require_map(v, "map_merge")?;
                for (k, val) in map.borrow().iter() {
                    result.insert(k.clone(), val.clone());
                }
                ensure_map_len("map_merge", result.len(), ev)?;
            }
            Ok(RuntimeValue::Map(Rc::new(RefCell::new(result))))
        },
    );

    // ── sequence-sort-by ──────────────────────────────────────────────────────
    ev.register_special(
        "sequence_sort_by",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "callable"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_sort_by")?;
            let cb = args[1].clone();
            ensure_sequence_len("sequence_sort_by", seq.len(), ev)?;
            let mut keyed: Vec<(RuntimeValue, RuntimeValue)> = seq
                .into_iter()
                .map(|item| {
                    let key = ev.invoke_callback(&cb, vec![item.clone()])?;
                    Ok::<(RuntimeValue, RuntimeValue), crate::values::EvalSignal>((key, item))
                })
                .collect::<Result<Vec<_>, _>>()?;
            keyed.sort_by(|a, b| crate::builtins::reflect::runtime_value_cmp(&a.0, &b.0));
            Ok(RuntimeValue::List(Rc::new(RefCell::new(
                keyed.into_iter().map(|(_, v)| v).collect(),
            ))))
        },
    );

    // ── sequence-group-by ─────────────────────────────────────────────────────
    ev.register_special(
        "sequence_group_by",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "callable"], "map"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_group_by")?;
            let cb = args[1].clone();
            let mut groups: HashMap<MapKey, Vec<RuntimeValue>> = HashMap::new();
            let mut key_order: Vec<MapKey> = Vec::new();
            for item in seq {
                let key_val = ev.invoke_callback(&cb, vec![item.clone()])?;
                let key = MapKey::try_from(&key_val)?;
                match groups.entry(key) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        key_order.push(e.key().clone());
                        e.insert(vec![item]);
                    }
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        e.get_mut().push(item);
                    }
                }
            }
            ensure_map_len("sequence_group_by", key_order.len(), ev)?;
            let mut result: IndexMap<MapKey, RuntimeValue> = IndexMap::new();
            for key in key_order {
                let list = groups.remove(&key).unwrap_or_default();
                ensure_sequence_len("sequence_group_by", list.len(), ev)?;
                result.insert(key, RuntimeValue::List(Rc::new(RefCell::new(list))));
            }
            Ok(RuntimeValue::Map(Rc::new(RefCell::new(result))))
        },
    );

    // ── sequence-zip ──────────────────────────────────────────────────────────
    ev.register_special(
        "sequence_zip",
        1,
        None,
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["*list"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seqs: Vec<Vec<RuntimeValue>> = args
                .iter()
                .map(|a| require_seq(a, "sequence_zip"))
                .collect::<Result<_, _>>()?;
            if seqs.is_empty() {
                return Ok(RuntimeValue::List(Rc::new(RefCell::new(vec![]))));
            }
            let min_len = seqs.iter().map(|s| s.len()).min().unwrap_or(0);
            ensure_sequence_len("sequence_zip", min_len, ev)?;
            ensure_sequence_len("sequence_zip", seqs.len(), ev)?;
            let result = (0..min_len)
                .map(|i| {
                    let pair: Vec<RuntimeValue> = seqs.iter().map(|s| s[i].clone()).collect();
                    RuntimeValue::List(Rc::new(RefCell::new(pair)))
                })
                .collect();
            Ok(RuntimeValue::List(Rc::new(RefCell::new(result))))
        },
    );

    // ── sequence-unique-by ────────────────────────────────────────────────────
    ev.register_special(
        "sequence_unique_by",
        1,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["list", "callable"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_unique_by")?;
            let has_key_fn = args.len() == 2;
            // Use HashSet<MapKey> for hashable keys (Null/Bool/Int/Str) — O(1)
            // per lookup. Fall back to Vec linear scan only for unhashable values
            // (Float, List, Map, Closure…) which are rare as dedup keys.
            let mut seen_hashable: HashSet<crate::values::MapKey> = HashSet::new();
            let mut seen_unhashable: Vec<RuntimeValue> = Vec::new();
            let mut result = Vec::new();
            for item in seq {
                let key = if has_key_fn {
                    ev.invoke_callback(&args[1], vec![item.clone()])?
                } else {
                    item.clone()
                };
                let is_new = match crate::values::MapKey::try_from(&key) {
                    Ok(mk) => seen_hashable.insert(mk),
                    Err(_) => {
                        if seen_unhashable.contains(&key) {
                            false
                        } else {
                            seen_unhashable.push(key);
                            true
                        }
                    }
                };
                if is_new {
                    result.push(item);
                }
            }
            ensure_sequence_len("sequence_unique_by", result.len(), ev)?;
            Ok(RuntimeValue::List(Rc::new(RefCell::new(result))))
        },
    );

    // ── sequence-each-pair ────────────────────────────────────────────────────
    ev.register_special(
        "sequence_each_pair",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["map", "callable"], "null"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence_each_pair")?;
            let cb = args[1].clone();
            if seq.len() % 2 != 0 {
                return Err(eval_err(
                    "sequence-each-pair expects an even-length sequence",
                ));
            }
            let mut i = 0;
            while i < seq.len() {
                ev.invoke_callback(&cb, vec![seq[i].clone(), seq[i + 1].clone()])?;
                i += 2;
            }
            Ok(RuntimeValue::Null)
        },
    );

    // ── map-of-entries ────────────────────────────────────────────────────────
    ev.register_special(
        "map_of_entries",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["list"], "map"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let entries = require_seq(&args[0], "map_of_entries")?;
            let mut result: IndexMap<MapKey, RuntimeValue> = IndexMap::new();
            for entry in entries {
                let pair = require_seq(&entry, "map_of_entries: each entry")?;
                if pair.len() != 2 {
                    return Err(eval_err("map-of-entries expects entry pairs of length 2"));
                }
                let key = MapKey::try_from(&pair[0])?;
                result.insert(key, pair[1].clone());
                ensure_map_len("map_of_entries", result.len(), ev)?;
            }
            Ok(RuntimeValue::Map(Rc::new(RefCell::new(result))))
        },
    );

    // ── map-update ────────────────────────────────────────────────────────────
    ev.register_special(
        "map_update",
        3,
        Some(4),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["map", "any", "callable", "any"], "map"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let map = crate::values::require_map(&args[0], "map_update")?;
            let key = MapKey::try_from(&args[1])?;
            let cb = args[2].clone();
            let default = args.get(3).cloned().unwrap_or(RuntimeValue::Null);
            let current = map.borrow().get(&key).cloned().unwrap_or(default);
            let new_val = ev.invoke_callback(&cb, vec![current])?;
            map.borrow_mut().insert(key, new_val);
            Ok(RuntimeValue::Map(map))
        },
    );
}

// ── polymorphic get helper ────────────────────────────────────────────────────

fn polymorphic_get(
    container: &RuntimeValue,
    key: &RuntimeValue,
    default: RuntimeValue,
    strict: bool,
) -> Result<RuntimeValue, crate::values::EvalSignal> {
    match container {
        RuntimeValue::Map(m) => {
            let k = MapKey::try_from(key)?;
            match m.borrow().get(&k) {
                Some(v) => Ok(v.clone()),
                None => {
                    if strict {
                        Err(eval_err("get_strict: key is missing"))
                    } else {
                        Ok(default)
                    }
                }
            }
        }
        RuntimeValue::List(l) => {
            let idx = require_index_key(key, strict)?;
            let borrow = l.borrow();
            match borrow.get(idx) {
                Some(v) => Ok(v.clone()),
                None => {
                    if strict {
                        Err(eval_err("get_strict: index out of range"))
                    } else {
                        Ok(default)
                    }
                }
            }
        }
        RuntimeValue::Tuple(items) => {
            let idx = require_index_key(key, strict)?;
            match items.get(idx) {
                Some(v) => Ok(v.clone()),
                None => {
                    if strict {
                        Err(eval_err("get_strict: tuple index out of range"))
                    } else {
                        Ok(default)
                    }
                }
            }
        }
        RuntimeValue::Str(s) => {
            let idx = require_index_key(key, strict)?;
            match s.chars().nth(idx) {
                Some(c) => Ok(RuntimeValue::Str(c.to_string().into())),
                None => {
                    if strict {
                        Err(eval_err("get_strict: string index out of range"))
                    } else {
                        Ok(default)
                    }
                }
            }
        }
        RuntimeValue::HostObject(object) => {
            if let Some(node) = object.as_any().downcast_ref::<NodeBridgeValue>() {
                let key = require_str(key, "get expects a string key for IR node containers")?;
                match node_field_value(node, key)? {
                    Some(value) => Ok(value),
                    None => {
                        if strict {
                            Err(eval_err("get_strict: key is missing"))
                        } else {
                            Ok(default)
                        }
                    }
                }
            } else {
                Err(eval_err(format!("get does not support {container}")))
            }
        }
        other => Err(eval_err(format!("get does not support {other}"))),
    }
}

fn require_index_key(key: &RuntimeValue, strict: bool) -> Result<usize, crate::values::EvalSignal> {
    let ctx = if strict { "get_strict" } else { "get" };
    let index = require_int_strict(key, ctx)?;
    if index < 0 {
        return Err(eval_err(format!("{ctx}: index must be non-negative")));
    }
    usize::try_from(index).map_err(|_| eval_err(format!("{ctx}: index is too large")))
}

fn require_non_negative_count(
    value: &RuntimeValue,
    ctx: &str,
) -> Result<usize, crate::values::EvalSignal> {
    let count = require_int_strict(value, ctx)?;
    if count < 0 {
        return Err(eval_err(format!("{ctx} expects a non-negative count")));
    }
    usize::try_from(count).map_err(|_| eval_err(format!("{ctx}: count is too large")))
}

fn polymorphic_size(value: &RuntimeValue) -> Result<RuntimeValue, crate::values::EvalSignal> {
    match value {
        RuntimeValue::List(l) => Ok(RuntimeValue::Int(l.borrow().len() as i64)),
        RuntimeValue::Tuple(items) => Ok(RuntimeValue::Int(items.len() as i64)),
        RuntimeValue::Map(m) => Ok(RuntimeValue::Int(m.borrow().len() as i64)),
        RuntimeValue::Str(s) => Ok(RuntimeValue::Int(s.chars().count() as i64)),
        RuntimeValue::HostObject(object) => {
            if let Some(node) = object.as_any().downcast_ref::<NodeBridgeValue>() {
                let unit = node_unit(node, "size")?;
                return unit.with_unit(|unit| {
                    unit.ir()
                        .node(node.node_id())
                        .map(|node| RuntimeValue::Int(node.children().len() as i64))
                        .ok_or_else(|| eval_err(format!("unknown node id: {}", node.node_id())))
                });
            }
            Err(eval_err(format!("size does not support {value}")))
        }
        other => Err(eval_err(format!("size does not support {other}"))),
    }
}

fn node_unit<'a>(
    node: &'a NodeBridgeValue,
    prefix: &str,
) -> Result<&'a UnitBridgeValue, crate::values::EvalSignal> {
    node.unit
        .as_any()
        .downcast_ref::<UnitBridgeValue>()
        .ok_or_else(|| eval_err(format!("{prefix} expects a live node")))
}

fn node_handle(
    unit: Rc<dyn crate::values::HostObject>,
    node_id: crate::ir::NodeId,
) -> RuntimeValue {
    RuntimeValue::HostObject(Rc::new(NodeBridgeValue::new(unit, node_id)))
}

fn node_field_value(
    node: &NodeBridgeValue,
    key: &str,
) -> Result<Option<RuntimeValue>, crate::values::EvalSignal> {
    let unit_bridge = node_unit(node, "get")?;
    unit_bridge.with_unit(|unit| {
        let active = unit
            .ir()
            .node(node.node_id())
            .ok_or_else(|| eval_err(format!("unknown node id: {}", node.node_id())))?;
        let unit_object = Rc::clone(&node.unit);
        let value = match key {
            "id" => Some(RuntimeValue::Int(node.node_id() as i64)),
            "stable_id" => Some(RuntimeValue::Str(
                unit.node_stable_id(node.node_id())
                    .map_err(eval_err)?
                    .as_str()
                    .to_string()
                    .into(),
            )),
            "kind" => Some(RuntimeValue::Str(
                match active {
                    Node::Name(_) => "Name",
                    Node::Literal(_) => "Literal",
                    Node::Call(_) => "Call",
                }
                .into(),
            )),
            "parent" => Some(
                unit.ir()
                    .parent(node.node_id())
                    .flatten()
                    .map(|parent_id| node_handle(Rc::clone(&unit_object), parent_id))
                    .unwrap_or(RuntimeValue::Null),
            ),
            "children" => Some(RuntimeValue::Tuple(
                active
                    .children()
                    .into_iter()
                    .map(|child_id| node_handle(Rc::clone(&unit_object), child_id))
                    .collect::<Vec<_>>()
                    .into(),
            )),
            "identifier" => match active {
                Node::Name(name) => Some(RuntimeValue::Str(name.identifier.clone())),
                _ => None,
            },
            "value" => match active {
                Node::Literal(literal) => {
                    Some(crate::values::runtime_value_from_literal(&literal.value))
                }
                _ => None,
            },
            "callee" => match active {
                Node::Call(call) => Some(node_handle(unit_object, call.callee)),
                _ => None,
            },
            "args" => match active {
                Node::Call(call) => Some(RuntimeValue::Tuple(
                    call.args
                        .iter()
                        .map(|arg_id| node_handle(Rc::clone(&unit_object), *arg_id))
                        .collect::<Vec<_>>()
                        .into(),
                )),
                _ => None,
            },
            _ => None,
        };
        Ok(value)
    })
}

fn range_len(start: i64, end: i64) -> Result<usize, crate::values::EvalSignal> {
    let len = i128::from(end) - i128::from(start);
    usize::try_from(len).map_err(|_| eval_err("sequence_range: list size overflow"))
}

fn ensure_sequence_len(
    context: &str,
    len: usize,
    ev: &Evaluator,
) -> Result<(), crate::values::EvalSignal> {
    let limit = ev.runtime_collection_limit();
    if len > limit {
        return Err(eval_err(format!(
            "{context}: list size limit {limit} exceeded"
        )));
    }
    // Charge produced elements against the active allocation budget (no-op
    // outside a sandbox) so a loop of bounded sequence ops cannot OOM the host.
    ev.charge_allocation(len)
}

fn ensure_map_len(
    context: &str,
    len: usize,
    ev: &Evaluator,
) -> Result<(), crate::values::EvalSignal> {
    let limit = ev.runtime_collection_limit();
    if len > limit {
        return Err(eval_err(format!(
            "{context}: map size limit {limit} exceeded"
        )));
    }
    ev.charge_allocation(len)
}

use super::args::{ensure_string_char_limit, normalize_slice_index, optional_slice_end};
