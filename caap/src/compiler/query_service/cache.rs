//! Cache key computation, provider execution record construction, effect policy,
//! identity tokens, IR change tracking — pure helpers with no semantic serde.
use std::collections::{BTreeMap, BTreeSet};

use crate::artifacts::{ArtifactCache, ArtifactInvalidationRecord, ArtifactKey};
use crate::compiler::ANNOTATION_PREDICATE_PREFIX;
use crate::diagnostics::Diagnostic;
use crate::error::{CaapError, CaapResult};
use crate::ir::{Node, NodeId};
use crate::semantic::{
    symbol_subject_id, BuiltinEffectTag, PhasePolicy, SemanticSubjectId, SemanticValue,
    UnifiedSemanticGraphSnapshot,
};
use crate::unit::Unit;
use crate::values::{MapKey, RuntimeValue};

use super::super::fact_schema::{require_registry_name, FactSchemaRegistry};
use super::super::query_provider::{
    extend_unique, semantic_cell_tracking_key, semantic_subject_tracking_key, ProviderCacheEntry,
    ProviderRollbackSnapshot, ProviderTransactionMode, QueryPlanStep, QueryProvider,
    QueryProviderContext, QueryProviderExecutionRecord,
};
use super::super::session::Compiler;

pub(super) fn current_unix_ns() -> CaapResult<i64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
        CaapError::compiler("system clock is before UNIX epoch; cannot timestamp query execution")
    })?;
    i64::try_from(duration.as_nanos())
        .map_err(|_| CaapError::compiler("system clock timestamp exceeds semantic integer range"))
}

pub struct QueryStageCacheKeyInput<'a> {
    pub unit: &'a Unit,
    pub stage: &'a str,
    pub phase: PhasePolicy,
    pub initial_bindings: &'a [(String, RuntimeValue)],
    pub versions: QueryStageCacheVersions,
}

#[derive(Clone, Copy)]
pub struct QueryStageCacheVersions {
    pub provider_registry: u64,
    pub compiler_registry: u64,
    pub host: u64,
    pub bootstrap_capability: u64,
    pub bootstrap_image: u64,
}

pub fn query_stage_cache_key(
    input: QueryStageCacheKeyInput<'_>,
) -> CaapResult<Option<ArtifactKey>> {
    let Some(initial_binding_tokens) = initial_bindings_identity_token(input.initial_bindings)
    else {
        return Ok(None);
    };
    let unit_fingerprint = input.unit.content_fingerprint()?;
    let mut parts = vec![
        "query_stage".to_string(),
        input.stage.to_string(),
        input.phase.as_str().to_string(),
        input.unit.unit_id().to_string(),
        input.unit.version().to_string(),
        "unit_fingerprint".to_string(),
        unit_fingerprint.to_string(),
        "provider_registry".to_string(),
        input.versions.provider_registry.to_string(),
        "compiler_registry".to_string(),
        input.versions.compiler_registry.to_string(),
        "host".to_string(),
        input.versions.host.to_string(),
        "bootstrap_capabilities".to_string(),
        input.versions.bootstrap_capability.to_string(),
        "bootstrap_images".to_string(),
        input.versions.bootstrap_image.to_string(),
    ];
    parts.extend(initial_binding_tokens);
    ArtifactKey::new(parts).map(Some)
}

pub(super) fn provider_cache_key(
    compiler: &Compiler,
    unit: &Unit,
    provider: &QueryProvider,
    phase: PhasePolicy,
    initial_bindings: &[(String, RuntimeValue)],
) -> CaapResult<Option<ArtifactKey>> {
    if !provider_cacheable(provider) {
        return Ok(None);
    }
    let unit_fingerprint = unit.content_fingerprint()?;
    let mut parts = vec![
        "provider_cache".to_string(),
        provider.name.clone(),
        provider.stage.clone(),
        provider.cache_scope.as_str().to_string(),
        phase.as_str().to_string(),
        unit.unit_id().to_string(),
        unit.version().to_string(),
        "unit_fingerprint".to_string(),
        unit_fingerprint.to_string(),
        "provider_registry".to_string(),
        compiler.dispatch.registry.version().to_string(),
        "compiler_registry".to_string(),
        compiler.registry.version().to_string(),
        "host".to_string(),
        compiler.host.host_version().to_string(),
        "bootstrap_capabilities".to_string(),
        compiler.bootstrap.capabilities.version().to_string(),
        "bootstrap_images".to_string(),
        compiler.bootstrap.images.version().to_string(),
    ];
    let Some(initial_binding_tokens) = initial_bindings_identity_token(initial_bindings) else {
        return Ok(None);
    };
    parts.extend(initial_binding_tokens);
    ArtifactKey::new(parts).map(Some)
}

