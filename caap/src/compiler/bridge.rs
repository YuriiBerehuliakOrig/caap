use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use crate::artifacts::{ArtifactCacheStats, ArtifactFingerprint, SourceArtifact};
use crate::bridges::NodeBridgeValue;
use crate::diagnostics::{CompilerEvent, Diagnostic};
use crate::error::{CaapError, CaapResult};
use crate::eval::Evaluator;
use crate::host::HostServiceExport;
use crate::semantic::{CapabilityName, EvalPolicy, PhasePolicy, SemanticEntry, SemanticValue};
use crate::unit::Unit;
use crate::values::{is_truthy, EvalResult, EvalSignal, HostObject, MapKey, RuntimeValue};

use super::bootstrap::{normalize_bootstrap_capabilities, BootstrapTraceEvent, EvaluationCapture};
use super::bridges::{
    ProviderContextBridgeValue, QueryArtifactProjection, QueryArtifactSource,
    QueryExecutionProjection, SemanticPolicyRegistration, UnitBridgeValue,
};
use super::eval_service::{evaluate_unit_capture, evaluate_unit_capture_with_bindings};
use super::fact_schema::{require_registry_name, FactSchemaRegistry};
use super::query_provider::{
    merge_provider_context_tracking, normalize_stage_name, NativeProviderContext,
    QueryExecutionOptions, QueryProviderCallbackOutcome, QueryProviderContractSpec,
    QueryProviderRegistrationSpec, QueryProviderSchedule, QueryStageSpec,
};
use super::query_service::{collect_record_strings, query_step_invalidation};
use super::session::{
    bootstrap_image_file_fingerprint, elapsed_ms_string, live_trace_event_line, path_to_string,
    resolve_source_path, should_emit_live_trace, source_artifact_to_template,
    source_template_stage, validate_base_semantic_entry, validate_dynamic_provider_dependency,
    Compiler,
};

#[derive(Debug)]
pub struct CompilerBridgeValue {
    pub(super) session: RefCell<Compiler>,
}

struct CompilerTransaction<'a> {
    bridge: &'a CompilerBridgeValue,
    working: Compiler,
}

impl<'a> CompilerTransaction<'a> {
    fn compiler(&self) -> &Compiler {
        &self.working
    }

    fn compiler_mut(&mut self) -> &mut Compiler {
        &mut self.working
    }

    fn commit(self) {
        *self.bridge.session.borrow_mut() = self.working;
    }
}

impl CompilerBridgeValue {
    pub fn from_session_state(compiler: Compiler) -> Self {
        Self {
            session: RefCell::new(compiler),
        }
    }

