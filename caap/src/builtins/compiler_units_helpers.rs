/// Helper utilities and accessor functions for the unit compiler CTFE builtins.
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

use crate::bridges::{NodeBridgeValue, SemanticEntryBridgeValue};
use crate::builtins::ir_builders::ExprSpecBridgeValue;
use crate::compiler::UnitBridgeValue;
use crate::error::CaapResult;
use crate::eval::Evaluator;
use crate::ir::{ExprSpec, Node, NodeId};
use crate::semantic::{
    node_subject_id, BuiltinEffectTag, ControlPolicy, EffectPolicy, EntrySource, EvalPolicy,
    FoldPolicy, PhasePolicy, ScopePolicy, SemanticEntry, SemanticSubjectId, SemanticValue,
    SymbolEntry, SymbolKind,
};
use crate::unit::Unit;
use crate::values::{eval_err, BuiltinInfo, EvalSignal, HostObject, MapKey, RuntimeValue};

pub(super) use super::semantic_projection::semantic_value_to_plain_runtime as semantic_value_to_runtime;
use super::semantic_projection::{
    effect_policy_runtime_value, optional_runtime_phase_policy,
    resolved_name_fact_entry as resolved_name_fact_entry_value, runtime_semantic_policy_updates,
    runtime_value_to_semantic_with_nodes, semantic_value_to_runtime_with_nodes,
};
pub(super) use super::semantic_projection::{entry_source_for_symbol_kind, semantic_entry_handle};

// ---------------------------------------------------------------------------
// Bridge accessors
// ---------------------------------------------------------------------------

pub(super) fn require_unit_bridge<'a>(
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

pub(super) fn require_unit_bridge_object(
    value: &RuntimeValue,
    message: &str,
) -> Result<Rc<dyn HostObject>, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    unit_bridge_from_object(object, message)?;
    Ok(Rc::clone(object))
}

pub(super) fn unit_bridge_from_object<'a>(
    object: &'a Rc<dyn HostObject>,
    message: &str,
) -> Result<&'a UnitBridgeValue, EvalSignal> {
    object
        .as_any()
        .downcast_ref::<UnitBridgeValue>()
        .ok_or_else(|| eval_err(message))
}

pub(super) fn node_bridge(value: &RuntimeValue) -> Option<&NodeBridgeValue> {
    let RuntimeValue::HostObject(object) = value else {
        return None;
    };
    object.as_any().downcast_ref::<NodeBridgeValue>()
}

pub(super) fn expr_spec_bridge(value: &RuntimeValue) -> Option<&ExprSpecBridgeValue> {
    let RuntimeValue::HostObject(object) = value else {
        return None;
    };
    object.as_any().downcast_ref::<ExprSpecBridgeValue>()
}

pub(super) fn track_direct_fact_read(
    unit: &UnitBridgeValue,
    node_id: NodeId,
    predicate: &str,
) -> Result<(), EvalSignal> {
    let Some(ctx) = unit.provider_context() else {
        return Ok(());
    };
    require_direct_provider_effect(&ctx, BuiltinEffectTag::ReadFacts)?;
    ctx.track_fact_read(node_id, predicate);
    Ok(())
}

pub(super) fn require_direct_unit_effect(
    unit: &UnitBridgeValue,
    effect: BuiltinEffectTag,
) -> Result<(), EvalSignal> {
    let Some(ctx) = unit.provider_context() else {
        return Ok(());
    };
    require_direct_provider_effect(&ctx, effect)
}

pub(super) fn track_direct_unit_ir_read(
    unit: &UnitBridgeValue,
    node_ids: impl IntoIterator<Item = NodeId>,
) -> Result<(), EvalSignal> {
    let Some(ctx) = unit.provider_context() else {
        return Ok(());
    };
    require_direct_provider_effect(&ctx, BuiltinEffectTag::ReadIr)?;
    ctx.track_unit_ir_read();
    for node_id in node_ids {
        ctx.track_node_read(node_id);
    }
    Ok(())
}

pub(super) fn track_direct_unit_ir_write(unit: &UnitBridgeValue) -> Result<(), EvalSignal> {
    let Some(ctx) = unit.provider_context() else {
        return Ok(());
    };
    require_direct_provider_effect(&ctx, BuiltinEffectTag::WriteIr)?;
    ctx.track_unit_ir_write();
    Ok(())
}

