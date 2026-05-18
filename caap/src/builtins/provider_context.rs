/// Provider-context CTFE builtins for Rust query providers.
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::bridges::{NodeBridgeValue, SemanticEntryBridgeValue};
use crate::builtins::compiler_query::query_artifact_to_value;
use crate::builtins::ir_builders::ExprSpecBridgeValue;
use crate::compiler::{
    ProviderContextBridgeValue, QueryArtifactSource, QueryExecutionOptions, UnitBridgeValue,
};
use crate::diagnostics::{Diagnostic, DiagnosticFix, DiagnosticSeverity};
use crate::eval::{eval_args, Evaluator};
use crate::ir::{CallNode, ExprSpec, IrLiteralData, Node, NodeId};
use crate::semantic::{
    node_subject_id, symbol_subject_id, ControlPolicy, EffectPolicy, EntrySource, EvalPolicy,
    PhasePolicy, ScopePolicy, SemanticEntry, SemanticRegistry, SemanticValue, SymbolEntry,
    SymbolKind,
};
use crate::unit::{LinkBinding, Unit};
use crate::values::{
    eval_err, is_truthy, BuiltinInfo, ClosureValue, EnvRef, Environment, EvalSignal, HostObject,
    MapKey, RuntimeValue,
};

#[derive(Clone, Debug)]
struct ResolutionScopeBridgeValue {
    registry: Rc<RefCell<SemanticRegistry>>,
}

impl ResolutionScopeBridgeValue {
    fn new(registry: SemanticRegistry) -> Self {
        Self {
            registry: Rc::new(RefCell::new(registry)),
        }
    }
}

impl HostObject for ResolutionScopeBridgeValue {
    fn type_name(&self) -> &'static str {
        "resolution-scope"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Clone, Debug)]
enum CtfeResult {
    NoChange,
    Replace(ExprSpec),
    Lift(RuntimeValue),
}

#[derive(Clone, Debug)]
struct CtfeResultBridgeValue {
    result: CtfeResult,
}

impl CtfeResultBridgeValue {
    fn new(result: CtfeResult) -> Self {
        Self { result }
    }
}

