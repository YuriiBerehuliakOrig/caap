use std::cell::RefCell;
/// Sequence + polymorphic container builtins.
///
/// Covers: sequence-range, sequence-each, sequence-map, sequence-filter,
/// sequence-fold-left, sequence-any, sequence-all, sequence-find,
/// sequence-find-reverse, sequence-count, sequence-reverse, sequence-slice,
/// sequence-take, sequence-drop, sequence-zip, sequence-flatten,
/// sequence-join, sequence-index-of, sequence-distinct,
/// get, get!, size, contains, map-keys, map-values, map-merge,
/// for-range.
use std::collections::HashMap;
use std::rc::Rc;

use crate::bridges::NodeBridgeValue;
use crate::compiler::UnitBridgeValue;
use crate::eval::{eval_args, Evaluator};
use crate::ir::Node;
use crate::values::{
    eval_err, is_truthy, require_int_strict, require_map, require_str, BuiltinInfo, MapKey,
    RuntimeValue,
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
    ev.register_builtin(BuiltinInfo {
        name: "sequence-range".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let start = require_int_strict(&args[0], "sequence-range")?;
            let end = require_int_strict(&args[1], "sequence-range")?;
            if end <= start {
                return Ok(RuntimeValue::List(Rc::new(RefCell::new(vec![]))));
            }
            let list: Vec<RuntimeValue> = (start..end).map(RuntimeValue::Int).collect();
            Ok(RuntimeValue::List(Rc::new(RefCell::new(list))))
        }),
    });

    // ── sequence-each ─────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-each".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-each")?;
            let cb = args[1].clone();
            for item in seq {
                ev.invoke_callback(&cb, vec![item])?;
            }
            Ok(RuntimeValue::Null)
        }),
    });

    // ── sequence-each-indexed ─────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-each-indexed".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-each-indexed")?;
            let cb = args[1].clone();
            for (i, item) in seq.into_iter().enumerate() {
                ev.invoke_callback(&cb, vec![item, RuntimeValue::Int(i as i64)])?;
            }
            Ok(RuntimeValue::Null)
        }),
    });

    // ── sequence-map ──────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-map".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-map")?;
            let cb = args[1].clone();
            let mut result = Vec::with_capacity(seq.len());
            for item in seq {
                result.push(ev.invoke_callback(&cb, vec![item])?);
            }
            Ok(RuntimeValue::List(Rc::new(RefCell::new(result))))
        }),
    });

    // ── sequence-filter ───────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-filter".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-filter")?;
            let cb = args[1].clone();
            let mut result = Vec::new();
            for item in seq {
                let keep = ev.invoke_callback(&cb, vec![item.clone()])?;
                if is_truthy(&keep) {
                    result.push(item);
                }
            }
            Ok(RuntimeValue::List(Rc::new(RefCell::new(result))))
        }),
    });

    // ── sequence-fold-left ────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-fold-left".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-fold-left")?;
            let mut acc = args[1].clone();
            let cb = args[2].clone();
            for item in seq {
                acc = ev.invoke_callback(&cb, vec![acc, item])?;
            }
            Ok(acc)
        }),
    });

    // ── sequence-find ─────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-find".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-find")?;
            let cb = args[1].clone();
            for item in seq {
                let matched = ev.invoke_callback(&cb, vec![item.clone()])?;
                if is_truthy(&matched) {
                    return Ok(item);
                }
            }
            Ok(RuntimeValue::Null)
        }),
    });

    // ── sequence-find-reverse ─────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-find-reverse".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-find-reverse")?;
            let cb = args[1].clone();
            for item in seq.into_iter().rev() {
                let matched = ev.invoke_callback(&cb, vec![item.clone()])?;
                if is_truthy(&matched) {
                    return Ok(item);
                }
            }
            Ok(RuntimeValue::Null)
        }),
    });

    // ── sequence-any ─────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-any".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-any")?;
            let cb = args[1].clone();
            for item in seq {
                if is_truthy(&ev.invoke_callback(&cb, vec![item])?) {
                    return Ok(RuntimeValue::Bool(true));
                }
            }
            Ok(RuntimeValue::Bool(false))
        }),
    });

    // ── sequence-all ─────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-all".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-all")?;
            let cb = args[1].clone();
            for item in seq {
                if !is_truthy(&ev.invoke_callback(&cb, vec![item])?) {
                    return Ok(RuntimeValue::Bool(false));
                }
            }
            Ok(RuntimeValue::Bool(true))
        }),
    });

    // ── sequence-count ────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-count".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-count")?;
            let cb = args[1].clone();
            let mut count: i64 = 0;
            for item in seq {
                if is_truthy(&ev.invoke_callback(&cb, vec![item])?) {
                    count += 1;
                }
            }
            Ok(RuntimeValue::Int(count))
        }),
    });

    // ── sequence-index-of ─────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-index-of".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-index-of")?;
            let cb = args[1].clone();
            for (i, item) in seq.into_iter().enumerate() {
                if is_truthy(&ev.invoke_callback(&cb, vec![item])?) {
                    return Ok(RuntimeValue::Int(i as i64));
                }
            }
            Ok(RuntimeValue::Int(-1))
        }),
    });

    // ── sequence-reverse ──────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-reverse".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let mut seq = require_seq(&args[0], "sequence-reverse")?;
            seq.reverse();
            Ok(RuntimeValue::List(Rc::new(RefCell::new(seq))))
        }),
    });

    // ── sequence-slice ────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-slice".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-slice")?;
            let start =
                normalize_slice_index(require_int_strict(&args[1], "sequence-slice")?, seq.len());
            let end = optional_slice_end(args.get(2), seq.len(), "sequence-slice")?;
            let sliced = if end <= start {
                Vec::new()
            } else {
                seq[start..end].to_vec()
            };
            Ok(RuntimeValue::List(Rc::new(RefCell::new(sliced))))
        }),
    });

    // ── sequence-take ─────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-take".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-take")?;
            let n = require_int_strict(&args[1], "sequence-take")?;
            if n < 0 {
                return Err(eval_err("sequence-take expects a non-negative count"));
            }
            let taken = seq.into_iter().take(n as usize).collect();
            Ok(RuntimeValue::List(Rc::new(RefCell::new(taken))))
        }),
    });

    // ── sequence-drop ─────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-drop".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-drop")?;
            let n = require_int_strict(&args[1], "sequence-drop")?;
            if n < 0 {
                return Err(eval_err("sequence-drop expects a non-negative count"));
            }
            let dropped = seq.into_iter().skip(n as usize).collect();
            Ok(RuntimeValue::List(Rc::new(RefCell::new(dropped))))
        }),
    });

    // ── sequence-flatten ──────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-flatten".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-flatten")?;
            let mut result = Vec::new();
            for item in seq {
                let inner = require_seq(&item, "sequence-flatten: inner item")?;
                result.extend(inner);
            }
            Ok(RuntimeValue::List(Rc::new(RefCell::new(result))))
        }),
    });

    // ── sequence-join ─────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-join".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-join")?;
            let sep = require_str(&args[1], "sequence-join")?.to_string();
            let mut parts = Vec::with_capacity(seq.len());
            for item in &seq {
                match item {
                    RuntimeValue::Str(s) => parts.push(s.to_string()),
                    other => {
                        return Err(eval_err(format!(
                            "sequence-join: expected string, got {other}"
                        )))
                    }
                }
            }
            Ok(RuntimeValue::Str(parts.join(&sep).into()))
        }),
    });

    // ── sequence-distinct ─────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-distinct".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-distinct")?;
            let mut seen: Vec<RuntimeValue> = Vec::new();
            let mut result = Vec::new();
            for item in seq {
                if !seen.contains(&item) {
                    seen.push(item.clone());
                    result.push(item);
                }
            }
            Ok(RuntimeValue::List(Rc::new(RefCell::new(result))))
        }),
    });

    // ── for-range ─────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "for-range".to_string(),
        metadata: crate::values::BuiltinMetadata::runtime_sequential(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let start = require_int_strict(&args[0], "for-range")?;
            let end = require_int_strict(&args[1], "for-range")?;
            let cb = args[2].clone();
            for i in start..end {
                ev.invoke_callback(&cb, vec![RuntimeValue::Int(i)])?;
            }
            Ok(RuntimeValue::Null)
        }),
    });

    // ── get ───────────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "get".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let default = args.get(2).cloned().unwrap_or(RuntimeValue::Null);
            polymorphic_get(&args[0], &args[1], default, false)
        }),
    });

    // ── get! ──────────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "get!".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            polymorphic_get(&args[0], &args[1], RuntimeValue::Null, true)
        }),
    });

    // ── size ──────────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "size".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            match &args[0] {
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
                                .ok_or_else(|| {
                                    eval_err(format!("unknown node id: {}", node.node_id()))
                                })
                        });
                    }
                    Err(eval_err(format!("size does not support {}", args[0])))
                }
                other => Err(eval_err(format!("size does not support {other}"))),
            }
        }),
    });

    // ── contains ──────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "contains".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    // ── map-keys ──────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "map-keys".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let map = require_map(&args[0], "map-keys")?;
            let keys: Vec<RuntimeValue> = map.borrow().keys().map(|k| k.clone().into()).collect();
            Ok(RuntimeValue::List(Rc::new(RefCell::new(keys))))
        }),
    });

    // ── map-values ────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "map-values".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let map = require_map(&args[0], "map-values")?;
            let vals: Vec<RuntimeValue> = map.borrow().values().cloned().collect();
            Ok(RuntimeValue::List(Rc::new(RefCell::new(vals))))
        }),
    });

    // ── map-merge ─────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "map-merge".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 0,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let mut result: HashMap<MapKey, RuntimeValue> = HashMap::new();
            for v in &args {
                let map = require_map(v, "map-merge")?;
                for (k, val) in map.borrow().iter() {
                    result.insert(k.clone(), val.clone());
                }
            }
            Ok(RuntimeValue::Map(Rc::new(RefCell::new(result))))
        }),
    });

    // ── sequence-sort-by ──────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-sort-by".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-sort-by")?;
            let cb = args[1].clone();
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
        }),
    });

    // ── sequence-sort-by-desc ─────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-sort-by-desc".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-sort-by-desc")?;
            let cb = args[1].clone();
            let mut keyed: Vec<(RuntimeValue, RuntimeValue)> = seq
                .into_iter()
                .map(|item| {
                    let key = ev.invoke_callback(&cb, vec![item.clone()])?;
                    Ok::<(RuntimeValue, RuntimeValue), crate::values::EvalSignal>((key, item))
                })
                .collect::<Result<Vec<_>, _>>()?;
            keyed.sort_by(|a, b| crate::builtins::reflect::runtime_value_cmp(&b.0, &a.0));
            Ok(RuntimeValue::List(Rc::new(RefCell::new(
                keyed.into_iter().map(|(_, v)| v).collect(),
            ))))
        }),
    });

    // ── sequence-group-by ─────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-group-by".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-group-by")?;
            let cb = args[1].clone();
            let mut groups: HashMap<MapKey, Vec<RuntimeValue>> = HashMap::new();
            let mut key_order: Vec<MapKey> = Vec::new();
            for item in seq {
                let key_val = ev.invoke_callback(&cb, vec![item.clone()])?;
                let key = MapKey::try_from(&key_val)?;
                if !groups.contains_key(&key) {
                    key_order.push(key.clone());
                }
                groups.entry(key).or_default().push(item);
            }
            let mut result: HashMap<MapKey, RuntimeValue> = HashMap::new();
            for key in key_order {
                let list = groups.remove(&key).unwrap_or_default();
                result.insert(key, RuntimeValue::List(Rc::new(RefCell::new(list))));
            }
            Ok(RuntimeValue::Map(Rc::new(RefCell::new(result))))
        }),
    });

    // ── sequence-zip ──────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-zip".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seqs: Vec<Vec<RuntimeValue>> = args
                .iter()
                .map(|a| require_seq(a, "sequence-zip"))
                .collect::<Result<_, _>>()?;
            if seqs.is_empty() {
                return Ok(RuntimeValue::List(Rc::new(RefCell::new(vec![]))));
            }
            let min_len = seqs.iter().map(|s| s.len()).min().unwrap_or(0);
            let result = (0..min_len)
                .map(|i| {
                    let pair: Vec<RuntimeValue> = seqs.iter().map(|s| s[i].clone()).collect();
                    RuntimeValue::List(Rc::new(RefCell::new(pair)))
                })
                .collect();
            Ok(RuntimeValue::List(Rc::new(RefCell::new(result))))
        }),
    });

    // ── sequence-unique-by ────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-unique-by".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-unique-by")?;
            let cb = args[1].clone();
            let mut seen: Vec<RuntimeValue> = Vec::new();
            let mut result = Vec::new();
            for item in seq {
                let key = ev.invoke_callback(&cb, vec![item.clone()])?;
                if !seen.contains(&key) {
                    seen.push(key);
                    result.push(item);
                }
            }
            Ok(RuntimeValue::List(Rc::new(RefCell::new(result))))
        }),
    });

    // ── sequence-each-pair ────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "sequence-each-pair".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let seq = require_seq(&args[0], "sequence-each-pair")?;
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
        }),
    });

    // ── map-of-entries ────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "map-of-entries".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let entries = require_seq(&args[0], "map-of-entries")?;
            let mut result: HashMap<MapKey, RuntimeValue> = HashMap::new();
            for entry in entries {
                let pair = require_seq(&entry, "map-of-entries: each entry")?;
                if pair.len() != 2 {
                    return Err(eval_err("map-of-entries expects entry pairs of length 2"));
                }
                let key = MapKey::try_from(&pair[0])?;
                result.insert(key, pair[1].clone());
            }
            Ok(RuntimeValue::Map(Rc::new(RefCell::new(result))))
        }),
    });

    // ── map-update ────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "map-update".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 3,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let map = crate::values::require_map(&args[0], "map-update")?;
            let key = MapKey::try_from(&args[1])?;
            let cb = args[2].clone();
            let default = args.get(3).cloned().unwrap_or(RuntimeValue::Null);
            let current = map.borrow().get(&key).cloned().unwrap_or(default);
            let new_val = ev.invoke_callback(&cb, vec![current])?;
            map.borrow_mut().insert(key, new_val);
            Ok(RuntimeValue::Map(map))
        }),
    });
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
                        Err(eval_err("get!: key is missing"))
                    } else {
                        Ok(default)
                    }
                }
            }
        }
        RuntimeValue::List(l) => {
            let idx = require_int_strict(key, "get")? as usize;
            let borrow = l.borrow();
            match borrow.get(idx) {
                Some(v) => Ok(v.clone()),
                None => {
                    if strict {
                        Err(eval_err("get!: index out of range"))
                    } else {
                        Ok(default)
                    }
                }
            }
        }
        RuntimeValue::Tuple(items) => {
            let idx = require_int_strict(key, "get")? as usize;
            match items.get(idx) {
                Some(v) => Ok(v.clone()),
                None => {
                    if strict {
                        Err(eval_err("get!: tuple index out of range"))
                    } else {
                        Ok(default)
                    }
                }
            }
        }
        RuntimeValue::Str(s) => {
            let idx = require_int_strict(key, "get")? as usize;
            match s.chars().nth(idx) {
                Some(c) => Ok(RuntimeValue::Str(c.to_string().into())),
                None => {
                    if strict {
                        Err(eval_err("get!: string index out of range"))
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
                            Err(eval_err("get!: key is missing"))
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

fn optional_slice_end(
    value: Option<&RuntimeValue>,
    len: usize,
    ctx: &str,
) -> Result<usize, crate::values::EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(len),
        Some(value) => Ok(normalize_slice_index(require_int_strict(value, ctx)?, len)),
    }
}

fn normalize_slice_index(index: i64, len: usize) -> usize {
    if index < 0 {
        len.saturating_sub(index.unsigned_abs() as usize)
    } else {
        (index as usize).min(len)
    }
}
