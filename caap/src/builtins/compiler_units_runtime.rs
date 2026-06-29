/// Unit compiler CTFE builtins for the currently supportable compiler-unit surface.
use std::rc::Rc;

use crate::bridges::NodeBridgeValue;
use crate::builtins::ir_builders::ExprSpecBridgeValue;
use crate::compiler::UnitBridgeValue;
use crate::error::CaapError;
use crate::eval::{eval_args, Evaluator};
use crate::ir::{ExprSpec, Node};
use crate::semantic::{
    node_subject_id, BuiltinEffectTag, PhasePolicy, SemanticValue, SymbolEntry, SymbolKind,
};
use crate::syntax_authoring;
use crate::unit::{LinkBinding, RewriteRecord, RewriteTombstone, Unit, UnitTemplate};
use crate::values::{
    eval_err, runtime_value_from_literal, BuiltinInfo, EvalSignal, HostObject, RuntimeValue,
};

use crate::compiler::annotation_tracking_predicate;

use super::compiler_node_match::ctfe_node_match;
use super::compiler_units_helpers::{
    add_public_name, call_semantics_from_entry_value, expr_spec_bridge, expr_spec_children,
    expr_spec_kind_label, link_binding_to_value, map, map_entries, meta_fact_get_by_key,
    meta_fact_has_by_key, node_bridge, node_handle, node_handle_from_live_node_id, node_id_arg,
    node_id_in_unit, node_id_sequence_in_unit, node_kind_label, optional_map_bool,
    optional_node_id, optional_phase, optional_symbol_kind, require_direct_unit_effect,
    require_expr_spec, require_node_bridge, require_semantic_entry, require_string,
    require_unit_bridge, require_unit_bridge_object, required_map_string,
    resolved_block_fact_node_id, resolved_name_fact_entry, runtime_to_semantic,
    semantic_entry_handle, semantic_value_to_runtime, semantic_value_to_runtime_in_unit,
    set_symbol_semantics, short_circuit_policy_label, source_span_to_value, spec_value, string,
    symbol_entry_to_value, symbol_semantics_updates, track_direct_annotation_read,
    track_direct_annotation_write, track_direct_fact_subject_read, track_direct_fact_write,
    track_direct_node_write, track_direct_symbol_read, track_direct_symbol_write,
    track_direct_unit_fact_table_read, track_direct_unit_ir_read, track_direct_unit_ir_write,
    track_direct_unit_symbol_table_read, track_direct_unit_symbol_table_write, tuple,
    unit_bridge_from_object, unit_origin_stage_value, with_node,
};
use super::semantic_projection::builtin_policy_projection_fields;

#[derive(Debug)]
pub(super) struct UnitTemplateBridgeValue {
    pub(super) template: UnitTemplate,
}

#[derive(Clone, Debug)]
struct CallSemanticsProjection {
    value: RuntimeValue,
}

impl CallSemanticsProjection {
    fn from_builtin(info: &BuiltinInfo) -> Self {
        let metadata = info.metadata();
        let mut fields = vec![("callee_class", string("builtin"))];
        fields.extend(builtin_policy_projection_fields(&metadata));
        fields.extend([
            (
                "short_circuit_policy",
                string(short_circuit_policy_label(&info.name)),
            ),
            ("builtin_name", string(&info.name)),
            ("min_arity", RuntimeValue::Int(info.min_arity as i64)),
            (
                "max_arity",
                info.max_arity
                    .map(|arity| RuntimeValue::Int(arity as i64))
                    .unwrap_or(RuntimeValue::Null),
            ),
        ]);
        Self {
            value: map_entries(fields),
        }
    }

    fn from_semantic(value: &SemanticValue) -> Result<Self, EvalSignal> {
        let value = semantic_value_to_runtime(value);
        if !matches!(value, RuntimeValue::Map(_)) {
            return Err(eval_err("call semantics fact must contain a semantic map"));
        }
        Ok(Self { value })
    }

    fn value(&self) -> RuntimeValue {
        self.value.clone()
    }
}

impl HostObject for UnitTemplateBridgeValue {
    fn type_name(&self) -> &'static str {
        "unit_template"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn require_unit_template_bridge<'a>(
    value: &'a RuntimeValue,
    message: &str,
) -> Result<&'a UnitTemplateBridgeValue, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    object
        .as_any()
        .downcast_ref::<UnitTemplateBridgeValue>()
        .ok_or_else(|| eval_err(message))
}

fn call_semantics_projection(
    ev: &Evaluator,
    value: &RuntimeValue,
    fact_key: &str,
    builtin_name: &str,
) -> Result<Option<CallSemanticsProjection>, EvalSignal> {
    with_node(
        value,
        &format!("{builtin_name} expects a live Call node"),
        |_, unit, node| {
            let Node::Call(call) = node else {
                return Err(eval_err(format!("{builtin_name} expects a live Call node")));
            };
            let subject = node_subject_id(call.id);
            if let Some(semantics) = unit
                .semantics()
                .get_fact(&subject, fact_key)
                .map_err(eval_err)?
            {
                return CallSemanticsProjection::from_semantic(semantics).map(Some);
            }
            let Some(Node::Name(callee)) = unit.ir().node(call.callee) else {
                return Ok(None);
            };
            Ok(ev
                .builtin_info(&callee.identifier)
                .map(CallSemanticsProjection::from_builtin))
        },
    )
}

pub fn register(ev: &mut Evaluator) {
    register_node(ev);
    register_unit(ev);
    register_meta(ev);
}

