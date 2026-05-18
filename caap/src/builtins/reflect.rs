/// Reflection and type-checking builtins.
///
/// Covers: value-is-*, host-value-kind, runtime-error, invoke, apply, gensym,
/// value-lt / value-gt aliases.
use std::sync::atomic::{AtomicU64, Ordering};

use crate::eval::{eval_args, Evaluator};
use crate::values::{eval_err, BuiltinInfo, RuntimeValue};

static GENSYM_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn register(ev: &mut Evaluator) {
    // ── type predicates ───────────────────────────────────────────────────────

    ev.register_builtin(BuiltinInfo {
        name: "value-is-null".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Bool(matches!(args[0], RuntimeValue::Null)))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "value-is-bool".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Bool(matches!(args[0], RuntimeValue::Bool(_))))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "value-is-int".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Bool(matches!(args[0], RuntimeValue::Int(_))))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "value-is-float".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Bool(matches!(
                args[0],
                RuntimeValue::Float(_)
            )))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "value-is-string".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Bool(matches!(args[0], RuntimeValue::Str(_))))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "value-is-list".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Bool(matches!(args[0], RuntimeValue::List(_))))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "value-is-tuple".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Bool(matches!(
                args[0],
                RuntimeValue::Tuple(_)
            )))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "value-is-map".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Bool(matches!(args[0], RuntimeValue::Map(_))))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "value-is-callable".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Bool(matches!(
                args[0],
                RuntimeValue::Closure(_) | RuntimeValue::Builtin(_) | RuntimeValue::HostFunction(_)
            )))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "value-is-error?".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let _args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Bool(false))
        }),
    });

    // ── host-value-kind ───────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "host-value-kind".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let kind = match &args[0] {
                RuntimeValue::Null => "null",
                RuntimeValue::Bool(_) => "bool",
                RuntimeValue::Int(_) => "int",
                RuntimeValue::Float(_) => "float",
                RuntimeValue::Str(_) => "string",
                RuntimeValue::List(_) => "list",
                RuntimeValue::Tuple(_) => "tuple",
                RuntimeValue::Map(_) => "map",
                RuntimeValue::Closure(_) | RuntimeValue::Builtin(_) => "closure",
                RuntimeValue::HostFunction(_) => "host-function",
                RuntimeValue::HostObject(object) => object.type_name(),
                RuntimeValue::UninitializedTopLevel => "uninitialized-top-level",
            };
            Ok(RuntimeValue::Str(kind.into()))
        }),
    });

    // ── runtime-error ─────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "runtime-error".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime()
            .with_control_policy(crate::semantic::ControlPolicy::StructuredExit),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let msg = match &args[0] {
                RuntimeValue::Str(s) => s.to_string(),
                other => format!("{other}"),
            };
            Err(eval_err(msg))
        }),
    });

    // ── invoke ────────────────────────────────────────────────────────────────
    // (invoke callable arg ...)
    ev.register_builtin(BuiltinInfo {
        name: "invoke".to_string(),
        metadata: crate::values::BuiltinMetadata::runtime_sequential(),
        min_arity: 1,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let callable = args[0].clone();
            ev.invoke_callback(&callable, args[1..].to_vec())
        }),
    });

    // ── apply ─────────────────────────────────────────────────────────────────
    // (apply callable fixed-arg... list-arg)
    // Spreads the last argument (a list) as additional args.
    ev.register_builtin(BuiltinInfo {
        name: "apply".to_string(),
        metadata: crate::values::BuiltinMetadata::runtime_sequential(),
        min_arity: 2,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if args.len() < 2 {
                return Err(eval_err(
                    "apply expects a callable and a final list argument",
                ));
            }
            let callable = args[0].clone();
            let fixed = args[1..args.len() - 1].to_vec();
            let rest = match &args[args.len() - 1] {
                RuntimeValue::List(l) => l.borrow().clone(),
                RuntimeValue::Tuple(items) => items.iter().cloned().collect(),
                other => {
                    return Err(eval_err(format!(
                        "apply: last argument must be a list or tuple, got {other}"
                    )))
                }
            };
            let all_args = [fixed, rest].concat();
            ev.invoke_callback(&callable, all_args)
        }),
    });

    // ── gensym ────────────────────────────────────────────────────────────────
    // Returns a unique symbol string on each call.
    ev.register_builtin(BuiltinInfo {
        name: "gensym".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 0,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let n = GENSYM_COUNTER.fetch_add(1, Ordering::Relaxed);
            let prefix = if args.is_empty() {
                "g".to_string()
            } else {
                match &args[0] {
                    RuntimeValue::Str(s) => s.to_string(),
                    _ => "g".to_string(),
                }
            };
            Ok(RuntimeValue::Str(format!("{prefix}__{n}").into()))
        }),
    });

    // ── value-lt / value-gt (canonical names from Python registry) ────────────
    // Identical behaviour to "lt" / "gt" already in arithmetic.rs.
    ev.register_builtin(BuiltinInfo {
        name: "value-lt".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 0,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if args.len() < 2 {
                return Ok(RuntimeValue::Bool(true));
            }
            Ok(RuntimeValue::Bool(
                args.windows(2).all(|w| cmp_lt(&w[0], &w[1])),
            ))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "value-gt".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 0,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if args.len() < 2 {
                return Ok(RuntimeValue::Bool(true));
            }
            Ok(RuntimeValue::Bool(
                args.windows(2).all(|w| cmp_lt(&w[1], &w[0])),
            ))
        }),
    });

    // ── le / ge ───────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "le".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 0,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if args.len() < 2 {
                return Ok(RuntimeValue::Bool(true));
            }
            Ok(RuntimeValue::Bool(
                args.windows(2).all(|w| !cmp_lt(&w[1], &w[0])),
            ))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ge".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 0,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if args.len() < 2 {
                return Ok(RuntimeValue::Bool(true));
            }
            Ok(RuntimeValue::Bool(
                args.windows(2).all(|w| !cmp_lt(&w[0], &w[1])),
            ))
        }),
    });
}

