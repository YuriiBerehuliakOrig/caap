/// Unit compiler CTFE builtins — Rust port of the currently supportable
/// `caap/builtins/compiler/units.py` surface.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::bridges::{NodeBridgeValue, SemanticEntryBridgeValue};
use crate::builtins::ir_builders::ExprSpecBridgeValue;
use crate::compiler::UnitBridgeValue;
use crate::eval::{eval_args, Evaluator};
use crate::ir::{CallNode, ExprSpec, IrLiteralData, Node, NodeId};
use crate::semantic::{
    node_subject_id, subject_id, symbol_subject_id, ControlPolicy, EffectPolicy, EntrySource,
    EvalPolicy, PhasePolicy, ScopePolicy, SemanticEntry, SemanticSubjectId, SemanticValue,
    StableId, SymbolEntry, SymbolKind,
};
use crate::syntax_authoring;
use crate::unit::{LinkBinding, RewriteRecord, RewriteTombstone, Unit, UnitTemplate};
use crate::values::{
    eval_err, runtime_value_from_literal, BuiltinInfo, EvalSignal, HostObject, MapKey, RuntimeValue,
};

#[derive(Debug)]
struct UnitTemplateBridgeValue {
    template: UnitTemplate,
}

#[derive(Clone, Debug)]
struct CallSemanticsProjection {
    value: RuntimeValue,
}