fn register_node(ev: &mut Evaluator) {
    ev.register_special(
        "ctfe_node_kind",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                return Ok(string(expr_spec_kind_label(&spec.spec())));
            }
            with_node(
                &args[0],
                "ctfe-node-kind expects a live node or detached node spec",
                |_, _, node| Ok(string(node_kind_label(node))),
            )
        },
    );
    ev.register_special(
        "ctfe_node_is_call",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                return Ok(RuntimeValue::Bool(matches!(spec.spec(), ExprSpec::Call(_))));
            }
            with_node(
                &args[0],
                "ctfe-node-is-call expects a live node or detached node spec",
                |_, _, node| Ok(RuntimeValue::Bool(matches!(node, Node::Call(_)))),
            )
        },
    );
    ev.register_special(
        "ctfe_node_is_name",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                return Ok(RuntimeValue::Bool(matches!(spec.spec(), ExprSpec::Name(_))));
            }
            with_node(
                &args[0],
                "ctfe-node-is-name expects a live node or detached node spec",
                |_, _, node| Ok(RuntimeValue::Bool(matches!(node, Node::Name(_)))),
            )
        },
    );
    ev.register_special(
        "ctfe_node_is_literal",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                return Ok(RuntimeValue::Bool(matches!(
                    &spec.spec(),
                    ExprSpec::Literal(_)
                )));
            }
            with_node(
                &args[0],
                "ctfe-node-is-literal expects a live node or detached node spec",
                |_, _, node| Ok(RuntimeValue::Bool(matches!(node, Node::Literal(_)))),
            )
        },
    );
    ev.register_special(
        "ctfe_node_live?",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let Some(node) = node_bridge(&args[0]) else {
                return Ok(RuntimeValue::Bool(false));
            };
            Ok(RuntimeValue::Bool(node_is_live(node)?))
        },
    );
    ev.register_special(
        "ctfe_node_id",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if expr_spec_bridge(&args[0]).is_some() {
                return Ok(RuntimeValue::Null);
            }
            let node = require_node_bridge(&args[0], "ctfe-node-id expects a live node")?;
            Ok(RuntimeValue::Int(node.node_id as i64))
        },
    );
    ev.register_special(
        "ctfe_node_parent",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if expr_spec_bridge(&args[0]).is_some() {
                return Ok(RuntimeValue::Null);
            }
            let node = require_node_bridge(&args[0], "ctfe-node-parent expects a live node")?;
            let unit = unit_bridge_from_object(&node.unit, "ctfe-node-parent expects a live node")?;
            unit.with_unit(|unit| {
                if unit.ir().node(node.node_id).is_none() {
                    return Ok(RuntimeValue::Null);
                }
                match unit.ir().parent(node.node_id).flatten() {
                    Some(parent_id) => node_handle(Rc::clone(&node.unit), parent_id),
                    None => Ok(RuntimeValue::Null),
                }
            })
        },
    );
    ev.register_special(
        "ctfe_node_ancestor?",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let node = require_node_bridge(&args[0], "ctfe-node-ancestor? expects a live node")?;
            let ancestor =
                require_node_bridge(&args[1], "ctfe-node-ancestor? expects a live ancestor node")?;
            if !Rc::ptr_eq(&node.unit, &ancestor.unit) {
                return Ok(RuntimeValue::Bool(false));
            }
            let unit =
                unit_bridge_from_object(&node.unit, "ctfe-node-ancestor? expects a live node")?;
            Ok(RuntimeValue::Bool(unit.with_unit(|unit| {
                let mut current = unit.ir().parent(node.node_id).flatten();
                while let Some(parent_id) = current {
                    if parent_id == ancestor.node_id {
                        return true;
                    }
                    current = unit.ir().parent(parent_id).flatten();
                }
                false
            })))
        },
    );
    ev.register_special(
        "ctfe_node_children",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                return Ok(tuple(
                    expr_spec_children(&spec.spec())
                        .into_iter()
                        .map(spec_value)
                        .collect(),
                ));
            }
            with_node(
                &args[0],
                "ctfe-node-children expects a live node or detached node spec",
                |node_handle, _, node| {
                    Ok(tuple(
                        node.children()
                            .iter()
                            .map(|node_id| {
                                node_handle_from_live_node_id(
                                    Rc::clone(&node_handle.unit),
                                    *node_id,
                                )
                            })
                            .collect(),
                    ))
                },
            )
        },
    );
    ev.register_special(
        "ctfe_node_match",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            ctfe_node_match(&args[0], &args[1])
        },
    );
    ev.register_special(
        "ctfe_node_call_callee",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                let ExprSpec::Call(call) = spec.spec() else {
                    return Err(eval_err("ctfe-node-call-callee expects a Call node"));
                };
                return Ok(spec_value((*call.callee).clone()));
            }
            with_node(
                &args[0],
                "ctfe-node-call-callee expects a Call node",
                |node_handle, _, node| {
                    let Node::Call(call) = node else {
                        return Err(eval_err("ctfe-node-call-callee expects a Call node"));
                    };
                    Ok(node_handle_from_live_node_id(
                        Rc::clone(&node_handle.unit),
                        call.callee,
                    ))
                },
            )
        },
    );
    ev.register_special(
        "ctfe_node_call_args",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                let ExprSpec::Call(call) = spec.spec() else {
                    return Err(eval_err("ctfe-node-call-args expects a Call node"));
                };
                return Ok(tuple(call.args.iter().cloned().map(spec_value).collect()));
            }
            with_node(
                &args[0],
                "ctfe-node-call-args expects a Call node",
                |node_handle, _, node| {
                    let Node::Call(call) = node else {
                        return Err(eval_err("ctfe-node-call-args expects a Call node"));
                    };
                    Ok(tuple(
                        call.args
                            .iter()
                            .map(|node_id| {
                                node_handle_from_live_node_id(
                                    Rc::clone(&node_handle.unit),
                                    *node_id,
                                )
                            })
                            .collect(),
                    ))
                },
            )
        },
    );
    ev.register_special(
        "ctfe_node_call_semantics",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let fact_key = require_string(
                &args[1],
                "ctfe-node-call-semantics expects a fact key string",
            )?;
            Ok(
                call_semantics_projection(ev, &args[0], &fact_key, "ctfe_node_call_semantics")?
                    .map(|semantics| semantics.value())
                    .unwrap_or(RuntimeValue::Null),
            )
        },
    );
    ev.register_special(
        "ctfe_node_to_spec",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let node = require_node_bridge(&args[0], "ctfe-node-to-spec expects a live node")?;
            let unit =
                unit_bridge_from_object(&node.unit, "ctfe-node-to-spec expects a live node")?;
            let spec = unit
                .with_unit(|unit| unit.ir().expr_spec_for_subtree(node.node_id))
                .map_err(eval_err)?;
            Ok(RuntimeValue::HostObject(Rc::new(ExprSpecBridgeValue::new(
                spec,
            ))))
        },
    );
    ev.register_special(
        "ctfe_node_resolved_block",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let node =
                require_node_bridge(&args[0], "ctfe-node-resolved-block expects a live node")?;
            let fact_key = require_string(
                &args[1],
                "ctfe-node-resolved-block expects a fact key string",
            )?;
            let default = args.get(2).cloned().unwrap_or(RuntimeValue::Null);
            let unit = unit_bridge_from_object(
                &node.unit,
                "ctfe-node-resolved-block expects a live node",
            )?;
            let block_id = unit
                .with_unit(|unit| {
                    unit.semantics()
                        .get_fact(&node_subject_id(node.node_id), &fact_key)
                        .map(resolved_block_fact_node_id)
                })
                .map_err(eval_err)?
                .transpose()?;
            match block_id {
                Some(block_id) if unit.with_unit(|unit| unit.ir().node(block_id).is_some()) => Ok(
                    node_handle_from_live_node_id(Rc::clone(&node.unit), block_id),
                ),
                _ => Ok(default),
            }
        },
    );
    ev.register_special(
        "ctfe_node_resolved_name_entry",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let node = require_node_bridge(
                &args[0],
                "ctfe-node-resolved-name-entry expects a live node",
            )?;
            let fact_key = require_string(
                &args[1],
                "ctfe-node-resolved-name-entry expects a fact key string",
            )?;
            let default = args.get(2).cloned().unwrap_or(RuntimeValue::Null);
            let unit = unit_bridge_from_object(
                &node.unit,
                "ctfe-node-resolved-name-entry expects a live node",
            )?;
            let entry = unit
                .with_unit(|unit| {
                    unit.semantics()
                        .get_fact(&node_subject_id(node.node_id), &fact_key)
                        .map(resolved_name_fact_entry)
                })
                .map_err(eval_err)?
                .transpose()?;
            Ok(entry.map(semantic_entry_handle).unwrap_or(default))
        },
    );
    ev.register_special(
        "ctfe_call_semantics_from_entry",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let entry = require_semantic_entry(
                &args[0],
                "ctfe-call-semantics-from-entry expects a resolved semantic entry",
            )?;
            Ok(call_semantics_from_entry_value(ev, entry))
        },
    );
    ev.register_special(
        "ctfe_node_name_identifier",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                let ExprSpec::Name(name) = spec.spec() else {
                    return Err(eval_err("ctfe-node-name-identifier expects a Name node"));
                };
                return Ok(string(name.identifier.as_str()));
            }
            with_node(
                &args[0],
                "ctfe-node-name-identifier expects a Name node",
                |_, _, node| {
                    let Node::Name(name) = node else {
                        return Err(eval_err("ctfe-node-name-identifier expects a Name node"));
                    };
                    Ok(string(name.identifier.as_ref()))
                },
            )
        },
    );
    ev.register_special(
        "ctfe_node_literal_value",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                let ExprSpec::Literal(literal) = spec.spec() else {
                    return Err(eval_err("ctfe-node-literal-value expects a Literal node"));
                };
                return Ok(runtime_value_from_literal(&literal.value));
            }
            with_node(
                &args[0],
                "ctfe-node-literal-value expects a Literal node",
                |_, _, node| {
                    let Node::Literal(literal) = node else {
                        return Err(eval_err("ctfe-node-literal-value expects a Literal node"));
                    };
                    Ok(runtime_value_from_literal(&literal.value))
                },
            )
        },
    );
}