pub(super) fn track_direct_unit_fact_table_read(unit: &UnitBridgeValue) -> Result<(), EvalSignal> {
    let Some(ctx) = unit.provider_context() else {
        return Ok(());
    };
    require_direct_provider_effect(&ctx, BuiltinEffectTag::ReadFacts)?;
    ctx.track_unit_fact_table_read();
    Ok(())
}

pub(super) fn track_direct_fact_subject_read(
    unit: &UnitBridgeValue,
    subject: &SemanticSubjectId,
    predicate: &str,
) -> Result<(), EvalSignal> {
    let Some(ctx) = unit.provider_context() else {
        return Ok(());
    };
    require_direct_provider_effect(&ctx, BuiltinEffectTag::ReadFacts)?;
    ctx.track_fact_subject_read(subject, predicate);
    Ok(())
}

pub(super) fn track_direct_unit_symbol_table_read(
    unit: &UnitBridgeValue,
) -> Result<(), EvalSignal> {
    let Some(ctx) = unit.provider_context() else {
        return Ok(());
    };
    require_direct_provider_effect(&ctx, BuiltinEffectTag::ReadSymbols)?;
    ctx.track_unit_symbol_table_read();
    Ok(())
}

pub(super) fn track_direct_unit_symbol_table_write(
    unit: &UnitBridgeValue,
) -> Result<(), EvalSignal> {
    let Some(ctx) = unit.provider_context() else {
        return Ok(());
    };
    require_direct_provider_effect(&ctx, BuiltinEffectTag::WriteSymbols)?;
    ctx.track_unit_symbol_table_write();
    Ok(())
}

pub(super) fn track_direct_symbol_read(
    unit: &UnitBridgeValue,
    name: &str,
) -> Result<(), EvalSignal> {
    let Some(ctx) = unit.provider_context() else {
        return Ok(());
    };
    require_direct_provider_effect(&ctx, BuiltinEffectTag::ReadSymbols)?;
    ctx.track_symbol_read(name);
    Ok(())
}

pub(super) fn track_direct_node_write(
    unit: &UnitBridgeValue,
    node_id: NodeId,
) -> Result<(), EvalSignal> {
    let Some(ctx) = unit.provider_context() else {
        return Ok(());
    };
    require_direct_provider_effect(&ctx, BuiltinEffectTag::WriteIr)?;
    ctx.track_node_write(node_id);
    Ok(())
}

pub(super) fn track_direct_fact_write(
    unit: &UnitBridgeValue,
    node_id: NodeId,
    predicate: &str,
) -> Result<(), EvalSignal> {
    let Some(ctx) = unit.provider_context() else {
        return Ok(());
    };
    require_direct_provider_effect(&ctx, BuiltinEffectTag::WriteFacts)?;
    ctx.track_fact_write(node_id, predicate);
    Ok(())
}

pub(super) fn track_direct_annotation_read(
    unit: &UnitBridgeValue,
    node_id: NodeId,
    key: &str,
) -> Result<(), EvalSignal> {
    let Some(ctx) = unit.provider_context() else {
        return Ok(());
    };
    require_direct_provider_effect(&ctx, BuiltinEffectTag::ReadAttributes)?;
    ctx.track_annotation_read(node_id, key);
    Ok(())
}

pub(super) fn track_direct_annotation_write(
    unit: &UnitBridgeValue,
    node_id: NodeId,
    key: &str,
) -> Result<(), EvalSignal> {
    let Some(ctx) = unit.provider_context() else {
        return Ok(());
    };
    require_direct_provider_effect(&ctx, BuiltinEffectTag::WriteAttributes)?;
    ctx.track_annotation_write(node_id, key);
    Ok(())
}

pub(super) fn track_direct_symbol_write(
    unit: &UnitBridgeValue,
    name: &str,
) -> Result<(), EvalSignal> {
    let Some(ctx) = unit.provider_context() else {
        return Ok(());
    };
    require_direct_provider_effect(&ctx, BuiltinEffectTag::WriteSymbols)?;
    ctx.track_symbol_write(name);
    Ok(())
}

