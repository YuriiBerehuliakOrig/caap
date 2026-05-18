//! Minimal compiler host/session substrate for the Rust CAAP port.
//!
//! This mirrors the Python boundary where `CompilerHost` owns long-lived host
//! resources and `Compiler` owns mutable per-session state. A fresh session is
//! intentionally bare: no stdlib, stages, providers, or system services are
//! bootstrapped implicitly.

use std::any::Any;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;
use std::time::{Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::artifacts::{
    ArtifactCache, ArtifactCacheStats, ArtifactFingerprint, ArtifactInvalidationRecord,
    ArtifactKey, ArtifactValue, SourceArtifact, SourceTemplateArtifact, SourceTemplateCache,
};
use crate::bridges::NodeBridgeValue;
use crate::diagnostics::{
    render_diagnostic, CompilerEvent, CompilerEventLog, Diagnostic, DiagnosticExplanation,
    DiagnosticExplanationRegistry, DiagnosticSeverity,
};
use crate::eval::Evaluator;
use crate::frontend::{
    parse, parse_forms, parse_forms_with_source_path, parse_with_source_path, ParsedForm,
    ParsedSource,
};
use crate::host::{HostServiceExport, HostServiceRegistry, HostSystemPolicy};
use crate::ir::{Node, NodeId};
use crate::semantic::{
    node_subject_id, symbol_subject_id, ControlPolicy, EffectPolicy, EvalPolicy, PhasePolicy,
    ScopePolicy, SemanticSubjectId, SemanticValue, SymbolKind,
};
use crate::unit::{Unit, UnitAttributeSnapshot, UnitSnapshot, UnitSyntaxState, UnitTemplate};
use crate::values::{
    is_truthy, Environment, EvalResult, EvalSignal, HostObject, MapKey, RuntimeValue,
};

const HOST_SOURCE_TEMPLATE_CACHE_MAX_ENTRIES: usize = 512;

type HostSourceTemplateCache = Rc<Mutex<BTreeMap<(String, ArtifactKey), UnitTemplate>>>;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompilerHostConfig {
    pub search_paths: Vec<String>,
    pub validation_debug: bool,
}

impl CompilerHostConfig {
    pub fn new(search_paths: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut search_paths: Vec<String> = search_paths.into_iter().collect();
        if search_paths.iter().any(String::is_empty) {
            return Err("compiler host search paths must be non-empty".to_string());
        }
        search_paths.sort();
        search_paths.dedup();
        Ok(Self {
            search_paths,
            validation_debug: false,
        })
    }

    pub fn with_validation_debug(mut self, validation_debug: bool) -> Self {
        self.validation_debug = validation_debug;
        self
    }
}

#[derive(Clone, Debug)]
pub struct CompilerHost {
    config: CompilerHostConfig,
    runtime_services: HostServiceRegistry,
    compile_time_services: HostServiceRegistry,
    source_template_cache: HostSourceTemplateCache,
    host_version: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompilerNameService {
    names: BTreeSet<String>,
    version: u64,
}

#[derive(Clone, Debug)]
pub struct Compiler {
    host: Rc<CompilerHost>,
    units: BTreeMap<String, Unit>,
    registry: CompilerRegistry,
    module_catalog: ModuleCatalog,
    diagnostics: Vec<Diagnostic>,
    events: CompilerEventLog,
    explanations: DiagnosticExplanationRegistry,
    artifact_cache: ArtifactCache,
    provider_ctfe_cache: BTreeMap<ArtifactKey, ProviderCacheEntry>,
    source_templates: SourceTemplateCache,
    name_service: CompilerNameService,
    semantic_policies: BTreeMap<String, SemanticPolicyRegistration>,
    fact_schema: FactSchemaRegistry,
    language_builtin_bridges: BTreeSet<String>,
    registered_stages: BTreeSet<String>,
    provider_registry: QueryProviderRegistry,
    bootstrap_capabilities: BootstrapCapabilityGraph,
    bootstrap_images: BootstrapImageStore,
    active_provider_context: Option<QueryProviderContext>,
    provider_dynamic_requires: BTreeMap<String, Vec<String>>,
    pending_query_restart: Option<String>,
    bootstrap_executions: Vec<String>,
    bootstrap_trace: Vec<BootstrapTraceEvent>,
    bootstrap_execution_memo: BTreeSet<String>,
    active_bootstrap_depth: usize,
    session_version: u64,
}

#[derive(Clone, Debug)]
pub struct CompilerCatalog<'a> {
    units: &'a BTreeMap<String, Unit>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompilerRegistry {
    values: BTreeMap<String, RuntimeValue>,
    compile_time_functions: BTreeSet<String>,
    version: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompilerRegistrySnapshot {
    values: BTreeMap<String, RuntimeValue>,
    compile_time_functions: BTreeSet<String>,
    version: u64,
}

#[derive(Debug)]
pub struct CompilerBridgeValue {
    host: Rc<CompilerHost>,
    registry: RefCell<CompilerRegistry>,
    provider_registry: RefCell<QueryProviderRegistry>,
    registered_stages: RefCell<BTreeSet<String>>,
    units: RefCell<BTreeMap<String, Unit>>,
    module_catalog: RefCell<ModuleCatalog>,
    name_service: RefCell<CompilerNameService>,
    semantic_policies: RefCell<BTreeMap<String, SemanticPolicyRegistration>>,
    fact_schema: RefCell<FactSchemaRegistry>,
    language_builtin_bridges: RefCell<BTreeSet<String>>,
    diagnostics: RefCell<Vec<Diagnostic>>,
    artifact_cache: RefCell<ArtifactCache>,
    provider_ctfe_cache: RefCell<BTreeMap<ArtifactKey, ProviderCacheEntry>>,
    bootstrap_capabilities: RefCell<BootstrapCapabilityGraph>,
    bootstrap_images: RefCell<BootstrapImageStore>,
    active_provider_context: RefCell<Option<QueryProviderContext>>,
    provider_dynamic_requires: RefCell<BTreeMap<String, Vec<String>>>,
    bootstrap_executions: RefCell<Vec<String>>,
    bootstrap_trace: RefCell<Vec<BootstrapTraceEvent>>,
    bootstrap_execution_memo: RefCell<BTreeSet<String>>,
    active_bootstrap_depth: RefCell<usize>,
    bootstrap_path_stack: RefCell<Vec<String>>,
    bootstrap_unit_stack: RefCell<Vec<String>>,
    bootstrap_capability_stack: RefCell<Vec<Vec<String>>>,
    source_templates: RefCell<SourceTemplateCache>,
    explanations: RefCell<DiagnosticExplanationRegistry>,
    events: RefCell<Vec<CompilerEvent>>,
    dirty_session: RefCell<bool>,
}

#[derive(Debug)]
pub struct UnitBridgeValue {
    unit: RefCell<Unit>,
}

#[derive(Debug)]
pub struct ProviderContextBridgeValue {
    context: QueryProviderContext,
    compiler: Rc<CompilerBridgeValue>,
    unit: Rc<UnitBridgeValue>,
    reads_subjects: RefCell<BTreeSet<String>>,
    writes_subjects: RefCell<BTreeSet<String>>,
    read_cells: RefCell<BTreeSet<String>>,
    write_cells: RefCell<BTreeSet<String>>,
    reads_files: RefCell<BTreeSet<String>>,
    writes_files: RefCell<BTreeSet<String>>,
    artifact_dependencies: RefCell<BTreeSet<ArtifactKey>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueryArtifactProjection {
    pub artifact_kind: String,
    pub stage: String,
    pub family: String,
    pub phase: PhasePolicy,
    pub key: ArtifactKey,
    pub origin_key: Option<ArtifactKey>,
    pub dependencies: Vec<ArtifactKey>,
    pub diagnostics: Vec<Diagnostic>,
    pub iterations: usize,
    pub execution_summary: Vec<QueryProviderExecutionRecord>,
    pub reads_subjects: Vec<String>,
    pub writes_subjects: Vec<String>,
    pub read_cells: Vec<String>,
    pub write_cells: Vec<String>,
    pub reads_files: Vec<String>,
    pub writes_files: Vec<String>,
    pub value: ArtifactValue,
}

#[derive(Clone, Debug)]
pub struct QueryExecutionProjection {
    pub plan: QueryPlan,
    pub artifact: QueryArtifactProjection,
    pub invalidations: Vec<Option<ArtifactInvalidationRecord>>,
    pub unit: Unit,
}

#[derive(Clone, Debug)]
pub enum QueryArtifactSource {
    Unit(Box<Unit>),
    Path(String),
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticPolicyRegistration {
    pub name: String,
    pub phase_policy: PhasePolicy,
    pub effect_policy: EffectPolicy,
    pub eval_policy: EvalPolicy,
    pub control_policy: ControlPolicy,
    pub scope_policy: ScopePolicy,
    pub form_policy: String,
    pub normalizer: RuntimeValue,
    pub unit_id: Option<String>,
    pub stable_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FactSchemaTypeBridgeKind {
    ResolvedName,
    ResolvedBlock,
    CallSemantics,
    String,
    Map,
    Object,
}

impl FactSchemaTypeBridgeKind {
    pub fn from_bridge_name(name: &str) -> Result<Self, String> {
        match name {
            "resolved-name" => Ok(Self::ResolvedName),
            "resolved-block" => Ok(Self::ResolvedBlock),
            "call-semantics" => Ok(Self::CallSemantics),
            "string" => Ok(Self::String),
            "map" => Ok(Self::Map),
            "object" => Ok(Self::Object),
            _ => Err(format!(
                "unknown Python fact schema type bridge {name:?}; known bridges: \
                 call-semantics, map, object, resolved-block, resolved-name, string"
            )),
        }
    }

    fn accepts(self, value: &SemanticValue) -> bool {
        match self {
            Self::String => matches!(value, SemanticValue::Str(_)),
            Self::Map | Self::ResolvedName | Self::ResolvedBlock | Self::CallSemantics => {
                matches!(value, SemanticValue::Map(_))
            }
            Self::Object => true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FactSchemaTypeBridge {
    pub label: String,
    pub bridge_name: String,
    pub kind: FactSchemaTypeBridgeKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FactSchemaEntry {
    pub predicate: String,
    pub type_label: String,
    pub bridge_name: String,
    pub allow_none: bool,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FactSchemaRegistry {
    #[serde(default)]
    type_bridges: BTreeMap<String, FactSchemaTypeBridge>,
    #[serde(default)]
    schemas: BTreeMap<String, FactSchemaEntry>,
    #[serde(default)]
    version: u64,
}

impl FactSchemaRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_type_bridge(
        &mut self,
        label: impl Into<String>,
        bridge_name: impl Into<String>,
    ) -> Result<(), String> {
        let label = require_registry_name(label.into())?;
        let bridge_name = require_registry_name(bridge_name.into())?;
        let kind = FactSchemaTypeBridgeKind::from_bridge_name(&bridge_name)?;
        let entry = FactSchemaTypeBridge {
            label: label.clone(),
            bridge_name,
            kind,
        };
        if self.type_bridges.get(&label) != Some(&entry) {
            self.type_bridges.insert(label, entry);
            self.version += 1;
        }
        Ok(())
    }

    pub fn register_schema(
        &mut self,
        predicate: impl Into<String>,
        type_label: impl Into<String>,
        allow_none: bool,
        description: Option<String>,
    ) -> Result<(), String> {
        let predicate = require_registry_name(predicate.into())?;
        let type_label = require_registry_name(type_label.into())?;
        let bridge = self.type_bridges.get(&type_label).ok_or_else(|| {
            format!(
                "unknown compiler fact schema type {type_label:?}; register a type bridge \
                 with ctfe-compiler-fact-schema-type-bridge-register first"
            )
        })?;
        if description.as_deref().is_some_and(str::is_empty) {
            return Err("compiler fact schema description must be non-empty".to_string());
        }
        let entry = FactSchemaEntry {
            predicate: predicate.clone(),
            type_label,
            bridge_name: bridge.bridge_name.clone(),
            allow_none,
            description,
        };
        if self.schemas.get(&predicate) != Some(&entry) {
            self.schemas.insert(predicate, entry);
            self.version += 1;
        }
        Ok(())
    }

    pub fn lookup(&self, predicate: &str) -> Result<Option<&FactSchemaEntry>, String> {
        require_registry_name(predicate.to_string())?;
        Ok(self.schemas.get(predicate))
    }

    pub fn validate_value(&self, predicate: &str, value: &SemanticValue) -> Result<(), String> {
        let Some(schema) = self.lookup(predicate)? else {
            return Ok(());
        };
        if matches!(value, SemanticValue::Null) {
            if schema.allow_none {
                return Ok(());
            }
            return Err(format!("fact {predicate:?} does not allow null values"));
        }
        let bridge = self.type_bridges.get(&schema.type_label).ok_or_else(|| {
            format!(
                "compiler fact schema type {:?} has no bridge",
                schema.type_label
            )
        })?;
        if bridge.kind.accepts(value) {
            Ok(())
        } else {
            Err(format!(
                "fact {predicate:?} expects value compatible with schema type {:?}",
                schema.type_label
            ))
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        for (label, bridge) in &self.type_bridges {
            require_registry_name(label.clone())?;
            require_registry_name(bridge.label.clone())?;
            require_registry_name(bridge.bridge_name.clone())?;
            if label != &bridge.label {
                return Err(format!(
                    "compiler fact schema bridge key {label:?} does not match entry label {:?}",
                    bridge.label
                ));
            }
            let kind = FactSchemaTypeBridgeKind::from_bridge_name(&bridge.bridge_name)?;
            if kind != bridge.kind {
                return Err(format!(
                    "compiler fact schema bridge {:?} kind does not match bridge name {:?}",
                    bridge.label, bridge.bridge_name
                ));
            }
        }
        for (predicate, schema) in &self.schemas {
            require_registry_name(predicate.clone())?;
            require_registry_name(schema.predicate.clone())?;
            require_registry_name(schema.type_label.clone())?;
            require_registry_name(schema.bridge_name.clone())?;
            if predicate != &schema.predicate {
                return Err(format!(
                    "compiler fact schema key {predicate:?} does not match entry predicate {:?}",
                    schema.predicate
                ));
            }
            let bridge = self.type_bridges.get(&schema.type_label).ok_or_else(|| {
                format!(
                    "compiler fact schema {:?} references unknown type {:?}",
                    schema.predicate, schema.type_label
                )
            })?;
            if bridge.bridge_name != schema.bridge_name {
                return Err(format!(
                    "compiler fact schema {:?} bridge name {:?} does not match type bridge {:?}",
                    schema.predicate, schema.bridge_name, bridge.bridge_name
                ));
            }
            if schema.description.as_deref().is_some_and(str::is_empty) {
                return Err(format!(
                    "compiler fact schema {:?} description must be non-empty",
                    schema.predicate
                ));
            }
        }
        Ok(())
    }

    pub fn type_bridge(&self, label: &str) -> Result<Option<&FactSchemaTypeBridge>, String> {
        require_registry_name(label.to_string())?;
        Ok(self.type_bridges.get(label))
    }

    pub fn schemas(&self) -> Vec<&FactSchemaEntry> {
        self.schemas.values().collect()
    }

    pub fn type_bridges(&self) -> Vec<&FactSchemaTypeBridge> {
        self.type_bridges.values().collect()
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleMaterialization {
    pub module_name: String,
    pub unit_id: String,
    pub source_path: Option<String>,
    pub catalog_version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PackageImportSymbol {
    pub name: String,
    #[serde(rename = "as")]
    pub alias: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PackageImport {
    #[serde(rename = "module")]
    pub module_name: String,
    #[serde(rename = "as")]
    pub alias: String,
    pub symbols: Vec<PackageImportSymbol>,
    #[serde(default)]
    pub syntax: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PackageExport {
    pub name: String,
    pub path: Option<String>,
    pub registry: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PackageDescriptor {
    pub name: String,
    pub index_path: String,
    pub base_dir: String,
    pub imports: Vec<PackageImport>,
    pub syntax_imports: Vec<PackageImport>,
    pub exports: Vec<PackageExport>,
    pub capabilities: Vec<String>,
    pub declaration_count: usize,
    pub state: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModuleCatalog {
    modules: BTreeMap<String, ModuleMaterialization>,
    version: u64,
}

pub struct CompilerEvaluationService<'a> {
    compiler: &'a mut Compiler,
}

pub struct CompilerQueryService<'a> {
    compiler: &'a mut Compiler,
}

pub struct CompilerBootstrapController<'a> {
    compiler: &'a mut Compiler,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct BootstrapVirtualFileSystem {
    files: BTreeMap<String, String>,
    version: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct BootstrapCapabilityGraph {
    grants: BTreeMap<String, BTreeSet<String>>,
    version: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BootstrapImage {
    pub name: String,
    pub units: Vec<UnitTemplate>,
    pub capabilities: BootstrapCapabilityGraph,
    #[serde(default)]
    pub fact_schema: FactSchemaRegistry,
    #[serde(default)]
    pub language_builtin_bridges: Vec<String>,
    pub session_version: u64,
}

#[derive(Clone, Debug, Default)]
pub struct BootstrapImageStore {
    images: BTreeMap<String, BootstrapImage>,
    version: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BootstrapImageFile {
    pub format_name: String,
    pub format_version: u32,
    pub image: BootstrapImage,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BootstrapImageTrustPolicy {
    trusted_fingerprints: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub struct EvaluationCapture {
    pub unit_id: String,
    pub phase: PhasePolicy,
    pub value: Option<RuntimeValue>,
    pub bindings: Vec<(String, RuntimeValue)>,
    pub diagnostics: Vec<Diagnostic>,
    pub skipped_forms: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootstrapTraceEvent {
    pub action: String,
    pub target: String,
    pub depth: usize,
    pub succeeded: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryStageSpec {
    pub name: String,
    pub requires: Vec<String>,
    pub phase_policy: PhasePolicy,
    pub input_kinds: Vec<String>,
    pub family_label: Option<String>,
    pub aliases: Vec<String>,
    pub restart_stage: Option<String>,
}

#[derive(Clone)]
pub struct QueryProvider {
    pub name: String,
    pub stage: String,
    pub family: Option<String>,
    pub phase_policy: PhasePolicy,
    pub requires: Vec<String>,
    pub requires_data: Vec<String>,
    pub provides_data: Vec<String>,
    pub provides: Vec<String>,
    pub effect_tags: Vec<String>,
    pub input_schema: Option<String>,
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    pub cache_scope: String,
    pub resume_policy: String,
    pub registration_index: u64,
    callback: Rc<dyn Fn(&mut Compiler, &mut Unit) -> Result<QueryProviderCallbackOutcome, String>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryProviderCallbackOutcome {
    pub changed: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderTransactionMode {
    None,
    Semantic,
    Attributes,
    Unit,
}

#[derive(Clone, Debug)]
enum ProviderRollbackSnapshot {
    Semantic(crate::semantic::UnifiedSemanticGraphSnapshot),
    Attributes(UnitAttributeSnapshot),
    Unit(Box<UnitSnapshot>),
}

impl QueryProviderCallbackOutcome {
    pub fn unchanged() -> Self {
        Self {
            changed: Some(false),
        }
    }

    pub fn changed(changed: bool) -> Self {
        Self {
            changed: Some(changed),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueryProviderContext {
    pub provider: String,
    pub stage: String,
    pub family: Option<String>,
    pub phase: PhasePolicy,
    pub unit_id: String,
    pub effect_tags: Vec<String>,
    pub initial_bindings: Vec<(String, RuntimeValue)>,
    pub registration_index: u64,
    pub reads_subjects: Vec<String>,
    pub writes_subjects: Vec<String>,
    pub read_cells: Vec<String>,
    pub write_cells: Vec<String>,
    pub reads_files: Vec<String>,
    pub writes_files: Vec<String>,
    pub artifact_dependencies: Vec<ArtifactKey>,
}

#[derive(Clone, Debug)]
pub struct ProviderCacheEntry {
    pub snapshot: Option<UnitSnapshot>,
    pub diagnostics: Vec<Diagnostic>,
    pub reads_subjects: Vec<String>,
    pub writes_subjects: Vec<String>,
    pub read_cells: Vec<String>,
    pub write_cells: Vec<String>,
    pub reads_files: Vec<String>,
    pub writes_files: Vec<String>,
    pub artifact_dependencies: Vec<ArtifactKey>,
    pub dynamic_requires: Vec<String>,
    pub changed: bool,
    pub restart_requested: bool,
    pub restart_stage: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryProviderRegistrationSpec {
    pub family: Option<String>,
    pub input_schema: Option<String>,
    pub requires_data: Vec<String>,
    pub provides_data: Vec<String>,
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    pub cache_scope: String,
    pub resume_policy: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QueryTransactionMode {
    #[default]
    InPlace,
    AtomicUnit,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueryExecutionOptions {
    pub transaction_mode: QueryTransactionMode,
    pub restart_limit: usize,
    pub allowed_effect_tags: Option<Vec<String>>,
    pub initial_bindings: Vec<(String, RuntimeValue)>,
}

impl Default for QueryExecutionOptions {
    fn default() -> Self {
        Self {
            transaction_mode: QueryTransactionMode::InPlace,
            restart_limit: 1,
            allowed_effect_tags: None,
            initial_bindings: Vec::new(),
        }
    }
}

impl QueryExecutionOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_transaction_mode(mut self, transaction_mode: QueryTransactionMode) -> Self {
        self.transaction_mode = transaction_mode;
        self
    }

    pub fn with_restart_limit(mut self, restart_limit: usize) -> Self {
        self.restart_limit = restart_limit;
        self
    }

    pub fn with_allowed_effect_tags(
        mut self,
        effect_tags: impl IntoIterator<Item = String>,
    ) -> Self {
        self.allowed_effect_tags = Some(effect_tags.into_iter().collect());
        self
    }

    pub fn with_initial_bindings(
        mut self,
        initial_bindings: impl IntoIterator<Item = (String, RuntimeValue)>,
    ) -> Self {
        self.initial_bindings = initial_bindings.into_iter().collect();
        self
    }
}

impl Default for QueryProviderRegistrationSpec {
    fn default() -> Self {
        Self {
            family: None,
            input_schema: None,
            requires_data: Vec::new(),
            provides_data: Vec::new(),
            reads: Vec::new(),
            writes: Vec::new(),
            cache_scope: "none".to_string(),
            resume_policy: "safe".to_string(),
        }
    }
}

impl QueryProviderRegistrationSpec {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPlanStep {
    pub stage: String,
    pub provider_names: Vec<String>,
    pub effect_tags: Vec<String>,
    pub cached: bool,
    pub artifact_key: Option<ArtifactKey>,
    pub restarted: bool,
    pub restart_target: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPlan {
    pub target: String,
    pub phase: PhasePolicy,
    pub steps: Vec<QueryPlanStep>,
    pub executed: Vec<QueryProviderExecutionRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryProviderExecutionRecord {
    pub provider_name: String,
    pub stage: String,
    pub family: Option<String>,
    pub phase_policy: PhasePolicy,
    pub effect_tags: Vec<String>,
    pub requires: Vec<String>,
    pub requires_data: Vec<String>,
    pub provides_data: Vec<String>,
    pub provides: Vec<String>,
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    pub reads_subjects: Vec<String>,
    pub writes_subjects: Vec<String>,
    pub read_cells: Vec<String>,
    pub write_cells: Vec<String>,
    pub reads_files: Vec<String>,
    pub writes_files: Vec<String>,
    pub artifact_dependencies: Vec<ArtifactKey>,
    pub cache_scope: String,
    pub resume_policy: String,
    pub iteration: usize,
    pub changed: bool,
    pub diagnostics_emitted: usize,
    pub rolled_back: bool,
    pub stopped_by_error: bool,
    pub outcome_kind: String,
    pub diagnostic_codes: Vec<String>,
    pub rewrite_count: usize,
    pub erased_count: usize,
    pub touched_node_kinds: Vec<String>,
    pub change_domains: Vec<String>,
    pub restart_requested: bool,
    pub restart_stage: Option<String>,
    pub outcome_summary: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default)]
pub struct QueryProviderRegistry {
    stages: BTreeMap<String, QueryStageSpec>,
    aliases: BTreeMap<String, String>,
    default_stage_by_family: BTreeMap<String, String>,
    input_kind_to_stage: BTreeMap<String, String>,
    providers: Vec<QueryProvider>,
    next_registration_index: u64,
    version: u64,
}

#[derive(Clone, Debug)]
pub struct QueryProviderSchedule {
    pub groups: Vec<Vec<QueryProvider>>,
    pub barriers: Vec<Option<Vec<String>>>,
}

impl CompilerHost {
    pub fn new() -> Self {
        Self {
            config: CompilerHostConfig::default(),
            runtime_services: HostServiceRegistry::new(),
            compile_time_services: HostServiceRegistry::new(),
            source_template_cache: Rc::new(Mutex::new(BTreeMap::new())),
            host_version: 0,
        }
    }

    pub fn with_config(config: CompilerHostConfig) -> Self {
        Self {
            config,
            runtime_services: HostServiceRegistry::new(),
            compile_time_services: HostServiceRegistry::new(),
            source_template_cache: Rc::new(Mutex::new(BTreeMap::new())),
            host_version: 0,
        }
    }

    pub fn config(&self) -> &CompilerHostConfig {
        &self.config
    }

    pub fn runtime_services(&self) -> &HostServiceRegistry {
        &self.runtime_services
    }

    pub fn runtime_services_mut(&mut self) -> &mut HostServiceRegistry {
        self.host_version += 1;
        &mut self.runtime_services
    }

    pub fn register_default_runtime_system_libraries(&mut self) -> Result<(), String> {
        self.runtime_services.register_default_system_libraries()?;
        self.host_version += 1;
        Ok(())
    }

    pub fn compile_time_services(&self) -> &HostServiceRegistry {
        &self.compile_time_services
    }

    pub fn compile_time_services_mut(&mut self) -> &mut HostServiceRegistry {
        self.host_version += 1;
        &mut self.compile_time_services
    }

    pub fn register_default_compile_time_system_libraries(&mut self) -> Result<(), String> {
        let read_root = std::env::current_dir()
            .map_err(|error| format!("compile-time host policy current_dir failed: {error}"))?;
        self.compile_time_services
            .set_system_policy(HostSystemPolicy::compile_time_sandbox(Some(vec![
                read_root,
            ])));
        self.compile_time_services
            .register_default_system_libraries()?;
        self.host_version += 1;
        Ok(())
    }

    pub fn host_version(&self) -> u64 {
        self.host_version
    }

    pub fn new_session(&self) -> Compiler {
        Compiler::new(Rc::new(self.clone()))
    }
}

impl Default for CompilerHost {
    fn default() -> Self {
        Self::new()
    }
}

impl CompilerNameService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, name: impl Into<String>) -> Result<(), String> {
        let name = name.into();
        if name.is_empty() {
            return Err("compiler name must be non-empty".to_string());
        }
        if self.names.insert(name) {
            self.version += 1;
        }
        Ok(())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.names.iter().map(String::as_str).collect()
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

impl CompilerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_value(
        &mut self,
        name: impl Into<String>,
        value: RuntimeValue,
    ) -> Result<RuntimeValue, String> {
        let name = require_registry_name(name.into())?;
        if self.values.contains_key(&name) {
            return Err(format!("compiler registry already contains {name:?}"));
        }
        self.values.insert(name, value.clone());
        self.version += 1;
        Ok(value)
    }

    pub fn register_compile_time_function(
        &mut self,
        name: impl Into<String>,
        value: RuntimeValue,
    ) -> Result<(), String> {
        let name = require_registry_name(name.into())?;
        if self.values.contains_key(&name) {
            return Err(format!("compiler registry already contains {name:?}"));
        }
        self.values.insert(name.clone(), value);
        self.compile_time_functions.insert(name);
        self.version += 1;
        Ok(())
    }

    pub fn lookup_value(&self, name: &str) -> Result<Option<&RuntimeValue>, String> {
        require_registry_name(name.to_string())?;
        Ok(self.values.get(name))
    }

    pub fn require_value(&self, name: &str) -> Result<&RuntimeValue, String> {
        self.lookup_value(name)?
            .ok_or_else(|| format!("compiler registry does not contain {name:?}"))
    }

    pub fn is_compile_time_function(&self, name: &str) -> Result<bool, String> {
        require_registry_name(name.to_string())?;
        Ok(self.compile_time_functions.contains(name))
    }

    pub fn registered_names(&self) -> Vec<&str> {
        self.values.keys().map(String::as_str).collect()
    }

    pub fn compile_time_function_names(&self) -> Vec<&str> {
        self.compile_time_functions
            .iter()
            .map(String::as_str)
            .collect()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn snapshot(&self) -> CompilerRegistrySnapshot {
        CompilerRegistrySnapshot {
            values: self.values.clone(),
            compile_time_functions: self.compile_time_functions.clone(),
            version: self.version,
        }
    }

    pub fn restore_snapshot(&mut self, snapshot: CompilerRegistrySnapshot) {
        self.values = snapshot.values;
        self.compile_time_functions = snapshot.compile_time_functions;
        self.version = snapshot.version;
    }
}

impl CompilerBridgeValue {
    pub fn from_compiler(compiler: &Compiler) -> Self {
        Self {
            host: compiler.host.clone(),
            registry: RefCell::new(compiler.registry.clone()),
            provider_registry: RefCell::new(compiler.provider_registry.clone()),
            registered_stages: RefCell::new(compiler.registered_stages.clone()),
            units: RefCell::new(compiler.units.clone()),
            module_catalog: RefCell::new(compiler.module_catalog.clone()),
            name_service: RefCell::new(compiler.name_service.clone()),
            semantic_policies: RefCell::new(compiler.semantic_policies.clone()),
            fact_schema: RefCell::new(compiler.fact_schema.clone()),
            language_builtin_bridges: RefCell::new(compiler.language_builtin_bridges.clone()),
            diagnostics: RefCell::new(compiler.diagnostics.clone()),
            artifact_cache: RefCell::new(compiler.artifact_cache.clone()),
            provider_ctfe_cache: RefCell::new(compiler.provider_ctfe_cache.clone()),
            bootstrap_capabilities: RefCell::new(compiler.bootstrap_capabilities.clone()),
            bootstrap_images: RefCell::new(compiler.bootstrap_images.clone()),
            active_provider_context: RefCell::new(compiler.active_provider_context.clone()),
            provider_dynamic_requires: RefCell::new(compiler.provider_dynamic_requires.clone()),
            bootstrap_executions: RefCell::new(compiler.bootstrap_executions.clone()),
            bootstrap_trace: RefCell::new(compiler.bootstrap_trace.clone()),
            bootstrap_execution_memo: RefCell::new(compiler.bootstrap_execution_memo.clone()),
            active_bootstrap_depth: RefCell::new(compiler.active_bootstrap_depth),
            bootstrap_path_stack: RefCell::new(Vec::new()),
            bootstrap_unit_stack: RefCell::new(Vec::new()),
            bootstrap_capability_stack: RefCell::new(Vec::new()),
            source_templates: RefCell::new(compiler.source_templates.clone()),
            explanations: RefCell::new(compiler.explanations.clone()),
            events: RefCell::new(Vec::new()),
            dirty_session: RefCell::new(false),
        }
    }

    pub fn register_value(
        &self,
        name: impl Into<String>,
        value: RuntimeValue,
    ) -> Result<RuntimeValue, String> {
        let name = name.into();
        let registered = self
            .registry
            .borrow_mut()
            .register_value(name.clone(), value)?;
        self.push_event(
            "compiler.registry.value.register",
            Some(name),
            "registered compiler registry value",
            [(
                "registry_version".to_string(),
                self.registry.borrow().version().to_string(),
            )],
        )?;
        Ok(registered)
    }

    pub fn register_compile_time_function(
        &self,
        name: impl Into<String>,
        value: RuntimeValue,
    ) -> Result<(), String> {
        let name = name.into();
        self.registry
            .borrow_mut()
            .register_compile_time_function(name.clone(), value)?;
        self.push_event(
            "compiler.registry.compile-time-function.register",
            Some(name),
            "registered compiler compile-time function",
            [(
                "registry_version".to_string(),
                self.registry.borrow().version().to_string(),
            )],
        )
    }

    pub fn lookup_registered_value(&self, name: &str) -> Result<Option<RuntimeValue>, String> {
        Ok(self.registry.borrow().lookup_value(name)?.cloned())
    }

    pub fn compile_time_function_names(&self) -> Vec<String> {
        self.registry
            .borrow()
            .compile_time_function_names()
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    pub fn lookup_compiled_unit(&self, unit_id: &str) -> Result<Option<UnitBridgeValue>, String> {
        if unit_id.is_empty() {
            return Err("compiler unit id lookup must be non-empty".to_string());
        }
        Ok(self
            .units
            .borrow()
            .get(unit_id)
            .map(UnitBridgeValue::from_unit))
    }

    pub fn emit_event(
        &self,
        component: impl Into<String>,
        action: impl Into<String>,
        message: impl Into<String>,
        fields: impl IntoIterator<Item = (String, String)>,
    ) -> Result<(), String> {
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
    ) -> Result<(), String> {
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
        let stage = spec.name.clone();
        self.provider_registry.borrow_mut().register_stage(spec)?;
        self.registered_stages.borrow_mut().insert(stage);
        *self.dirty_session.borrow_mut() = true;
        Ok(())
    }

    pub fn host_service_export(
        &self,
        library: &str,
        export: &str,
        phase: PhasePolicy,
    ) -> Result<RuntimeValue, String> {
        self.host_service_registry(phase)
            .export(library, export, phase)
    }

    pub fn host_service_libraries(&self, phase: PhasePolicy) -> Vec<String> {
        self.host_service_registry(phase)
            .library_names()
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    pub fn host_service_library_catalog(
        &self,
        library: &str,
        phase: PhasePolicy,
    ) -> Result<Vec<HostServiceExport>, String> {
        let services = self.host_service_registry(phase);
        let library_entry = services
            .library(library)?
            .ok_or_else(|| format!("host service library does not exist: {library}"))?;
        let mut entries = Vec::new();
        for export_name in library_entry.export_names() {
            services.export(library, export_name, phase)?;
            let export = library_entry
                .export(export_name)?
                .expect("export name listed by library must resolve")
                .clone();
            entries.push(export);
        }
        Ok(entries)
    }

    fn host_service_registry(&self, phase: PhasePolicy) -> &HostServiceRegistry {
        match phase {
            PhasePolicy::CompileTime => self.host.compile_time_services(),
            PhasePolicy::Runtime | PhasePolicy::Dual => self.host.runtime_services(),
        }
    }

    pub fn register_stage_alias(
        &self,
        stage: impl Into<String>,
        alias: impl Into<String>,
    ) -> Result<(), String> {
        self.provider_registry
            .borrow_mut()
            .register_alias(stage, alias)?;
        *self.dirty_session.borrow_mut() = true;
        Ok(())
    }

    pub fn register_stage_restart_policy(
        &self,
        stage: impl Into<String>,
        restart_stage: impl Into<String>,
    ) -> Result<(), String> {
        self.provider_registry
            .borrow_mut()
            .register_restart_stage(stage, restart_stage)?;
        *self.dirty_session.borrow_mut() = true;
        Ok(())
    }

    pub fn register_semantic_policy(
        &self,
        mut policy: SemanticPolicyRegistration,
    ) -> Result<(), String> {
        if policy.name.is_empty() {
            return Err("registered semantic policy name must be non-empty".to_string());
        }
        if policy.phase_policy != PhasePolicy::CompileTime {
            return Err(format!(
                "registered semantic policy {:?} must use compile_time phase policy",
                policy.name
            ));
        }
        if policy.eval_policy != EvalPolicy::SpecialForm {
            return Err(format!(
                "registered semantic policy {:?} must use special_form eval policy",
                policy.name
            ));
        }
        if matches!(policy.normalizer, RuntimeValue::Null) {
            return Err(format!(
                "registered semantic policy {:?} requires a normalizer",
                policy.name
            ));
        }
        policy.unit_id.get_or_insert_with(|| "<ctfe>".to_string());
        let name = policy.name.clone();
        self.semantic_policies
            .borrow_mut()
            .insert(name.clone(), policy);
        self.push_event(
            "compiler.semantic-policy.register",
            Some(name),
            "registered compiler semantic policy",
            [],
        )?;
        *self.dirty_session.borrow_mut() = true;
        Ok(())
    }

    pub fn list_semantic_policies(&self) -> Vec<SemanticPolicyRegistration> {
        self.semantic_policies.borrow().values().cloned().collect()
    }

    pub fn describe_semantic_policy(
        &self,
        name: &str,
    ) -> Result<Option<SemanticPolicyRegistration>, String> {
        if name.is_empty() {
            return Err("semantic policy lookup name must be non-empty".to_string());
        }
        Ok(self.semantic_policies.borrow().get(name).cloned())
    }

    pub fn register_fact_schema_type_bridge(
        &self,
        label: impl Into<String>,
        bridge_name: impl Into<String>,
    ) -> Result<(), String> {
        let label = label.into();
        let bridge_name = bridge_name.into();
        self.fact_schema
            .borrow_mut()
            .register_type_bridge(label.clone(), bridge_name.clone())?;
        self.registry.borrow_mut().register_value(
            format!("caap.fact_schema.type_bridge.{label}"),
            RuntimeValue::Str(bridge_name.into()),
        )?;
        self.push_event(
            "compiler.fact-schema.type-bridge.register",
            Some(label),
            "registered compiler fact schema type bridge",
            [],
        )?;
        *self.dirty_session.borrow_mut() = true;
        Ok(())
    }

    pub fn register_fact_schema(
        &self,
        predicate: impl Into<String>,
        type_label: impl Into<String>,
        allow_none: bool,
        description: Option<String>,
    ) -> Result<(), String> {
        let predicate = predicate.into();
        let type_label = type_label.into();
        self.fact_schema.borrow_mut().register_schema(
            predicate.clone(),
            type_label.clone(),
            allow_none,
            description,
        )?;
        self.push_event(
            "compiler.fact-schema.register",
            Some(predicate),
            "registered compiler fact schema",
            [("type".to_string(), type_label)],
        )?;
        *self.dirty_session.borrow_mut() = true;
        Ok(())
    }

    pub fn validate_fact_value(
        &self,
        predicate: &str,
        value: &SemanticValue,
    ) -> Result<(), String> {
        self.fact_schema.borrow().validate_value(predicate, value)
    }

    pub fn fact_schema(&self) -> FactSchemaRegistry {
        self.fact_schema.borrow().clone()
    }

    pub fn register_language_builtin_bridge(&self, name: impl Into<String>) -> Result<(), String> {
        let name = name.into();
        validate_language_builtin_bridge(&name)?;
        if self
            .language_builtin_bridges
            .borrow_mut()
            .insert(name.clone())
        {
            self.push_event(
                "compiler.language-builtin-bridge.register",
                Some(name),
                "registered compiler language builtin bridge",
                [],
            )?;
            *self.dirty_session.borrow_mut() = true;
        }
        Ok(())
    }

    pub fn language_builtin_bridges(&self) -> Vec<String> {
        self.language_builtin_bridges
            .borrow()
            .iter()
            .cloned()
            .collect()
    }

    pub fn register_provider(
        &self,
        name: impl Into<String>,
        target: impl Into<String>,
        callback: RuntimeValue,
        requires: impl IntoIterator<Item = String>,
        effects: impl IntoIterator<Item = String>,
        spec: QueryProviderRegistrationSpec,
    ) -> Result<(), String> {
        let name = name.into();
        let target = target.into();
        let stage = self.provider_registry.borrow().resolve_stage(target)?;
        let family = match spec.family.clone() {
            Some(family) => Some(normalize_stage_name(family)?),
            None => self
                .provider_registry
                .borrow()
                .stage_spec(&stage)?
                .and_then(|stage| stage.family_label.clone()),
        };
        let provider_callback = callback.clone();
        self.provider_registry
            .borrow_mut()
            .register_provider_contract_with_outcome(
                name,
                stage,
                family,
                PhasePolicy::CompileTime,
                requires,
                effects,
                spec,
                move |compiler, unit| {
                    invoke_registered_provider_callback(provider_callback.clone(), compiler, unit)
                },
            )?;
        *self.dirty_session.borrow_mut() = true;
        Ok(())
    }

    pub fn list_stages(&self) -> Vec<QueryStageSpec> {
        self.provider_registry
            .borrow()
            .stages
            .values()
            .cloned()
            .collect()
    }

    pub fn list_providers(
        &self,
        stage_or_target: Option<String>,
    ) -> Result<Vec<QueryProvider>, String> {
        let registry = self.provider_registry.borrow();
        match stage_or_target {
            Some(stage_or_target) => registry.providers_for_stage(stage_or_target),
            None => Ok(registry.ordered_providers()),
        }
    }

    pub fn provider_schedule_for_stage(
        &self,
        stage_or_target: impl Into<String>,
    ) -> Result<QueryProviderSchedule, String> {
        self.provider_schedule_for_stage_with_satisfied(stage_or_target, &BTreeSet::new())
    }

    pub fn provider_schedule_for_stage_with_satisfied(
        &self,
        stage_or_target: impl Into<String>,
        previously_satisfied: &BTreeSet<String>,
    ) -> Result<QueryProviderSchedule, String> {
        let registry = self.provider_registry.borrow();
        let available_data = registry.data_keys_for_satisfied_providers(previously_satisfied);
        registry.provider_schedule_for_stage_with_dynamic_requires(
            stage_or_target,
            available_data,
            previously_satisfied,
            &self.provider_dynamic_requires.borrow(),
        )
    }

    pub fn provider_dynamic_requires_for(&self, provider_name: &str) -> Vec<String> {
        self.provider_dynamic_requires
            .borrow()
            .get(provider_name)
            .cloned()
            .unwrap_or_default()
    }

    fn note_nested_query_artifact_dependency(&self, artifact: &QueryArtifactProjection) {
        let Some(provider_name) = artifact
            .execution_summary
            .last()
            .map(|record| record.provider_name.as_str())
        else {
            return;
        };
        self.note_dynamic_provider_dependency(provider_name);
        if let Some(context) = self.active_provider_context.borrow_mut().as_mut() {
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
    }

    fn note_dynamic_provider_dependency(&self, provider_name: &str) {
        let Some(active) = self.active_provider_context.borrow().as_ref().cloned() else {
            return;
        };
        if provider_name.is_empty() {
            return;
        }
        let mut requires = self.provider_dynamic_requires.borrow_mut();
        let entry = requires.entry(active.provider).or_default();
        if !entry.iter().any(|name| name == provider_name) {
            entry.push(provider_name.to_string());
            entry.sort();
        }
    }

    pub fn cache_stats(&self) -> ArtifactCacheStats {
        self.artifact_cache.borrow().stats().clone()
    }

    pub fn bootstrap_trace(&self) -> Vec<BootstrapTraceEvent> {
        self.bootstrap_trace.borrow().clone()
    }

    pub fn current_bootstrap_path(&self) -> Option<String> {
        self.bootstrap_path_stack.borrow().last().cloned()
    }

    pub fn current_bootstrap_capabilities(&self) -> Vec<String> {
        self.bootstrap_capability_stack
            .borrow()
            .last()
            .cloned()
            .unwrap_or_default()
    }

    pub fn plan_query(
        &self,
        target: impl Into<String>,
        phase: PhasePolicy,
    ) -> Result<QueryPlan, String> {
        self.provider_registry.borrow().plan(target, phase)
    }

    pub fn plan_query_with_source_options(
        &self,
        target: impl Into<String>,
        source: QueryArtifactSource,
        phase: PhasePolicy,
        options: QueryExecutionOptions,
    ) -> Result<QueryPlan, String> {
        let target = target.into();
        let (unit, origin_stage) = self.query_source_unit_and_origin_stage(source)?;
        let initial_bindings = normalize_initial_bindings(options.initial_bindings)?;
        let mut plan = {
            let registry = self.provider_registry.borrow();
            match origin_stage {
                Some(origin_stage) => {
                    registry.plan_from_stage_to_target(origin_stage, target, phase)?
                }
                None => registry.plan(target, phase)?,
            }
        };
        let cache = self.artifact_cache.borrow();
        for step in &mut plan.steps {
            let key = query_stage_cache_key(
                &unit,
                &step.stage,
                phase,
                &initial_bindings,
                self.provider_registry.borrow().version(),
                self.registry.borrow().version(),
                self.host.host_version(),
            )?;
            step.cached = cache.peek(&key).is_some();
            step.artifact_key = Some(key);
        }
        Ok(plan)
    }

    pub fn query_artifact(
        &self,
        target: impl Into<String>,
        source: QueryArtifactSource,
        phase: PhasePolicy,
    ) -> Result<QueryArtifactProjection, String> {
        Ok(self
            .query_execution_projection(target, source, phase)?
            .artifact)
    }

    pub fn query_artifact_with_options(
        &self,
        target: impl Into<String>,
        source: QueryArtifactSource,
        phase: PhasePolicy,
        options: QueryExecutionOptions,
    ) -> Result<QueryArtifactProjection, String> {
        Ok(self
            .query_execution_projection_with_options(target, source, phase, options)?
            .artifact)
    }

    fn query_source_unit_and_origin_stage(
        &self,
        source: QueryArtifactSource,
    ) -> Result<(Unit, Option<String>), String> {
        match source {
            QueryArtifactSource::Unit(unit) => {
                let origin_stage = self
                    .provider_registry
                    .borrow()
                    .stage_for_input_kind("unit")
                    .ok();
                Ok((*unit, origin_stage))
            }
            QueryArtifactSource::Path(path) => {
                let unit = self.load_surface_unit_template(path)?.clone_unit();
                let origin_stage = self
                    .provider_registry
                    .borrow()
                    .stage_for_input_kind("surface")
                    .ok();
                Ok((unit, origin_stage))
            }
            QueryArtifactSource::Text(text) => {
                let unit = self.load_surface_text_unit_template(text)?.clone_unit();
                let origin_stage = self
                    .provider_registry
                    .borrow()
                    .stage_for_input_kind("surface")
                    .ok();
                Ok((unit, origin_stage))
            }
        }
    }

    pub fn query_execution_projection(
        &self,
        target: impl Into<String>,
        source: QueryArtifactSource,
        phase: PhasePolicy,
    ) -> Result<QueryExecutionProjection, String> {
        self.query_execution_projection_with_options(
            target,
            source,
            phase,
            QueryExecutionOptions::default(),
        )
    }

    pub fn query_execution_projection_with_options(
        &self,
        target: impl Into<String>,
        source: QueryArtifactSource,
        phase: PhasePolicy,
        options: QueryExecutionOptions,
    ) -> Result<QueryExecutionProjection, String> {
        let target = target.into();
        let mut unit = match source {
            QueryArtifactSource::Unit(unit) => *unit,
            QueryArtifactSource::Path(path) => self.load_surface_unit_template(path)?.clone_unit(),
            QueryArtifactSource::Text(text) => {
                self.load_surface_text_unit_template(text)?.clone_unit()
            }
        };
        let mut compiler = self.snapshot_compiler();
        let plan = compiler
            .queries()
            .query_with_options(&target, &mut unit, phase, options)?;
        let step = plan
            .steps
            .last()
            .ok_or_else(|| format!("query target {target:?} produced no plan steps"))?;
        let key = step
            .artifact_key
            .clone()
            .ok_or_else(|| format!("query target {target:?} did not produce an artifact key"))?;
        let value = compiler
            .artifact_cache
            .peek(&key)
            .cloned()
            .ok_or_else(|| format!("query target {target:?} did not store artifact {key}"))?;
        let origin_key = compiler.artifact_cache.lineage_id_for_key(&key).cloned();
        let dependencies = compiler
            .artifact_cache
            .dependencies_for(&key)
            .map(|dependencies| dependencies.to_vec())
            .unwrap_or_default();
        let diagnostics = compiler.diagnostics.clone();
        let execution_summary = plan.executed.clone();
        let artifact = QueryArtifactProjection {
            artifact_kind: "query".to_string(),
            stage: step.stage.clone(),
            family: step.stage.clone(),
            phase,
            key,
            origin_key,
            dependencies,
            diagnostics,
            iterations: execution_summary.len(),
            reads_subjects: collect_record_strings(&execution_summary, |record| {
                &record.reads_subjects
            }),
            writes_subjects: collect_record_strings(&execution_summary, |record| {
                &record.writes_subjects
            }),
            read_cells: collect_record_strings(&execution_summary, |record| &record.read_cells),
            write_cells: collect_record_strings(&execution_summary, |record| &record.write_cells),
            reads_files: collect_record_strings(&execution_summary, |record| &record.reads_files),
            writes_files: collect_record_strings(&execution_summary, |record| &record.writes_files),
            execution_summary,
            value,
        };
        let invalidations = plan
            .steps
            .iter()
            .map(|step| query_step_invalidation(&compiler.artifact_cache, step))
            .collect();
        self.restore_from_compiler(compiler);
        self.note_nested_query_artifact_dependency(&artifact);
        Ok(QueryExecutionProjection {
            plan,
            artifact,
            invalidations,
            unit,
        })
    }

    fn snapshot_compiler(&self) -> Compiler {
        let mut compiler = Compiler::new(self.host.clone());
        compiler.registry = self.registry.borrow().clone();
        compiler.provider_registry = self.provider_registry.borrow().clone();
        compiler.registered_stages = self.registered_stages.borrow().clone();
        compiler.units = self.units.borrow().clone();
        compiler.module_catalog = self.module_catalog.borrow().clone();
        compiler.name_service = self.name_service.borrow().clone();
        compiler.semantic_policies = self.semantic_policies.borrow().clone();
        compiler.fact_schema = self.fact_schema.borrow().clone();
        compiler.language_builtin_bridges = self.language_builtin_bridges.borrow().clone();
        compiler.diagnostics = self.diagnostics.borrow().clone();
        compiler.artifact_cache = self.artifact_cache.borrow().clone();
        compiler.provider_ctfe_cache = self.provider_ctfe_cache.borrow().clone();
        compiler.bootstrap_capabilities = self.bootstrap_capabilities.borrow().clone();
        compiler.bootstrap_images = self.bootstrap_images.borrow().clone();
        compiler.active_provider_context = self.active_provider_context.borrow().clone();
        compiler.provider_dynamic_requires = self.provider_dynamic_requires.borrow().clone();
        compiler.bootstrap_executions = self.bootstrap_executions.borrow().clone();
        compiler.bootstrap_trace = self.bootstrap_trace.borrow().clone();
        compiler.bootstrap_execution_memo = self.bootstrap_execution_memo.borrow().clone();
        compiler.active_bootstrap_depth = *self.active_bootstrap_depth.borrow();
        compiler.source_templates = self.source_templates.borrow().clone();
        compiler.explanations = self.explanations.borrow().clone();
        compiler
    }

    fn restore_from_compiler(&self, compiler: Compiler) {
        let events = compiler.events.events().to_vec();
        *self.registry.borrow_mut() = compiler.registry;
        *self.provider_registry.borrow_mut() = compiler.provider_registry;
        *self.registered_stages.borrow_mut() = compiler.registered_stages;
        *self.units.borrow_mut() = compiler.units;
        *self.module_catalog.borrow_mut() = compiler.module_catalog;
        *self.name_service.borrow_mut() = compiler.name_service;
        *self.semantic_policies.borrow_mut() = compiler.semantic_policies;
        *self.fact_schema.borrow_mut() = compiler.fact_schema;
        *self.language_builtin_bridges.borrow_mut() = compiler.language_builtin_bridges;
        *self.diagnostics.borrow_mut() = compiler.diagnostics;
        *self.artifact_cache.borrow_mut() = compiler.artifact_cache;
        *self.provider_ctfe_cache.borrow_mut() = compiler.provider_ctfe_cache;
        *self.bootstrap_capabilities.borrow_mut() = compiler.bootstrap_capabilities;
        *self.bootstrap_images.borrow_mut() = compiler.bootstrap_images;
        *self.active_provider_context.borrow_mut() = compiler.active_provider_context;
        *self.provider_dynamic_requires.borrow_mut() = compiler.provider_dynamic_requires;
        *self.bootstrap_executions.borrow_mut() = compiler.bootstrap_executions;
        *self.bootstrap_trace.borrow_mut() = compiler.bootstrap_trace;
        *self.bootstrap_execution_memo.borrow_mut() = compiler.bootstrap_execution_memo;
        *self.active_bootstrap_depth.borrow_mut() = compiler.active_bootstrap_depth;
        *self.source_templates.borrow_mut() = compiler.source_templates;
        *self.explanations.borrow_mut() = compiler.explanations;
        self.events.borrow_mut().extend(events);
        *self.dirty_session.borrow_mut() = true;
    }

    pub fn load_surface_unit_template(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<UnitBridgeValue, String> {
        let started = Instant::now();
        let resolved = resolve_source_path(path.as_ref())?;
        let text = std::fs::read_to_string(&resolved)
            .map_err(|error| format!("surface file read failed: {error}"))?;
        let source_path = path_to_string(&resolved)?;
        if source_declares_syntax_imports_with_body(&text)? {
            return Err(format!(
                "source {source_path} declares syntax imports and must be loaded through the dynamic syntax loader"
            ));
        }
        let unit_id = source_module_name(&text).unwrap_or_else(|_| source_path.clone());
        let token = source_path_token(&resolved)?;
        let source = SourceArtifact::path(source_path.clone(), token, text)?;
        let template_cache = self.host.source_template_cache.clone();
        let artifact = self.source_templates.borrow_mut().load(
            source,
            "parse",
            PhasePolicy::CompileTime,
            |source| source_artifact_to_template(&template_cache, source, &unit_id),
        )?;
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
        *self.dirty_session.borrow_mut() = true;
        Ok(UnitBridgeValue::from_unit(&Unit::from_template(
            artifact.template,
        )?))
    }

    pub fn load_surface_text_unit_template(
        &self,
        text: impl Into<String>,
    ) -> Result<UnitBridgeValue, String> {
        let started = Instant::now();
        let unit_id = "<inline.caap>".to_string();
        let source = SourceArtifact::inline(text)?;
        let template_cache = self.host.source_template_cache.clone();
        let artifact = self.source_templates.borrow_mut().load(
            source,
            "parse",
            PhasePolicy::CompileTime,
            |source| source_artifact_to_template(&template_cache, source, &unit_id),
        )?;
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
        *self.dirty_session.borrow_mut() = true;
        Ok(UnitBridgeValue::from_unit(&Unit::from_template(
            artifact.template,
        )?))
    }

    pub fn parse_surface_file_forms(
        &self,
        path: impl AsRef<Path>,
        leading_heads: Option<BTreeSet<String>>,
    ) -> Result<Vec<ParsedForm>, String> {
        let resolved = resolve_source_path(path.as_ref())?;
        let text = std::fs::read_to_string(&resolved)
            .map_err(|error| format!("surface file read failed: {error}"))?;
        let source_path = path_to_string(&resolved)?;
        let parsed = parse_forms_with_source_path(&text, &source_path)?;
        let mut forms = Vec::new();
        for form in parsed.forms {
            if let Some(leading_heads) = &leading_heads {
                let head = form.head_symbol();
                if !head.is_some_and(|head| leading_heads.contains(head)) {
                    break;
                }
            }
            forms.push(form);
        }
        Ok(forms)
    }

    pub fn execute_bootstrap_file(
        &self,
        path: impl AsRef<Path>,
        compiler_value: RuntimeValue,
    ) -> Result<RuntimeValue, String> {
        self.execute_bootstrap_file_with_capabilities(
            path,
            self.current_bootstrap_capabilities(),
            compiler_value,
        )
    }

    pub fn execute_bootstrap_file_with_capabilities(
        &self,
        path: impl AsRef<Path>,
        internal_capabilities: impl IntoIterator<Item = String>,
        compiler_value: RuntimeValue,
    ) -> Result<RuntimeValue, String> {
        let resolved = self.resolve_bootstrap_source_path(path.as_ref())?;
        let target = path_to_string(&resolved)?;
        let capabilities = normalize_bootstrap_capabilities(internal_capabilities)?;
        let depth = *self.active_bootstrap_depth.borrow();
        let action = if depth == 0 {
            "bootstrap.raw"
        } else {
            "bootstrap.nested_raw"
        };
        let memo_key = bootstrap_execution_memo_key(action, &target, &capabilities);
        if self.bootstrap_execution_memo.borrow().contains(&memo_key) {
            if depth == 0 {
                self.bootstrap_trace.borrow_mut().push(BootstrapTraceEvent {
                    action: "bootstrap.session_memo".to_string(),
                    target,
                    depth,
                    succeeded: true,
                });
            }
            return Ok(RuntimeValue::Null);
        }
        *self.active_bootstrap_depth.borrow_mut() += 1;
        self.bootstrap_path_stack.borrow_mut().push(target.clone());
        self.bootstrap_capability_stack
            .borrow_mut()
            .push(capabilities);
        let started = Instant::now();
        let result = self.execute_bootstrap_file_inner(&resolved, &target, compiler_value);
        let elapsed_ms = elapsed_ms_string(started);
        self.bootstrap_capability_stack.borrow_mut().pop();
        self.bootstrap_path_stack.borrow_mut().pop();
        *self.active_bootstrap_depth.borrow_mut() -= 1;

        if result.is_ok() {
            self.bootstrap_execution_memo.borrow_mut().insert(memo_key);
        }
        self.bootstrap_trace.borrow_mut().push(BootstrapTraceEvent {
            action: action.to_string(),
            target: target.clone(),
            depth,
            succeeded: result.is_ok(),
        });
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
        *self.dirty_session.borrow_mut() = true;
        result
    }

    pub fn evaluate_bootstrap_file(
        &self,
        path: impl AsRef<Path>,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
        internal_capabilities: impl IntoIterator<Item = String>,
        skip_leading_forms: usize,
        prepare_pipeline: bool,
        compiler_value: RuntimeValue,
    ) -> Result<EvaluationCapture, String> {
        let capabilities = normalize_bootstrap_capabilities(internal_capabilities)?;
        let resolved = self.resolve_bootstrap_source_path(path.as_ref())?;
        let target = path_to_string(&resolved)?;
        let depth = *self.active_bootstrap_depth.borrow();
        *self.active_bootstrap_depth.borrow_mut() += 1;
        self.bootstrap_path_stack.borrow_mut().push(target.clone());
        self.bootstrap_capability_stack
            .borrow_mut()
            .push(capabilities);

        let started = Instant::now();
        let result = self.evaluate_bootstrap_file_inner(
            &resolved,
            initial,
            skip_leading_forms,
            prepare_pipeline,
            compiler_value,
        );
        let elapsed_ms = elapsed_ms_string(started);

        self.bootstrap_capability_stack.borrow_mut().pop();
        self.bootstrap_path_stack.borrow_mut().pop();
        *self.active_bootstrap_depth.borrow_mut() -= 1;

        self.bootstrap_trace.borrow_mut().push(BootstrapTraceEvent {
            action: "bootstrap.evaluate".to_string(),
            target: target.clone(),
            depth,
            succeeded: result.is_ok(),
        });
        self.push_event(
            "bootstrap.evaluate",
            Some(target),
            "evaluated bootstrap source",
            [
                ("depth".to_string(), depth.to_string()),
                ("elapsed_ms".to_string(), elapsed_ms),
                ("prepare_pipeline".to_string(), prepare_pipeline.to_string()),
                ("succeeded".to_string(), result.is_ok().to_string()),
            ],
        )?;
        *self.dirty_session.borrow_mut() = true;
        result
    }

    fn evaluate_bootstrap_file_inner(
        &self,
        resolved: &Path,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
        skip_leading_forms: usize,
        prepare_pipeline: bool,
        compiler_value: RuntimeValue,
    ) -> Result<EvaluationCapture, String> {
        let mut unit = self.load_surface_unit_template(resolved)?.clone_unit();
        let unit_id = unit.unit_id().to_string();
        self.bootstrap_unit_stack.borrow_mut().push(unit_id);
        let mut bindings: Vec<(String, RuntimeValue)> = initial.into_iter().collect();
        bindings.push(("compiler".to_string(), compiler_value));
        if prepare_pipeline {
            self.prepare_unit_for_evaluation(&mut unit, bindings.clone())?;
        }
        let effective_skip = bootstrap_declaration_form_count(&unit).max(skip_leading_forms);
        let result = match evaluate_unit_capture_with_bindings(&unit, bindings, effective_skip) {
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
                self.diagnostics.borrow_mut().push(diagnostic.clone());
                Ok(EvaluationCapture {
                    unit_id: unit.unit_id().to_string(),
                    phase: PhasePolicy::CompileTime,
                    value: None,
                    bindings: Vec::new(),
                    diagnostics: vec![diagnostic],
                    skipped_forms: effective_skip,
                })
            }
            Err(signal) => Err(eval_signal_message(signal)),
        };
        self.bootstrap_unit_stack.borrow_mut().pop();
        result
    }

    fn prepare_unit_for_evaluation(
        &self,
        unit: &mut Unit,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
    ) -> Result<(), String> {
        let (from_stage, target_stage) = {
            let registry = self.provider_registry.borrow();
            if !registry.has_stages() {
                return Ok(());
            }
            (
                registry.stage_for_input_kind("unit")?,
                registry.terminal_stage()?,
            )
        };
        let initial: Vec<(String, RuntimeValue)> = initial.into_iter().collect();
        let mut compiler = self.snapshot_compiler();
        compiler.queries().query_from_stage_to_target_with_options(
            &from_stage,
            &target_stage,
            unit,
            PhasePolicy::CompileTime,
            QueryExecutionOptions::new().with_initial_bindings(initial),
        )?;
        self.restore_from_compiler(compiler);
        Ok(())
    }

    pub(crate) fn resolve_bootstrap_source_path(&self, path: &Path) -> Result<PathBuf, String> {
        if path.is_absolute() {
            return resolve_source_path(path);
        }
        if let Some(current) = self.bootstrap_path_stack.borrow().last() {
            if let Some(parent) = Path::new(current).parent() {
                return resolve_source_path(&parent.join(path));
            }
        }
        resolve_source_path(path)
    }

    fn execute_bootstrap_file_inner(
        &self,
        resolved: &Path,
        target: &str,
        compiler_value: RuntimeValue,
    ) -> Result<RuntimeValue, String> {
        self.bootstrap_executions
            .borrow_mut()
            .push(target.to_string());
        let unit = self.load_surface_unit_template(resolved)?.clone_unit();
        let unit_id = unit.unit_id().to_string();
        let unit_version = unit.version().to_string();
        self.name_service.borrow_mut().register(unit_id.clone())?;
        self.units
            .borrow_mut()
            .insert(unit_id.clone(), unit.clone());
        self.push_event(
            "compiler.unit.register",
            Some(unit_id.clone()),
            "registered compiler unit",
            [("unit_version".to_string(), unit_version)],
        )?;
        self.bootstrap_unit_stack.borrow_mut().push(unit_id);
        let result = evaluate_unit_capture(
            &unit,
            [("compiler".to_string(), compiler_value)],
            bootstrap_declaration_form_count(&unit),
        );
        self.bootstrap_unit_stack.borrow_mut().pop();
        match result {
            Ok(value) => Ok(value),
            Err(EvalSignal::Error(error)) => {
                self.diagnostics
                    .borrow_mut()
                    .push(Diagnostic::from_evaluation_error(&error));
                Err(error.message().to_string())
            }
            Err(signal) => Err(eval_signal_message(signal)),
        }
    }

    pub fn compile_unit(
        &self,
        unit: &Unit,
        raise_on_error: bool,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
    ) -> Result<UnitBridgeValue, String> {
        let initial: Vec<(String, RuntimeValue)> = initial.into_iter().collect();
        if self.registered_stages.borrow().is_empty() {
            let diagnostic =
                Diagnostic::new(DiagnosticSeverity::Error, "no compiler stages registered")?
                    .with_code("CAAP-COMPILER-001")?
                    .add_help("execute an explicit bootstrap before compiling")?;
            self.diagnostics.borrow_mut().push(diagnostic);
            *self.dirty_session.borrow_mut() = true;
            if raise_on_error {
                return Err("no compiler stages registered".to_string());
            }
            return Ok(UnitBridgeValue::from_unit(unit));
        }

        let mut compiler = self.snapshot_compiler();
        let mut compiled_unit = unit.clone();
        let before_diagnostics = compiler.diagnostics.len();
        compiler.queries().query_with_options(
            "compile_unit",
            &mut compiled_unit,
            PhasePolicy::CompileTime,
            QueryExecutionOptions::new().with_initial_bindings(initial),
        )?;
        let unit_id = compiled_unit.unit_id().to_string();
        let unit_version = compiled_unit.version().to_string();
        compiler.emit_compiler_event(
            "compiler.compile",
            Some(unit_id),
            "compiled unit",
            [("unit_version".to_string(), unit_version)],
        );
        let error_message = if raise_on_error {
            compile_error_message(&compiler.diagnostics[before_diagnostics..])
        } else {
            None
        };
        self.restore_from_compiler(compiler);
        if let Some(error_message) = error_message {
            return Err(error_message);
        }
        Ok(UnitBridgeValue::from_unit(&compiled_unit))
    }

    pub fn register_compiled_unit(
        &self,
        module_name: impl Into<String>,
        unit: &Unit,
    ) -> Result<(), String> {
        let module_name = require_registry_name(module_name.into())?;
        let mut unit = unit.clone();
        unit.set_unit_id(module_name.clone())?;
        let unit_version = unit.version().to_string();
        self.name_service
            .borrow_mut()
            .register(module_name.clone())?;
        self.units.borrow_mut().insert(module_name.clone(), unit);
        self.module_catalog
            .borrow_mut()
            .record(module_name.clone(), module_name.clone(), None)?;
        self.push_event(
            "compiler.unit.register",
            Some(module_name.clone()),
            "registered compiler unit",
            [("unit_version".to_string(), unit_version.clone())],
        )?;
        self.push_event(
            "compiler.module.materialize",
            Some(module_name.clone()),
            "recorded module materialization",
            [("unit".to_string(), module_name)],
        )?;
        *self.dirty_session.borrow_mut() = true;
        Ok(())
    }

    pub fn evaluate_capture(
        &self,
        unit: &Unit,
        phase: PhasePolicy,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
        compiler_value: RuntimeValue,
    ) -> Result<EvaluationCapture, String> {
        self.evaluate_capture_skipping(unit, phase, initial, compiler_value, 0)
    }

    pub fn evaluate_capture_skipping(
        &self,
        unit: &Unit,
        phase: PhasePolicy,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
        compiler_value: RuntimeValue,
        skip_leading_forms: usize,
    ) -> Result<EvaluationCapture, String> {
        let mut bindings: Vec<(String, RuntimeValue)> = initial.into_iter().collect();
        bindings.push(("compiler".to_string(), compiler_value));
        match evaluate_unit_capture_with_bindings(unit, bindings, skip_leading_forms) {
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
                self.diagnostics.borrow_mut().push(diagnostic.clone());
                *self.dirty_session.borrow_mut() = true;
                Ok(EvaluationCapture {
                    unit_id: unit.unit_id().to_string(),
                    phase,
                    value: None,
                    bindings: Vec::new(),
                    diagnostics: vec![diagnostic],
                    skipped_forms: skip_leading_forms,
                })
            }
            Err(signal) => Err(eval_signal_message(signal)),
        }
    }

    pub fn register_diagnostic_explanation(
        &self,
        code: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
        help: impl IntoIterator<Item = String>,
    ) -> Result<(), String> {
        let explanation = DiagnosticExplanation::new(code, title, body)?.with_help(help)?;
        self.explanations.borrow_mut().register(explanation);
        *self.dirty_session.borrow_mut() = true;
        Ok(())
    }

    pub fn push_diagnostic(&self, diagnostic: Diagnostic) {
        self.diagnostics.borrow_mut().push(diagnostic);
        *self.dirty_session.borrow_mut() = true;
    }

    fn push_event(
        &self,
        kind: impl Into<String>,
        target: Option<String>,
        message: impl Into<String>,
        metadata: impl IntoIterator<Item = (String, String)>,
    ) -> Result<(), String> {
        let event = CompilerEvent::with_target(kind, target, message, metadata)?;
        if should_emit_live_trace(&event) {
            eprintln!("{}", live_trace_event_line(&event));
        }
        self.events.borrow_mut().push(event);
        Ok(())
    }

    fn apply_to_compiler(&self, compiler: &mut Compiler) {
        compiler.registry = self.registry.borrow().clone();
        compiler.provider_registry = self.provider_registry.borrow().clone();
        compiler.registered_stages = self.registered_stages.borrow().clone();
        compiler.units = self.units.borrow().clone();
        compiler.module_catalog = self.module_catalog.borrow().clone();
        compiler.name_service = self.name_service.borrow().clone();
        compiler.semantic_policies = self.semantic_policies.borrow().clone();
        compiler.fact_schema = self.fact_schema.borrow().clone();
        compiler.language_builtin_bridges = self.language_builtin_bridges.borrow().clone();
        compiler.diagnostics = self.diagnostics.borrow().clone();
        compiler.artifact_cache = self.artifact_cache.borrow().clone();
        compiler.bootstrap_capabilities = self.bootstrap_capabilities.borrow().clone();
        compiler.bootstrap_images = self.bootstrap_images.borrow().clone();
        compiler.active_provider_context = self.active_provider_context.borrow().clone();
        compiler.provider_dynamic_requires = self.provider_dynamic_requires.borrow().clone();
        compiler.bootstrap_executions = self.bootstrap_executions.borrow().clone();
        compiler.bootstrap_trace = self.bootstrap_trace.borrow().clone();
        compiler.active_bootstrap_depth = *self.active_bootstrap_depth.borrow();
        compiler.source_templates = self.source_templates.borrow().clone();
        compiler.explanations = self.explanations.borrow().clone();
        if *self.dirty_session.borrow() {
            compiler.session_version += 1;
        }
        for event in self.events.borrow_mut().drain(..) {
            compiler.emit_event(event);
        }
    }
}

impl HostObject for CompilerBridgeValue {
    fn type_name(&self) -> &'static str {
        "compiler"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl UnitBridgeValue {
    pub fn from_unit(unit: &Unit) -> Self {
        Self {
            unit: RefCell::new(unit.clone()),
        }
    }

    pub fn clone_unit(&self) -> Unit {
        self.unit.borrow().clone()
    }

    pub fn with_unit<R>(&self, f: impl FnOnce(&Unit) -> R) -> R {
        f(&self.unit.borrow())
    }

    pub fn with_unit_mut<R>(&self, f: impl FnOnce(&mut Unit) -> R) -> R {
        f(&mut self.unit.borrow_mut())
    }

    fn apply_to_unit(&self, unit: &mut Unit) {
        *unit = self.unit.borrow().clone();
    }
}

impl HostObject for UnitBridgeValue {
    fn type_name(&self) -> &'static str {
        "unit"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl ProviderContextBridgeValue {
    pub fn new(
        context: QueryProviderContext,
        compiler: Rc<CompilerBridgeValue>,
        unit: Rc<UnitBridgeValue>,
    ) -> Self {
        Self {
            context,
            compiler,
            unit,
            reads_subjects: RefCell::new(BTreeSet::new()),
            writes_subjects: RefCell::new(BTreeSet::new()),
            read_cells: RefCell::new(BTreeSet::new()),
            write_cells: RefCell::new(BTreeSet::new()),
            reads_files: RefCell::new(BTreeSet::new()),
            writes_files: RefCell::new(BTreeSet::new()),
            artifact_dependencies: RefCell::new(BTreeSet::new()),
        }
    }

    pub fn context(&self) -> &QueryProviderContext {
        &self.context
    }

    pub fn compiler(&self) -> Rc<CompilerBridgeValue> {
        Rc::clone(&self.compiler)
    }

    pub fn unit(&self) -> Rc<UnitBridgeValue> {
        Rc::clone(&self.unit)
    }

    pub fn initial_bindings(&self) -> &[(String, RuntimeValue)] {
        &self.context.initial_bindings
    }

    pub fn track_fact_read(&self, node_id: NodeId, namespace: &str) {
        self.track_semantic_read(node_id, namespace);
    }

    pub fn track_fact_write(&self, node_id: NodeId, namespace: &str) {
        self.track_semantic_write(node_id, namespace);
    }

    pub fn track_annotation_read(&self, node_id: NodeId, key: &str) {
        self.track_semantic_read(node_id, &annotation_tracking_predicate(key));
    }

    pub fn track_annotation_write(&self, node_id: NodeId, key: &str) {
        self.track_semantic_write(node_id, &annotation_tracking_predicate(key));
    }

    pub fn track_node_read(&self, node_id: NodeId) {
        self.track_semantic_read(node_id, "$ir");
    }

    pub fn track_node_write(&self, node_id: NodeId) {
        self.track_semantic_write(node_id, "$ir");
    }

    pub fn track_symbol_read(&self, name: &str) {
        if let Ok(subject) = symbol_subject_id(name) {
            let subject = semantic_subject_tracking_key(&subject);
            self.reads_subjects.borrow_mut().insert(subject.clone());
            self.read_cells
                .borrow_mut()
                .insert(semantic_cell_tracking_key(&subject, "symbol.entry"));
        }
    }

    pub fn track_file_read(&self, path: impl Into<String>) {
        let path = path.into();
        if !path.is_empty() {
            self.reads_files.borrow_mut().insert(path);
        }
    }

    pub fn track_file_write(&self, path: impl Into<String>) {
        let path = path.into();
        if !path.is_empty() {
            self.writes_files.borrow_mut().insert(path);
        }
    }

    pub fn absorb_artifact_dependencies(&self, artifact: &QueryArtifactProjection) {
        self.reads_subjects
            .borrow_mut()
            .extend(artifact.reads_subjects.iter().cloned());
        self.read_cells
            .borrow_mut()
            .extend(artifact.read_cells.iter().cloned());
        self.reads_files
            .borrow_mut()
            .extend(artifact.reads_files.iter().cloned());
        self.artifact_dependencies
            .borrow_mut()
            .insert(artifact.key.clone());
        self.artifact_dependencies
            .borrow_mut()
            .extend(artifact.dependencies.iter().cloned());
    }

    pub fn tracked_context(&self) -> QueryProviderContext {
        let mut context = self.context.clone();
        extend_unique(
            &mut context.reads_subjects,
            self.reads_subjects.borrow().iter().cloned(),
        );
        extend_unique(
            &mut context.writes_subjects,
            self.writes_subjects.borrow().iter().cloned(),
        );
        extend_unique(
            &mut context.read_cells,
            self.read_cells.borrow().iter().cloned(),
        );
        extend_unique(
            &mut context.write_cells,
            self.write_cells.borrow().iter().cloned(),
        );
        extend_unique(
            &mut context.reads_files,
            self.reads_files.borrow().iter().cloned(),
        );
        extend_unique(
            &mut context.writes_files,
            self.writes_files.borrow().iter().cloned(),
        );
        extend_unique_artifact_keys(
            &mut context.artifact_dependencies,
            self.artifact_dependencies.borrow().iter().cloned(),
        );
        context
    }

    fn track_semantic_read(&self, node_id: NodeId, predicate: &str) {
        let subject = semantic_subject_tracking_key(&node_subject_id(node_id));
        self.reads_subjects.borrow_mut().insert(subject.clone());
        self.read_cells
            .borrow_mut()
            .insert(semantic_cell_tracking_key(&subject, predicate));
    }

    fn track_semantic_write(&self, node_id: NodeId, predicate: &str) {
        let subject = semantic_subject_tracking_key(&node_subject_id(node_id));
        self.writes_subjects.borrow_mut().insert(subject.clone());
        self.write_cells
            .borrow_mut()
            .insert(semantic_cell_tracking_key(&subject, predicate));
    }
}

impl HostObject for ProviderContextBridgeValue {
    fn type_name(&self) -> &'static str {
        "ProviderContext"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn require_registry_name(name: String) -> Result<String, String> {
    if name.is_empty() {
        return Err("compiler registry names must be non-empty strings".to_string());
    }
    Ok(name)
}

fn validate_language_builtin_bridge(name: &str) -> Result<(), String> {
    match name {
        "core-special" | "core-value" | "data" | "mutable-data" => Ok(()),
        _ => Err(format!(
            "unknown Python language builtin bridge {name:?}; known bridges: \
             core-special, core-value, data, mutable-data"
        )),
    }
}

fn normalize_bootstrap_capabilities(
    capabilities: impl IntoIterator<Item = String>,
) -> Result<Vec<String>, String> {
    let mut normalized = Vec::new();
    for capability in capabilities {
        if capability.is_empty() {
            return Err("bootstrap internal capability name must be non-empty".to_string());
        }
        if capability != "host_services" {
            return Err(format!(
                "unsupported bootstrap internal capability: {capability}; supported capabilities: host_services"
            ));
        }
        if !normalized.contains(&capability) {
            normalized.push(capability);
        }
    }
    normalized.sort();
    Ok(normalized)
}

fn bootstrap_execution_memo_key(action: &str, target: &str, capabilities: &[String]) -> String {
    format!("{action}\0{target}\0{}", capabilities.join("\0"))
}

impl ModuleCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(
        &mut self,
        module_name: impl Into<String>,
        unit_id: impl Into<String>,
        source_path: Option<String>,
    ) -> Result<&ModuleMaterialization, String> {
        let module_name = module_name.into();
        let unit_id = unit_id.into();
        if module_name.is_empty() {
            return Err("module catalog name must be non-empty".to_string());
        }
        if unit_id.is_empty() {
            return Err("module catalog unit id must be non-empty".to_string());
        }
        if source_path.as_ref().is_some_and(|path| path.is_empty()) {
            return Err("module catalog source path must be non-empty when present".to_string());
        }
        self.version += 1;
        self.modules.insert(
            module_name.clone(),
            ModuleMaterialization {
                module_name: module_name.clone(),
                unit_id,
                source_path,
                catalog_version: self.version,
            },
        );
        Ok(self
            .modules
            .get(&module_name)
            .expect("module materialization inserted"))
    }

    pub fn get(&self, module_name: &str) -> Result<Option<&ModuleMaterialization>, String> {
        if module_name.is_empty() {
            return Err("module catalog lookup name must be non-empty".to_string());
        }
        Ok(self.modules.get(module_name))
    }

    pub fn unit_id_for_module(&self, module_name: &str) -> Result<Option<&str>, String> {
        Ok(self
            .get(module_name)?
            .map(|materialization| materialization.unit_id.as_str()))
    }

    pub fn module_names(&self) -> Vec<&str> {
        self.modules.keys().map(String::as_str).collect()
    }

    pub fn materializations(&self) -> Vec<&ModuleMaterialization> {
        self.modules.values().collect()
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

impl BootstrapVirtualFileSystem {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        path: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<(), String> {
        let path = normalize_virtual_path(path.into())?;
        let text = text.into();
        self.files.insert(path, text);
        self.version += 1;
        Ok(())
    }

    pub fn read(&self, path: &str) -> Result<&str, String> {
        let path = normalize_virtual_path(path.to_string())?;
        self.files
            .get(&path)
            .map(String::as_str)
            .ok_or_else(|| format!("virtual bootstrap file does not exist: {path}"))
    }

    pub fn contains(&self, path: &str) -> bool {
        normalize_virtual_path(path.to_string())
            .ok()
            .is_some_and(|path| self.files.contains_key(&path))
    }

    pub fn paths(&self) -> Vec<&str> {
        self.files.keys().map(String::as_str).collect()
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

impl BootstrapCapabilityGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn grant(
        &mut self,
        unit_id: impl Into<String>,
        capability: impl Into<String>,
    ) -> Result<(), String> {
        let unit_id = unit_id.into();
        let capability = capability.into();
        if unit_id.is_empty() {
            return Err("bootstrap capability unit id must be non-empty".to_string());
        }
        if capability.is_empty() {
            return Err("bootstrap capability name must be non-empty".to_string());
        }
        if self.grants.entry(unit_id).or_default().insert(capability) {
            self.version += 1;
        }
        Ok(())
    }

    pub fn grant_many(
        &mut self,
        unit_id: impl Into<String>,
        capabilities: impl IntoIterator<Item = String>,
    ) -> Result<(), String> {
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err("bootstrap capability unit id must be non-empty".to_string());
        }
        for capability in capabilities {
            self.grant(unit_id.clone(), capability)?;
        }
        Ok(())
    }

    pub fn allows(&self, unit_id: &str, capability: &str) -> bool {
        self.grants.get(unit_id).is_some_and(|capabilities| {
            capabilities.contains("*")
                || capabilities.contains(capability)
                || capability
                    .split_once('.')
                    .is_some_and(|(library, _)| capabilities.contains(&format!("{library}.*")))
        })
    }

    pub fn require(&self, unit_id: &str, capability: &str) -> Result<(), String> {
        if unit_id.is_empty() {
            return Err("bootstrap capability unit id must be non-empty".to_string());
        }
        if capability.is_empty() {
            return Err("bootstrap capability name must be non-empty".to_string());
        }
        if self.allows(unit_id, capability) {
            Ok(())
        } else {
            Err(format!(
                "bootstrap capability denied for {unit_id}: {capability}"
            ))
        }
    }

    pub fn capabilities_for(&self, unit_id: &str) -> Vec<&str> {
        self.grants
            .get(unit_id)
            .map(|capabilities| capabilities.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    pub fn unit_ids(&self) -> Vec<&str> {
        self.grants.keys().map(String::as_str).collect()
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

impl BootstrapImage {
    pub fn unit_ids(&self) -> Vec<&str> {
        self.units
            .iter()
            .map(|unit| unit.unit_id.as_str())
            .collect()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() {
            return Err("bootstrap image name must be non-empty".to_string());
        }
        self.fact_schema.validate()?;
        let mut language_builtin_bridges = BTreeSet::new();
        for bridge in &self.language_builtin_bridges {
            validate_language_builtin_bridge(bridge)?;
            if !language_builtin_bridges.insert(bridge) {
                return Err(format!(
                    "bootstrap image contains duplicate language builtin bridge: {bridge}"
                ));
            }
        }
        Ok(())
    }
}

impl BootstrapImageFile {
    pub const FORMAT_NAME: &'static str = "caap-rust-bootstrap-image";
    pub const FORMAT_VERSION: u32 = 1;

    pub fn new(image: BootstrapImage) -> Self {
        Self {
            format_name: Self::FORMAT_NAME.to_string(),
            format_version: Self::FORMAT_VERSION,
            image,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.format_name != Self::FORMAT_NAME {
            return Err("bootstrap image file format name is unsupported".to_string());
        }
        if self.format_version != Self::FORMAT_VERSION {
            return Err("bootstrap image file format version is unsupported".to_string());
        }
        self.image.validate()?;
        Ok(())
    }

    pub fn to_json_string(&self) -> Result<String, String> {
        self.validate()?;
        serde_json::to_string_pretty(self)
            .map_err(|error| format!("failed to serialize bootstrap image file: {error}"))
    }

    pub fn from_json_str(text: &str) -> Result<Self, String> {
        let image_file: Self = serde_json::from_str(text)
            .map_err(|error| format!("failed to deserialize bootstrap image file: {error}"))?;
        image_file.validate()?;
        Ok(image_file)
    }

    pub fn write_json_file(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        fs::write(path, self.to_json_string()?).map_err(|error| {
            format!(
                "failed to write bootstrap image file {}: {error}",
                path.display()
            )
        })
    }

    pub fn read_json_file(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|error| {
            format!(
                "failed to read bootstrap image file {}: {error}",
                path.display()
            )
        })?;
        Self::from_json_str(&text)
    }
}

impl BootstrapImageTrustPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_trusted_fingerprint(
        mut self,
        fingerprint: impl Into<String>,
    ) -> Result<Self, String> {
        self.trust_fingerprint(fingerprint)?;
        Ok(self)
    }

    pub fn trust_fingerprint(&mut self, fingerprint: impl Into<String>) -> Result<(), String> {
        let fingerprint = fingerprint.into();
        if fingerprint.is_empty() {
            return Err("bootstrap image trusted fingerprint must be non-empty".to_string());
        }
        self.trusted_fingerprints.insert(fingerprint);
        Ok(())
    }

    pub fn trust_file(&mut self, path: impl AsRef<Path>) -> Result<String, String> {
        let fingerprint = bootstrap_image_file_fingerprint(path)?;
        self.trust_fingerprint(fingerprint.clone())?;
        Ok(fingerprint)
    }

    pub fn is_trusted_fingerprint(&self, fingerprint: &str) -> bool {
        self.trusted_fingerprints.contains(fingerprint)
    }

    pub fn require_fingerprint(&self, fingerprint: &str) -> Result<(), String> {
        if self.is_trusted_fingerprint(fingerprint) {
            Ok(())
        } else {
            Err(format!(
                "bootstrap image file fingerprint is not trusted: {fingerprint}"
            ))
        }
    }

    pub fn trusted_fingerprints(&self) -> Vec<&str> {
        self.trusted_fingerprints
            .iter()
            .map(String::as_str)
            .collect()
    }
}

impl BootstrapImageStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn store(&mut self, image: BootstrapImage) -> Result<(), String> {
        image.validate()?;
        self.images.insert(image.name.clone(), image);
        self.version += 1;
        Ok(())
    }

    pub fn get(&self, name: &str) -> Result<Option<&BootstrapImage>, String> {
        if name.is_empty() {
            return Err("bootstrap image name must be non-empty".to_string());
        }
        Ok(self.images.get(name))
    }

    pub fn image_file(&self, name: &str) -> Result<BootstrapImageFile, String> {
        let image = self
            .get(name)?
            .cloned()
            .ok_or_else(|| format!("bootstrap image does not exist: {name}"))?;
        Ok(BootstrapImageFile::new(image))
    }

    pub fn restore_image_file(&mut self, image_file: BootstrapImageFile) -> Result<(), String> {
        image_file.validate()?;
        self.store(image_file.image)
    }

    pub fn save_image_file(&self, name: &str, path: impl AsRef<Path>) -> Result<(), String> {
        self.image_file(name)?.write_json_file(path)
    }

    pub fn load_image_file(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        self.restore_image_file(BootstrapImageFile::read_json_file(path)?)
    }

    pub fn image_names(&self) -> Vec<&str> {
        self.images.keys().map(String::as_str).collect()
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

impl Compiler {
    pub fn new(host: Rc<CompilerHost>) -> Self {
        Self {
            host,
            units: BTreeMap::new(),
            registry: CompilerRegistry::new(),
            module_catalog: ModuleCatalog::new(),
            diagnostics: Vec::new(),
            events: CompilerEventLog::new(),
            explanations: DiagnosticExplanationRegistry::new(),
            artifact_cache: ArtifactCache::new(),
            source_templates: SourceTemplateCache::new(),
            name_service: CompilerNameService::new(),
            semantic_policies: BTreeMap::new(),
            fact_schema: FactSchemaRegistry::new(),
            language_builtin_bridges: BTreeSet::new(),
            registered_stages: BTreeSet::new(),
            provider_registry: QueryProviderRegistry::new(),
            bootstrap_capabilities: BootstrapCapabilityGraph::new(),
            bootstrap_images: BootstrapImageStore::new(),
            provider_ctfe_cache: BTreeMap::new(),
            active_provider_context: None,
            provider_dynamic_requires: BTreeMap::new(),
            pending_query_restart: None,
            bootstrap_executions: Vec::new(),
            bootstrap_trace: Vec::new(),
            bootstrap_execution_memo: BTreeSet::new(),
            active_bootstrap_depth: 0,
            session_version: 0,
        }
    }

    pub fn host(&self) -> &CompilerHost {
        &self.host
    }

    pub fn units(&self) -> &BTreeMap<String, Unit> {
        &self.units
    }

    pub fn get_unit(&self, unit_id: &str) -> Result<Option<&Unit>, String> {
        if unit_id.is_empty() {
            return Err("compiler unit id lookup must be non-empty".to_string());
        }
        Ok(self.units.get(unit_id))
    }

    pub fn register_unit(&mut self, unit: Unit) -> Result<(), String> {
        let unit_id = unit.unit_id().to_string();
        if unit_id.is_empty() {
            return Err("compiler unit id must be non-empty".to_string());
        }
        let unit_version = unit.version().to_string();
        self.name_service.register(unit_id.clone())?;
        self.units.insert(unit_id.clone(), unit);
        self.session_version += 1;
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
        );
        Ok(())
    }

    pub fn registry(&self) -> &CompilerRegistry {
        &self.registry
    }

    pub fn fact_schema(&self) -> &FactSchemaRegistry {
        &self.fact_schema
    }

    pub fn language_builtin_bridges(&self) -> Vec<&str> {
        self.language_builtin_bridges
            .iter()
            .map(String::as_str)
            .collect()
    }

    pub fn registry_snapshot(&self) -> CompilerRegistrySnapshot {
        self.registry.snapshot()
    }

    pub fn restore_registry_snapshot(
        &mut self,
        snapshot: CompilerRegistrySnapshot,
    ) -> Result<(), String> {
        self.registry.restore_snapshot(snapshot);
        self.session_version += 1;
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
        );
        Ok(())
    }

    pub fn register_value(
        &mut self,
        name: impl Into<String>,
        value: RuntimeValue,
    ) -> Result<RuntimeValue, String> {
        let name = name.into();
        let registered = self.registry.register_value(name.clone(), value)?;
        self.session_version += 1;
        self.emit_compiler_event(
            "compiler.registry.value.register",
            Some(name),
            "registered compiler registry value",
            [(
                "registry_version".to_string(),
                self.registry.version().to_string(),
            )],
        );
        Ok(registered)
    }

    pub fn register_compile_time_function(
        &mut self,
        name: impl Into<String>,
        value: RuntimeValue,
    ) -> Result<(), String> {
        let name = name.into();
        self.registry
            .register_compile_time_function(name.clone(), value)?;
        self.session_version += 1;
        self.emit_compiler_event(
            "compiler.registry.compile-time-function.register",
            Some(name),
            "registered compiler compile-time function",
            [(
                "registry_version".to_string(),
                self.registry.version().to_string(),
            )],
        );
        Ok(())
    }

    pub fn lookup_registered_value(&self, name: &str) -> Result<Option<&RuntimeValue>, String> {
        self.registry.lookup_value(name)
    }

    pub fn require_registered_value(&self, name: &str) -> Result<&RuntimeValue, String> {
        self.registry.require_value(name)
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn push_diagnostic(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
        self.session_version += 1;
    }

    pub fn emit_event(&mut self, event: CompilerEvent) {
        self.events.emit(event);
        self.session_version += 1;
    }

    pub fn events(&self) -> &CompilerEventLog {
        &self.events
    }

    pub fn explanations(&self) -> &DiagnosticExplanationRegistry {
        &self.explanations
    }

    pub fn explanations_mut(&mut self) -> &mut DiagnosticExplanationRegistry {
        self.session_version += 1;
        &mut self.explanations
    }

    pub fn artifact_cache(&self) -> &ArtifactCache {
        &self.artifact_cache
    }

    pub fn artifact_cache_mut(&mut self) -> &mut ArtifactCache {
        &mut self.artifact_cache
    }

    pub fn save_artifact_cache_file(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        self.artifact_cache.save_cache_file(path)?;
        self.emit_compiler_event(
            "compiler.artifact-cache.save",
            Some(path.display().to_string()),
            "saved artifact cache file",
            [
                (
                    "generation".to_string(),
                    self.artifact_cache.stats().generation.to_string(),
                ),
                ("path".to_string(), path.display().to_string()),
            ],
        );
        Ok(())
    }

    pub fn load_artifact_cache_file(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        self.artifact_cache.load_cache_file(path)?;
        self.emit_compiler_event(
            "compiler.artifact-cache.load",
            Some(path.display().to_string()),
            "loaded artifact cache file",
            [
                (
                    "generation".to_string(),
                    self.artifact_cache.stats().generation.to_string(),
                ),
                ("path".to_string(), path.display().to_string()),
            ],
        );
        Ok(())
    }

    pub fn source_templates(&self) -> &SourceTemplateCache {
        &self.source_templates
    }

    pub fn name_service(&self) -> &CompilerNameService {
        &self.name_service
    }

    pub fn catalog(&self) -> CompilerCatalog<'_> {
        CompilerCatalog { units: &self.units }
    }

    pub fn module_catalog(&self) -> &ModuleCatalog {
        &self.module_catalog
    }

    pub fn record_module_materialization(
        &mut self,
        module_name: impl Into<String>,
        unit_id: impl Into<String>,
        source_path: Option<String>,
    ) -> Result<(), String> {
        let module_name = module_name.into();
        let unit_id = unit_id.into();
        if !self.units.contains_key(&unit_id) {
            return Err(format!(
                "module materialization unit is not registered: {unit_id}"
            ));
        }
        let catalog_version = self
            .module_catalog
            .record(module_name.clone(), unit_id.clone(), source_path)?
            .catalog_version;
        self.emit_compiler_event(
            "compiler.module.materialize",
            Some(module_name),
            "recorded module materialization",
            [
                ("catalog_version".to_string(), catalog_version.to_string()),
                ("unit".to_string(), unit_id),
            ],
        );
        Ok(())
    }

    pub fn discover_package_file(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<PackageDescriptor, String> {
        let resolved = resolve_source_path(path.as_ref())?;
        let source_path = path_to_string(&resolved)?;
        let text = fs::read_to_string(&resolved)
            .map_err(|error| format!("package file read failed: {error}"))?;
        parse_package_declarations(&text, &source_path)
    }

    pub fn discover_package_file_or_none(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<Option<PackageDescriptor>, String> {
        let resolved = resolve_source_path(path.as_ref())?;
        let source_path = path_to_string(&resolved)?;
        let text = fs::read_to_string(&resolved)
            .map_err(|error| format!("package file read failed: {error}"))?;
        parse_package_declarations_or_none(&text, &source_path)
    }

    pub fn collect_source_module_name(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<Option<String>, String> {
        Ok(self
            .discover_package_file_or_none(path)?
            .map(|descriptor| descriptor.name))
    }

    pub fn collect_source_imports(&self, path: impl AsRef<Path>) -> Result<Vec<String>, String> {
        let Some(descriptor) = self.discover_package_file_or_none(path)? else {
            return Ok(Vec::new());
        };
        Ok(package_dependency_module_names(&descriptor.imports))
    }

    pub fn collect_source_syntax_imports(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<Vec<String>, String> {
        let Some(descriptor) = self.discover_package_file_or_none(path)? else {
            return Ok(Vec::new());
        };
        Ok(package_dependency_module_names(&descriptor.syntax_imports))
    }

    pub fn materialize_package_file(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<ModuleMaterialization, String> {
        let resolved = resolve_source_path(path.as_ref())?;
        let source_path = path_to_string(&resolved)?;
        let text = fs::read_to_string(&resolved)
            .map_err(|error| format!("package file read failed: {error}"))?;
        let descriptor = parse_package_declarations(&text, &source_path)?;
        let module_name = descriptor.name;
        let template = self
            .load_surface_path_template(&resolved, module_name.clone())?
            .template;
        let unit = Unit::from_template(template)?;
        self.register_unit(unit)?;
        self.record_module_materialization(
            module_name.clone(),
            module_name.clone(),
            Some(source_path.clone()),
        )?;
        self.emit_compiler_event(
            "compiler.package.materialize",
            Some(module_name.clone()),
            "materialized CAAP package source",
            [
                ("path".to_string(), source_path),
                ("unit".to_string(), module_name.clone()),
            ],
        );
        self.module_catalog
            .get(&module_name)?
            .cloned()
            .ok_or_else(|| format!("module materialization was not recorded: {module_name}"))
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
        &self.provider_registry
    }

    pub fn active_provider_context(&self) -> Option<&QueryProviderContext> {
        self.active_provider_context.as_ref()
    }

    pub fn request_query_restart(&mut self, stage: impl Into<String>) -> Result<(), String> {
        let stage = self.provider_registry.resolve_stage(stage.into())?;
        self.pending_query_restart = Some(stage);
        self.session_version += 1;
        Ok(())
    }

    pub fn session_version(&self) -> u64 {
        self.session_version
    }

    pub fn has_bootstrap_executions(&self) -> bool {
        !self.bootstrap_executions.is_empty()
    }

    pub fn bootstrap_executions(&self) -> &[String] {
        &self.bootstrap_executions
    }

    pub fn bootstrap_trace(&self) -> &[BootstrapTraceEvent] {
        &self.bootstrap_trace
    }

    pub fn bootstrap_capabilities(&self) -> &BootstrapCapabilityGraph {
        &self.bootstrap_capabilities
    }

    pub fn bootstrap_images(&self) -> &BootstrapImageStore {
        &self.bootstrap_images
    }

    pub fn store_bootstrap_image(
        &mut self,
        name: impl Into<String>,
    ) -> Result<BootstrapImage, String> {
        let name = name.into();
        if name.is_empty() {
            return Err("bootstrap image name must be non-empty".to_string());
        }
        let image = BootstrapImage {
            name,
            units: self.units.values().map(Unit::to_template).collect(),
            capabilities: self.bootstrap_capabilities.clone(),
            fact_schema: self.fact_schema.clone(),
            language_builtin_bridges: self.language_builtin_bridges.iter().cloned().collect(),
            session_version: self.session_version,
        };
        self.bootstrap_images.store(image.clone())?;
        self.session_version += 1;
        Ok(image)
    }

    pub fn restore_bootstrap_image(&mut self, name: &str) -> Result<(), String> {
        let image = self
            .bootstrap_images
            .get(name)?
            .cloned()
            .ok_or_else(|| format!("bootstrap image does not exist: {name}"))?;
        let mut units = BTreeMap::new();
        for template in image.units {
            let unit = Unit::from_template(template)?;
            units.insert(unit.unit_id().to_string(), unit);
        }
        self.units = units;
        self.bootstrap_capabilities = image.capabilities;
        self.fact_schema = image.fact_schema;
        self.language_builtin_bridges = image.language_builtin_bridges.into_iter().collect();
        self.session_version += 1;
        Ok(())
    }

    pub fn save_bootstrap_image_file(
        &mut self,
        name: &str,
        path: impl AsRef<Path>,
    ) -> Result<(), String> {
        let path = path.as_ref();
        self.bootstrap_images.save_image_file(name, path)?;
        self.emit_compiler_event(
            "bootstrap.image.save",
            Some(name.to_string()),
            "saved bootstrap image file",
            [("path".to_string(), path.display().to_string())],
        );
        Ok(())
    }

    pub fn load_bootstrap_image_file(&mut self, path: impl AsRef<Path>) -> Result<String, String> {
        let path = path.as_ref();
        let image_file = BootstrapImageFile::read_json_file(path)?;
        let image_name = image_file.image.name.clone();
        self.bootstrap_images.restore_image_file(image_file)?;
        self.emit_compiler_event(
            "bootstrap.image.load",
            Some(image_name.clone()),
            "loaded bootstrap image file",
            [("path".to_string(), path.display().to_string())],
        );
        Ok(image_name)
    }

    pub fn load_trusted_bootstrap_image_file(
        &mut self,
        path: impl AsRef<Path>,
        trust_policy: &BootstrapImageTrustPolicy,
    ) -> Result<String, String> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|error| {
            format!(
                "failed to read bootstrap image file {}: {error}",
                path.display()
            )
        })?;
        let fingerprint = ArtifactFingerprint::sha256(text.as_bytes()).to_string();
        trust_policy.require_fingerprint(&fingerprint)?;
        let image_file = BootstrapImageFile::from_json_str(&text)?;
        let image_name = image_file.image.name.clone();
        self.bootstrap_images.restore_image_file(image_file)?;
        self.emit_compiler_event(
            "bootstrap.image.load",
            Some(image_name.clone()),
            "loaded trusted bootstrap image file",
            [
                ("fingerprint".to_string(), fingerprint),
                ("path".to_string(), path.display().to_string()),
                ("trusted".to_string(), "true".to_string()),
            ],
        );
        Ok(image_name)
    }

    pub fn record_bootstrap_execution(&mut self, path: impl Into<String>) -> Result<(), String> {
        let path = path.into();
        if path.is_empty() {
            return Err("bootstrap execution path must be non-empty".to_string());
        }
        self.bootstrap_executions.push(path);
        self.session_version += 1;
        Ok(())
    }

    fn push_bootstrap_trace(
        &mut self,
        action: impl Into<String>,
        target: impl Into<String>,
        depth: usize,
        succeeded: bool,
    ) -> Result<(), String> {
        let action = action.into();
        let target = target.into();
        if action.is_empty() {
            return Err("bootstrap trace action must be non-empty".to_string());
        }
        if target.is_empty() {
            return Err("bootstrap trace target must be non-empty".to_string());
        }
        self.bootstrap_trace.push(BootstrapTraceEvent {
            action,
            target,
            depth,
            succeeded,
        });
        Ok(())
    }

    fn emit_compiler_event(
        &mut self,
        kind: impl Into<String>,
        target: Option<String>,
        message: impl Into<String>,
        metadata: impl IntoIterator<Item = (String, String)>,
    ) {
        if let Ok(event) = CompilerEvent::with_target(kind, target, message, metadata) {
            if should_emit_live_trace(&event) {
                eprintln!("{}", live_trace_event_line(&event));
            }
            self.emit_event(event);
        }
    }

    pub fn register_stage(&mut self, stage: impl Into<String>) -> Result<(), String> {
        let stage = normalize_stage_name(stage.into())?;
        self.provider_registry
            .register_stage(QueryStageSpec::new(stage.clone())?)?;
        if self.registered_stages.insert(stage) {
            self.session_version += 1;
        }
        Ok(())
    }

    pub fn register_stage_spec(&mut self, spec: QueryStageSpec) -> Result<(), String> {
        let stage = normalize_stage_name(spec.name.clone())?;
        self.provider_registry.register_stage(spec)?;
        if self.registered_stages.insert(stage) {
            self.session_version += 1;
        }
        Ok(())
    }

    pub fn register_stage_alias(
        &mut self,
        stage: impl Into<String>,
        alias: impl Into<String>,
    ) -> Result<(), String> {
        self.provider_registry.register_alias(stage, alias)?;
        self.session_version += 1;
        Ok(())
    }

    pub fn register_stage_restart_policy(
        &mut self,
        stage: impl Into<String>,
        restart_stage: impl Into<String>,
    ) -> Result<(), String> {
        self.provider_registry
            .register_restart_stage(stage, restart_stage)?;
        self.session_version += 1;
        Ok(())
    }

    pub fn registered_stages(&self) -> Vec<&str> {
        self.registered_stages.iter().map(String::as_str).collect()
    }

    pub fn register_provider(
        &mut self,
        name: impl Into<String>,
        stage: impl Into<String>,
        phase_policy: PhasePolicy,
        callback: impl Fn(&mut Compiler, &mut Unit) -> Result<(), String> + 'static,
    ) -> Result<(), String> {
        self.provider_registry
            .register_provider(name, stage, phase_policy, callback)?;
        self.session_version += 1;
        Ok(())
    }

    pub fn register_provider_with_effects(
        &mut self,
        name: impl Into<String>,
        stage: impl Into<String>,
        phase_policy: PhasePolicy,
        effect_tags: impl IntoIterator<Item = String>,
        callback: impl Fn(&mut Compiler, &mut Unit) -> Result<(), String> + 'static,
    ) -> Result<(), String> {
        self.provider_registry.register_provider_with_effects(
            name,
            stage,
            phase_policy,
            effect_tags,
            callback,
        )?;
        self.session_version += 1;
        Ok(())
    }

    pub fn register_provider_contract(
        &mut self,
        name: impl Into<String>,
        stage: impl Into<String>,
        family: Option<String>,
        phase_policy: PhasePolicy,
        requires: impl IntoIterator<Item = String>,
        effect_tags: impl IntoIterator<Item = String>,
        spec: QueryProviderRegistrationSpec,
        callback: impl Fn(&mut Compiler, &mut Unit) -> Result<(), String> + 'static,
    ) -> Result<(), String> {
        self.provider_registry.register_provider_contract(
            name,
            stage,
            family,
            phase_policy,
            requires,
            effect_tags,
            spec,
            callback,
        )?;
        self.session_version += 1;
        Ok(())
    }

    pub fn register_provider_contract_with_outcome(
        &mut self,
        name: impl Into<String>,
        stage: impl Into<String>,
        family: Option<String>,
        phase_policy: PhasePolicy,
        requires: impl IntoIterator<Item = String>,
        effect_tags: impl IntoIterator<Item = String>,
        spec: QueryProviderRegistrationSpec,
        callback: impl Fn(&mut Compiler, &mut Unit) -> Result<QueryProviderCallbackOutcome, String>
            + 'static,
    ) -> Result<(), String> {
        self.provider_registry
            .register_provider_contract_with_outcome(
                name,
                stage,
                family,
                phase_policy,
                requires,
                effect_tags,
                spec,
                callback,
            )?;
        self.session_version += 1;
        Ok(())
    }

    pub fn load_surface_text_template(
        &mut self,
        text: impl Into<String>,
        unit_id: impl Into<String>,
    ) -> Result<SourceTemplateArtifact, String> {
        let started = Instant::now();
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err("surface text unit id must be non-empty".to_string());
        }
        let source = SourceArtifact::inline(text)?;
        let template_cache = self.host.source_template_cache.clone();
        let artifact =
            self.source_templates
                .load(source, "parse", PhasePolicy::CompileTime, |source| {
                    source_artifact_to_template(&template_cache, source, &unit_id)
                })?;
        self.emit_source_template_event("inline", &unit_id, &artifact, elapsed_ms_string(started));
        Ok(artifact)
    }

    pub fn load_surface_path_template(
        &mut self,
        path: impl AsRef<Path>,
        unit_id: impl Into<String>,
    ) -> Result<SourceTemplateArtifact, String> {
        let started = Instant::now();
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err("surface path unit id must be non-empty".to_string());
        }
        let resolved = resolve_source_path(path.as_ref())?;
        let text = std::fs::read_to_string(&resolved)
            .map_err(|error| format!("surface file read failed: {error}"))?;
        let source_path = path_to_string(&resolved)?;
        if source_declares_syntax_imports_with_body(&text)? {
            return Err(format!(
                "source {source_path} declares syntax imports and must be loaded through the dynamic syntax loader"
            ));
        }
        let token = source_path_token(&resolved)?;
        let source = SourceArtifact::path(source_path, token, text)?;
        let template_cache = self.host.source_template_cache.clone();
        let artifact =
            self.source_templates
                .load(source, "parse", PhasePolicy::CompileTime, |source| {
                    source_artifact_to_template(&template_cache, source, &unit_id)
                })?;
        self.emit_source_template_event("path", &unit_id, &artifact, elapsed_ms_string(started));
        Ok(artifact)
    }

    pub fn load_surface_virtual_template(
        &mut self,
        path: impl Into<String>,
        text: impl Into<String>,
        unit_id: impl Into<String>,
    ) -> Result<SourceTemplateArtifact, String> {
        let started = Instant::now();
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err("surface virtual unit id must be non-empty".to_string());
        }
        let path = normalize_virtual_path(path.into())?;
        let text = text.into();
        let token = ArtifactFingerprint::sha256(text.as_bytes()).to_string();
        let source = SourceArtifact::path(format!("vfs:{path}"), token, text)?;
        let template_cache = self.host.source_template_cache.clone();
        let artifact =
            self.source_templates
                .load(source, "parse", PhasePolicy::CompileTime, |source| {
                    source_artifact_to_template(&template_cache, source, &unit_id)
                })?;
        self.emit_source_template_event("vfs", &unit_id, &artifact, elapsed_ms_string(started));
        Ok(artifact)
    }

    fn emit_source_template_event(
        &mut self,
        origin: &str,
        unit_id: &str,
        artifact: &SourceTemplateArtifact,
        elapsed_ms: String,
    ) {
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
        );
    }

    pub fn compile(&mut self, unit: &mut Unit) -> Result<(), String> {
        if self.registered_stages.is_empty() {
            let mut diagnostic =
                Diagnostic::new(DiagnosticSeverity::Error, "no compiler stages registered")?;
            diagnostic.code = Some("CAAP-COMPILER-001".to_string());
            diagnostic
                .help
                .push("execute an explicit bootstrap before compiling".to_string());
            self.push_diagnostic(diagnostic);
            return Err("no compiler stages registered".to_string());
        }
        self.register_unit(unit.clone())?;
        self.emit_compiler_event(
            "compiler.compile",
            Some(unit.unit_id().to_string()),
            "compiled unit",
            [("unit_version".to_string(), unit.version().to_string())],
        );
        Ok(())
    }
}

impl QueryStageSpec {
    pub fn new(name: impl Into<String>) -> Result<Self, String> {
        let name = normalize_stage_name(name.into())?;
        Ok(Self {
            name,
            requires: Vec::new(),
            phase_policy: PhasePolicy::CompileTime,
            input_kinds: Vec::new(),
            family_label: None,
            aliases: Vec::new(),
            restart_stage: None,
        })
    }

    pub fn with_requires(
        mut self,
        requires: impl IntoIterator<Item = String>,
    ) -> Result<Self, String> {
        self.requires = normalize_unique_labels(requires, "compiler stage dependency")?;
        Ok(self)
    }

    pub fn with_aliases(
        mut self,
        aliases: impl IntoIterator<Item = String>,
    ) -> Result<Self, String> {
        self.aliases = normalize_unique_labels(aliases, "compiler stage alias")?;
        Ok(self)
    }

    pub fn with_input_kinds(
        mut self,
        input_kinds: impl IntoIterator<Item = String>,
    ) -> Result<Self, String> {
        self.input_kinds = normalize_unique_labels(input_kinds, "compiler stage input kind")?;
        Ok(self)
    }

    pub fn with_family_label(mut self, family_label: impl Into<String>) -> Result<Self, String> {
        self.family_label = Some(normalize_stage_name(family_label.into())?);
        Ok(self)
    }

    pub fn with_restart_stage(mut self, restart_stage: impl Into<String>) -> Result<Self, String> {
        self.restart_stage = Some(normalize_stage_name(restart_stage.into())?);
        Ok(self)
    }
}

impl fmt::Debug for QueryProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueryProvider")
            .field("name", &self.name)
            .field("stage", &self.stage)
            .field("family", &self.family)
            .field("phase_policy", &self.phase_policy)
            .field("requires", &self.requires)
            .field("requires_data", &self.requires_data)
            .field("provides_data", &self.provides_data)
            .field("provides", &self.provides)
            .field("effect_tags", &self.effect_tags)
            .field("input_schema", &self.input_schema)
            .field("reads", &self.reads)
            .field("writes", &self.writes)
            .field("cache_scope", &self.cache_scope)
            .field("resume_policy", &self.resume_policy)
            .field("registration_index", &self.registration_index)
            .finish_non_exhaustive()
    }
}

impl QueryProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_stages(&self) -> bool {
        !self.stages.is_empty()
    }

    pub fn register_stage(&mut self, spec: QueryStageSpec) -> Result<(), String> {
        let name = normalize_stage_name(spec.name.clone())?;
        let requires = normalize_unique_labels(spec.requires, "compiler stage dependency")?;
        let aliases = normalize_unique_labels(spec.aliases, "compiler stage alias")?;
        let input_kinds = normalize_unique_labels(spec.input_kinds, "compiler stage input kind")?;
        let family_label = spec.family_label.map(normalize_stage_name).transpose()?;
        let restart_stage = spec.restart_stage.map(normalize_stage_name).transpose()?;
        for required in &requires {
            if required == &name {
                return Err("compiler stage cannot require itself".to_string());
            }
        }
        for alias in &aliases {
            if let Some(owner) = self.aliases.get(alias) {
                if owner != &name {
                    return Err(format!(
                        "compiler stage alias {alias:?} is already registered for stage {owner:?}"
                    ));
                }
            }
        }
        for input_kind in &input_kinds {
            if let Some(owner) = self.input_kind_to_stage.get(input_kind) {
                if owner != &name {
                    return Err(format!(
                        "input kind {input_kind:?} is already accepted by stage {owner:?}"
                    ));
                }
            }
        }
        self.drop_stage_indexes(&name);
        for alias in &aliases {
            self.aliases.insert(alias.clone(), name.clone());
        }
        for input_kind in &input_kinds {
            self.input_kind_to_stage
                .insert(input_kind.clone(), name.clone());
        }
        if let Some(family) = &family_label {
            self.default_stage_by_family
                .entry(family.clone())
                .or_insert_with(|| name.clone());
        }
        self.stages.insert(
            name.clone(),
            QueryStageSpec {
                name,
                requires,
                phase_policy: spec.phase_policy,
                input_kinds,
                family_label,
                aliases,
                restart_stage,
            },
        );
        self.version += 1;
        Ok(())
    }

    fn drop_stage_indexes(&mut self, stage: &str) {
        self.aliases.retain(|_, owner| owner != stage);
        self.input_kind_to_stage.retain(|_, owner| owner != stage);
        self.default_stage_by_family
            .retain(|_, owner| owner != stage);
    }

    pub fn register_alias(
        &mut self,
        stage: impl Into<String>,
        alias: impl Into<String>,
    ) -> Result<(), String> {
        let stage = self.resolve_stage(stage.into())?;
        let alias = normalize_stage_name(alias.into())?;
        if let Some(owner) = self.aliases.get(&alias) {
            if owner != &stage {
                return Err(format!(
                    "compiler stage alias {alias:?} is already registered for stage {owner:?}"
                ));
            }
        }
        self.aliases.insert(alias.clone(), stage.clone());
        if let Some(spec) = self.stages.get_mut(&stage) {
            if !spec.aliases.contains(&alias) {
                spec.aliases.push(alias);
                spec.aliases.sort();
            }
        }
        self.version += 1;
        Ok(())
    }

    pub fn register_restart_stage(
        &mut self,
        stage: impl Into<String>,
        restart_stage: impl Into<String>,
    ) -> Result<(), String> {
        let stage = self.resolve_stage(stage.into())?;
        let restart_stage = self.resolve_stage(restart_stage.into())?;
        if let Some(spec) = self.stages.get_mut(&stage) {
            spec.restart_stage = Some(restart_stage);
        }
        self.version += 1;
        Ok(())
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn register_provider(
        &mut self,
        name: impl Into<String>,
        stage: impl Into<String>,
        phase_policy: PhasePolicy,
        callback: impl Fn(&mut Compiler, &mut Unit) -> Result<(), String> + 'static,
    ) -> Result<(), String> {
        self.register_provider_with_effects(
            name,
            stage,
            phase_policy,
            Vec::<String>::new(),
            callback,
        )
    }

    pub fn register_provider_with_effects(
        &mut self,
        name: impl Into<String>,
        stage: impl Into<String>,
        phase_policy: PhasePolicy,
        effect_tags: impl IntoIterator<Item = String>,
        callback: impl Fn(&mut Compiler, &mut Unit) -> Result<(), String> + 'static,
    ) -> Result<(), String> {
        let name = name.into();
        if name.is_empty() {
            return Err("query provider name must be non-empty".to_string());
        }
        if self.providers.iter().any(|provider| provider.name == name) {
            return Err(format!("query provider already registered: {name}"));
        }
        let stage = self.resolve_stage(stage.into())?;
        let effect_tags = normalize_unique_labels(effect_tags, "query provider effect tag")?;
        self.providers.push(QueryProvider {
            name,
            stage,
            family: None,
            phase_policy,
            requires: Vec::new(),
            requires_data: Vec::new(),
            provides_data: Vec::new(),
            provides: Vec::new(),
            effect_tags,
            input_schema: None,
            reads: Vec::new(),
            writes: Vec::new(),
            cache_scope: "none".to_string(),
            resume_policy: "safe".to_string(),
            registration_index: self.next_registration_index,
            callback: Rc::new(move |compiler, unit| {
                callback(compiler, unit).map(|()| QueryProviderCallbackOutcome::default())
            }),
        });
        self.next_registration_index += 1;
        self.version += 1;
        Ok(())
    }

    pub fn register_provider_contract(
        &mut self,
        name: impl Into<String>,
        stage: impl Into<String>,
        family: Option<String>,
        phase_policy: PhasePolicy,
        requires: impl IntoIterator<Item = String>,
        effect_tags: impl IntoIterator<Item = String>,
        spec: QueryProviderRegistrationSpec,
        callback: impl Fn(&mut Compiler, &mut Unit) -> Result<(), String> + 'static,
    ) -> Result<(), String> {
        self.register_provider_contract_with_outcome(
            name,
            stage,
            family,
            phase_policy,
            requires,
            effect_tags,
            spec,
            move |compiler, unit| {
                callback(compiler, unit).map(|()| QueryProviderCallbackOutcome::default())
            },
        )
    }

    pub fn register_provider_contract_with_outcome(
        &mut self,
        name: impl Into<String>,
        stage: impl Into<String>,
        family: Option<String>,
        phase_policy: PhasePolicy,
        requires: impl IntoIterator<Item = String>,
        effect_tags: impl IntoIterator<Item = String>,
        spec: QueryProviderRegistrationSpec,
        callback: impl Fn(&mut Compiler, &mut Unit) -> Result<QueryProviderCallbackOutcome, String>
            + 'static,
    ) -> Result<(), String> {
        let name = name.into();
        if name.is_empty() {
            return Err("query provider name must be non-empty".to_string());
        }
        if self.providers.iter().any(|provider| provider.name == name) {
            return Err(format!("query provider already registered: {name}"));
        }
        let stage = self.resolve_stage(stage.into())?;
        let family = family.map(normalize_stage_name).transpose()?;
        let requires = require_non_empty_labels(requires, "query provider requirement")?;
        let effect_tags = normalize_unique_labels(effect_tags, "query provider effect tag")?;
        let requires_data = normalize_data_keys(spec.requires_data)?;
        let explicit_provides_data = normalize_data_keys(spec.provides_data)?;
        let reads = normalize_unique_labels(spec.reads, "query provider read set")?;
        let writes = normalize_unique_labels(spec.writes, "query provider write set")?;
        let provides_data = if explicit_provides_data.is_empty() {
            data_keys_for_domains(&writes)?
        } else {
            explicit_provides_data
        };
        let cache_scope = normalize_stage_name(spec.cache_scope)?;
        let resume_policy = normalize_stage_name(spec.resume_policy)?;
        self.providers.push(QueryProvider {
            name,
            stage,
            family,
            phase_policy,
            requires,
            requires_data,
            provides_data: provides_data.clone(),
            provides: provides_data,
            effect_tags,
            input_schema: spec.input_schema,
            reads,
            writes,
            cache_scope,
            resume_policy,
            registration_index: self.next_registration_index,
            callback: Rc::new(callback),
        });
        self.next_registration_index += 1;
        self.version += 1;
        Ok(())
    }

    pub fn resolve_stage(&self, target: impl Into<String>) -> Result<String, String> {
        if self.stages.is_empty() {
            return Err("no compiler stages registered".to_string());
        }
        let target = normalize_stage_name(target.into())?;
        if self.stages.contains_key(&target) {
            return Ok(target);
        }
        self.aliases
            .get(&target)
            .cloned()
            .ok_or_else(|| format!("unsupported query target: {target}"))
    }

    pub fn stage_names(&self) -> Vec<&str> {
        self.stages.keys().map(String::as_str).collect()
    }

    pub fn stage_spec(&self, stage: &str) -> Result<Option<&QueryStageSpec>, String> {
        let stage = self.resolve_stage(stage.to_string())?;
        Ok(self.stages.get(&stage))
    }

    pub fn default_stage_for_family(&self, family: impl Into<String>) -> Result<String, String> {
        let family = normalize_stage_name(family.into())?;
        self.default_stage_by_family
            .get(&family)
            .cloned()
            .ok_or_else(|| format!("unsupported query provider family {family:?}"))
    }

    pub fn stage_for_input_kind(&self, input_kind: impl Into<String>) -> Result<String, String> {
        let input_kind = normalize_stage_name(input_kind.into())?;
        self.input_kind_to_stage
            .get(&input_kind)
            .cloned()
            .ok_or_else(|| format!("unsupported query input kind {input_kind:?}"))
    }

    pub fn restart_stage_for(&self, stage: impl Into<String>) -> Result<String, String> {
        let stage = self.resolve_stage(stage.into())?;
        let spec = self
            .stages
            .get(&stage)
            .ok_or_else(|| format!("unsupported query stage: {stage}"))?;
        Ok(spec.restart_stage.clone().unwrap_or(stage))
    }

    pub fn explicit_restart_stage_for(
        &self,
        stage: impl Into<String>,
    ) -> Result<Option<String>, String> {
        let stage = self.resolve_stage(stage.into())?;
        let spec = self
            .stages
            .get(&stage)
            .ok_or_else(|| format!("unsupported query stage: {stage}"))?;
        Ok(spec.restart_stage.clone())
    }

    pub fn ordered_providers(&self) -> Vec<QueryProvider> {
        let mut providers = self.providers.clone();
        providers.sort_by_key(|provider| provider.registration_index);
        providers
    }

    pub fn providers_for_stage(
        &self,
        stage: impl Into<String>,
    ) -> Result<Vec<QueryProvider>, String> {
        let stage = self.resolve_stage(stage.into())?;
        Ok(self
            .ordered_providers()
            .into_iter()
            .filter(|provider| provider.stage == stage)
            .collect())
    }

    fn provider_names_for_completed_origin(
        &self,
        origin_stage: Option<&str>,
    ) -> Result<BTreeSet<String>, String> {
        let Some(origin_stage) = origin_stage else {
            return Ok(BTreeSet::new());
        };
        let completed_stages: BTreeSet<String> =
            self.route_to_stage(origin_stage)?.into_iter().collect();
        Ok(self
            .ordered_providers()
            .into_iter()
            .filter(|provider| completed_stages.contains(&provider.stage))
            .map(|provider| provider.name)
            .collect())
    }

    fn data_keys_for_satisfied_providers(&self, satisfied: &BTreeSet<String>) -> Vec<String> {
        let mut keys = Vec::new();
        for provider in self.ordered_providers() {
            if satisfied.contains(&provider.name) {
                extend_available_data(&mut keys, provider.provides_data.iter().cloned());
            }
        }
        keys
    }

    pub fn provider_schedule_for_stage(
        &self,
        stage: impl Into<String>,
    ) -> Result<QueryProviderSchedule, String> {
        self.provider_schedule_for_stage_with_available_data(stage, [])
    }

    pub fn provider_schedule_for_stage_with_available_data(
        &self,
        stage: impl Into<String>,
        available_data: impl IntoIterator<Item = String>,
    ) -> Result<QueryProviderSchedule, String> {
        self.provider_schedule_for_stage_with_dynamic_requires(
            stage,
            available_data,
            &BTreeSet::new(),
            &BTreeMap::new(),
        )
    }

    fn provider_schedule_for_stage_with_dynamic_requires(
        &self,
        stage: impl Into<String>,
        available_data: impl IntoIterator<Item = String>,
        previously_satisfied: &BTreeSet<String>,
        dynamic_requires: &BTreeMap<String, Vec<String>>,
    ) -> Result<QueryProviderSchedule, String> {
        let providers = self.providers_for_stage(stage)?;
        let available_data = normalize_data_keys(available_data)?;
        let observed_requires =
            observed_provider_requires(&providers, previously_satisfied, dynamic_requires);
        provider_schedule_batches(providers, &available_data, &observed_requires)
    }

    pub fn plan(&self, target: impl Into<String>, phase: PhasePolicy) -> Result<QueryPlan, String> {
        let target = self.resolve_stage(target.into())?;
        let route = self.route_to_stage(&target)?;
        self.plan_for_route(target, route, phase)
    }

    pub fn plan_from_stage_to_target(
        &self,
        from_stage: impl Into<String>,
        target: impl Into<String>,
        phase: PhasePolicy,
    ) -> Result<QueryPlan, String> {
        let target = self.resolve_stage(target.into())?;
        let route = self.route_from_stage_to_target(from_stage, target.clone())?;
        self.plan_for_route(target, route, phase)
    }

    fn plan_from_origin_option(
        &self,
        from_stage: Option<&str>,
        target: impl Into<String>,
        phase: PhasePolicy,
    ) -> Result<QueryPlan, String> {
        match from_stage {
            Some(from_stage) => self.plan_from_stage_to_target(from_stage, target, phase),
            None => self.plan(target, phase),
        }
    }

    fn plan_for_route(
        &self,
        target: String,
        route: Vec<String>,
        phase: PhasePolicy,
    ) -> Result<QueryPlan, String> {
        let mut steps = Vec::with_capacity(route.len());
        let mut available_data = Vec::new();
        for stage in route {
            let schedule = self.provider_schedule_for_stage_with_available_data(
                stage.clone(),
                available_data.clone(),
            )?;
            let providers: Vec<QueryProvider> = schedule.groups.into_iter().flatten().collect();
            let provider_names = providers
                .iter()
                .map(|provider| provider.name.clone())
                .collect();
            let effect_tags = normalize_unique_labels(
                providers
                    .iter()
                    .flat_map(|provider| provider.effect_tags.iter().cloned()),
                "query stage effect tag",
            )?;
            steps.push(QueryPlanStep {
                stage,
                provider_names,
                effect_tags,
                cached: false,
                artifact_key: None,
                restarted: false,
                restart_target: None,
            });
            extend_available_data(
                &mut available_data,
                providers
                    .iter()
                    .flat_map(|provider| provider.provides_data.iter().cloned()),
            );
        }
        Ok(QueryPlan {
            target,
            phase,
            steps,
            executed: Vec::new(),
        })
    }

    pub fn terminal_stages(&self) -> Result<Vec<String>, String> {
        if self.stages.is_empty() {
            return Err("no compiler stages registered".to_string());
        }
        let mut dependents = BTreeSet::new();
        for spec in self.stages.values() {
            for dependency in &spec.requires {
                if !self.stages.contains_key(dependency) {
                    return Err(format!(
                        "compiler stage {:?} depends on missing stage {:?}",
                        spec.name, dependency
                    ));
                }
                dependents.insert(dependency.clone());
            }
        }
        let mut terminals: Vec<String> = self
            .stages
            .keys()
            .filter(|stage| !dependents.contains(*stage))
            .cloned()
            .collect();
        terminals.sort();
        Ok(terminals)
    }

    pub fn terminal_stage(&self) -> Result<String, String> {
        let terminals = self.terminal_stages()?;
        if terminals.len() != 1 {
            let labels = if terminals.is_empty() {
                "<none>".to_string()
            } else {
                terminals.join(", ")
            };
            return Err(format!(
                "compiler stage graph must have exactly one terminal stage, got: {labels}"
            ));
        }
        Ok(terminals[0].clone())
    }

    pub fn route_to_stage(&self, target: &str) -> Result<Vec<String>, String> {
        let target = self.resolve_stage(target.to_string())?;
        let mut route = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        self.visit_stage(&target, &mut visiting, &mut visited, &mut route)?;
        Ok(route)
    }

    pub fn route_from_stage_to_target(
        &self,
        from_stage: impl Into<String>,
        target: impl Into<String>,
    ) -> Result<Vec<String>, String> {
        if self.stages.is_empty() {
            return Err("no compiler stages registered".to_string());
        }
        let from_stage = self.resolve_stage(from_stage.into())?;
        let target = self.resolve_stage(target.into())?;
        let _ = self.route_to_stage(&target)?;
        if from_stage == target {
            return Ok(Vec::new());
        }

        let mut adjacency: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for spec in self.stages.values() {
            for dependency in &spec.requires {
                adjacency
                    .entry(dependency.as_str())
                    .or_default()
                    .push(spec.name.as_str());
            }
        }
        for dependents in adjacency.values_mut() {
            dependents.sort();
        }

        let mut seen = BTreeSet::from([from_stage.clone()]);
        let mut queue = VecDeque::from([(from_stage.clone(), Vec::<String>::new())]);
        while let Some((stage, path)) = queue.pop_front() {
            for candidate in adjacency.get(stage.as_str()).into_iter().flatten() {
                if !seen.insert((*candidate).to_string()) {
                    continue;
                }
                let mut next_path = path.clone();
                next_path.push((*candidate).to_string());
                if candidate == &target.as_str() {
                    return Ok(next_path);
                }
                queue.push_back(((*candidate).to_string(), next_path));
            }
        }
        Err(format!(
            "cannot schedule compiler from {from_stage:?} to {target:?}"
        ))
    }

    fn visit_stage(
        &self,
        stage: &str,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        route: &mut Vec<String>,
    ) -> Result<(), String> {
        if visited.contains(stage) {
            return Ok(());
        }
        if !visiting.insert(stage.to_string()) {
            return Err(format!("compiler stage graph contains a cycle at {stage}"));
        }
        let spec = self
            .stages
            .get(stage)
            .ok_or_else(|| format!("unsupported query stage: {stage}"))?;
        for required in &spec.requires {
            if !self.stages.contains_key(required) {
                return Err(format!(
                    "compiler stage {stage:?} depends on missing stage {required:?}"
                ));
            }
            self.visit_stage(required, visiting, visited, route)?;
        }
        visiting.remove(stage);
        visited.insert(stage.to_string());
        route.push(stage.to_string());
        Ok(())
    }
}

fn provider_schedule_batches(
    providers: Vec<QueryProvider>,
    available_data: &[String],
    dynamic_requires: &BTreeMap<String, Vec<String>>,
) -> Result<QueryProviderSchedule, String> {
    if providers.is_empty() {
        return Ok(QueryProviderSchedule {
            groups: Vec::new(),
            barriers: Vec::new(),
        });
    }

    let positions: BTreeMap<String, usize> = providers
        .iter()
        .enumerate()
        .map(|(index, provider)| (provider.name.clone(), index))
        .collect();
    let by_name: BTreeMap<String, QueryProvider> = providers
        .iter()
        .map(|provider| (provider.name.clone(), provider.clone()))
        .collect();
    let mut outgoing: BTreeMap<String, BTreeSet<String>> = providers
        .iter()
        .map(|provider| (provider.name.clone(), BTreeSet::new()))
        .collect();
    let mut incoming_count: BTreeMap<String, usize> = providers
        .iter()
        .map(|provider| (provider.name.clone(), 0))
        .collect();

    for provider in &providers {
        for requirement in &provider.requires {
            if !positions.contains_key(requirement) {
                return Err(format!(
                    "provider {:?} requires missing provider {:?}",
                    provider.name, requirement
                ));
            }
            add_provider_schedule_edge(
                &mut outgoing,
                &mut incoming_count,
                requirement,
                &provider.name,
            );
        }
        for requirement in dynamic_requires
            .get(&provider.name)
            .into_iter()
            .flatten()
            .filter(|requirement| positions.contains_key(*requirement))
        {
            add_provider_schedule_edge(
                &mut outgoing,
                &mut incoming_count,
                requirement,
                &provider.name,
            );
        }
    }
    add_provider_data_edges(
        &providers,
        &positions,
        &mut outgoing,
        &mut incoming_count,
        available_data,
    )?;
    add_provider_effect_conflict_edges(&providers, &mut outgoing, &mut incoming_count);

    let mut ready: Vec<String> = providers
        .iter()
        .filter(|provider| incoming_count.get(&provider.name).copied().unwrap_or(0) == 0)
        .map(|provider| provider.name.clone())
        .collect();
    ready.sort_by_key(|name| positions[name]);

    let mut visited = BTreeSet::new();
    let mut batches = Vec::new();
    while !ready.is_empty() {
        let batch_names = std::mem::take(&mut ready);
        let mut batch = Vec::with_capacity(batch_names.len());
        let mut next = Vec::new();
        for name in batch_names {
            if !visited.insert(name.clone()) {
                continue;
            }
            batch.push(
                by_name
                    .get(&name)
                    .cloned()
                    .ok_or_else(|| format!("provider scheduler lost provider {name:?}"))?,
            );
            for target in outgoing.get(&name).into_iter().flatten() {
                let Some(count) = incoming_count.get_mut(target) else {
                    continue;
                };
                *count = count.saturating_sub(1);
                if *count == 0 {
                    next.push(target.clone());
                }
            }
        }
        if !batch.is_empty() {
            batches.push(batch);
        }
        next.sort_by_key(|name| positions[name]);
        next.dedup();
        ready = next;
    }

    if visited.len() != providers.len() {
        return Err("provider scheduler detected a cycle inside one stage".to_string());
    }

    let barriers = provider_schedule_barriers(&batches);
    Ok(QueryProviderSchedule {
        groups: batches,
        barriers,
    })
}

fn add_provider_schedule_edge(
    outgoing: &mut BTreeMap<String, BTreeSet<String>>,
    incoming_count: &mut BTreeMap<String, usize>,
    before: &str,
    after: &str,
) {
    if before == after {
        return;
    }
    let Some(targets) = outgoing.get_mut(before) else {
        return;
    };
    if targets.insert(after.to_string()) {
        *incoming_count.entry(after.to_string()).or_default() += 1;
    }
}

fn observed_provider_requires(
    providers: &[QueryProvider],
    previously_satisfied: &BTreeSet<String>,
    dynamic_requires: &BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    let stage_provider_names: BTreeSet<String> = providers
        .iter()
        .map(|provider| provider.name.clone())
        .collect();
    providers
        .iter()
        .map(|provider| {
            let mut requirements: Vec<String> = dynamic_requires
                .get(&provider.name)
                .into_iter()
                .flatten()
                .filter(|requirement| {
                    stage_provider_names.contains(*requirement)
                        || previously_satisfied.contains(*requirement)
                })
                .cloned()
                .collect();
            requirements.sort();
            requirements.dedup();
            (provider.name.clone(), requirements)
        })
        .collect()
}

fn add_provider_data_edges(
    providers: &[QueryProvider],
    positions: &BTreeMap<String, usize>,
    outgoing: &mut BTreeMap<String, BTreeSet<String>>,
    incoming_count: &mut BTreeMap<String, usize>,
    available_data: &[String],
) -> Result<(), String> {
    for provider in providers {
        for requirement in &provider.requires_data {
            if available_data
                .iter()
                .any(|key| data_key_matches(requirement, key))
            {
                continue;
            }
            let mut suppliers: Vec<&str> = providers
                .iter()
                .filter(|candidate| candidate.name != provider.name)
                .filter(|candidate| {
                    candidate
                        .provides_data
                        .iter()
                        .any(|provided| data_key_matches(requirement, provided))
                })
                .map(|candidate| candidate.name.as_str())
                .collect();
            suppliers.sort_by_key(|name| positions[*name]);
            if suppliers.is_empty() {
                return Err(format!(
                    "provider {:?} requires data {:?} from a later or missing stage",
                    provider.name, requirement
                ));
            }
            for supplier in suppliers {
                add_provider_schedule_edge(outgoing, incoming_count, supplier, &provider.name);
            }
        }
    }
    Ok(())
}

fn add_provider_effect_conflict_edges(
    providers: &[QueryProvider],
    outgoing: &mut BTreeMap<String, BTreeSet<String>>,
    incoming_count: &mut BTreeMap<String, usize>,
) {
    for (index, current) in providers.iter().enumerate() {
        if current.reads.is_empty() && current.writes.is_empty() {
            continue;
        }
        for later in providers.iter().skip(index + 1) {
            if !provider_effects_conflict(current, later) {
                continue;
            }
            let reachable = provider_schedule_reachability(providers, outgoing);
            if reachable
                .get(&later.name)
                .is_some_and(|targets| targets.contains(&current.name))
            {
                continue;
            }
            add_provider_schedule_edge(outgoing, incoming_count, &current.name, &later.name);
        }
    }
}

fn provider_effects_conflict(current: &QueryProvider, later: &QueryProvider) -> bool {
    strings_intersect(&current.reads, &later.writes)
        || strings_intersect(&current.writes, &later.reads)
        || strings_intersect(&current.writes, &later.writes)
}

fn provider_schedule_reachability(
    providers: &[QueryProvider],
    outgoing: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut reachable = BTreeMap::new();
    for provider in providers {
        let mut visited = BTreeSet::new();
        let mut stack: Vec<String> = outgoing
            .get(&provider.name)
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        while let Some(name) = stack.pop() {
            if !visited.insert(name.clone()) {
                continue;
            }
            stack.extend(outgoing.get(&name).into_iter().flatten().cloned());
        }
        reachable.insert(provider.name.clone(), visited);
    }
    reachable
}

fn provider_schedule_barriers(groups: &[Vec<QueryProvider>]) -> Vec<Option<Vec<String>>> {
    let mut barriers = Vec::with_capacity(groups.len());
    for index in 0..groups.len() {
        let Some(next_group) = groups.get(index + 1) else {
            barriers.push(None);
            continue;
        };
        let mut reasons = Vec::new();
        for current in &groups[index] {
            for later in next_group {
                push_provider_barrier_reason(
                    &mut reasons,
                    "writes after reads on",
                    &current.reads,
                    &later.writes,
                );
                push_provider_barrier_reason(
                    &mut reasons,
                    "reads after writes on",
                    &current.writes,
                    &later.reads,
                );
                push_provider_barrier_reason(
                    &mut reasons,
                    "competing writes on",
                    &current.writes,
                    &later.writes,
                );
            }
        }
        reasons.sort();
        reasons.dedup();
        barriers.push((!reasons.is_empty()).then_some(reasons));
    }
    barriers
}

fn push_provider_barrier_reason(
    reasons: &mut Vec<String>,
    label: &str,
    left: &[String],
    right: &[String],
) {
    let intersection = string_intersection(left, right);
    if !intersection.is_empty() {
        reasons.push(format!("{label} {}", intersection.join(", ")));
    }
}

fn strings_intersect(left: &[String], right: &[String]) -> bool {
    left.iter().any(|item| right.contains(item))
}

fn string_intersection(left: &[String], right: &[String]) -> Vec<String> {
    let mut values: Vec<String> = left
        .iter()
        .filter(|item| right.contains(item))
        .cloned()
        .collect();
    values.sort();
    values.dedup();
    values
}

fn data_key_matches(requirement: &str, provided: &str) -> bool {
    requirement == provided
        || requirement
            .strip_suffix(".*")
            .is_some_and(|prefix| provided.starts_with(&format!("{prefix}.")))
}

fn extend_available_data(
    available_data: &mut Vec<String>,
    values: impl IntoIterator<Item = String>,
) {
    let mut seen: BTreeSet<String> = available_data.iter().cloned().collect();
    for value in values {
        if seen.insert(value.clone()) {
            available_data.push(value);
        }
    }
}

fn extend_unique(target: &mut Vec<String>, values: impl IntoIterator<Item = String>) {
    let mut seen: BTreeSet<String> = target.iter().cloned().collect();
    for value in values {
        if seen.insert(value.clone()) {
            target.push(value);
        }
    }
    target.sort();
}

fn extend_unique_artifact_keys(
    target: &mut Vec<ArtifactKey>,
    values: impl IntoIterator<Item = ArtifactKey>,
) {
    let mut seen: BTreeSet<ArtifactKey> = target.iter().cloned().collect();
    for value in values {
        if seen.insert(value.clone()) {
            target.push(value);
        }
    }
    target.sort();
}

fn semantic_subject_tracking_key(subject: &SemanticSubjectId) -> String {
    format!("{}:{}", subject.kind, subject.value)
}

fn semantic_cell_tracking_key(subject: &str, predicate: &str) -> String {
    format!("{subject}@{predicate}")
}

fn annotation_tracking_predicate(key: &str) -> String {
    format!("annotation.{key}")
}

fn merge_provider_context_tracking(
    target: &mut QueryProviderContext,
    source: &QueryProviderContext,
) {
    extend_unique(
        &mut target.reads_subjects,
        source.reads_subjects.iter().cloned(),
    );
    extend_unique(
        &mut target.writes_subjects,
        source.writes_subjects.iter().cloned(),
    );
    extend_unique(&mut target.read_cells, source.read_cells.iter().cloned());
    extend_unique(&mut target.write_cells, source.write_cells.iter().cloned());
    extend_unique(&mut target.reads_files, source.reads_files.iter().cloned());
    extend_unique(
        &mut target.writes_files,
        source.writes_files.iter().cloned(),
    );
    extend_unique_artifact_keys(
        &mut target.artifact_dependencies,
        source.artifact_dependencies.iter().cloned(),
    );
}

impl<'a> CompilerCatalog<'a> {
    pub fn get_compiled_unit(&self, unit_id: &str) -> Result<Option<&'a Unit>, String> {
        if unit_id.is_empty() {
            return Err("compiler catalog unit id lookup must be non-empty".to_string());
        }
        Ok(self.units.get(unit_id))
    }

    pub fn unit_ids(&self) -> Vec<&'a str> {
        self.units.keys().map(String::as_str).collect()
    }

    pub fn contains_unit(&self, unit_id: &str) -> bool {
        self.units.contains_key(unit_id)
    }
}

impl<'a> CompilerQueryService<'a> {
    pub fn plan_query(&self, target: &str, phase: PhasePolicy) -> Result<QueryPlan, String> {
        self.compiler.provider_registry.plan(target, phase)
    }

    pub fn compile(&mut self, unit: &mut Unit) -> Result<QueryPlan, String> {
        self.query("compile_unit", unit, PhasePolicy::CompileTime)
    }

    pub fn query(
        &mut self,
        target: &str,
        unit: &mut Unit,
        phase: PhasePolicy,
    ) -> Result<QueryPlan, String> {
        self.query_with_options(target, unit, phase, QueryExecutionOptions::default())
    }

    pub fn query_with_transaction_mode(
        &mut self,
        target: &str,
        unit: &mut Unit,
        phase: PhasePolicy,
        transaction_mode: QueryTransactionMode,
    ) -> Result<QueryPlan, String> {
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
    ) -> Result<QueryPlan, String> {
        self.query_with_options_from_origin(None, target, unit, phase, options)
    }

    pub fn query_from_stage_to_target_with_options(
        &mut self,
        from_stage: &str,
        target: &str,
        unit: &mut Unit,
        phase: PhasePolicy,
        options: QueryExecutionOptions,
    ) -> Result<QueryPlan, String> {
        self.query_with_options_from_origin(Some(from_stage), target, unit, phase, options)
    }

    fn query_with_options_from_origin(
        &mut self,
        origin_stage: Option<&str>,
        target: &str,
        unit: &mut Unit,
        phase: PhasePolicy,
        options: QueryExecutionOptions,
    ) -> Result<QueryPlan, String> {
        let restart_limit = options.restart_limit;
        let allowed_effect_tags = options
            .allowed_effect_tags
            .map(|tags| normalize_unique_labels(tags, "query allowed effect tag"))
            .transpose()?;
        let initial_bindings = normalize_initial_bindings(options.initial_bindings)?;
        match options.transaction_mode {
            QueryTransactionMode::InPlace => self.query_inner(
                origin_stage,
                target,
                unit,
                phase,
                restart_limit,
                allowed_effect_tags.as_deref(),
                &initial_bindings,
            ),
            QueryTransactionMode::AtomicUnit => {
                let unit_snapshot = unit.snapshot();
                let cache_snapshot = self.compiler.artifact_cache.snapshot();
                match self.query_inner(
                    origin_stage,
                    target,
                    unit,
                    phase,
                    restart_limit,
                    allowed_effect_tags.as_deref(),
                    &initial_bindings,
                ) {
                    Ok(plan) => Ok(plan),
                    Err(error) => {
                        unit.restore_snapshot(unit_snapshot);
                        self.compiler
                            .artifact_cache
                            .restore_snapshot(cache_snapshot)
                            .map_err(|rollback_error| {
                                format!("{error}; query rollback failed: {rollback_error}")
                            })?;
                        Err(error)
                    }
                }
            }
        }
    }

    fn query_inner(
        &mut self,
        origin_stage: Option<&str>,
        target: &str,
        unit: &mut Unit,
        phase: PhasePolicy,
        restart_limit: usize,
        allowed_effect_tags: Option<&[String]>,
        initial_bindings: &[(String, RuntimeValue)],
    ) -> Result<QueryPlan, String> {
        let mut plan = match origin_stage {
            Some(origin_stage) => self.compiler.provider_registry.plan_from_stage_to_target(
                origin_stage,
                target,
                phase,
            )?,
            None => self.compiler.provider_registry.plan(target, phase)?,
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
        );
        let mut index = 0;
        let mut restarts_remaining = restart_limit;
        let mut satisfied_providers = self
            .compiler
            .provider_registry
            .provider_names_for_completed_origin(origin_stage)?;
        let mut available_data = self
            .compiler
            .provider_registry
            .data_keys_for_satisfied_providers(&satisfied_providers);
        while index < plan.steps.len() {
            let restart_target;
            let stop_pipeline;
            {
                let step = &mut plan.steps[index];
                let stage_started = Instant::now();
                let cache_key = query_stage_cache_key(
                    unit,
                    &step.stage,
                    phase,
                    initial_bindings,
                    self.compiler.provider_registry.version(),
                    self.compiler.registry.version(),
                    self.compiler.host.host_version(),
                )?;
                step.artifact_key = Some(cache_key.clone());
                if let Some(cached_value) = self.compiler.artifact_cache.get(&cache_key).cloned() {
                    step.cached = true;
                    let iteration_offset = plan.executed.len();
                    let mut cached_records = cached_execution_records(&cached_value);
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
                    step.effect_tags = normalize_unique_labels(
                        cached_records
                            .iter()
                            .flat_map(|record| record.effect_tags.iter().cloned()),
                        "query stage effect tag",
                    )?;
                    plan.executed.extend(cached_records);
                    self.compiler.emit_compiler_event(
                        "query.stage.cache-hit",
                        Some(step.stage.clone()),
                        "reused cached query stage",
                        [
                            ("elapsed_ms".to_string(), elapsed_ms_string(stage_started)),
                            ("phase".to_string(), phase.as_str().to_string()),
                            ("unit".to_string(), unit.unit_id().to_string()),
                        ],
                    );
                    index += 1;
                    continue;
                }
                let provider_groups = self
                    .compiler
                    .provider_registry
                    .provider_schedule_for_stage_with_dynamic_requires(
                        step.stage.clone(),
                        available_data.clone(),
                        &satisfied_providers,
                        &self.compiler.provider_dynamic_requires,
                    )?
                    .groups;
                step.provider_names = provider_groups
                    .iter()
                    .flatten()
                    .map(|provider| provider.name.clone())
                    .collect();
                step.effect_tags = normalize_unique_labels(
                    provider_groups
                        .iter()
                        .flatten()
                        .flat_map(|provider| provider.effect_tags.iter().cloned()),
                    "query stage effect tag",
                )?;
                let stage_execution_start = plan.executed.len();
                let mut stage_stop_pipeline = false;
                for providers in provider_groups {
                    for provider in providers {
                        if provider.phase_policy != PhasePolicy::Dual
                            && provider.phase_policy != phase
                        {
                            return Err(format!(
                                "query provider {} is not available in phase {}",
                                provider.name,
                                phase.as_str()
                            ));
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
                        );
                        let provider_cache_key = provider_cache_key(
                            self.compiler,
                            unit,
                            &provider,
                            phase,
                            initial_bindings,
                        )?;
                        if let Some(cache_key) = &provider_cache_key {
                            if let Some(cached_entry) =
                                self.compiler.provider_ctfe_cache.get(cache_key).cloned()
                            {
                                let iteration = plan.executed.len();
                                if let Some(snapshot) = cached_entry.snapshot.clone() {
                                    unit.restore_snapshot(snapshot);
                                }
                                self.compiler
                                    .diagnostics
                                    .extend(cached_entry.diagnostics.clone());
                                self.compiler.provider_dynamic_requires.insert(
                                    provider.name.clone(),
                                    cached_entry.dynamic_requires.clone(),
                                );
                                if cached_entry.restart_requested {
                                    self.compiler.pending_query_restart =
                                        cached_entry.restart_stage.clone();
                                }
                                plan.executed.push(cached_provider_execution_record(
                                    &provider,
                                    iteration,
                                    &cached_entry,
                                ));
                                self.compiler.emit_compiler_event(
                                    "query.provider.cache-hit",
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
                                );
                                continue;
                            }
                        }
                        let before_version = unit.version();
                        let before_ir = unit.ir().to_template();
                        let before_diagnostics = self.compiler.diagnostics.len();
                        let iteration = plan.executed.len();
                        let rollback_snapshot = capture_provider_rollback_snapshot(unit, &provider);
                        let previous_context = self.compiler.active_provider_context.clone();
                        self.compiler.active_provider_context = Some(QueryProviderContext {
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
                        let result = (provider.callback)(self.compiler, unit);
                        let provider_context = self.compiler.active_provider_context.clone();
                        self.compiler.active_provider_context = previous_context;
                        let diagnostics_emitted = self
                            .compiler
                            .diagnostics
                            .len()
                            .saturating_sub(before_diagnostics);
                        let diagnostic_codes = self.compiler.diagnostics[before_diagnostics..]
                            .iter()
                            .filter_map(|diagnostic| diagnostic.code.clone())
                            .collect();
                        let stopped_by_diagnostic = self.compiler.diagnostics[before_diagnostics..]
                            .iter()
                            .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error);
                        let ir_change = provider_ir_change_stats(&before_ir.nodes, unit);
                        let observed_changed = unit.version() != before_version;
                        match result {
                            Ok(outcome) => {
                                let reported_changed = outcome.changed.unwrap_or(false);
                                let changed = observed_changed || reported_changed;
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
                                    self.compiler.pending_query_restart.clone()
                                } else {
                                    provider_restart_stage(
                                        self.compiler,
                                        &provider,
                                        reported_changed && !rolled_back,
                                    )?
                                };
                                if !stopped_by_diagnostic {
                                    self.compiler.pending_query_restart = restart_stage.clone();
                                }
                                let record = provider_execution_record(
                                    &provider,
                                    iteration,
                                    changed && !rolled_back,
                                    diagnostics_emitted,
                                    rolled_back,
                                    stopped_by_diagnostic,
                                    if stopped_by_diagnostic {
                                        "stopped_by_error"
                                    } else {
                                        "ok"
                                    },
                                    diagnostic_codes,
                                    ir_change,
                                    restart_stage.clone(),
                                    provider_context.as_ref(),
                                );
                                if !stopped_by_diagnostic {
                                    if let Some(cache_key) = provider_cache_key {
                                        if let Some(cache_entry) = provider_cache_entry(
                                            unit,
                                            &provider,
                                            &record,
                                            self.compiler.diagnostics[before_diagnostics..]
                                                .to_vec(),
                                            self.compiler
                                                .provider_dynamic_requires
                                                .get(&provider.name)
                                                .cloned()
                                                .unwrap_or_default(),
                                        ) {
                                            self.compiler
                                                .provider_ctfe_cache
                                                .insert(cache_key, cache_entry);
                                        }
                                    }
                                }
                                plan.executed.push(record);
                                if stopped_by_diagnostic {
                                    self.compiler.pending_query_restart = None;
                                    stage_stop_pipeline = true;
                                }
                                self.compiler.emit_compiler_event(
                                    if stopped_by_diagnostic {
                                        "query.provider.stopped-by-error"
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
                                )
                            }
                            Err(error) => {
                                let rolled_back = if let Some(snapshot) = rollback_snapshot {
                                    restore_provider_rollback_snapshot(unit, snapshot)?;
                                    true
                                } else {
                                    false
                                };
                                let restart_stage = self.compiler.pending_query_restart.clone();
                                plan.executed.push(provider_execution_record(
                                    &provider,
                                    iteration,
                                    observed_changed && !rolled_back,
                                    diagnostics_emitted,
                                    rolled_back,
                                    true,
                                    "error",
                                    diagnostic_codes,
                                    ir_change,
                                    restart_stage,
                                    provider_context.as_ref(),
                                ));
                                self.compiler.pending_query_restart = None;
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
                                );
                                return Err(error);
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
                restart_target = self.compiler.pending_query_restart.take();
                stop_pipeline = stage_stop_pipeline;
                step.restart_target = restart_target.clone();
                self.compiler.artifact_cache.store(
                    cache_key,
                    ArtifactValue::Semantic(query_stage_artifact_value(
                        unit,
                        step,
                        phase,
                        &plan.executed[stage_execution_start..],
                    )?),
                    collect_record_artifact_dependencies(&plan.executed[stage_execution_start..]),
                )?;
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
                );
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
                    return Err(format!(
                        "query restart budget exhausted while restarting from {restart_target}"
                    ));
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
                );
                let original_route = self
                    .compiler
                    .provider_registry
                    .plan_from_origin_option(origin_stage, plan.target.clone(), phase)?
                    .steps;
                let Some(restart_index) = original_route
                    .iter()
                    .position(|step| step.stage == restart_target)
                else {
                    return Err(format!(
                        "query restart stage {restart_target} is not in route to {}",
                        plan.target
                    ));
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
        self.compiler.register_unit(unit.clone())?;
        Ok(plan)
    }
}

impl<'a> CompilerBootstrapController<'a> {
    pub fn grant_capability(
        &mut self,
        unit_id: impl Into<String>,
        capability: impl Into<String>,
    ) -> Result<(), String> {
        self.compiler
            .bootstrap_capabilities
            .grant(unit_id, capability)?;
        self.compiler.session_version += 1;
        Ok(())
    }

    pub fn grant_capabilities(
        &mut self,
        unit_id: impl Into<String>,
        capabilities: impl IntoIterator<Item = String>,
    ) -> Result<(), String> {
        self.compiler
            .bootstrap_capabilities
            .grant_many(unit_id, capabilities)?;
        self.compiler.session_version += 1;
        Ok(())
    }

    pub fn require_capability(&self, unit_id: &str, capability: &str) -> Result<(), String> {
        self.compiler
            .bootstrap_capabilities
            .require(unit_id, capability)
    }

    pub fn execute_text_with_capabilities(
        &mut self,
        text: impl Into<String>,
        unit_id: impl Into<String>,
        capabilities: impl IntoIterator<Item = String>,
    ) -> EvalResult {
        let unit_id = unit_id.into();
        self.grant_capabilities(unit_id.clone(), capabilities)
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?;
        self.execute_text(text, unit_id)
    }

    pub fn execute_virtual_file(
        &mut self,
        vfs: &BootstrapVirtualFileSystem,
        path: impl Into<String>,
        unit_id: impl Into<String>,
    ) -> EvalResult {
        let path = path.into();
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err(EvalSignal::Error(crate::values::EvaluationError::new(
                "bootstrap unit id must be non-empty",
            )));
        }
        let path = normalize_virtual_path(path)
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?;
        let depth = self.compiler.active_bootstrap_depth;
        self.compiler.active_bootstrap_depth += 1;
        let started = Instant::now();
        let result = self.execute_virtual_file_inner(vfs, path.clone(), unit_id, depth);
        let elapsed_ms = elapsed_ms_string(started);
        self.compiler.active_bootstrap_depth -= 1;
        let action = if depth == 0 {
            "bootstrap.vfs"
        } else {
            "bootstrap.nested_vfs"
        };
        let target = format!("vfs:{path}");
        let _ = self
            .compiler
            .push_bootstrap_trace(action, target.clone(), depth, result.is_ok());
        self.compiler.emit_compiler_event(
            "bootstrap.execute",
            Some(target),
            "executed bootstrap source",
            [
                ("action".to_string(), action.to_string()),
                ("depth".to_string(), depth.to_string()),
                ("elapsed_ms".to_string(), elapsed_ms),
                ("succeeded".to_string(), result.is_ok().to_string()),
            ],
        );
        result
    }

    pub fn execute_file(
        &mut self,
        path: impl AsRef<Path>,
        unit_id: impl Into<String>,
    ) -> EvalResult {
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err(EvalSignal::Error(crate::values::EvaluationError::new(
                "bootstrap unit id must be non-empty",
            )));
        }
        let resolved = resolve_source_path(path.as_ref())
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?;
        let target = path_to_string(&resolved)
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?;
        let depth = self.compiler.active_bootstrap_depth;
        self.compiler.active_bootstrap_depth += 1;
        let started = Instant::now();
        let result = self.execute_file_inner(resolved, unit_id, depth);
        let elapsed_ms = elapsed_ms_string(started);
        self.compiler.active_bootstrap_depth -= 1;
        let action = if depth == 0 {
            "bootstrap.raw"
        } else {
            "bootstrap.nested_raw"
        };
        let _ = self
            .compiler
            .push_bootstrap_trace(action, target.clone(), depth, result.is_ok());
        self.compiler.emit_compiler_event(
            "bootstrap.execute",
            Some(target),
            "executed bootstrap source",
            [
                ("action".to_string(), action.to_string()),
                ("depth".to_string(), depth.to_string()),
                ("elapsed_ms".to_string(), elapsed_ms),
                ("succeeded".to_string(), result.is_ok().to_string()),
            ],
        );
        result
    }

    pub fn execute_text(
        &mut self,
        text: impl Into<String>,
        unit_id: impl Into<String>,
    ) -> EvalResult {
        let text = text.into();
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err(EvalSignal::Error(crate::values::EvaluationError::new(
                "bootstrap unit id must be non-empty",
            )));
        }
        if text.is_empty() {
            return Err(EvalSignal::Error(crate::values::EvaluationError::new(
                "bootstrap text must be non-empty",
            )));
        }

        let depth = self.compiler.active_bootstrap_depth;
        self.compiler.active_bootstrap_depth += 1;
        let started = Instant::now();
        let result = self.execute_text_inner(text, unit_id.clone(), depth);
        let elapsed_ms = elapsed_ms_string(started);
        self.compiler.active_bootstrap_depth -= 1;

        let action = if depth == 0 {
            "bootstrap.raw"
        } else {
            "bootstrap.nested_raw"
        };
        let _ = self
            .compiler
            .push_bootstrap_trace(action, unit_id.clone(), depth, result.is_ok());
        self.compiler.emit_compiler_event(
            "bootstrap.execute",
            Some(unit_id),
            "executed bootstrap source",
            [
                ("action".to_string(), action.to_string()),
                ("depth".to_string(), depth.to_string()),
                ("elapsed_ms".to_string(), elapsed_ms),
                ("succeeded".to_string(), result.is_ok().to_string()),
            ],
        );
        result
    }

    fn execute_text_inner(&mut self, text: String, unit_id: String, _depth: usize) -> EvalResult {
        self.compiler
            .record_bootstrap_execution(format!("<inline:{unit_id}>"))
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?;
        let template = self
            .compiler
            .load_surface_text_template(text, unit_id.clone())
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?
            .template;
        let unit = Unit::from_template(template)
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?;
        self.compiler
            .register_unit(unit.clone())
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?;
        self.compiler.evaluation().evaluate(
            &unit,
            PhasePolicy::CompileTime,
            Vec::<(String, RuntimeValue)>::new(),
        )
    }

    fn execute_file_inner(&mut self, path: PathBuf, unit_id: String, _depth: usize) -> EvalResult {
        let path_string = path_to_string(&path)
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?;
        self.compiler
            .record_bootstrap_execution(path_string)
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?;
        let template = self
            .compiler
            .load_surface_path_template(path, unit_id)
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?
            .template;
        let unit = Unit::from_template(template)
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?;
        self.compiler
            .register_unit(unit.clone())
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?;
        self.compiler.evaluation().evaluate(
            &unit,
            PhasePolicy::CompileTime,
            Vec::<(String, RuntimeValue)>::new(),
        )
    }

    fn execute_virtual_file_inner(
        &mut self,
        vfs: &BootstrapVirtualFileSystem,
        path: String,
        unit_id: String,
        _depth: usize,
    ) -> EvalResult {
        let text = vfs
            .read(&path)
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?;
        if text.is_empty() {
            return Err(EvalSignal::Error(crate::values::EvaluationError::new(
                "bootstrap virtual file text must be non-empty",
            )));
        }
        self.compiler
            .record_bootstrap_execution(format!("<vfs:{path}>"))
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?;
        let template = self
            .compiler
            .load_surface_virtual_template(path, text.to_string(), unit_id)
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?
            .template;
        let unit = Unit::from_template(template)
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?;
        self.compiler
            .register_unit(unit.clone())
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?;
        self.compiler.evaluation().evaluate(
            &unit,
            PhasePolicy::CompileTime,
            Vec::<(String, RuntimeValue)>::new(),
        )
    }
}

impl<'a> CompilerEvaluationService<'a> {
    pub fn evaluate(
        &mut self,
        unit: &Unit,
        phase: PhasePolicy,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
    ) -> EvalResult {
        let bridge = Rc::new(CompilerBridgeValue::from_compiler(self.compiler));
        let mut bindings: Vec<(String, RuntimeValue)> = initial.into_iter().collect();
        bindings.push((
            "compiler".to_string(),
            RuntimeValue::HostObject(bridge.clone()),
        ));
        let result = evaluate_unit(unit, bindings);
        bridge.apply_to_compiler(self.compiler);
        if let Err(EvalSignal::Error(error)) = &result {
            self.compiler
                .push_diagnostic(Diagnostic::from_evaluation_error(error));
        }
        let _ = phase;
        result
    }

    pub fn evaluate_registered(
        &mut self,
        unit_id: &str,
        phase: PhasePolicy,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
    ) -> EvalResult {
        let unit = self
            .compiler
            .get_unit(unit_id)
            .map_err(|message| EvalSignal::Error(crate::values::EvaluationError::new(message)))?
            .cloned()
            .ok_or_else(|| {
                EvalSignal::Error(crate::values::EvaluationError::new(format!(
                    "compiled unit not found: {unit_id}"
                )))
            })?;
        self.evaluate(&unit, phase, initial)
    }

    pub fn evaluate_with_host_libraries(
        &mut self,
        unit: &Unit,
        phase: PhasePolicy,
        libraries: impl IntoIterator<Item = String>,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
    ) -> EvalResult {
        let mut bindings = Vec::new();
        {
            let services = match phase {
                PhasePolicy::CompileTime => self.compiler.host.compile_time_services(),
                PhasePolicy::Runtime | PhasePolicy::Dual => self.compiler.host.runtime_services(),
            };
            for library in libraries {
                let library_entry = services
                    .library(&library)
                    .map_err(|message| {
                        EvalSignal::Error(crate::values::EvaluationError::new(message))
                    })?
                    .ok_or_else(|| {
                        EvalSignal::Error(crate::values::EvaluationError::new(format!(
                            "host service library does not exist: {library}"
                        )))
                    })?;
                for export in library_entry.export_names() {
                    let value = services
                        .export(&library, export, phase)
                        .map_err(|message| {
                            EvalSignal::Error(crate::values::EvaluationError::new(message))
                        })?;
                    bindings.push((format!("{library}.{export}"), value));
                }
            }
        }
        bindings.extend(initial);
        self.evaluate(unit, phase, bindings)
    }

    pub fn evaluate_capture(
        &mut self,
        unit: &Unit,
        phase: PhasePolicy,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
        skip_leading_forms: usize,
    ) -> Result<EvaluationCapture, EvalSignal> {
        let bridge = Rc::new(CompilerBridgeValue::from_compiler(self.compiler));
        let mut bindings: Vec<(String, RuntimeValue)> = initial.into_iter().collect();
        bindings.push((
            "compiler".to_string(),
            RuntimeValue::HostObject(bridge.clone()),
        ));
        let result = evaluate_unit_capture_with_bindings(unit, bindings, skip_leading_forms);
        bridge.apply_to_compiler(self.compiler);
        match result {
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
                self.compiler.push_diagnostic(diagnostic.clone());
                Ok(EvaluationCapture {
                    unit_id: unit.unit_id().to_string(),
                    phase,
                    value: None,
                    bindings: Vec::new(),
                    diagnostics: vec![diagnostic],
                    skipped_forms: skip_leading_forms,
                })
            }
            Err(signal) => Err(signal),
        }
    }
}

fn normalize_stage_name(value: String) -> Result<String, String> {
    let value = value.trim().replace('-', "_").to_lowercase();
    if value.is_empty() {
        return Err("compiler stage name must be non-empty".to_string());
    }
    Ok(value)
}

fn normalize_virtual_path(value: String) -> Result<String, String> {
    let value = value.trim().trim_start_matches('/').to_string();
    if value.is_empty() {
        return Err("virtual bootstrap path must be non-empty".to_string());
    }
    Ok(value)
}

fn normalize_unique_labels(
    values: impl IntoIterator<Item = String>,
    label: &str,
) -> Result<Vec<String>, String> {
    let mut normalized = Vec::new();
    for value in values {
        normalized
            .push(normalize_stage_name(value).map_err(|_| format!("{label} must be non-empty"))?);
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn normalize_data_keys(values: impl IntoIterator<Item = String>) -> Result<Vec<String>, String> {
    let mut normalized = Vec::new();
    for value in values {
        normalized.push(normalize_data_key(&value)?);
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn normalize_data_key(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("provider data key must be non-empty".to_string());
    }
    if let Some((domain, rest)) = value.split_once(':') {
        return normalize_domain_data_key(domain, rest);
    }
    if let Some((domain, rest)) = value.split_once('.') {
        return normalize_domain_data_key(domain, rest);
    }
    normalize_domain_data_key(value, "*")
}

fn normalize_domain_data_key(domain: &str, rest: &str) -> Result<String, String> {
    let domain = normalize_provider_data_domain(domain)?;
    let rest = rest.trim();
    if rest.is_empty() {
        return Err("provider data key must include a subject or '*' wildcard".to_string());
    }
    Ok(format!("{domain}.{rest}"))
}

fn normalize_provider_data_domain(domain: &str) -> Result<&'static str, String> {
    match domain.trim().replace('_', "-").to_lowercase().as_str() {
        "annotation" | "annotations" | "attribute" | "attributes" => Ok("annotations"),
        "cache" => Ok("cache"),
        "diagnostic" | "diagnostics" => Ok("diagnostics"),
        "event" | "events" => Ok("events"),
        "fact" | "facts" => Ok("facts"),
        "file" | "files" => Ok("files"),
        "host-service" | "host-services" => Ok("host-services"),
        "ir" => Ok("ir"),
        "registry" => Ok("registry"),
        "symbol" | "symbols" => Ok("symbols"),
        "type" | "types" => Ok("types"),
        _ => Err(format!("unsupported provider data key domain {domain:?}")),
    }
}

fn data_keys_for_domains(domains: &[String]) -> Result<Vec<String>, String> {
    normalize_data_keys(domains.iter().map(|domain| format!("{domain}.*")))
}

fn require_non_empty_labels(
    values: impl IntoIterator<Item = String>,
    label: &str,
) -> Result<Vec<String>, String> {
    let mut labels = Vec::new();
    for value in values {
        if value.is_empty() {
            return Err(format!("{label} must be non-empty"));
        }
        labels.push(value);
    }
    Ok(labels)
}

fn enforce_query_effect_policy(
    plan: &QueryPlan,
    allowed_effect_tags: Option<&[String]>,
) -> Result<(), String> {
    let Some(allowed_effect_tags) = allowed_effect_tags else {
        return Ok(());
    };
    let allowed: BTreeSet<&str> = allowed_effect_tags.iter().map(String::as_str).collect();
    for step in &plan.steps {
        for effect_tag in &step.effect_tags {
            if !allowed.contains(effect_tag.as_str()) {
                return Err(format!(
                    "query effect tag {effect_tag:?} is not allowed for stage {}",
                    step.stage
                ));
            }
        }
    }
    Ok(())
}

fn source_artifact_to_template(
    cache: &HostSourceTemplateCache,
    source: &SourceArtifact,
    unit_id: &str,
) -> Result<UnitTemplate, String> {
    let cache_key = (
        unit_id.to_string(),
        source.parse_surface_key("parse", PhasePolicy::CompileTime)?,
    );
    if let Some(template) = cache
        .lock()
        .map_err(|_| "host source template cache is poisoned".to_string())?
        .get(&cache_key)
        .cloned()
    {
        return Ok(template);
    }

    let source_path = match &source.origin {
        crate::artifacts::SourceOrigin::Path { path, .. } => Some(path.as_str()),
        crate::artifacts::SourceOrigin::Inline { .. } => None,
    };
    let graph = match source_path {
        Some(path) => parse_with_source_path(&source.text, path)
            .map_err(|error| format!("failed to parse source artifact {path}: {error}"))?,
        None => parse(&source.text)
            .map_err(|error| format!("failed to parse inline source artifact: {error}"))?,
    };
    let mut unit = Unit::from_graph(unit_id, graph)?;
    let mut syntax = UnitSyntaxState::new("caap")?;
    if let Some(path) = source_path {
        syntax = syntax.with_source(path, source.fingerprint.to_string())?;
    }
    unit.set_syntax_state(syntax);
    let template = unit.to_template();
    let mut cache = cache
        .lock()
        .map_err(|_| "host source template cache is poisoned".to_string())?;
    if cache.len() >= HOST_SOURCE_TEMPLATE_CACHE_MAX_ENTRIES {
        if let Some(first_key) = cache.keys().next().cloned() {
            cache.remove(&first_key);
        }
    }
    cache.insert(cache_key, template.clone());
    Ok(template)
}

const PACKAGE_DECLARATION_HEADS: &[&str] = &[
    "module",
    "import-namespace",
    "import-symbols",
    "import-as",
    "syntax-import",
    "module-capability",
    "export",
    "export-as",
];

pub fn source_module_name(source: &str) -> Result<String, String> {
    Ok(parse_package_declarations(source, "<source>")?.name)
}

pub fn parse_package_declarations(
    source: &str,
    path: impl Into<String>,
) -> Result<PackageDescriptor, String> {
    let path = path.into();
    let scan = scan_package_declaration_forms(source, true)
        .map_err(|error| format!("failed to parse package declarations in {path}: {error}"))?;
    if scan.late_declaration_after_body {
        return Err(format!(
            "{path} package declaration after implementation body"
        ));
    }
    parse_package_declarations_from_parsed(scan.parsed, path)
}

pub fn parse_package_declarations_or_none(
    source: &str,
    path: impl Into<String>,
) -> Result<Option<PackageDescriptor>, String> {
    let path = path.into();
    let scan = scan_package_declaration_forms(source, false)?;
    if scan.parsed.forms.is_empty() {
        return Ok(None);
    }
    parse_package_declarations_from_parsed(scan.parsed, path).map(Some)
}

pub fn package_dependency_module_names(imports: &[PackageImport]) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = BTreeSet::new();
    for import in imports {
        if seen.insert(import.module_name.clone()) {
            names.push(import.module_name.clone());
        }
    }
    names
}

fn parse_package_declarations_from_parsed(
    parsed: ParsedSource,
    path: String,
) -> Result<PackageDescriptor, String> {
    let base_dir = path_base_dir(&path);
    let mut name: Option<String> = None;
    let mut imports = Vec::new();
    let mut syntax_imports = Vec::new();
    let mut exports = Vec::new();
    let mut capabilities = Vec::new();
    let mut declaration_count = 0;
    let mut implementation_seen = false;

    for form in parsed.forms {
        let Some(head) = package_declaration_head(&form).map(str::to_string) else {
            implementation_seen = true;
            continue;
        };
        if implementation_seen {
            return Err(format!(
                "{path} package declaration after implementation body"
            ));
        }
        declaration_count += 1;
        let ParsedForm::List { items, .. } = form else {
            return Err(format!(
                "{path} package declaration head requires a list form"
            ));
        };
        let args = &items[1..];
        match head.as_str() {
            "module" => {
                name = Some(
                    non_empty_string_arg(args, 0)
                        .ok_or_else(|| format!("{path} module declaration requires a name"))?,
                );
            }
            "module-capability" => {
                capabilities.push(non_empty_string_arg(args, 0).ok_or_else(|| {
                    format!("{path} module-capability declaration requires a capability")
                })?);
            }
            "import-namespace" => {
                let module_name = non_empty_string_arg(args, 0)
                    .ok_or_else(|| format!("{path} import-namespace requires module and alias"))?;
                let alias = non_empty_string_arg(args, 1)
                    .ok_or_else(|| format!("{path} import-namespace requires module and alias"))?;
                imports.push(PackageImport {
                    module_name,
                    alias,
                    symbols: Vec::new(),
                    syntax: false,
                });
            }
            "syntax-import" => {
                let module_name = non_empty_string_arg(args, 0)
                    .ok_or_else(|| format!("{path} syntax-import requires a module"))?;
                let entry = PackageImport {
                    alias: module_name.clone(),
                    module_name,
                    symbols: Vec::new(),
                    syntax: true,
                };
                imports.push(entry.clone());
                syntax_imports.push(entry);
            }
            "import-symbols" => {
                let module_name = non_empty_string_arg(args, 0)
                    .ok_or_else(|| format!("{path} import-symbols requires a module"))?;
                let symbols = args
                    .iter()
                    .skip(1)
                    .filter_map(parsed_string_value)
                    .filter(|name| !name.is_empty())
                    .map(|name| PackageImportSymbol {
                        name: name.to_string(),
                        alias: name.to_string(),
                    })
                    .collect();
                imports.push(PackageImport {
                    alias: module_name.clone(),
                    module_name,
                    symbols,
                    syntax: false,
                });
            }
            "import-as" => {
                let module_name = non_empty_string_arg(args, 0).ok_or_else(|| {
                    format!("{path} import-as requires module, source, and alias")
                })?;
                let source_name = non_empty_string_arg(args, 1).ok_or_else(|| {
                    format!("{path} import-as requires module, source, and alias")
                })?;
                let alias = non_empty_string_arg(args, 2).ok_or_else(|| {
                    format!("{path} import-as requires module, source, and alias")
                })?;
                imports.push(PackageImport {
                    alias: module_name.clone(),
                    module_name,
                    symbols: vec![PackageImportSymbol {
                        name: source_name,
                        alias,
                    }],
                    syntax: false,
                });
            }
            "export" => {
                let module_name = name.as_ref().ok_or_else(|| {
                    format!("{path} export declarations require module first and string names")
                })?;
                for arg in args {
                    let public_name = parsed_string_value(arg)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| {
                            format!(
                                "{path} export declarations require module first and string names"
                            )
                        })?;
                    exports.push(PackageExport {
                        name: public_name.to_string(),
                        path: None,
                        registry: export_registry_name(module_name, public_name),
                    });
                }
            }
            "export-as" => {
                let module_name = name.as_ref().ok_or_else(|| {
                    format!("{path} export-as declarations require module first, local, and public")
                })?;
                let local_name = non_empty_string_arg(args, 0).ok_or_else(|| {
                    format!("{path} export-as declarations require module first, local, and public")
                })?;
                let public_name = non_empty_string_arg(args, 1).ok_or_else(|| {
                    format!("{path} export-as declarations require module first, local, and public")
                })?;
                exports.push(PackageExport {
                    registry: export_registry_name(module_name, &public_name),
                    name: public_name,
                    path: Some(local_name),
                });
            }
            other => {
                return Err(format!("{path} unknown package declaration head: {other}"));
            }
        }
    }

    let name = name.ok_or_else(|| format!("{path} is missing module declaration"))?;
    Ok(PackageDescriptor {
        name,
        index_path: path,
        base_dir,
        imports,
        syntax_imports,
        exports,
        capabilities,
        declaration_count,
        state: "unloaded".to_string(),
    })
}

fn package_declaration_head(form: &ParsedForm) -> Option<&str> {
    let ParsedForm::List { items, .. } = form else {
        return None;
    };
    let Some(ParsedForm::Symbol { text, .. }) = items.first() else {
        return None;
    };
    if PACKAGE_DECLARATION_HEADS.contains(&text.as_str()) {
        Some(text)
    } else {
        None
    }
}

fn non_empty_string_arg(args: &[ParsedForm], index: usize) -> Option<String> {
    parsed_string_value(args.get(index)?)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn parsed_string_value(form: &ParsedForm) -> Option<&str> {
    match form {
        ParsedForm::String { value, .. } => Some(value),
        _ => None,
    }
}

fn source_declares_syntax_imports_with_body(source: &str) -> Result<bool, String> {
    let scan = scan_package_declaration_forms(source, false)?;
    Ok(scan.body_after_leading_declarations && parsed_forms_include_syntax_import(&scan.parsed))
}

fn parsed_forms_include_syntax_import(parsed: &ParsedSource) -> bool {
    parsed
        .forms
        .iter()
        .any(|form| package_declaration_head(form) == Some("syntax-import"))
}

struct PackageDeclarationScan {
    parsed: ParsedSource,
    body_after_leading_declarations: bool,
    late_declaration_after_body: bool,
}

fn scan_package_declaration_forms(
    source: &str,
    detect_late_declarations: bool,
) -> Result<PackageDeclarationScan, String> {
    let mut forms = Vec::new();
    let mut offset = skip_surface_trivia(source, 0)?;
    let mut implementation_seen = false;
    let mut body_after_leading_declarations = false;
    let mut late_declaration_after_body = false;

    while offset < source.len() {
        if !source[offset..].starts_with('(') {
            body_after_leading_declarations = !forms.is_empty();
            break;
        }
        let head = scan_surface_list_head(source, offset)?;
        let is_package_declaration =
            head.is_some_and(|head| PACKAGE_DECLARATION_HEADS.contains(&head));
        if !is_package_declaration {
            body_after_leading_declarations = !forms.is_empty();
            implementation_seen = true;
            if !detect_late_declarations {
                break;
            }
            let end = scan_surface_list_end(source, offset)?;
            offset = skip_surface_trivia(source, end)?;
            continue;
        }
        let end = scan_surface_list_end(source, offset)?;
        let slice = &source[offset..end];
        let parsed = parse_forms(slice).map_err(|error| {
            format!("failed to parse leading package declaration near byte {offset}: {error}")
        })?;
        let [form] = parsed.forms.as_slice() else {
            return Err("package declaration scanner expected exactly one form".to_string());
        };
        if package_declaration_head(form).is_some() {
            if implementation_seen {
                late_declaration_after_body = true;
                break;
            }
            forms.push(form.clone());
        } else {
            body_after_leading_declarations = !forms.is_empty();
            implementation_seen = true;
            if !detect_late_declarations {
                break;
            }
        }
        offset = skip_surface_trivia(source, end)?;
    }

    Ok(PackageDeclarationScan {
        parsed: ParsedSource { forms },
        body_after_leading_declarations,
        late_declaration_after_body,
    })
}

fn skip_surface_trivia(source: &str, mut offset: usize) -> Result<usize, String> {
    while offset < source.len() {
        let rest = &source[offset..];
        if let Some(ch) = rest.chars().next().filter(|ch| ch.is_whitespace()) {
            offset += ch.len_utf8();
            continue;
        }
        if rest.starts_with(';') {
            offset = rest
                .find('\n')
                .map(|index| offset + index + 1)
                .unwrap_or(source.len());
            continue;
        }
        if rest.starts_with("#|") {
            let Some(end) = rest.find("|#") else {
                return Err("unterminated #| block comment in package declarations".to_string());
            };
            offset += end + 2;
            continue;
        }
        if rest.starts_with("/*") {
            let Some(end) = rest.find("*/") else {
                return Err("unterminated /* block comment in package declarations".to_string());
            };
            offset += end + 2;
            continue;
        }
        break;
    }
    Ok(offset)
}

fn scan_surface_list_head(source: &str, offset: usize) -> Result<Option<&str>, String> {
    if !source[offset..].starts_with('(') {
        return Ok(None);
    }
    let start = skip_surface_trivia(source, offset + 1)?;
    if start >= source.len() {
        return Ok(None);
    }
    let first = source[start..].chars().next();
    if matches!(first, Some('(' | ')' | '"')) {
        return Ok(None);
    }
    let mut end = start;
    while end < source.len() {
        let Some(ch) = source[end..].chars().next() else {
            break;
        };
        if ch.is_whitespace() || matches!(ch, '(' | ')' | '"' | ';') {
            break;
        }
        end += ch.len_utf8();
    }
    if end == start {
        return Ok(None);
    }
    Ok(Some(&source[start..end]))
}

fn scan_surface_list_end(source: &str, start: usize) -> Result<usize, String> {
    let mut offset = start;
    let mut depth = 0usize;
    while offset < source.len() {
        let rest = &source[offset..];
        if rest.starts_with(';') {
            offset = rest
                .find('\n')
                .map(|index| offset + index + 1)
                .unwrap_or(source.len());
            continue;
        }
        if rest.starts_with("#|") {
            let Some(end) = rest.find("|#") else {
                return Err("unterminated #| block comment in package declaration".to_string());
            };
            offset += end + 2;
            continue;
        }
        if rest.starts_with("/*") {
            let Some(end) = rest.find("*/") else {
                return Err("unterminated /* block comment in package declaration".to_string());
            };
            offset += end + 2;
            continue;
        }
        let ch = rest
            .chars()
            .next()
            .ok_or_else(|| "unexpected end of package declaration".to_string())?;
        match ch {
            '"' => offset = scan_surface_string_end(source, offset, '"')?,
            '(' => {
                depth += 1;
                offset += 1;
            }
            ')' => {
                if depth == 0 {
                    return Err("unbalanced ')' in package declaration".to_string());
                }
                depth -= 1;
                offset += 1;
                if depth == 0 {
                    return Ok(offset);
                }
            }
            _ => offset += ch.len_utf8(),
        }
    }
    Err(format!(
        "unterminated package declaration starting at byte {start}"
    ))
}

fn scan_surface_string_end(source: &str, start: usize, delimiter: char) -> Result<usize, String> {
    let mut offset = start + delimiter.len_utf8();
    let mut escaped = false;
    while offset < source.len() {
        let ch = source[offset..]
            .chars()
            .next()
            .ok_or_else(|| "unexpected end of string literal".to_string())?;
        offset += ch.len_utf8();
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == delimiter {
            return Ok(offset);
        }
    }
    Err("unterminated string literal in package declaration".to_string())
}

fn export_registry_name(module_name: &str, public_name: &str) -> String {
    if public_name == module_name
        || public_name.starts_with("stdlib.")
        || public_name.starts_with("sys.")
        || public_name.starts_with("example.")
    {
        public_name.to_string()
    } else {
        format!("{module_name}.{public_name}")
    }
}

fn path_base_dir(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(path_to_string_lossy)
        .unwrap_or_default()
}

fn path_to_string_lossy(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn resolve_source_path(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("source path must be non-empty".to_string());
    }
    std::fs::canonicalize(path).map_err(|error| {
        format!(
            "source path resolution failed for {}: {error}",
            path.display()
        )
    })
}

fn source_path_token(path: &Path) -> Result<String, String> {
    let metadata =
        std::fs::metadata(path).map_err(|error| format!("source path metadata failed: {error}"))?;
    let modified = metadata
        .modified()
        .map_err(|error| format!("source path modified time failed: {error}"))?
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("source path modified time is before unix epoch: {error}"))?;
    Ok(format!(
        "len:{}:modified:{}:{}",
        metadata.len(),
        modified.as_secs(),
        modified.subsec_nanos()
    ))
}

fn elapsed_ms_string(started: Instant) -> String {
    format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0)
}

fn path_to_string(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| "source path is not valid UTF-8".to_string())
}

pub fn bootstrap_image_file_fingerprint(path: impl AsRef<Path>) -> Result<String, String> {
    let path = path.as_ref();
    let text = fs::read_to_string(path).map_err(|error| {
        format!(
            "failed to read bootstrap image file {}: {error}",
            path.display()
        )
    })?;
    Ok(ArtifactFingerprint::sha256(text.as_bytes()).to_string())
}

fn query_stage_cache_key(
    unit: &Unit,
    stage: &str,
    phase: PhasePolicy,
    initial_bindings: &[(String, RuntimeValue)],
    provider_registry_version: u64,
    compiler_registry_version: u64,
    host_version: u64,
) -> Result<ArtifactKey, String> {
    let mut parts = vec![
        "query-stage".to_string(),
        stage.to_string(),
        phase.as_str().to_string(),
        unit.unit_id().to_string(),
        unit.version().to_string(),
        "provider-registry".to_string(),
        provider_registry_version.to_string(),
        "compiler-registry".to_string(),
        compiler_registry_version.to_string(),
        "host".to_string(),
        host_version.to_string(),
    ];
    parts.extend(initial_bindings_identity_token(initial_bindings));
    Ok(ArtifactKey::new(parts)?)
}

fn provider_cache_key(
    compiler: &Compiler,
    unit: &Unit,
    provider: &QueryProvider,
    phase: PhasePolicy,
    initial_bindings: &[(String, RuntimeValue)],
) -> Result<Option<ArtifactKey>, String> {
    if !provider_cacheable(provider) {
        return Ok(None);
    }
    let mut parts = vec![
        "provider-cache".to_string(),
        provider.name.clone(),
        provider.stage.clone(),
        provider.cache_scope.clone(),
        phase.as_str().to_string(),
        unit.unit_id().to_string(),
        unit.version().to_string(),
        compiler.provider_registry.version().to_string(),
        compiler.registry.version().to_string(),
        compiler.host.host_version().to_string(),
    ];
    parts.extend(initial_bindings_identity_token(initial_bindings));
    Ok(ArtifactKey::new(parts).map(Some)?)
}

fn provider_cacheable(provider: &QueryProvider) -> bool {
    provider.cache_scope != "none"
        && !provider
            .reads
            .iter()
            .any(|read| matches!(read.as_str(), "files" | "file" | "fs"))
        && !provider.effect_tags.iter().any(|tag| {
            matches!(
                tag.as_str(),
                "emit_events"
                    | "use_host_services"
                    | "host_services"
                    | "read_files"
                    | "read-files"
                    | "use_files"
                    | "use-files"
            )
        })
}

fn capture_provider_rollback_snapshot(
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

fn restore_provider_rollback_snapshot(
    unit: &mut Unit,
    snapshot: ProviderRollbackSnapshot,
) -> Result<(), String> {
    match snapshot {
        ProviderRollbackSnapshot::Semantic(snapshot) => {
            Ok(unit.semantics_mut().restore_snapshot(snapshot)?)
        }
        ProviderRollbackSnapshot::Attributes(snapshot) => {
            unit.restore_attribute_snapshot(snapshot);
            Ok(())
        }
        ProviderRollbackSnapshot::Unit(snapshot) => {
            unit.restore_snapshot(*snapshot);
            Ok(())
        }
    }
}

fn provider_transaction_mode(provider: &QueryProvider) -> ProviderTransactionMode {
    let mut unit_writes: BTreeSet<String> = provider
        .writes
        .iter()
        .filter(|domain| {
            matches!(
                domain.as_str(),
                "ir" | "attributes" | "facts" | "symbols" | "types"
            )
        })
        .cloned()
        .collect();
    if unit_writes.is_empty() {
        return ProviderTransactionMode::None;
    }
    if unit_writes
        .iter()
        .all(|domain| matches!(domain.as_str(), "facts" | "symbols" | "types"))
    {
        return ProviderTransactionMode::Semantic;
    }
    if unit_writes.len() == 1 && unit_writes.remove("attributes") {
        return ProviderTransactionMode::Attributes;
    }
    ProviderTransactionMode::Unit
}

fn provider_restart_stage(
    compiler: &Compiler,
    provider: &QueryProvider,
    committed_change: bool,
) -> Result<Option<String>, String> {
    if let Some(stage) = compiler.pending_query_restart.clone() {
        return Ok(Some(stage));
    }
    if !committed_change {
        return Ok(None);
    }
    if provider.resume_policy == "never" {
        return Ok(None);
    }
    if provider.resume_policy == "bootstrap_safe" && compiler.active_bootstrap_depth > 0 {
        return Ok(None);
    }
    if provider.effect_tags.iter().any(|tag| {
        matches!(
            tag.as_str(),
            "use_host_services" | "use-host-services" | "host_services" | "host-services"
        )
    }) {
        return Ok(None);
    }
    compiler
        .provider_registry
        .explicit_restart_stage_for(provider.stage.clone())
}

fn initial_bindings_identity_token(initial_bindings: &[(String, RuntimeValue)]) -> Vec<String> {
    if initial_bindings.is_empty() {
        return Vec::new();
    }
    let mut items: Vec<(String, String)> = initial_bindings
        .iter()
        .map(|(name, value)| (name.clone(), runtime_value_identity_token(value)))
        .collect();
    items.sort();
    items
        .into_iter()
        .flat_map(|(name, value)| ["initial-binding".to_string(), name, value])
        .collect()
}

fn normalize_initial_bindings(
    initial_bindings: Vec<(String, RuntimeValue)>,
) -> Result<Vec<(String, RuntimeValue)>, String> {
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::with_capacity(initial_bindings.len());
    for (name, value) in initial_bindings {
        let name = require_registry_name(name)?;
        if !seen.insert(name.clone()) {
            return Err(format!("query initial binding {name:?} is duplicated"));
        }
        normalized.push((name, value));
    }
    normalized.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(normalized)
}

fn runtime_value_identity_token(value: &RuntimeValue) -> String {
    match value {
        RuntimeValue::Null => "null".to_string(),
        RuntimeValue::Bool(value) => format!("bool:{value}"),
        RuntimeValue::Int(value) => format!("int:{value}"),
        RuntimeValue::Float(value) => format!("float:{value:?}"),
        RuntimeValue::Str(value) => format!("str:{value}"),
        RuntimeValue::Tuple(items) => {
            let items = items
                .iter()
                .map(runtime_value_identity_token)
                .collect::<Vec<_>>()
                .join(",");
            format!("tuple:[{items}]")
        }
        RuntimeValue::List(items) => {
            let items = items
                .borrow()
                .iter()
                .map(runtime_value_identity_token)
                .collect::<Vec<_>>()
                .join(",");
            format!("list:[{items}]")
        }
        RuntimeValue::Map(map) => {
            let mut items: Vec<(String, String)> = map
                .borrow()
                .iter()
                .map(|(key, value)| {
                    (
                        map_key_identity_token(key),
                        runtime_value_identity_token(value),
                    )
                })
                .collect();
            items.sort();
            let items = items
                .into_iter()
                .map(|(key, value)| format!("{key}:{value}"))
                .collect::<Vec<_>>()
                .join(",");
            format!("map:{{{items}}}")
        }
        RuntimeValue::Closure(value) => format!("closure:{:p}", Rc::as_ptr(value)),
        RuntimeValue::Builtin(value) => {
            format!("builtin:{}:{:p}", value.name, Rc::as_ptr(value))
        }
        RuntimeValue::HostFunction(value) => {
            format!("host-function:{}:{:p}", value.name, Rc::as_ptr(value))
        }
        RuntimeValue::HostObject(value) => {
            format!("host-object:{}:{:p}", value.type_name(), Rc::as_ptr(value))
        }
        RuntimeValue::UninitializedTopLevel => "uninitialized-top-level".to_string(),
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

fn query_step_invalidation(
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

fn provider_execution_record(
    provider: &QueryProvider,
    iteration: usize,
    changed: bool,
    diagnostics_emitted: usize,
    rolled_back: bool,
    stopped_by_error: bool,
    outcome_kind: impl Into<String>,
    diagnostic_codes: Vec<String>,
    ir_change: ProviderIrChangeStats,
    restart_stage: Option<String>,
    context: Option<&QueryProviderContext>,
) -> QueryProviderExecutionRecord {
    let restart_requested = restart_stage.is_some();
    let reads_subjects = merge_runtime_tracking(provider.reads.clone(), context, |context| {
        &context.reads_subjects
    });
    let writes_subjects = merge_runtime_tracking(provider.writes.clone(), context, |context| {
        &context.writes_subjects
    });
    let read_cells = merge_runtime_tracking(provider.reads.clone(), context, |context| {
        &context.read_cells
    });
    let write_cells = merge_runtime_tracking(provider.writes.clone(), context, |context| {
        &context.write_cells
    });
    let reads_files = context
        .map(|context| context.reads_files.clone())
        .unwrap_or_default();
    let writes_files = context
        .map(|context| context.writes_files.clone())
        .unwrap_or_default();
    let artifact_dependencies = context
        .map(|context| context.artifact_dependencies.clone())
        .unwrap_or_default();
    QueryProviderExecutionRecord {
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
        cache_scope: provider.cache_scope.clone(),
        resume_policy: provider.resume_policy.clone(),
        iteration,
        changed,
        diagnostics_emitted,
        rolled_back,
        stopped_by_error,
        outcome_kind: outcome_kind.into(),
        diagnostic_codes,
        rewrite_count: ir_change.rewrite_count,
        erased_count: ir_change.erased_count,
        touched_node_kinds: ir_change.touched_node_kinds,
        change_domains: ir_change.change_domains,
        restart_requested,
        restart_stage,
        outcome_summary: Vec::new(),
    }
}

fn cached_provider_execution_record(
    provider: &QueryProvider,
    iteration: usize,
    entry: &ProviderCacheEntry,
) -> QueryProviderExecutionRecord {
    QueryProviderExecutionRecord {
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
        cache_scope: provider.cache_scope.clone(),
        resume_policy: provider.resume_policy.clone(),
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

fn provider_cache_entry(
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

fn collect_record_strings(
    records: &[QueryProviderExecutionRecord],
    selector: impl Fn(&QueryProviderExecutionRecord) -> &Vec<String>,
) -> Vec<String> {
    let mut values = BTreeSet::new();
    for record in records {
        values.extend(selector(record).iter().cloned());
    }
    values.into_iter().collect()
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
struct ProviderIrChangeStats {
    rewrite_count: usize,
    erased_count: usize,
    touched_node_kinds: Vec<String>,
    change_domains: Vec<String>,
}

fn provider_ir_change_stats(before_nodes: &[Node], unit: &Unit) -> ProviderIrChangeStats {
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

fn query_stage_artifact_value(
    unit: &Unit,
    step: &QueryPlanStep,
    phase: PhasePolicy,
    execution_records: &[QueryProviderExecutionRecord],
) -> Result<SemanticValue, String> {
    Ok(SemanticValue::map([
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
            "providers".to_string(),
            SemanticValue::List(
                step.provider_names
                    .iter()
                    .cloned()
                    .map(SemanticValue::Str)
                    .collect(),
            ),
        ),
        (
            "provider_count".to_string(),
            SemanticValue::Int(step.provider_names.len() as i64),
        ),
        (
            "effect_tags".to_string(),
            SemanticValue::List(
                step.effect_tags
                    .iter()
                    .cloned()
                    .map(SemanticValue::Str)
                    .collect(),
            ),
        ),
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
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        ),
    ])?)
}

fn cached_execution_records(value: &ArtifactValue) -> Vec<QueryProviderExecutionRecord> {
    let ArtifactValue::Semantic(SemanticValue::Map(entries)) = value else {
        return Vec::new();
    };
    semantic_map_get(entries, "execution_summary")
        .and_then(|value| match value {
            SemanticValue::List(records) => Some(
                records
                    .iter()
                    .filter_map(query_provider_execution_record_from_semantic_value)
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

fn provider_execution_record_to_semantic_value(
    record: &QueryProviderExecutionRecord,
) -> Result<SemanticValue, String> {
    Ok(SemanticValue::map([
        (
            "provider_name".to_string(),
            SemanticValue::Str(record.provider_name.clone()),
        ),
        (
            "stage".to_string(),
            SemanticValue::Str(record.stage.clone()),
        ),
        (
            "family".to_string(),
            optional_semantic_string(record.family.as_deref()),
        ),
        (
            "phase_policy".to_string(),
            SemanticValue::Str(record.phase_policy.as_str().to_string()),
        ),
        (
            "effect_tags".to_string(),
            semantic_string_list(&record.effect_tags),
        ),
        (
            "requires".to_string(),
            semantic_string_list(&record.requires),
        ),
        (
            "requires_data".to_string(),
            semantic_string_list(&record.requires_data),
        ),
        (
            "provides_data".to_string(),
            semantic_string_list(&record.provides_data),
        ),
        (
            "provides".to_string(),
            semantic_string_list(&record.provides),
        ),
        ("reads".to_string(), semantic_string_list(&record.reads)),
        ("writes".to_string(), semantic_string_list(&record.writes)),
        (
            "artifact_dependencies".to_string(),
            semantic_artifact_key_list(&record.artifact_dependencies),
        ),
        (
            "reads_subjects".to_string(),
            semantic_string_list(&record.reads_subjects),
        ),
        (
            "writes_subjects".to_string(),
            semantic_string_list(&record.writes_subjects),
        ),
        (
            "read_cells".to_string(),
            semantic_string_list(&record.read_cells),
        ),
        (
            "write_cells".to_string(),
            semantic_string_list(&record.write_cells),
        ),
        (
            "reads_files".to_string(),
            semantic_string_list(&record.reads_files),
        ),
        (
            "writes_files".to_string(),
            semantic_string_list(&record.writes_files),
        ),
        (
            "cache_scope".to_string(),
            SemanticValue::Str(record.cache_scope.clone()),
        ),
        (
            "resume_policy".to_string(),
            SemanticValue::Str(record.resume_policy.clone()),
        ),
        (
            "iteration".to_string(),
            SemanticValue::Int(record.iteration as i64),
        ),
        ("changed".to_string(), SemanticValue::Bool(record.changed)),
        (
            "diagnostics_emitted".to_string(),
            SemanticValue::Int(record.diagnostics_emitted as i64),
        ),
        (
            "rolled_back".to_string(),
            SemanticValue::Bool(record.rolled_back),
        ),
        (
            "stopped_by_error".to_string(),
            SemanticValue::Bool(record.stopped_by_error),
        ),
        (
            "outcome_kind".to_string(),
            SemanticValue::Str(record.outcome_kind.clone()),
        ),
        (
            "diagnostic_codes".to_string(),
            semantic_string_list(&record.diagnostic_codes),
        ),
        (
            "rewrite_count".to_string(),
            SemanticValue::Int(record.rewrite_count as i64),
        ),
        (
            "erased_count".to_string(),
            SemanticValue::Int(record.erased_count as i64),
        ),
        (
            "touched_node_kinds".to_string(),
            semantic_string_list(&record.touched_node_kinds),
        ),
        (
            "change_domains".to_string(),
            semantic_string_list(&record.change_domains),
        ),
        (
            "restart_requested".to_string(),
            SemanticValue::Bool(record.restart_requested),
        ),
        (
            "restart_stage".to_string(),
            optional_semantic_string(record.restart_stage.as_deref()),
        ),
        (
            "outcome_summary".to_string(),
            SemanticValue::map(
                record
                    .outcome_summary
                    .iter()
                    .map(|(key, value)| (key.clone(), SemanticValue::Str(value.clone()))),
            )?,
        ),
    ])?)
}

fn query_provider_execution_record_from_semantic_value(
    value: &SemanticValue,
) -> Option<QueryProviderExecutionRecord> {
    let SemanticValue::Map(entries) = value else {
        return None;
    };
    Some(QueryProviderExecutionRecord {
        provider_name: semantic_map_string(entries, "provider_name")?,
        stage: semantic_map_string(entries, "stage")?,
        family: semantic_map_optional_string(entries, "family")?,
        phase_policy: semantic_map_phase_policy(entries, "phase_policy")?,
        effect_tags: semantic_map_string_list(entries, "effect_tags")?,
        requires: semantic_map_string_list(entries, "requires")?,
        requires_data: semantic_map_string_list(entries, "requires_data").unwrap_or_default(),
        provides_data: semantic_map_string_list(entries, "provides_data").unwrap_or_default(),
        provides: semantic_map_string_list(entries, "provides")?,
        reads: semantic_map_string_list(entries, "reads")?,
        writes: semantic_map_string_list(entries, "writes")?,
        artifact_dependencies: semantic_map_artifact_key_list(entries, "artifact_dependencies")
            .unwrap_or_default(),
        reads_subjects: semantic_map_string_list(entries, "reads_subjects")
            .unwrap_or_else(|| semantic_map_string_list(entries, "reads").unwrap_or_default()),
        writes_subjects: semantic_map_string_list(entries, "writes_subjects")
            .unwrap_or_else(|| semantic_map_string_list(entries, "writes").unwrap_or_default()),
        read_cells: semantic_map_string_list(entries, "read_cells")
            .unwrap_or_else(|| semantic_map_string_list(entries, "reads").unwrap_or_default()),
        write_cells: semantic_map_string_list(entries, "write_cells")
            .unwrap_or_else(|| semantic_map_string_list(entries, "writes").unwrap_or_default()),
        reads_files: semantic_map_string_list(entries, "reads_files").unwrap_or_default(),
        writes_files: semantic_map_string_list(entries, "writes_files").unwrap_or_default(),
        cache_scope: semantic_map_string(entries, "cache_scope")?,
        resume_policy: semantic_map_string(entries, "resume_policy")?,
        iteration: semantic_map_usize(entries, "iteration")?,
        changed: semantic_map_bool(entries, "changed")?,
        diagnostics_emitted: semantic_map_usize(entries, "diagnostics_emitted")?,
        rolled_back: semantic_map_bool(entries, "rolled_back")?,
        stopped_by_error: semantic_map_bool(entries, "stopped_by_error")?,
        outcome_kind: semantic_map_string(entries, "outcome_kind")?,
        diagnostic_codes: semantic_map_string_list(entries, "diagnostic_codes")?,
        rewrite_count: semantic_map_usize(entries, "rewrite_count")?,
        erased_count: semantic_map_usize(entries, "erased_count")?,
        touched_node_kinds: semantic_map_string_list(entries, "touched_node_kinds")?,
        change_domains: semantic_map_string_list(entries, "change_domains")?,
        restart_requested: semantic_map_bool(entries, "restart_requested")?,
        restart_stage: semantic_map_optional_string(entries, "restart_stage")?,
        outcome_summary: semantic_map_string_pairs(entries, "outcome_summary")?,
    })
}

fn semantic_string_list(values: &[String]) -> SemanticValue {
    SemanticValue::List(values.iter().cloned().map(SemanticValue::Str).collect())
}

fn semantic_artifact_key_list(values: &[ArtifactKey]) -> SemanticValue {
    SemanticValue::List(
        values
            .iter()
            .map(|key| {
                SemanticValue::List(
                    key.parts()
                        .iter()
                        .cloned()
                        .map(SemanticValue::Str)
                        .collect(),
                )
            })
            .collect(),
    )
}

fn optional_semantic_string(value: Option<&str>) -> SemanticValue {
    value
        .map(|value| SemanticValue::Str(value.to_string()))
        .unwrap_or(SemanticValue::Null)
}

fn semantic_map_get<'a>(
    entries: &'a [(String, SemanticValue)],
    key: &str,
) -> Option<&'a SemanticValue> {
    entries
        .iter()
        .find_map(|(entry_key, value)| (entry_key == key).then_some(value))
}

fn semantic_map_string(entries: &[(String, SemanticValue)], key: &str) -> Option<String> {
    match semantic_map_get(entries, key)? {
        SemanticValue::Str(value) => Some(value.clone()),
        _ => None,
    }
}

fn semantic_map_optional_string(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Option<Option<String>> {
    match semantic_map_get(entries, key)? {
        SemanticValue::Null => Some(None),
        SemanticValue::Str(value) => Some(Some(value.clone())),
        _ => None,
    }
}

fn semantic_map_string_list(entries: &[(String, SemanticValue)], key: &str) -> Option<Vec<String>> {
    match semantic_map_get(entries, key)? {
        SemanticValue::List(values) => values
            .iter()
            .map(|value| match value {
                SemanticValue::Str(value) => Some(value.clone()),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn semantic_map_artifact_key_list(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Option<Vec<ArtifactKey>> {
    match semantic_map_get(entries, key)? {
        SemanticValue::List(values) => values
            .iter()
            .map(|value| match value {
                SemanticValue::List(parts) => {
                    let parts: Option<Vec<String>> = parts
                        .iter()
                        .map(|part| match part {
                            SemanticValue::Str(part) => Some(part.clone()),
                            _ => None,
                        })
                        .collect();
                    ArtifactKey::new(parts?).ok()
                }
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn semantic_map_bool(entries: &[(String, SemanticValue)], key: &str) -> Option<bool> {
    match semantic_map_get(entries, key)? {
        SemanticValue::Bool(value) => Some(*value),
        _ => None,
    }
}

fn semantic_map_usize(entries: &[(String, SemanticValue)], key: &str) -> Option<usize> {
    match semantic_map_get(entries, key)? {
        SemanticValue::Int(value) if *value >= 0 => Some(*value as usize),
        _ => None,
    }
}

fn semantic_map_phase_policy(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Option<PhasePolicy> {
    match semantic_map_string(entries, key)?.as_str() {
        "runtime" => Some(PhasePolicy::Runtime),
        "compile_time" => Some(PhasePolicy::CompileTime),
        "dual" => Some(PhasePolicy::Dual),
        _ => None,
    }
}

fn semantic_map_string_pairs(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Option<Vec<(String, String)>> {
    match semantic_map_get(entries, key)? {
        SemanticValue::Map(values) => values
            .iter()
            .map(|(key, value)| match value {
                SemanticValue::Str(value) => Some((key.clone(), value.clone())),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn evaluate_unit(
    unit: &Unit,
    initial: impl IntoIterator<Item = (String, RuntimeValue)>,
) -> EvalResult {
    evaluate_unit_capture(unit, initial, 0)
}

fn evaluate_unit_capture(
    unit: &Unit,
    initial: impl IntoIterator<Item = (String, RuntimeValue)>,
    skip_leading_forms: usize,
) -> EvalResult {
    evaluate_unit_capture_with_bindings(unit, initial, skip_leading_forms).map(|(value, _)| value)
}

fn evaluate_unit_capture_with_bindings(
    unit: &Unit,
    initial: impl IntoIterator<Item = (String, RuntimeValue)>,
    skip_leading_forms: usize,
) -> Result<(RuntimeValue, Vec<(String, RuntimeValue)>), EvalSignal> {
    let top_level_names = unit
        .semantics()
        .symbols()
        .values()
        .filter(|entry| entry.kind == SymbolKind::TopLevel)
        .map(|entry| entry.name.clone())
        .collect();
    let mut evaluator = Evaluator::with_top_level_names(unit.ir().clone(), top_level_names);
    let env = evaluator.make_env();
    for (name, value) in initial {
        if name.is_empty() {
            return Err(EvalSignal::Error(crate::values::EvaluationError::new(
                "initial binding name must be non-empty",
            )));
        }
        Environment::define(&env, name, value);
    }
    let forms = evaluator.graph().top_level_form_ids().to_vec();
    if skip_leading_forms > forms.len() {
        return Err(EvalSignal::Error(crate::values::EvaluationError::new(
            "cannot skip more forms than unit contains",
        )));
    }
    let value = evaluator.eval_top_level_sequence(&forms[skip_leading_forms..], &env)?;
    Ok((value, capture_environment_bindings(&env)))
}

fn capture_environment_bindings(env: &crate::values::EnvRef) -> Vec<(String, RuntimeValue)> {
    let mut bindings: Vec<(String, RuntimeValue)> = env
        .borrow()
        .values
        .iter()
        .filter(|(_, value)| !matches!(value, RuntimeValue::UninitializedTopLevel))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    bindings.sort_by(|left, right| left.0.cmp(&right.0));
    bindings
}

fn bootstrap_declaration_form_count(unit: &Unit) -> usize {
    unit.top_level_form_ids()
        .iter()
        .take_while(|node_id| is_bootstrap_declaration_form(unit, **node_id))
        .count()
}

fn is_bootstrap_declaration_form(unit: &Unit, node_id: NodeId) -> bool {
    let Some(Node::Call(call)) = unit.ir().node(node_id) else {
        return false;
    };
    let Some(Node::Name(callee)) = unit.ir().node(call.callee) else {
        return false;
    };
    PACKAGE_DECLARATION_HEADS.contains(&callee.identifier.as_ref())
}

fn invoke_registered_provider_callback(
    callback: RuntimeValue,
    compiler: &mut Compiler,
    unit: &mut Unit,
) -> Result<QueryProviderCallbackOutcome, String> {
    let compiler_bridge = Rc::new(CompilerBridgeValue::from_compiler(compiler));
    let unit_bridge = Rc::new(UnitBridgeValue::from_unit(unit));
    let Some(active_context) = compiler.active_provider_context.clone() else {
        return Err("query provider callback requires an active provider context".to_string());
    };
    let context_bridge = Rc::new(ProviderContextBridgeValue::new(
        active_context.clone(),
        Rc::clone(&compiler_bridge),
        Rc::clone(&unit_bridge),
    ));
    let root_id = unit_bridge.with_unit(Unit::root_id);
    let unit_object: Rc<dyn HostObject> = unit_bridge.clone();
    let root_handle = RuntimeValue::HostObject(Rc::new(NodeBridgeValue::new(unit_object, root_id)));
    let mut evaluator = Evaluator::new(Default::default());
    let args = registered_provider_callback_args(
        &callback,
        RuntimeValue::HostObject(compiler_bridge.clone()),
        RuntimeValue::HostObject(unit_bridge.clone()),
        RuntimeValue::HostObject(context_bridge.clone()),
        root_handle,
    );
    let mut initial_bindings = active_context.initial_bindings;
    initial_bindings.push((
        "compiler".to_string(),
        RuntimeValue::HostObject(compiler_bridge.clone()),
    ));
    initial_bindings.push((
        "unit".to_string(),
        RuntimeValue::HostObject(unit_bridge.clone()),
    ));
    let result =
        invoke_provider_callback_with_initial(&mut evaluator, &callback, args, &initial_bindings);
    let mut tracked_context = context_bridge.tracked_context();
    if let Some(active_context) = compiler_bridge.active_provider_context.borrow().as_ref() {
        merge_provider_context_tracking(&mut tracked_context, active_context);
    }
    *compiler_bridge.active_provider_context.borrow_mut() = Some(tracked_context);
    compiler_bridge.apply_to_compiler(compiler);
    unit_bridge.apply_to_unit(unit);
    result
        .map(|value| provider_callback_outcome_from_value(&value))
        .map_err(eval_signal_message)
}

fn provider_callback_outcome_from_value(value: &RuntimeValue) -> QueryProviderCallbackOutcome {
    QueryProviderCallbackOutcome::changed(provider_result_changed(value))
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
    compiler: RuntimeValue,
    unit: RuntimeValue,
    context: RuntimeValue,
    root: RuntimeValue,
) -> Vec<RuntimeValue> {
    match callback {
        RuntimeValue::Closure(closure) if closure.params.len() == 3 => {
            vec![compiler, unit, context]
        }
        RuntimeValue::HostFunction(host)
            if host.min_arity <= 3 && host.max_arity.is_none_or(|max| max >= 3) =>
        {
            vec![compiler, unit, context]
        }
        _ => vec![context, root],
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
    }
}

fn live_trace_event_line(event: &CompilerEvent) -> String {
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

fn should_emit_live_trace(event: &CompilerEvent) -> bool {
    if std::env::var_os("CAAP_RUST_LIVE_TRACE").is_none() {
        return false;
    }
    let Some(filter) = std::env::var("CAAP_RUST_LIVE_TRACE_FILTER")
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

fn compile_error_message(diagnostics: &[Diagnostic]) -> Option<String> {
    let errors: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
        .collect();
    if errors.is_empty() {
        return None;
    }
    let rendered = errors
        .into_iter()
        .map(|diagnostic| render_diagnostic(diagnostic, None))
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!("CAAP compilation failed:\n{rendered}"))
}