impl CallSemanticsProjection {
    fn from_builtin(info: &BuiltinInfo) -> Self {
        let metadata = info.metadata();
        Self {
            value: map([
                ("callee_class", string("builtin")),
                ("phase_policy", string(metadata.phase_policy.as_str())),
                ("eval_policy", string(metadata.eval_policy.as_str())),
                ("control_policy", string(metadata.control_policy.as_str())),
                ("scope_policy", string(metadata.scope_policy.as_str())),
                (
                    "effect_policy",
                    string(effect_policy_label(&metadata.effect_policy)),
                ),
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
            ]),
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

    fn field(&self, key: &str) -> Option<RuntimeValue> {
        let RuntimeValue::Map(map) = &self.value else {
            return None;
        };
        map.borrow().get(&MapKey::Str(key.into())).cloned()
    }

    fn field_or_null(&self, key: &str) -> RuntimeValue {
        self.field(key).unwrap_or(RuntimeValue::Null)
    }

    fn nullable_field_or(&self, key: &str, default: RuntimeValue) -> RuntimeValue {
        match self.field(key) {
            None | Some(RuntimeValue::Null) => default,
            Some(value) => value,
        }
    }

    fn callee_policy_value(&self) -> RuntimeValue {
        map([
            ("eval_policy", self.field_or_null("eval_policy")),
            ("control_policy", self.field_or_null("control_policy")),
            ("scope_policy", self.field_or_null("scope_policy")),
            ("phase_policy", self.field_or_null("phase_policy")),
            ("effect_policy", self.field_or_null("effect_policy")),
        ])
    }
}

impl HostObject for UnitTemplateBridgeValue {
    fn type_name(&self) -> &'static str {
        "unit-template"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub fn register(ev: &mut Evaluator) {
    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-id".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(&args[0], "ctfe-unit-id expects a unit handle")?;
            Ok(unit.with_unit(|unit| string(unit.unit_id())))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-root".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit_object =
                require_unit_bridge_object(&args[0], "ctfe-unit-root expects a unit handle")?;
            let unit =
                unit_bridge_from_object(&unit_object, "ctfe-unit-root expects a unit handle")?;
            unit.with_unit(|unit| node_handle(Rc::clone(&unit_object), unit.root_id()))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-version".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(&args[0], "ctfe-unit-version expects a unit handle")?;
            Ok(unit.with_unit(|unit| RuntimeValue::Int(unit.version() as i64)))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-top-level-forms".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-top-level-forms expects a unit handle")?;
            let unit_object = require_unit_bridge_object(
                &args[0],
                "ctfe-unit-top-level-forms expects a unit handle",
            )?;
            Ok(unit.with_unit(|unit| {
                tuple(
                    unit.top_level_form_ids()
                        .iter()
                        .map(|node_id| node_handle_unchecked(Rc::clone(&unit_object), *node_id))
                        .collect(),
                )
            }))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-top-level-form-at".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-top-level-form-at expects a unit handle",
            )?;
            let unit_object = require_unit_bridge_object(
                &args[0],
                "ctfe-unit-top-level-form-at expects a unit handle",
            )?;
            let index = nonnegative_usize(
                &args[1],
                "ctfe-unit-top-level-form-at expects an exact integer index",
            )?;
            unit.with_unit(|unit| {
                unit.top_level_form_ids()
                    .get(index)
                    .map(|node_id| node_handle_unchecked(Rc::clone(&unit_object), *node_id))
                    .ok_or_else(|| eval_err("ctfe-unit-top-level-form-at index is out of range"))
            })
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-facts".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(&args[0], "ctfe-unit-facts expects a unit handle")?;
            unit.with_unit(|unit| {
                let facts = unit.semantics().query_facts(None, None).map_err(eval_err)?;
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
            })
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-top-level-symbol-names".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-top-level-symbol-names expects a unit handle",
            )?;
            Ok(unit.with_unit(|unit| {
                tuple(
                    unit.semantics()
                        .symbols()
                        .values()
                        .filter(|entry| entry.kind == SymbolKind::TopLevel)
                        .map(|entry| string(entry.name.as_str()))
                        .collect(),
                )
            }))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-top-level-symbols".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-top-level-symbols expects a unit handle",
            )?;
            Ok(unit.with_unit(|unit| {
                tuple(
                    unit.semantics()
                        .symbols()
                        .values()
                        .filter(|entry| entry.kind == SymbolKind::TopLevel)
                        .map(symbol_entry_to_value)
                        .collect(),
                )
            }))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-symbols".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(&args[0], "ctfe-unit-symbols expects a unit handle")?;
            Ok(unit.with_unit(|unit| {
                tuple(
                    unit.semantics()
                        .symbols()
                        .values()
                        .map(symbol_entry_to_value)
                        .collect(),
                )
            }))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-link-bindings".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-link-bindings expects a unit handle")?;
            Ok(unit.with_unit(|unit| {
                tuple(
                    unit.link_bindings()
                        .iter()
                        .map(link_binding_to_value)
                        .collect(),
                )
            }))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-public-names".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-public-names expects a unit handle")?;
            Ok(unit.with_unit(public_names_to_value))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-node-location".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-node-location expects a unit handle")?;
            let node_id = node_id_arg(&args[1], "ctfe-unit-node-location expects a node id")?;
            unit.with_unit(|unit| {
                if unit.ir().node(node_id).is_none() {
                    return Err(eval_err(format!("unknown node id: {node_id}")));
                }
                Ok(tuple(vec![
                    string(unit.unit_id()),
                    RuntimeValue::Int(node_id as i64),
                ]))
            })
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-link-binding-new".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let syntax = match args.get(3) {
                None => false,
                Some(RuntimeValue::Bool(value)) => *value,
                Some(_) => {
                    return Err(eval_err(
                        "ctfe-unit-link-binding-new expects a boolean syntax flag",
                    ))
                }
            };
            let binding = LinkBinding::with_syntax(
                require_string(
                    &args[0],
                    "ctfe-unit-link-binding-new expects a source unit string",
                )?,
                require_string(
                    &args[1],
                    "ctfe-unit-link-binding-new expects a source name string",
                )?,
                require_string(
                    &args[2],
                    "ctfe-unit-link-binding-new expects a local name string",
                )?,
                syntax,
            )
            .map_err(eval_err)?;
            Ok(link_binding_to_value(&binding))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-to-template".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-to-template expects a unit handle")?;
            Ok(unit.with_unit(|unit| {
                RuntimeValue::HostObject(Rc::new(UnitTemplateBridgeValue {
                    template: unit.to_template(),
                }))
            }))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-template-instantiate".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let template = require_unit_template_bridge(
                &args[0],
                "ctfe-unit-template-instantiate expects a unit template handle",
            )?;
            let unit = Unit::from_template(template.template.clone()).map_err(eval_err)?;
            Ok(RuntimeValue::HostObject(Rc::new(
                UnitBridgeValue::from_unit(&unit),
            )))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-kind".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                return Ok(string(expr_spec_kind_label(&spec.spec())));
            }
            with_node(
                &args[0],
                "ctfe-node-kind expects a live node or detached node spec",
                |_, _, node| Ok(string(node_kind_label(node))),
            )
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-is-call".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                return Ok(RuntimeValue::Bool(matches!(spec.spec(), ExprSpec::Call(_))));
            }
            with_node(
                &args[0],
                "ctfe-node-is-call expects a live node or detached node spec",
                |node_handle, unit, node| {
                    Ok(RuntimeValue::Bool(
                        matches!(node, Node::Call(_))
                            && !is_structural_call(unit, node_handle.node_id),
                    ))
                },
            )
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-is-name".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                return Ok(RuntimeValue::Bool(matches!(spec.spec(), ExprSpec::Name(_))));
            }
            with_node(
                &args[0],
                "ctfe-node-is-name expects a live node or detached node spec",
                |node_handle, unit, node| {
                    Ok(RuntimeValue::Bool(
                        matches!(node, Node::Name(_))
                            && !is_structural_name(unit, node_handle.node_id),
                    ))
                },
            )
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-is-literal".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-live?".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let Some(node) = node_bridge(&args[0]) else {
                return Ok(RuntimeValue::Bool(false));
            };
            Ok(RuntimeValue::Bool(node_is_live(node)))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-id".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if expr_spec_bridge(&args[0]).is_some() {
                return Ok(RuntimeValue::Null);
            }
            let node = require_node_bridge(&args[0], "ctfe-node-id expects a live node")?;
            Ok(RuntimeValue::Int(node.node_id as i64))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-parent".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-ancestor?".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-children".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
                                node_handle_unchecked(Rc::clone(&node_handle.unit), *node_id)
                            })
                            .collect(),
                    ))
                },
            )
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-call-callee".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
                    Ok(node_handle_unchecked(
                        Rc::clone(&node_handle.unit),
                        call.callee,
                    ))
                },
            )
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-call-args".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
                                node_handle_unchecked(Rc::clone(&node_handle.unit), *node_id)
                            })
                            .collect(),
                    ))
                },
            )
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-call-semantics".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(
                call_semantics_projection(ev, &args[0], "ctfe-node-call-semantics")?
                    .map(|semantics| semantics.value())
                    .unwrap_or(RuntimeValue::Null),
            )
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-call-has-semantics".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Bool(call_has_semantics(ev, &args[0])?))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-call-effect-policy".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            call_semantics_projection(ev, &args[0], "ctfe-node-call-effect-policy").map(
                |semantics| {
                    semantics
                        .map(|semantics| semantics.field_or_null("effect_policy"))
                        .unwrap_or_else(|| args.get(1).cloned().unwrap_or(RuntimeValue::Null))
                },
            )
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-call-eval-policy".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            call_semantics_projection(ev, &args[0], "ctfe-node-call-eval-policy").map(|semantics| {
                semantics
                    .map(|semantics| semantics.field_or_null("eval_policy"))
                    .unwrap_or_else(|| args.get(1).cloned().unwrap_or(RuntimeValue::Null))
            })
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-call-short-circuit-policy".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            call_semantics_projection(ev, &args[0], "ctfe-node-call-short-circuit-policy").map(
                |semantics| {
                    semantics
                        .map(|semantics| semantics.field_or_null("short_circuit_policy"))
                        .unwrap_or_else(|| args.get(1).cloned().unwrap_or(RuntimeValue::Null))
                },
            )
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-call-builtin-name".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            call_semantics_projection(ev, &args[0], "ctfe-node-call-builtin-name").map(
                |semantics| {
                    semantics
                        .map(|semantics| {
                            semantics.nullable_field_or(
                                "builtin_name",
                                args.get(1).cloned().unwrap_or(RuntimeValue::Null),
                            )
                        })
                        .unwrap_or_else(|| args.get(1).cloned().unwrap_or(RuntimeValue::Null))
                },
            )
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-call-min-arity".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            call_semantics_projection(ev, &args[0], "ctfe-node-call-min-arity").map(|semantics| {
                semantics
                    .map(|semantics| {
                        semantics.nullable_field_or(
                            "min_arity",
                            args.get(1).cloned().unwrap_or(RuntimeValue::Null),
                        )
                    })
                    .unwrap_or_else(|| args.get(1).cloned().unwrap_or(RuntimeValue::Null))
            })
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-call-max-arity".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            call_semantics_projection(ev, &args[0], "ctfe-node-call-max-arity").map(|semantics| {
                semantics
                    .map(|semantics| {
                        semantics.nullable_field_or(
                            "max_arity",
                            args.get(1).cloned().unwrap_or(RuntimeValue::Null),
                        )
                    })
                    .unwrap_or_else(|| args.get(1).cloned().unwrap_or(RuntimeValue::Null))
            })
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-call-callee-policy".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(
                call_semantics_projection(ev, &args[0], "ctfe-node-call-callee-policy")?
                    .map(|semantics| semantics.callee_policy_value())
                    .unwrap_or(RuntimeValue::Null),
            )
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-call-scope-descriptor".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            with_node(
                &args[0],
                "ctfe-node-call-scope-descriptor expects a live Call node",
                |node_handle, unit, node| {
                    let Node::Call(call) = node else {
                        return Err(eval_err(
                            "ctfe-node-call-scope-descriptor expects a live Call node",
                        ));
                    };
                    let Some(callee) = call_callee_name(unit, call)? else {
                        return Ok(RuntimeValue::Null);
                    };
                    match callee.as_str() {
                        "lambda" => lambda_scope_descriptor(node_handle, unit, call),
                        "bind" => bind_scope_descriptor(node_handle, unit, call),
                        _ => Ok(RuntimeValue::Null),
                    }
                },
            )
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-call-control-descriptor".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            with_node(
                &args[0],
                "ctfe-node-call-control-descriptor expects a live Call node",
                |node_handle, unit, node| {
                    let Node::Call(call) = node else {
                        return Err(eval_err(
                            "ctfe-node-call-control-descriptor expects a live Call node",
                        ));
                    };
                    let Some(callee) = call_callee_name(unit, call)? else {
                        return Ok(RuntimeValue::Null);
                    };
                    match callee.as_str() {
                        "block" => block_control_descriptor(node_handle, unit, call),
                        "leave" => leave_control_descriptor(node_handle, unit, call),
                        _ => Ok(RuntimeValue::Null),
                    }
                },
            )
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-to-spec".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-resolved-block-new".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let node =
                require_node_bridge(&args[0], "ctfe-resolved-block-new expects a live node")?;
            Ok(map([("block_id", RuntimeValue::Int(node.node_id as i64))]))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-resolved-block".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let node =
                require_node_bridge(&args[0], "ctfe-node-resolved-block expects a live node")?;
            let default = args.get(1).cloned().unwrap_or(RuntimeValue::Null);
            let unit = unit_bridge_from_object(
                &node.unit,
                "ctfe-node-resolved-block expects a live node",
            )?;
            let block_id = unit
                .with_unit(|unit| {
                    unit.semantics()
                        .get_fact(&node_subject_id(node.node_id), "caap.fact.resolved_block")
                        .map(resolved_block_fact_node_id)
                })
                .map_err(eval_err)?
                .transpose()?;
            match block_id {
                Some(block_id) if unit.with_unit(|unit| unit.ir().node(block_id).is_some()) => {
                    Ok(node_handle_unchecked(Rc::clone(&node.unit), block_id))
                }
                _ => Ok(default),
            }
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-resolved-name-new".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let entry = require_semantic_entry(
                &args[0],
                "ctfe-resolved-name-new expects a semantic entry",
            )?;
            Ok(map([("entry", semantic_entry_to_runtime_value(entry))]))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-resolved-name-entry".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let node = require_node_bridge(
                &args[0],
                "ctfe-node-resolved-name-entry expects a live node",
            )?;
            let default = args.get(1).cloned().unwrap_or(RuntimeValue::Null);
            let unit = unit_bridge_from_object(
                &node.unit,
                "ctfe-node-resolved-name-entry expects a live node",
            )?;
            let entry = unit
                .with_unit(|unit| {
                    unit.semantics()
                        .get_fact(&node_subject_id(node.node_id), "caap.fact.resolved_name")
                        .map(resolved_name_fact_entry)
                })
                .map_err(eval_err)?
                .transpose()?;
            Ok(entry.map(semantic_entry_handle).unwrap_or(default))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-call-semantics-from-entry".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let entry = require_semantic_entry(
                &args[0],
                "ctfe-call-semantics-from-entry expects a resolved semantic entry",
            )?;
            Ok(call_semantics_from_entry_value(ev, entry))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-meta-annotation-get".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
            unit.with_unit(|unit| {
                unit.semantics()
                    .get_fact(&node_subject_id(node.node_id), &annotation_predicate(&key))
                    .map(|value| value.map(semantic_value_to_runtime).unwrap_or(default))
            })
            .map_err(eval_err)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-has-annotation".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                let key = require_string(
                    &args[1],
                    "ctfe-node-has-annotation expects a non-empty annotation key",
                )?;
                return Ok(RuntimeValue::Bool(
                    key == "source_span" && spec.source_span().is_some(),
                ));
            }
            let node =
                require_node_bridge(&args[0], "ctfe-node-has-annotation expects a live node")?;
            let key = require_string(
                &args[1],
                "ctfe-node-has-annotation expects a non-empty annotation key",
            )?;
            let unit = unit_bridge_from_object(
                &node.unit,
                "ctfe-node-has-annotation expects a live node",
            )?;
            unit.with_unit(|unit| {
                unit.semantics()
                    .get_fact(&node_subject_id(node.node_id), &annotation_predicate(&key))
                    .map(|value| value.is_some())
            })
            .map(RuntimeValue::Bool)
            .map_err(eval_err)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-meta-annotation-set".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
            unit.with_unit_mut(|unit| {
                unit.semantics_mut().set_fact(
                    node_subject_id(node.node_id),
                    annotation_predicate(&key),
                    value,
                )
            })
            .map_err(eval_err)?;
            Ok(args[2].clone())
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-meta-annotation-set-many".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 1,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            if (args.len() - 1) % 2 != 0 {
                return Err(eval_err(
                    "ctfe-meta-annotation-set-many expects key/value pairs after target",
                ));
            }
            if let Some(spec) = expr_spec_bridge(&args[0]) {
                for pair in args[1..].chunks(2) {
                    let key = require_string(
                        &pair[0],
                        "ctfe-meta-annotation-set-many expects annotation keys",
                    )?;
                    if key != "source_span" {
                        return Err(eval_err(
                            "ctfe-meta-annotation-set on detached specs only supports source_span",
                        ));
                    }
                    let span = match &pair[1] {
                        RuntimeValue::Null => None,
                        value => Some(crate::builtins::surface::require_span_map(
                            value,
                            "ctfe-meta-annotation-set source_span expects SourceSpan or null",
                        )?),
                    };
                    spec.set_source_span(span);
                }
                return Ok(args[0].clone());
            }
            let node = require_node_bridge(
                &args[0],
                "ctfe-meta-annotation-set-many expects a live node",
            )?;
            let unit = unit_bridge_from_object(
                &node.unit,
                "ctfe-meta-annotation-set-many expects a live node",
            )?;
            unit.with_unit_mut(|unit| {
                for pair in args[1..].chunks(2) {
                    let key = require_string(
                        &pair[0],
                        "ctfe-meta-annotation-set-many expects annotation keys",
                    )
                    .map_err(|error| error.to_string())?;
                    let value = runtime_to_semantic(&pair[1]).map_err(|error| error.to_string())?;
                    unit.semantics_mut().set_fact(
                        node_subject_id(node.node_id),
                        annotation_predicate(&key),
                        value,
                    )?;
                }
                Ok::<(), String>(())
            })
            .map_err(eval_err)?;
            Ok(args[0].clone())
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-name-identifier".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-node-literal-value".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-meta-fact-get-by-key".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            meta_fact_get_by_key(&args)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-meta-fact-get".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let namespace =
                require_string(&args[1], "ctfe-meta-fact-get expects a namespace string")?;
            let key = require_string(&args[2], "ctfe-meta-fact-get expects a key string")?;
            let predicate = format!("{namespace}.{key}");
            let mut rewritten_args = vec![args[0].clone(), string(predicate)];
            if let Some(default) = args.get(3) {
                rewritten_args.push(default.clone());
            }
            meta_fact_get_by_key(&rewritten_args)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-meta-fact-has-by-key".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let marker = RuntimeValue::Str("__missing_fact_marker__".into());
            let value = meta_fact_get_by_key(&[args[0].clone(), args[1].clone(), marker.clone()])?;
            Ok(RuntimeValue::Bool(value != marker))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-meta-fact-has".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let namespace =
                require_string(&args[1], "ctfe-meta-fact-has expects a namespace string")?;
            let key = require_string(&args[2], "ctfe-meta-fact-has expects a key string")?;
            let marker = RuntimeValue::Str("__missing_fact_marker__".into());
            let value = meta_fact_get_by_key(&[
                args[0].clone(),
                string(format!("{namespace}.{key}")),
                marker.clone(),
            ])?;
            Ok(RuntimeValue::Bool(value != marker))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-meta-fact-set-by-key".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
            unit.with_unit_mut(|unit| {
                unit.semantics_mut()
                    .set_fact(node_subject_id(node.node_id), predicate, value)
            })
            .map_err(eval_err)?;
            Ok(args[2].clone())
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-meta-fact-set".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 4,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let namespace =
                require_string(&args[1], "ctfe-meta-fact-set expects a namespace string")?;
            let key = require_string(&args[2], "ctfe-meta-fact-set expects a key string")?;
            let node = require_node_bridge(&args[0], "ctfe-meta-fact-set expects a live node")?;
            let value = runtime_to_semantic(&args[3])?;
            let unit =
                unit_bridge_from_object(&node.unit, "ctfe-meta-fact-set expects a live node")?;
            unit.with_unit_mut(|unit| {
                unit.semantics_mut().set_fact(
                    node_subject_id(node.node_id),
                    format!("{namespace}.{key}"),
                    value,
                )
            })
            .map_err(eval_err)?;
            Ok(args[3].clone())
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-declare-symbol!".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 2,
        max_arity: Some(5),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
            unit.with_unit_mut(|unit| {
                if let Some(node_id) = node_id {
                    if unit.ir().node(node_id).is_none() {
                        return Err(format!("unknown node id: {node_id}"));
                    }
                }
                let entry = SymbolEntry::new(name, kind, phase, node_id)?;
                unit.semantics_mut().define_symbol(entry);
                Ok::<(), String>(())
            })
            .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-set-symbol-semantics!".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_impure(),
        min_arity: 3,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
            unit.with_unit_mut(|unit| {
                if let Some(node_id) = node_id {
                    if unit.ir().node(node_id).is_none() {
                        return Err(format!("unknown node id: {node_id}"));
                    }
                }
                set_symbol_semantics(unit, name, semantics, updates, node_id)
            })
            .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-origin-stage".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(unit_origin_stage_value(&args[0]))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-query".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(&args[0], "ctfe-unit-query expects a unit handle")?;
            unit.with_unit(|unit| unit_query(unit, &args[1]))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-set-id!".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(&args[0], "ctfe-unit-set-id! expects a unit handle")?;
            let id = require_string(&args[1], "ctfe-unit-set-id! expects a non-empty id")?;
            unit.with_unit_mut(|unit| unit.set_unit_id(id))
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-add-link-binding!".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit = require_unit_bridge(
                &args[0],
                "ctfe-unit-add-link-binding! expects a unit handle",
            )?;
            let binding = link_binding_arg(&args[1])?;
            unit.with_unit_mut(|unit| unit.add_link_binding(binding));
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-add-public-name!".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-add-public-name! expects a unit handle")?;
            let name = require_string(
                &args[1],
                "ctfe-unit-add-public-name! expects a non-empty name",
            )?;
            unit.with_unit_mut(|unit| add_public_name(unit, name))
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-explain-name".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-explain-name expects a unit handle")?;
            let default = args.get(2).cloned();
            match unit.with_unit(|unit| explain_name(unit, &args[1])) {
                Ok(value) => Ok(value),
                Err(_) if default.is_some() => Ok(default.unwrap()),
                Err(error) => Err(error),
            }
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-explain-rewrite".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-explain-rewrite expects a unit handle")?;
            let default = args.get(2).cloned();
            match unit.with_unit(|unit| explain_rewrite(unit, &args[1])) {
                Ok(value) => Ok(value),
                Err(_) if default.is_some() => Ok(default.unwrap()),
                Err(error) => Err(error),
            }
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-syntax-rule-set!".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
                unit.set_syntax_state(syntax);
                Ok::<(), String>(())
            })
            .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-syntax-metadata-set!".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
                unit.set_syntax_state(syntax);
                Ok::<(), String>(())
            })
            .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-syntax-metadata-get".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-syntax-hook-set!".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let unit =
                require_unit_bridge(&args[0], "ctfe-unit-syntax-hook-set! expects a unit handle")?;
            let hook_ref = require_string(
                &args[1],
                "ctfe-unit-syntax-hook-set! expects a non-empty hook ref",
            )?;
            let function_name = require_string(
                &args[2],
                "ctfe-unit-syntax-hook-set! expects a non-empty function name",
            )?;
            unit.with_unit_mut(|unit| {
                let mut syntax = unit.syntax_state().clone();
                let hooks = syntax
                    .grammar_metadata
                    .get("semantic_hook_functions")
                    .cloned()
                    .unwrap_or_else(|| SemanticValue::Map(Vec::new()));
                let mut entries = match hooks {
                    SemanticValue::Map(entries) => entries,
                    _ => Vec::new(),
                };
                entries.retain(|(key, _)| key != &hook_ref);
                entries.push((hook_ref, SemanticValue::Str(function_name)));
                syntax
                    .set_grammar_metadata("semantic_hook_functions", SemanticValue::Map(entries))?;
                unit.set_syntax_state(syntax);
                Ok::<(), String>(())
            })
            .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-syntax-authoring-source-apply!".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
                unit.set_syntax_state(syntax);
                Ok::<(), String>(())
            })
            .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-syntax-rule-define!".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
                unit.set_syntax_state(syntax);
                Ok::<(), String>(())
            })
            .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-unit-syntax-rule-define-inline-node!".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
                unit.set_syntax_state(syntax);
                Ok::<(), String>(())
            })
            .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });
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