    fn begin_transaction(&self) -> CompilerTransaction<'_> {
        CompilerTransaction {
            bridge: self,
            working: self.session.borrow().clone(),
        }
    }

    pub fn with_current_unit(self, unit_id: impl Into<String>) -> Self {
        self.session
            .borrow_mut()
            .bootstrap
            .unit_stack
            .push(unit_id.into());
        self
    }

    pub fn current_bootstrap_unit_id(&self) -> Option<String> {
        self.session.borrow().bootstrap.unit_stack.last().cloned()
    }

    pub fn has_current_bootstrap_capability(&self, capability: &str) -> bool {
        let Ok(capability_name) = CapabilityName::new(capability) else {
            return false;
        };
        if self
            .current_bootstrap_capabilities()
            .iter()
            .any(|current| current.covers(&capability_name))
        {
            return true;
        }
        self.current_bootstrap_unit_id().is_some_and(|unit_id| {
            self.session
                .borrow()
                .bootstrap
                .capabilities
                .allows(&unit_id, capability)
        })
    }

    pub fn require_current_bootstrap_capability(&self, capability: &str) -> CaapResult<()> {
        if self.has_current_bootstrap_capability(capability) {
            Ok(())
        } else {
            let unit_id = self
                .current_bootstrap_unit_id()
                .unwrap_or_else(|| "<unknown>".to_string());
            Err(CaapError::compiler(format!(
                "bootstrap capability denied for {unit_id}: {capability}"
            )))
        }
    }

    /// Require the current bootstrap unit to hold the capability needed to bind
    /// `library.export` as a host service. The specific capability comes from
    /// the export's contract (`sys.fs.write`, …); pure operations need none.
    /// Missing export contracts are rejected instead of falling back to a broad
    /// authority, keeping host capability semantics explicit.
    pub fn host_service_required_capability(
        &self,
        library: &str,
        export: &str,
        phase: PhasePolicy,
    ) -> CaapResult<Option<String>> {
        let session = self.session.borrow();
        match phase {
            PhasePolicy::CompileTime => session.host.compile_time_services(),
            PhasePolicy::Runtime | PhasePolicy::Dual => session.host.runtime_services(),
        }
        .export_required_capability(library, export)
    }

    pub fn require_host_service_capability(
        &self,
        library: &str,
        export: &str,
        phase: PhasePolicy,
    ) -> CaapResult<()> {
        match self.host_service_required_capability(library, export, phase)? {
            Some(capability) => self.require_current_bootstrap_capability(&capability),
            None => Ok(()),
        }
    }

    pub fn register_value(
        &self,
        name: impl Into<String>,
        value: RuntimeValue,
    ) -> CaapResult<RuntimeValue> {
        let name = name.into();
        let registered = self
            .session
            .borrow_mut()
            .register_value(name.clone(), value)?;
        Ok(registered)
    }

    pub fn lookup_registered_value(&self, name: &str) -> CaapResult<Option<RuntimeValue>> {
        Ok(self.session.borrow().registry.lookup_value(name)?.cloned())
    }

    pub fn lookup_compiled_unit(&self, unit_id: &str) -> CaapResult<Option<UnitBridgeValue>> {
        if unit_id.is_empty() {
            return Err(CaapError::compiler(
                "compiler unit id lookup must be non-empty",
            ));
        }
        Ok(self
            .session
            .borrow()
            .units
            .get(unit_id)
            .cloned()
            .map(UnitBridgeValue::from_unit_snapshot))
    }

    pub fn emit_event(
        &self,
        component: impl Into<String>,
        action: impl Into<String>,
        message: impl Into<String>,
        fields: impl IntoIterator<Item = (String, String)>,
    ) -> CaapResult<()> {
        let component = require_registry_name(component.into())?;
        let action = require_registry_name(action.into())?;
        self.push_event(format!("{component}.{action}"), None, message, fields)
    }

    pub fn register_stage(
        &self,
        name: impl Into<String>,
        requires: impl IntoIterator<Item = String>,
        family_label: Option<String>,
        aliases: impl IntoIterator<Item = String>,
        restart_stage: Option<String>,
        input_kinds: impl IntoIterator<Item = String>,
    ) -> CaapResult<()> {
        let mut spec = QueryStageSpec::new(name.into())?
            .with_requires(requires)?
            .with_aliases(aliases)?
            .with_input_kinds(input_kinds)?;
        if let Some(family_label) = family_label {
            spec = spec.with_family_label(family_label)?;
        }
        if let Some(restart_stage) = restart_stage {
            spec = spec.with_restart_stage(restart_stage)?;
        }
        let mut session = self.session.borrow_mut();
        Rc::make_mut(&mut session.dispatch.registry).register_stage(spec)?;
        session.advance_session_version()?;
        Ok(())
    }

    pub fn host_service_export(
        &self,
        library: &str,
        export: &str,
        phase: PhasePolicy,
    ) -> CaapResult<RuntimeValue> {
        let session = self.session.borrow();
        match phase {
            PhasePolicy::CompileTime => session.host.compile_time_services(),
            PhasePolicy::Runtime | PhasePolicy::Dual => session.host.runtime_services(),
        }
        .export(library, export, phase)
    }

    pub fn host_service_libraries(&self, phase: PhasePolicy) -> Vec<String> {
        let session = self.session.borrow();
        match phase {
            PhasePolicy::CompileTime => session.host.compile_time_services(),
            PhasePolicy::Runtime | PhasePolicy::Dual => session.host.runtime_services(),
        }
        .library_names()
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    pub fn host_service_library_catalog(
        &self,
        library: &str,
        phase: PhasePolicy,
    ) -> CaapResult<Vec<HostServiceExport>> {
        let session = self.session.borrow();
        let services = match phase {
            PhasePolicy::CompileTime => session.host.compile_time_services(),
            PhasePolicy::Runtime | PhasePolicy::Dual => session.host.runtime_services(),
        };
        let library_entry = services.library(library)?.ok_or_else(|| {
            CaapError::compiler(format!("host service library does not exist: {library}"))
        })?;
        let mut entries = Vec::new();
        for export_name in library_entry.export_names() {
            services.export(library, export_name, phase)?;
            let export = library_entry
                .export(export_name)?
                .ok_or_else(|| {
                    CaapError::compiler(format!(
                        "host service export listed by library is missing: {library}.{export_name}"
                    ))
                })?
                .clone();
            entries.push(export);
        }
        Ok(entries)
    }

    pub fn register_stage_alias(
        &self,
        stage: impl Into<String>,
        alias: impl Into<String>,
    ) -> CaapResult<()> {
        let mut session = self.session.borrow_mut();
        Rc::make_mut(&mut session.dispatch.registry).register_alias(stage, alias)?;
        session.advance_session_version()?;
        Ok(())
    }

    pub fn register_stage_restart_policy(
        &self,
        stage: impl Into<String>,
        restart_stage: impl Into<String>,
    ) -> CaapResult<()> {
        let mut session = self.session.borrow_mut();
        Rc::make_mut(&mut session.dispatch.registry)
            .register_restart_stage(stage, restart_stage)?;
        session.advance_session_version()?;
        Ok(())
    }

    pub fn register_semantic_policy(
        &self,
        mut policy: SemanticPolicyRegistration,
    ) -> CaapResult<()> {
        if policy.name.is_empty() {
            return Err(CaapError::compiler(
                "registered semantic policy name must be non-empty",
            ));
        }
        if policy.phase_policy != PhasePolicy::CompileTime {
            return Err(CaapError::compiler(format!(
                "registered semantic policy {:?} must use compile_time phase policy",
                policy.name
            )));
        }
        if policy.eval_policy != EvalPolicy::SpecialForm {
            return Err(CaapError::compiler(format!(
                "registered semantic policy {:?} must use special_form eval policy",
                policy.name
            )));
        }
        if matches!(policy.normalizer, RuntimeValue::Null) {
            return Err(CaapError::compiler(format!(
                "registered semantic policy {:?} requires a normalizer",
                policy.name
            )));
        }
        policy.unit_id.get_or_insert_with(|| "<ctfe>".to_string());
        let name = policy.name.clone();
        let mut session = self.session.borrow_mut();
        Rc::make_mut(&mut session.semantic_policies).insert(name.clone(), policy);
        session.advance_session_version()?;
        drop(session);
        self.push_event(
            "compiler.semantic_policy.register",
            Some(name),
            "registered compiler semantic policy",
            [],
        )?;
        Ok(())
    }

    pub fn list_semantic_policies(&self) -> Vec<SemanticPolicyRegistration> {
        let mut policies: Vec<_> = self
            .session
            .borrow()
            .semantic_policies
            .values()
            .cloned()
            .collect();
        policies.sort_by(|left, right| left.name.cmp(&right.name));
        policies
    }

    pub fn register_fact_schema_type_bridge(
        &self,
        label: impl Into<String>,
        bridge_name: impl Into<String>,
    ) -> CaapResult<()> {
        let label = label.into();
        let bridge_name = bridge_name.into();
        let mut session = self.session.borrow_mut();
        Rc::make_mut(&mut session.fact_schema)
            .register_type_bridge(label.clone(), bridge_name.clone())?;
        session
            .registry
            .register_value(
                format!("caap.fact_schema.type_bridge.{label}"),
                RuntimeValue::Str(bridge_name.into()),
            )
            .map_err(CaapError::compiler)?;
        session.advance_session_version()?;
        drop(session);
        self.push_event(
            "compiler.fact_schema.type_bridge.register",
            Some(label),
            "registered compiler fact schema type bridge",
            [],
        )?;
        Ok(())
    }

    pub fn register_fact_schema(
        &self,
        predicate: impl Into<String>,
        type_label: impl Into<String>,
        allow_none: bool,
        description: Option<String>,
    ) -> CaapResult<()> {
        let predicate = predicate.into();
        let type_label = type_label.into();
        let mut session = self.session.borrow_mut();
        Rc::make_mut(&mut session.fact_schema).register_schema(
            predicate.clone(),
            type_label.clone(),
            allow_none,
            description,
        )?;
        session.advance_session_version()?;
        drop(session);
        self.push_event(
            "compiler.fact_schema.register",
            Some(predicate),
            "registered compiler fact schema",
            [("type".to_string(), type_label)],
        )?;
        Ok(())
    }

    pub fn validate_fact_value(&self, predicate: &str, value: &SemanticValue) -> CaapResult<()> {
        self.session
            .borrow()
            .fact_schema
            .validate_value(predicate, value)
    }

    pub fn fact_schema(&self) -> FactSchemaRegistry {
        (*self.session.borrow().fact_schema).clone()
    }

    pub fn register_base_semantic_entries(
        &self,
        entries: impl IntoIterator<Item = SemanticEntry>,
    ) -> CaapResult<()> {
        let mut changed = Vec::new();
        {
            let mut session = self.session.borrow_mut();
            for entry in entries {
                validate_base_semantic_entry(&entry)?;
                let name = entry.name.clone();
                if session.base_semantic_entries.get(&name) != Some(&entry) {
                    Rc::make_mut(&mut session.base_semantic_entries).insert(name.clone(), entry);
                    changed.push(name);
                }
            }
            if !changed.is_empty() {
                session.advance_session_version()?;
            }
        }
        for name in &changed {
            self.push_event(
                "compiler.base_semantic_entry.register",
                Some(name.clone()),
                "registered compiler base semantic entry",
                [],
            )?;
        }
        Ok(())
    }

    pub fn base_semantic_entries(&self) -> Vec<SemanticEntry> {
        let mut entries: Vec<_> = self
            .session
            .borrow()
            .base_semantic_entries
            .values()
            .cloned()
            .collect();
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        entries
    }

    pub fn register_provider(
        &self,
        name: impl Into<String>,
        target: impl Into<String>,
        callback: RuntimeValue,
        requires: impl IntoIterator<Item = String>,
        effects: impl IntoIterator<Item = String>,
        spec: QueryProviderRegistrationSpec,
    ) -> CaapResult<()> {
        let name = name.into();
        let target = target.into();
        let stage = self
            .session
            .borrow()
            .dispatch
            .registry
            .resolve_stage(target)?;
        let family = match spec.family.clone() {
            Some(family) => Some(normalize_stage_name(family)?),
            None => self
                .session
                .borrow()
                .dispatch
                .registry
                .stage_spec(&stage)?
                .and_then(|stage| stage.family_label.clone()),
        };
        let provider_callback = callback.clone();
        validate_registered_provider_callback_abi(&name, &provider_callback)?;
        let mut session = self.session.borrow_mut();
        Rc::make_mut(&mut session.dispatch.registry)
            .register_provider_contract_with_outcome_and_effect_postconditions(
                QueryProviderContractSpec {
                    name,
                    stage,
                    family,
                    phase_policy: PhasePolicy::CompileTime,
                    requires: requires.into_iter().collect(),
                    effect_tags: effects.into_iter().collect(),
                    registration: spec,
                },
                move |context| {
                    invoke_registered_provider_callback(provider_callback.clone(), context)
                },
                false,
            )?;
        session.advance_session_version()?;
        Ok(())
    }

    pub fn list_stages(&self) -> Vec<QueryStageSpec> {
        let mut stages: Vec<_> = self
            .session
            .borrow()
            .dispatch
            .registry
            .stages
            .values()
            .cloned()
            .collect();
        stages.sort_by(|left, right| left.name.cmp(&right.name));
        stages
    }

    pub fn list_providers(&self) -> Vec<super::query_provider::QueryProvider> {
        self.session.borrow().dispatch.registry.ordered_providers()
    }

    pub fn list_providers_for_stage(
        &self,
        stage_or_target: impl Into<String>,
    ) -> CaapResult<Vec<super::query_provider::QueryProvider>> {
        let session = self.session.borrow();
        let registry = &session.dispatch.registry;
        registry.providers_for_stage(stage_or_target.into())
    }

    pub fn provider_schedule_for_stage(
        &self,
        stage_or_target: impl Into<String>,
    ) -> CaapResult<QueryProviderSchedule> {
        self.provider_schedule_for_stage_with_satisfied(stage_or_target, &BTreeSet::new())
    }

    pub fn provider_schedule_for_stage_with_satisfied(
        &self,
        stage_or_target: impl Into<String>,
        previously_satisfied: &BTreeSet<String>,
    ) -> CaapResult<QueryProviderSchedule> {
        let session = self.session.borrow();
        let registry = &session.dispatch.registry;
        let available_data = registry.data_keys_for_satisfied_providers(previously_satisfied);
        registry.provider_schedule_for_stage_with_dynamic_requires(
            stage_or_target,
            available_data,
            previously_satisfied,
            &session.dispatch.dynamic_requires,
        )
    }

    pub fn provider_dynamic_requires_for(&self, provider_name: &str) -> Vec<String> {
        self.session
            .borrow()
            .dispatch
            .dynamic_requires
            .get(provider_name)
            .cloned()
            .unwrap_or_default()
    }

    fn note_nested_query_artifact_dependency(
        &self,
        artifact: &QueryArtifactProjection,
    ) -> CaapResult<()> {
        use super::query_provider::{extend_unique, extend_unique_artifact_keys};
        let Some(provider_name) = artifact
            .execution_summary
            .last()
            .map(|record| record.provider_name.as_str())
        else {
            return Ok(());
        };
        self.note_dynamic_provider_dependency(provider_name)?;
        if let Some(context) = self.session.borrow_mut().dispatch.active_context.as_mut() {
            extend_unique(
                &mut context.reads_subjects,
                artifact.reads_subjects.iter().cloned(),
            );
            extend_unique(&mut context.read_cells, artifact.read_cells.iter().cloned());
            extend_unique(
                &mut context.reads_files,
                artifact.reads_files.iter().cloned(),
            );
            extend_unique_artifact_keys(
                &mut context.artifact_dependencies,
                std::iter::once(artifact.key.clone()).chain(artifact.dependencies.iter().cloned()),
            );
        }
        Ok(())
    }

    fn note_dynamic_provider_dependency(&self, provider_name: &str) -> CaapResult<()> {
        let Some(active) = self
            .session
            .borrow()
            .dispatch
            .active_context
            .as_ref()
            .cloned()
        else {
            return Ok(());
        };
        if provider_name.is_empty() {
            return Ok(());
        }
        validate_dynamic_provider_dependency(
            &self.session.borrow().dispatch.dynamic_requires,
            &active.provider,
            provider_name,
        )?;
        let mut session = self.session.borrow_mut();
        let entry = session
            .dispatch
            .dynamic_requires
            .entry(active.provider)
            .or_default();
        if !entry.iter().any(|name| name == provider_name) {
            entry.push(provider_name.to_string());
            entry.sort();
            session.advance_session_version()?;
        }
        Ok(())
    }

    pub fn cache_stats(&self) -> ArtifactCacheStats {
        self.session.borrow().cache.artifact_cache.stats().clone()
    }

    pub fn bootstrap_trace(&self) -> Vec<BootstrapTraceEvent> {
        self.session.borrow().bootstrap.trace.borrow().clone()
    }

    pub fn current_bootstrap_context(&self) -> (Option<String>, Vec<String>) {
        (
            self.session.borrow().bootstrap.path_stack.last().cloned(),
            self.current_bootstrap_capabilities()
                .into_iter()
                .map(CapabilityName::into_string)
                .collect(),
        )
    }

    fn current_bootstrap_capabilities(&self) -> Vec<CapabilityName> {
        if let Some(capabilities) = self
            .session
            .borrow()
            .bootstrap
            .capability_stack
            .last()
            .cloned()
        {
            return capabilities;
        }
        self.current_bootstrap_unit_id()
            .map(|unit_id| {
                self.session
                    .borrow()
                    .bootstrap
                    .capabilities
                    .capabilities_for(&unit_id)
            })
            .unwrap_or_default()
    }

    fn query_source_unit_and_origin_stage(
        &self,
        source: QueryArtifactSource,
    ) -> CaapResult<(Unit, Option<String>)> {
        match source {
            QueryArtifactSource::Unit(unit) => {
                let origin_stage = self.optional_query_source_origin_stage("unit");
                Ok((*unit, origin_stage))
            }
            QueryArtifactSource::Path(path) => {
                let unit = self.load_surface_unit_template(path)?.clone_unit_snapshot();
                let origin_stage = self.required_query_source_origin_stage("surface")?;
                Ok((unit, origin_stage))
            }
            QueryArtifactSource::Text(text) => {
                let unit = self
                    .load_surface_text_unit_template(text)?
                    .clone_unit_snapshot();
                let origin_stage = self.required_query_source_origin_stage("surface")?;
                Ok((unit, origin_stage))
            }
        }
    }

    fn optional_query_source_origin_stage(&self, input_kind: &str) -> Option<String> {
        self.session
            .borrow()
            .dispatch
            .registry
            .stage_for_input_kind(input_kind)
            .ok()
    }

    fn required_query_source_origin_stage(&self, input_kind: &str) -> CaapResult<Option<String>> {
        let session = self.session.borrow();
        let registry = &session.dispatch.registry;
        if !registry.has_stages() {
            return Ok(None);
        }
        registry.stage_for_input_kind(input_kind).map(Some).map_err(|error| {
            CaapError::compiler(format!(
                "query source input kind {input_kind:?} must be registered for source-backed queries: {error}"
            ))
        })
    }

    pub fn query_execution_projection_with_options(
        &self,
        target: impl Into<String>,
        source: QueryArtifactSource,
        phase: PhasePolicy,
        options: QueryExecutionOptions,
    ) -> CaapResult<QueryExecutionProjection> {
        let target = target.into();
        let (mut unit, origin_stage) = self.query_source_unit_and_origin_stage(source)?;
        let mut transaction = self.begin_transaction();
        let before_diagnostics = transaction.compiler().diag.diagnostics.len();
        let plan = match origin_stage.as_deref() {
            Some(origin_stage) => transaction
                .compiler_mut()
                .queries()
                .query_from_stage_to_target_with_options(
                    origin_stage,
                    &target,
                    &mut unit,
                    phase,
                    options,
                )?,
            None => transaction
                .compiler_mut()
                .queries()
                .query_with_options(&target, &mut unit, phase, options)?,
        };
        let execution_diagnostics =
            transaction.compiler().diag.diagnostics[before_diagnostics..].to_vec();
        let execution_summary = plan.executed.clone();
        let artifact = match plan.steps.last().and_then(|step| {
            step.artifact_key
                .clone()
                .map(|key| (step.stage.clone(), key))
        }) {
            Some((stage, key)) => {
                let value = transaction
                    .compiler()
                    .cache
                    .artifact_cache
                    .peek(&key)
                    .cloned()
                    .ok_or_else(|| {
                        CaapError::compiler(format!(
                            "query target {target:?} did not store artifact {key}"
                        ))
                    })?;
                let origin_key = transaction
                    .compiler()
                    .cache
                    .artifact_cache
                    .lineage_id_for_key(&key)
                    .cloned();
                let dependencies = transaction
                    .compiler()
                    .cache
                    .artifact_cache
                    .dependencies_for(&key)
                    .map(|dependencies| dependencies.to_vec())
                    .unwrap_or_default();
                Some(QueryArtifactProjection {
                    artifact_kind: "query".to_string(),
                    stage: stage.clone(),
                    family: stage,
                    phase,
                    key,
                    origin_key,
                    dependencies,
                    diagnostics: transaction.compiler().diag.diagnostics.clone(),
                    execution_diagnostics: execution_diagnostics.clone(),
                    iterations: execution_summary.len(),
                    reads_subjects: collect_record_strings(&execution_summary, |record| {
                        &record.reads_subjects
                    }),
                    writes_subjects: collect_record_strings(&execution_summary, |record| {
                        &record.writes_subjects
                    }),
                    read_cells: collect_record_strings(&execution_summary, |record| {
                        &record.read_cells
                    }),
                    write_cells: collect_record_strings(&execution_summary, |record| {
                        &record.write_cells
                    }),
                    reads_files: collect_record_strings(&execution_summary, |record| {
                        &record.reads_files
                    }),
                    writes_files: collect_record_strings(&execution_summary, |record| {
                        &record.writes_files
                    }),
                    execution_summary,
                    value,
                })
            }
            None => None,
        };
        let invalidations = plan
            .steps
            .iter()
            .map(|step| query_step_invalidation(&transaction.compiler().cache.artifact_cache, step))
            .collect();
        transaction.commit();
        if let Some(artifact) = &artifact {
            self.note_nested_query_artifact_dependency(artifact)?;
        }
        Ok(QueryExecutionProjection {
            plan,
            artifact,
            execution_diagnostics,
            invalidations,
            unit,
        })
    }

    pub fn load_surface_unit_template(
        &self,
        path: impl AsRef<Path>,
    ) -> CaapResult<UnitBridgeValue> {
        self.load_surface_unit_template_with_unit_id(path, None::<String>)
    }

    pub fn load_surface_unit_template_with_unit_id(
        &self,
        path: impl AsRef<Path>,
        unit_id: Option<impl Into<String>>,
    ) -> CaapResult<UnitBridgeValue> {
        let started = Instant::now();
        let resolved = resolve_source_path(path.as_ref())?;
        let text = std::fs::read_to_string(&resolved)
            .map_err(|error| CaapError::compiler(format!("surface file read failed: {error}")))?;
        let source_path = path_to_string(&resolved)?;
        let unit_id = match unit_id {
            Some(unit_id) => {
                let unit_id = unit_id.into();
                if unit_id.is_empty() {
                    return Err(CaapError::compiler(
                        "surface file unit id must be non-empty",
                    ));
                }
                unit_id
            }
            None => source_path.clone(),
        };
        let token = source_path_token(&text);
        let source = SourceArtifact::path(source_path.clone(), token, text)?;
        let template_cache = self.session.borrow().host.source_template_cache.clone();
        let stage = source_template_stage(&unit_id);
        let artifact = {
            let mut session = self.session.borrow_mut();
            Rc::make_mut(&mut session.cache.source_templates).load(
                source,
                stage,
                PhasePolicy::CompileTime,
                |source| source_artifact_to_template(&template_cache, source, &unit_id),
            )?
        };
        self.push_event(
            "source.template.load",
            Some(unit_id.clone()),
            "loaded source template",
            [
                ("cache_hit".to_string(), artifact.cache_hit.to_string()),
                ("elapsed_ms".to_string(), elapsed_ms_string(started)),
                ("key".to_string(), artifact.key.to_string()),
                ("lineage".to_string(), artifact.lineage_id.to_string()),
                ("origin".to_string(), "path".to_string()),
            ],
        )?;
        self.session.borrow_mut().advance_session_version()?;
        Ok(UnitBridgeValue::from_unit_snapshot(Unit::from_template(
            artifact.template,
        )?))
    }

    pub fn load_surface_text_unit_template(
        &self,
        text: impl Into<String>,
    ) -> CaapResult<UnitBridgeValue> {
        let started = Instant::now();
        let unit_id = "<inline.caap>".to_string();
        let source = SourceArtifact::inline(text)?;
        let template_cache = self.session.borrow().host.source_template_cache.clone();
        let artifact = {
            let mut session = self.session.borrow_mut();
            Rc::make_mut(&mut session.cache.source_templates).load(
                source,
                "parse",
                PhasePolicy::CompileTime,
                |source| source_artifact_to_template(&template_cache, source, &unit_id),
            )?
        };
        self.push_event(
            "source.template.load",
            Some(unit_id.clone()),
            "loaded source template",
            [
                ("cache_hit".to_string(), artifact.cache_hit.to_string()),
                ("elapsed_ms".to_string(), elapsed_ms_string(started)),
                ("key".to_string(), artifact.key.to_string()),
                ("lineage".to_string(), artifact.lineage_id.to_string()),
                ("origin".to_string(), "inline".to_string()),
            ],
        )?;
        self.session.borrow_mut().advance_session_version()?;
        Ok(UnitBridgeValue::from_unit_snapshot(Unit::from_template(
            artifact.template,
        )?))
    }

    pub fn execute_bootstrap_file_with_capabilities<T>(
        &self,
        path: impl AsRef<Path>,
        internal_capabilities: impl IntoIterator<Item = T>,
        compiler_value: RuntimeValue,
    ) -> CaapResult<RuntimeValue>
    where
        T: Into<String>,
    {
        let resolved = self.resolve_bootstrap_source_path(path.as_ref())?;
        let target = path_to_string(&resolved)?;
        let capabilities = normalize_bootstrap_capabilities(internal_capabilities)?;
        let depth = self.session.borrow().bootstrap.active_depth;
        let action = if depth == 0 {
            "bootstrap.raw"
        } else {
            "bootstrap.nested_raw"
        };
        let fingerprint = bootstrap_image_file_fingerprint(&resolved)?;
        let memo_key = super::bootstrap::bootstrap_execution_memo_key(
            action,
            &target,
            &fingerprint,
            &capabilities,
        );
        if self
            .session
            .borrow()
            .bootstrap
            .execution_memo
            .borrow()
            .contains(&memo_key)
        {
            if depth == 0 {
                self.session
                    .borrow()
                    .bootstrap
                    .trace
                    .borrow_mut()
                    .push(BootstrapTraceEvent {
                        action: "bootstrap.session_memo".to_string(),
                        target,
                        depth,
                        succeeded: true,
                    });
            }
            return Ok(RuntimeValue::Null);
        }
        {
            let mut session = self.session.borrow_mut();
            let entered_depth = session.bootstrap.enter_execution()?;
            debug_assert_eq!(entered_depth, depth);
            session.bootstrap.path_stack.push(target.clone());
            session.bootstrap.capability_stack.push(capabilities);
        }
        let started = Instant::now();
        let result = self.execute_bootstrap_file_inner(&resolved, &target, compiler_value);
        let elapsed_ms = elapsed_ms_string(started);
        {
            let mut session = self.session.borrow_mut();
            session.bootstrap.capability_stack.pop();
            session.bootstrap.path_stack.pop();
            session.bootstrap.leave_execution()?;
            if result.is_ok() {
                session
                    .bootstrap
                    .execution_memo
                    .borrow_mut()
                    .insert(memo_key);
            }
            session
                .bootstrap
                .trace
                .borrow_mut()
                .push(BootstrapTraceEvent {
                    action: action.to_string(),
                    target: target.clone(),
                    depth,
                    succeeded: result.is_ok(),
                });
            session.advance_session_version()?;
        }
        self.push_event(
            "bootstrap.execute",
            Some(target),
            "executed bootstrap source",
            [
                ("action".to_string(), action.to_string()),
                ("depth".to_string(), depth.to_string()),
                ("elapsed_ms".to_string(), elapsed_ms),
                ("succeeded".to_string(), result.is_ok().to_string()),
            ],
        )?;
        result
    }

    pub fn evaluate_bootstrap_file<T>(
        &self,
        path: impl AsRef<Path>,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
        internal_capabilities: impl IntoIterator<Item = T>,
        skip_leading_forms: usize,
        compiler_value: RuntimeValue,
    ) -> CaapResult<EvaluationCapture>
    where
        T: Into<String>,
    {
        let capabilities = normalize_bootstrap_capabilities(internal_capabilities)?;
        let resolved = self.resolve_bootstrap_source_path(path.as_ref())?;
        let target = path_to_string(&resolved)?;
        let depth;
        {
            let mut session = self.session.borrow_mut();
            depth = session.bootstrap.enter_execution()?;
            session.bootstrap.path_stack.push(target.clone());
            session.bootstrap.capability_stack.push(capabilities);
        }

        let started = Instant::now();
        let result = self.evaluate_bootstrap_file_inner(
            &resolved,
            initial,
            skip_leading_forms,
            compiler_value,
        );
        let elapsed_ms = elapsed_ms_string(started);

        {
            let mut session = self.session.borrow_mut();
            session.bootstrap.capability_stack.pop();
            session.bootstrap.path_stack.pop();
            session.bootstrap.leave_execution()?;
            session
                .bootstrap
                .trace
                .borrow_mut()
                .push(BootstrapTraceEvent {
                    action: "bootstrap.evaluate".to_string(),
                    target: target.clone(),
                    depth,
                    succeeded: result.is_ok(),
                });
            session.advance_session_version()?;
        }
        self.push_event(
            "bootstrap.evaluate",
            Some(target),
            "evaluated bootstrap source",
            [
                ("depth".to_string(), depth.to_string()),
                ("elapsed_ms".to_string(), elapsed_ms),
                ("succeeded".to_string(), result.is_ok().to_string()),
            ],
        )?;
        result
    }

    fn evaluate_bootstrap_file_inner(
        &self,
        resolved: &Path,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
        skip_leading_forms: usize,
        compiler_value: RuntimeValue,
    ) -> CaapResult<EvaluationCapture> {
        let unit = self
            .load_surface_unit_template(resolved)?
            .clone_unit_snapshot();
        let unit_id = unit.unit_id().to_string();
        self.session.borrow_mut().bootstrap.unit_stack.push(unit_id);
        let result = {
            let mut bindings: Vec<(String, RuntimeValue)> = initial.into_iter().collect();
            bindings.push(("compiler".to_string(), compiler_value));
            let effective_skip = skip_leading_forms;
            match evaluate_unit_capture_with_bindings(
                &unit,
                PhasePolicy::CompileTime,
                bindings,
                effective_skip,
            ) {
                Ok((value, captured_bindings)) => Ok(EvaluationCapture {
                    unit_id: unit.unit_id().to_string(),
                    phase: PhasePolicy::CompileTime,
                    value: Some(value),
                    bindings: captured_bindings,
                    diagnostics: Vec::new(),
                    skipped_forms: effective_skip,
                }),
                Err(EvalSignal::Error(error)) => {
                    let diagnostic = Diagnostic::from_evaluation_error(&error);
                    self.session
                        .borrow_mut()
                        .diag
                        .diagnostics
                        .push(diagnostic.clone());
                    Ok(EvaluationCapture {
                        unit_id: unit.unit_id().to_string(),
                        phase: PhasePolicy::CompileTime,
                        value: None,
                        bindings: Vec::new(),
                        diagnostics: vec![diagnostic],
                        skipped_forms: effective_skip,
                    })
                }
                Err(signal) => Err(signal.into()),
            }
        };
        self.session.borrow_mut().bootstrap.unit_stack.pop();
        result
    }

    pub(crate) fn resolve_bootstrap_source_path(&self, path: &Path) -> CaapResult<PathBuf> {
        resolve_source_path(&self.bootstrap_relative_source_path(path)?)
    }

    pub(crate) fn bootstrap_source_path_is_file(&self, path: &Path) -> CaapResult<bool> {
        let candidate = self.bootstrap_relative_source_path(path)?;
        match std::fs::metadata(&candidate) {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(CaapError::compiler(format!(
                "source path metadata failed for {}: {error}",
                candidate.display()
            ))),
        }
    }

    fn bootstrap_relative_source_path(&self, path: &Path) -> CaapResult<PathBuf> {
        if path.as_os_str().is_empty() {
            return Err(CaapError::compiler("source path must be non-empty"));
        }
        if path.is_absolute() {
            return Ok(path.to_path_buf());
        }
        if let Some(current) = self.session.borrow().bootstrap.path_stack.last() {
            if let Some(parent) = Path::new(current).parent() {
                return Ok(parent.join(path));
            }
        }
        Ok(path.to_path_buf())
    }

    fn execute_bootstrap_file_inner(
        &self,
        resolved: &Path,
        target: &str,
        compiler_value: RuntimeValue,
    ) -> CaapResult<RuntimeValue> {
        self.session
            .borrow()
            .bootstrap
            .executions
            .borrow_mut()
            .push(target.to_string());
        let unit = self
            .load_surface_unit_template(resolved)?
            .clone_unit_snapshot();
        let unit_id = unit.unit_id().to_string();
        {
            let mut session = self.session.borrow_mut();
            session.register_unit(unit.clone())?;
        }
        self.session.borrow_mut().bootstrap.unit_stack.push(unit_id);
        let result = evaluate_unit_capture(
            &unit,
            PhasePolicy::CompileTime,
            [("compiler".to_string(), compiler_value)],
            0,
        );
        self.session.borrow_mut().bootstrap.unit_stack.pop();
        match result {
            Ok(value) => Ok(value),
            Err(EvalSignal::Error(error)) => {
                self.session
                    .borrow_mut()
                    .push_diagnostic(Diagnostic::from_evaluation_error(&error))?;
                Err(error.into())
            }
            Err(signal) => Err(signal.into()),
        }
    }

    pub fn register_compiled_unit(
        &self,
        unit_id: impl Into<String>,
        unit: &Unit,
    ) -> CaapResult<()> {
        let unit_id = require_registry_name(unit_id.into())?;
        let mut unit = unit.clone();
        unit.set_unit_id(unit_id.clone())?;
        {
            let mut session = self.session.borrow_mut();
            session.register_unit(unit)?;
        }
        Ok(())
    }

    pub fn evaluate_capture(
        &self,
        unit: &Unit,
        phase: PhasePolicy,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
        compiler_value: RuntimeValue,
    ) -> CaapResult<EvaluationCapture> {
        self.evaluate_capture_skipping(unit, phase, initial, compiler_value, 0)
    }

    pub fn evaluate_capture_skipping(
        &self,
        unit: &Unit,
        phase: PhasePolicy,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
        compiler_value: RuntimeValue,
        skip_leading_forms: usize,
    ) -> CaapResult<EvaluationCapture> {
        let mut bindings: Vec<(String, RuntimeValue)> = initial.into_iter().collect();
        bindings.push(("compiler".to_string(), compiler_value));
        match evaluate_unit_capture_with_bindings(unit, phase, bindings, skip_leading_forms) {
            Ok((value, captured_bindings)) => Ok(EvaluationCapture {
                unit_id: unit.unit_id().to_string(),
                phase,
                value: Some(value),
                bindings: captured_bindings,
                diagnostics: Vec::new(),
                skipped_forms: skip_leading_forms,
            }),
            Err(EvalSignal::Error(error)) => {
                let diagnostic = Diagnostic::from_evaluation_error(&error);
                let mut session = self.session.borrow_mut();
                session.push_diagnostic(diagnostic.clone())?;
                Ok(EvaluationCapture {
                    unit_id: unit.unit_id().to_string(),
                    phase,
                    value: None,
                    bindings: Vec::new(),
                    diagnostics: vec![diagnostic],
                    skipped_forms: skip_leading_forms,
                })
            }
            Err(signal) => Err(signal.into()),
        }
    }

    pub fn push_diagnostic(&self, diagnostic: Diagnostic) -> CaapResult<()> {
        self.session.borrow_mut().push_diagnostic(diagnostic)
    }

    fn push_event(
        &self,
        kind: impl Into<String>,
        target: Option<String>,
        message: impl Into<String>,
        metadata: impl IntoIterator<Item = (String, String)>,
    ) -> CaapResult<()> {
        let event = CompilerEvent::with_target(kind, target, message, metadata)
            .map_err(CaapError::compiler)?;
        if should_emit_live_trace(&event) {
            eprintln!("{}", live_trace_event_line(&event));
        }
        self.session.borrow_mut().emit_event(event)
    }

    pub(super) fn commit_session_into(&self, compiler: &mut Compiler) {
        *compiler = self.clone_session_state();
    }

    pub(super) fn clone_session_state(&self) -> Compiler {
        self.session.borrow().clone()
    }
}

