pub(super) mod cache;
pub(super) mod serde;

pub use cache::{
    collect_record_strings, provider_effect_policy_violation, query_stage_cache_key,
    query_step_invalidation, QueryStageCacheKeyInput, QueryStageCacheVersions,
};
#[cfg(test)]
pub use cache::{initial_bindings_identity_token, ProviderIrChangeStats, SemanticWriteSummary};
#[cfg(test)]
pub use serde::cached_execution_records;
pub use serde::{
    cached_execution_records_for_request, cached_query_stage_unit_template,
    provider_execution_record_to_semantic_value, CachedQueryStageReplayRequest,
};

use std::any::Any;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::Instant;

use super::query_provider::{
    enforce_query_effect_policy, extend_available_data, EffectSet, NativeProviderContext,
    QueryExecutionOptions, QueryInnerRequest, QueryPlan, QueryPlanStep, QueryProvider,
    QueryProviderCallbackOutcome, QueryProviderContext, QueryProviderExecutionRecord,
    QueryTransactionMode,
};
use super::session::{elapsed_ms_string, Compiler};
use crate::artifacts::{ArtifactKey, ArtifactValue, QueryStageArtifactValue};
use crate::diagnostics::DiagnosticSeverity;
use crate::error::{CaapError, CaapResult};
use crate::semantic::{PhasePolicy, SemanticValue};
use crate::unit::Unit;

use cache::{
    cached_provider_execution_record, capture_provider_rollback_snapshot, current_unix_ns,
    normalize_initial_bindings, provider_cache_entry, provider_cache_key,
    provider_execution_record, provider_ir_change_stats, provider_restart_stage,
    restore_provider_rollback_snapshot, validate_unit_fact_schema, ProviderExecutionRecordInput,
};
use serde::{semantic_effect_set, semantic_string_list};

pub struct CompilerQueryService<'a> {
    pub(super) compiler: &'a mut Compiler,
}

impl<'a> CompilerQueryService<'a> {
    pub fn plan_query(&self, target: &str, phase: PhasePolicy) -> CaapResult<QueryPlan> {
        self.compiler.dispatch.registry.plan(target, phase)
    }

    pub fn compile(&mut self, unit: &mut Unit) -> CaapResult<QueryPlan> {
        self.query("compile_unit", unit, PhasePolicy::CompileTime)
    }

    pub fn query(
        &mut self,
        target: &str,
        unit: &mut Unit,
        phase: PhasePolicy,
    ) -> CaapResult<QueryPlan> {
        self.query_with_options(target, unit, phase, QueryExecutionOptions::default())
    }

    pub fn query_with_transaction_mode(
        &mut self,
        target: &str,
        unit: &mut Unit,
        phase: PhasePolicy,
        transaction_mode: QueryTransactionMode,
    ) -> CaapResult<QueryPlan> {
        self.query_with_options(
            target,
            unit,
            phase,
            QueryExecutionOptions::new().with_transaction_mode(transaction_mode),
        )
    }

    pub fn query_with_options(
        &mut self,
        target: &str,
        unit: &mut Unit,
        phase: PhasePolicy,
        options: QueryExecutionOptions,
    ) -> CaapResult<QueryPlan> {
        self.query_with_options_from_origin(None, target, unit, phase, options)
    }

    pub fn query_from_stage_to_target_with_options(
        &mut self,
        from_stage: &str,
        target: &str,
        unit: &mut Unit,
        phase: PhasePolicy,
        options: QueryExecutionOptions,
    ) -> CaapResult<QueryPlan> {
        self.query_with_options_from_origin(Some(from_stage), target, unit, phase, options)
    }