fn require_unit_bridge_object(
    value: &RuntimeValue,
    message: &str,
) -> Result<Rc<dyn HostObject>, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    unit_bridge_from_object(object, message)?;
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

fn node_bridge(value: &RuntimeValue) -> Option<&NodeBridgeValue> {
    let RuntimeValue::HostObject(object) = value else {
        return None;
    };
    object.as_any().downcast_ref::<NodeBridgeValue>()
}

fn expr_spec_bridge(value: &RuntimeValue) -> Option<&ExprSpecBridgeValue> {
    let RuntimeValue::HostObject(object) = value else {
        return None;
    };
    object.as_any().downcast_ref::<ExprSpecBridgeValue>()
}

fn require_node_bridge<'a>(
    value: &'a RuntimeValue,
    message: &str,
) -> Result<&'a NodeBridgeValue, EvalSignal> {
    node_bridge(value).ok_or_else(|| eval_err(message))
}

fn node_handle(unit: Rc<dyn HostObject>, node_id: NodeId) -> Result<RuntimeValue, EvalSignal> {
    let unit_bridge = unit_bridge_from_object(&unit, "node handle requires a unit handle")?;
    if !unit_bridge.with_unit(|unit| unit.ir().node(node_id).is_some()) {
        return Err(eval_err(format!("unknown node id: {node_id}")));
    }
    Ok(node_handle_unchecked(unit, node_id))
}

fn node_handle_unchecked(unit: Rc<dyn HostObject>, node_id: NodeId) -> RuntimeValue {
    RuntimeValue::HostObject(Rc::new(NodeBridgeValue::new(unit, node_id)))
}

fn node_is_live(node: &NodeBridgeValue) -> bool {
    unit_bridge_from_object(&node.unit, "ctfe-node-live? expects a live node")
        .map(|unit| unit.with_unit(|unit| unit.ir().node(node.node_id).is_some()))
        .unwrap_or(false)
}

fn with_node<R>(
    value: &RuntimeValue,
    message: &str,
    f: impl FnOnce(&NodeBridgeValue, &Unit, &Node) -> Result<R, EvalSignal>,
) -> Result<R, EvalSignal> {
    let node = require_node_bridge(value, message)?;
    let unit_bridge = unit_bridge_from_object(&node.unit, message)?;
    unit_bridge.with_unit(|unit| {
        let graph_node = unit
            .ir()
            .node(node.node_id)
            .ok_or_else(|| eval_err(format!("unknown node id: {}", node.node_id)))?;
        f(node, unit, graph_node)
    })
}

fn node_kind_label(node: &Node) -> &'static str {
    match node {
        Node::Call(_) => "Call",
        Node::Name(_) => "Name",
        Node::Literal(_) => "Literal",
    }
}