impl HostObject for CompilerBridgeValue {
    fn type_name(&self) -> &'static str {
        "compiler"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    /// Expose the session's registered values (what `ctfe-compiler-register-value`
    /// populates) so the `compiler` object can be expanded in a debugger.
    fn debug_children(&self) -> Vec<(String, RuntimeValue)> {
        let session = self.session.borrow();
        session
            .registry
            .registered_names()
            .into_iter()
            .filter_map(|name| {
                session
                    .registry
                    .lookup_value(name)
                    .ok()
                    .flatten()
                    .map(|value| (name.to_string(), value.clone()))
            })
            .collect()
    }

    fn has_debug_children(&self) -> bool {
        !self.session.borrow().registry.registered_names().is_empty()
    }
}

fn source_path_token(text: &str) -> String {
    format!(
        "sha256:{}",
        ArtifactFingerprint::sha256(text.as_bytes()).as_str()
    )
}

fn invoke_registered_provider_callback(
    callback: RuntimeValue,
    context: &mut NativeProviderContext<'_>,
) -> Result<QueryProviderCallbackOutcome, String> {
    context.with_isolated_state(|compiler, unit| {
        let compiler_bridge = Rc::new(CompilerBridgeValue::from_session_state(compiler));
        let unit_bridge = Rc::new(UnitBridgeValue::from_unit_snapshot(unit));
        let active_context = {
            compiler_bridge
                .session
                .borrow()
                .dispatch
                .active_context
                .clone()
        };
        let outcome = match active_context {
            Some(active_context) => {
                let context_bridge = Rc::new(ProviderContextBridgeValue::new(
                    active_context.clone(),
                    Rc::clone(&compiler_bridge),
                    Rc::clone(&unit_bridge),
                ));
                unit_bridge.attach_provider_context(Rc::downgrade(&context_bridge));
                let unit_object: Rc<dyn HostObject> = unit_bridge.clone();
                let root_handle = provider_root_handle_or_null(&unit_bridge, unit_object);
                let mut evaluator = Evaluator::with_phase(Default::default(), active_context.phase);
                match registered_provider_callback_args(
                    &callback,
                    RuntimeValue::HostObject(context_bridge.clone()),
                    root_handle,
                ) {
                    Ok(args) => {
                        let initial_bindings = active_context.initial_bindings;
                        let result = invoke_provider_callback_with_initial(
                            &mut evaluator,
                            &callback,
                            args,
                            &initial_bindings,
                        );
                        let mut tracked_context = context_bridge.tracked_context();
                        if let Some(active_context) = compiler_bridge
                            .session
                            .borrow()
                            .dispatch
                            .active_context
                            .as_ref()
                        {
                            merge_provider_context_tracking(&mut tracked_context, active_context);
                        }
                        compiler_bridge.session.borrow_mut().dispatch.active_context =
                            Some(tracked_context);
                        result
                            .map(|value| provider_callback_outcome_from_value(&value))
                            .map_err(eval_signal_message)
                    }
                    Err(error) => Err(error),
                }
            }
            None => Err("query provider callback requires an active provider context".to_string()),
        };
        (
            compiler_bridge.clone_session_state(),
            unit_bridge.clone_unit_snapshot(),
            outcome,
        )
    })
}