fn provider_cacheable(provider: &QueryProvider) -> bool {
    provider.cache_scope.is_cacheable()
        && !provider.reads.iter().any(|read| read == "files")
        && !provider
            .effect_tags
            .contains_builtin(BuiltinEffectTag::EmitEvents)
        && !provider
            .effect_tags
            .contains_builtin(BuiltinEffectTag::UseHostServices)
        && !provider
            .effect_tags
            .contains_builtin(BuiltinEffectTag::HostServices)
        && !provider
            .effect_tags
            .contains_builtin(BuiltinEffectTag::ReadFiles)
        && !provider
            .effect_tags
            .contains_builtin(BuiltinEffectTag::UseFiles)
}

pub fn provider_effect_policy_violation(
    provider: &QueryProvider,
    diagnostics_emitted: usize,
    ir_change: &ProviderIrChangeStats,
    attributes_changed: bool,
    semantic_writes: &SemanticWriteSummary,
    context: Option<&QueryProviderContext>,
) -> Option<String> {
    if ir_change.has_ir_change() && !provider_declares_effect(provider, BuiltinEffectTag::WriteIr) {
        return Some(format!(
            "query provider {} modified IR without declaring write-ir effect",
            provider.name
        ));
    }
    if diagnostics_emitted > 0
        && !provider_declares_effect(provider, BuiltinEffectTag::EmitDiagnostics)
    {
        return Some(format!(
            "query provider {} emitted diagnostics without declaring emit-diagnostics effect",
            provider.name
        ));
    }
    if attributes_changed && !provider_declares_effect(provider, BuiltinEffectTag::WriteAttributes)
    {
        return Some(format!(
            "query provider {} modified unit attributes without declaring write-attributes effect",
            provider.name
        ));
    }
    if semantic_writes.symbols
        && !provider_declares_effect(provider, BuiltinEffectTag::WriteSymbols)
    {
        return Some(format!(
            "query provider {} modified symbols without declaring write-symbols effect",
            provider.name
        ));
    }
    if semantic_writes.facts && !provider_declares_effect(provider, BuiltinEffectTag::WriteFacts) {
        return Some(format!(
            "query provider {} modified facts without declaring write-facts effect",
            provider.name
        ));
    }
    if semantic_writes.attributes
        && !provider_declares_effect(provider, BuiltinEffectTag::WriteAttributes)
    {
        return Some(format!(
            "query provider {} modified semantic attributes without declaring write-attributes effect",
            provider.name
        ));
    }
    if context.is_some_and(|context| !context.reads_files.is_empty())
        && !provider_declares_effect(provider, BuiltinEffectTag::ReadFiles)
    {
        return Some(format!(
            "query provider {} read files without declaring read-files effect",
            provider.name
        ));
    }
    if context.is_some_and(|context| !context.writes_files.is_empty())
        && !provider_declares_effect(provider, BuiltinEffectTag::WriteFiles)
    {
        return Some(format!(
            "query provider {} wrote files without declaring write-files effect",
            provider.name
        ));
    }
    None
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SemanticWriteSummary {
    pub facts: bool,
    pub attributes: bool,
    pub symbols: bool,
}

pub(super) fn semantic_write_summary(
    before: &UnifiedSemanticGraphSnapshot,
    after: &UnifiedSemanticGraphSnapshot,
) -> SemanticWriteSummary {
    let mut summary = SemanticWriteSummary::default();
    collect_symbol_write_summary(before, after, &mut summary);
    collect_fact_write_summary(before, after, &mut summary);
    collect_semantic_registry_write_summary(before, after, &mut summary);
    collect_stable_id_write_summary(before, after, &mut summary);
    summary
}

pub(super) fn validate_unit_fact_schema(
    registry: &FactSchemaRegistry,
    unit: &Unit,
) -> CaapResult<()> {
    if registry.schemas().is_empty() {
        return Ok(());
    }
    for (subject, predicate, value) in unit.semantics().query_facts(None, None)? {
        registry
            .validate_value(&predicate, &value)
            .map_err(|error| {
                CaapError::compiler(format!(
                    "unit {:?} fact {}:{} {:?} violates compiler fact schema: {error}",
                    unit.unit_id(),
                    subject.kind,
                    subject.value,
                    predicate
                ))
            })?;
    }
    Ok(())
}

pub(super) fn merge_semantic_write_tracking(
    context: Option<QueryProviderContext>,
    before: &UnifiedSemanticGraphSnapshot,
    after: &UnifiedSemanticGraphSnapshot,
) -> Option<QueryProviderContext> {
    let mut context = context?;
    let mut writes_subjects = Vec::new();
    let mut write_cells = Vec::new();

    let symbol_write_start = write_cells.len();
    collect_symbol_write_tracking(before, after, &mut writes_subjects, &mut write_cells);
    if write_cells.len() != symbol_write_start {
        push_unit_write(
            &context.unit_id,
            &mut writes_subjects,
            &mut write_cells,
            "symbols",
        );
    }
    let fact_write_start = write_cells.len();
    collect_fact_write_tracking(before, after, &mut writes_subjects, &mut write_cells);
    if write_cells.len() != fact_write_start {
        push_unit_write(
            &context.unit_id,
            &mut writes_subjects,
            &mut write_cells,
            "facts",
        );
    }
    collect_semantic_registry_write_tracking(before, after, &mut writes_subjects, &mut write_cells);
    collect_stable_id_write_tracking(before, after, &mut writes_subjects, &mut write_cells);

    extend_unique(&mut context.writes_subjects, writes_subjects);
    extend_unique(&mut context.write_cells, write_cells);
    Some(context)
}

fn collect_symbol_write_tracking(
    before: &UnifiedSemanticGraphSnapshot,
    after: &UnifiedSemanticGraphSnapshot,
    writes_subjects: &mut Vec<String>,
    write_cells: &mut Vec<String>,
) {
    let before_symbols: BTreeMap<_, _> = before
        .symbols
        .iter()
        .map(|(name, entry)| (name, entry))
        .collect();
    let after_symbols: BTreeMap<_, _> = after
        .symbols
        .iter()
        .map(|(name, entry)| (name, entry))
        .collect();
    let mut names: BTreeSet<&String> = before_symbols.keys().copied().collect();
    names.extend(after_symbols.keys().copied());

    for name in names {
        if before_symbols.get(name) == after_symbols.get(name) {
            continue;
        }
        if let Ok(subject) = symbol_subject_id(name) {
            push_semantic_write(writes_subjects, write_cells, &subject, "symbol.entry");
        }
    }
}

fn collect_symbol_write_summary(
    before: &UnifiedSemanticGraphSnapshot,
    after: &UnifiedSemanticGraphSnapshot,
    summary: &mut SemanticWriteSummary,
) {
    if before.symbols != after.symbols {
        summary.symbols = true;
    }
}

fn collect_fact_write_tracking(
    before: &UnifiedSemanticGraphSnapshot,
    after: &UnifiedSemanticGraphSnapshot,
    writes_subjects: &mut Vec<String>,
    write_cells: &mut Vec<String>,
) {
    let before_facts = fact_history_map(before);
    let after_facts = fact_history_map(after);
    let mut cells: BTreeSet<(&SemanticSubjectId, &str)> = BTreeSet::new();
    for (subject, inner) in &before_facts {
        for predicate in inner.keys() {
            cells.insert((subject, predicate.as_str()));
        }
    }
    for (subject, inner) in &after_facts {
        for predicate in inner.keys() {
            cells.insert((subject, predicate.as_str()));
        }
    }
    for (subject, predicate) in cells {
        if before_facts.get(subject).and_then(|m| m.get(predicate))
            == after_facts.get(subject).and_then(|m| m.get(predicate))
        {
            continue;
        }
        push_semantic_write(writes_subjects, write_cells, subject, predicate);
    }
}

fn collect_fact_write_summary(
    before: &UnifiedSemanticGraphSnapshot,
    after: &UnifiedSemanticGraphSnapshot,
    summary: &mut SemanticWriteSummary,
) {
    let before_facts = fact_history_map(before);
    let after_facts = fact_history_map(after);
    let mut cells: BTreeSet<(&SemanticSubjectId, &str)> = BTreeSet::new();
    for (subject, inner) in &before_facts {
        for predicate in inner.keys() {
            cells.insert((subject, predicate.as_str()));
        }
    }
    for (subject, inner) in &after_facts {
        for predicate in inner.keys() {
            cells.insert((subject, predicate.as_str()));
        }
    }
    for (subject, predicate) in cells {
        if before_facts.get(subject).and_then(|m| m.get(predicate))
            == after_facts.get(subject).and_then(|m| m.get(predicate))
        {
            continue;
        }
        if predicate.starts_with(ANNOTATION_PREDICATE_PREFIX) {
            summary.attributes = true;
        } else {
            summary.facts = true;
        }
    }
}

fn collect_semantic_registry_write_tracking(
    before: &UnifiedSemanticGraphSnapshot,
    after: &UnifiedSemanticGraphSnapshot,
    writes_subjects: &mut Vec<String>,
    write_cells: &mut Vec<String>,
) {
    let before_entries: BTreeMap<_, _> = before
        .semantics
        .entries
        .iter()
        .map(|(name, entry)| (name, entry))
        .collect();
    let after_entries: BTreeMap<_, _> = after
        .semantics
        .entries
        .iter()
        .map(|(name, entry)| (name, entry))
        .collect();
    let mut names: BTreeSet<&String> = before_entries.keys().copied().collect();
    names.extend(after_entries.keys().copied());

    for name in names {
        if before_entries.get(name) == after_entries.get(name) {
            continue;
        }
        let subject = format!("semantic:{name}");
        push_tracking_write(writes_subjects, write_cells, subject, "semantic.entry");
    }
}

fn collect_semantic_registry_write_summary(
    before: &UnifiedSemanticGraphSnapshot,
    after: &UnifiedSemanticGraphSnapshot,
    summary: &mut SemanticWriteSummary,
) {
    if before.semantics.entries != after.semantics.entries {
        summary.symbols = true;
    }
}

fn collect_stable_id_write_tracking(
    before: &UnifiedSemanticGraphSnapshot,
    after: &UnifiedSemanticGraphSnapshot,
    writes_subjects: &mut Vec<String>,
    write_cells: &mut Vec<String>,
) {
    let before_ids: BTreeMap<_, _> = before
        .stable_ids
        .iter()
        .map(|(key, stable_id)| (key, stable_id))
        .collect();
    let after_ids: BTreeMap<_, _> = after
        .stable_ids
        .iter()
        .map(|(key, stable_id)| (key, stable_id))
        .collect();
    let mut keys: BTreeSet<&String> = before_ids.keys().copied().collect();
    keys.extend(after_ids.keys().copied());

    for key in keys {
        if before_ids.get(key) == after_ids.get(key) {
            continue;
        }
        let subject = format!("stable-id:{key}");
        push_tracking_write(writes_subjects, write_cells, subject, "stable_id");
    }
}

fn collect_stable_id_write_summary(
    before: &UnifiedSemanticGraphSnapshot,
    after: &UnifiedSemanticGraphSnapshot,
    summary: &mut SemanticWriteSummary,
) {
    if before.stable_ids != after.stable_ids {
        summary.symbols = true;
    }
}

fn fact_history_map(
    snapshot: &UnifiedSemanticGraphSnapshot,
) -> BTreeMap<SemanticSubjectId, BTreeMap<String, Vec<(u64, SemanticValue)>>> {
    let Some(facts) = &snapshot.facts else {
        return BTreeMap::new();
    };
    let mut map: BTreeMap<SemanticSubjectId, BTreeMap<String, Vec<(u64, SemanticValue)>>> =
        BTreeMap::new();
    for (version, subject, predicate, value) in &facts.triples {
        map.entry(subject.clone())
            .or_default()
            .entry(predicate.clone())
            .or_default()
            .push((*version, value.clone()));
    }
    map
}

fn push_semantic_write(
    writes_subjects: &mut Vec<String>,
    write_cells: &mut Vec<String>,
    subject: &SemanticSubjectId,
    predicate: &str,
) {
    let subject = semantic_subject_tracking_key(subject);
    push_tracking_write(writes_subjects, write_cells, subject, predicate);
}

fn push_tracking_write(
    writes_subjects: &mut Vec<String>,
    write_cells: &mut Vec<String>,
    subject: String,
    predicate: &str,
) {
    writes_subjects.push(subject.clone());
    write_cells.push(semantic_cell_tracking_key(&subject, predicate));
}

fn push_unit_write(
    unit_id: &str,
    writes_subjects: &mut Vec<String>,
    write_cells: &mut Vec<String>,
    predicate: &str,
) {
    if let Ok(subject) = SemanticSubjectId::new("unit", unit_id.to_string()) {
        push_semantic_write(writes_subjects, write_cells, &subject, predicate);
    }
}

fn provider_declares_effect(provider: &QueryProvider, expected: BuiltinEffectTag) -> bool {
    provider.effect_tags.contains_builtin(expected)
}

pub(super) fn capture_provider_rollback_snapshot(
    unit: &Unit,
    provider: &QueryProvider,
) -> Option<ProviderRollbackSnapshot> {
    match provider_transaction_mode(provider) {
        ProviderTransactionMode::None => None,
        ProviderTransactionMode::Semantic => Some(ProviderRollbackSnapshot::Semantic(
            unit.semantics().snapshot(),
        )),
        ProviderTransactionMode::Attributes => Some(ProviderRollbackSnapshot::Attributes(
            unit.capture_attribute_snapshot(),
        )),
        ProviderTransactionMode::Unit => {
            Some(ProviderRollbackSnapshot::Unit(Box::new(unit.snapshot())))
        }
    }
}

pub(super) fn restore_provider_rollback_snapshot(
    unit: &mut Unit,
    snapshot: ProviderRollbackSnapshot,
) -> CaapResult<()> {
    match snapshot {
        ProviderRollbackSnapshot::Semantic(snapshot) => {
            Ok(unit.semantics_mut()?.restore_snapshot(snapshot)?)
        }
        ProviderRollbackSnapshot::Attributes(snapshot) => {
            unit.restore_attribute_snapshot(snapshot)?;
            Ok(())
        }
        ProviderRollbackSnapshot::Unit(snapshot) => Ok(unit.restore_snapshot(*snapshot)?),
    }
}

fn provider_transaction_mode(provider: &QueryProvider) -> ProviderTransactionMode {
    let mut unit_writes: BTreeSet<String> = provider
        .writes
        .iter()
        .filter(|domain| matches!(domain.as_str(), "ir" | "attributes" | "facts" | "symbols"))
        .cloned()
        .collect();
    if unit_writes.is_empty() {
        return ProviderTransactionMode::None;
    }
    if unit_writes
        .iter()
        .all(|domain| matches!(domain.as_str(), "facts" | "symbols"))
    {
        return ProviderTransactionMode::Semantic;
    }
    if unit_writes.len() == 1 && unit_writes.remove("attributes") {
        return ProviderTransactionMode::Attributes;
    }
    ProviderTransactionMode::Unit
}

pub(super) fn provider_restart_stage(
    compiler: &Compiler,
    provider: &QueryProvider,
    committed_change: bool,
) -> CaapResult<Option<String>> {
    if let Some(stage) = compiler.dispatch.pending_restart.clone() {
        return Ok(Some(stage));
    }
    if !committed_change {
        return Ok(None);
    }
    if !provider
        .resume_policy
        .allows_restart(compiler.bootstrap.active_depth > 0)
    {
        return Ok(None);
    }
    if provider
        .effect_tags
        .contains_builtin(BuiltinEffectTag::UseHostServices)
        || provider
            .effect_tags
            .contains_builtin(BuiltinEffectTag::HostServices)
    {
        return Ok(None);
    }
    compiler
        .dispatch
        .registry
        .explicit_restart_stage_for(provider.stage.clone())
}

pub fn initial_bindings_identity_token(
    initial_bindings: &[(String, RuntimeValue)],
) -> Option<Vec<String>> {
    if initial_bindings.is_empty() {
        return Some(Vec::new());
    }
    let mut items: Vec<(String, String)> = initial_bindings
        .iter()
        .map(|(name, value)| runtime_value_identity_token(value).map(|token| (name.clone(), token)))
        .collect::<Option<_>>()?;
    items.sort();
    Some(
        items
            .into_iter()
            .flat_map(|(name, value)| ["initial_binding".to_string(), name, value])
            .collect(),
    )
}

pub(super) fn normalize_initial_bindings(
    initial_bindings: Vec<(String, RuntimeValue)>,
) -> CaapResult<Vec<(String, RuntimeValue)>> {
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::with_capacity(initial_bindings.len());
    for (name, value) in initial_bindings {
        let name = require_registry_name(name)?;
        if !seen.insert(name.clone()) {
            return Err(CaapError::compiler(format!(
                "query initial binding {name:?} is duplicated"
            )));
        }
        normalized.push((name, value));
    }
    normalized.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(normalized)
}

fn runtime_value_identity_token(value: &RuntimeValue) -> Option<String> {
    match value {
        RuntimeValue::Null => Some("null".to_string()),
        RuntimeValue::Bool(value) => Some(format!("bool:{value}")),
        RuntimeValue::Int(value) => Some(format!("int:{value}")),
        RuntimeValue::Float(value) => Some(format!("float:{value:?}")),
        RuntimeValue::Str(value) => Some(format!("str:{value}")),
        RuntimeValue::Bytes(value) => Some(format!(
            "bytes:{}:{}",
            value.len(),
            value.iter().map(|b| format!("{b:02x}")).collect::<String>()
        )),
        RuntimeValue::Tuple(items) => {
            let items = items
                .iter()
                .map(runtime_value_identity_token)
                .collect::<Option<Vec<_>>>()?
                .join(",");
            Some(format!("tuple:[{items}]"))
        }
        RuntimeValue::List(items) => {
            let items = items
                .borrow()
                .iter()
                .map(runtime_value_identity_token)
                .collect::<Option<Vec<_>>>()?
                .join(",");
            Some(format!("list:[{items}]"))
        }
        RuntimeValue::Map(map) => {
            let mut items: Vec<(String, String)> = map
                .borrow()
                .iter()
                .map(|(key, value)| {
                    runtime_value_identity_token(value)
                        .map(|value| (map_key_identity_token(key), value))
                })
                .collect::<Option<_>>()?;
            items.sort();
            let items = items
                .into_iter()
                .map(|(key, value)| format!("{key}:{value}"))
                .collect::<Vec<_>>()
                .join(",");
            Some(format!("map:{{{items}}}"))
        }
        RuntimeValue::Closure(_)
        | RuntimeValue::Macro(_)
        | RuntimeValue::Builtin(_)
        | RuntimeValue::HostFunction(_)
        | RuntimeValue::HostObject(_)
        // A mutable reference has no stable identity token (its contents can
        // change), so a query input containing one is not identity-cacheable.
        | RuntimeValue::Ref(_) => None,
        RuntimeValue::UninitializedTopLevel => Some("uninitialized_top_level".to_string()),
    }
}

fn map_key_identity_token(key: &MapKey) -> String {
    match key {
        MapKey::Null => "null".to_string(),
        MapKey::Bool(value) => format!("bool:{value}"),
        MapKey::Int(value) => format!("int:{value}"),
        MapKey::Str(value) => format!("str:{value}"),
    }
}

pub fn query_step_invalidation(
    cache: &ArtifactCache,
    step: &QueryPlanStep,
) -> Option<ArtifactInvalidationRecord> {
    let key = step.artifact_key.as_ref()?;
    cache.latest_invalidation_for_key(key).cloned().or_else(|| {
        cache
            .lineage_id_for_key(key)
            .and_then(|lineage_id| cache.latest_invalidation_for_lineage(lineage_id))
            .cloned()
    })
}

pub(super) struct ProviderExecutionRecordInput<'a> {
    pub provider: &'a QueryProvider,
    pub iteration: usize,
    pub changed: bool,
    pub diagnostics_emitted: usize,
    pub rolled_back: bool,
    pub stopped_by_error: bool,
    pub outcome_kind: String,
    pub diagnostic_codes: Vec<String>,
    pub ir_change: ProviderIrChangeStats,
    pub attributes_changed: bool,
    pub restart_stage: Option<String>,
    pub context: Option<&'a QueryProviderContext>,
}