fn is_structural_name(unit: &Unit, node_id: NodeId) -> bool {
    is_bind_binding_name(unit, node_id) || is_lambda_parameter_name(unit, node_id)
}

fn is_structural_call(unit: &Unit, node_id: NodeId) -> bool {
    is_bind_binding_group_call(unit, node_id)
        || is_bind_binding_pair_call(unit, node_id)
        || is_lambda_parameters_group_call(unit, node_id)
}

fn is_bind_binding_name(unit: &Unit, node_id: NodeId) -> bool {
    let Some(pair_id) = unit.ir().parent(node_id).flatten() else {
        return false;
    };
    let Some(Node::Call(pair)) = unit.ir().node(pair_id) else {
        return false;
    };
    pair.callee == node_id && is_bind_binding_pair_call(unit, pair_id)
}

fn is_lambda_parameter_name(unit: &Unit, node_id: NodeId) -> bool {
    let Some(group_id) = unit.ir().parent(node_id).flatten() else {
        return false;
    };
    let Some(Node::Call(group)) = unit.ir().node(group_id) else {
        return false;
    };
    (group.callee == node_id || group.args.contains(&node_id))
        && is_lambda_parameters_group_call(unit, group_id)
}

fn is_bind_binding_group_call(unit: &Unit, node_id: NodeId) -> bool {
    let Some(bind_id) = unit.ir().parent(node_id).flatten() else {
        return false;
    };
    let Some(Node::Call(bind_call)) = unit.ir().node(bind_id) else {
        return false;
    };
    bind_call.args.first().copied() == Some(node_id)
        && matches!(
            unit.ir().node(bind_call.callee),
            Some(Node::Name(name)) if name.identifier.as_ref() == "bind"
        )
}

fn is_bind_binding_pair_call(unit: &Unit, node_id: NodeId) -> bool {
    let Some(Node::Call(_)) = unit.ir().node(node_id) else {
        return false;
    };
    let Some(group_id) = unit.ir().parent(node_id).flatten() else {
        return false;
    };
    let Some(Node::Call(group)) = unit.ir().node(group_id) else {
        return false;
    };
    if group.callee != node_id && !group.args.contains(&node_id) {
        return false;
    }
    is_bind_binding_group_call(unit, group_id)
}

fn is_lambda_parameters_group_call(unit: &Unit, node_id: NodeId) -> bool {
    let Some(lambda_id) = unit.ir().parent(node_id).flatten() else {
        return false;
    };
    let Some(Node::Call(lambda_call)) = unit.ir().node(lambda_id) else {
        return false;
    };
    lambda_call.args.first().copied() == Some(node_id)
        && matches!(
            unit.ir().node(lambda_call.callee),
            Some(Node::Name(name)) if name.identifier.as_ref() == "lambda"
        )
}

fn expr_spec_kind_label(spec: &ExprSpec) -> &'static str {
    match spec {
        ExprSpec::Call(_) => "Call",
        ExprSpec::Name(_) => "Name",
        ExprSpec::Literal(_) => "Literal",
    }
}

fn expr_spec_children(spec: &ExprSpec) -> Vec<ExprSpec> {
    match spec {
        ExprSpec::Call(call) => {
            let mut children = vec![(*call.callee).clone()];
            children.extend(call.args.iter().cloned());
            children
        }
        ExprSpec::Name(_) | ExprSpec::Literal(_) => Vec::new(),
    }
}

fn spec_value(spec: ExprSpec) -> RuntimeValue {
    RuntimeValue::HostObject(Rc::new(ExprSpecBridgeValue::new(spec)))
}

fn meta_fact_get_by_key(args: &[RuntimeValue]) -> Result<RuntimeValue, EvalSignal> {
    let node = require_node_bridge(&args[0], "ctfe-meta-fact-get-by-key expects a live node")?;
    let predicate = require_string(
        &args[1],
        "ctfe-meta-fact-get-by-key expects a predicate string",
    )?;
    let unit =
        unit_bridge_from_object(&node.unit, "ctfe-meta-fact-get-by-key expects a live node")?;
    unit.with_unit(|unit| {
        Ok(unit
            .semantics()
            .get_fact(&node_subject_id(node.node_id), &predicate)
            .map_err(eval_err)?
            .map(semantic_value_to_runtime)
            .unwrap_or_else(|| args.get(2).cloned().unwrap_or(RuntimeValue::Null)))
    })
}

fn unit_origin_stage_value(value: &RuntimeValue) -> RuntimeValue {
    if let RuntimeValue::HostObject(object) = value {
        if let Some(unit) = object.as_any().downcast_ref::<UnitBridgeValue>() {
            return unit.with_unit(|unit| {
                semantic_attribute_string(unit, "origin_stage")
                    .or_else(|| semantic_attribute_string(unit, "origin_family"))
                    .map(string)
                    .unwrap_or(RuntimeValue::Null)
            });
        }
    }
    if let RuntimeValue::Map(map) = value {
        let map = map.borrow();
        if let Some(stage) = map.get(&MapKey::Str("stage".into())) {
            return stage.clone();
        }
        if let Some(family) = map.get(&MapKey::Str("family".into())) {
            return family.clone();
        }
    }
    RuntimeValue::Null
}

fn semantic_attribute_string(unit: &Unit, key: &str) -> Option<String> {
    match unit.attributes().get(key) {
        Some(SemanticValue::Str(value)) => Some(value.clone()),
        _ => None,
    }
}

fn unit_query(unit: &Unit, query_spec: &RuntimeValue) -> Result<RuntimeValue, EvalSignal> {
    let constraints = query_constraints(query_spec)?;
    if constraints.is_none() {
        return Ok(tuple(Vec::new()));
    }
    let constraints = constraints.unwrap();
    let mut results = unit.semantics().query_facts(None, None).map_err(eval_err)?;
    for constraint in constraints {
        match constraint {
            UnitQueryConstraint::Has { subject, predicate } => {
                results.retain(|(fact_subject, fact_predicate, _)| {
                    subject.as_ref().is_none_or(|wanted| wanted == fact_subject)
                        && predicate
                            .as_ref()
                            .is_none_or(|wanted| wanted == fact_predicate)
                });
            }
            UnitQueryConstraint::NotHas { subject, predicate } => {
                results.retain(|(fact_subject, fact_predicate, _)| {
                    !(subject.as_ref().is_none_or(|wanted| wanted == fact_subject)
                        && predicate
                            .as_ref()
                            .is_none_or(|wanted| wanted == fact_predicate))
                });
            }
        }
    }
    Ok(tuple(
        results
            .iter()
            .map(|(subject, predicate, value)| {
                map([
                    ("subject", subject_to_value(subject)),
                    ("predicate", string(predicate)),
                    ("value", semantic_value_to_runtime(value)),
                ])
            })
            .collect(),
    ))
}

enum UnitQueryConstraint {
    Has {
        subject: Option<SemanticSubjectId>,
        predicate: Option<String>,
    },
    NotHas {
        subject: Option<SemanticSubjectId>,
        predicate: Option<String>,
    },
}

fn query_constraints(value: &RuntimeValue) -> Result<Option<Vec<UnitQueryConstraint>>, EvalSignal> {
    let items = sequence_items(value)
        .ok_or_else(|| eval_err("ctfe-unit-query expects a constraint sequence"))?;
    let mut constraints = Vec::new();
    for item in items {
        let Some(parts) = sequence_items(&item) else {
            return Ok(None);
        };
        if parts.is_empty() {
            return Ok(None);
        }
        let op = require_string(&parts[0], "ctfe-unit-query constraint expects an operation")?;
        let subject = parts.get(1).map(subject_arg).transpose()?.flatten();
        let predicate = parts
            .get(2)
            .map(|value| require_string(value, "ctfe-unit-query constraint expects a predicate"))
            .transpose()?;
        match op.as_str() {
            "has" => constraints.push(UnitQueryConstraint::Has { subject, predicate }),
            "not_has" => constraints.push(UnitQueryConstraint::NotHas { subject, predicate }),
            _ => return Ok(None),
        }
    }
    Ok(Some(constraints))
}

fn subject_arg(value: &RuntimeValue) -> Result<Option<SemanticSubjectId>, EvalSignal> {
    match value {
        RuntimeValue::Null => Ok(None),
        RuntimeValue::Int(node_id) if *node_id >= 0 => {
            Ok(Some(node_subject_id(*node_id as NodeId)))
        }
        RuntimeValue::Str(text) => SemanticSubjectId::parse(text)
            .map(Some)
            .or_else(|_| subject_id("symbol", text.to_string()).map(Some))
            .map_err(eval_err),
        RuntimeValue::Map(map) => {
            let map = map.borrow();
            let kind = map
                .get(&MapKey::Str("kind".into()))
                .ok_or_else(|| eval_err("semantic subject map requires kind"))?;
            let value = map
                .get(&MapKey::Str("value".into()))
                .ok_or_else(|| eval_err("semantic subject map requires value"))?;
            subject_id(
                require_string(kind, "semantic subject kind must be a string")?,
                require_string(value, "semantic subject value must be a string")?,
            )
            .map(Some)
            .map_err(eval_err)
        }
        _ => Err(eval_err(
            "ctfe-unit-query subject must be null, int, string, or map",
        )),
    }
}

fn add_public_name(unit: &mut Unit, name: String) -> Result<(), String> {
    let existing = unit.semantics().lookup_symbol(&name)?.cloned();
    let mut entry = existing.unwrap_or(SymbolEntry::new(
        name.clone(),
        SymbolKind::TopLevel,
        PhasePolicy::Runtime,
        None,
    )?);
    let mut public_names: HashSet<String> = entry.public_names.into_iter().collect();
    public_names.insert(name.clone());
    entry.public_names = public_names.into_iter().collect();
    entry.public_names.sort();
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

fn link_binding_arg(value: &RuntimeValue) -> Result<LinkBinding, EvalSignal> {
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
            "ctfe-unit-add-link-binding! expects a tuple or map link binding",
        )),
    }
}