fn provider_root_handle_or_null(
    unit_bridge: &UnitBridgeValue,
    unit_object: Rc<dyn HostObject>,
) -> RuntimeValue {
    unit_bridge
        .with_unit(|unit| {
            let root_id = unit.root_id();
            unit.ir().node(root_id).map(|_| root_id)
        })
        .map(|root_id| {
            RuntimeValue::HostObject(Rc::new(NodeBridgeValue::new(unit_object, root_id)))
        })
        .unwrap_or(RuntimeValue::Null)
}

fn provider_callback_outcome_from_value(value: &RuntimeValue) -> QueryProviderCallbackOutcome {
    QueryProviderCallbackOutcome::changed(provider_result_changed(value))
}

fn validate_registered_provider_callback_abi(
    provider_name: &str,
    callback: &RuntimeValue,
) -> CaapResult<()> {
    provider_callback_abi_arity(callback)
        .map(|_| ())
        .map_err(|message| {
            CaapError::compiler(format!(
                "query provider {provider_name:?} callback ABI is invalid: {message}"
            ))
        })
}

fn provider_callback_abi_arity(callback: &RuntimeValue) -> Result<usize, String> {
    match callback {
        RuntimeValue::Closure(closure) => match closure.params.len() {
            1 | 2 => Ok(closure.params.len()),
            actual => Err(format!(
                "closure must accept (ctx) or (ctx root), got {actual} parameters"
            )),
        },
        RuntimeValue::HostFunction(host) => callback_callable_abi_arity(
            host.min_arity,
            host.max_arity,
            &format!("host function {}", host.name),
        ),
        RuntimeValue::Builtin(builtin) => callback_callable_abi_arity(
            builtin.min_arity,
            builtin.max_arity,
            &format!("builtin {}", builtin.name),
        ),
        other => Err(format!(
            "expected a provider callback function, got {}",
            runtime_value_kind_name(other)
        )),
    }
}