fn register_unit(ev: &mut Evaluator) {
    ev.register_special(
        "ctfe_unit_id",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(&args[0], "ctfe-unit-id expects a unit handle")?;
            Ok(unit.with_unit(|unit| string(unit.unit_id())))
        },
    );
    ev.register_special(
        "ctfe_unit_root",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit_object =
                require_unit_bridge_object(&args[0], "ctfe-unit-root expects a unit handle")?;
            let unit =
                unit_bridge_from_object(&unit_object, "ctfe-unit-root expects a unit handle")?;
            let root_id = unit.with_unit(|unit| unit.root_id());
            track_direct_unit_ir_read(unit, [root_id])?;
            node_handle(Rc::clone(&unit_object), root_id)
        },
    );
    ev.register_special(
        "ctfe_unit_version",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(&args[0], "ctfe-unit-version expects a unit handle")?;
            Ok(unit.with_unit(|unit| RuntimeValue::Int(unit.version() as i64)))
        },
    );
    ev.register_special(
        "ctfe_unit_top_level_forms",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-top-level-forms expects a unit handle")?;
            let unit_object = require_unit_bridge_object(
                &args[0],
                "ctfe-unit-top-level-forms expects a unit handle",
            )?;
            let form_ids = unit.with_unit(|unit| unit.top_level_form_ids().to_vec());
            track_direct_unit_ir_read(unit, form_ids.iter().copied())?;
            Ok(tuple(
                form_ids
                    .iter()
                    .map(|node_id| node_handle_from_live_node_id(Rc::clone(&unit_object), *node_id))
                    .collect(),
            ))
        },
    );
    ev.register_special(
        "ctfe_unit_set_top_level_forms!",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-set-top-level-forms! expects a unit handle",
            )?;
            let unit_object = require_unit_bridge_object(
                &args[0],
                "ctfe-unit-set-top-level-forms! expects a unit handle",
            )?;
            let form_ids = node_id_sequence_in_unit(
                &args[1],
                &unit_object,
                "ctfe-unit-set-top-level-forms! expects live nodes from the same unit",
            )?;
            track_direct_unit_ir_write(unit)?;
            for node_id in &form_ids {
                track_direct_node_write(unit, *node_id)?;
            }
            unit.with_unit_mut(|unit| unit.set_ir_top_level_form_ids(form_ids))
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        },
    );
    ev.register_special(
        "ctfe_unit_set_root!",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(&args[0], "ctfe-unit-set-root! expects a unit handle")?;
            let unit_object =
                require_unit_bridge_object(&args[0], "ctfe-unit-set-root! expects a unit handle")?;
            let root_id = node_id_in_unit(
                &args[1],
                &unit_object,
                "ctfe-unit-set-root! expects a live node from the same unit",
            )?;
            track_direct_node_write(unit, root_id)?;
            unit.with_unit_mut(|unit| unit.set_ir_root_id(root_id))
                .map_err(eval_err)?;
            Ok(args[1].clone())
        },
    );
    ev.register_special(
        "ctfe_unit_append_top_level!",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-append-top-level! expects a unit handle",
            )?;
            let unit_object = require_unit_bridge_object(
                &args[0],
                "ctfe-unit-append-top-level! expects a unit handle",
            )?;
            let spec = require_expr_spec(
                &args[1],
                "ctfe-unit-append-top-level! expects an expression spec",
            )?;
            require_direct_unit_effect(unit, BuiltinEffectTag::WriteIr)?;
            let node_id = unit
                .with_unit_mut(|unit| unit.append_ir_top_level_with_spec(&spec))
                .map_err(eval_err)?;
            track_direct_node_write(unit, node_id)?;
            Ok(node_handle_from_live_node_id(unit_object, node_id))
        },
    );
    ev.register_special(
        "ctfe_unit_erase_detached!",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-erase-detached! expects a unit handle")?;
            let unit_object = require_unit_bridge_object(
                &args[0],
                "ctfe-unit-erase-detached! expects a unit handle",
            )?;
            let node_id = node_id_in_unit(
                &args[1],
                &unit_object,
                "ctfe-unit-erase-detached! expects a live node from the same unit",
            )?;
            track_direct_node_write(unit, node_id)?;
            let erased = unit
                .with_unit_mut(|unit| unit.erase_ir_subtree(node_id))
                .map_err(eval_err)?;
            Ok(tuple(
                erased
                    .into_iter()
                    .map(|node_id| RuntimeValue::Int(node_id as i64))
                    .collect(),
            ))
        },
    );
    ev.register_special(
        "ctfe_unit_facts",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(&args[0], "ctfe-unit-facts expects a unit handle")?;
            let facts = unit
                .with_unit(|unit| unit.semantics().query_facts(None, None))
                .map_err(eval_err)?;
            track_direct_unit_fact_table_read(unit)?;
            for (subject, predicate, _) in &facts {
                track_direct_fact_subject_read(unit, subject, predicate)?;
            }
            Ok(tuple(
                facts
                    .iter()
                    .map(|(subject, predicate, value)| {
                        tuple(vec![
                            string(format!("{}:{}", subject.kind, subject.value)),
                            string(predicate),
                            semantic_value_to_runtime(value),
                        ])
                    })
                    .collect(),
            ))
        },
    );
    ev.register_special(
        "ctfe_unit_top_level_symbols",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-top-level-symbols expects a unit handle",
            )?;
            let entries: Vec<SymbolEntry> = unit.with_unit(|unit| {
                unit.semantics()
                    .symbols()
                    .values()
                    .filter(|entry| entry.kind == SymbolKind::TopLevel)
                    .cloned()
                    .collect()
            });
            track_direct_unit_symbol_table_read(unit)?;
            for entry in &entries {
                track_direct_symbol_read(unit, &entry.name)?;
            }
            Ok(tuple(entries.iter().map(symbol_entry_to_value).collect()))
        },
    );
    ev.register_special(
        "ctfe_unit_symbols",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(&args[0], "ctfe-unit-symbols expects a unit handle")?;
            let entries: Vec<SymbolEntry> =
                unit.with_unit(|unit| unit.semantics().symbols().values().cloned().collect());
            track_direct_unit_symbol_table_read(unit)?;
            for entry in &entries {
                track_direct_symbol_read(unit, &entry.name)?;
            }
            Ok(tuple(entries.iter().map(symbol_entry_to_value).collect()))
        },
    );
    ev.register_special(
        "ctfe_unit_dependency_bindings",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-dependency-bindings expects a unit handle",
            )?;
            let bindings: Vec<LinkBinding> = unit.with_unit(|unit| unit.link_bindings().to_vec());
            track_direct_unit_symbol_table_read(unit)?;
            for binding in &bindings {
                track_direct_symbol_read(unit, &binding.local_name)?;
            }
            Ok(tuple(bindings.iter().map(link_binding_to_value).collect()))
        },
    );
    ev.register_special(
        "ctfe_unit_exposed_names",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-exposed-names expects a unit handle")?;
            let names = unit.with_unit(|unit| {
                let mut names: Vec<String> = unit
                    .semantics()
                    .symbols()
                    .values()
                    .flat_map(|entry| entry.public_names.iter().cloned())
                    .collect();
                names.sort();
                names
            });
            track_direct_unit_symbol_table_read(unit)?;
            for name in &names {
                track_direct_symbol_read(unit, name)?;
            }
            Ok(tuple(names.into_iter().map(string).collect()))
        },
    );
    ev.register_special(
        "ctfe_unit_node_location",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-node-location expects a unit handle")?;
            let node_id = node_id_arg(&args[1], "ctfe-unit-node-location expects a node id")?;
            track_direct_unit_ir_read(unit, [node_id])?;
            unit.with_unit(|unit| {
                if unit.ir().node(node_id).is_none() {
                    return Err(eval_err(format!("unknown node id: {node_id}")));
                }
                Ok(tuple(vec![
                    string(unit.unit_id()),
                    RuntimeValue::Int(node_id as i64),
                ]))
            })
        },
    );
    // Optional source location for a node: the byte/line/col span when one is
    // attached (hand-written forms carry it), or null for span-less synthetic
    // nodes (surface/macro/derive-generated). Presentational only — `node_id`
    // stays the stable identity; do not key caching/facts on these coordinates.
    ev.register_special(
        "ctfe_unit_node_span",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(&args[0], "ctfe-unit-node-span expects a unit handle")?;
            let node_id = node_id_arg(&args[1], "ctfe-unit-node-span expects a node id")?;
            track_direct_unit_ir_read(unit, [node_id])?;
            unit.with_unit(|unit| {
                if unit.ir().node(node_id).is_none() {
                    return Err(eval_err(format!("unknown node id: {node_id}")));
                }
                Ok(match unit.ir().source_span(node_id) {
                    Some(span) => source_span_to_value(span),
                    None => RuntimeValue::Null,
                })
            })
        },
    );
    ev.register_special(
        "ctfe_unit_to_template",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-to-template expects a unit handle")?;
            let ir_ids = unit.with_unit(|unit| {
                let mut ids = vec![unit.root_id()];
                ids.extend(unit.top_level_form_ids().iter().copied());
                ids
            });
            let facts = unit
                .with_unit(|unit| unit.semantics().query_facts(None, None))
                .map_err(eval_err)?;
            let symbols: Vec<String> = unit.with_unit(|unit| {
                unit.semantics()
                    .symbols()
                    .values()
                    .map(|entry| entry.name.clone())
                    .collect()
            });
            track_direct_unit_ir_read(unit, ir_ids)?;
            track_direct_unit_fact_table_read(unit)?;
            for (subject, predicate, _) in &facts {
                track_direct_fact_subject_read(unit, subject, predicate)?;
            }
            track_direct_unit_symbol_table_read(unit)?;
            for symbol in &symbols {
                track_direct_symbol_read(unit, symbol)?;
            }
            require_direct_unit_effect(unit, BuiltinEffectTag::ReadAttributes)?;
            Ok(unit.with_unit(|unit| {
                RuntimeValue::HostObject(Rc::new(UnitTemplateBridgeValue {
                    template: unit.to_template(),
                }))
            }))
        },
    );
    ev.register_special(
        "ctfe_unit_template_instantiate",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let template = require_unit_template_bridge(
                &args[0],
                "ctfe-unit-template-instantiate expects a unit template handle",
            )?;
            let unit = Unit::from_template(template.template.clone()).map_err(eval_err)?;
            Ok(RuntimeValue::HostObject(Rc::new(
                UnitBridgeValue::from_unit_snapshot(unit),
            )))
        },
    );
    ev.register_special(
        "ctfe_unit_declare_symbol!",
        2,
        Some(5),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-declare-symbol! expects a unit handle")?;
            let name = require_string(
                &args[1],
                "unit-declare-symbol! expects a non-empty symbol name",
            )?;
            let phase = optional_phase(
                args.get(2),
                PhasePolicy::Dual,
                "unit-declare-symbol! expects runtime, compile_time, or dual phase",
            )?;
            let node_id = optional_node_id(args.get(3), "unit-declare-symbol! expects a node id")?;
            let kind = optional_symbol_kind(args.get(4))?;
            track_direct_symbol_write(unit, &name)?;
            unit.with_unit_mut(|unit| {
                if let Some(node_id) = node_id {
                    if unit.ir().node(node_id).is_none() {
                        return Err(EvalSignal::from(CaapError::ir(format!(
                            "unknown node id: {node_id}"
                        ))));
                    }
                }
                let entry = SymbolEntry::new(name, kind, phase, node_id)?;
                unit.semantics_mut()?.define_symbol(entry)?;
                Ok::<(), EvalSignal>(())
            })?;
            Ok(RuntimeValue::Null)
        },
    );
    ev.register_special(
        "ctfe_unit_set_symbol_semantics!",
        3,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-set-symbol-semantics! expects a unit handle",
            )?;
            let name = require_string(
                &args[1],
                "unit-declare-symbol! expects a non-empty symbol name",
            )?;
            let semantics = runtime_to_semantic(&args[2])?;
            let updates = symbol_semantics_updates(&args[2])?;
            let node_id = optional_node_id(
                args.get(3),
                "ctfe-unit-set-symbol-semantics! expects a node id",
            )?;
            track_direct_symbol_write(unit, &name)?;
            unit.with_unit_mut(|unit| {
                if let Some(node_id) = node_id {
                    if unit.ir().node(node_id).is_none() {
                        return Err(CaapError::ir(format!("unknown node id: {node_id}")));
                    }
                }
                set_symbol_semantics(unit, name, semantics, updates, node_id)
            })
            .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        },
    );
    ev.register_special(
        "ctfe_unit_set_id!",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(&args[0], "ctfe-unit-set-id! expects a unit handle")?;
            let id = require_string(&args[1], "ctfe-unit-set-id! expects a non-empty id")?;
            track_direct_unit_symbol_table_write(unit)?;
            unit.with_unit_mut(|unit| unit.set_unit_id(id))
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        },
    );
    ev.register_special(
        "ctfe_unit_add_dependency_binding!",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-add-dependency-binding! expects a unit handle",
            )?;
            let binding = link_binding_arg(&args[1])?;
            track_direct_unit_symbol_table_write(unit)?;
            track_direct_symbol_write(unit, &binding.local_name)?;
            unit.with_unit_mut(|unit| unit.add_link_binding(binding))
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        },
    );
    ev.register_special(
        "ctfe_unit_add_exposed_name!",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-add-exposed-name! expects a unit handle",
            )?;
            let name = require_string(
                &args[1],
                "ctfe-unit-add-exposed-name! expects a non-empty name",
            )?;
            track_direct_symbol_write(unit, &name)?;
            unit.with_unit_mut(|unit| add_public_name(unit, name))
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        },
    );
    ev.register_special(
        "ctfe_unit_rewrite_report",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-rewrite-report expects a unit handle")?;
            unit.with_unit(|unit| rewrite_report(unit, &args[1]))
        },
    );
    ev.register_special(
        "ctfe_unit_syntax_rule_set!",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-syntax-rule-set! expects a unit handle")?;
            let name = require_string(
                &args[1],
                "ctfe-unit-syntax-rule-set! expects a non-empty rule name",
            )?;
            let rule = runtime_to_semantic(&args[2])?;
            unit.with_unit_mut(|unit| {
                let mut syntax = unit.syntax_state().clone();
                syntax.set_grammar_rule(name, rule)?;
                unit.set_syntax_state(syntax)?;
                Ok::<(), EvalSignal>(())
            })?;
            Ok(RuntimeValue::Null)
        },
    );
    // Record the formal parameters of a parametric rule (rule name → list of
    // param-name strings). Surface-grammar compilation emits a parametric
    // `[name, params…, "->"]` rule header for any rule registered here, so the
    // body can use `(param "x")` / `(call "rule" arg…)`.
    ev.register_special("ctfe_unit_syntax_rule_params_set!", 3, Some(3), crate::values::BuiltinMetadata::compile_time_compiler_registry(), |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-syntax-rule-params-set! expects a unit handle",
            )?;
            let name = require_string(
                &args[1],
                "ctfe-unit-syntax-rule-params-set! expects a non-empty rule name",
            )?;
            let params: Vec<String> = match runtime_to_semantic(&args[2])? {
                SemanticValue::List(items) => items
                    .into_iter()
                    .map(|item| match item {
                        SemanticValue::Str(value) => Ok(value),
                        _ => Err(eval_err(
                            "ctfe-unit-syntax-rule-params-set! expects a list of string parameter names",
                        )),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                _ => {
                    return Err(eval_err(
                        "ctfe-unit-syntax-rule-params-set! expects a list of string parameter names",
                    ))
                }
            };
            unit.with_unit_mut(|unit| {
                let mut syntax = unit.syntax_state().clone();
                syntax.set_grammar_rule_params(name, params)?;
                unit.set_syntax_state(syntax)?;
                Ok::<(), EvalSignal>(())
            })?;
            Ok(RuntimeValue::Null)
        });
    ev.register_special(
        "ctfe_unit_syntax_metadata_set!",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-syntax-metadata-set! expects a unit handle",
            )?;
            let key = require_string(
                &args[1],
                "ctfe-unit-syntax-metadata-set! expects a non-empty metadata key",
            )?;
            let value = runtime_to_semantic(&args[2])?;
            unit.with_unit_mut(|unit| {
                let mut syntax = unit.syntax_state().clone();
                syntax.set_grammar_metadata(key, value)?;
                unit.set_syntax_state(syntax)?;
                Ok::<(), EvalSignal>(())
            })?;
            Ok(RuntimeValue::Null)
        },
    );
    ev.register_special(
        "ctfe_unit_syntax_metadata_get",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-syntax-metadata-get expects a unit handle",
            )?;
            let key = require_string(
                &args[1],
                "ctfe-unit-syntax-metadata-get expects a non-empty metadata key",
            )?;
            Ok(unit.with_unit(|unit| {
                unit.syntax_state()
                    .grammar_metadata(&key)
                    .map(semantic_value_to_runtime)
                    .unwrap_or_else(|| args.get(2).cloned().unwrap_or(RuntimeValue::Null))
            }))
        },
    );
    ev.register_special(
        "ctfe_unit_syntax_hook_set_inline_node!",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-syntax-hook-set-inline-node! expects a unit handle",
            )?;
            let hook_ref = require_string(
                &args[1],
                "ctfe-unit-syntax-hook-set-inline-node! expects a non-empty hook ref",
            )?;
            let node = require_node_bridge(
                &args[2],
                "ctfe-unit-syntax-hook-set-inline-node! expects an implementation node",
            )?;
            let implementation_source = unit.with_unit(|unit| {
                let span = unit.ir().source_span(node.node_id).ok_or_else(|| {
                    eval_err("inline syntax hook implementation node has no source span")
                })?;
                let path = span
                    .path
                    .as_ref()
                    .or(unit.syntax_state().source_path.as_ref())
                    .ok_or_else(|| {
                        eval_err("inline syntax hook implementation node has no source path")
                    })?;
                let text = std::fs::read_to_string(path).map_err(|error| {
                    eval_err(format!("inline syntax hook source read failed: {error}"))
                })?;
                let slice = text
                    .get(span.start..span.end)
                    .ok_or_else(|| eval_err("inline syntax hook source span is out of bounds"))?;
                syntax_authoring::extract_inline_lambda_source(slice)
                    .map(str::to_string)
                    .map_err(eval_err)
            })?;
            unit.with_unit_mut(|unit| {
                let mut syntax = unit.syntax_state().clone();
                syntax_authoring::set_inline_syntax_hook_source(
                    &mut syntax,
                    &hook_ref,
                    &implementation_source,
                )?;
                unit.set_syntax_state(syntax)?;
                Ok::<(), EvalSignal>(())
            })?;
            Ok(RuntimeValue::Null)
        },
    );
    ev.register_special(
        "ctfe_unit_syntax_authoring_source_apply!",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-syntax-authoring-source-apply! expects a unit handle",
            )?;
            let source = require_string(
                &args[1],
                "ctfe-unit-syntax-authoring-source-apply! expects source text",
            )?;
            unit.with_unit_mut(|unit| {
                let mut syntax = unit.syntax_state().clone();
                syntax_authoring::apply_authoring_grammar_source(&mut syntax, &source)?;
                unit.set_syntax_state(syntax)?;
                Ok::<(), EvalSignal>(())
            })?;
            Ok(RuntimeValue::Null)
        },
    );
    ev.register_special(
        "ctfe_unit_syntax_rule_define!",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-syntax-rule-define! expects a unit handle",
            )?;
            let source = require_string(
                &args[1],
                "ctfe-unit-syntax-rule-define! expects source text",
            )?;
            let function_name = require_string(
                &args[2],
                "ctfe-unit-syntax-rule-define! expects a function name",
            )?;
            unit.with_unit_mut(|unit| {
                let mut syntax = unit.syntax_state().clone();
                syntax_authoring::define_authoring_syntax_rule(
                    &mut syntax,
                    &source,
                    &function_name,
                )?;
                unit.set_syntax_state(syntax)?;
                Ok::<(), EvalSignal>(())
            })?;
            Ok(RuntimeValue::Null)
        },
    );
    ev.register_special(
        "ctfe_unit_syntax_rule_define_inline_node!",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-syntax-rule-define-inline-node! expects a unit handle",
            )?;
            let source = require_string(
                &args[1],
                "ctfe-unit-syntax-rule-define-inline-node! expects source text",
            )?;
            let node = require_node_bridge(
                &args[2],
                "ctfe-unit-syntax-rule-define-inline-node! expects an implementation node",
            )?;
            let implementation_source = unit.with_unit(|unit| {
                let span = unit.ir().source_span(node.node_id).ok_or_else(|| {
                    eval_err("inline syntax implementation node has no source span")
                })?;
                let path = span
                    .path
                    .as_ref()
                    .or(unit.syntax_state().source_path.as_ref())
                    .ok_or_else(|| {
                        eval_err("inline syntax implementation node has no source path")
                    })?;
                let text = std::fs::read_to_string(path).map_err(|error| {
                    eval_err(format!("inline syntax source read failed: {error}"))
                })?;
                let slice = text.get(span.start..span.end).ok_or_else(|| {
                    eval_err("inline syntax implementation source span is out of bounds")
                })?;
                syntax_authoring::extract_inline_lambda_source(slice)
                    .map(str::to_string)
                    .map_err(eval_err)
            })?;
            unit.with_unit_mut(|unit| {
                let mut syntax = unit.syntax_state().clone();
                syntax_authoring::define_authoring_syntax_rule_inline_source(
                    &mut syntax,
                    &source,
                    &implementation_source,
                )?;
                unit.set_syntax_state(syntax)?;
                Ok::<(), EvalSignal>(())
            })?;
            Ok(RuntimeValue::Null)
        },
    );
}

