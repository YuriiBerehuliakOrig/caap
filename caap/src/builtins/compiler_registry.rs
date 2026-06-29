use super::compiler_units_helpers::{map_entries, string, tuple};
use super::semantic_projection::builtin_policy_projection_fields;
use crate::compiler::CompilerBridgeValue;
use crate::eval::{eval_args, Evaluator};
use crate::ir::CallNode;
use crate::values::{eval_err, BuiltinMetadata, EnvRef, EvalSignal, MapKey, RuntimeValue};
/// Compiler registry CTFE builtins — port of `caap/builtins/compiler/registry.py`.
pub fn register(ev: &mut Evaluator) {
    ev.register_special(
        "ctfe_compiler_register_value",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
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
        },
    );

    ev.register_special(
        "ctfe_compiler_lookup_value",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        lookup_value,
    );

    ev.register_special(
        "ctfe_compiler_builtin_semantic_entries",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            require_compiler_bridge(
                &args[0],
                "ctfe-compiler-builtin-semantic-entries expects a compiler bridge",
            )?;
            let entries = ev
                .builtin_names()
                .into_iter()
                .filter_map(|name| {
                    let metadata = &ev.builtin_info(name)?.metadata;
                    metadata
                        .is_public()
                        .then(|| builtin_semantic_entry_value(name, metadata))
                })
                .collect();
            Ok(tuple(entries))
        },
    );

    ev.register_special(
        "ctfe_compiler_emit_event",
        4,
        Some(5),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
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
        },
    );
}

fn builtin_semantic_entry_value(name: &str, metadata: &BuiltinMetadata) -> RuntimeValue {
    let mut fields = vec![("name", string(name)), ("source", string("builtin"))];
    fields.extend(builtin_policy_projection_fields(metadata));
    map_entries(fields)
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

// Canonical string coercion lives in `args`; re-export under the names other
// modules already import from `compiler_registry`.
pub(crate) use super::args::{require_named_string, require_string};

fn event_fields(value: &RuntimeValue) -> Result<Vec<(String, String)>, EvalSignal> {
    match value {
        RuntimeValue::Null => Ok(Vec::new()),
        RuntimeValue::Map(map) => map
            .borrow()
            .iter()
            .map(|(key, value)| Ok((field_key(key)?, event_field_value(value)?)))
            .collect(),
        _ => Err(eval_err(
            "ctfe-compiler-emit-event expects a fields map when provided",
        )),
    }
}

fn field_key(key: &MapKey) -> Result<String, EvalSignal> {
    match key {
        MapKey::Str(value) if !value.is_empty() => Ok(value.to_string()),
        _ => Err(eval_err(
            "ctfe-compiler-emit-event field keys must be non-empty strings",
        )),
    }
}

fn event_field_value(value: &RuntimeValue) -> Result<String, EvalSignal> {
    match value {
        RuntimeValue::Null => Ok("null".to_string()),
        RuntimeValue::Bool(value) => Ok(value.to_string()),
        RuntimeValue::Int(value) => Ok(value.to_string()),
        RuntimeValue::Float(value) => Ok(value.to_string()),
        RuntimeValue::Str(value) => Ok(value.to_string()),
        _ => Err(eval_err(
            "ctfe-compiler-emit-event field values must be scalar",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn event_fields_accept_string_keyed_scalar_values() {
        let fields = RuntimeValue::Map(Rc::new(RefCell::new(IndexMap::from([
            (MapKey::Str(Rc::from("count")), RuntimeValue::Int(3)),
            (MapKey::Str(Rc::from("ok")), RuntimeValue::Bool(true)),
        ]))));

        let mut projected = event_fields(&fields).unwrap();
        projected.sort();

        assert_eq!(
            projected,
            vec![
                ("count".to_string(), "3".to_string()),
                ("ok".to_string(), "true".to_string())
            ]
        );
    }

    #[test]
    fn event_fields_reject_non_string_keys() {
        let fields = RuntimeValue::Map(Rc::new(RefCell::new(IndexMap::from([(
            MapKey::Null,
            RuntimeValue::Str(Rc::from("value")),
        )]))));

        let error = event_fields(&fields).unwrap_err().to_string();

        assert!(error.contains("field keys must be non-empty strings"));
    }

    #[test]
    fn event_fields_reject_structured_values() {
        let fields = RuntimeValue::Map(Rc::new(RefCell::new(IndexMap::from([(
            MapKey::Str(Rc::from("nested")),
            RuntimeValue::Tuple(Vec::new().into()),
        )]))));

        let error = event_fields(&fields).unwrap_err().to_string();

        assert!(error.contains("field values must be scalar"));
    }
}