fn callback_callable_abi_arity(
    min_arity: usize,
    max_arity: Option<usize>,
    label: &str,
) -> Result<usize, String> {
    let accepts = |arity: usize| min_arity <= arity && max_arity.is_none_or(|max| arity <= max);
    if max_arity == Some(1) && accepts(1) {
        return Ok(1);
    }
    if accepts(2) {
        return Ok(2);
    }
    if accepts(1) {
        return Ok(1);
    }
    Err(format!(
        "{label} must accept provider callback arity 1 or 2, got min={} max={}",
        min_arity,
        max_arity
            .map(|max| max.to_string())
            .unwrap_or_else(|| "unbounded".to_string())
    ))
}

fn runtime_value_kind_name(value: &RuntimeValue) -> &'static str {
    match value {
        RuntimeValue::Null => "null",
        RuntimeValue::Bool(_) => "bool",
        RuntimeValue::Int(_) => "int",
        RuntimeValue::Float(_) => "float",
        RuntimeValue::Str(_) => "string",
        RuntimeValue::Bytes(_) => "bytes",
        RuntimeValue::Tuple(_) => "tuple",
        RuntimeValue::Closure(_) => "closure",
        RuntimeValue::Macro(_) => "macro",
        RuntimeValue::Builtin(_) => "builtin",
        RuntimeValue::HostFunction(_) => "host_function",
        RuntimeValue::HostObject(_) => "host_object",
        RuntimeValue::List(_) => "list",
        RuntimeValue::Map(_) => "map",
        RuntimeValue::Ref(_) => "ref",
        RuntimeValue::UninitializedTopLevel => "uninitialized_top_level",
    }
}