    fn query_with_options_from_origin(
        &mut self,
        origin_stage: Option<&str>,
        target: &str,
        unit: &mut Unit,
        phase: PhasePolicy,
        options: QueryExecutionOptions,
    ) -> CaapResult<QueryPlan> {
        tracing::debug!(target, phase = ?phase, "provider query");
        let restart_limit = options.restart_limit;
        let allowed_effect_tags = options.allowed_effect_tags;
        let initial_bindings = normalize_initial_bindings(options.initial_bindings)?;
        let deadline = options.timeout.map(|t| Instant::now() + t);
        match options.transaction_mode {
            QueryTransactionMode::InPlace => self.query_inner(QueryInnerRequest {
                origin_stage,
                target,
                unit,
                phase,
                restart_limit,
                allowed_effect_tags: allowed_effect_tags.as_ref(),
                initial_bindings: &initial_bindings,
                deadline,
            }),
            QueryTransactionMode::AtomicUnit => {
                let unit_snapshot = unit.snapshot();
                let cache_snapshot = self.compiler.cache.artifact_cache.snapshot();
                let ctfe_cache_snapshot = self.compiler.cache.ctfe_cache.clone();
                let dynamic_requires_snapshot = self.compiler.dispatch.dynamic_requires.clone();
                let pending_restart_snapshot = self.compiler.dispatch.pending_restart.clone();
                let active_context_snapshot = self.compiler.dispatch.active_context.clone();
                match self.query_inner(QueryInnerRequest {
                    origin_stage,
                    target,
                    unit,
                    phase,
                    restart_limit,
                    allowed_effect_tags: allowed_effect_tags.as_ref(),
                    initial_bindings: &initial_bindings,
                    deadline,
                }) {
                    Ok(plan) => Ok(plan),
                    Err(error) => {
                        unit.restore_snapshot(unit_snapshot)
                            .map_err(|rollback_error| {
                                CaapError::compiler(format!(
                                    "{error}; unit rollback failed: {rollback_error}"
                                ))
                            })?;
                        Rc::make_mut(&mut self.compiler.cache.artifact_cache)
                            .restore_snapshot(cache_snapshot)
                            .map_err(|rollback_error| {
                                CaapError::compiler(format!(
                                    "{error}; query rollback failed: {rollback_error}"
                                ))
                            })?;
                        self.compiler.cache.ctfe_cache = ctfe_cache_snapshot;
                        self.compiler.dispatch.dynamic_requires = dynamic_requires_snapshot;
                        self.compiler.dispatch.pending_restart = pending_restart_snapshot;
                        self.compiler.dispatch.active_context = active_context_snapshot;
                        Err(error)
                    }
                }
            }
        }
    }

