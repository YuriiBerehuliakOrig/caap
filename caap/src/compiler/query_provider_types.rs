use std::collections::BTreeSet;
use std::fmt;
use std::rc::Rc;

use crate::artifacts::ArtifactKey;
use crate::diagnostics::Diagnostic;
use crate::error::{CaapError, CaapResult};
use crate::ir::{ExprSpec, NodeId};
use crate::semantic::{BuiltinEffectTag, EffectSet, PhasePolicy, SemanticSubjectId, SemanticValue};
use crate::unit::{Unit, UnitAttributeSnapshot, UnitSnapshot};
use crate::values::RuntimeValue;

use super::session::Compiler;

const PROVIDER_DOMAINS: &[&str] = &[
    "attributes",
    "cache",
    "diagnostics",
    "events",
    "facts",
    "files",
    "host_services",
    "ir",
    "registry",
    "symbols",
    "unit",
];

// ──────────────────────────────────────────────────────────────
// Public callback type alias
// ──────────────────────────────────────────────────────────────

pub type QueryProviderCallback =
    dyn for<'a> Fn(&mut NativeProviderContext<'a>) -> Result<QueryProviderCallbackOutcome, String>;

pub struct NativeProviderContext<'a> {
    compiler: &'a mut Compiler,
    unit: &'a mut Unit,
}

impl<'a> NativeProviderContext<'a> {
    pub(super) fn new(compiler: &'a mut Compiler, unit: &'a mut Unit) -> Self {
        Self { compiler, unit }
    }

    pub(super) fn with_isolated_state<R>(
        &mut self,
        run: impl FnOnce(Compiler, Unit) -> (Compiler, Unit, R),
    ) -> R {
        let (compiler, unit, result) = run(self.compiler.clone(), self.unit.clone());
        *self.compiler = compiler;
        *self.unit = unit;
        result
    }

    pub fn unit_id(&self) -> &str {
        self.unit.unit_id()
    }

    pub fn unit_attribute(&mut self, key: &str) -> Result<Option<SemanticValue>, String> {
        self.require_effect(BuiltinEffectTag::ReadAttributes)?;
        self.compiler
            .track_active_unit_cell_read(self.unit.unit_id(), "attributes");
        Ok(self.unit.attributes().get(key).cloned())
    }

    pub fn active_provider_context(&self) -> Option<&QueryProviderContext> {
        self.compiler.active_provider_context()
    }

    fn require_effect(&self, effect: BuiltinEffectTag) -> Result<(), String> {
        let Some(context) = self.active_provider_context() else {
            return Err(format!(
                "native provider operation requires active provider context for {}",
                effect.as_str()
            ));
        };
        if context.effect_tags.contains_builtin(effect) {
            Ok(())
        } else {
            Err(format!(
                "provider {} does not declare required effect {}",
                context.provider,
                effect.as_str()
            ))
        }
    }

    pub fn set_unit_attribute(
        &mut self,
        key: impl Into<String>,
        value: SemanticValue,
    ) -> Result<(), String> {
        self.require_effect(BuiltinEffectTag::WriteAttributes)?;
        self.compiler
            .track_active_unit_cell_write(self.unit.unit_id(), "attributes");
        self.unit
            .set_attribute(key, value)
            .map_err(|error| error.to_string())
    }

    pub fn append_ir_top_level_with_spec(&mut self, spec: &ExprSpec) -> Result<NodeId, String> {
        self.require_effect(BuiltinEffectTag::WriteIr)?;
        self.unit
            .append_ir_top_level_with_spec(spec)
            .map_err(|error| error.to_string())
    }

