pub(super) mod bootstrap_images;
pub(super) mod registration;
pub(super) mod registry;
pub(super) mod types;

pub use registry::{
    validate_base_semantic_entry, validate_dynamic_provider_dependency, CompilerRegistry,
    CompilerRegistrySnapshot,
};
pub(super) use types::{BootstrapState, CompileCache, DiagnosticAccumulator, ProviderDispatch};

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Instant;

use crate::artifacts::{
    ArtifactCache, ArtifactFingerprint, ArtifactKey, SourceArtifact, SourceTemplateArtifact,
    SourceTemplateCache,
};
use crate::diagnostics::{
    CompilerEvent, CompilerEventLog, Diagnostic, DiagnosticCode, DiagnosticSeverity,
};
use crate::error::{CaapError, CaapResult};
use crate::frontend::{parse_segmental, parse_segmental_with_source_path};
use crate::semantic::{PhasePolicy, SemanticEntry, SemanticSubjectId};
use crate::unit::{Unit, UnitSyntaxState, UnitTemplate};
use crate::values::RuntimeValue;

use super::bootstrap::CompilerBootstrapController;
use super::bridges::SemanticPolicyRegistration;
use super::fact_schema::FactSchemaRegistry;
use super::host::{CompilerCatalog, CompilerHost, CompilerNameService, DiagnosticSink};
use super::query_provider::{
    extend_unique, normalize_virtual_path, semantic_cell_tracking_key,
    semantic_subject_tracking_key, QueryProviderContext, QueryProviderRegistry,
};

use super::eval_service::CompilerEvaluationService;
use super::query_service::CompilerQueryService;

#[cfg(test)]
use super::bootstrap::{
    BootstrapCapabilityGraph, BootstrapImage, BootstrapImageFile, BootstrapVirtualFileSystem,
};
#[cfg(test)]
use super::fact_schema::FactSchemaTypeBridge;
#[cfg(test)]
use super::query_provider::{
    QueryProviderCallbackOutcome, QueryProviderContractSpec, QueryProviderRegistrationSpec,
    QueryStageSpec,
};

const HOST_SOURCE_TEMPLATE_CACHE_MAX_ENTRIES: usize = 512;

pub(super) type HostSourceTemplateCache = Rc<Mutex<BTreeMap<(String, ArtifactKey), UnitTemplate>>>;

fn track_active_unit_cell(
    context: Option<&mut QueryProviderContext>,
    unit_id: &str,
    predicate: &str,
    write: bool,
) {
    let Some(context) = context else {
        return;
    };
    let subject = SemanticSubjectId::new("unit", unit_id.to_string())
        .expect("active query unit id must be non-empty");
    let subject_key = semantic_subject_tracking_key(&subject);
    let cell_key = semantic_cell_tracking_key(&subject_key, predicate);
    if write {
        extend_unique(&mut context.writes_subjects, [subject_key]);
        extend_unique(&mut context.write_cells, [cell_key]);
    } else {
        extend_unique(&mut context.reads_subjects, [subject_key]);
        extend_unique(&mut context.read_cells, [cell_key]);
    }
}

#[derive(Clone, Debug)]
pub struct Compiler {
    pub(super) host: Rc<CompilerHost>,
    pub(super) units: Rc<BTreeMap<String, Unit>>,
    pub(super) registry: CompilerRegistry,
    pub(super) unit_registry_version: u64,
    pub(super) session_version: u64,
    pub(super) name_service: CompilerNameService,
    pub(super) semantic_policies: Rc<BTreeMap<String, SemanticPolicyRegistration>>,
    pub(super) fact_schema: Rc<FactSchemaRegistry>,
    pub(super) base_semantic_entries: Rc<BTreeMap<String, SemanticEntry>>,
    pub(super) bootstrap: BootstrapState,
    pub(super) dispatch: ProviderDispatch,
    pub(super) cache: CompileCache,
    pub(super) diag: DiagnosticAccumulator,
}

impl Compiler {
    pub fn new(host: Rc<CompilerHost>) -> Self {
        tracing::debug!("compiler session created");
        Self {
            host,
            units: Rc::new(BTreeMap::new()),
            registry: CompilerRegistry::new(),
            unit_registry_version: 0,
            session_version: 0,
            name_service: CompilerNameService::new(),
            semantic_policies: Rc::new(BTreeMap::new()),
            fact_schema: Rc::new(FactSchemaRegistry::new()),
            base_semantic_entries: Rc::new(BTreeMap::new()),
            bootstrap: BootstrapState::new(),
            dispatch: ProviderDispatch::new(),
            cache: CompileCache::new(),
            diag: DiagnosticAccumulator::new(),
        }
    }