fn require_direct_provider_effect(
    ctx: &crate::compiler::ProviderContextBridgeValue,
    effect: BuiltinEffectTag,
) -> Result<(), EvalSignal> {
    if ctx.declares_builtin_effect(effect) {
        Ok(())
    } else {
        Err(eval_err(format!(
            "provider {} does not declare required effect {}",
            ctx.context().provider,
            effect.as_str()
        )))
    }
}

pub(super) fn require_expr_spec(
    value: &RuntimeValue,
    message: &str,
) -> Result<ExprSpec, EvalSignal> {
    expr_spec_bridge(value)
        .map(ExprSpecBridgeValue::clone_spec)
        .ok_or_else(|| eval_err(message))
}

pub(super) fn require_node_bridge<'a>(
    value: &'a RuntimeValue,
    message: &str,
) -> Result<&'a NodeBridgeValue, EvalSignal> {
    node_bridge(value).ok_or_else(|| eval_err(message))
}

// ---------------------------------------------------------------------------
// Node ID extraction helpers
// ---------------------------------------------------------------------------

pub(super) fn node_id_in_unit(
    value: &RuntimeValue,
    unit_object: &Rc<dyn HostObject>,
    message: &str,
) -> Result<NodeId, EvalSignal> {
    match value {
        RuntimeValue::HostObject(object) => {
            let node = object
                .as_any()
                .downcast_ref::<NodeBridgeValue>()
                .ok_or_else(|| eval_err(message))?;
            if !Rc::ptr_eq(&node.unit, unit_object) {
                return Err(eval_err(message));
            }
            Ok(node.node_id)
        }
        RuntimeValue::Int(value) if *value >= 0 => Ok(*value as NodeId),
        _ => Err(eval_err(message)),
    }
}

pub(super) fn node_id_sequence_in_unit(
    value: &RuntimeValue,
    unit_object: &Rc<dyn HostObject>,
    message: &str,
) -> Result<Vec<NodeId>, EvalSignal> {
    match value {
        RuntimeValue::Tuple(items) => items
            .iter()
            .map(|item| node_id_in_unit(item, unit_object, message))
            .collect(),
        RuntimeValue::List(items) => items
            .borrow()
            .iter()
            .map(|item| node_id_in_unit(item, unit_object, message))
            .collect(),
        _ => Err(eval_err(message)),
    }
}

pub(super) fn node_id_arg(value: &RuntimeValue, message: &str) -> Result<NodeId, EvalSignal> {
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

pub(super) fn optional_node_id(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<NodeId>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(value) => node_id_arg(value, message).map(Some),
    }
}

// ---------------------------------------------------------------------------
// Node handle construction
// ---------------------------------------------------------------------------

pub(super) fn node_handle(
    unit: Rc<dyn HostObject>,
    node_id: NodeId,
) -> Result<RuntimeValue, EvalSignal> {
    let unit_bridge = unit_bridge_from_object(&unit, "node handle requires a unit handle")?;
    if !unit_bridge.with_unit(|unit| unit.ir().node(node_id).is_some()) {
        return Err(eval_err(format!("unknown node id: {node_id}")));
    }
    Ok(node_handle_from_live_node_id(unit, node_id))
}

pub(super) fn node_handle_from_live_node_id(
    unit: Rc<dyn HostObject>,
    node_id: NodeId,
) -> RuntimeValue {
    RuntimeValue::HostObject(Rc::new(NodeBridgeValue::new(unit, node_id)))
}

// ---------------------------------------------------------------------------
// Node traversal helpers
// ---------------------------------------------------------------------------

pub(super) fn with_node<R>(
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

pub(super) fn node_kind_label(node: &Node) -> &'static str {
    match node {
        Node::Call(_) => "Call",
        Node::Name(_) => "Name",
        Node::Literal(_) => "Literal",
    }
}

pub(super) fn expr_spec_kind_label(spec: &ExprSpec) -> &'static str {
    match spec {
        ExprSpec::Call(_) => "Call",
        ExprSpec::Name(_) => "Name",
        ExprSpec::Literal(_) => "Literal",
    }
}

pub(super) fn expr_spec_children(spec: &ExprSpec) -> Vec<ExprSpec> {
    match spec {
        ExprSpec::Call(call) => {
            let mut children = vec![(*call.callee).clone()];
            children.extend(call.args.iter().cloned());
            children
        }
        ExprSpec::Name(_) | ExprSpec::Literal(_) => Vec::new(),
    }
}