impl HostObject for CtfeResultBridgeValue {
    fn type_name(&self) -> &'static str {
        "ctfe-result"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub fn register(ev: &mut Evaluator) {
    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-require-effect".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-require-effect expects a provider context",
            )?;
            let effect = normalize_effect(&require_string(
                &args[1],
                "ctfe-provider-require-effect expects an effect name",
            )?);
            if ctx.context().effect_tags.iter().any(|tag| tag == &effect) {
                Ok(RuntimeValue::Null)
            } else {
                Err(eval_err(format!(
                    "provider {} does not declare required effect {effect}",
                    ctx.context().provider
                )))
            }
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-unit-version".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-unit-version expects a provider context",
            )?;
            Ok(RuntimeValue::Int(ctx.unit().clone_unit().version() as i64))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-unit".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-unit expects a provider context",
            )?;
            Ok(RuntimeValue::HostObject(ctx.unit()))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-query-artifact".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(5),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-query-artifact expects a provider context",
            )?;
            require_provider_effect(ctx, "read_registry")?;
            let target = require_string(
                &args[1],
                "ctfe-provider-query-artifact expects a non-empty target name",
            )?;
            let source = provider_query_artifact_source(&args[2])?;
            let phase = args
                .get(3)
                .map(|value| {
                    phase_value(
                        value,
                        "ctfe-provider-query-artifact expects phase runtime, compile_time, or dual",
                    )
                })
                .transpose()?
                .unwrap_or(PhasePolicy::CompileTime);
            let initial = provider_initial_bindings(
                args.get(4),
                "ctfe-provider-query-artifact expects an initial bindings map when provided",
            )?;
            let artifact = ctx
                .compiler()
                .query_artifact_with_options(
                    target,
                    source,
                    phase,
                    QueryExecutionOptions::new().with_initial_bindings(initial),
                )
                .map_err(eval_err)?;
            ctx.absorb_artifact_dependencies(&artifact);
            Ok(query_artifact_to_value(&artifact))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-lookup-value".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-lookup-value expects a provider context",
            )?;
            require_provider_effect(ctx, "read_registry")?;
            let name = require_string(
                &args[1],
                "ctfe-provider-lookup-value expects a non-empty registered value name",
            )?;
            Ok(ctx
                .compiler()
                .lookup_registered_value(&name)
                .map_err(eval_err)?
                .unwrap_or_else(|| args.get(2).cloned().unwrap_or(RuntimeValue::Null)))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-compiled-unit".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-compiled-unit expects a provider context",
            )?;
            require_provider_effect(ctx, "read_registry")?;
            let unit_id = require_string(
                &args[1],
                "ctfe-provider-compiled-unit expects a non-empty unit id",
            )?;
            Ok(ctx
                .compiler()
                .lookup_compiled_unit(&unit_id)
                .map_err(eval_err)?
                .map(|unit| RuntimeValue::HostObject(Rc::new(unit)))
                .unwrap_or_else(|| args.get(2).cloned().unwrap_or(RuntimeValue::Null)))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-resolve-path".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-resolve-path expects a provider context",
            )?;
            require_provider_effect(ctx, "read_files")?;
            let path = require_string(
                &args[1],
                "ctfe-provider-resolve-path expects a non-empty path",
            )?;
            let base = args
                .get(2)
                .map(|value| {
                    require_string(
                        value,
                        "ctfe-provider-resolve-path expects a non-empty base path when provided",
                    )
                })
                .transpose()?;
            let resolved = provider_resolve_path(&path, base.as_deref())?;
            let resolved = path_to_string(&resolved)?;
            ctx.track_file_read(resolved.clone());
            Ok(RuntimeValue::Str(resolved.into()))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-read-text".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-read-text expects a provider context",
            )?;
            require_provider_effect(ctx, "read_files")?;
            let path =
                require_string(&args[1], "ctfe-provider-read-text expects a non-empty path")?;
            let base = args
                .get(2)
                .map(|value| {
                    require_string(
                        value,
                        "ctfe-provider-read-text expects a non-empty base path when provided",
                    )
                })
                .transpose()?;
            let resolved = provider_resolve_path(&path, base.as_deref())?;
            ctx.track_file_read(path_to_string(&resolved)?);
            let text = fs::read_to_string(&resolved)
                .map_err(|error| eval_err(format!("ctfe-provider-read-text: {error}")))?;
            Ok(RuntimeValue::Str(text.into()))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-base-resolution-scope".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-base-resolution-scope expects a provider context",
            )?;
            require_provider_effect(ctx, "read_symbols")?;
            let registry = ctx
                .unit()
                .with_unit(|unit| base_resolution_scope_for_context(ctx, unit))
                .map_err(eval_err)?;
            Ok(RuntimeValue::HostObject(Rc::new(
                ResolutionScopeBridgeValue::new(registry),
            )))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-resolution-scope-fork".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let scope = require_resolution_scope(
                &args[0],
                "ctfe-resolution-scope-fork expects a resolution scope",
            )?;
            let child = scope.registry.borrow().fork();
            Ok(RuntimeValue::HostObject(Rc::new(
                ResolutionScopeBridgeValue::new(child),
            )))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-resolution-scope-lookup".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let scope = require_resolution_scope(
                &args[0],
                "ctfe-resolution-scope-lookup expects a resolution scope",
            )?;
            let name = require_string(
                &args[1],
                "ctfe-resolution-scope-lookup expects a non-empty name",
            )?;
            let default = args.get(2).cloned().unwrap_or(RuntimeValue::Null);
            let resolved = scope
                .registry
                .borrow()
                .lookup(&name)
                .map_err(eval_err)?
                .cloned();
            Ok(resolved.map(semantic_entry_handle).unwrap_or(default))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-resolution-scope-define!".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let scope = require_resolution_scope(
                &args[0],
                "ctfe-resolution-scope-define! expects a resolution scope",
            )?;
            let entry = require_semantic_entry(
                &args[1],
                "ctfe-resolution-scope-define! expects a semantic entry",
            )?;
            scope
                .registry
                .borrow_mut()
                .define(entry.clone())
                .map_err(eval_err)?;
            Ok(args[1].clone())
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-semantic-entry-new".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 3,
        max_arity: Some(5),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-semantic-entry-new expects a provider context",
            )?;
            let name = require_string(
                &args[1],
                "ctfe-provider-semantic-entry-new expects a non-empty name",
            )?;
            let source = entry_source_value(
                &args[2],
                "ctfe-provider-semantic-entry-new expects a valid entry source",
            )?;
            let node_id = match args.get(3) {
                None | Some(RuntimeValue::Null) => None,
                Some(value) => Some(require_node_id(
                    value,
                    "ctfe-provider-semantic-entry-new expects a node",
                )?),
            };
            let phase_policy = match args.get(4) {
                None | Some(RuntimeValue::Null) => PhasePolicy::Runtime,
                Some(value) => phase_value(
                    value,
                    "ctfe-provider-semantic-entry-new expects phase runtime, compile_time, or dual",
                )?,
            };
            let mut entry = SemanticEntry::new(name, source).map_err(eval_err)?;
            entry.phase_policy = phase_policy;
            entry.node_id = node_id;
            entry.unit_id = Some(ctx.unit().clone_unit().unit_id().to_string());
            Ok(semantic_entry_handle(entry))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-semantic-entry-source".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let entry = require_semantic_entry(
                &args[0],
                "ctfe-semantic-entry-source expects a resolved semantic entry",
            )?;
            Ok(RuntimeValue::Str(entry.source.as_str().into()))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-semantic-entry-name".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let entry = require_semantic_entry(
                &args[0],
                "ctfe-semantic-entry-name expects a resolved semantic entry",
            )?;
            Ok(RuntimeValue::Str(entry.name.as_str().into()))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-semantic-entry-unit".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let entry = require_semantic_entry(
                &args[0],
                "ctfe-semantic-entry-unit expects a resolved semantic entry",
            )?;
            Ok(entry
                .unit_id
                .as_deref()
                .map(|unit_id| RuntimeValue::Str(unit_id.into()))
                .unwrap_or_else(|| args.get(1).cloned().unwrap_or(RuntimeValue::Null)))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-semantic-entry-node".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let entry = require_semantic_entry(
                &args[0],
                "ctfe-semantic-entry-node expects a resolved semantic entry",
            )?;
            let default = args.get(2).cloned().unwrap_or(RuntimeValue::Null);
            let Some(node_id) = entry.node_id else {
                return Ok(default);
            };
            let unit_object =
                require_unit_object(&args[1], "ctfe-semantic-entry-node expects a unit handle")?;
            let unit = unit_bridge_from_object(
                &unit_object,
                "ctfe-semantic-entry-node expects a unit handle",
            )?;
            if !unit.with_unit(|unit| unit.ir().node(node_id).is_some()) {
                return Ok(default);
            }
            Ok(RuntimeValue::HostObject(Rc::new(NodeBridgeValue::new(
                unit_object,
                node_id,
            ))))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-node-call-scope-descriptor".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-node-call-scope-descriptor expects a provider context",
            )?;
            let node_id = require_node_id(
                &args[1],
                "ctfe-provider-node-call-scope-descriptor expects a node",
            )?;
            ctx.track_node_read(node_id);
            ctx.unit().with_unit(|unit| {
                let Some(call) = call_node(unit, node_id)? else {
                    return Ok(RuntimeValue::Null);
                };
                let Some(callee) = callee_name(unit, call)? else {
                    return Ok(RuntimeValue::Null);
                };
                match callee.as_str() {
                    "lambda" => lambda_scope_descriptor(ctx, unit, call),
                    "bind" => bind_scope_descriptor(ctx, unit, call),
                    _ => Ok(RuntimeValue::Null),
                }
            })
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-node-call-control-descriptor".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-node-call-control-descriptor expects a provider context",
            )?;
            let node_id = require_node_id(
                &args[1],
                "ctfe-provider-node-call-control-descriptor expects a node",
            )?;
            ctx.track_node_read(node_id);
            ctx.unit().with_unit(|unit| {
                let Some(call) = call_node(unit, node_id)? else {
                    return Ok(RuntimeValue::Null);
                };
                let Some(callee) = callee_name(unit, call)? else {
                    return Ok(RuntimeValue::Null);
                };
                match callee.as_str() {
                    "block" => block_control_descriptor(ctx, call),
                    "leave" => leave_control_descriptor(ctx, unit, call),
                    _ => Ok(RuntimeValue::Null),
                }
            })
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-lookup-binding".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-lookup-binding expects a provider context",
            )?;
            let node_id = require_node_id(&args[1], "ctfe-provider-lookup-binding expects a node")?;
            let name = require_string(
                &args[2],
                "ctfe-provider-lookup-binding expects a binding name",
            )?;
            ctx.track_node_read(node_id);
            ctx.track_symbol_read(&name);
            let default = args.get(3).cloned();
            let report = ctx.unit().with_unit(|unit| {
                explain_binding_lookup(ctx, unit, node_id, &name, default.clone())
            })?;
            if report.found {
                return Ok(report.value.unwrap_or(RuntimeValue::Null));
            }
            match default {
                Some(value) => Ok(value),
                None => Err(eval_err(format!(
                    "ctfe-provider-lookup-binding could not resolve binding {name:?}"
                ))),
            }
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-explain-binding".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-explain-binding expects a provider context",
            )?;
            let node_id =
                require_node_id(&args[1], "ctfe-provider-explain-binding expects a node")?;
            let name = require_string(
                &args[2],
                "ctfe-provider-explain-binding expects a binding name",
            )?;
            ctx.track_node_read(node_id);
            ctx.track_symbol_read(&name);
            let default = args.get(3).cloned();
            ctx.unit()
                .with_unit(|unit| explain_binding_lookup(ctx, unit, node_id, &name, default))
                .map(binding_lookup_report_value)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-invoke-callback".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_special_impure(),
        min_arity: 2,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            require_provider_context(
                &args[0],
                "ctfe-provider-invoke-callback expects a provider context",
            )?;
            let callback = args[1].clone();
            ev.invoke_callback(&callback, args[2..].to_vec())
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-traversal-walk".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_special_impure(),
        min_arity: 3,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-traversal-walk expects a provider context",
            )?;
            let root_id =
                require_node_id(&args[1], "ctfe-provider-traversal-walk expects a root node")?;
            let callback = args[2].clone();
            let options = TraversalOptions::from_value(args.get(3))?;

            match options.mode {
                TraversalMode::Walk => {
                    let node_ids = collect_traversal_nodes(ctx, root_id, &options)?;
                    for node_id in node_ids {
                        ev.invoke_callback(&callback, vec![node_handle(ctx, node_id)])?;
                    }
                    Ok(RuntimeValue::Null)
                }
                TraversalMode::FindFirst => {
                    let node_ids = collect_traversal_nodes(ctx, root_id, &options)?;
                    for node_id in node_ids {
                        let handle = node_handle(ctx, node_id);
                        if is_truthy(&ev.invoke_callback(&callback, vec![handle.clone()])?) {
                            return Ok(handle);
                        }
                    }
                    Ok(RuntimeValue::Null)
                }
                TraversalMode::Filter => {
                    let node_ids = collect_traversal_nodes(ctx, root_id, &options)?;
                    let mut matches = Vec::new();
                    for node_id in node_ids {
                        let handle = node_handle(ctx, node_id);
                        if is_truthy(&ev.invoke_callback(&callback, vec![handle.clone()])?) {
                            matches.push(handle);
                        }
                    }
                    Ok(RuntimeValue::Tuple(matches.into()))
                }
                TraversalMode::Stateful => {
                    ctx.unit().with_unit(|unit| {
                        unit.ir().node(root_id).map(|_| ()).ok_or_else(|| {
                            eval_err("ctfe-provider-traversal-walk root node is missing")
                        })
                    })?;
                    let mut stack =
                        vec![(root_id, options.initial_state.unwrap_or(RuntimeValue::Null))];
                    while let Some((node_id, state)) = stack.pop() {
                        ctx.track_node_read(node_id);
                        let next =
                            ev.invoke_callback(&callback, vec![node_handle(ctx, node_id), state])?;
                        if matches!(next, RuntimeValue::Null) {
                            continue;
                        }
                        let child_states = traversal_child_states(&next)?;
                        for (child_id, child_state) in child_states.into_iter().rev() {
                            stack.push((child_id, child_state));
                        }
                    }
                    Ok(RuntimeValue::Null)
                }
            }
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-fact-get".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-fact-get expects a provider context",
            )?;
            require_provider_effect(ctx, "read_facts")?;
            let namespace = require_string(&args[1], "ctfe-provider-fact-get expects a namespace")?;
            let node_id = require_node_id(&args[2], "ctfe-provider-fact-get expects a node id")?;
            ctx.track_fact_read(node_id, &namespace);
            let default = args.get(3).cloned().unwrap_or(RuntimeValue::Null);
            ctx.unit().with_unit(|unit| {
                unit.semantics()
                    .get_fact(&node_subject_id(node_id), &namespace)
                    .map_err(eval_err)
                    .map(|value| {
                        value
                            .map(|value| semantic_value_to_runtime_in_context(ctx, value))
                            .unwrap_or(default)
                    })
            })
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-fact-set".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 4,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-fact-set expects a provider context",
            )?;
            require_provider_effect(ctx, "write_facts")?;
            let namespace = require_string(&args[1], "ctfe-provider-fact-set expects a namespace")?;
            let node_id = require_node_id(&args[2], "ctfe-provider-fact-set expects a node id")?;
            ctx.track_fact_write(node_id, &namespace);
            let value = runtime_to_semantic(&args[3])?;
            ctx.compiler()
                .validate_fact_value(&namespace, &value)
                .map_err(eval_err)?;
            ctx.unit()
                .with_unit_mut(|unit| {
                    unit.semantics_mut()
                        .set_fact(node_subject_id(node_id), namespace, value)
                })
                .map_err(eval_err)?;
            Ok(args[3].clone())
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-annotation-get".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-annotation-get expects a provider context",
            )?;
            require_provider_effect(ctx, "read_attributes")?;
            let node_id =
                require_node_id(&args[1], "ctfe-provider-annotation-get expects a node id")?;
            let key = require_string(
                &args[2],
                "ctfe-provider-annotation-get expects a non-empty key",
            )?;
            ctx.track_annotation_read(node_id, &key);
            let predicate = annotation_predicate(&key);
            let default = args.get(3).cloned().unwrap_or(RuntimeValue::Null);
            ctx.unit().with_unit(|unit| {
                unit.semantics()
                    .get_fact(&node_subject_id(node_id), &predicate)
                    .map_err(eval_err)
                    .map(|value| {
                        value
                            .map(|value| semantic_value_to_runtime_in_context(ctx, value))
                            .unwrap_or(default)
                    })
            })
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-annotation-set".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 4,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-annotation-set expects a provider context",
            )?;
            require_provider_effect(ctx, "write_attributes")?;
            let node_id =
                require_node_id(&args[1], "ctfe-provider-annotation-set expects a node id")?;
            let key = require_string(
                &args[2],
                "ctfe-provider-annotation-set expects a non-empty key",
            )?;
            ctx.track_annotation_write(node_id, &key);
            let value = runtime_to_semantic(&args[3])?;
            let predicate = annotation_predicate(&key);
            ctx.unit()
                .with_unit_mut(|unit| {
                    unit.semantics_mut()
                        .set_fact(node_subject_id(node_id), predicate, value)
                })
                .map_err(eval_err)?;
            Ok(args[3].clone())
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-call-callee-entry".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-call-callee-entry expects a provider context",
            )?;
            require_provider_effect(ctx, "read_facts")?;
            let node_id = require_node_id(
                &args[1],
                "ctfe-provider-call-callee-entry expects a call node",
            )?;
            ctx.unit().with_unit(|unit| {
                let Some(call) = call_node(unit, node_id)? else {
                    return Ok(RuntimeValue::Null);
                };
                let Some(entry) = callee_entry_for_call(ctx, unit, call)? else {
                    return Ok(RuntimeValue::Null);
                };
                Ok(semantic_entry_handle(entry))
            })
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-normalizable-entry?".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let entry = require_semantic_entry(
                &args[0],
                "ctfe-provider-normalizable-entry? expects a semantic entry",
            )?;
            Ok(RuntimeValue::Bool(should_normalize_ctfe(entry)))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-execute-ctfe-entry".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-execute-ctfe-entry expects a provider context",
            )?;
            let node_id = require_node_id(
                &args[1],
                "ctfe-provider-execute-ctfe-entry expects a call node",
            )?;
            let entry = require_semantic_entry(
                &args[2],
                "ctfe-provider-execute-ctfe-entry expects a semantic entry",
            )?;
            execute_ctfe_entry(ev, ctx, node_id, entry)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-materialize-ctfe-result".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_emit_diagnostics(),
        min_arity: 4,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-materialize-ctfe-result expects a provider context",
            )?;
            let node_id = require_node_id(
                &args[1],
                "ctfe-provider-materialize-ctfe-result expects a node",
            )?;
            let entry = require_semantic_entry(
                &args[2],
                "ctfe-provider-materialize-ctfe-result expects a semantic entry",
            )?;
            match materialize_ctfe_result(ctx, &args[3]) {
                Ok(Some(spec)) => Ok(RuntimeValue::HostObject(Rc::new(ExprSpecBridgeValue::new(
                    spec,
                )))),
                Ok(None) => Ok(RuntimeValue::Null),
                Err(signal) => {
                    emit_ctfe_error(ctx, node_id, entry, "unliftable_result", signal.to_string())?;
                    Ok(RuntimeValue::Null)
                }
            }
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-fold-safe?".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-fold-safe? expects a provider context",
            )?;
            let node_id = require_node_id(&args[1], "ctfe-provider-fold-safe? expects a node")?;
            ctx.unit()
                .with_unit(|unit| node_is_fold_safe(ctx, unit, node_id, &mut Vec::new()))
                .map(RuntimeValue::Bool)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-fold-compile-time-call".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_write_ir(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-fold-compile-time-call expects a provider context",
            )?;
            require_provider_effect(ctx, "write_ir")?;
            let node_id = require_node_id(
                &args[1],
                "ctfe-provider-fold-compile-time-call expects a call node",
            )?;
            fold_compile_time_call(ev, ctx, node_id)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-linkage-project".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 4,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-linkage-project expects a provider context",
            )?;
            let artifact_id = require_string(
                &args[1],
                "ctfe-provider-linkage-project expects an artifact id",
            )?;
            let links = link_binding_sequence(&args[2])?;
            let public_bindings = public_binding_sequence(&args[3])?;
            ctx.unit()
                .with_unit_mut(|unit| {
                    unit.set_unit_id(artifact_id)?;
                    for binding in links {
                        unit.add_link_binding(binding);
                    }
                    for (local_name, _) in public_bindings {
                        add_public_name(unit, local_name)?;
                    }
                    Ok::<(), String>(())
                })
                .map_err(eval_err)?;
            Ok(RuntimeValue::HostObject(ctx.unit()))
        }),
    });

    register_diagnostic_builtin(
        ev,
        "ctfe-provider-diagnostics-error",
        DiagnosticSeverity::Error,
    );
    register_diagnostic_builtin(
        ev,
        "ctfe-provider-diagnostics-warning",
        DiagnosticSeverity::Warning,
    );
    register_diagnostic_builtin(
        ev,
        "ctfe-provider-diagnostics-note",
        DiagnosticSeverity::Note,
    );
    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-diagnostics-suggest".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_emit_diagnostics(),
        min_arity: 6,
        max_arity: Some(8),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-diagnostics-suggest expects a provider context",
            )?;
            let severity = args
                .get(7)
                .map(|value| diagnostic_severity_value(value, "ctfe-provider-diagnostics-suggest severity must be error, warning, or note"))
                .transpose()?
                .unwrap_or(DiagnosticSeverity::Warning);
            let fix = DiagnosticFix::new(
                require_string(&args[4], "ctfe-provider-diagnostics-suggest expects a fix label")?,
                require_string(&args[5], "ctfe-provider-diagnostics-suggest expects a fix kind")?,
            )
            .map_err(eval_err)?
            .with_metadata(diagnostic_fix_metadata(args.get(6))?)
            .map_err(eval_err)?;
            let mut diagnostic = build_provider_diagnostic(
                ctx,
                severity,
                &args[1],
                &args[2],
                Some(&args[3]),
                None,
                "caap.provider.suggest",
            )?;
            diagnostic = diagnostic.add_fix(fix).map_err(eval_err)?;
            ctx.compiler().push_diagnostic(diagnostic);
            Ok(RuntimeValue::Null)
        }),
    });
    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-error".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_emit_diagnostics(),
        min_arity: 3,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-compiler-error expects a provider context",
            )?;
            emit_provider_diagnostic(
                ctx,
                DiagnosticSeverity::Error,
                &args[1],
                &args[2],
                args.get(3),
                None,
                "caap.macro.error",
            )
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-node-replace".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_write_ir(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-node-replace expects a provider context",
            )?;
            require_provider_effect(ctx, "write_ir")?;
            let node_id = require_node_id(&args[1], "ctfe-provider-node-replace expects a node")?;
            let spec = require_expr_spec(
                &args[2],
                "ctfe-provider-node-replace expects an expression spec replacement",
            )?;
            let new_id = ctx
                .unit()
                .with_unit_mut(|unit| {
                    let new_id = unit.replace_ir_subtree_with_spec(node_id, &spec)?;
                    record_provider_rewrite(ctx, unit, "replace", [new_id], [node_id])?;
                    Ok::<NodeId, String>(new_id)
                })
                .map_err(eval_err)?;
            Ok(node_handle(ctx, new_id))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-node-replace-many".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_write_ir(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-node-replace-many expects a provider context",
            )?;
            require_provider_effect(ctx, "write_ir")?;
            let node_id =
                require_node_id(&args[1], "ctfe-provider-node-replace-many expects a node")?;
            let specs = expr_spec_sequence(
                &args[2],
                "ctfe-provider-node-replace-many expects expression spec replacements",
            )?;
            let new_ids = ctx
                .unit()
                .with_unit_mut(|unit| {
                    let snapshot = unit.snapshot();
                    let result: Result<Option<NodeId>, String> = (|| {
                        if specs.is_empty() {
                            record_provider_erase(ctx, unit, node_id)?;
                            unit.erase_ir_subtree(node_id)?;
                            return Ok(None);
                        }
                        if specs.len() == 1 {
                            let new_id = unit.replace_ir_subtree_with_spec(node_id, &specs[0])?;
                            record_provider_rewrite(
                                ctx,
                                unit,
                                "replace_many",
                                [new_id],
                                [node_id],
                            )?;
                            return Ok(Some(new_id));
                        }
                        let sequence = ExprSpec::call(ExprSpec::name("do")?, specs.clone());
                        let new_id = unit.replace_ir_subtree_with_spec(node_id, &sequence)?;
                        let ids = replacement_record_ids(unit, new_id, "replace_many");
                        record_provider_rewrite(ctx, unit, "replace_many", ids, [node_id])?;
                        Ok(Some(new_id))
                    })();
                    if result.is_err() {
                        unit.restore_snapshot(snapshot);
                    }
                    result
                })
                .map_err(eval_err)?;
            Ok(new_ids
                .map(|id| node_handle(ctx, id))
                .unwrap_or(RuntimeValue::Null))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-node-wrap".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_write_ir(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-node-wrap expects a provider context",
            )?;
            require_provider_effect(ctx, "write_ir")?;
            let node_id = require_node_id(&args[1], "ctfe-provider-node-wrap expects a node")?;
            let callee = require_expr_spec(
                &args[2],
                "ctfe-provider-node-wrap expects an expression spec callee",
            )?;
            let new_id = ctx
                .unit()
                .with_unit_mut(|unit| {
                    let new_id = unit.wrap_ir_subtree_with_spec(node_id, &callee)?;
                    record_provider_rewrite(ctx, unit, "replace", [new_id], [node_id])?;
                    Ok::<NodeId, String>(new_id)
                })
                .map_err(eval_err)?;
            Ok(node_handle(ctx, new_id))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-node-insert-before".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_write_ir(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-node-insert-before expects a provider context",
            )?;
            require_provider_effect(ctx, "write_ir")?;
            let node_id =
                require_node_id(&args[1], "ctfe-provider-node-insert-before expects a node")?;
            let spec = require_expr_spec(
                &args[2],
                "ctfe-provider-node-insert-before expects an expression spec",
            )?;
            let new_id = ctx
                .unit()
                .with_unit_mut(|unit| {
                    let new_id = unit.insert_ir_before_with_spec(node_id, &spec)?;
                    record_provider_rewrite(ctx, unit, "insert_before", [new_id], [node_id])?;
                    Ok::<NodeId, String>(new_id)
                })
                .map_err(eval_err)?;
            Ok(node_handle(ctx, new_id))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-node-insert-after".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_write_ir(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-node-insert-after expects a provider context",
            )?;
            require_provider_effect(ctx, "write_ir")?;
            let node_id =
                require_node_id(&args[1], "ctfe-provider-node-insert-after expects a node")?;
            let spec = require_expr_spec(
                &args[2],
                "ctfe-provider-node-insert-after expects an expression spec",
            )?;
            let new_id = ctx
                .unit()
                .with_unit_mut(|unit| {
                    let new_id = unit.insert_ir_after_with_spec(node_id, &spec)?;
                    record_provider_rewrite(ctx, unit, "insert_after", [new_id], [node_id])?;
                    Ok::<NodeId, String>(new_id)
                })
                .map_err(eval_err)?;
            Ok(node_handle(ctx, new_id))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-provider-node-erase".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_write_ir(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-node-erase expects a provider context",
            )?;
            require_provider_effect(ctx, "write_ir")?;
            let node_id = require_node_id(&args[1], "ctfe-provider-node-erase expects a node")?;
            ctx.unit()
                .with_unit_mut(|unit| {
                    record_provider_erase(ctx, unit, node_id)?;
                    unit.erase_ir_subtree(node_id)
                })
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });
}