fn provider_result_changed(value: &RuntimeValue) -> bool {
    if let RuntimeValue::Map(map) = value {
        if let Some(changed) = map.borrow().get(&MapKey::Str("changed".into())) {
            return is_truthy(changed);
        }
    }
    is_truthy(value)
}

fn registered_provider_callback_args(
    callback: &RuntimeValue,
    context: RuntimeValue,
    root: RuntimeValue,
) -> Result<Vec<RuntimeValue>, String> {
    match provider_callback_abi_arity(callback) {
        Ok(1) => Ok(vec![context]),
        Ok(2) => Ok(vec![context, root]),
        Ok(actual) => Err(format!(
            "query provider callback ABI resolved to unsupported arity {actual}"
        )),
        Err(error) => Err(format!(
            "query provider callback ABI is invalid during invocation: {error}"
        )),
    }
}

fn invoke_provider_callback_with_initial(
    evaluator: &mut Evaluator,
    callback: &RuntimeValue,
    args: Vec<RuntimeValue>,
    initial_bindings: &[(String, RuntimeValue)],
) -> EvalResult {
    let RuntimeValue::Closure(closure) = callback else {
        return evaluator.invoke_callback(callback, args);
    };
    evaluator.invoke_closure_with_initial_bindings(closure, args, initial_bindings)
}

fn eval_signal_message(signal: EvalSignal) -> String {
    match signal {
        EvalSignal::Error(error) => error.message().to_string(),
        EvalSignal::Leave(signal) => {
            format!(
                "provider callback attempted to leave block {}",
                signal.target_block_id
            )
        }
        EvalSignal::Exception(val) => format!("uncaught exception in provider callback: {val}"),
        // Unreachable by construction (tail positions are never provider
        // boundaries) — message kept honest just in case.
        EvalSignal::TailCall(_) => "internal tail-call signal escaped its closure".to_string(),
    }
}