pub(super) fn provider_execution_record(
    input: ProviderExecutionRecordInput<'_>,
) -> CaapResult<QueryProviderExecutionRecord> {
    let ProviderExecutionRecordInput {
        provider,
        iteration,
        changed,
        diagnostics_emitted,
        rolled_back,
        stopped_by_error,
        outcome_kind,
        diagnostic_codes,
        ir_change,
        attributes_changed,
        restart_stage,
        context,
    } = input;
    let restart_requested = restart_stage.is_some();
    let reads_subjects = merge_runtime_tracking(provider.reads.clone(), context, |context| {
        &context.reads_subjects
    });
    let mut writes_subjects = merge_runtime_tracking(provider.writes.clone(), context, |context| {
        &context.writes_subjects
    });
    let read_cells = merge_runtime_tracking(provider.reads.clone(), context, |context| {
        &context.read_cells
    });
    let mut write_cells = merge_runtime_tracking(provider.writes.clone(), context, |context| {
        &context.write_cells
    });
    if ir_change.has_ir_change() {
        if let Some(context) = context {
            push_unit_write(
                &context.unit_id,
                &mut writes_subjects,
                &mut write_cells,
                "ir",
            );
            writes_subjects.sort();
            writes_subjects.dedup();
            write_cells.sort();
            write_cells.dedup();
        }
    }
    if attributes_changed {
        if let Some(context) = context {
            push_unit_write(
                &context.unit_id,
                &mut writes_subjects,
                &mut write_cells,
                "attributes",
            );
            writes_subjects.sort();
            writes_subjects.dedup();
            write_cells.sort();
            write_cells.dedup();
        }
    }
    let reads_files = context
        .map(|context| context.reads_files.clone())
        .unwrap_or_default();
    let writes_files = context
        .map(|context| context.writes_files.clone())
        .unwrap_or_default();
    let artifact_dependencies = context
        .map(|context| context.artifact_dependencies.clone())
        .unwrap_or_default();
    Ok(QueryProviderExecutionRecord {
        recorded_at_unix_ns: current_unix_ns()?,
        provider_name: provider.name.clone(),
        stage: provider.stage.clone(),
        family: provider.family.clone(),
        phase_policy: provider.phase_policy,
        effect_tags: provider.effect_tags.clone(),
        requires: provider.requires.clone(),
        requires_data: provider.requires_data.clone(),
        provides_data: provider.provides_data.clone(),
        provides: provider.provides.clone(),
        reads: provider.reads.clone(),
        writes: provider.writes.clone(),
        reads_subjects,
        writes_subjects,
        read_cells,
        write_cells,
        reads_files,
        writes_files,
        artifact_dependencies,
        cache_scope: provider.cache_scope,
        resume_policy: provider.resume_policy,
        iteration,
        changed,
        diagnostics_emitted,
        rolled_back,
        stopped_by_error,
        outcome_kind,
        diagnostic_codes,
        rewrite_count: ir_change.rewrite_count,
        erased_count: ir_change.erased_count,
        touched_node_kinds: ir_change.touched_node_kinds,
        change_domains: ir_change.change_domains,
        restart_requested,
        restart_stage,
        outcome_summary: Vec::new(),
    })
}