fn symbol_entry_to_value(entry: &SymbolEntry) -> RuntimeValue {
    map([
        ("name", string(entry.name.as_str())),
        ("kind", string(symbol_kind_label(entry.kind))),
        (
            "node",
            entry
                .node_id
                .map(|node_id| RuntimeValue::Int(node_id as i64))
                .unwrap_or(RuntimeValue::Null),
        ),
        ("phase", string(entry.phase_policy.as_str())),
        ("public", RuntimeValue::Bool(entry.public)),
        (
            "public_names",
            tuple(entry.public_names.iter().map(string).collect()),
        ),
    ])
}

fn link_binding_to_value(binding: &LinkBinding) -> RuntimeValue {
    map([
        ("source_unit", string(binding.source_unit.as_str())),
        ("source_name", string(binding.source_name.as_str())),
        ("local_name", string(binding.local_name.as_str())),
        ("syntax", RuntimeValue::Bool(binding.syntax)),
    ])
}

fn public_names_to_value(unit: &Unit) -> RuntimeValue {
    let mut names: Vec<String> = unit
        .semantics()
        .symbols()
        .values()
        .flat_map(|entry| entry.public_names.iter().cloned())
        .collect();
    names.sort();
    names.dedup();
    tuple(names.iter().map(string).collect())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SymbolSemanticsUpdates {
    phase: Option<PhasePolicy>,
    effect_policy: Option<EffectPolicy>,
    eval_policy: Option<EvalPolicy>,
    control_policy: Option<ControlPolicy>,
    scope_policy: Option<ScopePolicy>,
}

fn set_symbol_semantics(
    unit: &mut Unit,
    name: String,
    semantics: SemanticValue,
    updates: SymbolSemanticsUpdates,
    node_id: Option<NodeId>,
) -> Result<(), String> {
    let mut entry = unit
        .semantics()
        .lookup_symbol(&name)?
        .cloned()
        .unwrap_or(SymbolEntry::new(
            name.clone(),
            SymbolKind::TopLevel,
            PhasePolicy::Dual,
            node_id,
        )?);
    if let Some(phase) = updates.phase {
        entry.phase_policy = phase;
    }
    if node_id.is_some() {
        entry.node_id = node_id;
    }
    let source = entry_source_for_symbol_kind(entry.kind);
    let mut semantic_entry = unit
        .semantics()
        .lookup_semantic(&name)?
        .cloned()
        .unwrap_or(SemanticEntry::new(name.clone(), source)?);
    semantic_entry.name = name.clone();
    semantic_entry.source = source;
    semantic_entry.phase_policy = entry.phase_policy;
    semantic_entry.node_id = entry.node_id;
    semantic_entry.unit_id = Some(unit.unit_id().to_string());
    semantic_entry.value = semantics.clone();
    if let Some(effect_policy) = updates.effect_policy {
        semantic_entry.effect_policy = effect_policy;
    }
    if let Some(eval_policy) = updates.eval_policy {
        semantic_entry.eval_policy = eval_policy;
    }
    if let Some(control_policy) = updates.control_policy {
        semantic_entry.control_policy = control_policy;
    }
    if let Some(scope_policy) = updates.scope_policy {
        semantic_entry.scope_policy = scope_policy;
    }
    unit.semantics_mut().define_symbol(entry);
    unit.semantics_mut().define_semantic(semantic_entry)?;
    unit.semantics_mut()
        .set_fact(symbol_subject_id(name)?, "symbol.semantics", semantics)?;
    Ok(())
}

fn symbol_semantics_updates(value: &RuntimeValue) -> Result<SymbolSemanticsUpdates, EvalSignal> {
    let RuntimeValue::Map(map) = value else {
        return Ok(SymbolSemanticsUpdates::default());
    };
    let map = map.borrow();
    let phase = optional_runtime_phase_policy(
        map.get(&MapKey::Str("phase".into())),
        "ctfe-unit-set-symbol-semantics! expects phase runtime, compile_time, or dual",
    )?;
    let effect_policy = optional_runtime_effect_policy(
        map.get(&MapKey::Str("effect_policy".into()))
            .or_else(|| map.get(&MapKey::Str("effect".into()))),
        "ctfe-unit-set-symbol-semantics! expects effect policy",
    )?;
    let eval_policy = optional_runtime_eval_policy(
        map.get(&MapKey::Str("eval_policy".into()))
            .or_else(|| map.get(&MapKey::Str("eval".into()))),
        "ctfe-unit-set-symbol-semantics! expects eval policy",
    )?;
    let control_policy = optional_runtime_control_policy(
        map.get(&MapKey::Str("control_policy".into()))
            .or_else(|| map.get(&MapKey::Str("control".into()))),
        "ctfe-unit-set-symbol-semantics! expects control policy",
    )?;
    let scope_policy = optional_runtime_scope_policy(
        map.get(&MapKey::Str("scope_policy".into()))
            .or_else(|| map.get(&MapKey::Str("scope".into()))),
        "ctfe-unit-set-symbol-semantics! expects scope policy",
    )?;
    Ok(SymbolSemanticsUpdates {
        phase,
        effect_policy,
        eval_policy,
        control_policy,
        scope_policy,
    })
}

fn optional_runtime_phase_policy(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<PhasePolicy>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(value) => phase_value(value, message).map(Some),
    }
}

fn optional_runtime_eval_policy(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<EvalPolicy>, EvalSignal> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        RuntimeValue::Null => Ok(None),
        RuntimeValue::Str(text) => match text.as_ref() {
            "eager" => Ok(Some(EvalPolicy::Eager)),
            "lazy_if" => Ok(Some(EvalPolicy::LazyIf)),
            "sequential" => Ok(Some(EvalPolicy::Sequential)),
            "special_form" => Ok(Some(EvalPolicy::SpecialForm)),
            _ => Err(eval_err(message)),
        },
        _ => Err(eval_err(message)),
    }
}

fn optional_runtime_control_policy(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<ControlPolicy>, EvalSignal> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        RuntimeValue::Null => Ok(None),
        RuntimeValue::Str(text) => match text.as_ref() {
            "plain" => Ok(Some(ControlPolicy::Plain)),
            "conditional_branch" => Ok(Some(ControlPolicy::ConditionalBranch)),
            "structured_exit" => Ok(Some(ControlPolicy::StructuredExit)),
            _ => Err(eval_err(message)),
        },
        _ => Err(eval_err(message)),
    }
}

fn optional_runtime_scope_policy(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<ScopePolicy>, EvalSignal> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        RuntimeValue::Null => Ok(None),
        RuntimeValue::Str(text) => match text.as_ref() {
            "none" => Ok(Some(ScopePolicy::None)),
            "lexical_binding" => Ok(Some(ScopePolicy::LexicalBinding)),
            _ => Err(eval_err(message)),
        },
        _ => Err(eval_err(message)),
    }
}

fn optional_runtime_effect_policy(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<EffectPolicy>, EvalSignal> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        RuntimeValue::Null => Ok(None),
        RuntimeValue::Str(text) if text.as_ref() == "pure" => Ok(Some(EffectPolicy::pure())),
        RuntimeValue::Str(text) => EffectPolicy::single(text.to_string())
            .map(Some)
            .map_err(eval_err),
        RuntimeValue::Tuple(items) => items
            .iter()
            .map(|item| require_string(item, message))
            .collect::<Result<Vec<_>, _>>()
            .and_then(|tags| EffectPolicy::new(tags).map(Some).map_err(eval_err)),
        RuntimeValue::List(items) => items
            .borrow()
            .iter()
            .map(|item| require_string(item, message))
            .collect::<Result<Vec<_>, _>>()
            .and_then(|tags| EffectPolicy::new(tags).map(Some).map_err(eval_err)),
        _ => Err(eval_err(message)),
    }
}

fn annotation_predicate(key: &str) -> String {
    format!("annotation.{key}")
}

fn call_semantics_projection(
    ev: &Evaluator,
    value: &RuntimeValue,
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
                .get_fact(&subject, "caap.fact.call_semantics")
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

fn call_has_semantics(ev: &Evaluator, value: &RuntimeValue) -> Result<bool, EvalSignal> {
    with_node(
        value,
        "ctfe-node-call-has-semantics expects a live Call node",
        |_, unit, node| {
            let Node::Call(call) = node else {
                return Err(eval_err(
                    "ctfe-node-call-has-semantics expects a live Call node",
                ));
            };
            let subject = node_subject_id(call.id);
            if let Some(semantics) = unit
                .semantics()
                .get_fact(&subject, "caap.fact.call_semantics")
                .map_err(eval_err)?
            {
                CallSemanticsProjection::from_semantic(semantics)?;
                return Ok(true);
            }
            if let Some(Node::Name(callee)) = unit.ir().node(call.callee) {
                if ev.builtin_info(&callee.identifier).is_some() {
                    return Ok(true);
                }
            }
            Ok(false)
        },
    )
}

fn effect_policy_label(policy: &EffectPolicy) -> String {
    if policy.is_pure() {
        return "pure".to_string();
    }
    policy.tags().join("|")
}

fn short_circuit_policy_label(name: &str) -> &'static str {
    match name {
        "or" => "truthy",
        "and" => "falsey",
        _ => "none",
    }
}

fn call_callee_name(unit: &Unit, call: &CallNode) -> Result<Option<String>, EvalSignal> {
    match unit.ir().node(call.callee) {
        Some(Node::Name(name)) => Ok(Some(name.identifier.to_string())),
        Some(_) => Ok(None),
        None => Err(eval_err("call descriptor callee node is missing")),
    }
}