    pub fn set_unit_fact(
        &mut self,
        subject: SemanticSubjectId,
        key: impl Into<String>,
        value: SemanticValue,
    ) -> Result<(), String> {
        self.require_effect(BuiltinEffectTag::WriteFacts)?;
        self.unit
            .semantics_mut()
            .map_err(|error| error.to_string())?
            .set_fact(subject, key, value)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub fn push_diagnostic(&mut self, diagnostic: Diagnostic) -> Result<(), String> {
        self.require_effect(BuiltinEffectTag::EmitDiagnostics)?;
        self.compiler
            .push_diagnostic(diagnostic)
            .map_err(|error| error.to_string())
    }

    pub fn request_query_restart(&mut self, stage: impl Into<String>) -> Result<(), String> {
        self.require_effect(BuiltinEffectTag::RequestRestart)?;
        self.compiler
            .request_query_restart(stage)
            .map_err(|error| error.to_string())
    }
}

// ──────────────────────────────────────────────────────────────
// Stage specification
// ──────────────────────────────────────────────────────────────

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

impl QueryStageSpec {
    pub fn new(name: impl Into<String>) -> CaapResult<Self> {
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

    pub fn with_requires(mut self, requires: impl IntoIterator<Item = String>) -> CaapResult<Self> {
        self.requires = normalize_unique_labels(requires, "compiler stage dependency")?;
        Ok(self)
    }

    pub fn with_aliases(mut self, aliases: impl IntoIterator<Item = String>) -> CaapResult<Self> {
        self.aliases = normalize_unique_labels(aliases, "compiler stage alias")?;
        Ok(self)
    }

    pub fn with_input_kinds(
        mut self,
        input_kinds: impl IntoIterator<Item = String>,
    ) -> CaapResult<Self> {
        self.input_kinds = normalize_unique_labels(input_kinds, "compiler stage input kind")?;
        Ok(self)
    }

    pub fn with_family_label(mut self, family_label: impl Into<String>) -> CaapResult<Self> {
        self.family_label = Some(normalize_stage_name(family_label.into())?);
        Ok(self)
    }

    pub fn with_restart_stage(mut self, restart_stage: impl Into<String>) -> CaapResult<Self> {
        self.restart_stage = Some(normalize_stage_name(restart_stage.into())?);
        Ok(self)
    }
}

// ──────────────────────────────────────────────────────────────
// Provider
// ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum QueryProviderCacheScope {
    #[default]
    None,
    Unit,
}

impl QueryProviderCacheScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Unit => "unit",
        }
    }

    pub fn is_cacheable(self) -> bool {
        self != Self::None
    }
}

impl fmt::Display for QueryProviderCacheScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum QueryProviderResumePolicy {
    #[default]
    Safe,
    Never,
    BootstrapSafe,
}

impl QueryProviderResumePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Never => "never",
            Self::BootstrapSafe => "bootstrap_safe",
        }
    }

    pub fn allows_restart(self, bootstrap_active: bool) -> bool {
        match self {
            Self::Safe => true,
            Self::Never => false,
            Self::BootstrapSafe => !bootstrap_active,
        }
    }
}

impl fmt::Display for QueryProviderResumePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
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
    pub effect_tags: EffectSet,
    pub input_schema: Option<String>,
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    pub cache_scope: QueryProviderCacheScope,
    pub resume_policy: QueryProviderResumePolicy,
    pub registration_index: u64,
    pub enforce_effect_postconditions: bool,
    pub(super) callback: Rc<QueryProviderCallback>,
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
            .field(
                "enforce_effect_postconditions",
                &self.enforce_effect_postconditions,
            )
            .finish_non_exhaustive()
    }
}

// ──────────────────────────────────────────────────────────────
// Callback outcome
// ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QueryProviderCallbackOutcome {
    pub changed: bool,
}

impl QueryProviderCallbackOutcome {
    pub fn unchanged() -> Self {
        Self { changed: false }
    }

    pub fn changed(changed: bool) -> Self {
        Self { changed }
    }
}

// ──────────────────────────────────────────────────────────────
// Transaction / rollback helpers (compiler-internal)
// ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProviderTransactionMode {
    None,
    Semantic,
    Attributes,
    Unit,
}

#[derive(Clone, Debug)]
pub(super) enum ProviderRollbackSnapshot {
    Semantic(crate::semantic::UnifiedSemanticGraphSnapshot),
    Attributes(UnitAttributeSnapshot),
    Unit(Box<UnitSnapshot>),
}

// ──────────────────────────────────────────────────────────────
// Provider context
// ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct QueryProviderContext {
    pub provider: String,
    pub stage: String,
    pub family: Option<String>,
    pub phase: PhasePolicy,
    pub unit_id: String,
    pub effect_tags: EffectSet,
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

// ──────────────────────────────────────────────────────────────
// Cache entry
// ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct ProviderCacheEntry {
    pub recorded_at_unix_ns: i64,
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

// ──────────────────────────────────────────────────────────────
// Registration specs
// ──────────────────────────────────────────────────────────────

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

pub struct QueryProviderContractSpec {
    pub name: String,
    pub stage: String,
    pub family: Option<String>,
    pub phase_policy: PhasePolicy,
    pub requires: Vec<String>,
    pub effect_tags: Vec<String>,
    pub registration: QueryProviderRegistrationSpec,
}

// ──────────────────────────────────────────────────────────────
// Execution options and transaction mode
// ──────────────────────────────────────────────────────────────

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
    pub allowed_effect_tags: Option<EffectSet>,
    pub initial_bindings: Vec<(String, RuntimeValue)>,
    pub timeout: Option<std::time::Duration>,
}

