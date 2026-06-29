use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

use crate::bridges::{NodeBridgeValue, SemanticEntryBridgeValue};
use crate::builtins::ir_builders::ExprSpecBridgeValue;
use crate::compiler::FactSchemaTypeBridgeKind;
use crate::compiler::{ProviderContextBridgeValue, UnitBridgeValue};
use crate::diagnostics::{Diagnostic, DiagnosticFix, DiagnosticSeverity};
use crate::error::{CaapError, CaapResult};
use crate::eval::Evaluator;
use crate::graph::IRGraph;
use crate::ir::{CallNode, ExprSpec, IrLiteralData, Node, NodeId};
use crate::semantic::{
    node_subject_id, BuiltinEffectTag, EffectTag, EntrySource, PhasePolicy, SemanticEntry,
    SemanticRegistry, SemanticValue, SymbolEntry, SymbolKind,
};
use crate::unit::Unit;
use crate::values::{
    eval_err, runtime_value_from_literal, ClosureValue, EnvRef, Environment, EvalSignal,
    HostObject, MapKey, RuntimeValue,
};

pub(super) use super::semantic_projection::{entry_source_for_symbol_kind, semantic_entry_handle};
use super::semantic_projection::{
    resolved_name_fact_entry, runtime_value_to_semantic_with_nodes,
    semantic_entry_from_semantic_value, semantic_value_to_runtime_with_nodes,
};

#[derive(Clone, Debug)]
pub(super) struct ResolutionScopeBridgeValue {
    pub(super) registry: Rc<RefCell<SemanticRegistry>>,
}

impl ResolutionScopeBridgeValue {
    pub(super) fn new(registry: SemanticRegistry) -> Self {
        Self {
            registry: Rc::new(RefCell::new(registry)),
        }
    }
}

impl HostObject for ResolutionScopeBridgeValue {
    fn type_name(&self) -> &'static str {
        "resolution_scope"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Clone, Debug)]
pub(super) enum CtfeResult {
    NoChange,
    Replace(ExprSpec),
    Lift(RuntimeValue),
}

#[derive(Clone, Debug)]
pub(super) struct CtfeResultBridgeValue {
    result: CtfeResult,
}

impl CtfeResultBridgeValue {
    pub(super) fn new(result: CtfeResult) -> Self {
        Self { result }
    }
}

