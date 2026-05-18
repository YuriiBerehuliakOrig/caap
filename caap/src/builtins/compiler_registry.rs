/// Compiler registry CTFE builtins — port of `caap/builtins/compiler/registry.py`.
use crate::compiler::CompilerBridgeValue;
use crate::eval::{eval_args, Evaluator};
use crate::ir::CallNode;
use crate::values::{eval_err, BuiltinInfo, EnvRef, EvalSignal, MapKey, RuntimeValue};

pub fn register(ev: &mut Evaluator) {
    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-register-value".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-register-value expects a compiler bridge",
            )?;
            let name = require_named_string(
                &args[1],
                "ctfe-compiler-register-value expects a non-empty string name",
            )?;
            bridge
                .register_value(name, args[2].clone())
                .map_err(eval_err)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-register-compile-time-function".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-register-compile-time-function expects a compiler bridge",
            )?;
            let name = require_named_string(
                &args[1],
                "ctfe-compiler-register-compile-time-function expects a non-empty string name",
            )?;
            bridge
                .register_compile_time_function(name, args[2].clone())
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-lookup-value".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(lookup_value),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-emit-event".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 4,
        max_arity: Some(5),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-emit-event expects a compiler bridge",
            )?;
            let component = require_named_string(
                &args[1],
                "ctfe-compiler-emit-event expects a non-empty component name",
            )?;
            let action = require_named_string(
                &args[2],
                "ctfe-compiler-emit-event expects a non-empty action name",
            )?;
            let message = require_string(
                &args[3],
                "ctfe-compiler-emit-event expects a message string",
            )?;
            let fields = if args.len() == 5 {
                event_fields(&args[4])?
            } else {
                Vec::new()
            };
            bridge
                .emit_event(component, action, message, fields)
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });
}

fn lookup_value(
    ev: &mut Evaluator,
    call: &CallNode,
    env: &EnvRef,
) -> Result<RuntimeValue, EvalSignal> {
    let args = eval_args(ev, call, env)?;
    let bridge = require_compiler_bridge(
        &args[0],
        "ctfe-compiler-lookup-value expects a compiler bridge",
    )?;
    let name = require_named_string(
        &args[1],
        "ctfe-compiler-lookup-value expects a non-empty string name",
    )?;
    if let Some(value) = bridge.lookup_registered_value(&name).map_err(eval_err)? {
        return Ok(value);
    }
    if args.len() == 3 {
        return Ok(args[2].clone());
    }
    Err(eval_err(format!(
        "compiler registry does not contain {name:?}"
    )))
}

pub(crate) fn require_compiler_bridge<'a>(
    value: &'a RuntimeValue,
    message: &str,
) -> Result<&'a CompilerBridgeValue, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    object
        .as_any()
        .downcast_ref::<CompilerBridgeValue>()
        .ok_or_else(|| eval_err(message))
}

pub(crate) fn require_named_string(
    value: &RuntimeValue,
    message: &str,
) -> Result<String, EvalSignal> {
    let text = require_string(value, message)?;
    if text.is_empty() {
        return Err(eval_err(message));
    }
    Ok(text)
}

pub(crate) fn require_string(value: &RuntimeValue, message: &str) -> Result<String, EvalSignal> {
    match value {
        RuntimeValue::Str(text) => Ok(text.to_string()),
        _ => Err(eval_err(message)),
    }
}

fn event_fields(value: &RuntimeValue) -> Result<Vec<(String, String)>, EvalSignal> {
    match value {
        RuntimeValue::Null => Ok(Vec::new()),
        RuntimeValue::Map(map) => Ok(map
            .borrow()
            .iter()
            .map(|(key, value)| (field_key(key), value.to_string()))
            .collect()),
        _ => Err(eval_err(
            "ctfe-compiler-emit-event expects a fields map when provided",
        )),
    }
}

fn field_key(key: &MapKey) -> String {
    match key {
        MapKey::Str(value) => value.to_string(),
        other => other.to_string(),
    }
}