impl Default for QueryExecutionOptions {
    fn default() -> Self {
        Self {
            transaction_mode: QueryTransactionMode::InPlace,
            restart_limit: 1,
            allowed_effect_tags: None,
            initial_bindings: Vec::new(),
            timeout: None,
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
    ) -> CaapResult<Self> {
        self.allowed_effect_tags = Some(EffectSet::from_unique_strings(
            effect_tags,
            "query allowed effect tag",
        )?);
        Ok(self)
    }

    pub fn with_initial_bindings(
        mut self,
        initial_bindings: impl IntoIterator<Item = (String, RuntimeValue)>,
    ) -> Self {
        self.initial_bindings = initial_bindings.into_iter().collect();
        self
    }

    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

// ──────────────────────────────────────────────────────────────
// Inner request (execution plumbing, compiler-internal)
// ──────────────────────────────────────────────────────────────

pub(super) struct QueryInnerRequest<'a> {
    pub(super) origin_stage: Option<&'a str>,
    pub(super) target: &'a str,
    pub(super) unit: &'a mut Unit,
    pub(super) phase: PhasePolicy,
    pub(super) restart_limit: usize,
    pub(super) allowed_effect_tags: Option<&'a EffectSet>,
    pub(super) initial_bindings: &'a [(String, RuntimeValue)],
    pub(super) deadline: Option<std::time::Instant>,
}

// ──────────────────────────────────────────────────────────────
// Query plan
// ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryPlanStep {
    pub stage: String,
    pub provider_names: Vec<String>,
    pub effect_tags: EffectSet,
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
    pub recorded_at_unix_ns: i64,
    pub provider_name: String,
    pub stage: String,
    pub family: Option<String>,
    pub phase_policy: PhasePolicy,
    pub effect_tags: EffectSet,
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
    pub cache_scope: QueryProviderCacheScope,
    pub resume_policy: QueryProviderResumePolicy,
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

// ──────────────────────────────────────────────────────────────
// Schedule (shared between types and registry)
// ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct QueryProviderSchedule {
    pub groups: Vec<Vec<QueryProvider>>,
    pub barriers: Vec<Option<Vec<String>>>,
}

// ──────────────────────────────────────────────────────────────
// Standalone helper functions (pub(super) = visible to `compiler`)
// ──────────────────────────────────────────────────────────────

pub(super) fn extend_available_data(
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

pub(super) fn extend_unique(target: &mut Vec<String>, values: impl IntoIterator<Item = String>) {
    let mut seen: BTreeSet<String> = target.iter().cloned().collect();
    for value in values {
        if seen.insert(value.clone()) {
            target.push(value);
        }
    }
    target.sort();
}

pub(super) fn extend_unique_artifact_keys(
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

pub(super) fn semantic_subject_tracking_key(subject: &SemanticSubjectId) -> String {
    format!("{}:{}", subject.kind, subject.value)
}

pub(super) fn semantic_cell_tracking_key(subject: &str, predicate: &str) -> String {
    format!("{subject}@{predicate}")
}

pub(crate) const ANNOTATION_PREDICATE_PREFIX: &str = "annotation.";

pub(crate) fn annotation_tracking_predicate(key: &str) -> String {
    format!("{ANNOTATION_PREDICATE_PREFIX}{key}")
}

pub(super) fn merge_provider_context_tracking(
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

pub(super) fn normalize_stage_name(value: String) -> CaapResult<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(CaapError::compiler("compiler stage name must be non-empty"));
    }
    Ok(value.to_string())
}

pub(super) fn normalize_cache_scope(value: String) -> CaapResult<QueryProviderCacheScope> {
    if value.is_empty() {
        return Err(CaapError::compiler(
            "query provider cache_scope must be non-empty",
        ));
    }
    match value.as_str() {
        "none" => Ok(QueryProviderCacheScope::None),
        "unit" => Ok(QueryProviderCacheScope::Unit),
        _ => Err(CaapError::compiler(format!(
            "unsupported query provider cache_scope {value:?}; supported values: none, unit"
        ))),
    }
}

pub(super) fn normalize_resume_policy(value: String) -> CaapResult<QueryProviderResumePolicy> {
    if value.is_empty() {
        return Err(CaapError::compiler(
            "query provider resume_policy must be non-empty",
        ));
    }
    match value.as_str() {
        "safe" => Ok(QueryProviderResumePolicy::Safe),
        "never" => Ok(QueryProviderResumePolicy::Never),
        "bootstrap_safe" => Ok(QueryProviderResumePolicy::BootstrapSafe),
        _ => Err(CaapError::compiler(format!(
            "unsupported query provider resume_policy {value:?}; supported values: safe, never, bootstrap_safe"
        ))),
    }
}

pub(super) fn normalize_virtual_path(value: String) -> CaapResult<String> {
    let value = value.trim().trim_start_matches('/').to_string();
    if value.is_empty() {
        return Err(CaapError::compiler(
            "virtual bootstrap path must be non-empty",
        ));
    }
    Ok(value)
}

pub(super) fn normalize_unique_labels(
    values: impl IntoIterator<Item = String>,
    label: &str,
) -> CaapResult<Vec<String>> {
    let mut normalized = BTreeSet::new();
    for value in values {
        let value = normalize_stage_name(value)
            .map_err(|_| CaapError::compiler(format!("{label} must be non-empty")))?;
        if !normalized.insert(value.clone()) {
            return Err(CaapError::compiler(format!(
                "{label} is duplicated: {value}"
            )));
        }
    }
    Ok(normalized.into_iter().collect())
}

pub(super) fn normalize_data_keys(
    values: impl IntoIterator<Item = String>,
) -> CaapResult<Vec<String>> {
    let mut normalized = BTreeSet::new();
    for value in values {
        let value = normalize_data_key(&value)?;
        if !normalized.insert(value.clone()) {
            return Err(CaapError::compiler(format!(
                "provider data key is duplicated: {value}"
            )));
        }
    }
    Ok(normalized.into_iter().collect())
}

pub(super) fn normalize_data_key(value: &str) -> CaapResult<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(CaapError::compiler("provider data key must be non-empty"));
    }
    if let Some((domain, rest)) = value.split_once(':') {
        return normalize_domain_data_key(domain, rest);
    }
    if let Some((domain, rest)) = value.split_once('.') {
        return normalize_domain_data_key(domain, rest);
    }
    normalize_domain_data_key(value, "*")
}