    pub fn set_diagnostic_sink(&mut self, sink: DiagnosticSink) {
        self.diag.sink = sink;
    }

    pub fn host(&self) -> &CompilerHost {
        &self.host
    }

    pub fn units(&self) -> &BTreeMap<String, Unit> {
        &self.units
    }

    pub fn get_unit(&self, unit_id: &str) -> CaapResult<Option<&Unit>> {
        if unit_id.is_empty() {
            return Err(CaapError::compiler(
                "compiler unit id lookup must be non-empty",
            ));
        }
        Ok(self.units.get(unit_id))
    }

    pub fn register_unit(&mut self, unit: Unit) -> CaapResult<()> {
        let unit_id = unit.unit_id().to_string();
        if unit_id.is_empty() {
            return Err(CaapError::compiler("compiler unit id must be non-empty"));
        }
        let unit_version = unit.version().to_string();
        let unit_changed = match self.units.get(&unit_id) {
            Some(existing) => existing.content_fingerprint()? != unit.content_fingerprint()?,
            None => true,
        };
        let next_versions = if unit_changed {
            Some((
                self.next_unit_registry_version()?,
                self.next_session_version()?,
            ))
        } else {
            None
        };
        self.name_service.register(unit_id.clone())?;
        Rc::make_mut(&mut self.units).insert(unit_id.clone(), unit);
        if let Some((unit_registry_version, session_version)) = next_versions {
            self.unit_registry_version = unit_registry_version;
            self.session_version = session_version;
        }
        self.emit_compiler_event(
            "compiler.unit.register",
            Some(unit_id),
            "registered compiler unit",
            [
                (
                    "session_version".to_string(),
                    self.session_version.to_string(),
                ),
                ("unit_version".to_string(), unit_version),
            ],
        )?;
        Ok(())
    }

    pub fn registry(&self) -> &CompilerRegistry {
        &self.registry
    }

    pub fn fact_schema(&self) -> &FactSchemaRegistry {
        &self.fact_schema
    }

    pub fn base_semantic_entries(&self) -> Vec<&SemanticEntry> {
        let mut entries: Vec<_> = self.base_semantic_entries.values().collect();
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        entries
    }

    pub fn registry_snapshot(&self) -> CompilerRegistrySnapshot {
        self.registry.snapshot()
    }

    pub fn restore_registry_snapshot(
        &mut self,
        snapshot: CompilerRegistrySnapshot,
    ) -> CaapResult<()> {
        self.registry.restore_snapshot(snapshot)?;
        self.advance_session_version()?;
        self.emit_compiler_event(
            "compiler.registry.restore",
            None,
            "restored compiler registry snapshot",
            [
                (
                    "registry_version".to_string(),
                    self.registry.version().to_string(),
                ),
                (
                    "registered_count".to_string(),
                    self.registry.registered_names().len().to_string(),
                ),
            ],
        )?;
        Ok(())
    }

    pub fn register_value(
        &mut self,
        name: impl Into<String>,
        value: RuntimeValue,
    ) -> CaapResult<RuntimeValue> {
        let name = name.into();
        let registered = self.registry.register_value(name.clone(), value)?;
        self.advance_session_version()?;
        self.emit_compiler_event(
            "compiler.registry.value.register",
            Some(name),
            "registered compiler registry value",
            [(
                "registry_version".to_string(),
                self.registry.version().to_string(),
            )],
        )?;
        Ok(registered)
    }

    pub fn lookup_registered_value(&self, name: &str) -> CaapResult<Option<&RuntimeValue>> {
        self.registry.lookup_value(name)
    }

    pub fn require_registered_value(&self, name: &str) -> CaapResult<&RuntimeValue> {
        self.registry.require_value(name)
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diag.diagnostics
    }

    pub fn push_diagnostic(&mut self, diagnostic: Diagnostic) -> CaapResult<()> {
        let session_version = self.next_session_version()?;
        self.diag.sink.emit(&diagnostic);
        self.diag.diagnostics.push(diagnostic);
        self.session_version = session_version;
        Ok(())
    }

    pub fn emit_event(&mut self, event: CompilerEvent) -> CaapResult<()> {
        let session_version = self.next_session_version()?;
        self.diag.events.emit(event);
        self.session_version = session_version;
        Ok(())
    }