    fn query_inner(&mut self, request: QueryInnerRequest<'_>) -> CaapResult<QueryPlan> {
        let QueryInnerRequest {
            origin_stage,
            target,
            unit,
            phase,
            restart_limit,
            allowed_effect_tags,
            initial_bindings,
            deadline,
        } = request;
        let mut plan = match origin_stage {
            Some(origin_stage) => self.compiler.dispatch.registry.plan_from_stage_to_target(
                origin_stage,
                target,
                phase,
            )?,
            None => self.compiler.dispatch.registry.plan(target, phase)?,
        };
        enforce_query_effect_policy(&plan, allowed_effect_tags)?;
        self.compiler.emit_compiler_event(
            "query.plan",
            Some(plan.target.clone()),
            "planned query route",
            [
                ("phase".to_string(), phase.as_str().to_string()),
                ("steps".to_string(), plan.steps.len().to_string()),
                ("unit".to_string(), unit.unit_id().to_string()),
            ],
        )?;
        let mut index = 0;
        let mut restarts_remaining = restart_limit;
        let mut satisfied_providers = self
            .compiler
            .dispatch
            .registry
            .provider_names_for_completed_origin(origin_stage)?;
        let mut available_data = self
            .compiler
            .dispatch
            .registry
            .data_keys_for_satisfied_providers(&satisfied_providers);
        while index < plan.steps.len() {
            let restart_target;
            let stop_pipeline;
            {
                let step = &mut plan.steps[index];
                let stage_started = Instant::now();
                let cache_key = query_stage_cache_key(QueryStageCacheKeyInput {
                    unit,
                    stage: &step.stage,
                    phase,
                    initial_bindings,
                    versions: QueryStageCacheVersions {
                        provider_registry: self.compiler.dispatch.registry.version(),
                        compiler_registry: self.compiler.registry.version(),
                        host: self.compiler.host.host_version(),
                        bootstrap_capability: self.compiler.bootstrap.capabilities.version(),
                        bootstrap_image: self.compiler.bootstrap.images.version(),
                    },
                })?;
                step.artifact_key = cache_key.clone();
                if let Some(cached_value) = cache_key
                    .as_ref()
                    .and_then(|key| self.compiler.artifact_cache_mut().get(key).cloned())
                {
                    let iteration_offset = plan.executed.len();
                    let mut cached_records = cached_execution_records_for_request(
                        &cached_value,
                        CachedQueryStageReplayRequest {
                            stage: &step.stage,
                            phase,
                            unit_id: unit.unit_id(),
                        },
                    )?;
                    if cached_records.iter().any(|record| record.stopped_by_error) {
                        self.compiler.emit_compiler_event(
                            "query.stage.cache_skip",
                            Some(step.stage.clone()),
                            "ignored cached query stage stopped by error",
                            [
                                ("elapsed_ms".to_string(), elapsed_ms_string(stage_started)),
                                ("phase".to_string(), phase.as_str().to_string()),
                                ("unit".to_string(), unit.unit_id().to_string()),
                            ],
                        )?;
                    } else if let Some(unit_template) =
                        cached_query_stage_unit_template(&cached_value)?
                    {
                        let cached_unit = Unit::from_template(unit_template).map_err(|err| {
                            CaapError::compiler(format!(
                                "cached query stage unit template is invalid: {err}"
                            ))
                        })?;
                        unit.restore_snapshot(cached_unit.snapshot())
                            .map_err(|err| {
                                CaapError::compiler(format!(
                                    "cached query stage unit snapshot is invalid: {err}"
                                ))
                            })?;
                        step.cached = true;
                        for (offset, record) in cached_records.iter_mut().enumerate() {
                            record.iteration = iteration_offset + offset;
                        }
                        extend_available_data(
                            &mut available_data,
                            cached_records
                                .iter()
                                .flat_map(|record| record.provides_data.iter().cloned()),
                        );
                        satisfied_providers.extend(
                            cached_records
                                .iter()
                                .map(|record| record.provider_name.clone()),
                        );
                        step.provider_names = cached_records
                            .iter()
                            .map(|record| record.provider_name.clone())
                            .collect();
                        step.effect_tags = EffectSet::from_string_set(
                            cached_records.iter().flat_map(|record| {
                                record.effect_tags.iter_strs().map(str::to_string)
                            }),
                            "query stage effect tag",
                        )?;
                        plan.executed.extend(cached_records);
                        self.compiler.emit_compiler_event(
                            "query.stage.cache_hit",
                            Some(step.stage.clone()),
                            "reused cached query stage",
                            [
                                ("elapsed_ms".to_string(), elapsed_ms_string(stage_started)),
                                ("phase".to_string(), phase.as_str().to_string()),
                                ("unit".to_string(), unit.unit_id().to_string()),
                            ],
                        )?;
                        index += 1;
                        continue;
                    } else {
                        self.compiler.emit_compiler_event(
                            "query.stage.cache_skip",
                            Some(step.stage.clone()),
                            "ignored cached query stage without unit snapshot",
                            [
                                ("elapsed_ms".to_string(), elapsed_ms_string(stage_started)),
                                ("phase".to_string(), phase.as_str().to_string()),
                                ("unit".to_string(), unit.unit_id().to_string()),
                            ],
                        )?;
                    }
                }
                let provider_groups = self
                    .compiler
                    .dispatch
                    .registry
                    .provider_schedule_for_stage_with_dynamic_requires(
                        step.stage.clone(),
                        available_data.clone(),
                        &satisfied_providers,
                        &self.compiler.dispatch.dynamic_requires,
                    )?
                    .groups;
                step.provider_names = provider_groups
                    .iter()
                    .flatten()
                    .map(|provider| provider.name.clone())
                    .collect();
                step.effect_tags = EffectSet::from_string_set(
                    provider_groups
                        .iter()
                        .flatten()
                        .flat_map(|provider| provider.effect_tags.iter_strs().map(str::to_string)),
                    "query stage effect tag",
                )?;
                let stage_execution_start = plan.executed.len();
                let mut stage_stop_pipeline = false;
                for providers in provider_groups {
                    for provider in providers {
                        if provider.phase_policy != PhasePolicy::Dual
                            && provider.phase_policy != phase
                        {
                            return Err(CaapError::compiler(format!(
                                "query provider {} is not available in phase {}",
                                provider.name,
                                phase.as_str()
                            )));
                        }
                        if let Some(dl) = deadline {
                            if Instant::now() >= dl {
                                return Err(CaapError::compiler(format!(
                                    "query timed out before executing provider {}",
                                    provider.name
                                )));
                            }
                        }
                        let provider_started = Instant::now();
                        self.compiler.emit_compiler_event(
                            "query.provider.start",
                            Some(provider.name.clone()),
                            "started query provider",
                            [
                                ("phase".to_string(), phase.as_str().to_string()),
                                ("stage".to_string(), provider.stage.clone()),
                                ("unit".to_string(), unit.unit_id().to_string()),
                            ],
                        )?;
                        let provider_cache_key = provider_cache_key(
                            self.compiler,
                            unit,
                            &provider,
                            phase,
                            initial_bindings,
                        )?;
                        if let Some(cache_key) = &provider_cache_key {
                            if let Some(cached_entry) =
                                self.compiler.cache.ctfe_cache.get(cache_key).cloned()
                            {
                                let iteration = plan.executed.len();
                                if let Some(snapshot) = cached_entry.snapshot.clone() {
                                    unit.restore_snapshot(snapshot).map_err(|err| {
                                        CaapError::compiler(format!(
                                            "cached provider unit snapshot is invalid: {err}"
                                        ))
                                    })?;
                                }
                                self.compiler
                                    .diag
                                    .diagnostics
                                    .extend(cached_entry.diagnostics.clone());
                                self.compiler.dispatch.dynamic_requires.insert(
                                    provider.name.clone(),
                                    cached_entry.dynamic_requires.clone(),
                                );
                                if cached_entry.restart_requested {
                                    self.compiler.dispatch.pending_restart =
                                        cached_entry.restart_stage.clone();
                                }
                                plan.executed.push(cached_provider_execution_record(
                                    &provider,
                                    iteration,
                                    &cached_entry,
                                ));
                                self.compiler.emit_compiler_event(
                                    "query.provider.cache_hit",
                                    Some(provider.name.clone()),
                                    "reused cached query provider result",
                                    [
                                        (
                                            "elapsed_ms".to_string(),
                                            elapsed_ms_string(provider_started),
                                        ),
                                        ("phase".to_string(), phase.as_str().to_string()),
                                        ("stage".to_string(), provider.stage.clone()),
                                        ("unit".to_string(), unit.unit_id().to_string()),
                                    ],
                                )?;
                                continue;
                            }
                        }
                        let before_version = unit.version();
                        let before_ir = unit.ir().to_template();
                        let before_attributes = unit.attributes().clone();
                        let before_semantics = unit.semantics().snapshot();
                        let before_diagnostics = self.compiler.diag.diagnostics.len();
                        let iteration = plan.executed.len();
                        let rollback_snapshot = capture_provider_rollback_snapshot(unit, &provider);
                        let effect_policy_snapshot = unit.snapshot();
                        let previous_context = self.compiler.dispatch.active_context.clone();
                        self.compiler.dispatch.active_context = Some(QueryProviderContext {
                            provider: provider.name.clone(),
                            stage: provider.stage.clone(),
                            family: provider.family.clone(),
                            phase,
                            unit_id: unit.unit_id().to_string(),
                            effect_tags: provider.effect_tags.clone(),
                            initial_bindings: initial_bindings.to_vec(),
                            registration_index: provider.registration_index,
                            reads_subjects: Vec::new(),
                            writes_subjects: Vec::new(),
                            read_cells: Vec::new(),
                            write_cells: Vec::new(),
                            reads_files: Vec::new(),
                            writes_files: Vec::new(),
                            artifact_dependencies: Vec::new(),
                        });
                        let result = invoke_query_provider_callback(&provider, self.compiler, unit);
                        let provider_context = self.compiler.dispatch.active_context.clone();
                        self.compiler.dispatch.active_context = previous_context;
                        let diagnostics_emitted = self
                            .compiler
                            .diag
                            .diagnostics
                            .len()
                            .saturating_sub(before_diagnostics);
                        let diagnostic_codes = self.compiler.diag.diagnostics[before_diagnostics..]
                            .iter()
                            .filter_map(|diagnostic| diagnostic.code.clone())
                            .collect();
                        let stopped_by_diagnostic = self.compiler.diag.diagnostics
                            [before_diagnostics..]
                            .iter()
                            .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error);
                        let ir_change = provider_ir_change_stats(&before_ir.nodes, unit);
                        let attributes_changed = unit.attributes() != &before_attributes;
                        let after_semantics = unit.semantics().snapshot();
                        let semantic_writes =
                            cache::semantic_write_summary(&before_semantics, &after_semantics);
                        let provider_context = cache::merge_semantic_write_tracking(
                            provider_context,
                            &before_semantics,
                            &after_semantics,
                        );
                        let observed_changed = unit.version() != before_version;
                        if let Err(error) =
                            validate_unit_fact_schema(&self.compiler.fact_schema, unit)
                        {
                            unit.restore_snapshot(effect_policy_snapshot)
                                .map_err(|err| {
                                    CaapError::compiler(format!(
                                        "{error}; fact schema rollback failed: {err}"
                                    ))
                                })?;
                            self.compiler.diag.diagnostics.truncate(before_diagnostics);
                            self.compiler.dispatch.pending_restart = None;
                            self.compiler.emit_compiler_event(
                                "query.provider.fact_schema_violation",
                                Some(provider.name.clone()),
                                "query provider wrote a fact that violates the compiler fact schema",
                                [
                                    ("error".to_string(), error.to_string()),
                                    ("phase".to_string(), phase.as_str().to_string()),
                                    ("stage".to_string(), provider.stage.clone()),
                                    ("unit".to_string(), unit.unit_id().to_string()),
                                ],
                            )?;
                            return Err(error);
                        }
                        let effect_violation = provider.enforce_effect_postconditions.then(|| {
                            provider_effect_policy_violation(
                                &provider,
                                diagnostics_emitted,
                                &ir_change,
                                attributes_changed,
                                &semantic_writes,
                                provider_context.as_ref(),
                            )
                        });
                        if let Some(message) = effect_violation.flatten() {
                            unit.restore_snapshot(effect_policy_snapshot)
                                .map_err(|err| {
                                    CaapError::compiler(format!(
                                        "{message}; effect policy rollback failed: {err}"
                                    ))
                                })?;
                            self.compiler.diag.diagnostics.truncate(before_diagnostics);
                            self.compiler.dispatch.pending_restart = None;
                            self.compiler.emit_compiler_event(
                                "query.provider.effect_violation",
                                Some(provider.name.clone()),
                                "query provider violated declared effect policy",
                                [
                                    ("error".to_string(), message.clone()),
                                    ("phase".to_string(), phase.as_str().to_string()),
                                    ("stage".to_string(), provider.stage.clone()),
                                    ("unit".to_string(), unit.unit_id().to_string()),
                                ],
                            )?;
                            return Err(CaapError::compiler(message));
                        }
                        match result {
                            Ok(outcome) => {
                                let changed = observed_changed || outcome.changed;
                                let rolled_back = if stopped_by_diagnostic {
                                    if let Some(snapshot) = rollback_snapshot {
                                        restore_provider_rollback_snapshot(unit, snapshot)?;
                                        true
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                };
                                let restart_stage = if stopped_by_diagnostic {
                                    self.compiler.dispatch.pending_restart.clone()
                                } else {
                                    provider_restart_stage(
                                        self.compiler,
                                        &provider,
                                        changed && !rolled_back,
                                    )?
                                };
                                if !stopped_by_diagnostic {
                                    self.compiler.dispatch.pending_restart = restart_stage.clone();
                                }
                                let record =
                                    provider_execution_record(ProviderExecutionRecordInput {
                                        provider: &provider,
                                        iteration,
                                        changed: changed && !rolled_back,
                                        diagnostics_emitted,
                                        rolled_back,
                                        stopped_by_error: stopped_by_diagnostic,
                                        outcome_kind: if stopped_by_diagnostic {
                                            "stopped_by_error"
                                        } else {
                                            "ok"
                                        }
                                        .to_string(),
                                        diagnostic_codes,
                                        ir_change,
                                        attributes_changed,
                                        restart_stage: restart_stage.clone(),
                                        context: provider_context.as_ref(),
                                    })?;
                                if !stopped_by_diagnostic {
                                    if let Some(cache_key) = provider_cache_key {
                                        if let Some(cache_entry) = provider_cache_entry(
                                            unit,
                                            &provider,
                                            &record,
                                            self.compiler.diag.diagnostics[before_diagnostics..]
                                                .to_vec(),
                                            self.compiler
                                                .dispatch
                                                .dynamic_requires
                                                .get(&provider.name)
                                                .cloned()
                                                .unwrap_or_default(),
                                        ) {
                                            self.compiler
                                                .cache
                                                .ctfe_cache
                                                .insert(cache_key, cache_entry);
                                        }
                                    }
                                }
                                plan.executed.push(record);
                                if stopped_by_diagnostic {
                                    self.compiler.dispatch.pending_restart = None;
                                    stage_stop_pipeline = true;
                                }
                                self.compiler.emit_compiler_event(
                                    if stopped_by_diagnostic {
                                        "query.provider.stopped_by_error"
                                    } else {
                                        "query.provider.finish"
                                    },
                                    Some(provider.name.clone()),
                                    if stopped_by_diagnostic {
                                        "query provider stopped after error diagnostic"
                                    } else {
                                        "finished query provider"
                                    },
                                    [
                                        (
                                            "elapsed_ms".to_string(),
                                            elapsed_ms_string(provider_started),
                                        ),
                                        ("phase".to_string(), phase.as_str().to_string()),
                                        ("stage".to_string(), provider.stage.clone()),
                                        ("unit".to_string(), unit.unit_id().to_string()),
                                    ],
                                )?;
                            }
                            Err(error) => {
                                let rolled_back = if let Some(snapshot) = rollback_snapshot {
                                    restore_provider_rollback_snapshot(unit, snapshot)?;
                                    true
                                } else {
                                    false
                                };
                                let restart_stage = self.compiler.dispatch.pending_restart.clone();
                                plan.executed.push(provider_execution_record(
                                    ProviderExecutionRecordInput {
                                        provider: &provider,
                                        iteration,
                                        changed: observed_changed && !rolled_back,
                                        diagnostics_emitted,
                                        rolled_back,
                                        stopped_by_error: true,
                                        outcome_kind: "error".to_string(),
                                        diagnostic_codes,
                                        ir_change,
                                        attributes_changed,
                                        restart_stage,
                                        context: provider_context.as_ref(),
                                    },
                                )?);
                                self.compiler.dispatch.pending_restart = None;
                                self.compiler.emit_compiler_event(
                                    "query.provider.error",
                                    Some(provider.name.clone()),
                                    "query provider failed",
                                    [
                                        (
                                            "elapsed_ms".to_string(),
                                            elapsed_ms_string(provider_started),
                                        ),
                                        ("error".to_string(), error.clone()),
                                        ("phase".to_string(), phase.as_str().to_string()),
                                        ("stage".to_string(), provider.stage.clone()),
                                        ("unit".to_string(), unit.unit_id().to_string()),
                                    ],
                                )?;
                                return Err(CaapError::compiler(error));
                            }
                        }
                        if stage_stop_pipeline {
                            break;
                        }
                    }
                    if stage_stop_pipeline {
                        break;
                    }
                }
                restart_target = self.compiler.dispatch.pending_restart.take();
                stop_pipeline = stage_stop_pipeline;
                step.restart_target = restart_target.clone();
                let stage_records = &plan.executed[stage_execution_start..];
                if let Some(cache_key) = cache_key {
                    Rc::make_mut(&mut self.compiler.cache.artifact_cache).store(
                        cache_key,
                        ArtifactValue::QueryStage(Box::new(query_stage_artifact_value(
                            unit,
                            step,
                            phase,
                            stage_records,
                        )?)),
                        collect_record_artifact_dependencies(stage_records),
                    )?;
                }
                let mut stage_metadata = vec![
                    ("cached".to_string(), false.to_string()),
                    ("elapsed_ms".to_string(), elapsed_ms_string(stage_started)),
                    ("phase".to_string(), phase.as_str().to_string()),
                    (
                        "provider_count".to_string(),
                        plan.executed[stage_execution_start..].len().to_string(),
                    ),
                    ("restarted".to_string(), step.restarted.to_string()),
                    ("stop_pipeline".to_string(), stop_pipeline.to_string()),
                    ("unit".to_string(), unit.unit_id().to_string()),
                ];
                if let Some(restart_target) = &restart_target {
                    stage_metadata.push(("restart_target".to_string(), restart_target.clone()));
                }
                self.compiler.emit_compiler_event(
                    "query.stage.finish",
                    Some(step.stage.clone()),
                    "finished query stage",
                    stage_metadata,
                )?;
                if !stop_pipeline {
                    extend_available_data(
                        &mut available_data,
                        plan.executed[stage_execution_start..]
                            .iter()
                            .flat_map(|record| record.provides_data.iter().cloned()),
                    );
                    satisfied_providers.extend(
                        plan.executed[stage_execution_start..]
                            .iter()
                            .map(|record| record.provider_name.clone()),
                    );
                }
            }
            if stop_pipeline {
                break;
            }
            if let Some(restart_target) = restart_target {
                if restarts_remaining == 0 {
                    return Err(CaapError::compiler(format!(
                        "query restart budget exhausted while restarting from {restart_target}"
                    )));
                }
                restarts_remaining -= 1;
                self.compiler.emit_compiler_event(
                    "query.restart",
                    Some(restart_target.clone()),
                    "scheduled query restart",
                    [
                        ("phase".to_string(), phase.as_str().to_string()),
                        ("unit".to_string(), unit.unit_id().to_string()),
                    ],
                )?;
                let original_route = self
                    .compiler
                    .dispatch
                    .registry
                    .plan_from_origin_option(origin_stage, plan.target.clone(), phase)?
                    .steps;
                let Some(restart_index) = original_route
                    .iter()
                    .position(|step| step.stage == restart_target)
                else {
                    return Err(CaapError::compiler(format!(
                        "query restart stage {restart_target} is not in route to {}",
                        plan.target
                    )));
                };
                let mut restart_steps: Vec<QueryPlanStep> =
                    original_route.into_iter().skip(restart_index).collect();
                for restart_step in &mut restart_steps {
                    restart_step.restarted = true;
                }
                plan.steps.splice(index + 1..index + 1, restart_steps);
            }
            index += 1;
        }
        self.compiler
            .register_unit(unit.clone())
            .map_err(CaapError::compiler)?;
        Ok(plan)
    }
}