fn register_diagnostic_builtin(
    ev: &mut Evaluator,
    name: &'static str,
    severity: DiagnosticSeverity,
) {
    ev.register_builtin(BuiltinInfo {
        name: name.to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_emit_diagnostics(),
        min_arity: 3,
        max_arity: Some(5),
        eager_handler: None,
        handler: Box::new(move |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "provider diagnostic expects a provider context",
            )?;
            emit_provider_diagnostic(
                ctx,
                severity,
                &args[1],
                &args[2],
                args.get(3),
                args.get(4),
                "caap.provider.diagnostic",
            )
        }),
    });
}

fn record_provider_rewrite(
    ctx: &ProviderContextBridgeValue,
    unit: &mut Unit,
    operation: &str,
    node_ids: impl IntoIterator<Item = NodeId>,
    sources: impl IntoIterator<Item = NodeId>,
) -> Result<(), String> {
    let context = ctx.context();
    let node_ids: Vec<NodeId> = node_ids.into_iter().collect();
    let sources: Vec<NodeId> = sources.into_iter().collect();
    for source in &sources {
        ctx.track_node_read(*source);
        ctx.track_node_write(*source);
    }
    for node_id in &node_ids {
        ctx.track_node_write(*node_id);
    }
    unit.record_rewrite_provenance(
        context.provider.clone(),
        context.stage.clone(),
        context.family.clone(),
        operation.to_string(),
        node_ids,
        sources,
    )?;
    Ok(())
}

fn replacement_record_ids(unit: &Unit, new_id: NodeId, operation: &str) -> Vec<NodeId> {
    if operation != "replace_many" {
        return vec![new_id];
    }
    let Some(Node::Call(call)) = unit.ir().node(new_id) else {
        return vec![new_id];
    };
    let Some(Node::Name(callee)) = unit.ir().node(call.callee) else {
        return vec![new_id];
    };
    if callee.identifier.as_ref() != "do" {
        return vec![new_id];
    }
    let mut ids = Vec::with_capacity(call.args.len() + 1);
    ids.push(new_id);
    ids.extend(call.args.iter().copied());
    ids
}

fn record_provider_erase(
    ctx: &ProviderContextBridgeValue,
    unit: &mut Unit,
    node_id: NodeId,
) -> Result<(), String> {
    let context = ctx.context();
    ctx.track_node_write(node_id);
    unit.record_erase_rewrite_tombstones(
        context.provider.clone(),
        context.stage.clone(),
        context.family.clone(),
        node_id,
    )?;
    Ok(())
}

fn emit_provider_diagnostic(
    ctx: &ProviderContextBridgeValue,
    severity: DiagnosticSeverity,
    node_value: &RuntimeValue,
    message_value: &RuntimeValue,
    code_value: Option<&RuntimeValue>,
    notes_value: Option<&RuntimeValue>,
    default_code: &str,
) -> Result<RuntimeValue, EvalSignal> {
    let diagnostic = build_provider_diagnostic(
        ctx,
        severity,
        node_value,
        message_value,
        code_value,
        notes_value,
        default_code,
    )?;
    ctx.compiler().push_diagnostic(diagnostic);
    Ok(RuntimeValue::Null)
}

fn build_provider_diagnostic(
    ctx: &ProviderContextBridgeValue,
    severity: DiagnosticSeverity,
    node_value: &RuntimeValue,
    message_value: &RuntimeValue,
    code_value: Option<&RuntimeValue>,
    notes_value: Option<&RuntimeValue>,
    default_code: &str,
) -> Result<Diagnostic, EvalSignal> {
    let node_id = require_node_id(node_value, "provider diagnostic expects a node id")?;
    let message = require_string(message_value, "provider diagnostic expects a message")?;
    let code = code_value
        .map(|value| require_string(value, "provider diagnostic expects a diagnostic code"))
        .transpose()?
        .unwrap_or_else(|| default_code.to_string());
    let mut diagnostic = Diagnostic::new(severity, message).map_err(eval_err)?;
    diagnostic.code = Some(code);
    diagnostic.span = ctx
        .unit()
        .with_unit(|unit| unit.ir().source_span(node_id).cloned());
    for note in diagnostic_notes(notes_value)? {
        diagnostic = diagnostic.add_note(note).map_err(eval_err)?;
    }
    Ok(diagnostic)
}

fn diagnostic_severity_value(
    value: &RuntimeValue,
    message: &str,
) -> Result<DiagnosticSeverity, EvalSignal> {
    match require_string(value, message)?.as_str() {
        "error" => Ok(DiagnosticSeverity::Error),
        "warning" => Ok(DiagnosticSeverity::Warning),
        "note" => Ok(DiagnosticSeverity::Note),
        _ => Err(eval_err(message)),
    }
}

fn diagnostic_notes(value: Option<&RuntimeValue>) -> Result<Vec<String>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(Vec::new()),
        Some(RuntimeValue::Str(text)) => Ok(vec![text.to_string()]),
        Some(RuntimeValue::Tuple(items)) => items
            .iter()
            .map(|value| require_string(value, "ctfe-provider-diagnostics notes expect strings"))
            .collect(),
        Some(RuntimeValue::List(items)) => items
            .borrow()
            .iter()
            .map(|value| require_string(value, "ctfe-provider-diagnostics notes expect strings"))
            .collect(),
        Some(_) => Err(eval_err(
            "ctfe-provider-diagnostics notes expect a string or sequence of strings",
        )),
    }
}

fn diagnostic_fix_metadata(
    value: Option<&RuntimeValue>,
) -> Result<Vec<(String, String)>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(Vec::new()),
        Some(RuntimeValue::Map(map)) => {
            let mut metadata = Vec::new();
            for (key, value) in map.borrow().iter() {
                let MapKey::Str(key) = key else {
                    return Err(eval_err("diagnostic fix metadata expects string keys"));
                };
                metadata.push((key.to_string(), value.to_string()));
            }
            metadata.sort_by(|left, right| left.0.cmp(&right.0));
            Ok(metadata)
        }
        Some(_) => Err(eval_err("diagnostic fix metadata expects a map")),
    }
}

fn require_provider_context<'a>(
    value: &'a RuntimeValue,
    message: &str,
) -> Result<&'a ProviderContextBridgeValue, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    object
        .as_any()
        .downcast_ref::<ProviderContextBridgeValue>()
        .ok_or_else(|| eval_err(message))
}

fn require_provider_effect(
    ctx: &ProviderContextBridgeValue,
    effect: &str,
) -> Result<(), EvalSignal> {
    if ctx.context().effect_tags.iter().any(|tag| tag == effect) {
        Ok(())
    } else {
        Err(eval_err(format!(
            "provider {} does not declare required effect {effect}",
            ctx.context().provider
        )))
    }
}

fn require_resolution_scope<'a>(
    value: &'a RuntimeValue,
    message: &str,
) -> Result<&'a ResolutionScopeBridgeValue, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    object
        .as_any()
        .downcast_ref::<ResolutionScopeBridgeValue>()
        .ok_or_else(|| eval_err(message))
}

fn require_semantic_entry<'a>(
    value: &'a RuntimeValue,
    message: &str,
) -> Result<&'a SemanticEntry, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    object
        .as_any()
        .downcast_ref::<SemanticEntryBridgeValue>()
        .map(SemanticEntryBridgeValue::entry)
        .ok_or_else(|| eval_err(message))
}

fn require_unit_object(
    value: &RuntimeValue,
    message: &str,
) -> Result<Rc<dyn HostObject>, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    if object.as_any().downcast_ref::<UnitBridgeValue>().is_none() {
        return Err(eval_err(message));
    }
    Ok(Rc::clone(object))
}

fn unit_bridge_from_object<'a>(
    object: &'a Rc<dyn HostObject>,
    message: &str,
) -> Result<&'a UnitBridgeValue, EvalSignal> {
    object
        .as_any()
        .downcast_ref::<UnitBridgeValue>()
        .ok_or_else(|| eval_err(message))
}

fn require_string(value: &RuntimeValue, message: &str) -> Result<String, EvalSignal> {
    let RuntimeValue::Str(text) = value else {
        return Err(eval_err(message));
    };
    if text.is_empty() {
        return Err(eval_err(message));
    }
    Ok(text.to_string())
}

fn require_unit_bridge<'a>(
    value: &'a RuntimeValue,
    message: &str,
) -> Result<&'a UnitBridgeValue, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    object
        .as_any()
        .downcast_ref::<UnitBridgeValue>()
        .ok_or_else(|| eval_err(message))
}

fn provider_query_artifact_source(value: &RuntimeValue) -> Result<QueryArtifactSource, EvalSignal> {
    if let RuntimeValue::Str(path) = value {
        return Ok(QueryArtifactSource::Path(path.to_string()));
    }
    let unit = require_unit_bridge(
        value,
        "ctfe-provider-query-artifact expects a unit handle or path-like source",
    )?;
    Ok(QueryArtifactSource::Unit(Box::new(unit.clone_unit())))
}

fn provider_initial_bindings(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Vec<(String, RuntimeValue)>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(Vec::new()),
        Some(RuntimeValue::Map(map)) => map
            .borrow()
            .iter()
            .map(|(key, value)| match key {
                MapKey::Str(name) if !name.is_empty() => Ok((name.to_string(), value.clone())),
                _ => Err(eval_err(message)),
            })
            .collect(),
        Some(_) => Err(eval_err(message)),
    }
}

fn provider_resolve_path(path: &str, base: Option<&str>) -> Result<PathBuf, EvalSignal> {
    let candidate = PathBuf::from(path);
    if candidate.exists() {
        return candidate
            .canonicalize()
            .map_err(|error| eval_err(format!("ctfe-provider-resolve-path: {error}")));
    }
    if !candidate.is_absolute() {
        if let Some(base) = base {
            let base_path = PathBuf::from(base);
            let base_dir = if base_path.is_file() {
                base_path.parent().unwrap_or(Path::new(".")).to_path_buf()
            } else {
                base_path
            };
            let resolved = base_dir.join(&candidate);
            if resolved.exists() {
                return resolved
                    .canonicalize()
                    .map_err(|error| eval_err(format!("ctfe-provider-resolve-path: {error}")));
            }
        }
        return std::env::current_dir()
            .map(|cwd| cwd.join(candidate))
            .map_err(|error| eval_err(format!("ctfe-provider-resolve-path: {error}")));
    }
    Ok(candidate)
}

fn path_to_string(path: &Path) -> Result<String, EvalSignal> {
    path.to_str()
        .map(ToString::to_string)
        .ok_or_else(|| eval_err("path is not valid UTF-8"))
}

fn annotation_predicate(key: &str) -> String {
    format!("annotation.{key}")
}

fn require_node_id(value: &RuntimeValue, message: &str) -> Result<NodeId, EvalSignal> {
    match value {
        RuntimeValue::Int(value) if *value >= 0 => Ok(*value as NodeId),
        RuntimeValue::HostObject(object) => object
            .as_any()
            .downcast_ref::<NodeBridgeValue>()
            .map(NodeBridgeValue::node_id)
            .ok_or_else(|| eval_err(message)),
        _ => Err(eval_err(message)),
    }
}