    pub fn events(&self) -> &CompilerEventLog {
        &self.diag.events
    }

    pub fn artifact_cache(&self) -> &ArtifactCache {
        &self.cache.artifact_cache
    }

    pub fn artifact_cache_mut(&mut self) -> &mut ArtifactCache {
        Rc::make_mut(&mut self.cache.artifact_cache)
    }

    pub fn save_artifact_cache_file(&mut self, path: impl AsRef<Path>) -> CaapResult<()> {
        let path = path.as_ref();
        Rc::make_mut(&mut self.cache.artifact_cache).save_cache_file(path)?;
        self.emit_compiler_event(
            "compiler.artifact_cache.save",
            Some(path.display().to_string()),
            "saved artifact cache file",
            [
                (
                    "generation".to_string(),
                    self.cache.artifact_cache.stats().generation.to_string(),
                ),
                ("path".to_string(), path.display().to_string()),
            ],
        )?;
        Ok(())
    }

    pub fn load_artifact_cache_file(&mut self, path: impl AsRef<Path>) -> CaapResult<()> {
        let path = path.as_ref();
        Rc::make_mut(&mut self.cache.artifact_cache).load_cache_file(path)?;
        self.emit_compiler_event(
            "compiler.artifact_cache.load",
            Some(path.display().to_string()),
            "loaded artifact cache file",
            [
                (
                    "generation".to_string(),
                    self.cache.artifact_cache.stats().generation.to_string(),
                ),
                ("path".to_string(), path.display().to_string()),
            ],
        )?;
        Ok(())
    }

    pub fn source_templates(&self) -> &SourceTemplateCache {
        &self.cache.source_templates
    }

    pub fn name_service(&self) -> &CompilerNameService {
        &self.name_service
    }

