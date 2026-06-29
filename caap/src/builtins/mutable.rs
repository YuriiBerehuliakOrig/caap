/// Mutable collection builtins — port of `caap/builtins/lang/mutable.py`.
///
/// Covers: list-of, map-of, append, assoc, set, map-delete, list-remove-at.
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

use crate::eval::{eval_args, Evaluator};
use crate::values::{
    eval_err, require_int_strict, require_list, require_map, require_ref, EvalSignal, MapKey,
    RuntimeValue,
};

pub fn register(ev: &mut Evaluator) {
    // ── list-of ───────────────────────────────────────────────────────────────
    ev.register_special(
        "list_of",
        0,
        None,
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["*any"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            ensure_collection_len("list", 0, args.len(), ev, "list_of")?;
            Ok(RuntimeValue::List(Rc::new(RefCell::new(args))))
        },
    );

    // ── map-of ────────────────────────────────────────────────────────────────
    ev.register_special(
        "map_of",
        0,
        None,
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["*any"], "map"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if args.len() % 2 != 0 {
                return Err(eval_err(
                    "map_of expects an even number of arguments (key value pairs)",
                ));
            }
            let mut map = IndexMap::new();
            for chunk in args.chunks(2) {
                let key = MapKey::try_from(&chunk[0])?;
                map.insert(key, chunk[1].clone());
            }
            ensure_collection_len("map", 0, map.len(), ev, "map_of")?;
            Ok(RuntimeValue::Map(Rc::new(RefCell::new(map))))
        },
    );

    // ── append ────────────────────────────────────────────────────────────────
    // (append list item ...) — appends one or more items to list in place.
    ev.register_special(
        "append",
        2,
        None,
        crate::values::BuiltinMetadata::runtime_mutation()
            .with_signature(&["list", "*any"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let list = require_list(&args[0], "append")?;
            let mut borrow = list.borrow_mut();
            ensure_collection_len("list", borrow.len(), args.len() - 1, ev, "append")?;
            for v in &args[1..] {
                borrow.push(v.clone());
            }
            drop(borrow);
            Ok(RuntimeValue::List(list))
        },
    );

    // ── assoc ─────────────────────────────────────────────────────────────────
    // (assoc map key val [key val] ...) — sets one or more key/value pairs in place.
    ev.register_special(
        "assoc",
        3,
        None,
        crate::values::BuiltinMetadata::runtime_mutation().with_signature(&["map", "*any"], "map"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let map = require_map(&args[0], "assoc")?;
            let pairs = &args[1..];
            if pairs.len() % 2 != 0 {
                return Err(eval_err("assoc expects key/value pairs after target"));
            }
            let mut borrow = map.borrow_mut();
            let mut new_keys = Vec::new();
            for chunk in pairs.chunks(2) {
                let key = MapKey::try_from(&chunk[0])?;
                if !borrow.contains_key(&key) && !new_keys.contains(&key) {
                    new_keys.push(key);
                }
            }
            ensure_collection_len("map", borrow.len(), new_keys.len(), ev, "assoc")?;
            for chunk in pairs.chunks(2) {
                let key = MapKey::try_from(&chunk[0])?;
                borrow.insert(key, chunk[1].clone());
            }
            drop(borrow);
            Ok(RuntimeValue::Map(map))
        },
    );

    // ── set ───────────────────────────────────────────────────────────────────
    // set(container, key_or_index, value) — mutate in place, return container.
    ev.register_special(
        "set",
        3,
        Some(3),
        crate::values::BuiltinMetadata::runtime_mutation()
            .with_signature(&["any", "any", "any"], "any"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            match &args[0] {
                RuntimeValue::Map(m) => {
                    let key = MapKey::try_from(&args[1])?;
                    let mut borrow = m.borrow_mut();
                    if !borrow.contains_key(&key) {
                        ensure_collection_len("map", borrow.len(), 1, ev, "set")?;
                    }
                    borrow.insert(key, args[2].clone());
                    drop(borrow);
                    Ok(RuntimeValue::Map(Rc::clone(m)))
                }
                RuntimeValue::List(l) => {
                    let idx = require_int_strict(&args[1], "set")?;
                    let mut borrow = l.borrow_mut();
                    let len = borrow.len() as i64;
                    if idx < 0 || idx >= len {
                        return Err(eval_err("set: index out of range"));
                    }
                    borrow[idx as usize] = args[2].clone();
                    drop(borrow);
                    Ok(RuntimeValue::List(Rc::clone(l)))
                }
                other => Err(eval_err(format!("set does not support {other} containers"))),
            }
        },
    );

    // ── map-delete ────────────────────────────────────────────────────────────
    // (map-delete map key) — removes key from map in place, returns the map.
    ev.register_special(
        "map_delete",
        2,
        Some(2),
        crate::values::BuiltinMetadata::runtime_mutation().with_signature(&["map", "any"], "map"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let map = require_map(&args[0], "map_delete")?;
            let key = MapKey::try_from(&args[1])?;
            // shift_remove: preserves the remaining entries' insertion order (the
            // plain remove is IndexMap's swap_remove, which would scramble it).
            map.borrow_mut().shift_remove(&key);
            Ok(RuntimeValue::Map(map))
        },
    );

    // ── list-remove-at ────────────────────────────────────────────────────────
    // (list-remove-at list index) — removes element at index in place, returns the list.
    ev.register_special(
        "list_remove_at",
        2,
        Some(2),
        crate::values::BuiltinMetadata::runtime_mutation().with_signature(&["list", "int"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let list = require_list(&args[0], "list_remove_at")?;
            let idx = require_int_strict(&args[1], "list_remove_at")?;
            let mut borrow = list.borrow_mut();
            let len = borrow.len() as i64;
            if idx < 0 || idx >= len {
                return Err(eval_err(format!(
                    "list_remove_at: index {idx} out of range [0, {len})"
                )));
            }
            borrow.remove(idx as usize);
            drop(borrow);
            Ok(RuntimeValue::List(list))
        },
    );

    // ── ref / deref / set_ref ───────────────────────────────────────────────
    // First-class mutable reference cells (`RuntimeValue::Ref`): `ref` boxes a
    // value, `deref` reads it, `set_ref` writes it in place. Two refs to the same
    // cell alias; see `RtRef`.
    ev.register_special(
        "ref",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["any"], "ref"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Ref(Rc::new(RefCell::new(args[0].clone()))))
        },
    );
    ev.register_special(
        "deref",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["ref"], "any"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let cell = require_ref(&args[0], "deref")?;
            let value = cell.borrow().clone();
            Ok(value)
        },
    );
    ev.register_special(
        "set_ref",
        2,
        Some(2),
        crate::values::BuiltinMetadata::runtime_mutation().with_signature(&["ref", "any"], "any"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let cell = require_ref(&args[0], "set_ref")?;
            *cell.borrow_mut() = args[1].clone();
            Ok(args[1].clone())
        },
    );
}

fn ensure_collection_len(
    kind: &str,
    current_len: usize,
    additional_len: usize,
    ev: &Evaluator,
    context: &str,
) -> Result<(), EvalSignal> {
    let limit = ev.runtime_collection_limit();
    let Some(next_len) = current_len.checked_add(additional_len) else {
        return Err(eval_err(format!("{context}: {kind} size overflow")));
    };
    if next_len > limit {
        return Err(eval_err(format!(
            "{context}: {kind} size limit {limit} exceeded"
        )));
    }
    // Charge the newly added elements against the active allocation budget
    // (no-op outside a sandbox) so a loop building many bounded collections
    // cannot OOM the host.
    ev.charge_allocation(additional_len)
}
