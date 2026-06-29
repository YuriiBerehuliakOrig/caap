/// Reflection and type inspection builtins.
///
/// Covers: value-type, host-value-kind, runtime-error, invoke, apply, gensym,
/// and value-to-string. Type predicate helpers belong in stdlib and should
/// compose `value-type` with ordinary equality.
use std::sync::atomic::{AtomicU64, Ordering};

use crate::eval::{eval_args, Evaluator};
use crate::values::{eval_err, RuntimeValue};

static GENSYM_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn register(ev: &mut Evaluator) {
    // ── value-type ────────────────────────────────────────────────────────────
    // Returns a canonical tag string for any value. Unlike host-value-kind, this
    // uses the documented canonical names: "callable" covers all function types.
    ev.register_special(
        "value_type",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["any"], "string"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Str(
                crate::values::canonical_type_tag(&args[0]).into(),
            ))
        },
    );

    // ── host-value-kind ───────────────────────────────────────────────────────
    ev.register_special(
        "host_value_kind",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["any"], "string"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let kind = match &args[0] {
                RuntimeValue::Null => "null",
                RuntimeValue::Bool(_) => "bool",
                RuntimeValue::Int(_) => "int",
                RuntimeValue::Float(_) => "float",
                RuntimeValue::Str(_) => "string",
                RuntimeValue::Bytes(_) => "bytes",
                RuntimeValue::List(_) => "list",
                RuntimeValue::Tuple(_) => "tuple",
                RuntimeValue::Map(_) => "map",
                RuntimeValue::Closure(_) => "closure",
                RuntimeValue::Macro(_) => "macro",
                RuntimeValue::Builtin(_) => "builtin",
                RuntimeValue::HostFunction(_) => "host_function",
                RuntimeValue::HostObject(object) => object.type_name(),
                RuntimeValue::Ref(_) => "ref",
                RuntimeValue::UninitializedTopLevel => "uninitialized_top_level",
            };
            Ok(RuntimeValue::Str(kind.into()))
        },
    );

    // ── runtime-error ─────────────────────────────────────────────────────────
    ev.register_special(
        "runtime_error",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["string"], "any")
            .with_control_policy(crate::semantic::ControlPolicy::StructuredExit),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let msg = match &args[0] {
                RuntimeValue::Str(s) => s.to_string(),
                other => format!("{other}"),
            };
            Err(eval_err(msg))
        },
    );

    // ── apply ─────────────────────────────────────────────────────────────────
    // (apply callable fixed-arg... list-arg)
    // Spreads the last argument (a list) as additional args.
    ev.register_special(
        "apply",
        2,
        None,
        crate::values::BuiltinMetadata::runtime_sequential()
            .with_signature(&["callable", "*any"], "any"),
        |ev, call, env| {
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
        },
    );

    // ── gensym ────────────────────────────────────────────────────────────────
    // Returns a unique symbol string on each call.
    ev.register_special(
        "gensym",
        0,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["string"], "string"),
        |ev, call, env| {
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
        },
    );

    // ── value-to-string ───────────────────────────────────────────────────────
    // Returns the canonical display representation of any value as a string.
    // Uses the same Display impl rendered by the evaluator for output.
    ev.register_special(
        "value_to_string",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["any"], "string"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Str(args[0].to_string().into()))
        },
    );

    // ── ctfe-debug-frames ────────────────────────────────────────────────────
    // The live closure-call stack as data, outermost first:
    //   [{name: str|null, span: span-map|null} …]
    // STRICTLY diagnostics-class (REPL / pass tracing): marked impure so pure
    // effect scopes cannot observe frame names (alpha-renaming must stay
    // unobservable in semantics) and the fold engine never folds it. Best
    // effort by design: host-driven callbacks and sub-evaluator bodies don't
    // appear, and a TCO trampoline shows one collapsed frame.
    ev.register_special(
        "ctfe_debug_frames",
        0,
        Some(0),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, _call, _env| {
            use super::compiler_query_helpers::map;
            use std::cell::RefCell;
            use std::rc::Rc;

            let frames: Vec<RuntimeValue> = ev
                .diagnostic_frame_snapshot()
                .into_iter()
                .map(|(name, span)| {
                    map([
                        (
                            "name",
                            name.map(|n| RuntimeValue::Str(n.as_ref().into()))
                                .unwrap_or(RuntimeValue::Null),
                        ),
                        (
                            "span",
                            span.map(|s| crate::builtins::compiler_units_helpers_span_to_value(&s))
                                .unwrap_or(RuntimeValue::Null),
                        ),
                    ])
                })
                .collect();
            Ok(RuntimeValue::List(Rc::new(RefCell::new(frames))))
        },
    );

    // ── ctfe-kernel-vocabulary ────────────────────────────────────────────────
    // The kernel's callable vocabulary as data, for compile-time tooling written
    // IN the language (load-time name/arity checkers, linters). The same
    // introspection `language::kernel_vocabulary` already offers Rust tooling —
    // this is mechanism (what exists), not policy (what to do about it).
    //
    // Returns a map: name -> {
    //   kind      : "builtin" (eager) | "special" (lazy/special form)
    //   min_arity : int
    //   max_arity : int | null      (null = unbounded)
    //   pure      : bool            (no declared effect tags)
    //   effects   : [tag…]          (e.g. "mutation")
    // }
    // Frontend-lowered forms (lambda/bind/set!/block/leave) appear with kind
    // "special" and null arities — they never reach the builtin registry.
    ev.register_special(
        "ctfe_kernel_vocabulary",
        0,
        Some(0),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, _call, _env| {
            use super::args::{str_key, tuple};
            use super::compiler_query_helpers::map;
            use std::cell::RefCell;
            use std::rc::Rc;

            // Session cache: the vocabulary materializes several times per
            // bootstrap (forms/check/effects passes). Callers always receive
            // a DETACHED copy, so mutating a result cannot poison the cache;
            // register_builtin invalidates it.
            if let Some(cached) = ev.vocabulary_cache().borrow().as_ref() {
                return Ok(detached_value_copy(cached));
            }

            let mut vocabulary = indexmap::IndexMap::new();
            let names: Vec<String> = ev
                .builtin_names()
                .iter()
                .map(|name| name.to_string())
                .collect();
            for name in names {
                let Some(info) = ev.builtin_info(&name) else {
                    continue;
                };
                if !info.metadata.is_public() {
                    continue;
                }
                let entry = map([
                    (
                        "kind",
                        RuntimeValue::Str(
                            if info.metadata.eager_args() {
                                "builtin"
                            } else {
                                "special"
                            }
                            .into(),
                        ),
                    ),
                    ("params", {
                        let params: Vec<RuntimeValue> = info
                            .metadata
                            .signature
                            .map(|sig| sig.params)
                            .unwrap_or(&["*any"])
                            .iter()
                            .map(|t| RuntimeValue::Str((*t).into()))
                            .collect();
                        RuntimeValue::List(Rc::new(RefCell::new(params)))
                    }),
                    (
                        "result",
                        RuntimeValue::Str(
                            info.metadata
                                .signature
                                .map(|sig| sig.result)
                                .unwrap_or("any")
                                .into(),
                        ),
                    ),
                    ("min_arity", RuntimeValue::Int(info.min_arity as i64)),
                    (
                        "max_arity",
                        info.max_arity
                            .map(|max| RuntimeValue::Int(max as i64))
                            .unwrap_or(RuntimeValue::Null),
                    ),
                    (
                        "pure",
                        RuntimeValue::Bool(info.metadata.effect_policy.is_pure()),
                    ),
                    (
                        "effects",
                        tuple(
                            info.metadata
                                .effect_policy
                                .tags()
                                .into_iter()
                                .map(|tag| RuntimeValue::Str(tag.into()))
                                .collect(),
                        ),
                    ),
                ]);
                vocabulary.insert(str_key(&name), entry);
            }
            for form in crate::language::FRONTEND_SPECIAL_FORMS {
                let entry = map([
                    ("kind", RuntimeValue::Str("special".into())),
                    (
                        "params",
                        RuntimeValue::List(Rc::new(RefCell::new(vec![RuntimeValue::Str(
                            "*any".into(),
                        )]))),
                    ),
                    ("result", RuntimeValue::Str("any".into())),
                    ("min_arity", RuntimeValue::Int(0)),
                    ("max_arity", RuntimeValue::Null),
                    ("pure", RuntimeValue::Bool(true)),
                    ("effects", tuple(Vec::new())),
                ]);
                vocabulary.insert(str_key(form), entry);
            }
            let built = RuntimeValue::Map(Rc::new(RefCell::new(vocabulary)));
            *ev.vocabulary_cache().borrow_mut() = Some(detached_value_copy(&built));
            Ok(built)
        },
    );
}

/// A structurally-detached copy: fresh maps and lists all the way down,
/// scalars by value, tuples shared (immutable). Used by the vocabulary cache
/// so callers can never mutate the cached value through a returned handle.
fn detached_value_copy(value: &RuntimeValue) -> RuntimeValue {
    use std::cell::RefCell;
    use std::rc::Rc;
    match value {
        RuntimeValue::Map(fields) => RuntimeValue::Map(Rc::new(RefCell::new(
            fields
                .borrow()
                .iter()
                .map(|(key, value)| (key.clone(), detached_value_copy(value)))
                .collect(),
        ))),
        RuntimeValue::List(items) => RuntimeValue::List(Rc::new(RefCell::new(
            items.borrow().iter().map(detached_value_copy).collect(),
        ))),
        other => other.clone(),
    }
}

/// Public comparison helper used by sequence-sort-by.
pub fn runtime_value_cmp(a: &RuntimeValue, b: &RuntimeValue) -> std::cmp::Ordering {
    super::value_compare::runtime_value_cmp(a, b)
}