pub(super) fn cached_provider_execution_record(
    provider: &QueryProvider,
    iteration: usize,
    entry: &ProviderCacheEntry,
) -> QueryProviderExecutionRecord {
    QueryProviderExecutionRecord {
        recorded_at_unix_ns: entry.recorded_at_unix_ns,
        provider_name: provider.name.clone(),
        stage: provider.stage.clone(),
        family: provider.family.clone(),
        phase_policy: provider.phase_policy,
        effect_tags: provider.effect_tags.clone(),
        requires: provider.requires.clone(),
        requires_data: provider.requires_data.clone(),
        provides_data: provider.provides_data.clone(),
        provides: provider.provides.clone(),
        reads: provider.reads.clone(),
        writes: provider.writes.clone(),
        reads_subjects: entry.reads_subjects.clone(),
        writes_subjects: entry.writes_subjects.clone(),
        read_cells: entry.read_cells.clone(),
        write_cells: entry.write_cells.clone(),
        reads_files: entry.reads_files.clone(),
        writes_files: entry.writes_files.clone(),
        artifact_dependencies: entry.artifact_dependencies.clone(),
        cache_scope: provider.cache_scope,
        resume_policy: provider.resume_policy,
        iteration,
        changed: entry.changed,
        diagnostics_emitted: entry.diagnostics.len(),
        rolled_back: false,
        stopped_by_error: false,
        outcome_kind: "cached".to_string(),
        diagnostic_codes: entry
            .diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.code.clone())
            .collect(),
        rewrite_count: 0,
        erased_count: 0,
        touched_node_kinds: Vec::new(),
        change_domains: Vec::new(),
        restart_requested: entry.restart_requested,
        restart_stage: entry.restart_stage.clone(),
        outcome_summary: Vec::new(),
    }
}