pub(super) fn normalize_domain_data_key(domain: &str, rest: &str) -> CaapResult<String> {
    let domain = normalize_data_key_domain(domain)?;
    let rest = rest.trim();
    if rest.is_empty() {
        return Err(CaapError::compiler(
            "provider data key must include a subject or '*' wildcard",
        ));
    }
    if rest.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return Err(CaapError::compiler(
            "provider data key subject must not contain whitespace or control characters",
        ));
    }
    Ok(format!("{domain}.{rest}"))
}

fn normalize_data_key_domain(domain: &str) -> CaapResult<String> {
    let domain = domain.trim();
    if domain.is_empty() {
        return Err(CaapError::compiler(
            "provider data key domain must be non-empty",
        ));
    }
    if !domain
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err(CaapError::compiler(
            "provider data key domain must use ASCII letters, digits, '-' or '_'",
        ));
    }
    Ok(domain.to_string())
}

pub(super) fn normalize_provider_domain(domain: &str) -> CaapResult<&'static str> {
    let domain = domain.trim();
    if let Some(canonical) = PROVIDER_DOMAINS
        .iter()
        .copied()
        .find(|candidate| *candidate == domain)
    {
        return Ok(canonical);
    }
    Err(CaapError::compiler(format!(
        "unsupported provider domain {domain:?}; supported values: {}",
        PROVIDER_DOMAINS.join(", ")
    )))
}

pub(super) fn normalize_provider_domains(
    values: impl IntoIterator<Item = String>,
    label: &str,
) -> CaapResult<Vec<String>> {
    let mut normalized = BTreeSet::new();
    for value in values {
        let domain = normalize_provider_domain(&value)
            .map_err(|error| CaapError::compiler(format!("{label} {value:?} is invalid: {error}")))?
            .to_string();
        if !normalized.insert(domain.clone()) {
            return Err(CaapError::compiler(format!(
                "{label} is duplicated: {domain}"
            )));
        }
    }
    Ok(normalized.into_iter().collect())
}

pub(super) fn data_keys_for_domains(domains: &[String]) -> CaapResult<Vec<String>> {
    normalize_data_keys(domains.iter().map(|domain| format!("{domain}.*")))
}

pub(super) fn require_non_empty_labels(
    values: impl IntoIterator<Item = String>,
    label: &str,
) -> CaapResult<Vec<String>> {
    let mut labels = Vec::new();
    let mut seen = BTreeSet::new();
    for value in values {
        if value.is_empty() {
            return Err(CaapError::compiler(format!("{label} must be non-empty")));
        }
        if !seen.insert(value.clone()) {
            return Err(CaapError::compiler(format!(
                "{label} is duplicated: {value}"
            )));
        }
        labels.push(value);
    }
    Ok(labels)
}

pub(super) fn enforce_query_effect_policy(
    plan: &QueryPlan,
    allowed_effect_tags: Option<&EffectSet>,
) -> CaapResult<()> {
    let Some(allowed_effect_tags) = allowed_effect_tags else {
        return Ok(());
    };
    for step in &plan.steps {
        for effect_tag in step.effect_tags.iter() {
            if !allowed_effect_tags.contains(effect_tag) {
                return Err(CaapError::compiler(format!(
                    "query effect tag {effect_tag:?} is not allowed for stage {}",
                    step.stage
                )));
            }
        }
    }
    Ok(())
}