pub(super) fn spec_value(spec: ExprSpec) -> RuntimeValue {
    RuntimeValue::HostObject(Rc::new(ExprSpecBridgeValue::new(spec)))
}

// ---------------------------------------------------------------------------
// Fact query helpers
// ---------------------------------------------------------------------------

pub(super) fn meta_fact_get_by_key(args: &[RuntimeValue]) -> Result<RuntimeValue, EvalSignal> {
    let node = require_node_bridge(&args[0], "ctfe-meta-fact-get-by-key expects a live node")?;
    let predicate = require_string(
        &args[1],
        "ctfe-meta-fact-get-by-key expects a predicate string",
    )?;
    let unit =
        unit_bridge_from_object(&node.unit, "ctfe-meta-fact-get-by-key expects a live node")?;
    track_direct_fact_read(unit, node.node_id, &predicate)?;
    unit.with_unit(|unit| {
        Ok(unit
            .semantics()
            .get_fact(&node_subject_id(node.node_id), &predicate)
            .map_err(eval_err)?
            .map(|value| semantic_value_to_runtime_in_unit(&node.unit, value))
            .unwrap_or_else(|| args.get(2).cloned().unwrap_or(RuntimeValue::Null)))
    })
}

pub(super) fn meta_fact_has_by_key(args: &[RuntimeValue]) -> Result<RuntimeValue, EvalSignal> {
    let node = require_node_bridge(&args[0], "ctfe-meta-fact-has-by-key expects a live node")?;
    let predicate = require_string(
        &args[1],
        "ctfe-meta-fact-has-by-key expects a predicate string",
    )?;
    let unit =
        unit_bridge_from_object(&node.unit, "ctfe-meta-fact-has-by-key expects a live node")?;
    track_direct_fact_read(unit, node.node_id, &predicate)?;
    unit.with_unit(|unit| {
        unit.semantics()
            .get_fact(&node_subject_id(node.node_id), &predicate)
            .map(|value| RuntimeValue::Bool(value.is_some()))
            .map_err(eval_err)
    })
}

// ---------------------------------------------------------------------------
// Semantic entry helpers
// ---------------------------------------------------------------------------

pub(super) fn require_semantic_entry<'a>(
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

pub(super) fn resolved_name_fact_entry(
    value: Option<&SemanticValue>,
) -> Option<Result<SemanticEntry, EvalSignal>> {
    value.and_then(resolved_name_fact_entry_value)
}

pub(super) fn resolved_block_fact_node_id(
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

// ---------------------------------------------------------------------------
// Call semantics
// ---------------------------------------------------------------------------

pub(super) fn call_semantics_from_entry_value(
    ev: &Evaluator,
    entry: &SemanticEntry,
) -> RuntimeValue {
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
    let fold_policy = builtin_metadata
        .as_ref()
        .map(|metadata| metadata.fold_policy)
        .unwrap_or(entry.fold_policy);
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
        ("fold_policy", string(fold_policy.as_str())),
        ("effect_policy", effect_policy_runtime_value(&effect_policy)),
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

pub(super) fn short_circuit_policy_label(name: &str) -> &'static str {
    match name {
        "or" => "truthy",
        "and" => "falsey",
        _ => "none",
    }
}

// ---------------------------------------------------------------------------
// Semantic ↔ runtime value conversion
// ---------------------------------------------------------------------------

pub(super) fn semantic_value_to_runtime_in_unit(
    unit: &Rc<dyn HostObject>,
    value: &SemanticValue,
) -> RuntimeValue {
    semantic_value_to_runtime_with_nodes(value, &|node_id| {
        node_handle_from_live_node_id(Rc::clone(unit), node_id)
    })
}

pub(super) fn runtime_to_semantic(value: &RuntimeValue) -> Result<SemanticValue, EvalSignal> {
    runtime_value_to_semantic_with_nodes(
        value,
        "syntax metadata maps require string keys for semantic storage",
        "value cannot be stored in unit syntax metadata as semantic data",
        &|value| match value {
            RuntimeValue::HostObject(object) => object
                .as_any()
                .downcast_ref::<NodeBridgeValue>()
                .map(|node| SemanticValue::Node(node.node_id())),
            _ => None,
        },
    )
}

// ---------------------------------------------------------------------------
// Symbol / entry helpers
// ---------------------------------------------------------------------------

pub(super) fn symbol_kind_label(kind: SymbolKind) -> &'static str {
    kind.as_str()
}