    pub fn catalog(&self) -> CompilerCatalog<'_> {
        CompilerCatalog { units: &self.units }
    }

    pub fn evaluation(&mut self) -> CompilerEvaluationService<'_> {
        CompilerEvaluationService { compiler: self }
    }

    pub fn queries(&mut self) -> CompilerQueryService<'_> {
        CompilerQueryService { compiler: self }
    }

    pub fn bootstrap(&mut self) -> CompilerBootstrapController<'_> {
        CompilerBootstrapController { compiler: self }
    }

    pub fn provider_registry(&self) -> &QueryProviderRegistry {
        &self.dispatch.registry
    }

    pub fn active_provider_context(&self) -> Option<&QueryProviderContext> {
        self.dispatch.active_context.as_ref()
    }

    pub(super) fn track_active_unit_cell_read(&mut self, unit_id: &str, predicate: &str) {
        track_active_unit_cell(
            self.dispatch.active_context.as_mut(),
            unit_id,
            predicate,
            false,
        );
    }

    pub(super) fn track_active_unit_cell_write(&mut self, unit_id: &str, predicate: &str) {
        track_active_unit_cell(
            self.dispatch.active_context.as_mut(),
            unit_id,
            predicate,
            true,
        );
    }

    pub fn request_query_restart(&mut self, stage: impl Into<String>) -> CaapResult<()> {
        let stage = self.dispatch.registry.resolve_stage(stage.into())?;
        self.dispatch.pending_restart = Some(stage);
        self.advance_session_version()?;
        Ok(())
    }

    pub fn session_version(&self) -> u64 {
        self.session_version
    }

    fn next_session_version(&self) -> CaapResult<u64> {
        self.session_version
            .checked_add(1)
            .ok_or_else(|| CaapError::compiler("compiler session version overflow"))
    }

    fn next_unit_registry_version(&self) -> CaapResult<u64> {
        self.unit_registry_version
            .checked_add(1)
            .ok_or_else(|| CaapError::compiler("compiler unit registry version overflow"))
    }

    pub(super) fn advance_session_version(&mut self) -> CaapResult<()> {
        self.session_version = self.next_session_version()?;
        Ok(())
    }

    pub(super) fn advance_unit_registry_version(&mut self) -> CaapResult<()> {
        let unit_registry_version = self.next_unit_registry_version()?;
        let session_version = self.next_session_version()?;
        self.unit_registry_version = unit_registry_version;
        self.session_version = session_version;
        Ok(())
    }

    pub(super) fn emit_compiler_event(
        &mut self,
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
        self.emit_event(event)
    }

    pub fn load_surface_text_template(
        &mut self,
        text: impl Into<String>,
        unit_id: impl Into<String>,
    ) -> CaapResult<SourceTemplateArtifact> {
        let started = Instant::now();
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err(CaapError::compiler(
                "surface text unit id must be non-empty",
            ));
        }
        let source = SourceArtifact::inline(text)?;
        let template_cache = self.host.source_template_cache.clone();
        let stage = source_template_stage(&unit_id);
        let artifact = Rc::make_mut(&mut self.cache.source_templates).load(
            source,
            stage,
            PhasePolicy::CompileTime,
            |source| source_artifact_to_template(&template_cache, source, &unit_id),
        )?;
        self.emit_source_template_event("inline", &unit_id, &artifact, elapsed_ms_string(started))?;
        Ok(artifact)
    }

    pub fn load_surface_path_template(
        &mut self,
        path: impl AsRef<Path>,
        unit_id: impl Into<String>,
    ) -> CaapResult<SourceTemplateArtifact> {
        let started = Instant::now();
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err(CaapError::compiler(
                "surface path unit id must be non-empty",
            ));
        }
        let resolved = resolve_source_path(path.as_ref())?;
        let text = std::fs::read_to_string(&resolved)
            .map_err(|error| CaapError::compiler(format!("surface file read failed: {error}")))?;
        let source_path = path_to_string(&resolved)?;
        let token = source_path_token(&text);
        let source = SourceArtifact::path(source_path, token, text)?;
        let template_cache = self.host.source_template_cache.clone();
        let stage = source_template_stage(&unit_id);
        let artifact = Rc::make_mut(&mut self.cache.source_templates).load(
            source,
            stage,
            PhasePolicy::CompileTime,
            |source| source_artifact_to_template(&template_cache, source, &unit_id),
        )?;
        self.emit_source_template_event("path", &unit_id, &artifact, elapsed_ms_string(started))?;
        Ok(artifact)
    }

    pub fn load_surface_virtual_template(
        &mut self,
        path: impl Into<String>,
        text: impl Into<String>,
        unit_id: impl Into<String>,
    ) -> CaapResult<SourceTemplateArtifact> {
        let started = Instant::now();
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err(CaapError::compiler(
                "surface virtual unit id must be non-empty",
            ));
        }
        let path = normalize_virtual_path(path.into())?;
        let text = text.into();
        let token = ArtifactFingerprint::sha256(text.as_bytes()).to_string();
        let source = SourceArtifact::path(format!("vfs:{path}"), token, text)?;
        let template_cache = self.host.source_template_cache.clone();
        let stage = source_template_stage(&unit_id);
        let artifact = Rc::make_mut(&mut self.cache.source_templates).load(
            source,
            stage,
            PhasePolicy::CompileTime,
            |source| source_artifact_to_template(&template_cache, source, &unit_id),
        )?;
        self.emit_source_template_event("vfs", &unit_id, &artifact, elapsed_ms_string(started))?;
        Ok(artifact)
    }

    fn emit_source_template_event(
        &mut self,
        origin: &str,
        unit_id: &str,
        artifact: &SourceTemplateArtifact,
        elapsed_ms: String,
    ) -> CaapResult<()> {
        self.emit_compiler_event(
            "source.template.load",
            Some(unit_id.to_string()),
            "loaded source template",
            [
                ("cache_hit".to_string(), artifact.cache_hit.to_string()),
                ("elapsed_ms".to_string(), elapsed_ms),
                ("key".to_string(), artifact.key.to_string()),
                ("lineage".to_string(), artifact.lineage_id.to_string()),
                ("origin".to_string(), origin.to_string()),
            ],
        )
    }

    pub fn compile(&mut self, unit: &mut Unit) -> CaapResult<()> {
        if !self.dispatch.registry.has_stages() {
            let mut diagnostic =
                Diagnostic::new(DiagnosticSeverity::Error, "no compiler stages registered")?;
            diagnostic.code = Some(DiagnosticCode::Compiler.as_str().to_string());
            diagnostic
                .help
                .push("execute an explicit bootstrap before compiling".to_string());
            self.push_diagnostic(diagnostic)?;
            return Err(CaapError::compiler("no compiler stages registered"));
        }
        self.register_unit(unit.clone())
            .map_err(CaapError::compiler)?;
        self.emit_compiler_event(
            "compiler.compile",
            Some(unit.unit_id().to_string()),
            "compiled unit",
            [("unit_version".to_string(), unit.version().to_string())],
        )?;
        Ok(())
    }
}