fn cmp_lt(a: &RuntimeValue, b: &RuntimeValue) -> bool {
    match (a, b) {
        (RuntimeValue::Int(x), RuntimeValue::Int(y)) => x < y,
        (RuntimeValue::Float(x), RuntimeValue::Float(y)) => x < y,
        (RuntimeValue::Str(x), RuntimeValue::Str(y)) => x.as_ref() < y.as_ref(),
        (RuntimeValue::Int(x), RuntimeValue::Float(y)) => (*x as f64) < *y,
        (RuntimeValue::Float(x), RuntimeValue::Int(y)) => *x < (*y as f64),
        _ => false,
    }
}

/// Public comparison helper used by sequence-sort-by.
pub fn runtime_value_cmp(a: &RuntimeValue, b: &RuntimeValue) -> std::cmp::Ordering {
    match (a, b) {
        (RuntimeValue::Null, RuntimeValue::Null) => std::cmp::Ordering::Equal,
        (RuntimeValue::Bool(x), RuntimeValue::Bool(y)) => x.cmp(y),
        (RuntimeValue::Int(x), RuntimeValue::Int(y)) => x.cmp(y),
        (RuntimeValue::Float(x), RuntimeValue::Float(y)) => {
            x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
        }
        (RuntimeValue::Str(x), RuntimeValue::Str(y)) => x.as_ref().cmp(y.as_ref()),
        (RuntimeValue::Int(x), RuntimeValue::Float(y)) => (*x as f64)
            .partial_cmp(y)
            .unwrap_or(std::cmp::Ordering::Equal),
        (RuntimeValue::Float(x), RuntimeValue::Int(y)) => x
            .partial_cmp(&(*y as f64))
            .unwrap_or(std::cmp::Ordering::Equal),
        _ => type_rank(a).cmp(&type_rank(b)),
    }
}

fn type_rank(v: &RuntimeValue) -> u8 {
    match v {
        RuntimeValue::Null => 0,
        RuntimeValue::Bool(_) => 1,
        RuntimeValue::Int(_) => 2,
        RuntimeValue::Float(_) => 3,
        RuntimeValue::Str(_) => 4,
        _ => 255,
    }
}
