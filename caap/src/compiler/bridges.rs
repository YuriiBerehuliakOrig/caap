use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::{Rc, Weak};

use crate::artifacts::{ArtifactInvalidationRecord, ArtifactKey, ArtifactValue};
use crate::diagnostics::Diagnostic;
use crate::semantic::{
    node_subject_id, subject_id, symbol_subject_id, BuiltinEffectTag, ControlPolicy, EffectPolicy,
    EvalPolicy, FoldPolicy, PhasePolicy, ScopePolicy, SemanticSubjectId,
};
use crate::unit::Unit;
use crate::values::{HostObject, RuntimeValue};

use crate::ir::NodeId;

use super::bridge::CompilerBridgeValue;
use super::query_provider::{
    annotation_tracking_predicate, extend_unique, extend_unique_artifact_keys,
    semantic_cell_tracking_key, semantic_subject_tracking_key, QueryPlan, QueryProviderContext,
    QueryProviderExecutionRecord,
};

#[derive(Debug)]
pub struct UnitBridgeValue {
    unit: RefCell<Unit>,
    provider_context: RefCell<Option<Weak<ProviderContextBridgeValue>>>,
}

#[derive(Debug)]
pub struct ProviderContextBridgeValue {
    context: QueryProviderContext,
    compiler: Rc<CompilerBridgeValue>,
    unit: Rc<UnitBridgeValue>,
    unit_subject: SemanticSubjectId,
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
    pub execution_diagnostics: Vec<Diagnostic>,
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
    pub artifact: Option<QueryArtifactProjection>,
    pub execution_diagnostics: Vec<Diagnostic>,
    pub invalidations: Vec<Option<ArtifactInvalidationRecord>>,
    pub unit: Unit,
}

/// Where a query reads its input unit from.
#[derive(Clone, Debug)]
pub enum QueryArtifactSource {
    /// An already-built unit, used directly (no parse or load step).
    Unit(Box<Unit>),
    /// A path to a source file the query should load.
    Path(String),
    /// Raw source text the query should parse.
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
    pub fold_policy: FoldPolicy,
    pub form_policy: String,
    pub normalizer: RuntimeValue,
    pub unit_id: Option<String>,
    pub stable_id: Option<String>,
}

impl UnitBridgeValue {
    pub fn from_unit_snapshot(unit: Unit) -> Self {
        Self {
            unit: RefCell::new(unit),
            provider_context: RefCell::new(None),
        }
    }

    pub fn clone_unit_snapshot(&self) -> Unit {
        self.unit.borrow().clone()
    }

    pub fn with_unit<R>(&self, f: impl FnOnce(&Unit) -> R) -> R {
        f(&self.unit.borrow())
    }

    pub fn with_unit_mut<R>(&self, f: impl FnOnce(&mut Unit) -> R) -> R {
        f(&mut self.unit.borrow_mut())
    }

    pub(super) fn attach_provider_context(&self, context: Weak<ProviderContextBridgeValue>) {
        *self.provider_context.borrow_mut() = Some(context);
    }