pub(super) fn symbol_entry_to_value(entry: &SymbolEntry) -> RuntimeValue {
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
        ("phase_policy", string(entry.phase_policy.as_str())),
        ("public", RuntimeValue::Bool(entry.public)),
        (
            "public_names",
            tuple(entry.public_names.iter().map(string).collect()),
        ),
    ])
}

pub(super) fn unit_origin_stage_value(unit: &Unit) -> RuntimeValue {
    semantic_attribute_string(unit, "origin_stage")
        .or_else(|| semantic_attribute_string(unit, "origin_family"))
        .map(string)
        .unwrap_or(RuntimeValue::Null)
}

pub(super) fn semantic_attribute_string(unit: &Unit, key: &str) -> Option<String> {
    match unit.attributes().get(key) {
        Some(SemanticValue::Str(value)) => Some(value.clone()),
        _ => None,
    }
}

pub(super) fn add_public_name(unit: &mut Unit, name: String) -> CaapResult<()> {
    use crate::semantic::symbol_subject_id;
    use std::collections::HashSet;

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
    unit.semantics_mut()?.define_symbol(entry)?;
    let public_fact = SemanticValue::map([
        ("name".to_string(), SemanticValue::Str(name.clone())),
        ("public".to_string(), SemanticValue::Bool(true)),
    ])?;
    unit.semantics_mut()?
        .set_fact(symbol_subject_id(name)?, "symbol.entry", public_fact)?;
    Ok(())
}

pub(super) fn link_binding_to_value(binding: &crate::unit::LinkBinding) -> RuntimeValue {
    map([
        ("source_unit", string(binding.source_unit.as_str())),
        ("source_name", string(binding.source_name.as_str())),
        ("local_name", string(binding.local_name.as_str())),
        ("syntax", RuntimeValue::Bool(binding.syntax)),
    ])
}

// ---------------------------------------------------------------------------
// Misc parsing helpers
// ---------------------------------------------------------------------------

pub(super) fn optional_phase(
    value: Option<&RuntimeValue>,
    default: PhasePolicy,
    message: &str,
) -> Result<PhasePolicy, EvalSignal> {
    optional_runtime_phase_policy(value, message).map(|phase| phase.unwrap_or(default))
}

pub(super) fn optional_symbol_kind(value: Option<&RuntimeValue>) -> Result<SymbolKind, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(SymbolKind::TopLevel),
        Some(RuntimeValue::Str(text)) => SymbolKind::parse_label(text.as_ref())
            .map_err(|_| eval_err("unit-declare-symbol! expects a valid symbol kind")),
        Some(_) => Err(eval_err("unit-declare-symbol! expects a valid symbol kind")),
    }
}

// Non-empty string coercion: canonical `args::require_named_string`, kept under
// the historical local name to avoid touching call sites.
pub(super) use super::args::require_named_string as require_string;

pub(super) fn required_map_string(
    map: &IndexMap<MapKey, RuntimeValue>,
    key: &str,
) -> Result<String, EvalSignal> {
    let value = map
        .get(&MapKey::Str(key.into()))
        .ok_or_else(|| eval_err(format!("dependency binding requires {key:?}")))?;
    require_string(value, "dependency binding fields must be strings")
}