fn register_meta(ev: &mut Evaluator) {
    ev.register_special(
        "ctfe_meta_annotation_get",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                let key = require_string(
                    &args[1],
                    "ctfe-meta-annotation-get expects a non-empty annotation key",
                )?;
                let default = args.get(2).cloned().unwrap_or(RuntimeValue::Null);
                return if key == "source_span" {
                    Ok(spec
                        .source_span()
                        .map(|span| crate::builtins::surface::span_to_value(&span))
                        .unwrap_or(default))
                } else {
                    Ok(default)
                };
            }
            let node =
                require_node_bridge(&args[0], "ctfe-meta-annotation-get expects a live node")?;
            let key = require_string(
                &args[1],
                "ctfe-meta-annotation-get expects a non-empty annotation key",
            )?;
            let default = args.get(2).cloned().unwrap_or(RuntimeValue::Null);
            let unit = unit_bridge_from_object(
                &node.unit,
                "ctfe-meta-annotation-get expects a live node",
            )?;
            track_direct_annotation_read(unit, node.node_id, &key)?;
            unit.with_unit(|unit| {
                let fact = unit
                    .semantics()
                    .get_fact(
                        &node_subject_id(node.node_id),
                        &annotation_tracking_predicate(&key),
                    )
                    .map(|value| {
                        value.map(|value| semantic_value_to_runtime_in_unit(&node.unit, value))
                    });
                let from_facts: Option<RuntimeValue> = match fact {
                    Ok(value) => value,
                    Err(error) => return Err(error),
                };
                // For source_span specifically, never short-circuit on a
                // semantic-fact null override: the compiler may clear the
                // annotation during normalization, but the raw graph still
                // carries the lowering-time span we want for surface tooling.
                let from_facts = match from_facts {
                    Some(RuntimeValue::Null) if key == "source_span" => None,
                    other => other,
                };
                if let Some(value) = from_facts {
                    return Ok(value);
                }
                if key == "source_span" {
                    if let Some(span) = unit.ir().source_span(node.node_id) {
                        return Ok(crate::builtins::surface::span_to_value(span));
                    }
                }
                Ok(default)
            })
            .map_err(eval_err)
        },
    );
    ev.register_special(
        "ctfe_meta_annotation_set",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                let key = require_string(
                    &args[1],
                    "ctfe-meta-annotation-set expects a non-empty annotation key",
                )?;
                if key != "source_span" {
                    return Err(eval_err(
                        "ctfe-meta-annotation-set on detached specs only supports source_span",
                    ));
                }
                let span = match &args[2] {
                    RuntimeValue::Null => None,
                    value => Some(crate::builtins::surface::require_span_map(
                        value,
                        "ctfe-meta-annotation-set source_span expects SourceSpan or null",
                    )?),
                };
                spec.set_source_span(span);
                return Ok(args[0].clone());
            }
            let node =
                require_node_bridge(&args[0], "ctfe-meta-annotation-set expects a live node")?;
            let key = require_string(
                &args[1],
                "ctfe-meta-annotation-set expects a non-empty annotation key",
            )?;
            let value = runtime_to_semantic(&args[2])?;
            let unit = unit_bridge_from_object(
                &node.unit,
                "ctfe-meta-annotation-set expects a live node",
            )?;
            track_direct_annotation_write(unit, node.node_id, &key)?;
            unit.with_unit_mut(|unit| {
                unit.semantics_mut()?.set_fact(
                    node_subject_id(node.node_id),
                    annotation_tracking_predicate(&key),
                    value,
                )
            })
            .map_err(eval_err)?;
            Ok(args[2].clone())
        },
    );
    // Retract twins of the set builtins: delete the fact/annotation from the
    // current version onward (history stays for older-version queries; a later
    // re-set becomes visible again). Returns whether something was visible.
    ev.register_special(
        "ctfe_meta_fact_delete",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let node = require_node_bridge(&args[0], "ctfe-meta-fact-delete expects a live node")?;
            let predicate =
                require_string(&args[1], "ctfe-meta-fact-delete expects a predicate string")?;
            let unit =
                unit_bridge_from_object(&node.unit, "ctfe-meta-fact-delete expects a live node")?;
            track_direct_fact_write(unit, node.node_id, &predicate)?;
            let changed = unit
                .with_unit_mut(|unit| {
                    unit.semantics_mut()?
                        .remove_fact(node_subject_id(node.node_id), &predicate)
                })
                .map_err(eval_err)?;
            Ok(RuntimeValue::Bool(changed))
        },
    );
    ev.register_special(
        "ctfe_meta_annotation_delete",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let node =
                require_node_bridge(&args[0], "ctfe-meta-annotation-delete expects a live node")?;
            let key = require_string(
                &args[1],
                "ctfe-meta-annotation-delete expects a non-empty annotation key",
            )?;
            let unit = unit_bridge_from_object(
                &node.unit,
                "ctfe-meta-annotation-delete expects a live node",
            )?;
            track_direct_annotation_write(unit, node.node_id, &key)?;
            let changed = unit
                .with_unit_mut(|unit| {
                    unit.semantics_mut()?.remove_fact(
                        node_subject_id(node.node_id),
                        &annotation_tracking_predicate(&key),
                    )
                })
                .map_err(eval_err)?;
            Ok(RuntimeValue::Bool(changed))
        },
    );
    ev.register_special(
        "ctfe_meta_fact_get_by_key",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            meta_fact_get_by_key(&args)
        },
    );
    ev.register_special(
        "ctfe_meta_fact_has_by_key",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            meta_fact_has_by_key(&args)
        },
    );
    ev.register_special(
        "ctfe_meta_fact_set_by_key",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_impure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let node =
                require_node_bridge(&args[0], "ctfe-meta-fact-set-by-key expects a live node")?;
            let predicate = require_string(
                &args[1],
                "ctfe-meta-fact-set-by-key expects a predicate string",
            )?;
            let value = runtime_to_semantic(&args[2])?;
            let unit = unit_bridge_from_object(
                &node.unit,
                "ctfe-meta-fact-set-by-key expects a live node",
            )?;
            track_direct_fact_write(unit, node.node_id, &predicate)?;
            unit.with_unit_mut(|unit| {
                unit.semantics_mut()?
                    .set_fact(node_subject_id(node.node_id), predicate, value)
            })
            .map_err(eval_err)?;
            Ok(args[2].clone())
        },
    );
}