impl HostObject for CtfeResultBridgeValue {
    fn type_name(&self) -> &'static str {
        "ctfe_result"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub(super) fn record_provider_rewrite(
    ctx: &ProviderContextBridgeValue,
    unit: &mut Unit,
    operation: &str,
    node_ids: impl IntoIterator<Item = NodeId>,
    sources: impl IntoIterator<Item = NodeId>,
) -> CaapResult<()> {
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

pub(super) fn record_provider_erase(
    ctx: &ProviderContextBridgeValue,
    unit: &mut Unit,
    node_id: NodeId,
) -> CaapResult<()> {
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

pub(super) fn emit_provider_diagnostic(
    ctx: &ProviderContextBridgeValue,
    input: ProviderDiagnosticInput<'_>,
) -> Result<RuntimeValue, EvalSignal> {
    let diagnostic = build_provider_diagnostic(ctx, input)?;
    ctx.push_diagnostic(diagnostic).map_err(eval_err)?;
    Ok(RuntimeValue::Null)
}

pub(super) struct ProviderDiagnosticInput<'a> {
    pub severity: DiagnosticSeverity,
    pub node_value: &'a RuntimeValue,
    pub message_value: &'a RuntimeValue,
    pub code_value: Option<&'a RuntimeValue>,
    pub notes_value: Option<&'a RuntimeValue>,
    pub fixes_value: Option<&'a RuntimeValue>,
}

pub(super) fn build_provider_diagnostic(
    ctx: &ProviderContextBridgeValue,
    input: ProviderDiagnosticInput<'_>,
) -> Result<Diagnostic, EvalSignal> {
    let node_id = require_node_id(input.node_value, "provider diagnostic expects a node id")?;
    let message = require_string(input.message_value, "provider diagnostic expects a message")?;
    let code = input
        .code_value
        .map(|value| require_string(value, "provider diagnostic expects a diagnostic code"))
        .transpose()?;
    let mut diagnostic = Diagnostic::new(input.severity, message).map_err(eval_err)?;
    diagnostic.code = code;
    diagnostic.span = ctx
        .unit()
        .with_unit(|unit| unit.ir().source_span(node_id).cloned());
    for note in diagnostic_notes(input.notes_value)? {
        diagnostic = diagnostic.add_note(note).map_err(eval_err)?;
    }
    for fix in diagnostic_fixes(input.fixes_value)? {
        diagnostic = diagnostic.add_fix(fix).map_err(eval_err)?;
    }
    Ok(diagnostic)
}

pub(super) fn diagnostic_notes(value: Option<&RuntimeValue>) -> Result<Vec<String>, EvalSignal> {
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

pub(super) fn diagnostic_fixes(
    value: Option<&RuntimeValue>,
) -> Result<Vec<DiagnosticFix>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(Vec::new()),
        Some(value @ RuntimeValue::Map(_)) => Ok(vec![diagnostic_fix(value)?]),
        Some(RuntimeValue::Tuple(items)) => items.iter().map(diagnostic_fix).collect(),
        Some(RuntimeValue::List(items)) => items.borrow().iter().map(diagnostic_fix).collect(),
        Some(_) => Err(eval_err(
            "ctfe-provider-diagnostics fixes expect a fix map or sequence of fix maps",
        )),
    }
}

pub(super) fn diagnostic_fix(value: &RuntimeValue) -> Result<DiagnosticFix, EvalSignal> {
    let RuntimeValue::Map(map) = value else {
        return Err(eval_err("ctfe-provider-diagnostics fix expects a map"));
    };
    let entries = map.borrow();
    let label = map_get_string(
        &entries,
        "label",
        "ctfe-provider-diagnostics fix expects a non-empty label",
    )?;
    let kind = map_get_string(
        &entries,
        "kind",
        "ctfe-provider-diagnostics fix expects a non-empty kind",
    )?;
    let metadata = entries
        .get(&MapKey::Str("metadata".into()))
        .map(|value| diagnostic_fix_metadata(Some(value)))
        .transpose()?
        .unwrap_or_default();
    DiagnosticFix::new(label, kind)
        .map_err(eval_err)?
        .with_metadata(metadata)
        .map_err(eval_err)
}

pub(super) fn map_get_string(
    entries: &indexmap::IndexMap<MapKey, RuntimeValue>,
    key: &str,
    message: &str,
) -> Result<String, EvalSignal> {
    let value = entries
        .get(&MapKey::Str(key.into()))
        .ok_or_else(|| eval_err(message))?;
    let text = require_string(value, message)?;
    if text.is_empty() {
        return Err(eval_err(message));
    }
    Ok(text)
}

pub(super) fn diagnostic_fix_metadata(
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

pub(super) fn require_provider_context<'a>(
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

pub(super) fn require_provider_effect(
    ctx: &ProviderContextBridgeValue,
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

pub(super) fn require_resolution_scope<'a>(
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

pub(super) fn semantic_entry_from_runtime_descriptor(
    value: &RuntimeValue,
    message: &str,
) -> Result<SemanticEntry, EvalSignal> {
    if let RuntimeValue::HostObject(object) = value {
        if let Some(entry) = object.as_any().downcast_ref::<SemanticEntryBridgeValue>() {
            return Ok(entry.entry().clone());
        }
    }
    let semantic = runtime_to_semantic(value).map_err(|_| eval_err(message))?;
    semantic_entry_from_semantic_value(&semantic)
}

pub(super) fn require_unit_object(
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

pub(super) fn unit_bridge_from_object<'a>(
    object: &'a Rc<dyn HostObject>,
    message: &str,
) -> Result<&'a UnitBridgeValue, EvalSignal> {
    object
        .as_any()
        .downcast_ref::<UnitBridgeValue>()
        .ok_or_else(|| eval_err(message))
}

// Non-empty string coercion: canonical `args::require_named_string`, kept under
// the historical local name to avoid touching call sites.
pub(super) use super::args::require_named_string as require_string;

pub(super) fn require_node_id(value: &RuntimeValue, message: &str) -> Result<NodeId, EvalSignal> {
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

pub(super) fn require_expr_spec(
    value: &RuntimeValue,
    message: &str,
) -> Result<ExprSpec, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    object
        .as_any()
        .downcast_ref::<ExprSpecBridgeValue>()
        .map(ExprSpecBridgeValue::spec)
        .ok_or_else(|| eval_err(message))
}

pub(super) fn node_handle(ctx: &ProviderContextBridgeValue, node_id: NodeId) -> RuntimeValue {
    let unit: Rc<dyn HostObject> = ctx.unit();
    RuntimeValue::HostObject(Rc::new(NodeBridgeValue::new(unit, node_id)))
}

pub(super) fn call_node(unit: &Unit, node_id: NodeId) -> Result<Option<&CallNode>, EvalSignal> {
    match unit.ir().node(node_id) {
        Some(Node::Call(call)) => Ok(Some(call)),
        Some(_) => Ok(None),
        None => Err(eval_err("provider call descriptor node is missing")),
    }
}

pub(super) fn callee_entry_for_call(
    ctx: &ProviderContextBridgeValue,
    unit: &Unit,
    call: &CallNode,
) -> Result<Option<SemanticEntry>, EvalSignal> {
    let Some(Node::Name(callee)) = unit.ir().node(call.callee) else {
        return Ok(None);
    };
    let fact_schema = ctx.fact_schema();
    let resolved_name_predicates =
        fact_schema.predicates_by_bridge_kind(FactSchemaTypeBridgeKind::ResolvedName);
    let callee_subject = node_subject_id(call.callee);
    for predicate in resolved_name_predicates {
        if let Some(entry) = unit
            .semantics()
            .get_fact(&callee_subject, predicate)
            .map_err(eval_err)?
            .and_then(resolved_name_fact_entry)
            .transpose()?
        {
            return Ok(Some(entry));
        }
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

pub(super) fn should_normalize_ctfe(entry: &SemanticEntry) -> bool {
    entry.phase_policy == PhasePolicy::CompileTime
        && matches!(entry.source, EntrySource::Builtin | EntrySource::Registered)
}

pub(super) fn execute_ctfe_entry(
    ev: &mut Evaluator,
    ctx: &ProviderContextBridgeValue,
    node_id: NodeId,
    entry: &SemanticEntry,
) -> Result<RuntimeValue, EvalSignal> {
    if !should_normalize_ctfe(entry) {
        return Ok(ctfe_result_value(CtfeResult::NoChange));
    }
    let callback = ctx.lookup_registered_value(&entry.name).map_err(eval_err)?;
    let callback = match callback {
        Some(callback) => Some(callback),
        None if entry.source == EntrySource::Registered => initial_binding_value(ctx, &entry.name),
        None => None,
    };
    if let Some(callback) = callback {
        if entry.source == EntrySource::Registered && !is_provider_style_callback(&callback) {
            return Ok(ctfe_result_value(CtfeResult::NoChange));
        }
        return execute_ctfe_callback(ev, ctx, node_id, entry, callback);
    }
    Ok(ctfe_result_value(CtfeResult::NoChange))
}

pub(super) fn is_provider_style_callback(value: &RuntimeValue) -> bool {
    match value {
        RuntimeValue::Closure(closure) => closure.params == ["ctx", "node"],
        RuntimeValue::HostFunction(host) => {
            host.min_arity <= 2 && host.max_arity.is_none_or(|max| max >= 2)
        }
        _ => false,
    }
}

pub(super) fn execute_ctfe_callback(
    ev: &mut Evaluator,
    ctx: &ProviderContextBridgeValue,
    node_id: NodeId,
    entry: &SemanticEntry,
    callback: RuntimeValue,
) -> Result<RuntimeValue, EvalSignal> {
    let node = node_handle(ctx, node_id);
    let args = vec![RuntimeValue::HostObject(ctx_host_object(ctx)), node.clone()];
    // Bound the compile-time reduction: a non-terminating fold fails the
    // compile with a structured error instead of hanging it (phase 2 budget).
    match ev.with_eval_alloc_budget(crate::eval::DEFAULT_CTFE_FOLD_ALLOC_BUDGET, |ev| {
        ev.with_eval_step_budget(crate::eval::DEFAULT_CTFE_FOLD_STEP_BUDGET, |ev| {
            ev.invoke_callback(&callback, args)
        })
    }) {
        Ok(value) => Ok(ctfe_result_value(coerce_value_ctfe_result(
            ctx, node_id, value,
        )?)),
        Err(signal) => {
            emit_ctfe_error(ctx, node_id, entry, "function_failed", signal.to_string())?;
            Ok(ctfe_result_value(CtfeResult::NoChange))
        }
    }
}

pub(super) fn coerce_value_ctfe_result(
    ctx: &ProviderContextBridgeValue,
    node_id: NodeId,
    value: RuntimeValue,
) -> Result<CtfeResult, EvalSignal> {
    if let RuntimeValue::HostObject(object) = &value {
        if let Some(result) = object.as_any().downcast_ref::<CtfeResultBridgeValue>() {
            return Ok(result.result.clone());
        }
    }
    if matches!(value, RuntimeValue::Null) || same_node_handle(&value, node_id) {
        return Ok(CtfeResult::NoChange);
    }
    if let Some(spec) = runtime_expr_spec(ctx, &value)? {
        return Ok(CtfeResult::Replace(spec));
    }
    if lift_runtime_value(ctx, &value).is_err() {
        return Ok(CtfeResult::NoChange);
    }
    Ok(CtfeResult::Lift(value))
}

pub(super) fn same_node_handle(value: &RuntimeValue, node_id: NodeId) -> bool {
    let RuntimeValue::HostObject(object) = value else {
        return false;
    };
    object
        .as_any()
        .downcast_ref::<NodeBridgeValue>()
        .is_some_and(|node| node.node_id() == node_id)
}

pub(super) fn materialize_ctfe_result(
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
        return Ok(None);
    };
    match &result.result {
        CtfeResult::NoChange => Ok(None),
        CtfeResult::Replace(spec) => Ok(Some(spec.clone())),
        CtfeResult::Lift(value) => lift_runtime_value(ctx, value).map(Some),
    }
}

pub(super) fn fold_compile_time_call(
    ev: &mut Evaluator,
    ctx: &ProviderContextBridgeValue,
    node_id: NodeId,
) -> Result<RuntimeValue, EvalSignal> {
    let entry = ctx.unit().with_unit(|unit| {
        let Some(call) = call_node(unit, node_id)? else {
            return Ok::<Option<SemanticEntry>, EvalSignal>(None);
        };
        callee_entry_for_call(ctx, unit, call)
    })?;
    let Some(entry) = entry else {
        return Ok(node_handle(ctx, node_id));
    };
    if !should_normalize_ctfe(&entry) {
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

/// Extract lambda parameter names from a params node living in `unit`'s IR.
/// Mirrors the `lambda` special form's `extract_param_names`, but reads the
/// unit graph directly (the lambda lives in the unit being compiled, not in the
/// provider's own graph).
fn unit_lambda_param_names(unit: &Unit, params_id: NodeId) -> Result<Vec<String>, EvalSignal> {
    match unit.ir().node(params_id) {
        Some(Node::Call(call)) => {
            let mut names = Vec::with_capacity(call.args.len() + 1);
            for &item in std::iter::once(&call.callee).chain(call.args.iter()) {
                match unit.ir().node(item) {
                    Some(Node::Name(n)) => names.push(n.identifier.to_string()),
                    _ => return Err(eval_err("lambda params must be names")),
                }
            }
            Ok(names)
        }
        Some(Node::Literal(lit)) => match &lit.value {
            IrLiteralData::Null => Ok(Vec::new()),
            IrLiteralData::Tuple(items) => items
                .iter()
                .map(|item| match item {
                    IrLiteralData::Str(name) if !name.is_empty() => Ok(name.clone()),
                    _ => Err(eval_err("lambda params tuple must contain non-empty names")),
                })
                .collect(),
            _ => Err(eval_err(
                "lambda params node must be a call or null literal",
            )),
        },
        Some(Node::Name(_)) => Ok(Vec::new()),
        None => Err(eval_err("missing lambda params node")),
    }
}

/// Evaluate a call `(f a1..an)` whose callee resolves to the user lambda at
fn top_level_form_references_name(unit: &Unit, form_id: NodeId, name: &str) -> bool {
    let mut stack = vec![form_id];
    while let Some(id) = stack.pop() {
        match unit.ir().node(id) {
            Some(Node::Name(n)) if n.identifier.as_ref() == name => return true,
            Some(node) => stack.extend(node.children()),
            None => {}
        }
    }
    false
}

/// Synthesize a top-level `(bind name value null)` definition and declare its
/// module symbol — the substrate for residual specialization (a specialized
/// function is materialized once, shared by every rewritten call site). The whole
/// mutation runs inside one `with_unit_mut` against `&mut Unit`, so it never
/// exposes live node handles to CAAP across a structural change. Idempotent: a
/// no-op if `name` is already a symbol (memoized residual reuse). The
/// specialization pass owns the policy of what/when to synthesize.
pub(super) fn synthesize_internal_definition(
    ctx: &ProviderContextBridgeValue,
    name: &str,
    value: ExprSpec,
) -> Result<RuntimeValue, EvalSignal> {
    if name.is_empty() {
        return Err(eval_err(
            "ctfe-provider-synthesize-internal-definition! expects a non-empty name",
        ));
    }
    let bind_spec = ExprSpec::call(
        ExprSpec::name("bind").map_err(eval_err)?,
        vec![
            ExprSpec::literal(IrLiteralData::Str(name.to_string())),
            value,
            ExprSpec::literal(IrLiteralData::Null),
        ],
    );
    ctx.unit().with_unit_mut(|unit| -> Result<(), EvalSignal> {
        // Memoization: reuse an already-synthesized residual of the same name.
        if unit
            .semantics()
            .lookup_symbol(name)
            .map_err(eval_err)?
            .is_some()
        {
            return Ok(());
        }
        let form_id = unit
            .append_ir_top_level_with_spec(&bind_spec)
            .map_err(eval_err)?;
        // Place the definition immediately before the first existing top-level
        // form that REFERENCES `name`. That anchor is always a runtime form
        // (module/import/registration header forms never reference a synthesized
        // residual), so the definition lands after the leading setup/import
        // prefix — preserving the module loader's "skip leading setup forms at
        // runtime" invariant — yet before its first use, satisfying top-level
        // initialization order. Callers therefore rewrite the use sites BEFORE
        // synthesizing, so at least one reference already exists. If no
        // reference is found, the definition simply stays appended at the end.
        let form_ids: Vec<NodeId> = unit.ir().top_level_form_ids().to_vec();
        let anchor = form_ids
            .iter()
            .copied()
            .find(|&id| id != form_id && top_level_form_references_name(unit, id, name));
        if let Some(anchor) = anchor {
            let mut ids: Vec<NodeId> = form_ids;
            ids.retain(|&id| id != form_id);
            let pos = ids.iter().position(|&id| id == anchor).unwrap_or(ids.len());
            ids.insert(pos, form_id);
            unit.set_ir_top_level_form_ids(ids).map_err(eval_err)?;
        }
        // Match ordinary top-level `bind` symbols: the entry exists purely for
        // name resolution and carries no node_id (the runtime binding comes from
        // the synthesized `(bind name value null)` form). A node_id here would be
        // exposed as a raw Int by `ctfe-unit-top-level-symbols`, which downstream
        // providers would then mis-feed to node-only builtins.
        let entry = SymbolEntry::new(
            name.to_string(),
            SymbolKind::TopLevel,
            PhasePolicy::Dual,
            None,
        )
        .map_err(eval_err)?;
        unit.semantics_mut()
            .map_err(eval_err)?
            .define_symbol(entry)
            .map_err(eval_err)?;
        Ok(())
    })?;
    Ok(RuntimeValue::Null)
}

/// Whether `node` is a lambda form: a `Call` whose callee is the name `lambda`.
fn is_lambda_node(unit: &Unit, call: &CallNode) -> bool {
    matches!(
        unit.ir().node(call.callee),
        Some(Node::Name(name)) if name.identifier.as_ref() == "lambda"
    )
}

/// Reconstruct the unit's top-level compile-time environment so that a folded
/// function body can resolve its sibling functions and recurse.
///
/// Every name reference the resolver touched carries a `resolved_name` fact
/// pointing at its definition — the same fact the provider uses to resolve a
/// callee. Walking them gives every top-level binding the body might reference:
/// sibling functions (cross-function), the function itself (recursion), and
/// constants. Each lambda becomes a closure over `unit_graph` whose own env is
/// this same `module_env` — a deliberate `letrec` cycle, which is what lets a
/// function reference itself or its siblings during a fold. Anything in here
/// that is not actually foldable (effects, non-termination, free variables)
/// still fails at eval time under the phase/budget/depth gates and leaves the
/// call unchanged, so populating the env only ever enables conservative-safe
/// folds — never a wrong one.
fn build_module_env(
    ctx: &ProviderContextBridgeValue,
    unit: &Unit,
    unit_graph: &Rc<IRGraph>,
) -> EnvRef {
    let module_env = Environment::new(None);
    let fact_schema = ctx.fact_schema();
    let predicates = fact_schema.predicates_by_bridge_kind(FactSchemaTypeBridgeKind::ResolvedName);
    let mut bound: std::collections::HashSet<String> = std::collections::HashSet::new();
    for node_id in unit.ir().node_ids() {
        let Some(Node::Name(name)) = unit.ir().node(node_id) else {
            continue;
        };
        let identifier = name.identifier.to_string();
        if bound.contains(&identifier) {
            continue;
        }
        let subject = node_subject_id(node_id);
        let mut resolved_entry = None;
        for predicate in predicates.iter().copied() {
            if let Ok(Some(value)) = unit.semantics().get_fact(&subject, predicate) {
                if let Some(entry) = resolved_name_fact_entry(value).transpose().ok().flatten() {
                    resolved_entry = Some(entry);
                    break;
                }
            }
        }
        let Some(def_id) = resolved_entry.and_then(|entry| entry.node_id) else {
            continue;
        };
        match unit.ir().node(def_id) {
            Some(Node::Call(lambda)) if lambda.args.len() >= 2 && is_lambda_node(unit, lambda) => {
                let Ok(params) = unit_lambda_param_names(unit, lambda.args[0]) else {
                    continue;
                };
                let body_ids = lambda.args[1..].to_vec();
                let closure = RuntimeValue::Closure(Rc::new(ClosureValue {
                    params,
                    body_ids,
                    env: Rc::clone(&module_env),
                    graph: Rc::clone(unit_graph),
                }));
                Environment::define(&module_env, identifier.clone(), closure);
                bound.insert(identifier);
            }
            Some(Node::Literal(literal)) => {
                Environment::define(
                    &module_env,
                    identifier.clone(),
                    runtime_value_from_literal(&literal.value),
                );
                bound.insert(identifier);
            }
            _ => {}
        }
    }
    module_env
}

/// `lambda_node_id`, when every argument is a literal, by invoking the function
/// at compile time and replacing the call with the lifted result.
///
/// This is the substrate for "a compile-time function is just a function":
/// foldability is decided per call site by binding-time (all arguments known),
/// not by any special registration. Evaluation runs in the provider's
/// CompileTime phase under the fold step budget, so any effectful/runtime-only
/// operation phase-errors and any unliftable or failing evaluation is caught —
/// in which case the call is left unchanged (conservative fallback, never a
/// silent wrong fold). The policy of *which* calls to attempt (purity, closed,
/// non-recursive) lives in the stdlib provider that calls this primitive.
pub(super) fn evaluate_internal_call(
    ev: &mut Evaluator,
    ctx: &ProviderContextBridgeValue,
    call_node_id: NodeId,
    lambda_node_id: NodeId,
) -> Result<RuntimeValue, EvalSignal> {
    type Prepared = Option<(Vec<RuntimeValue>, Vec<String>, Vec<NodeId>)>;
    let prepared: Prepared = ctx
        .unit()
        .with_unit(|unit| -> Result<Prepared, EvalSignal> {
            let Some(Node::Call(call)) = unit.ir().node(call_node_id) else {
                return Ok(None);
            };
            let mut arg_values = Vec::with_capacity(call.args.len());
            for &arg in call.args.iter() {
                match unit.ir().node(arg) {
                    // Only fully-known (literal) arguments make a call foldable.
                    Some(Node::Literal(lit)) => {
                        arg_values.push(runtime_value_from_literal(&lit.value))
                    }
                    _ => return Ok(None),
                }
            }
            let Some(Node::Call(lambda)) = unit.ir().node(lambda_node_id) else {
                return Ok(None);
            };
            if lambda.args.len() < 2 {
                return Ok(None);
            }
            let params = unit_lambda_param_names(unit, lambda.args[0])?;
            if params.len() != arg_values.len() {
                return Ok(None);
            }
            let body_ids = lambda.args[1..].to_vec();
            Ok(Some((arg_values, params, body_ids)))
        })?;
    let Some((arg_values, params, body_ids)) = prepared else {
        return Ok(node_handle(ctx, call_node_id));
    };
    // Invoke a closure built over a snapshot of the unit graph, in the current
    // (CompileTime) phase under the fold budget. A pure, closed function over
    // literal arguments reduces to a value; anything else fails and we no-op.
    let unit_graph = Rc::new(ctx.unit().with_unit(|unit| unit.ir().clone()));
    // The body runs over a reconstructed module environment so it can resolve
    // sibling functions and recurse; without it, only self-contained bodies
    // (params + pure builtins) folded. The phase/budget/depth gates still bound
    // every call reached through it, so this never turns into a wrong fold. This
    // runs in the `fold_calls` stage, after all rewrite passes have settled, so
    // the captured bodies are final (see toolchain_foundation.caap).
    let module_env = ctx
        .unit()
        .with_unit(|unit| build_module_env(ctx, unit, &unit_graph));
    let closure = RuntimeValue::Closure(Rc::new(ClosureValue {
        params,
        body_ids,
        env: Rc::clone(&module_env),
        graph: unit_graph,
    }));
    // Bound the fold's call-recursion depth as well as its step count: a deep
    // recursion grows the native stack one frame per level and would abort the
    // compiler before the step budget trips. Over-deep folds fail and leave the
    // call to run at runtime (conservative no-op). Restored before any return.
    let saved_depth = ev.max_eval_depth();
    ev.set_max_eval_depth(saved_depth.min(crate::eval::DEFAULT_CTFE_FOLD_DEPTH_BUDGET));
    // Bound allocation alongside steps: an O(1)-step builtin can still allocate
    // O(limit) memory, so without this a hostile fold could OOM-abort the
    // compiler (uncatchable) before the step budget tripped.
    let fold_result = ev.with_eval_alloc_budget(crate::eval::DEFAULT_CTFE_FOLD_ALLOC_BUDGET, |e| {
        e.with_eval_step_budget(crate::eval::DEFAULT_CTFE_FOLD_STEP_BUDGET, |e| {
            e.invoke_callback(&closure, arg_values)
        })
    });
    ev.set_max_eval_depth(saved_depth);
    // The module env binds sibling closures whose own `env` points back at it (a
    // letrec cycle). Break it now that the fold is done so the scope and its
    // closures are freed rather than leaked.
    Environment::clear(&module_env);
    let value = match fold_result {
        Ok(value) => value,
        Err(_) => return Ok(node_handle(ctx, call_node_id)),
    };
    // Materialize only SCALAR results (int / bool / null) as literals. A call
    // that computes a string or a structured value is left as a runtime call:
    // baking such results into IR literals would both bloat the IR and, more
    // importantly, change what the call lowers to (e.g. a derived `to-string`
    // is meant to lower to its runtime bridge call, not to a string literal a
    // value-only backend cannot represent). Runtime representation of non-scalar
    // results is the backend's concern, not compile-time evaluation's.
    let literal = match &value {
        RuntimeValue::Int(_) | RuntimeValue::Bool(_) | RuntimeValue::Null => {
            runtime_to_literal(&value)
        }
        _ => return Ok(node_handle(ctx, call_node_id)),
    };
    let spec = match literal {
        Ok(literal) => ExprSpec::literal(literal),
        Err(_) => return Ok(node_handle(ctx, call_node_id)),
    };
    let new_id = ctx
        .unit()
        .with_unit_mut(|unit| unit.replace_ir_subtree_with_spec(call_node_id, &spec))
        .map_err(eval_err)?;
    Ok(node_handle(ctx, new_id))
}

pub(super) fn lift_runtime_value(
    ctx: &ProviderContextBridgeValue,
    value: &RuntimeValue,
) -> Result<ExprSpec, EvalSignal> {
    if let Some(spec) = runtime_expr_spec(ctx, value)? {
        return Ok(spec);
    }
    Ok(ExprSpec::literal(runtime_to_literal(value)?))
}

pub(super) fn runtime_expr_spec(
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

pub(super) fn runtime_to_literal(value: &RuntimeValue) -> Result<IrLiteralData, EvalSignal> {
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
        RuntimeValue::Bytes(_)
        | RuntimeValue::Closure(_)
        | RuntimeValue::Macro(_)
        | RuntimeValue::Builtin(_)
        | RuntimeValue::HostFunction(_)
        | RuntimeValue::HostObject(_)
        | RuntimeValue::Ref(_)
        | RuntimeValue::UninitializedTopLevel => {
            Err(eval_err("CTFE result is not liftable into IR"))
        }
    }
}

pub(super) fn ctfe_result_value(result: CtfeResult) -> RuntimeValue {
    RuntimeValue::HostObject(Rc::new(CtfeResultBridgeValue::new(result)))
}

pub(super) fn ctx_host_object(ctx: &ProviderContextBridgeValue) -> Rc<dyn HostObject> {
    ctx.clone_host_object()
}

pub(super) fn emit_ctfe_error(
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
    diagnostic.code = Some(format!("kernel.ctfe.{kind}"));
    diagnostic.span = ctx
        .unit()
        .with_unit(|unit| unit.ir().source_span(node_id).cloned());
    ctx.push_diagnostic(diagnostic).map_err(eval_err)?;
    Ok(())
}

pub(super) fn rt_map<const N: usize>(entries: [(&str, RuntimeValue); N]) -> RuntimeValue {
    let mut map = IndexMap::new();
    for (key, value) in entries {
        map.insert(MapKey::Str(key.into()), value);
    }
    RuntimeValue::Map(Rc::new(RefCell::new(map)))
}

#[cfg(test)]
pub(super) fn rt_string(value: impl AsRef<str>) -> RuntimeValue {
    RuntimeValue::Str(value.as_ref().into())
}

pub(super) fn base_resolution_scope_for_context(
    ctx: &ProviderContextBridgeValue,
    unit: &Unit,
) -> CaapResult<SemanticRegistry> {
    let mut registry = base_resolution_scope(unit)?;
    define_initial_binding_entries(&mut registry, ctx.initial_bindings())?;
    define_base_semantic_entries(&mut registry, ctx)?;
    Ok(registry)
}

pub(super) fn define_base_semantic_entries(
    registry: &mut SemanticRegistry,
    ctx: &ProviderContextBridgeValue,
) -> CaapResult<()> {
    for entry in ctx.base_semantic_entries() {
        registry.define(entry)?;
    }
    Ok(())
}

pub(super) fn base_resolution_scope(unit: &Unit) -> CaapResult<SemanticRegistry> {
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

pub(super) fn define_initial_binding_entries(
    registry: &mut SemanticRegistry,
    initial: &[(String, RuntimeValue)],
) -> CaapResult<()> {
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

pub(super) fn define_qualified_initial_binding_entries(
    registry: &mut SemanticRegistry,
    prefix: &str,
    value: &RuntimeValue,
) -> CaapResult<()> {
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
                initial_qualified_phase(value, segment)?.unwrap_or(PhasePolicy::CompileTime);
            registry.define(entry)?;
        }
    }
    Ok(())
}

pub(super) fn initial_qualified_phase(
    value: &RuntimeValue,
    segment: &str,
) -> CaapResult<Option<PhasePolicy>> {
    let Some(contracts) = map_get_str(value, "__contract_semantics__") else {
        return Ok(None);
    };
    let RuntimeValue::Map(_) = contracts else {
        return Err(CaapError::compiler(
            "initial binding __contract_semantics__ must be a map",
        ));
    };
    let Some(contract) = map_get_str(&contracts, segment) else {
        return Ok(None);
    };
    let RuntimeValue::Map(_) = contract else {
        return Err(CaapError::compiler(format!(
            "initial binding contract for {segment:?} must be a map"
        )));
    };
    let Some(phase) = map_get_str(&contract, "phase") else {
        return Ok(None);
    };
    let RuntimeValue::Str(phase) = phase else {
        return Err(CaapError::compiler(format!(
            "initial binding contract phase for {segment:?} must be a string"
        )));
    };
    PhasePolicy::parse_label(phase.as_ref())
        .map(Some)
        .map_err(|error| {
            CaapError::compiler(format!(
                "initial binding contract phase for {segment:?} is invalid: {error}"
            ))
        })
}

pub(super) fn initial_binding_value(
    ctx: &ProviderContextBridgeValue,
    name: &str,
) -> Option<RuntimeValue> {
    lookup_initial_binding_value(ctx.initial_bindings(), name)
        .map(|value| contextualize_runtime_value(ctx, &value, 0))
}

pub(super) fn lookup_initial_binding_value(
    initial: &[(String, RuntimeValue)],
    name: &str,
) -> Option<RuntimeValue> {
    initial
        .iter()
        .find_map(|(candidate, value)| (candidate == name).then(|| value.clone()))
        .or_else(|| lookup_qualified_initial_binding_value(initial, name))
}

pub(super) fn lookup_qualified_initial_binding_value(
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

pub(super) fn map_get_str(value: &RuntimeValue, key: &str) -> Option<RuntimeValue> {
    let RuntimeValue::Map(map) = value else {
        return None;
    };
    map.borrow().get(&MapKey::Str(key.into())).cloned()
}

pub(super) fn contextualize_runtime_value(
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
            Environment::define(&env, "compiler", ctx.compiler_host_value());
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TraversalMode {
    Walk,
    FindFirst,
    Filter,
    Stateful,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TraversalOrder {
    Preorder,
    Postorder,
}

#[derive(Clone, Debug)]
pub(super) struct TraversalOptions {
    pub(super) mode: TraversalMode,
    pub(super) order: TraversalOrder,
    pub(super) kind: Option<String>,
    pub(super) initial_state: Option<RuntimeValue>,
}

impl TraversalOptions {
    pub(super) fn from_value(value: Option<&RuntimeValue>) -> Result<Self, EvalSignal> {
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

pub(super) fn traversal_mode(value: &RuntimeValue) -> Result<TraversalMode, EvalSignal> {
    match require_string(
        value,
        "ctfe-provider-traversal-walk mode option must be a string",
    )?
    .as_str()
    {
        "walk" => Ok(TraversalMode::Walk),
        "find_first" => Ok(TraversalMode::FindFirst),
        "filter" => Ok(TraversalMode::Filter),
        "stateful" => Ok(TraversalMode::Stateful),
        _ => Err(eval_err(
            "ctfe-provider-traversal-walk mode must be one of walk, find_first, filter, or stateful",
        )),
    }
}

pub(super) fn traversal_order(value: &RuntimeValue) -> Result<TraversalOrder, EvalSignal> {
    match require_string(
        value,
        "ctfe-provider-traversal-walk order option must be a string",
    )?
    .as_str()
    {
        "preorder" => Ok(TraversalOrder::Preorder),
        "postorder" => Ok(TraversalOrder::Postorder),
        _ => Err(eval_err(
            "ctfe-provider-traversal-walk order must be one of preorder or postorder",
        )),
    }
}

pub(super) fn collect_traversal_nodes(
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

pub(super) fn traversal_kind_matches(node: &Node, kind: Option<&str>) -> bool {
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

pub(super) fn traversal_child_states(
    value: &RuntimeValue,
) -> Result<Vec<(NodeId, RuntimeValue)>, EvalSignal> {
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

pub(super) fn runtime_sequence_values(
    value: &RuntimeValue,
    message: &str,
) -> Result<Vec<RuntimeValue>, EvalSignal> {
    match value {
        RuntimeValue::Tuple(items) => Ok(items.iter().cloned().collect()),
        RuntimeValue::List(items) => Ok(items.borrow().iter().cloned().collect()),
        _ => Err(eval_err(message)),
    }
}

pub(super) fn effect_tag_from_runtime_name(effect: &str) -> Result<EffectTag, EvalSignal> {
    EffectTag::new(effect)
        .map_err(|error| eval_err(format!("invalid effect tag {effect:?}: {error}")))
}

pub(super) fn semantic_value_to_runtime_in_context(
    ctx: &ProviderContextBridgeValue,
    value: &SemanticValue,
) -> RuntimeValue {
    semantic_value_to_runtime_with_nodes(value, &|node_id| node_handle(ctx, node_id))
}

pub(super) fn runtime_to_semantic(value: &RuntimeValue) -> Result<SemanticValue, EvalSignal> {
    runtime_value_to_semantic_with_nodes(
        value,
        "provider fact maps require string keys",
        "provider facts support scalar, sequence, map, and node values",
        &|value| match value {
            RuntimeValue::HostObject(object) => object
                .as_any()
                .downcast_ref::<NodeBridgeValue>()
                .map(|node| SemanticValue::Node(node.node_id())),
            _ => None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::super::semantic_projection::semantic_entry_to_runtime_value;
    use super::*;
    use crate::semantic::EffectPolicy;

    fn expect_initial_phase_error(value: RuntimeValue, segment: &str, expected: &str) {
        let err = initial_qualified_phase(&value, segment).unwrap_err();
        assert!(
            err.to_string().contains(expected),
            "expected error containing {expected:?}, got {err}"
        );
    }

    #[test]
    fn initial_qualified_phase_reads_explicit_contracts() {
        let value = rt_map([
            ("field", RuntimeValue::Int(1)),
            (
                "__contract_semantics__",
                rt_map([("field", rt_map([("phase", rt_string("runtime"))]))]),
            ),
        ]);

        assert_eq!(
            initial_qualified_phase(&value, "field").unwrap(),
            Some(PhasePolicy::Runtime)
        );
        assert_eq!(initial_qualified_phase(&value, "missing").unwrap(), None);
    }

    #[test]
    fn initial_qualified_phase_rejects_malformed_contracts() {
        expect_initial_phase_error(
            rt_map([("__contract_semantics__", rt_string("bad"))]),
            "field",
            "__contract_semantics__ must be a map",
        );
        expect_initial_phase_error(
            rt_map([
                ("field", RuntimeValue::Int(1)),
                (
                    "__contract_semantics__",
                    rt_map([("field", rt_string("bad"))]),
                ),
            ]),
            "field",
            "contract for \"field\" must be a map",
        );
        expect_initial_phase_error(
            rt_map([
                ("field", RuntimeValue::Int(1)),
                (
                    "__contract_semantics__",
                    rt_map([("field", rt_map([("phase", RuntimeValue::Int(1))]))]),
                ),
            ]),
            "field",
            "phase for \"field\" must be a string",
        );
        expect_initial_phase_error(
            rt_map([
                ("field", RuntimeValue::Int(1)),
                (
                    "__contract_semantics__",
                    rt_map([("field", rt_map([("phase", rt_string("sometimes"))]))]),
                ),
            ]),
            "field",
            "phase policy must be one of runtime, compile_time, or dual",
        );
    }

    #[test]
    fn initial_qualified_phase_ignores_legacy_contract_semantics_alias() {
        let value = rt_map([
            ("field", RuntimeValue::Int(1)),
            (
                "contract_semantics",
                rt_map([("field", rt_map([("phase", rt_string("runtime"))]))]),
            ),
        ]);

        assert_eq!(initial_qualified_phase(&value, "field").unwrap(), None);
    }

    #[test]
    fn qualified_initial_bindings_propagate_malformed_contracts() {
        let value = rt_map([
            ("field", RuntimeValue::Int(1)),
            (
                "__contract_semantics__",
                rt_map([("field", rt_map([("phase", rt_string("sometimes"))]))]),
            ),
        ]);
        let mut registry = SemanticRegistry::new();

        let err =
            define_qualified_initial_binding_entries(&mut registry, "env", &value).unwrap_err();

        assert!(err
            .to_string()
            .contains("initial binding contract phase for \"field\""));
        assert!(registry.lookup("env.field").unwrap().is_none());
    }

    #[test]
    fn semantic_entry_descriptor_preserves_stable_id() {
        let entry = semantic_entry_from_runtime_descriptor(
            &rt_map([
                ("name", rt_string("demo.entry")),
                ("source", rt_string("builtin")),
                ("stable_id", rt_string("unit:demo.entry")),
            ]),
            "entry descriptor is invalid",
        )
        .unwrap();

        assert_eq!(
            entry.stable_id.as_ref().map(|stable_id| stable_id.as_str()),
            Some("unit:demo.entry")
        );
    }

    #[test]
    fn semantic_entry_descriptor_round_trips_multi_effect_policy() {
        let mut entry = SemanticEntry::new("demo.effects", EntrySource::Registered).unwrap();
        entry.effect_policy =
            EffectPolicy::new(["read_ir".to_string(), "write_ir".to_string()]).unwrap();

        let restored = semantic_entry_from_runtime_descriptor(
            &semantic_entry_to_runtime_value(&entry),
            "entry descriptor is invalid",
        )
        .unwrap();

        assert_eq!(restored.effect_policy.tags(), entry.effect_policy.tags());
    }

    #[test]
    fn semantic_entry_descriptor_rejects_empty_required_fields() {
        let error = semantic_entry_from_runtime_descriptor(
            &rt_map([("name", rt_string("")), ("source", rt_string("builtin"))]),
            "entry descriptor is invalid",
        )
        .unwrap_err()
        .to_string();

        assert!(
            error.contains("resolved-name entry requires name"),
            "got {error}"
        );
    }

    #[test]
    fn traversal_options_accept_only_canonical_mode_and_order_labels() {
        assert_eq!(
            traversal_mode(&rt_string("find_first")).unwrap(),
            TraversalMode::FindFirst
        );
        assert_eq!(
            traversal_order(&rt_string("postorder")).unwrap(),
            TraversalOrder::Postorder
        );

        let mode_error = traversal_mode(&rt_string("find-first"))
            .unwrap_err()
            .to_string();
        assert!(mode_error.contains("find_first"));

        let order_error = traversal_order(&rt_string("post_order"))
            .unwrap_err()
            .to_string();
        assert!(order_error.contains("postorder"));
    }

    #[test]
    fn traversal_options_ignore_noncanonical_initial_state_key() {
        let options =
            TraversalOptions::from_value(Some(&rt_map([("initial-state", RuntimeValue::Int(1))])))
                .unwrap();

        assert!(options.initial_state.is_none());
    }
}