pub(super) fn optional_map_bool(
    map: &IndexMap<MapKey, RuntimeValue>,
    key: &str,
) -> Result<Option<bool>, EvalSignal> {
    match map.get(&MapKey::Str(key.into())) {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(RuntimeValue::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(eval_err(format!("{key} must be a bool"))),
    }
}

// ---------------------------------------------------------------------------
// Symbol semantics
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct SymbolSemanticsUpdates {
    pub(super) phase: Option<PhasePolicy>,
    pub(super) effect_policy: Option<EffectPolicy>,
    pub(super) eval_policy: Option<EvalPolicy>,
    pub(super) control_policy: Option<ControlPolicy>,
    pub(super) scope_policy: Option<ScopePolicy>,
    pub(super) fold_policy: Option<FoldPolicy>,
}

pub(super) fn set_symbol_semantics(
    unit: &mut Unit,
    name: String,
    semantics: SemanticValue,
    updates: SymbolSemanticsUpdates,
    node_id: Option<NodeId>,
) -> crate::error::CaapResult<()> {
    use crate::semantic::{symbol_subject_id, SemanticEntry};
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
    if let Some(fold_policy) = updates.fold_policy {
        semantic_entry.fold_policy = fold_policy;
    }
    unit.semantics_mut()?.define_symbol(entry)?;
    unit.semantics_mut()?.define_semantic(semantic_entry)?;
    unit.semantics_mut()?
        .set_fact(symbol_subject_id(name)?, "symbol.semantics", semantics)?;
    Ok(())
}

pub(super) fn symbol_semantics_updates(
    value: &RuntimeValue,
) -> Result<SymbolSemanticsUpdates, EvalSignal> {
    let RuntimeValue::Map(map) = value else {
        return Ok(SymbolSemanticsUpdates::default());
    };
    let map = map.borrow();
    let updates = runtime_semantic_policy_updates(&map, "ctfe_unit_set_symbol_semantics!")?;
    Ok(SymbolSemanticsUpdates {
        phase: updates.phase_policy,
        effect_policy: updates.effect_policy,
        eval_policy: updates.eval_policy,
        control_policy: updates.control_policy,
        scope_policy: updates.scope_policy,
        fold_policy: updates.fold_policy,
    })
}

// ---------------------------------------------------------------------------
// Value construction primitives
// ---------------------------------------------------------------------------

/// Render a source span as the canonical span map ({path, start, end,
/// start_line, start_col, end_line, end_col}) shared by `ctfe_unit_node_span`
/// and `ctfe_spec_span`.
pub(super) fn source_span_to_value(span: &crate::source::SourceSpan) -> RuntimeValue {
    map([
        (
            "path",
            span.path
                .as_deref()
                .map(string)
                .unwrap_or(RuntimeValue::Null),
        ),
        ("start", RuntimeValue::Int(span.start as i64)),
        ("end", RuntimeValue::Int(span.end as i64)),
        ("start_line", RuntimeValue::Int(span.start_line as i64)),
        ("start_col", RuntimeValue::Int(span.start_col as i64)),
        ("end_line", RuntimeValue::Int(span.end_line as i64)),
        ("end_col", RuntimeValue::Int(span.end_col as i64)),
    ])
}

pub(super) fn map<const N: usize>(entries: [(&str, RuntimeValue); N]) -> RuntimeValue {
    map_entries(entries)
}

pub(super) fn map_entries<'a>(
    entries: impl IntoIterator<Item = (&'a str, RuntimeValue)>,
) -> RuntimeValue {
    let mut map = IndexMap::new();
    for (key, value) in entries {
        map.insert(MapKey::Str(key.into()), value);
    }
    RuntimeValue::Map(Rc::new(RefCell::new(map)))
}

pub(super) use super::args::{string, tuple};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_semantics_updates_rejects_legacy_policy_fields() {
        let value = map([("phase", string("runtime"))]);
        let error = symbol_semantics_updates(&value).unwrap_err().to_string();
        assert!(error.contains("legacy policy field"));
        assert!(error.contains("phase_policy"));
    }

    #[test]
    fn symbol_semantics_updates_accepts_canonical_policy_fields() {
        let value = map([
            ("phase_policy", string("compile_time")),
            ("effect_policy", string("read_ir")),
            ("eval_policy", string("special_form")),
            ("control_policy", string("structured_exit")),
            ("scope_policy", string("lexical_binding")),
        ]);
        let updates = symbol_semantics_updates(&value).unwrap();
        assert_eq!(updates.phase, Some(PhasePolicy::CompileTime));
        assert!(updates
            .effect_policy
            .as_ref()
            .is_some_and(|policy| policy.allows("read_ir")));
        assert_eq!(updates.eval_policy, Some(EvalPolicy::SpecialForm));
        assert_eq!(updates.control_policy, Some(ControlPolicy::StructuredExit));
        assert_eq!(updates.scope_policy, Some(ScopePolicy::LexicalBinding));
    }
}
