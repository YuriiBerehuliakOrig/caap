/// Internal host-service capability builtins.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::bridges::HostCapabilityBridgeValue;
use crate::builtins::compiler_registry::{require_compiler_bridge, require_named_string};
use crate::compiler::CompilerBridgeValue;
use crate::eval::{eval_args, Evaluator};
use crate::host::HostServiceExport;
use crate::semantic::PhasePolicy;
use crate::values::{
    eval_err, BuiltinInfo, EnvRef, EvalSignal, HostFunction, MapKey, RuntimeValue,
};

pub fn register(ev: &mut Evaluator) {
    ev.register_builtin(BuiltinInfo {
        name: "host-service-export".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let library = require_named_string(&args[0], "host-service-export expects a library")?;
            let export = require_named_string(&args[1], "host-service-export expects an export")?;
            let phase = optional_phase(args.get(2), PhasePolicy::CompileTime)?;
            with_compiler(env, |compiler| {
                compiler
                    .host_service_export(&library, &export, phase)
                    .map_err(eval_err)
            })?
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "host-runtime-service-export".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let capability = require_host_capability(
                &args[0],
                "host-runtime-service-export requires a host_services projection capability",
            )?;
            if capability.capability_kind() != "host_services" {
                return Err(eval_err(
                    "host-runtime-service-export requires a host_services projection capability",
                ));
            }
            let library =
                require_named_string(&args[1], "host-runtime-service-export expects a library")?;
            let export =
                require_named_string(&args[2], "host-runtime-service-export expects an export")?;
            with_compiler(env, |compiler| {
                compiler
                    .host_service_export(&library, &export, PhasePolicy::Runtime)
                    .map_err(eval_err)
            })?
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "host-service-capability".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let kind =
                require_named_string(&args[0], "host-service-capability expects a capability")?;
            Ok(RuntimeValue::HostObject(Rc::new(
                HostCapabilityBridgeValue::new(kind).map_err(eval_err)?,
            )))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "host-service-capability-export".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 3,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let capability = require_host_capability(
                &args[0],
                "host-service-capability-export expects a host capability",
            )?;
            let library =
                require_named_string(&args[1], "host-service-capability-export expects a library")?;
            let export =
                require_named_string(&args[2], "host-service-capability-export expects an export")?;
            let phase = optional_phase(args.get(3), PhasePolicy::CompileTime)?;
            let exported = with_compiler(env, |compiler| {
                compiler
                    .host_service_export(&library, &export, phase)
                    .map_err(eval_err)
            })??;
            wrap_capability_export(capability.capability_kind(), exported)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "host-service-libraries".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 0,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let phase = optional_phase(args.first(), PhasePolicy::CompileTime)?;
            with_compiler(env, |compiler| {
                Ok(tuple(
                    compiler
                        .host_service_libraries(phase)
                        .iter()
                        .map(string)
                        .collect(),
                ))
            })?
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "host-service-library-catalog".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let library =
                require_named_string(&args[0], "host-service-library-catalog expects a library")?;
            let phase = optional_phase(args.get(1), PhasePolicy::CompileTime)?;
            with_compiler(env, |compiler| {
                let catalog = compiler
                    .host_service_library_catalog(&library, phase)
                    .map_err(eval_err)?;
                Ok(tuple(
                    catalog.iter().map(host_service_catalog_entry).collect(),
                ))
            })?
        }),
    });
}

fn with_compiler<R>(
    env: &EnvRef,
    f: impl FnOnce(&CompilerBridgeValue) -> R,
) -> Result<R, EvalSignal> {
    let compiler =
        crate::values::Environment::lookup(env, "compiler").map_err(EvalSignal::Error)?;
    let compiler =
        require_compiler_bridge(&compiler, "host service builtin expects compiler binding")?;
    Ok(f(compiler))
}

fn require_host_capability<'a>(
    value: &'a RuntimeValue,
    message: &str,
) -> Result<&'a HostCapabilityBridgeValue, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    object
        .as_any()
        .downcast_ref::<HostCapabilityBridgeValue>()
        .ok_or_else(|| eval_err(message))
}