fn node_is_live(node: &NodeBridgeValue) -> Result<bool, EvalSignal> {
    let unit = unit_bridge_from_object(&node.unit, "ctfe-node-live? expects a live node")?;
    Ok(unit.with_unit(|unit| unit.ir().node(node.node_id).is_some()))
}

fn link_binding_arg(value: &RuntimeValue) -> Result<LinkBinding, EvalSignal> {
    match value {
        RuntimeValue::Map(map) => {
            let map = map.borrow();
            let syntax = optional_map_bool(&map, "syntax")?.unwrap_or(false);
            LinkBinding::with_syntax(
                required_map_string(&map, "source_unit")?,
                required_map_string(&map, "source_name")?,
                required_map_string(&map, "local_name")?,
                syntax,
            )
            .map_err(eval_err)
        }
        _ => Err(eval_err(
            "ctfe-unit-add-dependency-binding! expects a dependency binding descriptor map",
        )),
    }
}

pub(crate) fn rewrite_report(
    unit: &Unit,
    node_val: &RuntimeValue,
) -> Result<RuntimeValue, EvalSignal> {
    let origin_stage = unit_origin_stage_value(unit);
    if let RuntimeValue::Str(stable_id) = node_val {
        let tombstone = unit
            .get_erased_rewrite_tombstone(stable_id)
            .ok_or_else(|| {
                eval_err(format!(
                    "missing erased rewrite tombstone for stable id {stable_id:?}"
                ))
            })?;
        return rewrite_tombstone_to_summary(tombstone, origin_stage);
    }

    let node_id = node_id_arg(node_val, "ctfe-unit-rewrite-report expects a node id")?;
    unit.ir()
        .node(node_id)
        .ok_or_else(|| eval_err(format!("unknown node id: {node_id}")))?;
    let stable_id = unit.node_stable_id(node_id).map_err(eval_err)?;
    let chain = unit.live_rewrite_chain(node_id).map_err(eval_err)?;
    let latest = chain
        .first()
        .map(rewrite_record_to_runtime)
        .transpose()?
        .unwrap_or(RuntimeValue::Null);
    let chain_values = chain
        .iter()
        .map(rewrite_record_to_runtime)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(map([
        ("rewritten", RuntimeValue::Bool(!chain.is_empty())),
        ("erased", RuntimeValue::Bool(false)),
        ("node_id", RuntimeValue::Int(node_id as i64)),
        ("stable_id", string(stable_id.as_str())),
        ("origin_stage", origin_stage),
        ("latest", latest),
        ("chain", tuple(chain_values)),
        ("query", RuntimeValue::Null),
    ]))
}