fn lambda_scope_descriptor(
    node_handle: &NodeBridgeValue,
    unit: &Unit,
    call: &CallNode,
) -> Result<RuntimeValue, EvalSignal> {
    if call.args.len() < 2 {
        return Ok(RuntimeValue::Null);
    }
    let Some(params) = lambda_param_names(unit, call.args[0])? else {
        return Ok(RuntimeValue::Null);
    };
    let bindings = params
        .into_iter()
        .map(|name| scope_binding_value(&node_handle.unit, name, "parameter", Some(call.id)))
        .collect();
    Ok(scope_descriptor_value(
        Rc::clone(&node_handle.unit),
        bindings,
        Vec::new(),
        call.args[1..].to_vec(),
        false,
        false,
        "closure",
    ))
}

fn lambda_param_names(unit: &Unit, params_id: NodeId) -> Result<Option<Vec<String>>, EvalSignal> {
    match unit.ir().node(params_id) {
        Some(Node::Call(params)) => unit_call_item_ids(unit, params)
            .into_iter()
            .map(|param_id| match unit.ir().node(param_id) {
                Some(Node::Name(name)) => Ok(name.identifier.to_string()),
                _ => Err(eval_err("lambda scope descriptor params must be names")),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(Node::Literal(literal)) => match &literal.value {
            IrLiteralData::Tuple(items) => items
                .iter()
                .map(|item| match item {
                    IrLiteralData::Str(name) => Ok(name.clone()),
                    _ => Err(eval_err(
                        "lambda scope descriptor params literal must contain strings",
                    )),
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            IrLiteralData::Null => Ok(Some(Vec::new())),
            _ => Ok(None),
        },
        Some(Node::Name(_)) => Ok(Some(Vec::new())),
        None => Err(eval_err("lambda scope descriptor params node is missing")),
    }
}

fn bind_scope_descriptor(
    node_handle: &NodeBridgeValue,
    unit: &Unit,
    call: &CallNode,
) -> Result<RuntimeValue, EvalSignal> {
    let Some(parts) = bind_scope_parts(unit, call)? else {
        return Ok(RuntimeValue::Null);
    };
    let bindings = parts
        .bindings
        .into_iter()
        .map(|(name, node_id)| scope_binding_value(&node_handle.unit, name, "local", Some(node_id)))
        .collect();
    Ok(scope_descriptor_value(
        Rc::clone(&node_handle.unit),
        bindings,
        parts.binding_value_ids,
        parts.body_ids,
        parts.binding_values_use_child_scope,
        parts.exports_to_parent_scope,
        "scoped_eval",
    ))
}

#[derive(Debug)]
struct BindScopeParts {
    bindings: Vec<(String, NodeId)>,
    binding_value_ids: Vec<NodeId>,
    body_ids: Vec<NodeId>,
    binding_values_use_child_scope: bool,
    exports_to_parent_scope: bool,
}

fn bind_scope_parts(unit: &Unit, call: &CallNode) -> Result<Option<BindScopeParts>, EvalSignal> {
    if call.args.is_empty() {
        return Ok(None);
    }
    if let Some(parts) = flat_literal_bind_scope_parts(unit, call)? {
        return Ok(Some(parts));
    }
    match unit.ir().node(call.args[0]) {
        Some(Node::Name(name)) => {
            if call.args.len() < 2 {
                return Ok(None);
            }
            let value_id = call.args[1];
            Ok(Some(BindScopeParts {
                bindings: vec![(name.identifier.to_string(), value_id)],
                binding_value_ids: vec![value_id],
                body_ids: call.args[2..].to_vec(),
                binding_values_use_child_scope: false,
                exports_to_parent_scope: true,
            }))
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
                bindings.push((name, pair_items[1]));
                value_ids.push(pair_items[1]);
            }
            Ok(Some(BindScopeParts {
                bindings,
                binding_value_ids: value_ids,
                body_ids: call.args[1..].to_vec(),
                binding_values_use_child_scope: true,
                exports_to_parent_scope: true,
            }))
        }
        Some(Node::Literal(_)) => Ok(Some(BindScopeParts {
            bindings: Vec::new(),
            binding_value_ids: Vec::new(),
            body_ids: call.args[1..].to_vec(),
            binding_values_use_child_scope: false,
            exports_to_parent_scope: false,
        })),
        None => Err(eval_err("bind scope descriptor bindings node is missing")),
    }
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

fn flat_literal_bind_scope_parts(
    unit: &Unit,
    call: &CallNode,
) -> Result<Option<BindScopeParts>, EvalSignal> {
    if call.args.len() < 3 || !(call.args.len() - 1).is_multiple_of(2) {
        return Ok(None);
    }
    let mut bindings = Vec::new();
    let mut value_ids = Vec::new();
    for pair in call.args[..call.args.len() - 1].chunks_exact(2) {
        let name = match unit.ir().node(pair[0]) {
            Some(Node::Literal(literal)) => match &literal.value {
                IrLiteralData::Str(name) => name.clone(),
                _ => return Ok(None),
            },
            _ => return Ok(None),
        };
        bindings.push((name, pair[1]));
        value_ids.push(pair[1]);
    }
    Ok(Some(BindScopeParts {
        bindings,
        binding_value_ids: value_ids,
        body_ids: vec![*call.args.last().expect("checked non-empty bind args")],
        binding_values_use_child_scope: true,
        exports_to_parent_scope: true,
    }))
}

fn scope_descriptor_value(
    unit: Rc<dyn HostObject>,
    bindings: Vec<RuntimeValue>,
    binding_value_ids: Vec<NodeId>,
    body_ids: Vec<NodeId>,
    binding_values_use_child_scope: bool,
    exports_to_parent_scope: bool,
    result_kind: &str,
) -> RuntimeValue {
    map([
        ("bindings", tuple(bindings)),
        (
            "binding_value_ids",
            tuple(
                binding_value_ids
                    .iter()
                    .map(|id| RuntimeValue::Int(*id as i64))
                    .collect(),
            ),
        ),
        (
            "binding_values",
            tuple(
                binding_value_ids
                    .iter()
                    .map(|id| node_handle_unchecked(Rc::clone(&unit), *id))
                    .collect(),
            ),
        ),
        (
            "body_ids",
            tuple(
                body_ids
                    .iter()
                    .map(|id| RuntimeValue::Int(*id as i64))
                    .collect(),
            ),
        ),
        (
            "bodies",
            tuple(
                body_ids
                    .iter()
                    .map(|id| node_handle_unchecked(Rc::clone(&unit), *id))
                    .collect(),
            ),
        ),
        (
            "binding_values_use_child_scope",
            RuntimeValue::Bool(binding_values_use_child_scope),
        ),
        (
            "exports_to_parent_scope",
            RuntimeValue::Bool(exports_to_parent_scope),
        ),
        ("result_kind", string(result_kind)),
    ])
}

fn scope_binding_value(
    unit: &Rc<dyn HostObject>,
    name: String,
    kind: &str,
    node_id: Option<NodeId>,
) -> RuntimeValue {
    map([
        ("name", RuntimeValue::Str(name.into())),
        ("kind", string(kind)),
        (
            "node_id",
            node_id
                .map(|id| RuntimeValue::Int(id as i64))
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "node",
            node_id
                .map(|id| node_handle_unchecked(Rc::clone(unit), id))
                .unwrap_or(RuntimeValue::Null),
        ),
    ])
}

fn block_control_descriptor(
    node_handle: &NodeBridgeValue,
    unit: &Unit,
    call: &CallNode,
) -> Result<RuntimeValue, EvalSignal> {
    let Some((label, value_ids)) = block_control_parts(unit, call)? else {
        return Ok(RuntimeValue::Null);
    };
    Ok(control_descriptor_value(
        Rc::clone(&node_handle.unit),
        "block",
        label,
        value_ids,
    ))
}

fn block_control_parts(
    unit: &Unit,
    call: &CallNode,
) -> Result<Option<(RuntimeValue, Vec<NodeId>)>, EvalSignal> {
    if call.args.is_empty() {
        return Ok(None);
    }
    if call.args.len() > 1 {
        if let Some(Node::Literal(literal)) = unit.ir().node(call.args[0]) {
            return match &literal.value {
                IrLiteralData::Str(label) => Ok(Some((
                    RuntimeValue::Str(label.clone().into()),
                    call.args[1..].to_vec(),
                ))),
                IrLiteralData::Null => Ok(Some((RuntimeValue::Null, call.args[1..].to_vec()))),
                _ => Ok(Some((RuntimeValue::Null, call.args.clone()))),
            };
        }
    }
    Ok(Some((RuntimeValue::Null, call.args.clone())))
}

fn leave_control_descriptor(
    node_handle: &NodeBridgeValue,
    unit: &Unit,
    call: &CallNode,
) -> Result<RuntimeValue, EvalSignal> {
    let Some((label, value_ids)) = leave_control_parts(unit, call)? else {
        return Ok(RuntimeValue::Null);
    };
    Ok(control_descriptor_value(
        Rc::clone(&node_handle.unit),
        "leave",
        label,
        value_ids,
    ))
}

fn leave_control_parts(
    unit: &Unit,
    call: &CallNode,
) -> Result<Option<(RuntimeValue, Vec<NodeId>)>, EvalSignal> {
    let Some(first_id) = call.args.first().copied() else {
        return Ok(None);
    };
    match unit.ir().node(first_id) {
        Some(Node::Name(name)) => Ok(Some((
            RuntimeValue::Str(name.identifier.clone()),
            call.args[1..].to_vec(),
        ))),
        Some(Node::Literal(literal)) => match &literal.value {
            IrLiteralData::Null => Ok(Some((RuntimeValue::Null, call.args[1..].to_vec()))),
            IrLiteralData::Str(label) => Ok(Some((
                RuntimeValue::Str(label.clone().into()),
                call.args[1..].to_vec(),
            ))),
            IrLiteralData::Int(block_id) => Ok(Some((
                RuntimeValue::Int(*block_id),
                call.args[1..].to_vec(),
            ))),
            _ => Ok(None),
        },
        Some(_) => Ok(None),
        None => Err(eval_err("leave control descriptor target node is missing")),
    }
}

fn control_descriptor_value(
    unit: Rc<dyn HostObject>,
    kind: &str,
    label: RuntimeValue,
    value_ids: Vec<NodeId>,
) -> RuntimeValue {
    map([
        ("kind", string(kind)),
        ("label", label),
        (
            "value_ids",
            tuple(
                value_ids
                    .iter()
                    .map(|id| RuntimeValue::Int(*id as i64))
                    .collect(),
            ),
        ),
        (
            "values",
            tuple(
                value_ids
                    .iter()
                    .map(|id| node_handle_unchecked(Rc::clone(&unit), *id))
                    .collect(),
            ),
        ),
    ])
}

fn semantic_entry_handle(entry: SemanticEntry) -> RuntimeValue {
    RuntimeValue::HostObject(Rc::new(SemanticEntryBridgeValue::new(entry)))
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

fn resolved_name_fact_entry(
    value: Option<&SemanticValue>,
) -> Option<Result<SemanticEntry, EvalSignal>> {
    let Some(SemanticValue::Map(entries)) = value else {
        return None;
    };
    semantic_map_get(entries, "entry").map(semantic_entry_from_semantic_value)
}

fn semantic_entry_from_semantic_value(value: &SemanticValue) -> Result<SemanticEntry, EvalSignal> {
    let SemanticValue::Map(entries) = value else {
        return Err(eval_err("resolved-name entry must be a map"));
    };
    let name = required_semantic_str(entries, "name", "resolved-name entry requires name")?;
    let source = entry_source_label(required_semantic_str(
        entries,
        "source",
        "resolved-name entry requires source",
    )?)?;
    let mut entry = SemanticEntry::new(name, source).map_err(eval_err)?;
    entry.phase_policy = optional_semantic_phase(entries, "phase")?.unwrap_or(PhasePolicy::Runtime);
    entry.effect_policy = optional_semantic_effect_policy(entries, "effect_policy")?
        .or(optional_semantic_effect_policy(entries, "effect")?)
        .unwrap_or_else(EffectPolicy::pure);
    entry.eval_policy =
        optional_semantic_eval_policy(entries, "eval_policy")?.unwrap_or(EvalPolicy::Eager);
    entry.control_policy = optional_semantic_control_policy(entries, "control_policy")?
        .unwrap_or(ControlPolicy::Plain);
    entry.scope_policy =
        optional_semantic_scope_policy(entries, "scope_policy")?.unwrap_or(ScopePolicy::None);
    entry.node_id = optional_semantic_node_id(entries, "node_id")?;
    entry.unit_id = optional_semantic_str(entries, "unit_id")?;
    entry.value = semantic_map_get(entries, "value")
        .cloned()
        .unwrap_or(SemanticValue::Null);
    entry.stable_id = optional_semantic_str(entries, "stable_id")?
        .map(StableId::new)
        .transpose()
        .map_err(eval_err)?;
    Ok(entry)
}

fn semantic_entry_to_runtime_value(entry: &SemanticEntry) -> RuntimeValue {
    map([
        ("name", string(entry.name.as_str())),
        ("source", string(entry.source.as_str())),
        ("phase", string(entry.phase_policy.as_str())),
        (
            "effect_policy",
            string(effect_policy_label(&entry.effect_policy)),
        ),
        ("eval_policy", string(entry.eval_policy.as_str())),
        ("control_policy", string(entry.control_policy.as_str())),
        ("scope_policy", string(entry.scope_policy.as_str())),
        (
            "node_id",
            entry
                .node_id
                .map(|node_id| RuntimeValue::Int(node_id as i64))
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "unit_id",
            entry
                .unit_id
                .as_deref()
                .map(string)
                .unwrap_or(RuntimeValue::Null),
        ),
        ("value", semantic_value_to_runtime(&entry.value)),
        (
            "stable_id",
            entry
                .stable_id
                .as_ref()
                .map(|stable_id| string(stable_id.as_str()))
                .unwrap_or(RuntimeValue::Null),
        ),
    ])
}

fn call_semantics_from_entry_value(ev: &Evaluator, entry: &SemanticEntry) -> RuntimeValue {
    let builtin = (entry.source == EntrySource::Builtin)
        .then(|| ev.builtin_info(&entry.name))
        .flatten();
    let builtin_metadata = builtin.map(BuiltinInfo::metadata);
    let eval_policy = builtin_metadata
        .as_ref()
        .map(|metadata| metadata.eval_policy)
        .unwrap_or(entry.eval_policy);
    let control_policy = builtin_metadata
        .as_ref()
        .map(|metadata| metadata.control_policy)
        .unwrap_or(entry.control_policy);
    let scope_policy = builtin_metadata
        .as_ref()
        .map(|metadata| metadata.scope_policy)
        .unwrap_or(entry.scope_policy);
    let effect_policy = builtin_metadata
        .as_ref()
        .map(|metadata| metadata.effect_policy.clone())
        .unwrap_or_else(|| entry.effect_policy.clone());
    map([
        ("callee_class", string(entry.source.as_str())),
        ("phase_policy", string(entry.phase_policy.as_str())),
        ("eval_policy", string(eval_policy.as_str())),
        ("control_policy", string(control_policy.as_str())),
        ("scope_policy", string(scope_policy.as_str())),
        ("effect_policy", string(effect_policy_label(&effect_policy))),
        (
            "short_circuit_policy",
            string(
                builtin
                    .map(|info| short_circuit_policy_label(&info.name))
                    .unwrap_or("none"),
            ),
        ),
        (
            "builtin_name",
            builtin
                .map(|info| string(info.name.as_str()))
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "min_arity",
            builtin
                .map(|info| RuntimeValue::Int(info.min_arity as i64))
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "max_arity",
            builtin
                .and_then(|info| info.max_arity)
                .map(|arity| RuntimeValue::Int(arity as i64))
                .unwrap_or(RuntimeValue::Null),
        ),
    ])
}

fn semantic_map_get<'a>(
    entries: &'a [(String, SemanticValue)],
    key: &str,
) -> Option<&'a SemanticValue> {
    entries
        .iter()
        .find_map(|(entry_key, value)| (entry_key == key).then_some(value))
}

fn required_semantic_str<'a>(
    entries: &'a [(String, SemanticValue)],
    key: &str,
    message: &str,
) -> Result<&'a str, EvalSignal> {
    optional_semantic_str_ref(entries, key)?
        .ok_or_else(|| eval_err(message))
        .and_then(|value| {
            if value.is_empty() {
                Err(eval_err(message))
            } else {
                Ok(value)
            }
        })
}

fn optional_semantic_str(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<String>, EvalSignal> {
    optional_semantic_str_ref(entries, key).map(|value| value.map(str::to_string))
}

fn optional_semantic_str_ref<'a>(
    entries: &'a [(String, SemanticValue)],
    key: &str,
) -> Result<Option<&'a str>, EvalSignal> {
    match semantic_map_get(entries, key) {
        None | Some(SemanticValue::Null) => Ok(None),
        Some(SemanticValue::Str(value)) => Ok(Some(value.as_str())),
        Some(_) => Err(eval_err(format!(
            "resolved-name entry {key} must be a string"
        ))),
    }
}