pub(super) fn provider_cache_entry(
    unit: &Unit,
    provider: &QueryProvider,
    record: &QueryProviderExecutionRecord,
    diagnostics: Vec<Diagnostic>,
    dynamic_requires: Vec<String>,
) -> Option<ProviderCacheEntry> {
    if !provider_cacheable(provider) || record.stopped_by_error || record.rolled_back {
        return None;
    }
    let snapshot = (record.changed
        && provider_transaction_mode(provider) != ProviderTransactionMode::None)
        .then(|| unit.snapshot());
    Some(ProviderCacheEntry {
        recorded_at_unix_ns: record.recorded_at_unix_ns,
        snapshot,
        diagnostics,
        reads_subjects: record.reads_subjects.clone(),
        writes_subjects: record.writes_subjects.clone(),
        read_cells: record.read_cells.clone(),
        write_cells: record.write_cells.clone(),
        reads_files: record.reads_files.clone(),
        writes_files: record.writes_files.clone(),
        artifact_dependencies: record.artifact_dependencies.clone(),
        dynamic_requires,
        changed: record.changed,
        restart_requested: record.restart_requested,
        restart_stage: record.restart_stage.clone(),
    })
}

pub fn collect_record_strings(
    records: &[QueryProviderExecutionRecord],
    selector: impl Fn(&QueryProviderExecutionRecord) -> &Vec<String>,
) -> Vec<String> {
    let mut values = BTreeSet::new();
    for record in records {
        values.extend(selector(record).iter().cloned());
    }
    values.into_iter().collect()
}