pub(super) fn source_artifact_to_template(
    cache: &HostSourceTemplateCache,
    source: &SourceArtifact,
    unit_id: &str,
) -> CaapResult<UnitTemplate> {
    let cache_key = (
        unit_id.to_string(),
        source.parse_surface_key("parse", PhasePolicy::CompileTime)?,
    );
    if let Some(template) = cache
        .lock()
        .map_err(|_| CaapError::compiler("host source template cache is poisoned"))?
        .get(&cache_key)
        .cloned()
    {
        return Ok(template);
    }

    let source_path = match &source.origin {
        crate::artifacts::SourceOrigin::Path { path, .. } => Some(path.as_str()),
        crate::artifacts::SourceOrigin::Inline { .. } => None,
    };
    // Read segmentally so a top-level `extend_syntax` directive can grow the
    // grammar mid-source; the assembled graph stays whole-program, preserving
    // hoisted (forward-reference-capable) evaluation downstream.
    let graph = match source_path {
        Some(path) => parse_segmental_with_source_path(&source.text, path).map_err(|error| {
            CaapError::parse(format!("failed to parse source artifact {path}: {error}"))
        })?,
        None => parse_segmental(&source.text).map_err(|error| {
            CaapError::parse(format!("failed to parse inline source artifact: {error}"))
        })?,
    };
    let mut unit = Unit::from_graph(unit_id, graph)?;
    let mut syntax = UnitSyntaxState::new("caap")?;
    if let Some(path) = source_path {
        syntax = syntax.with_source(path, source.fingerprint.to_string())?;
    }
    unit.set_syntax_state(syntax)?;
    let template = unit.to_template();
    let mut cache = cache
        .lock()
        .map_err(|_| CaapError::compiler("host source template cache is poisoned"))?;
    if cache.len() >= HOST_SOURCE_TEMPLATE_CACHE_MAX_ENTRIES {
        if let Some(first_key) = cache.keys().next().cloned() {
            cache.remove(&first_key);
        }
    }
    cache.insert(cache_key, template.clone());
    Ok(template)
}

pub(super) fn source_template_stage(unit_id: &str) -> String {
    format!("parse:{unit_id}")
}

pub(super) fn resolve_source_path(path: &Path) -> CaapResult<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(CaapError::compiler("source path must be non-empty"));
    }
    std::fs::canonicalize(path).map_err(|error| {
        CaapError::compiler(format!(
            "source path resolution failed for {}: {error}",
            path.display()
        ))
    })
}

fn source_path_token(text: &str) -> String {
    format!(
        "sha256:{}",
        ArtifactFingerprint::sha256(text.as_bytes()).as_str()
    )
}

pub(super) fn elapsed_ms_string(started: Instant) -> String {
    format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0)
}

pub(super) fn path_to_string(path: &Path) -> CaapResult<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| CaapError::compiler("source path is not valid UTF-8"))
}

pub fn bootstrap_image_file_fingerprint(path: impl AsRef<Path>) -> CaapResult<String> {
    let path = path.as_ref();
    let text = fs::read_to_string(path).map_err(|error| {
        CaapError::compiler(format!(
            "failed to read bootstrap image file {}: {error}",
            path.display()
        ))
    })?;
    Ok(ArtifactFingerprint::sha256(text.as_bytes()).to_string())
}

pub(super) fn live_trace_event_line(event: &CompilerEvent) -> String {
    let target = event.target.as_deref().unwrap_or("-");
    let metadata = event
        .metadata
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(" ");
    if metadata.is_empty() {
        format!("[caap-trace] {} target={target}", event.kind)
    } else {
        format!("[caap-trace] {} target={target} {metadata}", event.kind)
    }
}

pub(super) fn should_emit_live_trace(event: &CompilerEvent) -> bool {
    if std::env::var_os("CAAP_LIVE_TRACE").is_none() {
        return false;
    }
    let Some(filter) = std::env::var("CAAP_LIVE_TRACE_FILTER")
        .ok()
        .filter(|filter| !filter.trim().is_empty())
    else {
        return true;
    };
    filter.split(',').map(str::trim).any(|needle| {
        !needle.is_empty()
            && (event.kind.contains(needle)
                || event
                    .target
                    .as_deref()
                    .is_some_and(|target| target.contains(needle)))
    })
}

#[cfg(test)]
mod tests;