fn rewrite_tombstone_to_summary(
    tombstone: &RewriteTombstone,
    origin_stage: RuntimeValue,
) -> Result<RuntimeValue, EvalSignal> {
    let latest = rewrite_record_to_runtime(&tombstone.latest)?;
    let chain = tombstone
        .chain
        .iter()
        .map(rewrite_record_to_runtime)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(map([
        ("rewritten", RuntimeValue::Bool(true)),
        ("erased", RuntimeValue::Bool(true)),
        ("node_id", RuntimeValue::Null),
        ("stable_id", string(tombstone.stable_id.as_str())),
        ("origin_stage", origin_stage),
        ("latest", latest),
        ("chain", tuple(chain)),
        ("query", RuntimeValue::Null),
    ]))
}

fn rewrite_record_to_runtime(record: &RewriteRecord) -> Result<RuntimeValue, EvalSignal> {
    let generation = i64::try_from(record.generation)
        .map_err(|_| eval_err("rewrite record generation exceeds runtime integer range"))?;
    Ok(map([
        ("provider_name", string(record.provider_name.as_str())),
        ("stage", string(record.stage.as_str())),
        (
            "family_label",
            record
                .family_label
                .as_ref()
                .map(|value| string(value.as_str()))
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "family",
            record
                .family_label
                .as_ref()
                .map(|value| string(value.as_str()))
                .unwrap_or(RuntimeValue::Null),
        ),
        ("operation", string(record.operation.as_str())),
        (
            "sources",
            tuple(
                record
                    .sources
                    .iter()
                    .map(|source| RuntimeValue::Int(*source as i64))
                    .collect(),
            ),
        ),
        ("generation", RuntimeValue::Int(generation)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::parse;
    use crate::values::MapKey;

    #[test]
    fn rewrite_report_requires_exact_erased_stable_id() {
        let graph = parse("old_value").unwrap();
        let mut unit = Unit::from_graph("rewrite_report_exact_id", graph).unwrap();
        let root_id = unit.root_id();
        let tombstones = unit
            .record_erase_rewrite_tombstones("erase_provider", "compile_unit", None, root_id)
            .unwrap();
        assert_eq!(tombstones.len(), 1);

        let error = rewrite_report(&unit, &RuntimeValue::Str("wrong_stable_id".into()))
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing erased rewrite tombstone"));

        let report = rewrite_report(
            &unit,
            &RuntimeValue::Str(tombstones[0].stable_id.as_str().into()),
        )
        .unwrap();
        let RuntimeValue::Map(fields) = report else {
            panic!("expected rewrite report map");
        };
        assert_eq!(
            fields.borrow().get(&MapKey::Str("erased".into())),
            Some(&RuntimeValue::Bool(true))
        );
    }

    #[test]
    fn node_live_rejects_malformed_node_bridge_unit() {
        let unit = Unit::empty("node_live_template").unwrap();
        let template = unit.to_template();
        let malformed = NodeBridgeValue::new(
            Rc::new(UnitTemplateBridgeValue { template }) as Rc<dyn HostObject>,
            1,
        );

        let error = node_is_live(&malformed).unwrap_err().to_string();
        assert!(error.contains("ctfe-node-live? expects a live node"));
    }

    #[test]
    fn dependency_binding_arg_rejects_legacy_tuple_shape() {
        let value = RuntimeValue::Tuple(
            vec![
                RuntimeValue::Str("dep".into()),
                RuntimeValue::Str("source".into()),
                RuntimeValue::Str("local".into()),
            ]
            .into(),
        );

        let error = link_binding_arg(&value).unwrap_err().to_string();

        assert!(error.contains("dependency binding descriptor map"));
    }
}