    pub fn provider_context(&self) -> Option<Rc<ProviderContextBridgeValue>> {
        self.provider_context
            .borrow()
            .as_ref()
            .and_then(Weak::upgrade)
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
        let unit_id = unit.with_unit(|unit| unit.unit_id().to_string());
        let unit_subject = subject_id("unit", unit_id.clone())
            .expect("provider context unit id must be non-empty");
        Self {
            context,
            compiler,
            unit,
            unit_subject,
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

    pub(crate) fn compiler_host_value(&self) -> RuntimeValue {
        RuntimeValue::HostObject(Rc::clone(&self.compiler) as Rc<dyn HostObject>)
    }

    pub(crate) fn clone_host_object(&self) -> Rc<dyn HostObject> {
        Rc::new(Self::new(
            self.context.clone(),
            Rc::clone(&self.compiler),
            Rc::clone(&self.unit),
        ))
    }

    pub(crate) fn push_diagnostic(&self, diagnostic: Diagnostic) -> crate::error::CaapResult<()> {
        self.compiler.push_diagnostic(diagnostic)
    }

    pub(crate) fn lookup_registered_value(
        &self,
        name: &str,
    ) -> crate::error::CaapResult<Option<RuntimeValue>> {
        self.compiler.lookup_registered_value(name)
    }

    pub(crate) fn base_semantic_entries(&self) -> Vec<crate::semantic::SemanticEntry> {
        self.compiler.base_semantic_entries()
    }

    pub(crate) fn validate_fact_value(
        &self,
        predicate: &str,
        value: &crate::semantic::SemanticValue,
    ) -> crate::error::CaapResult<()> {
        self.compiler.validate_fact_value(predicate, value)
    }

    pub fn fact_schema(&self) -> super::fact_schema::FactSchemaRegistry {
        self.compiler.fact_schema()
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

    pub fn track_fact_subject_read(&self, subject: &SemanticSubjectId, namespace: &str) {
        self.track_subject_read(subject, namespace);
    }

    pub fn track_fact_write(&self, node_id: NodeId, namespace: &str) {
        self.track_unit_fact_table_write();
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
        self.track_unit_ir_write();
        self.track_semantic_write(node_id, "$ir");
    }

    pub fn track_symbol_read(&self, name: &str) {
        if let Ok(subject) = symbol_subject_id(name) {
            self.track_subject_read(&subject, "symbol.entry");
        }
    }

    pub fn track_symbol_write(&self, name: &str) {
        self.track_unit_symbol_table_write();
        if let Ok(subject) = symbol_subject_id(name) {
            self.track_subject_write(&subject, "symbol.entry");
        }
    }

    pub fn track_unit_ir_read(&self) {
        self.track_unit_cell_read("ir");
    }

    pub fn track_unit_fact_table_read(&self) {
        self.track_unit_cell_read("facts");
    }

    pub fn track_unit_symbol_table_read(&self) {
        self.track_unit_cell_read("symbols");
    }

    pub fn track_unit_attribute_table_read(&self) {
        self.track_unit_cell_read("attributes");
    }

    pub fn track_unit_ir_write(&self) {
        self.track_unit_cell_write("ir");
    }

    pub fn track_unit_fact_table_write(&self) {
        self.track_unit_cell_write("facts");
    }

    pub fn track_unit_symbol_table_write(&self) {
        self.track_unit_cell_write("symbols");
    }

    pub fn track_unit_attribute_table_write(&self) {
        self.track_unit_cell_write("attributes");
    }

    pub fn declares_builtin_effect(&self, effect: BuiltinEffectTag) -> bool {
        self.context.effect_tags.contains_builtin(effect)
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
        self.track_subject_read(&node_subject_id(node_id), predicate);
    }

    fn track_subject_read(&self, subject: &SemanticSubjectId, predicate: &str) {
        let subject = semantic_subject_tracking_key(subject);
        self.reads_subjects.borrow_mut().insert(subject.clone());
        self.read_cells
            .borrow_mut()
            .insert(semantic_cell_tracking_key(&subject, predicate));
    }

    fn track_semantic_write(&self, node_id: NodeId, predicate: &str) {
        self.track_subject_write(&node_subject_id(node_id), predicate);
    }

    fn track_subject_write(&self, subject: &SemanticSubjectId, predicate: &str) {
        let subject = semantic_subject_tracking_key(subject);
        self.writes_subjects.borrow_mut().insert(subject.clone());
        self.write_cells
            .borrow_mut()
            .insert(semantic_cell_tracking_key(&subject, predicate));
    }

    fn track_unit_cell_read(&self, predicate: &str) {
        self.track_subject_read(self.unit_subject(), predicate);
    }

    fn track_unit_cell_write(&self, predicate: &str) {
        self.track_subject_write(self.unit_subject(), predicate);
    }

    fn unit_subject(&self) -> &SemanticSubjectId {
        &self.unit_subject
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