fn optional_semantic_node_id(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<NodeId>, EvalSignal> {
    match semantic_map_get(entries, key) {
        None | Some(SemanticValue::Null) => Ok(None),
        Some(SemanticValue::Int(value)) if *value >= 0 => Ok(Some(*value as NodeId)),
        Some(_) => Err(eval_err(format!(
            "resolved-name entry {key} must be a non-negative integer"
        ))),
    }
}

fn optional_semantic_phase(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<PhasePolicy>, EvalSignal> {
    match optional_semantic_str_ref(entries, key)? {
        None => Ok(None),
        Some("runtime") => Ok(Some(PhasePolicy::Runtime)),
        Some("compile_time" | "compile-time") => Ok(Some(PhasePolicy::CompileTime)),
        Some("dual") => Ok(Some(PhasePolicy::Dual)),
        Some(_) => Err(eval_err("resolved-name entry phase is invalid")),
    }
}

fn optional_semantic_eval_policy(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<EvalPolicy>, EvalSignal> {
    match optional_semantic_str_ref(entries, key)? {
        None => Ok(None),
        Some("eager") => Ok(Some(EvalPolicy::Eager)),
        Some("lazy_if") => Ok(Some(EvalPolicy::LazyIf)),
        Some("sequential") => Ok(Some(EvalPolicy::Sequential)),
        Some("special_form") => Ok(Some(EvalPolicy::SpecialForm)),
        Some(_) => Err(eval_err("resolved-name entry eval policy is invalid")),
    }
}

fn optional_semantic_control_policy(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<ControlPolicy>, EvalSignal> {
    match optional_semantic_str_ref(entries, key)? {
        None => Ok(None),
        Some("plain") => Ok(Some(ControlPolicy::Plain)),
        Some("conditional_branch") => Ok(Some(ControlPolicy::ConditionalBranch)),
        Some("structured_exit") => Ok(Some(ControlPolicy::StructuredExit)),
        Some(_) => Err(eval_err("resolved-name entry control policy is invalid")),
    }
}

fn optional_semantic_scope_policy(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<ScopePolicy>, EvalSignal> {
    match optional_semantic_str_ref(entries, key)? {
        None => Ok(None),
        Some("none") => Ok(Some(ScopePolicy::None)),
        Some("lexical_binding") => Ok(Some(ScopePolicy::LexicalBinding)),
        Some(_) => Err(eval_err("resolved-name entry scope policy is invalid")),
    }
}

fn optional_semantic_effect_policy(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<EffectPolicy>, EvalSignal> {
    match semantic_map_get(entries, key) {
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

fn entry_source_label(value: &str) -> Result<EntrySource, EvalSignal> {
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

fn resolved_block_fact_node_id(
    value: Option<&SemanticValue>,
) -> Option<Result<NodeId, EvalSignal>> {
    let Some(SemanticValue::Map(entries)) = value else {
        return None;
    };
    entries.iter().find_map(|(key, value)| {
        if key != "block_id" {
            return None;
        }
        Some(match value {
            SemanticValue::Int(block_id) if *block_id >= 0 => Ok(*block_id as NodeId),
            _ => Err(eval_err(
                "ctfe-node-resolved-block expects resolved block_id to be an integer",
            )),
        })
    })
}

fn nonnegative_usize(value: &RuntimeValue, message: &str) -> Result<usize, EvalSignal> {
    match value {
        RuntimeValue::Int(value) if *value >= 0 => Ok(*value as usize),
        _ => Err(eval_err(message)),
    }
}

fn optional_node_id(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<NodeId>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(value) => node_id_arg(value, message).map(Some),
    }
}

fn optional_phase(
    value: Option<&RuntimeValue>,
    default: PhasePolicy,
    message: &str,
) -> Result<PhasePolicy, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(default),
        Some(value) => phase_value(value, message),
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

fn optional_symbol_kind(value: Option<&RuntimeValue>) -> Result<SymbolKind, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(SymbolKind::TopLevel),
        Some(RuntimeValue::Str(text)) => match text.as_ref() {
            "top_level" | "top-level" => Ok(SymbolKind::TopLevel),
            "parameter" => Ok(SymbolKind::Parameter),
            "local" => Ok(SymbolKind::Local),
            "registered" | "injected" => Ok(SymbolKind::Injected),
            "builtin" => Ok(SymbolKind::Builtin),
            "external" => Ok(SymbolKind::External),
            _ => Err(eval_err("unit-declare-symbol! expects a valid symbol kind")),
        },
        Some(_) => Err(eval_err("unit-declare-symbol! expects a valid symbol kind")),
    }
}

pub(crate) fn explain_name(
    unit: &Unit,
    node_val: &RuntimeValue,
) -> Result<RuntimeValue, EvalSignal> {
    let node_id = node_id_arg(node_val, "ctfe-unit-explain-name expects a node id")?;
    let node = unit
        .ir()
        .node(node_id)
        .ok_or_else(|| eval_err(format!("unknown node id: {node_id}")))?;
    let Node::Name(name) = node else {
        return Err(eval_err("ctfe-unit-explain-name expects a Name node"));
    };
    let symbol = unit
        .semantics()
        .lookup_symbol(name.identifier.as_ref())
        .map_err(eval_err)?
        .cloned();
    Ok(map([
        ("identifier", string(name.identifier.as_ref())),
        ("resolved", RuntimeValue::Bool(symbol.is_some())),
        (
            "binding_kind",
            symbol
                .as_ref()
                .map(|entry| string(symbol_kind_label(entry.kind)))
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "phase",
            symbol
                .as_ref()
                .map(|entry| string(entry.phase_policy.as_str()))
                .unwrap_or(RuntimeValue::Null),
        ),
        ("effect", RuntimeValue::Null),
        (
            "unit_id",
            symbol
                .as_ref()
                .map(|_| string(unit.unit_id()))
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "node_id",
            symbol
                .as_ref()
                .and_then(|entry| entry.node_id)
                .map(|node_id| RuntimeValue::Int(node_id as i64))
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "definition",
            symbol
                .as_ref()
                .and_then(|entry| entry.node_id)
                .map(|node_id| {
                    map([
                        ("unit_id", string(unit.unit_id())),
                        ("node_id", RuntimeValue::Int(node_id as i64)),
                    ])
                })
                .unwrap_or(RuntimeValue::Null),
        ),
        ("callee_policy", RuntimeValue::Null),
        (
            "origin_stage",
            unit_origin_stage_value(&RuntimeValue::HostObject(Rc::new(
                UnitBridgeValue::from_unit(unit),
            ))),
        ),
        ("query", RuntimeValue::Null),
    ]))
}

pub(crate) fn explain_rewrite(
    unit: &Unit,
    node_val: &RuntimeValue,
) -> Result<RuntimeValue, EvalSignal> {
    let origin_stage = unit_origin_stage_value(&RuntimeValue::HostObject(Rc::new(
        UnitBridgeValue::from_unit(unit),
    )));
    if let RuntimeValue::Str(stable_id) = node_val {
        let tombstone = unit
            .get_erased_rewrite_tombstone(stable_id)
            .or_else(|| {
                let mut tombstones = unit.erased_rewrite_tombstones().values();
                let first = tombstones.next()?;
                tombstones.next().is_none().then_some(first)
            })
            .ok_or_else(|| {
                eval_err(format!(
                    "missing erased rewrite tombstone for stable id {stable_id:?}"
                ))
            })?;
        return Ok(rewrite_tombstone_to_summary(tombstone, origin_stage));
    }

    let node_id = node_id_arg(node_val, "ctfe-unit-explain-rewrite expects a node id")?;
    unit.ir()
        .node(node_id)
        .ok_or_else(|| eval_err(format!("unknown node id: {node_id}")))?;
    let stable_id = unit.node_stable_id(node_id).map_err(eval_err)?;
    let chain = unit.live_rewrite_chain(node_id).map_err(eval_err)?;
    let latest = chain
        .first()
        .map(rewrite_record_to_runtime)
        .unwrap_or(RuntimeValue::Null);
    Ok(map([
        ("rewritten", RuntimeValue::Bool(!chain.is_empty())),
        ("erased", RuntimeValue::Bool(false)),
        ("node_id", RuntimeValue::Int(node_id as i64)),
        ("stable_id", string(stable_id.as_str())),
        ("origin_stage", origin_stage),
        ("latest", latest),
        (
            "chain",
            tuple(chain.iter().map(rewrite_record_to_runtime).collect()),
        ),
        ("query", RuntimeValue::Null),
    ]))
}

fn rewrite_tombstone_to_summary(
    tombstone: &RewriteTombstone,
    origin_stage: RuntimeValue,
) -> RuntimeValue {
    map([
        ("rewritten", RuntimeValue::Bool(true)),
        ("erased", RuntimeValue::Bool(true)),
        ("node_id", RuntimeValue::Null),
        ("stable_id", string(tombstone.stable_id.as_str())),
        ("origin_stage", origin_stage),
        ("latest", rewrite_record_to_runtime(&tombstone.latest)),
        (
            "chain",
            tuple(
                tombstone
                    .chain
                    .iter()
                    .map(rewrite_record_to_runtime)
                    .collect(),
            ),
        ),
        ("query", RuntimeValue::Null),
    ])
}

fn rewrite_record_to_runtime(record: &RewriteRecord) -> RuntimeValue {
    map([
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
        ("generation", RuntimeValue::Int(record.generation as i64)),
    ])
}

fn node_id_arg(value: &RuntimeValue, message: &str) -> Result<NodeId, EvalSignal> {
    match value {
        RuntimeValue::Int(value) if *value >= 0 => Ok(*value as NodeId),
        RuntimeValue::HostObject(object) => object
            .as_any()
            .downcast_ref::<NodeBridgeValue>()
            .map(|node| node.node_id)
            .ok_or_else(|| eval_err(message)),
        _ => Err(eval_err(message)),
    }
}

fn symbol_kind_label(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::TopLevel => "top_level",
        SymbolKind::Parameter => "parameter",
        SymbolKind::Local => "local",
        SymbolKind::Injected => "registered",
        SymbolKind::Builtin => "builtin",
        SymbolKind::External => "external",
    }
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

fn semantic_value_to_runtime(value: &SemanticValue) -> RuntimeValue {
    match value {
        SemanticValue::Null => RuntimeValue::Null,
        SemanticValue::Bool(value) => RuntimeValue::Bool(*value),
        SemanticValue::Int(value) => RuntimeValue::Int(*value),
        SemanticValue::Float(value) => RuntimeValue::Float(*value),
        SemanticValue::Str(value) => string(value),
        SemanticValue::Node(node_id) => RuntimeValue::Int(*node_id as i64),
        SemanticValue::List(items) => RuntimeValue::List(Rc::new(RefCell::new(
            items.iter().map(semantic_value_to_runtime).collect(),
        ))),
        SemanticValue::Map(entries) => {
            let mut map = HashMap::new();
            for (key, value) in entries {
                map.insert(
                    MapKey::Str(key.as_str().into()),
                    semantic_value_to_runtime(value),
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
                    return Err(eval_err(
                        "syntax metadata maps require string keys for semantic storage",
                    ));
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
                eval_err("value cannot be stored in unit syntax metadata as semantic data")
            }),
        _ => Err(eval_err(
            "value cannot be stored in unit syntax metadata as semantic data",
        )),
    }
}

fn required_map_string(
    map: &HashMap<MapKey, RuntimeValue>,
    key: &str,
) -> Result<String, EvalSignal> {
    let value = map
        .get(&MapKey::Str(key.into()))
        .ok_or_else(|| eval_err(format!("link binding requires {key:?}")))?;
    require_string(value, "link binding fields must be strings")
}

fn optional_map_bool(
    map: &HashMap<MapKey, RuntimeValue>,
    key: &str,
) -> Result<Option<bool>, EvalSignal> {
    match map.get(&MapKey::Str(key.into())) {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(RuntimeValue::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(eval_err(format!("{key} must be a bool"))),
    }
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

fn sequence_items(value: &RuntimeValue) -> Option<Vec<RuntimeValue>> {
    match value {
        RuntimeValue::Tuple(items) => Some(items.iter().cloned().collect()),
        RuntimeValue::List(items) => Some(items.borrow().clone()),
        _ => None,
    }
}

fn subject_to_value(subject: &SemanticSubjectId) -> RuntimeValue {
    map([
        ("kind", string(subject.kind.as_str())),
        ("value", string(subject.value.as_str())),
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
