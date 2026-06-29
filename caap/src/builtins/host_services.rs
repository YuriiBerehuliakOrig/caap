/// Internal host-service capability builtins.
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

use crate::bridges::HostCapabilityBridgeValue;
use crate::builtins::compiler_registry::{require_compiler_bridge, require_named_string};
use crate::compiler::CompilerBridgeValue;
use crate::eval::{eval_args, Evaluator};
use crate::host::HostServiceExport;
use crate::semantic::{CapabilityName, PhasePolicy};
use crate::values::{eval_err, EnvRef, EvalSignal, HostFunction, MapKey, RuntimeValue};

pub fn register(ev: &mut Evaluator) {
    ev.register_special(
        "host_service_export",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let library = require_named_string(&args[0], "host-service-export expects a library")?;
            let export = require_named_string(&args[1], "host-service-export expects an export")?;
            let phase = optional_phase(args.get(2), PhasePolicy::CompileTime)?;
            with_compiler(env, |compiler| {
                compiler
                    .require_host_service_capability(&library, &export, phase)
                    .map_err(|error| eval_err(error.to_string()))?;
                compiler
                    .host_service_export(&library, &export, phase)
                    .map_err(eval_err)
            })?
        },
    );

    ev.register_special(
        "host_service_capability",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let kind =
                require_named_string(&args[0], "host-service-capability expects a capability")?;
            // Minting a capability projection requires holding the capability it
            // projects: a `sys.io` projection needs `sys.io`, while the root
            // `sys` projection requires explicit coarse authority.
            with_compiler(env, |compiler| {
                compiler
                    .require_current_bootstrap_capability(&kind)
                    .map_err(|error| eval_err(error.to_string()))
            })??;
            Ok(RuntimeValue::HostObject(Rc::new(
                HostCapabilityBridgeValue::new(kind).map_err(eval_err)?,
            )))
        },
    );

    ev.register_special(
        "host_service_capability_export",
        3,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
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
                require_capability_export_authority(
                    compiler,
                    capability.capability_kind(),
                    &library,
                    &export,
                    phase,
                )?;
                compiler
                    .host_service_export(&library, &export, phase)
                    .map_err(eval_err)
            })??;
            wrap_capability_export(capability.capability_kind(), exported)
        },
    );

    ev.register_special(
        "host_service_libraries",
        0,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
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
        },
    );

    ev.register_special(
        "host_service_library_catalog",
        1,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
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
        },
    );
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

fn require_capability_export_authority(
    compiler: &CompilerBridgeValue,
    capability_kind: &str,
    library: &str,
    export: &str,
    phase: PhasePolicy,
) -> Result<(), EvalSignal> {
    compiler
        .require_current_bootstrap_capability(capability_kind)
        .map_err(|error| eval_err(error.to_string()))?;
    let required = compiler
        .host_service_required_capability(library, export, phase)
        .map_err(eval_err)?;
    let allowed = match required.as_deref() {
        None => true,
        Some(required) => {
            let projection = CapabilityName::new(capability_kind).map_err(eval_err)?;
            let requested = CapabilityName::new(required).map_err(eval_err)?;
            projection.covers(&requested)
        }
    };
    if !allowed {
        return Err(eval_err(format!(
            "host-service-capability-export cannot project {library}.{export} requiring {:?} through capability {capability_kind:?}",
            required
        )));
    }
    Ok(())
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
        Some(RuntimeValue::Str(value)) => {
            PhasePolicy::parse_label(value.as_ref()).map_err(eval_err)
        }
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
    let phase_policy = function.phase_policy;
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
    .map(|function| function.with_phase_policy(phase_policy))
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
        ("public", string(entry.name.as_str())),
        ("export", string(entry.name.as_str())),
        ("phase", string(entry.phase.as_str())),
        ("effect", string(metadata.effect.as_str())),
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
        ("variadic", RuntimeValue::Bool(metadata.is_variadic())),
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
    let mut map = IndexMap::new();
    for (key, value) in entries {
        map.insert(MapKey::Str(key.into()), value);
    }
    RuntimeValue::Map(Rc::new(RefCell::new(map)))
}

use super::args::{string, tuple};
