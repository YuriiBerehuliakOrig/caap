/// Provider-context CTFE builtins for query providers.
use std::rc::Rc;

use crate::bridges::NodeBridgeValue;
use crate::compiler::annotation_tracking_predicate;
use crate::diagnostics::DiagnosticSeverity;
use crate::error::CaapResult;
use crate::eval::{eval_args, Evaluator};
use crate::ir::NodeId;
use crate::semantic::{node_subject_id, BuiltinEffectTag};
use crate::values::is_truthy;
use crate::values::{eval_err, RuntimeValue};

use super::compiler_node_match::ctfe_node_match;
use super::provider_context_helpers::*;
use super::semantic_projection::semantic_entry_to_runtime_value;

pub fn register(ev: &mut Evaluator) {
    ev.register_special(
        "ctfe_provider_require_effect",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-require-effect expects a provider context",
            )?;
            let effect = effect_tag_from_runtime_name(&require_string(
                &args[1],
                "ctfe-provider-require-effect expects an effect name",
            )?)?;
            if ctx.context().effect_tags.contains(&effect) {
                Ok(RuntimeValue::Null)
            } else {
                Err(eval_err(format!(
                    "provider {} does not declare required effect {}",
                    ctx.context().provider,
                    effect.as_str()
                )))
            }
        },
    );

    ev.register_special(
        "ctfe_provider_unit",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-unit expects a provider context",
            )?;
            Ok(RuntimeValue::HostObject(ctx.unit()))
        },
    );

    ev.register_special(
        "ctfe_provider_base_resolution_scope",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-base-resolution-scope expects a provider context",
            )?;
            require_provider_effect(ctx, BuiltinEffectTag::ReadSymbols)?;
            let registry = ctx
                .unit()
                .with_unit(|unit| base_resolution_scope_for_context(ctx, unit))
                .map_err(eval_err)?;
            Ok(RuntimeValue::HostObject(Rc::new(
                ResolutionScopeBridgeValue::new(registry),
            )))
        },
    );

    ev.register_special(
        "ctfe_resolution_scope_fork",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let scope = require_resolution_scope(
                &args[0],
                "ctfe-resolution-scope-fork expects a resolution scope",
            )?;
            let child = scope.registry.borrow().fork();
            Ok(RuntimeValue::HostObject(Rc::new(
                ResolutionScopeBridgeValue::new(child),
            )))
        },
    );

    ev.register_special(
        "ctfe_resolution_scope_lookup",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
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
        },
    );

    ev.register_special(
        "ctfe_resolution_scope_define!",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let scope = require_resolution_scope(
                &args[0],
                "ctfe-resolution-scope-define! expects a resolution scope",
            )?;
            let entry = semantic_entry_from_runtime_descriptor(
                &args[1],
                "ctfe-resolution-scope-define! expects a semantic entry descriptor",
            )?;
            scope
                .registry
                .borrow_mut()
                .define(entry)
                .map_err(eval_err)?;
            Ok(args[1].clone())
        },
    );

    ev.register_special(
        "ctfe_semantic_entry_node",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
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
        },
    );

    ev.register_special(
        "ctfe_semantic_entry_to_map",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let entry = require_semantic_entry(
                &args[0],
                "ctfe-semantic-entry-to-map expects a resolved semantic entry",
            )?;
            Ok(semantic_entry_to_runtime_value(entry))
        },
    );

    ev.register_special(
        "ctfe_provider_invoke_callback",
        2,
        None,
        crate::values::BuiltinMetadata::compile_time_special_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            require_provider_context(
                &args[0],
                "ctfe-provider-invoke-callback expects a provider context",
            )?;
            let callback = args[1].clone();
            ev.invoke_callback(&callback, args[2..].to_vec())
        },
    );

    ev.register_special(
        "ctfe_provider_traversal_walk",
        3,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_special_effects([
            BuiltinEffectTag::Impure,
            BuiltinEffectTag::ReadIr,
        ]),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-traversal-walk expects a provider context",
            )?;
            require_provider_effect(ctx, BuiltinEffectTag::ReadIr)?;
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
        },
    );

    ev.register_special(
        "ctfe_provider_fact_get",
        3,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-fact-get expects a provider context",
            )?;
            require_provider_effect(ctx, BuiltinEffectTag::ReadFacts)?;
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
        },
    );

    ev.register_special(
        "ctfe_provider_fact_set",
        4,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-fact-set expects a provider context",
            )?;
            require_provider_effect(ctx, BuiltinEffectTag::WriteFacts)?;
            let namespace = require_string(&args[1], "ctfe-provider-fact-set expects a namespace")?;
            let node_id = require_node_id(&args[2], "ctfe-provider-fact-set expects a node id")?;
            ctx.track_fact_write(node_id, &namespace);
            let value = runtime_to_semantic(&args[3])?;
            ctx.validate_fact_value(&namespace, &value)
                .map_err(eval_err)?;
            ctx.unit()
                .with_unit_mut(|unit| {
                    unit.semantics_mut()?
                        .set_fact(node_subject_id(node_id), namespace, value)
                })
                .map_err(eval_err)?;
            Ok(args[3].clone())
        },
    );

    ev.register_special(
        "ctfe_provider_annotation_get",
        3,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-annotation-get expects a provider context",
            )?;
            require_provider_effect(ctx, BuiltinEffectTag::ReadAttributes)?;
            let node_id =
                require_node_id(&args[1], "ctfe-provider-annotation-get expects a node id")?;
            let key = require_string(
                &args[2],
                "ctfe-provider-annotation-get expects a non-empty key",
            )?;
            ctx.track_annotation_read(node_id, &key);
            let predicate = annotation_tracking_predicate(&key);
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
        },
    );

    ev.register_special(
        "ctfe_provider_annotation_set",
        4,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-annotation-set expects a provider context",
            )?;
            require_provider_effect(ctx, BuiltinEffectTag::WriteAttributes)?;
            let node_id =
                require_node_id(&args[1], "ctfe-provider-annotation-set expects a node id")?;
            let key = require_string(
                &args[2],
                "ctfe-provider-annotation-set expects a non-empty key",
            )?;
            ctx.track_annotation_write(node_id, &key);
            let value = runtime_to_semantic(&args[3])?;
            let predicate = annotation_tracking_predicate(&key);
            ctx.unit()
                .with_unit_mut(|unit| {
                    unit.semantics_mut()?
                        .set_fact(node_subject_id(node_id), predicate, value)
                })
                .map_err(eval_err)?;
            Ok(args[3].clone())
        },
    );

    ev.register_special(
        "ctfe_provider_fold_compile_time_call",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_effects([
            BuiltinEffectTag::ReadIr,
            BuiltinEffectTag::ReadFacts,
            BuiltinEffectTag::ReadSymbols,
            BuiltinEffectTag::WriteIr,
        ]),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-fold-compile-time-call expects a provider context",
            )?;
            for effect in [
                BuiltinEffectTag::ReadIr,
                BuiltinEffectTag::ReadFacts,
                BuiltinEffectTag::ReadSymbols,
                BuiltinEffectTag::WriteIr,
            ] {
                require_provider_effect(ctx, effect)?;
            }
            let node_id = require_node_id(
                &args[1],
                "ctfe-provider-fold-compile-time-call expects a call node",
            )?;
            fold_compile_time_call(ev, ctx, node_id)
        },
    );

    ev.register_special(
        "ctfe_provider_evaluate_call!",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_effects([
            BuiltinEffectTag::ReadIr,
            BuiltinEffectTag::WriteIr,
        ]),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-evaluate-call! expects a provider context",
            )?;
            require_provider_effect(ctx, BuiltinEffectTag::WriteIr)?;
            let call_node =
                require_node_id(&args[1], "ctfe-provider-evaluate-call! expects a call node")?;
            let lambda_node = require_node_id(
                &args[2],
                "ctfe-provider-evaluate-call! expects a lambda node",
            )?;
            evaluate_internal_call(ev, ctx, call_node, lambda_node)
        },
    );

    ev.register_special(
        "ctfe_provider_synthesize_internal_definition!",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_effects([
            BuiltinEffectTag::WriteIr,
            BuiltinEffectTag::WriteSymbols,
        ]),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-synthesize-internal-definition! expects a provider context",
            )?;
            for effect in [BuiltinEffectTag::WriteIr, BuiltinEffectTag::WriteSymbols] {
                require_provider_effect(ctx, effect)?;
            }
            let name = require_string(
                &args[1],
                "ctfe-provider-synthesize-internal-definition! expects a name string",
            )?;
            let value = crate::builtins::ir_builders::require_expr_spec(
                &args[2],
                "ctfe-provider-synthesize-internal-definition! expects a value spec",
            )?;
            synthesize_internal_definition(ctx, &name, value)
        },
    );

    register_diagnostic_builtin(
        ev,
        "ctfe_provider_diagnostics_error",
        DiagnosticSeverity::Error,
    );
    register_diagnostic_builtin(
        ev,
        "ctfe_provider_diagnostics_warning",
        DiagnosticSeverity::Warning,
    );
    register_diagnostic_builtin(
        ev,
        "ctfe_provider_diagnostics_note",
        DiagnosticSeverity::Note,
    );
    register_diagnostic_builtin(
        ev,
        "ctfe_provider_diagnostics_hint",
        DiagnosticSeverity::Hint,
    );
    ev.register_special(
        "ctfe_provider_node_replace",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_write_ir(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-node-replace expects a provider context",
            )?;
            require_provider_effect(ctx, BuiltinEffectTag::WriteIr)?;
            let node_id = require_node_id(&args[1], "ctfe-provider-node-replace expects a node")?;
            let spec = require_expr_spec(
                &args[2],
                "ctfe-provider-node-replace expects an expression spec replacement",
            )?;
            let new_id = ctx
                .unit()
                .with_unit_mut(|unit| -> CaapResult<NodeId> {
                    let new_id = unit.replace_ir_subtree_with_spec(node_id, &spec)?;
                    record_provider_rewrite(ctx, unit, "replace", [new_id], [node_id])?;
                    Ok(new_id)
                })
                .map_err(eval_err)?;
            Ok(node_handle(ctx, new_id))
        },
    );

    ev.register_special(
        "ctfe_provider_node_rewrite",
        4,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_effects([
            BuiltinEffectTag::ReadIr,
            BuiltinEffectTag::WriteIr,
        ]),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-node-rewrite expects a provider context",
            )?;
            require_provider_effect(ctx, BuiltinEffectTag::ReadIr)?;
            require_provider_effect(ctx, BuiltinEffectTag::WriteIr)?;
            let node_id = require_node_id(&args[1], "ctfe-provider-node-rewrite expects a node")?;
            let node = node_handle(ctx, node_id);
            let match_result = ctfe_node_match(&node, &args[2])?;
            if !matches!(
                map_get_str(&match_result, "matched"),
                Some(RuntimeValue::Bool(true))
            ) {
                return Ok(rt_map([
                    ("matched", RuntimeValue::Bool(false)),
                    ("rewritten", RuntimeValue::Bool(false)),
                    ("node", node),
                    (
                        "bindings",
                        map_get_str(&match_result, "bindings").unwrap_or(RuntimeValue::Null),
                    ),
                    ("replacement", RuntimeValue::Null),
                ]));
            }

            let bindings = map_get_str(&match_result, "bindings").unwrap_or(RuntimeValue::Null);
            let replacement_value =
                ev.invoke_callback(&args[3], vec![bindings.clone(), node.clone()])?;
            let replacement = require_expr_spec(
                &replacement_value,
                "ctfe-provider-node-rewrite callback must return an expression spec",
            )?;
            let new_id = ctx
                .unit()
                .with_unit_mut(|unit| -> CaapResult<NodeId> {
                    let new_id = unit.replace_ir_subtree_with_spec(node_id, &replacement)?;
                    record_provider_rewrite(ctx, unit, "rewrite", [new_id], [node_id])?;
                    Ok(new_id)
                })
                .map_err(eval_err)?;
            Ok(rt_map([
                ("matched", RuntimeValue::Bool(true)),
                ("rewritten", RuntimeValue::Bool(true)),
                ("node", node),
                ("bindings", bindings),
                ("replacement", node_handle(ctx, new_id)),
            ]))
        },
    );

    ev.register_special(
        "ctfe_provider_node_erase",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_write_ir(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "ctfe-provider-node-erase expects a provider context",
            )?;
            require_provider_effect(ctx, BuiltinEffectTag::WriteIr)?;
            let node_id = require_node_id(&args[1], "ctfe-provider-node-erase expects a node")?;
            ctx.unit()
                .with_unit_mut(|unit| -> CaapResult<Vec<NodeId>> {
                    record_provider_erase(ctx, unit, node_id)?;
                    unit.erase_ir_subtree(node_id)
                })
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        },
    );
}

fn register_diagnostic_builtin(
    ev: &mut Evaluator,
    name: &'static str,
    severity: DiagnosticSeverity,
) {
    ev.register_special(
        name.to_string(),
        3,
        Some(6),
        crate::values::BuiltinMetadata::compile_time_emit_diagnostics(),
        move |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ctx = require_provider_context(
                &args[0],
                "provider diagnostic expects a provider context",
            )?;
            emit_provider_diagnostic(
                ctx,
                ProviderDiagnosticInput {
                    severity,
                    node_value: &args[1],
                    message_value: &args[2],
                    code_value: args.get(3),
                    notes_value: args.get(4),
                    fixes_value: args.get(5),
                },
            )
        },
    );
}