fn require_expr_spec(value: &RuntimeValue, message: &str) -> Result<ExprSpec, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    object
        .as_any()
        .downcast_ref::<ExprSpecBridgeValue>()
        .map(ExprSpecBridgeValue::spec)
        .ok_or_else(|| eval_err(message))
}

fn expr_spec_sequence(value: &RuntimeValue, message: &str) -> Result<Vec<ExprSpec>, EvalSignal> {
    let values: Vec<RuntimeValue> = match value {
        RuntimeValue::Tuple(items) => items.iter().cloned().collect(),
        RuntimeValue::List(items) => items.borrow().iter().cloned().collect(),
        _ => return Err(eval_err(message)),
    };
    values
        .iter()
        .map(|value| require_expr_spec(value, message))
        .collect()
}

fn node_handle(ctx: &ProviderContextBridgeValue, node_id: NodeId) -> RuntimeValue {
    let unit: Rc<dyn HostObject> = ctx.unit();
    RuntimeValue::HostObject(Rc::new(NodeBridgeValue::new(unit, node_id)))
}

fn semantic_entry_handle(entry: SemanticEntry) -> RuntimeValue {
    RuntimeValue::HostObject(Rc::new(SemanticEntryBridgeValue::new(entry)))
}

fn link_binding_sequence(value: &RuntimeValue) -> Result<Vec<LinkBinding>, EvalSignal> {
    runtime_sequence_values(
        value,
        "ctfe-provider-linkage-project expects a sequence of link bindings",
    )?
    .iter()
    .map(link_binding_value)
    .collect()
}

fn link_binding_value(value: &RuntimeValue) -> Result<LinkBinding, EvalSignal> {
    match value {
        RuntimeValue::Tuple(items) if items.len() == 3 || items.len() == 4 => {
            let syntax = items
                .get(3)
                .map(|value| match value {
                    RuntimeValue::Bool(value) => Ok(*value),
                    _ => Err(eval_err("link binding syntax flag must be a bool")),
                })
                .transpose()?
                .unwrap_or(false);
            LinkBinding::with_syntax(
                require_string(&items[0], "link binding source unit must be a string")?,
                require_string(&items[1], "link binding source name must be a string")?,
                require_string(&items[2], "link binding local name must be a string")?,
                syntax,
            )
            .map_err(eval_err)
        }
        RuntimeValue::List(items) => {
            let items = items.borrow();
            if items.len() != 3 && items.len() != 4 {
                return Err(eval_err("link binding sequence must have 3 or 4 items"));
            }
            let syntax = items
                .get(3)
                .map(|value| match value {
                    RuntimeValue::Bool(value) => Ok(*value),
                    _ => Err(eval_err("link binding syntax flag must be a bool")),
                })
                .transpose()?
                .unwrap_or(false);
            LinkBinding::with_syntax(
                require_string(&items[0], "link binding source unit must be a string")?,
                require_string(&items[1], "link binding source name must be a string")?,
                require_string(&items[2], "link binding local name must be a string")?,
                syntax,
            )
            .map_err(eval_err)
        }
        RuntimeValue::Map(map) => {
            let map = map.borrow();
            let syntax = match map.get(&MapKey::Str("syntax".into())) {
                None | Some(RuntimeValue::Null) => false,
                Some(RuntimeValue::Bool(value)) => *value,
                Some(_) => return Err(eval_err("link binding syntax flag must be a bool")),
            };
            LinkBinding::with_syntax(
                required_map_string(
                    &map,
                    "source_unit",
                    "link binding source_unit must be a string",
                )?,
                required_map_string(
                    &map,
                    "source_name",
                    "link binding source_name must be a string",
                )?,
                required_map_string(
                    &map,
                    "local_name",
                    "link binding local_name must be a string",
                )?,
                syntax,
            )
            .map_err(eval_err)
        }
        _ => Err(eval_err(
            "ctfe-provider-linkage-project link binding must be a tuple, list, or map",
        )),
    }
}

fn public_binding_sequence(value: &RuntimeValue) -> Result<Vec<(String, String)>, EvalSignal> {
    runtime_sequence_values(
        value,
        "ctfe-provider-linkage-project expects a sequence of public bindings",
    )?
    .iter()
    .map(public_binding_value)
    .collect()
}

fn public_binding_value(value: &RuntimeValue) -> Result<(String, String), EvalSignal> {
    match value {
        RuntimeValue::Tuple(items) if items.len() == 2 => Ok((
            require_string(&items[0], "public binding local name must be a string")?,
            require_string(&items[1], "public binding public name must be a string")?,
        )),
        RuntimeValue::List(items) if items.borrow().len() == 2 => {
            let items = items.borrow();
            Ok((
                require_string(&items[0], "public binding local name must be a string")?,
                require_string(&items[1], "public binding public name must be a string")?,
            ))
        }
        RuntimeValue::Map(map) => {
            let map = map.borrow();
            Ok((
                required_map_string(
                    &map,
                    "local_name",
                    "public binding local_name must be a string",
                )?,
                required_map_string(
                    &map,
                    "public_name",
                    "public binding public_name must be a string",
                )?,
            ))
        }
        _ => Err(eval_err(
            "ctfe-provider-linkage-project public binding must be a pair or map",
        )),
    }
}

fn required_map_string(
    map: &std::collections::HashMap<MapKey, RuntimeValue>,
    key: &str,
    message: &str,
) -> Result<String, EvalSignal> {
    let value = map
        .get(&MapKey::Str(key.into()))
        .ok_or_else(|| eval_err(message))?;
    require_string(value, message)
}

fn add_public_name(unit: &mut Unit, name: String) -> Result<(), String> {
    let existing = unit.semantics().lookup_symbol(&name)?.cloned();
    let mut entry = existing.unwrap_or(SymbolEntry::new(
        name.clone(),
        SymbolKind::TopLevel,
        PhasePolicy::Runtime,
        None,
    )?);
    if !entry.public_names.iter().any(|existing| existing == &name) {
        entry.public_names.push(name.clone());
        entry.public_names.sort();
    }
    entry.public = true;
    unit.semantics_mut().define_symbol(entry);
    let public_fact = SemanticValue::map([
        ("name".to_string(), SemanticValue::Str(name.clone())),
        ("public".to_string(), SemanticValue::Bool(true)),
    ])?;
    unit.semantics_mut()
        .set_fact(symbol_subject_id(name)?, "symbol.entry", public_fact)?;
    Ok(())
}

fn call_node(unit: &Unit, node_id: NodeId) -> Result<Option<&CallNode>, EvalSignal> {
    match unit.ir().node(node_id) {
        Some(Node::Call(call)) => Ok(Some(call)),
        Some(_) => Ok(None),
        None => Err(eval_err("provider call descriptor node is missing")),
    }
}

fn callee_name(unit: &Unit, call: &CallNode) -> Result<Option<String>, EvalSignal> {
    match unit.ir().node(call.callee) {
        Some(Node::Name(name)) => Ok(Some(name.identifier.to_string())),
        Some(_) => Ok(None),
        None => Err(eval_err("provider call descriptor callee node is missing")),
    }
}

fn lambda_scope_descriptor(
    ctx: &ProviderContextBridgeValue,
    unit: &Unit,
    call: &CallNode,
) -> Result<RuntimeValue, EvalSignal> {
    if call.args.is_empty() {
        return Ok(RuntimeValue::Null);
    }
    let params = lambda_param_names(unit, call.args[0])?;
    let bindings = params
        .into_iter()
        .map(|name| scope_binding_value(ctx, name, "parameter", Some(call.id)))
        .collect();
    Ok(scope_descriptor_value(
        bindings,
        Vec::new(),
        call.args[1..].to_vec(),
        false,
        false,
        "closure",
        ctx,
    ))
}

fn lambda_param_names(unit: &Unit, params_id: NodeId) -> Result<Vec<String>, EvalSignal> {
    match unit.ir().node(params_id) {
        Some(Node::Call(params)) => unit_call_item_ids(unit, params)
            .into_iter()
            .map(|param_id| match unit.ir().node(param_id) {
                Some(Node::Name(name)) => Ok(name.identifier.to_string()),
                _ => Err(eval_err("lambda scope descriptor params must be names")),
            })
            .collect(),
        Some(Node::Literal(literal)) if matches!(literal.value, IrLiteralData::Null) => {
            Ok(Vec::new())
        }
        Some(Node::Literal(literal)) => match &literal.value {
            IrLiteralData::Tuple(items) => items
                .iter()
                .map(|item| match item {
                    IrLiteralData::Str(name) if !name.is_empty() => Ok(name.clone()),
                    _ => Err(eval_err(
                        "lambda scope descriptor params tuple must contain names",
                    )),
                })
                .collect(),
            _ => Err(eval_err(
                "lambda scope descriptor params must be a params call, tuple, or null",
            )),
        },
        Some(Node::Name(_)) => Ok(Vec::new()),
        None => Err(eval_err("lambda scope descriptor params node is missing")),
    }
}

fn bind_scope_descriptor(
    ctx: &ProviderContextBridgeValue,
    unit: &Unit,
    call: &CallNode,
) -> Result<RuntimeValue, EvalSignal> {
    if call.args.is_empty() {
        return Ok(RuntimeValue::Null);
    }
    if let Some((bindings, value_ids, body_ids)) = flat_literal_bind_scope_parts(ctx, unit, call)? {
        return Ok(scope_descriptor_value(
            bindings,
            value_ids,
            body_ids,
            true,
            true,
            "scoped_eval",
            ctx,
        ));
    }
    match unit.ir().node(call.args[0]) {
        Some(Node::Name(name)) => {
            if call.args.len() < 2 {
                return Ok(RuntimeValue::Null);
            }
            let value_id = call.args[1];
            Ok(scope_descriptor_value(
                vec![scope_binding_value(
                    ctx,
                    name.identifier.to_string(),
                    "local",
                    Some(value_id),
                )],
                vec![value_id],
                call.args[2..].to_vec(),
                false,
                true,
                "scoped_eval",
                ctx,
            ))
        }
        Some(Node::Call(bindings_node)) => {
            let mut bindings = Vec::new();
            let mut value_ids = Vec::new();
            for pair_id in unit_call_item_ids(unit, bindings_node) {
                let pair = match unit.ir().node(pair_id) {
                    Some(Node::Call(pair)) => pair,
                    _ => {
                        return Err(eval_err(
                            "bind scope descriptor binding pair must be a call",
                        ))
                    }
                };
                let pair_items = unit_call_item_ids(unit, pair);
                if pair_items.len() != 2 {
                    return Err(eval_err(
                        "bind scope descriptor binding pair must contain name and value",
                    ));
                }
                let name = match unit.ir().node(pair_items[0]) {
                    Some(Node::Name(name)) => name.identifier.to_string(),
                    _ => {
                        return Err(eval_err(
                            "bind scope descriptor binding name must be a name",
                        ))
                    }
                };
                bindings.push(scope_binding_value(ctx, name, "local", Some(pair_items[1])));
                value_ids.push(pair_items[1]);
            }
            Ok(scope_descriptor_value(
                bindings,
                value_ids,
                call.args[1..].to_vec(),
                true,
                true,
                "scoped_eval",
                ctx,
            ))
        }
        Some(Node::Literal(_)) => Ok(scope_descriptor_value(
            Vec::new(),
            Vec::new(),
            call.args[1..].to_vec(),
            false,
            false,
            "scoped_eval",
            ctx,
        )),
        None => Err(eval_err("bind scope descriptor bindings node is missing")),
    }
}

fn flat_literal_bind_scope_parts(
    ctx: &ProviderContextBridgeValue,
    unit: &Unit,
    call: &CallNode,
) -> Result<Option<(Vec<RuntimeValue>, Vec<NodeId>, Vec<NodeId>)>, EvalSignal> {
    let Some((pairs, body_ids)) = flat_literal_bind_parts(unit, call)? else {
        return Ok(None);
    };
    let mut bindings = Vec::new();
    let mut value_ids = Vec::new();
    for (name, value_id) in pairs {
        bindings.push(scope_binding_value(ctx, name, "local", Some(value_id)));
        value_ids.push(value_id);
    }
    Ok(Some((bindings, value_ids, body_ids)))
}

fn scope_descriptor_value(
    bindings: Vec<RuntimeValue>,
    binding_value_ids: Vec<NodeId>,
    body_ids: Vec<NodeId>,
    binding_values_use_child_scope: bool,
    exports_to_parent_scope: bool,
    result_kind: &str,
    ctx: &ProviderContextBridgeValue,
) -> RuntimeValue {
    rt_map([
        ("bindings", rt_tuple(bindings)),
        (
            "binding_value_ids",
            rt_tuple(
                binding_value_ids
                    .iter()
                    .map(|id| RuntimeValue::Int(*id as i64))
                    .collect(),
            ),
        ),
        (
            "binding_values",
            rt_tuple(
                binding_value_ids
                    .iter()
                    .map(|id| node_handle(ctx, *id))
                    .collect(),
            ),
        ),
        (
            "body_ids",
            rt_tuple(
                body_ids
                    .iter()
                    .map(|id| RuntimeValue::Int(*id as i64))
                    .collect(),
            ),
        ),
        (
            "bodies",
            rt_tuple(body_ids.iter().map(|id| node_handle(ctx, *id)).collect()),
        ),
        (
            "binding_values_use_child_scope",
            RuntimeValue::Bool(binding_values_use_child_scope),
        ),
        (
            "exports_to_parent_scope",
            RuntimeValue::Bool(exports_to_parent_scope),
        ),
        ("result_kind", rt_string(result_kind)),
    ])
}