fn collect_record_artifact_dependencies(
    records: &[QueryProviderExecutionRecord],
) -> Vec<ArtifactKey> {
    let mut values = BTreeSet::new();
    for record in records {
        values.extend(record.artifact_dependencies.iter().cloned());
    }
    values.into_iter().collect()
}

fn collect_record_provider_names(records: &[QueryProviderExecutionRecord]) -> Vec<String> {
    records
        .iter()
        .map(|record| record.provider_name.clone())
        .collect()
}

fn collect_record_effect_tags(records: &[QueryProviderExecutionRecord]) -> CaapResult<EffectSet> {
    EffectSet::from_string_set(
        records
            .iter()
            .flat_map(|record| record.effect_tags.iter_strs().map(str::to_string)),
        "query stage effect tag",
    )
}

fn query_stage_artifact_value(
    unit: &Unit,
    step: &QueryPlanStep,
    phase: PhasePolicy,
    execution_records: &[QueryProviderExecutionRecord],
) -> CaapResult<QueryStageArtifactValue> {
    let provider_names = collect_record_provider_names(execution_records);
    let effect_tags = collect_record_effect_tags(execution_records)?;
    let summary = SemanticValue::map([
        ("stage".to_string(), SemanticValue::Str(step.stage.clone())),
        (
            "unit".to_string(),
            SemanticValue::Str(unit.unit_id().to_string()),
        ),
        (
            "phase".to_string(),
            SemanticValue::Str(phase.as_str().to_string()),
        ),
        (
            "unit_version".to_string(),
            SemanticValue::Int(unit.version() as i64),
        ),
        (
            "cache_written_at_unix_ns".to_string(),
            SemanticValue::Int(current_unix_ns()?),
        ),
        (
            "providers".to_string(),
            SemanticValue::List(
                provider_names
                    .iter()
                    .cloned()
                    .map(SemanticValue::Str)
                    .collect(),
            ),
        ),
        (
            "provider_count".to_string(),
            SemanticValue::Int(provider_names.len() as i64),
        ),
        ("effect_tags".to_string(), semantic_effect_set(&effect_tags)),
        (
            "reads_subjects".to_string(),
            semantic_string_list(&collect_record_strings(execution_records, |record| {
                &record.reads_subjects
            })),
        ),
        (
            "writes_subjects".to_string(),
            semantic_string_list(&collect_record_strings(execution_records, |record| {
                &record.writes_subjects
            })),
        ),
        (
            "read_cells".to_string(),
            semantic_string_list(&collect_record_strings(execution_records, |record| {
                &record.read_cells
            })),
        ),
        (
            "write_cells".to_string(),
            semantic_string_list(&collect_record_strings(execution_records, |record| {
                &record.write_cells
            })),
        ),
        (
            "reads_files".to_string(),
            semantic_string_list(&collect_record_strings(execution_records, |record| {
                &record.reads_files
            })),
        ),
        (
            "writes_files".to_string(),
            semantic_string_list(&collect_record_strings(execution_records, |record| {
                &record.writes_files
            })),
        ),
        ("restarted".to_string(), SemanticValue::Bool(step.restarted)),
        (
            "restart_target".to_string(),
            step.restart_target
                .as_ref()
                .map(|target| SemanticValue::Str(target.clone()))
                .unwrap_or(SemanticValue::Null),
        ),
        (
            "artifact_key".to_string(),
            SemanticValue::Str(
                step.artifact_key
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            ),
        ),
        (
            "execution_summary".to_string(),
            SemanticValue::List(
                execution_records
                    .iter()
                    .map(provider_execution_record_to_semantic_value)
                    .collect::<CaapResult<Vec<_>>>()?,
            ),
        ),
    ])?;
    Ok(QueryStageArtifactValue {
        summary,
        unit_template: unit.to_template(),
    })
}

fn invoke_query_provider_callback(
    provider: &QueryProvider,
    compiler: &mut Compiler,
    unit: &mut Unit,
) -> Result<QueryProviderCallbackOutcome, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut context = NativeProviderContext::new(compiler, unit);
        (provider.callback)(&mut context)
    }))
    .map_err(|panic| {
        format!(
            "query provider '{}' panicked: {}",
            provider.name,
            panic_payload_message(&panic)
        )
    })?
}

fn panic_payload_message(panic: &Box<dyn Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|value| (*value).to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
}