fn merge_runtime_tracking(
    mut static_values: Vec<String>,
    context: Option<&QueryProviderContext>,
    selector: impl Fn(&QueryProviderContext) -> &Vec<String>,
) -> Vec<String> {
    if let Some(context) = context {
        extend_unique(&mut static_values, selector(context).iter().cloned());
    } else {
        static_values.sort();
        static_values.dedup();
    }
    static_values
}

#[derive(Clone, Debug, Default)]
pub struct ProviderIrChangeStats {
    pub rewrite_count: usize,
    pub erased_count: usize,
    pub touched_node_kinds: Vec<String>,
    pub change_domains: Vec<String>,
}

impl ProviderIrChangeStats {
    pub fn has_ir_change(&self) -> bool {
        self.rewrite_count > 0 || self.erased_count > 0
    }
}

pub(super) fn provider_ir_change_stats(
    before_nodes: &[Node],
    unit: &Unit,
) -> ProviderIrChangeStats {
    let before = node_map(before_nodes.iter().cloned());
    let after_template = unit.ir().to_template();
    let after = node_map(after_template.nodes);
    let mut touched_node_kinds = BTreeSet::new();
    let mut rewrite_count = 0;
    let mut erased_count = 0;

    for (node_id, before_node) in &before {
        match after.get(node_id) {
            None => {
                erased_count += 1;
                touched_node_kinds.insert(node_kind_label(before_node).to_string());
            }
            Some(after_node) if after_node != before_node => {
                rewrite_count += 1;
                touched_node_kinds.insert(node_kind_label(before_node).to_string());
                touched_node_kinds.insert(node_kind_label(after_node).to_string());
            }
            Some(_) => {}
        }
    }
    for (node_id, after_node) in &after {
        if !before.contains_key(node_id) {
            rewrite_count += 1;
            touched_node_kinds.insert(node_kind_label(after_node).to_string());
        }
    }

    let mut change_domains = Vec::new();
    if rewrite_count > 0 || erased_count > 0 {
        change_domains.push("ir".to_string());
    }
    ProviderIrChangeStats {
        rewrite_count,
        erased_count,
        touched_node_kinds: touched_node_kinds.into_iter().collect(),
        change_domains,
    }
}

fn node_map(nodes: impl IntoIterator<Item = Node>) -> BTreeMap<NodeId, Node> {
    nodes.into_iter().map(|node| (node.id(), node)).collect()
}

fn node_kind_label(node: &Node) -> &'static str {
    match node {
        Node::Call(_) => "Call",
        Node::Name(_) => "Name",
        Node::Literal(_) => "Literal",
    }
}
