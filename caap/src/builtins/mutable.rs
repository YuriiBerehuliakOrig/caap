/// Mutable collection builtins — port of `caap/builtins/lang/mutable.py`.
///
/// Covers: list-of, map-of, append, append-many, assoc, assoc-many, set.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::eval::{eval_args, Evaluator};
use crate::values::{
    eval_err, require_int_strict, require_list, require_map, BuiltinInfo, MapKey, RuntimeValue,
};

pub fn register(ev: &mut Evaluator) {
    // ── list-of ───────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "list-of".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 0,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::List(Rc::new(RefCell::new(args))))
        }),
    });

    // ── map-of ────────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "map-of".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 0,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if args.len() % 2 != 0 {
                return Err(eval_err(
                    "map-of expects an even number of arguments (key value pairs)",
                ));
            }
            let mut map = HashMap::new();
            for chunk in args.chunks(2) {
                let key = MapKey::try_from(&chunk[0])?;
                map.insert(key, chunk[1].clone());
            }
            Ok(RuntimeValue::Map(Rc::new(RefCell::new(map))))
        }),
    });

    // ── append ────────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "append".to_string(),
        metadata: crate::values::BuiltinMetadata::runtime_mutation(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let list = require_list(&args[0], "append")?;
            list.borrow_mut().push(args[1].clone());
            Ok(RuntimeValue::List(list))
        }),
    });

    // ── append-many ───────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "append-many".to_string(),
        metadata: crate::values::BuiltinMetadata::runtime_mutation(),
        min_arity: 2,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let list = require_list(&args[0], "append-many")?;
            let mut borrow = list.borrow_mut();
            for v in &args[1..] {
                borrow.push(v.clone());
            }
            drop(borrow);
            Ok(RuntimeValue::List(list))
        }),
    });

    // ── assoc ─────────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "assoc".to_string(),
        metadata: crate::values::BuiltinMetadata::runtime_mutation(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let map = require_map(&args[0], "assoc")?;
            let key = MapKey::try_from(&args[1])?;
            map.borrow_mut().insert(key, args[2].clone());
            Ok(RuntimeValue::Map(map))
        }),
    });

    // ── assoc-many ────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "assoc-many".to_string(),
        metadata: crate::values::BuiltinMetadata::runtime_mutation(),
        min_arity: 3,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let map = require_map(&args[0], "assoc-many")?;
            let pairs = &args[1..];
            if pairs.len() % 2 != 0 {
                return Err(eval_err("assoc-many expects key/value pairs after target"));
            }
            let mut borrow = map.borrow_mut();
            for chunk in pairs.chunks(2) {
                let key = MapKey::try_from(&chunk[0])?;
                borrow.insert(key, chunk[1].clone());
            }
            drop(borrow);
            Ok(RuntimeValue::Map(map))
        }),
    });

    // ── set ───────────────────────────────────────────────────────────────────
    // set(container, key_or_index, value) — mutate in place, return container.
    ev.register_builtin(BuiltinInfo {
        name: "set".to_string(),
        metadata: crate::values::BuiltinMetadata::runtime_mutation(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            match &args[0] {
                RuntimeValue::Map(m) => {
                    let key = MapKey::try_from(&args[1])?;
                    m.borrow_mut().insert(key, args[2].clone());
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
        }),
    });
}