fn scope_binding_value(
    ctx: &ProviderContextBridgeValue,
    name: String,
    kind: &str,
    node_id: Option<NodeId>,
) -> RuntimeValue {
    rt_map([
        ("name", RuntimeValue::Str(name.into())),
        ("kind", rt_string(kind)),
        (
            "node_id",
            node_id
                .map(|id| RuntimeValue::Int(id as i64))
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "node",
            node_id
                .map(|id| node_handle(ctx, id))
                .unwrap_or(RuntimeValue::Null),
        ),
    ])
}

fn block_control_descriptor(
    ctx: &ProviderContextBridgeValue,
    call: &CallNode,
) -> Result<RuntimeValue, EvalSignal> {
    Ok(control_descriptor_value(
        ctx,
        "block",
        RuntimeValue::Null,
        call.args.clone(),
    ))
}

fn leave_control_descriptor(
    ctx: &ProviderContextBridgeValue,
    unit: &Unit,
    call: &CallNode,
) -> Result<RuntimeValue, EvalSignal> {
    let label = match call.args.first().and_then(|id| unit.ir().node(*id)) {
        Some(Node::Literal(literal)) => match &literal.value {
            IrLiteralData::Int(value) => RuntimeValue::Int(*value),
            _ => RuntimeValue::Null,
        },
        _ => RuntimeValue::Null,
    };
    Ok(control_descriptor_value(
        ctx,
        "leave",
        label,
        call.args.iter().skip(1).copied().collect(),
    ))
}

fn control_descriptor_value(
    ctx: &ProviderContextBridgeValue,
    kind: &str,
    label: RuntimeValue,
    value_ids: Vec<NodeId>,
) -> RuntimeValue {
    rt_map([
        ("kind", rt_string(kind)),
        ("label", label),
        (
            "value_ids",
            rt_tuple(
                value_ids
                    .iter()
                    .map(|id| RuntimeValue::Int(*id as i64))
                    .collect(),
            ),
        ),
        (
            "values",
            rt_tuple(value_ids.iter().map(|id| node_handle(ctx, *id)).collect()),
        ),
    ])
}

#[derive(Clone, Debug)]
struct BindingLookupReport {
    reconstructed: bool,
    found: bool,
    reason: Option<String>,
    value: Option<RuntimeValue>,
}

fn explain_binding_lookup(
    ctx: &ProviderContextBridgeValue,
    unit: &Unit,
    node_id: NodeId,
    name: &str,
    default: Option<RuntimeValue>,
) -> Result<BindingLookupReport, EvalSignal> {
    let default_value = default.clone();
    let Some(env) = reconstruct_binding_env(ctx, unit, node_id)? else {
        return Ok(BindingLookupReport {
            reconstructed: false,
            found: false,
            reason: Some("environment_unavailable".to_string()),
            value: default_value,
        });
    };
    match Environment::lookup(&env, name) {
        Ok(value) => Ok(BindingLookupReport {
            reconstructed: true,
            found: true,
            reason: None,
            value: Some(value),
        }),
        Err(_) => Ok(BindingLookupReport {
            reconstructed: true,
            found: false,
            reason: Some("binding_missing".to_string()),
            value: default_value,
        }),
    }
}

fn reconstruct_binding_env(
    ctx: &ProviderContextBridgeValue,
    unit: &Unit,
    node_id: NodeId,
) -> Result<Option<EnvRef>, EvalSignal> {
    let Some(mut path) = path_from_root_to_node(unit, node_id) else {
        return Ok(None);
    };
    let env = Environment::new(None);
    for (name, value) in ctx.initial_bindings() {
        Environment::define(
            &env,
            name.clone(),
            contextualize_runtime_value(ctx, value, 0),
        );
    }
    Environment::define(&env, "compiler", RuntimeValue::HostObject(ctx.compiler()));
    Environment::define(&env, "unit", RuntimeValue::HostObject(ctx.unit()));
    if path.len() <= 1 {
        return Ok(Some(env));
    }
    let mut evaluator = Evaluator::new(unit.ir().clone());
    for index in 0..path.len() - 1 {
        let parent_id = path[index];
        let child_id = path[index + 1];
        let Some(Node::Call(call)) = unit.ir().node(parent_id) else {
            continue;
        };
        let Some(callee) = callee_name(unit, call)? else {
            continue;
        };
        match callee.as_str() {
            "bind" => reconstruct_bind_env(unit, &mut evaluator, &env, call, child_id)?,
            "lambda" => reconstruct_lambda_env(unit, &env, call, child_id)?,
            _ => {}
        }
    }
    path.clear();
    Ok(Some(env))
}

fn path_from_root_to_node(unit: &Unit, node_id: NodeId) -> Option<Vec<NodeId>> {
    unit.ir().node(node_id)?;
    let mut path = vec![node_id];
    let mut current = node_id;
    loop {
        match unit.ir().parent(current)? {
            Some(parent) => {
                current = parent;
                path.push(current);
            }
            None => {
                path.reverse();
                return Some(path);
            }
        }
    }
}