fn optional_phase(
    value: Option<&RuntimeValue>,
    default: PhasePolicy,
) -> Result<PhasePolicy, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(default),
        Some(RuntimeValue::Str(value)) => match value.as_ref() {
            "runtime" => Ok(PhasePolicy::Runtime),
            "compile_time" | "compile-time" => Ok(PhasePolicy::CompileTime),
            "dual" => Ok(PhasePolicy::Dual),
            _ => Err(eval_err(
                "host service phase must be runtime, compile_time, or dual",
            )),
        },
        Some(_) => Err(eval_err("host service phase must be a string")),
    }
}

fn wrap_capability_export(
    expected_kind: &str,
    exported: RuntimeValue,
) -> Result<RuntimeValue, EvalSignal> {
    let RuntimeValue::HostFunction(function) = exported else {
        return Err(eval_err(
            "host-service-capability-export requires a callable host service export",
        ));
    };
    let expected_kind = expected_kind.to_string();
    let name = format!("capability:{}", function.name);
    let min_arity = function.min_arity + 1;
    let max_arity = function.max_arity.map(|arity| arity + 1);
    let wrapped = HostFunction::new(
        name,
        min_arity,
        max_arity,
        Box::new(move |args| {
            let capability = args
                .first()
                .ok_or_else(|| eval_err("missing host capability argument"))?;
            let capability = require_host_capability(
                capability,
                "host-service-capability-export wrapper expects a host capability",
            )?;
            if capability.capability_kind() != expected_kind {
                return Err(eval_err(format!(
                    "host capability kind {:?} does not match expected {:?}",
                    capability.capability_kind(),
                    expected_kind
                )));
            }
            (function.handler)(args[1..].to_vec())
        }),
    )
    .map_err(eval_err)?;
    Ok(RuntimeValue::HostFunction(Rc::new(wrapped)))
}

fn host_service_catalog_entry(entry: &HostServiceExport) -> RuntimeValue {
    let metadata = &entry.metadata;
    map([
        ("library", string(entry.library.as_str())),
        (
            "module",
            metadata
                .module
                .as_ref()
                .map(string)
                .unwrap_or(RuntimeValue::Null),
        ),
        ("public", string(metadata.public.as_str())),
        ("export", string(entry.name.as_str())),
        ("phase", string(entry.phase.as_str())),
        ("effect", string(metadata.effect.as_str())),
        ("pure", RuntimeValue::Bool(metadata.pure)),
        ("kind", string(metadata.kind.as_str())),
        ("policy", string(metadata.policy.as_str())),
        ("min_arity", RuntimeValue::Int(metadata.min_arity as i64)),
        (
            "max_arity",
            metadata
                .max_arity
                .map(|arity| RuntimeValue::Int(arity as i64))
                .unwrap_or(RuntimeValue::Null),
        ),
        ("variadic", RuntimeValue::Bool(metadata.variadic)),
        (
            "capability_kind",
            metadata
                .capability_kind
                .as_ref()
                .map(string)
                .unwrap_or(RuntimeValue::Null),
        ),
        ("signature", host_function_signature(entry)),
    ])
}

fn host_function_signature(entry: &HostServiceExport) -> RuntimeValue {
    let signature = &entry.metadata.signature;
    map([
        (
            "params",
            tuple(
                signature
                    .params
                    .iter()
                    .map(|param| {
                        map([
                            ("name", string(param.name.as_str())),
                            ("type", string(param.type_name.as_str())),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("result", string(signature.result.as_str())),
        (
            "min_arity",
            RuntimeValue::Int(entry.function.min_arity as i64),
        ),
        (
            "max_arity",
            entry
                .function
                .max_arity
                .map(|arity| RuntimeValue::Int(arity as i64))
                .unwrap_or(RuntimeValue::Null),
        ),
    ])
}

fn map<const N: usize>(entries: [(&str, RuntimeValue); N]) -> RuntimeValue {
    let mut map = HashMap::new();
    for (key, value) in entries {
        map.insert(MapKey::Str(key.into()), value);
    }
    RuntimeValue::Map(Rc::new(RefCell::new(map)))
}

fn tuple(items: Vec<RuntimeValue>) -> RuntimeValue {
    RuntimeValue::Tuple(items.into())
}

fn string(value: impl AsRef<str>) -> RuntimeValue {
    RuntimeValue::Str(value.as_ref().into())
}