fn reconstruct_bind_env(
    unit: &Unit,
    evaluator: &mut Evaluator,
    env: &EnvRef,
    call: &CallNode,
    child_id: NodeId,
) -> Result<(), EvalSignal> {
    if call.args.is_empty() {
        return Ok(());
    }
    if let Some((pairs, body_ids)) = flat_literal_bind_parts(unit, call)? {
        if !body_ids.contains(&child_id) && !pairs.iter().any(|(_, value_id)| *value_id == child_id)
        {
            return Ok(());
        }
        for (name, _) in &pairs {
            Environment::define(env, name.clone(), RuntimeValue::Null);
        }
        for (name, value_id) in pairs {
            if child_id == value_id {
                break;
            }
            let value = evaluator.eval(value_id, env)?;
            Environment::define(env, name, value);
        }
        return Ok(());
    }
    match unit.ir().node(call.args[0]) {
        Some(Node::Name(name)) => {
            if call.args.len() < 2
                || child_id == call.args[1]
                || !call.args[2..].contains(&child_id)
            {
                return Ok(());
            }
            let value = evaluator.eval(call.args[1], env)?;
            Environment::define(env, name.identifier.to_string(), value);
            Ok(())
        }
        Some(Node::Call(bindings_node)) => {
            let body_contains_child = call.args[1..].contains(&child_id);
            let mut pairs = Vec::new();
            for pair_id in unit_call_item_ids(unit, bindings_node) {
                let pair = match unit.ir().node(pair_id) {
                    Some(Node::Call(pair)) => pair,
                    _ => {
                        return Err(eval_err(
                            "bind binding reconstruction expects binding pairs",
                        ))
                    }
                };
                let pair_items = unit_call_item_ids(unit, pair);
                if pair_items.len() != 2 {
                    return Err(eval_err(
                        "bind binding reconstruction expects binding pairs",
                    ));
                }
                let name = match unit.ir().node(pair_items[0]) {
                    Some(Node::Name(name)) => name.identifier.to_string(),
                    _ => return Err(eval_err("bind binding reconstruction expects names")),
                };
                pairs.push((name, pair_items[1]));
            }
            for (name, _) in &pairs {
                Environment::define(env, name.clone(), RuntimeValue::Null);
            }
            for (name, value_id) in pairs {
                if child_id == value_id {
                    break;
                }
                let value = evaluator.eval(value_id, env)?;
                Environment::define(env, name, value);
                if !body_contains_child && child_id == value_id {
                    break;
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn flat_literal_bind_parts(
    unit: &Unit,
    call: &CallNode,
) -> Result<Option<(Vec<(String, NodeId)>, Vec<NodeId>)>, EvalSignal> {
    if call.args.len() < 3 || !(call.args.len() - 1).is_multiple_of(2) {
        return Ok(None);
    }
    let mut pairs = Vec::new();
    for pair in call.args[..call.args.len() - 1].chunks_exact(2) {
        let name = match unit.ir().node(pair[0]) {
            Some(Node::Literal(literal)) => match &literal.value {
                IrLiteralData::Str(name) if !name.is_empty() => name.clone(),
                _ => return Ok(None),
            },
            _ => return Ok(None),
        };
        pairs.push((name, pair[1]));
    }
    Ok(Some((
        pairs,
        vec![*call.args.last().expect("checked non-empty bind args")],
    )))
}

fn reconstruct_lambda_env(
    unit: &Unit,
    env: &EnvRef,
    call: &CallNode,
    child_id: NodeId,
) -> Result<(), EvalSignal> {
    if call.args.len() < 2 || !call.args[1..].contains(&child_id) {
        return Ok(());
    }
    for name in lambda_param_names(unit, call.args[0])? {
        Environment::define(env, name, RuntimeValue::UninitializedTopLevel);
    }
    Ok(())
}

fn unit_call_item_ids(unit: &Unit, call: &CallNode) -> Vec<NodeId> {
    match unit.ir().node(call.callee) {
        Some(Node::Name(name)) if is_synthetic_group_callee(&name.identifier) => call.args.clone(),
        _ => {
            let mut ids = Vec::with_capacity(call.args.len() + 1);
            ids.push(call.callee);
            ids.extend_from_slice(&call.args);
            ids
        }
    }
}

fn is_synthetic_group_callee(identifier: &str) -> bool {
    identifier.starts_with("__") && identifier.ends_with("__")
}

fn binding_lookup_report_value(report: BindingLookupReport) -> RuntimeValue {
    rt_map([
        ("reconstructed", RuntimeValue::Bool(report.reconstructed)),
        ("found", RuntimeValue::Bool(report.found)),
        (
            "reason",
            report
                .reason
                .map(|reason| RuntimeValue::Str(reason.into()))
                .unwrap_or(RuntimeValue::Null),
        ),
        ("value", report.value.unwrap_or(RuntimeValue::Null)),
    ])
}

fn callee_entry_for_call(
    ctx: &ProviderContextBridgeValue,
    unit: &Unit,
    call: &CallNode,
) -> Result<Option<SemanticEntry>, EvalSignal> {
    let Some(Node::Name(callee)) = unit.ir().node(call.callee) else {
        return Ok(None);
    };
    if let Some(entry) = unit
        .semantics()
        .get_fact(&node_subject_id(call.callee), "caap.fact.resolved_name")
        .map_err(eval_err)?
        .and_then(resolved_name_fact_entry)
        .transpose()?
    {
        return Ok(Some(entry));
    }
    if let Some(entry) = unit
        .semantics()
        .lookup_semantic(&callee.identifier)
        .map_err(eval_err)?
        .cloned()
    {
        return Ok(Some(entry));
    }
    if let Some(entry) = base_resolution_scope_for_context(ctx, unit)
        .map_err(eval_err)?
        .lookup(&callee.identifier)
        .map_err(eval_err)?
        .cloned()
    {
        return Ok(Some(entry));
    }
    let Some(symbol) = unit
        .semantics()
        .lookup_symbol(&callee.identifier)
        .map_err(eval_err)?
        .cloned()
    else {
        return Ok(None);
    };
    let mut entry = SemanticEntry::new(
        callee.identifier.to_string(),
        entry_source_for_symbol_kind(symbol.kind),
    )
    .map_err(eval_err)?;
    entry.phase_policy = symbol.phase_policy;
    entry.node_id = symbol.node_id;
    entry.unit_id = Some(unit.unit_id().to_string());
    Ok(Some(entry))
}

fn resolved_name_fact_entry(value: &SemanticValue) -> Option<Result<SemanticEntry, EvalSignal>> {
    let SemanticValue::Map(entries) = value else {
        return None;
    };
    semantic_value_get(entries, "entry").map(semantic_entry_from_semantic_value)
}

fn semantic_entry_from_semantic_value(value: &SemanticValue) -> Result<SemanticEntry, EvalSignal> {
    let SemanticValue::Map(entries) = value else {
        return Err(eval_err("resolved-name entry must be a map"));
    };
    let name = required_semantic_str(entries, "name", "resolved-name entry requires name")?;
    let source_text =
        required_semantic_str(entries, "source", "resolved-name entry requires source")?;
    let source = semantic_entry_source(&source_text)?;
    let mut entry = SemanticEntry::new(name, source).map_err(eval_err)?;
    entry.phase_policy = semantic_entry_phase(entries, "phase")?.unwrap_or(PhasePolicy::Runtime);
    entry.effect_policy = semantic_entry_effect_policy(entries, "effect_policy")?
        .or(semantic_entry_effect_policy(entries, "effect")?)
        .unwrap_or_else(EffectPolicy::pure);
    entry.eval_policy =
        semantic_entry_eval_policy(entries, "eval_policy")?.unwrap_or(EvalPolicy::Eager);
    entry.control_policy =
        semantic_entry_control_policy(entries, "control_policy")?.unwrap_or(ControlPolicy::Plain);
    entry.scope_policy =
        semantic_entry_scope_policy(entries, "scope_policy")?.unwrap_or(ScopePolicy::None);
    entry.node_id = semantic_entry_node_id(entries, "node_id")?;
    entry.unit_id = optional_semantic_str(entries, "unit_id")?;
    entry.value = semantic_value_get(entries, "value")
        .cloned()
        .unwrap_or(SemanticValue::Null);
    Ok(entry)
}

fn semantic_value_get<'a>(
    entries: &'a [(String, SemanticValue)],
    key: &str,
) -> Option<&'a SemanticValue> {
    entries
        .iter()
        .find_map(|(candidate, value)| (candidate == key).then_some(value))
}

fn required_semantic_str(
    entries: &[(String, SemanticValue)],
    key: &str,
    message: &str,
) -> Result<String, EvalSignal> {
    optional_semantic_str(entries, key)?.ok_or_else(|| eval_err(message))
}

fn optional_semantic_str(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<String>, EvalSignal> {
    match semantic_value_get(entries, key) {
        None | Some(SemanticValue::Null) => Ok(None),
        Some(SemanticValue::Str(value)) => Ok(Some(value.clone())),
        Some(_) => Err(eval_err(format!(
            "resolved-name entry {key} must be a string"
        ))),
    }
}

fn semantic_entry_phase(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<PhasePolicy>, EvalSignal> {
    match optional_semantic_str(entries, key)?.as_deref() {
        None => Ok(None),
        Some("runtime") => Ok(Some(PhasePolicy::Runtime)),
        Some("compile_time" | "compile-time") => Ok(Some(PhasePolicy::CompileTime)),
        Some("dual") => Ok(Some(PhasePolicy::Dual)),
        Some(_) => Err(eval_err("resolved-name entry phase is invalid")),
    }
}

fn semantic_entry_eval_policy(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<EvalPolicy>, EvalSignal> {
    match optional_semantic_str(entries, key)?.as_deref() {
        None => Ok(None),
        Some("eager") => Ok(Some(EvalPolicy::Eager)),
        Some("lazy_if") => Ok(Some(EvalPolicy::LazyIf)),
        Some("sequential") => Ok(Some(EvalPolicy::Sequential)),
        Some("special_form") => Ok(Some(EvalPolicy::SpecialForm)),
        Some(_) => Err(eval_err("resolved-name entry eval policy is invalid")),
    }
}

fn semantic_entry_control_policy(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<ControlPolicy>, EvalSignal> {
    match optional_semantic_str(entries, key)?.as_deref() {
        None => Ok(None),
        Some("plain") => Ok(Some(ControlPolicy::Plain)),
        Some("conditional_branch") => Ok(Some(ControlPolicy::ConditionalBranch)),
        Some("structured_exit") => Ok(Some(ControlPolicy::StructuredExit)),
        Some(_) => Err(eval_err("resolved-name entry control policy is invalid")),
    }
}

fn semantic_entry_scope_policy(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<ScopePolicy>, EvalSignal> {
    match optional_semantic_str(entries, key)?.as_deref() {
        None => Ok(None),
        Some("none") => Ok(Some(ScopePolicy::None)),
        Some("lexical_binding") => Ok(Some(ScopePolicy::LexicalBinding)),
        Some(_) => Err(eval_err("resolved-name entry scope policy is invalid")),
    }
}

fn semantic_entry_effect_policy(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<EffectPolicy>, EvalSignal> {
    match semantic_value_get(entries, key) {
        None | Some(SemanticValue::Null) => Ok(None),
        Some(SemanticValue::Str(value)) if value == "pure" => Ok(Some(EffectPolicy::pure())),
        Some(SemanticValue::Str(value)) => EffectPolicy::single(value.clone())
            .map(Some)
            .map_err(eval_err),
        Some(SemanticValue::List(items)) => items
            .iter()
            .map(|item| match item {
                SemanticValue::Str(value) if !value.is_empty() => Ok(value.clone()),
                _ => Err(eval_err("resolved-name entry effect policy is invalid")),
            })
            .collect::<Result<Vec<_>, _>>()
            .and_then(|tags| EffectPolicy::new(tags).map(Some).map_err(eval_err)),
        Some(_) => Err(eval_err("resolved-name entry effect policy is invalid")),
    }
}

fn semantic_entry_node_id(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<NodeId>, EvalSignal> {
    match semantic_value_get(entries, key) {
        None | Some(SemanticValue::Null) => Ok(None),
        Some(SemanticValue::Node(node_id)) => Ok(Some(*node_id)),
        Some(SemanticValue::Int(value)) if *value >= 0 => Ok(Some(*value as NodeId)),
        Some(_) => Err(eval_err(format!(
            "resolved-name entry {key} must be a non-negative integer"
        ))),
    }
}

fn semantic_entry_source(value: &str) -> Result<EntrySource, EvalSignal> {
    match value {
        "builtin" => Ok(EntrySource::Builtin),
        "top-level" | "top_level" => Ok(EntrySource::TopLevel),
        "registered" => Ok(EntrySource::Registered),
        "parameter" => Ok(EntrySource::Parameter),
        "local" => Ok(EntrySource::Local),
        "external" => Ok(EntrySource::External),
        _ => Err(eval_err("resolved-name entry source is invalid")),
    }
}

fn should_normalize_ctfe(entry: &SemanticEntry) -> bool {
    entry.phase_policy == PhasePolicy::CompileTime
        && matches!(
            entry.source,
            EntrySource::Builtin | EntrySource::Registered | EntrySource::TopLevel
        )
}

fn execute_ctfe_entry(
    ev: &mut Evaluator,
    ctx: &ProviderContextBridgeValue,
    node_id: NodeId,
    entry: &SemanticEntry,
) -> Result<RuntimeValue, EvalSignal> {
    if !should_normalize_ctfe(entry) {
        return Ok(ctfe_result_value(CtfeResult::NoChange));
    }
    let callback = ctx
        .compiler()
        .lookup_registered_value(&entry.name)
        .map_err(eval_err)?;
    let callback = match callback {
        Some(callback) => Some(callback),
        None if entry.source == EntrySource::Registered => initial_binding_value(ctx, &entry.name),
        None => None,
    };
    if let Some(callback) = callback {
        if entry.source == EntrySource::Registered && !is_provider_style_callback(&callback) {
            return execute_registered_ctfe_call(ctx, node_id, entry);
        }
        return execute_ctfe_callback(ev, ctx, node_id, entry, callback);
    }
    if entry.source == EntrySource::TopLevel {
        return execute_top_level_ctfe(ev, ctx, node_id, entry);
    }
    Ok(ctfe_result_value(CtfeResult::NoChange))
}

fn execute_registered_ctfe_call(
    ctx: &ProviderContextBridgeValue,
    node_id: NodeId,
    entry: &SemanticEntry,
) -> Result<RuntimeValue, EvalSignal> {
    let result = ctx.unit().with_unit(|unit| {
        let Some(env) = reconstruct_binding_env(ctx, unit, node_id)? else {
            return Ok(ctfe_result_value(CtfeResult::NoChange));
        };
        let mut evaluator = Evaluator::new(unit.ir().clone());
        evaluator.eval(node_id, &env)
    });
    match result {
        Ok(RuntimeValue::Null) => Ok(ctfe_result_value(CtfeResult::Replace(ExprSpec::literal(
            IrLiteralData::Null,
        )))),
        Ok(value) => Ok(ctfe_result_value(coerce_value_ctfe_result(
            ctx, node_id, value,
        )?)),
        Err(signal) => {
            if is_uninitialized_binding_signal(&signal) {
                return Ok(ctfe_result_value(CtfeResult::NoChange));
            }
            emit_ctfe_error(ctx, node_id, entry, "function_failed", signal.to_string())?;
            Ok(ctfe_result_value(CtfeResult::NoChange))
        }
    }
}

fn is_provider_style_callback(value: &RuntimeValue) -> bool {
    match value {
        RuntimeValue::Closure(closure) => closure.params == ["ctx", "node"],
        RuntimeValue::HostFunction(host) => {
            host.min_arity <= 2 && host.max_arity.is_none_or(|max| max >= 2)
        }
        _ => false,
    }
}

fn execute_ctfe_callback(
    ev: &mut Evaluator,
    ctx: &ProviderContextBridgeValue,
    node_id: NodeId,
    entry: &SemanticEntry,
    callback: RuntimeValue,
) -> Result<RuntimeValue, EvalSignal> {
    let node = node_handle(ctx, node_id);
    match ev.invoke_callback(
        &callback,
        vec![RuntimeValue::HostObject(ctx_host_object(ctx)), node.clone()],
    ) {
        Ok(value) => Ok(ctfe_result_value(coerce_value_ctfe_result(
            ctx, node_id, value,
        )?)),
        Err(signal) => {
            emit_ctfe_error(ctx, node_id, entry, "function_failed", signal.to_string())?;
            Ok(ctfe_result_value(CtfeResult::NoChange))
        }
    }
}

fn execute_top_level_ctfe(
    ev: &mut Evaluator,
    ctx: &ProviderContextBridgeValue,
    node_id: NodeId,
    entry: &SemanticEntry,
) -> Result<RuntimeValue, EvalSignal> {
    let Some(function_id) = entry.node_id else {
        return Ok(ctfe_result_value(CtfeResult::NoChange));
    };
    let callback = ctx.unit().with_unit(|unit| {
        let mut evaluator = Evaluator::new(unit.ir().clone());
        let env = reconstruct_binding_env(ctx, unit, function_id)?
            .unwrap_or_else(|| evaluator.make_env());
        evaluator.eval(function_id, &env)
    })?;
    let node = node_handle(ctx, node_id);
    match ev.invoke_callback(
        &callback,
        vec![RuntimeValue::HostObject(ctx_host_object(ctx)), node.clone()],
    ) {
        Ok(value) => Ok(ctfe_result_value(coerce_value_ctfe_result(
            ctx, node_id, value,
        )?)),
        Err(signal) => {
            emit_ctfe_error(ctx, node_id, entry, "function_failed", signal.to_string())?;
            Ok(ctfe_result_value(CtfeResult::NoChange))
        }
    }
}

fn coerce_value_ctfe_result(
    ctx: &ProviderContextBridgeValue,
    node_id: NodeId,
    value: RuntimeValue,
) -> Result<CtfeResult, EvalSignal> {
    if matches!(value, RuntimeValue::Null) || same_node_handle(&value, node_id) {
        return Ok(CtfeResult::NoChange);
    }
    if let Some(spec) = runtime_expr_spec(ctx, &value)? {
        return Ok(CtfeResult::Replace(spec));
    }
    Ok(CtfeResult::Lift(value))
}

fn same_node_handle(value: &RuntimeValue, node_id: NodeId) -> bool {
    let RuntimeValue::HostObject(object) = value else {
        return false;
    };
    object
        .as_any()
        .downcast_ref::<NodeBridgeValue>()
        .is_some_and(|node| node.node_id() == node_id)
}

fn materialize_ctfe_result(
    ctx: &ProviderContextBridgeValue,
    value: &RuntimeValue,
) -> Result<Option<ExprSpec>, EvalSignal> {
    if matches!(value, RuntimeValue::Null) {
        return Ok(None);
    }
    if let Some(spec) = runtime_expr_spec(ctx, value)? {
        return Ok(Some(spec));
    }
    let RuntimeValue::HostObject(object) = value else {
        return lift_runtime_value(ctx, value).map(Some);
    };
    let Some(result) = object.as_any().downcast_ref::<CtfeResultBridgeValue>() else {
        return lift_runtime_value(ctx, value).map(Some);
    };
    match &result.result {
        CtfeResult::NoChange => Ok(None),
        CtfeResult::Replace(spec) => Ok(Some(spec.clone())),
        CtfeResult::Lift(value) => lift_runtime_value(ctx, value).map(Some),
    }
}

fn fold_compile_time_call(
    ev: &mut Evaluator,
    ctx: &ProviderContextBridgeValue,
    node_id: NodeId,
) -> Result<RuntimeValue, EvalSignal> {
    let (entry, safe) = ctx.unit().with_unit(|unit| {
        let Some(call) = call_node(unit, node_id)? else {
            return Ok::<(Option<SemanticEntry>, bool), EvalSignal>((None, false));
        };
        let entry = callee_entry_for_call(ctx, unit, call)?;
        let safe = match entry.as_ref() {
            Some(entry) if entry.source == EntrySource::Registered => true,
            _ => node_is_fold_safe(ctx, unit, node_id, &mut Vec::new())?,
        };
        Ok((entry, safe))
    })?;
    let Some(entry) = entry else {
        return Ok(node_handle(ctx, node_id));
    };
    if !safe || !should_normalize_ctfe(&entry) {
        return Ok(node_handle(ctx, node_id));
    }
    let result = execute_ctfe_entry(ev, ctx, node_id, &entry)?;
    let spec = match materialize_ctfe_result(ctx, &result) {
        Ok(Some(spec)) => spec,
        Ok(None) => return Ok(node_handle(ctx, node_id)),
        Err(signal) => {
            emit_ctfe_error(
                ctx,
                node_id,
                &entry,
                "unliftable_result",
                signal.to_string(),
            )?;
            return Ok(node_handle(ctx, node_id));
        }
    };
    let new_id = ctx
        .unit()
        .with_unit_mut(|unit| unit.replace_ir_subtree_with_spec(node_id, &spec))
        .map_err(eval_err)?;
    Ok(node_handle(ctx, new_id))
}

fn node_is_fold_safe(
    ctx: &ProviderContextBridgeValue,
    unit: &Unit,
    node_id: NodeId,
    seen: &mut Vec<NodeId>,
) -> Result<bool, EvalSignal> {
    if seen.contains(&node_id) {
        return Ok(false);
    }
    let Some(node) = unit.ir().node(node_id) else {
        return Err(eval_err("ctfe-provider-fold-safe? node is missing"));
    };
    match node {
        Node::Literal(_) | Node::Name(_) => return Ok(true),
        Node::Call(_) => {}
    }
    seen.push(node_id);
    let result = match node {
        Node::Call(call) => {
            if let Some(callee) = callee_name(unit, call)? {
                match callee.as_str() {
                    "lambda" => call.args[1..]
                        .iter()
                        .map(|id| node_is_fold_safe(ctx, unit, *id, seen))
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .all(|safe| safe),
                    "bind" => bind_is_fold_safe(ctx, unit, call, seen)?,
                    "block" | "leave" => call
                        .args
                        .iter()
                        .map(|id| node_is_fold_safe(ctx, unit, *id, seen))
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .all(|safe| safe),
                    _ => call_is_fold_safe(ctx, unit, call, seen)?,
                }
            } else {
                call_is_fold_safe(ctx, unit, call, seen)?
            }
        }
        _ => true,
    };
    seen.pop();
    Ok(result)
}

fn bind_is_fold_safe(
    ctx: &ProviderContextBridgeValue,
    unit: &Unit,
    call: &CallNode,
    seen: &mut Vec<NodeId>,
) -> Result<bool, EvalSignal> {
    if call.args.is_empty() {
        return Ok(true);
    }
    let mut ids = Vec::new();
    match unit.ir().node(call.args[0]) {
        Some(Node::Name(_)) => ids.extend(call.args.iter().skip(1).copied()),
        Some(Node::Call(bindings)) => {
            for pair_id in unit_call_item_ids(unit, bindings) {
                let Some(Node::Call(pair)) = unit.ir().node(pair_id) else {
                    return Ok(false);
                };
                let pair_items = unit_call_item_ids(unit, pair);
                if pair_items.len() != 2 {
                    return Ok(false);
                }
                ids.push(pair_items[1]);
            }
            ids.extend(call.args.iter().skip(1).copied());
        }
        Some(Node::Literal(_)) => ids.extend(call.args.iter().skip(1).copied()),
        _ => return Ok(false),
    }
    for id in ids {
        if !node_is_fold_safe(ctx, unit, id, seen)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn call_is_fold_safe(
    ctx: &ProviderContextBridgeValue,
    unit: &Unit,
    call: &CallNode,
    seen: &mut Vec<NodeId>,
) -> Result<bool, EvalSignal> {
    for arg_id in &call.args {
        if !node_is_fold_safe(ctx, unit, *arg_id, seen)? {
            return Ok(false);
        }
    }
    if let Some(entry) = callee_entry_for_call(ctx, unit, call)? {
        return Ok(should_normalize_ctfe(&entry));
    }
    let Some(callee) = callee_name(unit, call)? else {
        return Ok(false);
    };
    let metadata = Evaluator::new(Default::default()).builtin_metadata(&callee);
    Ok(metadata.is_some_and(|metadata| {
        metadata.effect_policy.is_pure() && metadata.phase_policy != PhasePolicy::Runtime
    }))
}

fn lift_runtime_value(
    ctx: &ProviderContextBridgeValue,
    value: &RuntimeValue,
) -> Result<ExprSpec, EvalSignal> {
    if let Some(spec) = runtime_expr_spec(ctx, value)? {
        return Ok(spec);
    }
    Ok(ExprSpec::literal(runtime_to_literal(value)?))
}

fn runtime_expr_spec(
    ctx: &ProviderContextBridgeValue,
    value: &RuntimeValue,
) -> Result<Option<ExprSpec>, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Ok(None);
    };
    if let Some(spec) = object.as_any().downcast_ref::<ExprSpecBridgeValue>() {
        return Ok(Some(spec.clone_spec()));
    }
    if let Some(node) = object.as_any().downcast_ref::<NodeBridgeValue>() {
        return ctx
            .unit()
            .with_unit(|unit| unit.ir().expr_spec_for_subtree(node.node_id()).map(Some))
            .map_err(eval_err);
    }
    Ok(None)
}

fn runtime_to_literal(value: &RuntimeValue) -> Result<IrLiteralData, EvalSignal> {
    match value {
        RuntimeValue::Null => Ok(IrLiteralData::Null),
        RuntimeValue::Bool(value) => Ok(IrLiteralData::Bool(*value)),
        RuntimeValue::Int(value) => Ok(IrLiteralData::Int(*value)),
        RuntimeValue::Float(value) => Ok(IrLiteralData::Float(*value)),
        RuntimeValue::Str(value) => Ok(IrLiteralData::Str(value.to_string())),
        RuntimeValue::Tuple(items) => items
            .iter()
            .map(runtime_to_literal)
            .collect::<Result<Vec<_>, _>>()
            .map(IrLiteralData::Tuple),
        RuntimeValue::List(items) => items
            .borrow()
            .iter()
            .map(runtime_to_literal)
            .collect::<Result<Vec<_>, _>>()
            .map(IrLiteralData::Tuple),
        RuntimeValue::Map(map) => {
            let entries = map
                .borrow()
                .iter()
                .map(|(key, value)| {
                    let MapKey::Str(key) = key else {
                        return Err(eval_err("CTFE literal map keys must be strings"));
                    };
                    Ok((key.to_string(), runtime_to_literal(value)?))
                })
                .collect::<Result<Vec<_>, _>>()?;
            IrLiteralData::dict(entries).map_err(eval_err)
        }
        RuntimeValue::Closure(_)
        | RuntimeValue::Builtin(_)
        | RuntimeValue::HostFunction(_)
        | RuntimeValue::HostObject(_)
        | RuntimeValue::UninitializedTopLevel => {
            Err(eval_err("CTFE result is not liftable into IR"))
        }
    }
}

fn ctfe_result_value(result: CtfeResult) -> RuntimeValue {
    RuntimeValue::HostObject(Rc::new(CtfeResultBridgeValue::new(result)))
}

fn ctx_host_object(ctx: &ProviderContextBridgeValue) -> Rc<dyn HostObject> {
    Rc::new(ProviderContextBridgeValue::new(
        ctx.context().clone(),
        ctx.compiler(),
        ctx.unit(),
    ))
}

fn emit_ctfe_error(
    ctx: &ProviderContextBridgeValue,
    node_id: NodeId,
    entry: &SemanticEntry,
    kind: &str,
    detail: String,
) -> Result<(), EvalSignal> {
    let mut diagnostic = Diagnostic::new(
        DiagnosticSeverity::Error,
        format!("CTFE function {} failed: {detail}", entry.name),
    )
    .map_err(eval_err)?;
    diagnostic.code = Some(format!("caap.ctfe.{kind}"));
    diagnostic.span = ctx
        .unit()
        .with_unit(|unit| unit.ir().source_span(node_id).cloned());
    ctx.compiler().push_diagnostic(diagnostic);
    Ok(())
}

fn rt_map<const N: usize>(entries: [(&str, RuntimeValue); N]) -> RuntimeValue {
    let mut map = std::collections::HashMap::new();
    for (key, value) in entries {
        map.insert(MapKey::Str(key.into()), value);
    }
    RuntimeValue::Map(Rc::new(RefCell::new(map)))
}

fn rt_tuple(items: Vec<RuntimeValue>) -> RuntimeValue {
    RuntimeValue::Tuple(items.into())
}

fn rt_string(value: impl AsRef<str>) -> RuntimeValue {
    RuntimeValue::Str(value.as_ref().into())
}

fn base_resolution_scope_for_context(
    ctx: &ProviderContextBridgeValue,
    unit: &Unit,
) -> Result<SemanticRegistry, String> {
    let mut registry = base_resolution_scope(unit)?;
    define_initial_binding_entries(&mut registry, ctx.initial_bindings())?;
    define_language_builtin_bridge_entries(&mut registry, ctx)?;
    for name in ctx.compiler().compile_time_function_names() {
        let mut entry = SemanticEntry::new(name, EntrySource::Registered)?;
        entry.phase_policy = PhasePolicy::CompileTime;
        registry.define(entry)?;
    }
    Ok(registry)
}

fn define_language_builtin_bridge_entries(
    registry: &mut SemanticRegistry,
    ctx: &ProviderContextBridgeValue,
) -> Result<(), String> {
    let mut names: BTreeSet<&'static str> = BTreeSet::new();
    for bridge in ctx.compiler().language_builtin_bridges() {
        names.extend(language_builtin_bridge_names(&bridge));
    }
    for name in names {
        let mut entry = SemanticEntry::new(name, EntrySource::Builtin)?;
        entry.phase_policy = PhasePolicy::Dual;
        registry.define(entry)?;
    }
    Ok(())
}

fn language_builtin_bridge_names(bridge: &str) -> &'static [&'static str] {
    match bridge {
        "core-special" => &[
            "if",
            "or",
            "and",
            "do",
            "lambda",
            "bind",
            "block",
            "leave",
            "while",
            "host-call-method",
            "host-import",
        ],
        "core-value" => &[
            "eq",
            "value-lt",
            "value-gt",
            "invoke",
            "apply",
            "for-range",
            "int-add",
            "int-min",
            "int-max",
            "int-abs",
            "int-clamp",
            "int-sub",
            "int-and",
            "int-xor",
            "int-shr",
            "int-mul",
            "int-div",
            "int-rem",
            "int-mod",
            "runtime-error",
            "host-value-kind",
            "value-is-string",
            "value-is-bool",
            "value-is-int",
            "value-is-float",
            "value-is-null",
            "value-is-list",
            "value-is-tuple",
            "value-is-map",
            "value-is-callable",
            "value-is-error?",
            "lt",
            "gt",
            "int-to-float",
            "float-to-int",
        ],
        "data" => &[
            "string-concat-many",
            "string-last-segment",
            "string-find",
            "string-index-of",
            "string-slice",
            "string-split",
            "string-repeat",
            "string-format",
            "string-pad-left",
            "string-pad-right",
            "string-to-int",
            "int-to-string",
            "string-byte-length",
            "string-byte-at",
            "get",
            "get!",
            "size",
            "contains",
            "map-keys",
            "map-values",
            "map-merge",
            "map-update",
            "map-of-entries",
            "string-trim",
            "string-starts-with",
            "string-ends-with",
            "string-upcase",
            "string-downcase",
            "string-replace",
            "string-contains",
            "string-lines",
            "sequence-range",
            "sequence-each",
            "sequence-each-indexed",
            "sequence-each-pair",
            "sequence-fold-left",
            "sequence-find",
            "sequence-find-reverse",
            "sequence-any",
            "sequence-all",
            "sequence-index-of",
            "sequence-map",
            "sequence-filter",
            "sequence-count",
            "sequence-group-by",
            "sequence-reverse",
            "sequence-slice",
            "sequence-take",
            "sequence-drop",
            "sequence-zip",
            "sequence-flatten",
            "sequence-sort-by",
            "sequence-sort-by-desc",
            "sequence-distinct",
            "sequence-unique-by",
            "sequence-join",
            "stable-hash",
        ],
        "mutable-data" => &[
            "map-of",
            "assoc",
            "assoc-many",
            "list-of",
            "append",
            "append-many",
            "set",
        ],
        _ => &[],
    }
}

fn base_resolution_scope(unit: &Unit) -> Result<SemanticRegistry, String> {
    let snapshot = unit.semantics().snapshot();
    let mut local_entries: std::collections::BTreeMap<String, SemanticEntry> =
        snapshot.semantics.entries.into_iter().collect();
    let mut registry = SemanticRegistry::new();
    for (name, symbol) in snapshot.symbols {
        let mut entry = local_entries.remove(&name).unwrap_or(SemanticEntry::new(
            name.clone(),
            entry_source_for_symbol_kind(symbol.kind),
        )?);
        entry.name = name;
        if entry.node_id.is_none() {
            entry.node_id = symbol.node_id;
        }
        if entry.unit_id.is_none() {
            entry.unit_id = Some(unit.unit_id().to_string());
        }
        entry.phase_policy = symbol.phase_policy;
        registry.define(entry)?;
    }
    for (_, mut entry) in local_entries {
        if entry.unit_id.is_none() {
            entry.unit_id = Some(unit.unit_id().to_string());
        }
        registry.define(entry)?;
    }
    Ok(registry)
}

fn define_initial_binding_entries(
    registry: &mut SemanticRegistry,
    initial: &[(String, RuntimeValue)],
) -> Result<(), String> {
    for (name, value) in initial {
        if registry.lookup(name)?.is_none() {
            let mut entry = SemanticEntry::new(name.clone(), EntrySource::Registered)?;
            entry.phase_policy = PhasePolicy::Dual;
            registry.define(entry)?;
        }
        define_qualified_initial_binding_entries(registry, name, value)?;
    }
    Ok(())
}

fn define_qualified_initial_binding_entries(
    registry: &mut SemanticRegistry,
    prefix: &str,
    value: &RuntimeValue,
) -> Result<(), String> {
    let RuntimeValue::Map(map) = value else {
        return Ok(());
    };
    let map = map.borrow();
    for key in map.keys() {
        let MapKey::Str(segment) = key else {
            continue;
        };
        if segment.starts_with("__") {
            continue;
        }
        let name = format!("{prefix}.{segment}");
        if registry.lookup(&name)?.is_none() {
            let mut entry = SemanticEntry::new(name, EntrySource::Registered)?;
            entry.phase_policy =
                initial_qualified_phase(value, segment).unwrap_or(PhasePolicy::CompileTime);
            registry.define(entry)?;
        }
    }
    Ok(())
}

fn initial_qualified_phase(value: &RuntimeValue, segment: &str) -> Option<PhasePolicy> {
    let contracts = map_get_str(value, "__contract_semantics__")
        .or_else(|| map_get_str(value, "contract_semantics"))?;
    let contract = map_get_str(&contracts, segment)?;
    let phase = map_get_str(&contract, "phase")?;
    let RuntimeValue::Str(phase) = phase else {
        return None;
    };
    match phase.as_ref() {
        "compile_time" | "compile-time" => Some(PhasePolicy::CompileTime),
        "runtime" => Some(PhasePolicy::Runtime),
        "dual" => Some(PhasePolicy::Dual),
        _ => None,
    }
}

fn initial_binding_value(ctx: &ProviderContextBridgeValue, name: &str) -> Option<RuntimeValue> {
    lookup_initial_binding_value(ctx.initial_bindings(), name)
        .map(|value| contextualize_runtime_value(ctx, &value, 0))
}

fn lookup_initial_binding_value(
    initial: &[(String, RuntimeValue)],
    name: &str,
) -> Option<RuntimeValue> {
    initial
        .iter()
        .find_map(|(candidate, value)| (candidate == name).then(|| value.clone()))
        .or_else(|| lookup_qualified_initial_binding_value(initial, name))
}

fn lookup_qualified_initial_binding_value(
    initial: &[(String, RuntimeValue)],
    name: &str,
) -> Option<RuntimeValue> {
    if !name.contains('.') {
        return None;
    }
    let segments: Vec<&str> = name
        .split('.')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() < 2 {
        return None;
    }
    for prefix_len in (1..segments.len()).rev() {
        let prefix = segments[..prefix_len].join(".");
        let Some((_, mut value)) = initial
            .iter()
            .find(|(candidate, _)| candidate == &prefix)
            .map(|(candidate, value)| (candidate, value.clone()))
        else {
            continue;
        };
        let mut resolved = true;
        for segment in &segments[prefix_len..] {
            let Some(next) = map_get_str(&value, segment) else {
                resolved = false;
                break;
            };
            value = next;
        }
        if resolved {
            return Some(value);
        }
    }
    None
}

fn map_get_str(value: &RuntimeValue, key: &str) -> Option<RuntimeValue> {
    let RuntimeValue::Map(map) = value else {
        return None;
    };
    map.borrow().get(&MapKey::Str(key.into())).cloned()
}

fn contextualize_runtime_value(
    ctx: &ProviderContextBridgeValue,
    value: &RuntimeValue,
    depth: usize,
) -> RuntimeValue {
    if depth > 16 {
        return value.clone();
    }
    match value {
        RuntimeValue::Closure(closure) => {
            let env = Environment::new(Some(Rc::clone(&closure.env)));
            Environment::define(&env, "compiler", RuntimeValue::HostObject(ctx.compiler()));
            Environment::define(&env, "unit", RuntimeValue::HostObject(ctx.unit()));
            RuntimeValue::Closure(Rc::new(ClosureValue {
                params: closure.params.clone(),
                body_ids: closure.body_ids.clone(),
                env,
                graph: Rc::clone(&closure.graph),
            }))
        }
        RuntimeValue::Tuple(items) => RuntimeValue::Tuple(
            items
                .iter()
                .map(|item| contextualize_runtime_value(ctx, item, depth + 1))
                .collect::<Vec<_>>()
                .into(),
        ),
        RuntimeValue::List(items) => RuntimeValue::List(Rc::new(RefCell::new(
            items
                .borrow()
                .iter()
                .map(|item| contextualize_runtime_value(ctx, item, depth + 1))
                .collect(),
        ))),
        RuntimeValue::Map(map) => RuntimeValue::Map(Rc::new(RefCell::new(
            map.borrow()
                .iter()
                .map(|(key, item)| {
                    (
                        key.clone(),
                        contextualize_runtime_value(ctx, item, depth + 1),
                    )
                })
                .collect(),
        ))),
        _ => value.clone(),
    }
}

fn is_uninitialized_binding_signal(signal: &EvalSignal) -> bool {
    matches!(
        signal,
        EvalSignal::Error(error) if error.message().contains("was accessed before initialization")
    )
}

fn entry_source_for_symbol_kind(kind: SymbolKind) -> EntrySource {
    match kind {
        SymbolKind::TopLevel => EntrySource::TopLevel,
        SymbolKind::Parameter => EntrySource::Parameter,
        SymbolKind::Local => EntrySource::Local,
        SymbolKind::Injected => EntrySource::Registered,
        SymbolKind::Builtin => EntrySource::Builtin,
        SymbolKind::External => EntrySource::External,
    }
}

fn entry_source_value(value: &RuntimeValue, message: &str) -> Result<EntrySource, EvalSignal> {
    let RuntimeValue::Str(text) = value else {
        return Err(eval_err(message));
    };
    match text.as_ref() {
        "builtin" => Ok(EntrySource::Builtin),
        "top-level" | "top_level" => Ok(EntrySource::TopLevel),
        "registered" => Ok(EntrySource::Registered),
        "parameter" => Ok(EntrySource::Parameter),
        "local" => Ok(EntrySource::Local),
        "external" => Ok(EntrySource::External),
        _ => Err(eval_err(message)),
    }
}

fn phase_value(value: &RuntimeValue, message: &str) -> Result<PhasePolicy, EvalSignal> {
    let RuntimeValue::Str(text) = value else {
        return Err(eval_err(message));
    };
    match text.as_ref() {
        "runtime" => Ok(PhasePolicy::Runtime),
        "compile_time" | "compile-time" => Ok(PhasePolicy::CompileTime),
        "dual" => Ok(PhasePolicy::Dual),
        _ => Err(eval_err(message)),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TraversalMode {
    Walk,
    FindFirst,
    Filter,
    Stateful,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TraversalOrder {
    Preorder,
    Postorder,
}

#[derive(Clone, Debug)]
struct TraversalOptions {
    mode: TraversalMode,
    order: TraversalOrder,
    kind: Option<String>,
    initial_state: Option<RuntimeValue>,
}

impl TraversalOptions {
    fn from_value(value: Option<&RuntimeValue>) -> Result<Self, EvalSignal> {
        let mut options = Self {
            mode: TraversalMode::Walk,
            order: TraversalOrder::Preorder,
            kind: None,
            initial_state: None,
        };
        let Some(value) = value else {
            return Ok(options);
        };
        if matches!(value, RuntimeValue::Null) {
            return Ok(options);
        }
        let RuntimeValue::Map(map) = value else {
            return Err(eval_err(
                "ctfe-provider-traversal-walk options must be a map",
            ));
        };
        let map = map.borrow();
        if let Some(state) = map.get(&MapKey::Str("initial_state".into())) {
            options.initial_state = Some(state.clone());
        } else if let Some(state) = map.get(&MapKey::Str("initial-state".into())) {
            options.initial_state = Some(state.clone());
        }
        if let Some(mode) = map.get(&MapKey::Str("mode".into())) {
            options.mode = traversal_mode(mode)?;
        }
        if options.mode == TraversalMode::Stateful {
            return Ok(options);
        }
        if let Some(order) = map.get(&MapKey::Str("order".into())) {
            options.order = traversal_order(order)?;
        }
        if let Some(kind) = map.get(&MapKey::Str("kind".into())) {
            options.kind = Some(require_string(
                kind,
                "ctfe-provider-traversal-walk kind option must be a non-empty string",
            )?);
        }
        Ok(options)
    }
}

fn traversal_mode(value: &RuntimeValue) -> Result<TraversalMode, EvalSignal> {
    match require_string(
        value,
        "ctfe-provider-traversal-walk mode option must be a string",
    )?
    .as_str()
    {
        "walk" => Ok(TraversalMode::Walk),
        "find-first" | "find_first" => Ok(TraversalMode::FindFirst),
        "filter" => Ok(TraversalMode::Filter),
        "stateful" => Ok(TraversalMode::Stateful),
        _ => Err(eval_err(
            "ctfe-provider-traversal-walk mode is not supported",
        )),
    }
}

fn traversal_order(value: &RuntimeValue) -> Result<TraversalOrder, EvalSignal> {
    match require_string(
        value,
        "ctfe-provider-traversal-walk order option must be a string",
    )?
    .as_str()
    {
        "preorder" | "pre-order" | "pre_order" => Ok(TraversalOrder::Preorder),
        "postorder" | "post-order" | "post_order" => Ok(TraversalOrder::Postorder),
        _ => Err(eval_err(
            "ctfe-provider-traversal-walk order is not supported",
        )),
    }
}

fn collect_traversal_nodes(
    ctx: &ProviderContextBridgeValue,
    root_id: NodeId,
    options: &TraversalOptions,
) -> Result<Vec<NodeId>, EvalSignal> {
    ctx.unit().with_unit(|unit| {
        if unit.ir().node(root_id).is_none() {
            return Err(eval_err(
                "ctfe-provider-traversal-walk root node is missing",
            ));
        }
        let mut result = Vec::new();
        match options.order {
            TraversalOrder::Preorder => {
                let mut stack = vec![root_id];
                while let Some(node_id) = stack.pop() {
                    ctx.track_node_read(node_id);
                    let node = unit.ir().node(node_id).ok_or_else(|| {
                        eval_err("ctfe-provider-traversal-walk found a dangling child node")
                    })?;
                    if traversal_kind_matches(node, options.kind.as_deref()) {
                        result.push(node_id);
                    }
                    let children = node.children();
                    for child_id in children.into_iter().rev() {
                        stack.push(child_id);
                    }
                }
            }
            TraversalOrder::Postorder => {
                let mut stack = vec![(root_id, false)];
                while let Some((node_id, visited)) = stack.pop() {
                    ctx.track_node_read(node_id);
                    let node = unit.ir().node(node_id).ok_or_else(|| {
                        eval_err("ctfe-provider-traversal-walk found a dangling child node")
                    })?;
                    if visited {
                        if traversal_kind_matches(node, options.kind.as_deref()) {
                            result.push(node_id);
                        }
                    } else {
                        stack.push((node_id, true));
                        let children = node.children();
                        for child_id in children.into_iter().rev() {
                            stack.push((child_id, false));
                        }
                    }
                }
            }
        }
        Ok(result)
    })
}

fn traversal_kind_matches(node: &Node, kind: Option<&str>) -> bool {
    let Some(kind) = kind else {
        return true;
    };
    let actual = match node {
        Node::Name(_) => "name",
        Node::Literal(_) => "literal",
        Node::Call(_) => "call",
    };
    kind.trim()
        .trim_end_matches("_node")
        .trim_end_matches("-node")
        .eq_ignore_ascii_case(actual)
}

fn traversal_child_states(value: &RuntimeValue) -> Result<Vec<(NodeId, RuntimeValue)>, EvalSignal> {
    let values = runtime_sequence_values(
        value,
        "ctfe-provider-traversal-walk stateful callback must return a sequence",
    )?;
    values
        .iter()
        .map(|item| {
            let pair = runtime_sequence_values(
                item,
                "ctfe-provider-traversal-walk stateful child entry must be a pair",
            )?;
            if pair.len() != 2 {
                return Err(eval_err(
                    "ctfe-provider-traversal-walk stateful child entry must contain node and state",
                ));
            }
            let child_id = require_node_id(
                &pair[0],
                "ctfe-provider-traversal-walk stateful child entry expects a node",
            )?;
            Ok((child_id, pair[1].clone()))
        })
        .collect()
}

fn runtime_sequence_values(
    value: &RuntimeValue,
    message: &str,
) -> Result<Vec<RuntimeValue>, EvalSignal> {
    match value {
        RuntimeValue::Tuple(items) => Ok(items.iter().cloned().collect()),
        RuntimeValue::List(items) => Ok(items.borrow().iter().cloned().collect()),
        _ => Err(eval_err(message)),
    }
}

fn normalize_effect(effect: &str) -> String {
    effect.trim().replace('-', "_").to_ascii_lowercase()
}

fn semantic_value_to_runtime_in_context(
    ctx: &ProviderContextBridgeValue,
    value: &SemanticValue,
) -> RuntimeValue {
    match value {
        SemanticValue::Null => RuntimeValue::Null,
        SemanticValue::Bool(value) => RuntimeValue::Bool(*value),
        SemanticValue::Int(value) => RuntimeValue::Int(*value),
        SemanticValue::Float(value) => RuntimeValue::Float(*value),
        SemanticValue::Str(value) => RuntimeValue::Str(value.as_str().into()),
        SemanticValue::Node(node_id) => node_handle(ctx, *node_id),
        SemanticValue::List(items) => RuntimeValue::List(Rc::new(RefCell::new(
            items
                .iter()
                .map(|item| semantic_value_to_runtime_in_context(ctx, item))
                .collect(),
        ))),
        SemanticValue::Map(entries) => {
            let mut map = std::collections::HashMap::new();
            for (key, value) in entries {
                map.insert(
                    MapKey::Str(key.as_str().into()),
                    semantic_value_to_runtime_in_context(ctx, value),
                );
            }
            RuntimeValue::Map(Rc::new(RefCell::new(map)))
        }
    }
}

fn runtime_to_semantic(value: &RuntimeValue) -> Result<SemanticValue, EvalSignal> {
    match value {
        RuntimeValue::Null => Ok(SemanticValue::Null),
        RuntimeValue::Bool(value) => Ok(SemanticValue::Bool(*value)),
        RuntimeValue::Int(value) => Ok(SemanticValue::Int(*value)),
        RuntimeValue::Float(value) => Ok(SemanticValue::Float(*value)),
        RuntimeValue::Str(value) => Ok(SemanticValue::Str(value.to_string())),
        RuntimeValue::Tuple(items) => items
            .iter()
            .map(runtime_to_semantic)
            .collect::<Result<Vec<_>, _>>()
            .map(SemanticValue::List),
        RuntimeValue::List(items) => items
            .borrow()
            .iter()
            .map(runtime_to_semantic)
            .collect::<Result<Vec<_>, _>>()
            .map(SemanticValue::List),
        RuntimeValue::Map(map) => {
            let mut entries = Vec::new();
            for (key, value) in map.borrow().iter() {
                let MapKey::Str(key) = key else {
                    return Err(eval_err("provider fact maps require string keys"));
                };
                entries.push((key.to_string(), runtime_to_semantic(value)?));
            }
            SemanticValue::map(entries).map_err(eval_err)
        }
        RuntimeValue::HostObject(object) => object
            .as_any()
            .downcast_ref::<NodeBridgeValue>()
            .map(|node| SemanticValue::Node(node.node_id()))
            .ok_or_else(|| {
                eval_err("provider facts support scalar, sequence, map, and node values")
            }),
        _ => Err(eval_err(
            "provider facts support scalar, sequence, map, and node values",
        )),
    }
}
