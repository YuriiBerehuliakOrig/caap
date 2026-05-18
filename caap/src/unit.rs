//! Minimal CAAP Unit facade for the Rust port.
//!
//! Python CAAP treats `Unit` as the public assembly boundary over IR,
//! semantics, links, attributes, snapshots and transactions.  The Rust port is
//! not that far yet; this facade intentionally owns only generic IR state and
//! leaves module/stdlib semantics out of core.

use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use crate::eval::Evaluator;
use crate::graph::{IRGraph, IRGraphTemplate};
use crate::ir::{ExprSpec, NodeId};
use crate::semantic::{
    SemanticValue, StableId, SymbolEntry, SymbolKind, UnifiedSemanticGraph,
    UnifiedSemanticGraphSnapshot,
};
use crate::values::EvalResult;

#[derive(Clone, Debug)]
pub struct Unit {
    unit_id: String,
    ir: IRGraph,
    semantics: UnifiedSemanticGraph,
    link_bindings: Vec<LinkBinding>,
    attributes: BTreeMap<String, SemanticValue>,
    syntax_state: UnitSyntaxState,
    lifecycle_events: Vec<UnitLifecycleEvent>,
    erased_rewrite_tombstones: BTreeMap<String, RewriteTombstone>,
    stable_id: StableId,
    rewrite_generation: u64,
    version: u64,
}

#[derive(Clone, Debug)]
pub struct UnitSnapshot {
    unit_id: String,
    ir: IRGraph,
    semantics: UnifiedSemanticGraph,
    link_bindings: Vec<LinkBinding>,
    attributes: BTreeMap<String, SemanticValue>,
    syntax_state: UnitSyntaxState,
    lifecycle_events: Vec<UnitLifecycleEvent>,
    erased_rewrite_tombstones: BTreeMap<String, RewriteTombstone>,
    stable_id: StableId,
    rewrite_generation: u64,
    version: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnitAttributeSnapshot {
    attributes: BTreeMap<String, SemanticValue>,
    lifecycle_events: Vec<UnitLifecycleEvent>,
    version: u64,
}

#[derive(Clone, Debug)]
pub struct UnitTransaction {
    snapshot: UnitSnapshot,
}

#[derive(Clone, Default)]
pub struct UnitAssemblyPipeline {
    hooks: Vec<UnitAssemblyHook>,
}

#[derive(Clone)]
pub struct UnitAssemblyHook {
    name: String,
    callback: Rc<dyn Fn(&mut Unit) -> Result<(), String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnitTemplate {
    pub unit_id: String,
    pub ir: IRGraphTemplate,
    pub semantics: UnifiedSemanticGraphSnapshot,
    pub link_bindings: Vec<LinkBinding>,
    pub attributes: Vec<(String, SemanticValue)>,
    pub syntax_state: UnitSyntaxState,
    pub lifecycle_events: Vec<UnitLifecycleEvent>,
    pub erased_rewrite_tombstones: Vec<(String, RewriteTombstone)>,
    pub stable_id: StableId,
    pub rewrite_generation: u64,
    pub version: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UnitSyntaxState {
    pub language: String,
    pub source_path: Option<String>,
    pub source_fingerprint: Option<String>,
    pub revision: u64,
    pub grammar_rules: BTreeMap<String, SemanticValue>,
    pub grammar_metadata: BTreeMap<String, SemanticValue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UnitLifecycleEvent {
    pub kind: String,
    pub detail: String,
    pub unit_version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RewriteRecord {
    pub provider_name: String,
    pub stage: String,
    pub family_label: Option<String>,
    pub operation: String,
    pub sources: Vec<NodeId>,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RewriteTombstone {
    pub stable_id: String,
    pub latest: RewriteRecord,
    pub chain: Vec<RewriteRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinkBinding {
    pub source_unit: String,
    pub source_name: String,
    pub local_name: String,
    pub syntax: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UnitLinkState {
    pub unit_id: String,
    pub bindings: Vec<LinkBinding>,
    pub public_names: Vec<String>,
}

pub struct CrossUnitGraph<'a> {
    units: &'a BTreeMap<String, Unit>,
}

impl LinkBinding {
    pub fn new(
        source_unit: impl Into<String>,
        source_name: impl Into<String>,
        local_name: impl Into<String>,
    ) -> Result<Self, String> {
        Self::with_syntax(source_unit, source_name, local_name, false)
    }

    pub fn with_syntax(
        source_unit: impl Into<String>,
        source_name: impl Into<String>,
        local_name: impl Into<String>,
        syntax: bool,
    ) -> Result<Self, String> {
        let source_unit = source_unit.into();
        let source_name = source_name.into();
        let local_name = local_name.into();
        if source_unit.is_empty() {
            return Err("link source unit must be non-empty".to_string());
        }
        if source_name.is_empty() {
            return Err("link source name must be non-empty".to_string());
        }
        if local_name.is_empty() {
            return Err("link local name must be non-empty".to_string());
        }
        Ok(Self {
            source_unit,
            source_name,
            local_name,
            syntax,
        })
    }
}

impl UnitSyntaxState {
    pub fn new(language: impl Into<String>) -> Result<Self, String> {
        let language = language.into();
        if language.is_empty() {
            return Err("unit syntax language must be non-empty".to_string());
        }
        Ok(Self {
            language,
            source_path: None,
            source_fingerprint: None,
            revision: 0,
            grammar_rules: BTreeMap::new(),
            grammar_metadata: BTreeMap::new(),
        })
    }

    pub fn with_source(
        mut self,
        source_path: impl Into<String>,
        source_fingerprint: impl Into<String>,
    ) -> Result<Self, String> {
        let source_path = source_path.into();
        let source_fingerprint = source_fingerprint.into();
        if source_path.is_empty() {
            return Err("unit syntax source path must be non-empty".to_string());
        }
        if source_fingerprint.is_empty() {
            return Err("unit syntax source fingerprint must be non-empty".to_string());
        }
        self.source_path = Some(source_path);
        self.source_fingerprint = Some(source_fingerprint);
        self.revision += 1;
        Ok(self)
    }

    pub fn set_grammar_rule(
        &mut self,
        name: impl Into<String>,
        rule: SemanticValue,
    ) -> Result<(), String> {
        let name = name.into();
        if name.is_empty() {
            return Err("syntax rule name must be non-empty".to_string());
        }
        self.grammar_rules.insert(name, rule);
        self.revision += 1;
        Ok(())
    }

    pub fn set_grammar_metadata(
        &mut self,
        key: impl Into<String>,
        value: SemanticValue,
    ) -> Result<(), String> {
        let key = key.into();
        if key.is_empty() {
            return Err("syntax metadata key must be non-empty".to_string());
        }
        self.grammar_metadata.insert(key, value);
        self.revision += 1;
        Ok(())
    }

    pub fn grammar_metadata(&self, key: &str) -> Option<&SemanticValue> {
        self.grammar_metadata.get(key)
    }
}

impl UnitLifecycleEvent {
    pub fn new(
        kind: impl Into<String>,
        detail: impl Into<String>,
        unit_version: u64,
    ) -> Result<Self, String> {
        let kind = kind.into();
        let detail = detail.into();
        if kind.is_empty() {
            return Err("unit lifecycle event kind must be non-empty".to_string());
        }
        if detail.is_empty() {
            return Err("unit lifecycle event detail must be non-empty".to_string());
        }
        Ok(Self {
            kind,
            detail,
            unit_version,
        })
    }
}

impl UnitAssemblyHook {
    pub fn new(
        name: impl Into<String>,
        callback: impl Fn(&mut Unit) -> Result<(), String> + 'static,
    ) -> Result<Self, String> {
        let name = name.into();
        if name.is_empty() {
            return Err("unit assembly hook name must be non-empty".to_string());
        }
        Ok(Self {
            name,
            callback: Rc::new(callback),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Debug for UnitAssemblyHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnitAssemblyHook")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for UnitAssemblyPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnitAssemblyPipeline")
            .field("hooks", &self.hook_names())
            .finish()
    }
}

impl UnitAssemblyPipeline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_hook(
        &mut self,
        name: impl Into<String>,
        callback: impl Fn(&mut Unit) -> Result<(), String> + 'static,
    ) -> Result<(), String> {
        let hook = UnitAssemblyHook::new(name, callback)?;
        if self.hooks.iter().any(|existing| existing.name == hook.name) {
            return Err(format!(
                "unit assembly hook already registered: {}",
                hook.name
            ));
        }
        self.hooks.push(hook);
        Ok(())
    }

    pub fn hook_names(&self) -> Vec<&str> {
        self.hooks.iter().map(UnitAssemblyHook::name).collect()
    }

    pub fn apply(&self, unit: &mut Unit) -> Result<(), String> {
        for hook in &self.hooks {
            unit.record_lifecycle("assembly-hook", format!("start:{}", hook.name));
            match (hook.callback)(unit) {
                Ok(()) => unit.record_lifecycle("assembly-hook", format!("finish:{}", hook.name)),
                Err(error) => {
                    unit.record_lifecycle("assembly-hook-error", hook.name.clone());
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}

impl UnitLinkState {
    pub fn new(
        unit_id: impl Into<String>,
        bindings: impl IntoIterator<Item = LinkBinding>,
        public_names: impl IntoIterator<Item = String>,
    ) -> Result<Self, String> {
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err("unit link state id must be non-empty".to_string());
        }
        let bindings: Vec<LinkBinding> = bindings.into_iter().collect();
        let mut public_names: Vec<String> = public_names.into_iter().collect();
        if public_names.iter().any(String::is_empty) {
            return Err("unit link state public names must be non-empty".to_string());
        }
        public_names.sort();
        public_names.dedup();
        Ok(Self {
            unit_id,
            bindings,
            public_names,
        })
    }
}

impl<'a> CrossUnitGraph<'a> {
    pub fn new(units: &'a BTreeMap<String, Unit>) -> Self {
        Self { units }
    }

    pub fn outgoing(&self, unit_id: &str) -> Result<&'a [LinkBinding], String> {
        if unit_id.is_empty() {
            return Err("cross-unit graph unit id must be non-empty".to_string());
        }
        Ok(self
            .units
            .get(unit_id)
            .map(Unit::link_bindings)
            .unwrap_or(&[]))
    }

    pub fn resolve_local(
        &self,
        unit_id: &str,
        local_name: &str,
    ) -> Result<Option<&'a LinkBinding>, String> {
        if local_name.is_empty() {
            return Err("cross-unit graph local name must be non-empty".to_string());
        }
        Ok(self
            .outgoing(unit_id)?
            .iter()
            .find(|binding| binding.local_name == local_name))
    }

    pub fn endpoint_unit(&self, binding: &LinkBinding) -> Option<&'a Unit> {
        self.units.get(&binding.source_unit)
    }

    pub fn resolve_binding(
        &self,
        binding: &LinkBinding,
    ) -> Result<Option<&'a SymbolEntry>, String> {
        let Some(unit) = self.endpoint_unit(binding) else {
            return Ok(None);
        };
        Ok(unit.semantics().lookup_symbol(&binding.source_name)?)
    }
}

impl Unit {
    pub fn empty(unit_id: impl Into<String>) -> Result<Self, String> {
        Self::from_graph(unit_id, IRGraph::new())
    }

    pub fn from_graph(unit_id: impl Into<String>, ir: IRGraph) -> Result<Self, String> {
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err("unit id must be non-empty".to_string());
        }
        let stable_id = StableId::new(format!("unit:{unit_id}"))?;
        Ok(Self {
            unit_id,
            ir,
            semantics: UnifiedSemanticGraph::new(),
            link_bindings: Vec::new(),
            attributes: BTreeMap::new(),
            syntax_state: UnitSyntaxState::new("ir")?,
            lifecycle_events: Vec::new(),
            erased_rewrite_tombstones: BTreeMap::new(),
            stable_id,
            rewrite_generation: 0,
            version: 0,
        })
    }

    pub fn unit_id(&self) -> &str {
        &self.unit_id
    }

    pub fn set_unit_id(&mut self, unit_id: impl Into<String>) -> Result<(), String> {
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err("unit id must be non-empty".to_string());
        }
        if self.unit_id != unit_id {
            self.unit_id = unit_id;
            self.version += 1;
            self.record_lifecycle("unit-id", "set");
        }
        Ok(())
    }

    pub fn ir(&self) -> &IRGraph {
        &self.ir
    }

    pub fn ir_mut(&mut self) -> &mut IRGraph {
        &mut self.ir
    }

    pub fn replace_ir_subtree_with_spec(
        &mut self,
        target_id: NodeId,
        spec: &ExprSpec,
    ) -> Result<NodeId, String> {
        let snapshot = self.snapshot();
        let result = (|| {
            let new_id = self.ir.insert_expr_spec(spec)?;
            self.ir.replace_subtree(target_id, new_id)?;
            Ok(new_id)
        })();
        match result {
            Ok(new_id) => {
                self.version += 1;
                self.record_lifecycle("ir", "replace-subtree");
                Ok(new_id)
            }
            Err(error) => {
                self.restore_snapshot(snapshot);
                Err(error)
            }
        }
    }

    pub fn wrap_ir_subtree_with_spec(
        &mut self,
        target_id: NodeId,
        callee: &ExprSpec,
    ) -> Result<NodeId, String> {
        let target = self.ir.expr_spec_for_subtree(target_id)?;
        self.replace_ir_subtree_with_spec(target_id, &ExprSpec::call(callee.clone(), vec![target]))
    }

    pub fn insert_ir_before_with_spec(
        &mut self,
        anchor_id: NodeId,
        spec: &ExprSpec,
    ) -> Result<NodeId, String> {
        let snapshot = self.snapshot();
        let result = (|| {
            let new_id = self.ir.insert_expr_spec(spec)?;
            self.ir.insert_top_level_before(anchor_id, new_id)?;
            Ok(new_id)
        })();
        match result {
            Ok(new_id) => {
                self.version += 1;
                self.record_lifecycle("ir", "insert-before");
                Ok(new_id)
            }
            Err(error) => {
                self.restore_snapshot(snapshot);
                Err(error)
            }
        }
    }

    pub fn insert_ir_after_with_spec(
        &mut self,
        anchor_id: NodeId,
        spec: &ExprSpec,
    ) -> Result<NodeId, String> {
        let snapshot = self.snapshot();
        let result = (|| {
            let new_id = self.ir.insert_expr_spec(spec)?;
            self.ir.insert_top_level_after(anchor_id, new_id)?;
            Ok(new_id)
        })();
        match result {
            Ok(new_id) => {
                self.version += 1;
                self.record_lifecycle("ir", "insert-after");
                Ok(new_id)
            }
            Err(error) => {
                self.restore_snapshot(snapshot);
                Err(error)
            }
        }
    }

    pub fn append_ir_top_level_with_spec(&mut self, spec: &ExprSpec) -> Result<NodeId, String> {
        let snapshot = self.snapshot();
        let result = (|| {
            let new_id = self.ir.insert_expr_spec(spec)?;
            self.ir.add_top_level_form(new_id)?;
            Ok(new_id)
        })();
        match result {
            Ok(new_id) => {
                self.version += 1;
                self.record_lifecycle("ir", "append-top-level");
                Ok(new_id)
            }
            Err(error) => {
                self.restore_snapshot(snapshot);
                Err(error)
            }
        }
    }

    pub fn erase_ir_subtree(&mut self, target_id: NodeId) -> Result<Vec<NodeId>, String> {
        let dropped = self.ir.erase_detached_subtree(target_id)?;
        if !dropped.is_empty() {
            self.version += 1;
            self.record_lifecycle("ir", "erase-subtree");
        }
        Ok(dropped)
    }

    pub fn node_stable_id(&self, node_id: NodeId) -> Result<StableId, String> {
        if !self.ir.contains(node_id) {
            return Err(format!("node does not exist: {node_id}"));
        }
        let key = format!("node:{node_id}");
        if let Some(stable_id) = self.semantics.stable_ids().get(&key) {
            return Ok(stable_id.clone());
        }
        Ok(StableId::new(format!(
            "unit:{}:node:{node_id}",
            self.unit_id
        ))?)
    }

    pub fn erased_rewrite_tombstones(&self) -> &BTreeMap<String, RewriteTombstone> {
        &self.erased_rewrite_tombstones
    }

    pub fn get_erased_rewrite_tombstone(&self, stable_id: &str) -> Option<&RewriteTombstone> {
        self.erased_rewrite_tombstones.get(stable_id)
    }

    pub fn record_rewrite_provenance(
        &mut self,
        provider_name: impl Into<String>,
        stage: impl Into<String>,
        family_label: Option<String>,
        operation: impl Into<String>,
        node_ids: impl IntoIterator<Item = NodeId>,
        sources: impl IntoIterator<Item = NodeId>,
    ) -> Result<Option<RewriteRecord>, String> {
        let provider_name = provider_name.into();
        let stage = stage.into();
        let operation = operation.into();
        if provider_name.is_empty() {
            return Err("rewrite provider name must be non-empty".to_string());
        }
        if stage.is_empty() {
            return Err("rewrite stage must be non-empty".to_string());
        }
        if family_label.as_ref().is_some_and(String::is_empty) {
            return Err("rewrite family label must be non-empty when present".to_string());
        }
        if operation.is_empty() {
            return Err("rewrite operation must be non-empty".to_string());
        }
        let node_ids: Vec<NodeId> = node_ids.into_iter().collect();
        if node_ids.is_empty() {
            return Ok(None);
        }
        let sources: Vec<NodeId> = sources.into_iter().collect();
        self.rewrite_generation += 1;
        let record = RewriteRecord {
            provider_name,
            stage,
            family_label,
            operation,
            sources,
            generation: self.rewrite_generation,
        };
        let fact = rewrite_record_to_semantic_value(&record)?;
        for node_id in node_ids {
            if !self.ir.contains(node_id) {
                return Err(format!("rewrite target node does not exist: {node_id}"));
            }
            self.semantics.set_fact(
                crate::semantic::node_subject_id(node_id),
                "caap.fact.rewrite_provenance",
                fact.clone(),
            )?;
        }
        self.version += 1;
        self.record_lifecycle("rewrite", record.operation.clone());
        Ok(Some(record))
    }

    pub fn record_erase_rewrite_tombstones(
        &mut self,
        provider_name: impl Into<String>,
        stage: impl Into<String>,
        family_label: Option<String>,
        root_id: NodeId,
    ) -> Result<Vec<RewriteTombstone>, String> {
        let provider_name = provider_name.into();
        let stage = stage.into();
        if provider_name.is_empty() {
            return Err("rewrite provider name must be non-empty".to_string());
        }
        if stage.is_empty() {
            return Err("rewrite stage must be non-empty".to_string());
        }
        if family_label.as_ref().is_some_and(String::is_empty) {
            return Err("rewrite family label must be non-empty when present".to_string());
        }
        let subtree = self.subtree_ids(root_id)?;
        if subtree.is_empty() {
            return Ok(Vec::new());
        }
        self.rewrite_generation += 1;
        let mut tombstones = Vec::with_capacity(subtree.len());
        for node_id in subtree {
            let stable_id = self.node_stable_id(node_id)?.as_str().to_string();
            let erase_record = RewriteRecord {
                provider_name: provider_name.clone(),
                stage: stage.clone(),
                family_label: family_label.clone(),
                operation: "erase".to_string(),
                sources: vec![node_id],
                generation: self.rewrite_generation,
            };
            let mut chain = vec![erase_record.clone()];
            chain.extend(self.live_rewrite_chain(node_id)?);
            let tombstone = RewriteTombstone {
                stable_id: stable_id.clone(),
                latest: erase_record,
                chain,
            };
            self.erased_rewrite_tombstones
                .insert(stable_id, tombstone.clone());
            tombstones.push(tombstone);
        }
        self.version += 1;
        self.record_lifecycle("rewrite", "erase");
        Ok(tombstones)
    }

    pub fn live_rewrite_chain(&self, node_id: NodeId) -> Result<Vec<RewriteRecord>, String> {
        let mut chain = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut current_id = Some(node_id);
        while let Some(id) = current_id {
            let Some(value) = self.semantics.get_fact(
                &crate::semantic::node_subject_id(id),
                "caap.fact.rewrite_provenance",
            )?
            else {
                break;
            };
            let record = rewrite_record_from_semantic_value(value)?;
            current_id = record
                .sources
                .iter()
                .copied()
                .find(|source_id| seen.insert(*source_id));
            chain.push(record);
        }
        Ok(chain)
    }

    fn subtree_ids(&self, root_id: NodeId) -> Result<Vec<NodeId>, String> {
        if !self.ir.contains(root_id) {
            return Err(format!("subtree root does not exist: {root_id}"));
        }
        let mut ids = Vec::new();
        let mut stack = vec![root_id];
        while let Some(node_id) = stack.pop() {
            let node = self
                .ir
                .node(node_id)
                .ok_or_else(|| format!("subtree node does not exist: {node_id}"))?;
            ids.push(node_id);
            stack.extend(node.children());
        }
        Ok(ids)
    }

    pub fn semantics(&self) -> &UnifiedSemanticGraph {
        &self.semantics
    }

    pub fn semantics_mut(&mut self) -> &mut UnifiedSemanticGraph {
        self.version += 1;
        &mut self.semantics
    }

    pub fn link_bindings(&self) -> &[LinkBinding] {
        &self.link_bindings
    }

    pub fn add_link_binding(&mut self, binding: LinkBinding) {
        self.link_bindings.push(binding);
        self.version += 1;
        self.record_lifecycle("link-binding", "added");
    }

    pub fn attributes(&self) -> &BTreeMap<String, SemanticValue> {
        &self.attributes
    }

    pub fn capture_attribute_snapshot(&self) -> UnitAttributeSnapshot {
        UnitAttributeSnapshot {
            attributes: self.attributes.clone(),
            lifecycle_events: self.lifecycle_events.clone(),
            version: self.version,
        }
    }

    pub fn restore_attribute_snapshot(&mut self, snapshot: UnitAttributeSnapshot) {
        self.attributes = snapshot.attributes;
        self.lifecycle_events = snapshot.lifecycle_events;
        self.version = snapshot.version + 1;
    }

    pub fn set_attribute(
        &mut self,
        key: impl Into<String>,
        value: SemanticValue,
    ) -> Result<(), String> {
        let key = key.into();
        if key.is_empty() {
            return Err("unit attribute key must be non-empty".to_string());
        }
        self.attributes.insert(key, value);
        self.version += 1;
        self.record_lifecycle("attribute", "set");
        Ok(())
    }

    pub fn syntax_state(&self) -> &UnitSyntaxState {
        &self.syntax_state
    }

    pub fn set_syntax_state(&mut self, syntax_state: UnitSyntaxState) {
        self.syntax_state = syntax_state;
        self.version += 1;
        self.record_lifecycle("syntax-state", "updated");
    }

    pub fn lifecycle_events(&self) -> &[UnitLifecycleEvent] {
        &self.lifecycle_events
    }

    fn record_lifecycle(&mut self, kind: impl Into<String>, detail: impl Into<String>) {
        self.lifecycle_events.push(
            UnitLifecycleEvent::new(kind, detail, self.version)
                .expect("static lifecycle event labels are valid"),
        );
    }

    pub fn stable_id(&self) -> &StableId {
        &self.stable_id
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn into_graph(self) -> IRGraph {
        self.ir
    }

    pub fn root_id(&self) -> NodeId {
        self.ir.root_id
    }

    pub fn top_level_form_ids(&self) -> &[NodeId] {
        self.ir.top_level_form_ids()
    }

    pub fn is_empty(&self) -> bool {
        self.ir.top_level_form_ids().is_empty()
    }

    pub fn evaluate(self) -> EvalResult {
        let top_level_names = self
            .semantics
            .symbols()
            .values()
            .filter(|entry| entry.kind == SymbolKind::TopLevel)
            .map(|entry| entry.name.clone())
            .collect();
        Evaluator::with_top_level_names(self.ir, top_level_names).run()
    }

    pub fn snapshot(&self) -> UnitSnapshot {
        UnitSnapshot {
            unit_id: self.unit_id.clone(),
            ir: self.ir.clone(),
            semantics: self.semantics.clone(),
            link_bindings: self.link_bindings.clone(),
            attributes: self.attributes.clone(),
            syntax_state: self.syntax_state.clone(),
            lifecycle_events: self.lifecycle_events.clone(),
            erased_rewrite_tombstones: self.erased_rewrite_tombstones.clone(),
            stable_id: self.stable_id.clone(),
            rewrite_generation: self.rewrite_generation,
            version: self.version,
        }
    }

    pub fn restore_snapshot(&mut self, snapshot: UnitSnapshot) {
        self.unit_id = snapshot.unit_id;
        self.ir = snapshot.ir;
        self.semantics = snapshot.semantics;
        self.link_bindings = snapshot.link_bindings;
        self.attributes = snapshot.attributes;
        self.syntax_state = snapshot.syntax_state;
        self.lifecycle_events = snapshot.lifecycle_events;
        self.erased_rewrite_tombstones = snapshot.erased_rewrite_tombstones;
        self.stable_id = snapshot.stable_id;
        self.rewrite_generation = snapshot.rewrite_generation;
        self.version = snapshot.version;
    }

    pub fn begin_transaction(&self) -> UnitTransaction {
        UnitTransaction {
            snapshot: self.snapshot(),
        }
    }

    pub fn rollback_transaction(&mut self, transaction: UnitTransaction) {
        self.restore_snapshot(transaction.snapshot);
    }

    pub fn commit_transaction(&mut self, _transaction: UnitTransaction) -> u64 {
        self.version += 1;
        self.record_lifecycle("transaction", "committed");
        self.version
    }

    pub fn to_template(&self) -> UnitTemplate {
        let mut attributes: Vec<(String, SemanticValue)> = self
            .attributes
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        attributes.sort_by(|left, right| left.0.cmp(&right.0));
        UnitTemplate {
            unit_id: self.unit_id.clone(),
            ir: self.ir.to_template(),
            semantics: self.semantics.snapshot(),
            link_bindings: self.link_bindings.clone(),
            attributes,
            syntax_state: self.syntax_state.clone(),
            lifecycle_events: self.lifecycle_events.clone(),
            erased_rewrite_tombstones: self
                .erased_rewrite_tombstones
                .iter()
                .map(|(stable_id, tombstone)| (stable_id.clone(), tombstone.clone()))
                .collect(),
            stable_id: self.stable_id.clone(),
            rewrite_generation: self.rewrite_generation,
            version: self.version,
        }
    }

    pub fn from_template(template: UnitTemplate) -> Result<Self, String> {
        template.validate()?;
        let mut semantics = UnifiedSemanticGraph::without_facts();
        semantics.restore_snapshot(template.semantics)?;
        Ok(Self {
            unit_id: template.unit_id,
            ir: IRGraph::from_template(template.ir)?,
            semantics,
            link_bindings: template.link_bindings,
            attributes: template.attributes.into_iter().collect(),
            syntax_state: template.syntax_state,
            lifecycle_events: template.lifecycle_events,
            erased_rewrite_tombstones: template.erased_rewrite_tombstones.into_iter().collect(),
            stable_id: template.stable_id,
            rewrite_generation: template.rewrite_generation,
            version: template.version,
        })
    }
}

fn rewrite_record_to_semantic_value(record: &RewriteRecord) -> Result<SemanticValue, String> {
    let mut entries = vec![
        (
            "provider_name".to_string(),
            SemanticValue::Str(record.provider_name.clone()),
        ),
        (
            "stage".to_string(),
            SemanticValue::Str(record.stage.clone()),
        ),
        (
            "operation".to_string(),
            SemanticValue::Str(record.operation.clone()),
        ),
        (
            "sources".to_string(),
            SemanticValue::List(
                record
                    .sources
                    .iter()
                    .map(|source| SemanticValue::Int(*source as i64))
                    .collect(),
            ),
        ),
        (
            "generation".to_string(),
            SemanticValue::Int(record.generation as i64),
        ),
    ];
    if let Some(family_label) = &record.family_label {
        entries.push((
            "family_label".to_string(),
            SemanticValue::Str(family_label.clone()),
        ));
        entries.push((
            "family".to_string(),
            SemanticValue::Str(family_label.clone()),
        ));
    }
    Ok(SemanticValue::map(entries)?)
}

fn rewrite_record_from_semantic_value(value: &SemanticValue) -> Result<RewriteRecord, String> {
    let SemanticValue::Map(entries) = value else {
        return Err("rewrite provenance fact must be a map".to_string());
    };
    let provider_name = required_semantic_str(entries, "provider_name")?.to_string();
    let stage = required_semantic_str(entries, "stage")?.to_string();
    let family_label = optional_semantic_str(entries, "family_label")
        .or_else(|| optional_semantic_str(entries, "family"))
        .map(str::to_string);
    let operation = required_semantic_str(entries, "operation")?.to_string();
    let sources = required_semantic_node_list(entries, "sources")?;
    let generation = match semantic_map_get(entries, "generation") {
        Some(SemanticValue::Int(value)) if *value >= 0 => *value as u64,
        _ => return Err("rewrite provenance fact requires non-negative generation".to_string()),
    };
    Ok(RewriteRecord {
        provider_name,
        stage,
        family_label,
        operation,
        sources,
        generation,
    })
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
) -> Result<&'a str, String> {
    match semantic_map_get(entries, key) {
        Some(SemanticValue::Str(value)) if !value.is_empty() => Ok(value),
        _ => Err(format!("rewrite provenance fact requires non-empty {key}")),
    }
}

fn optional_semantic_str<'a>(entries: &'a [(String, SemanticValue)], key: &str) -> Option<&'a str> {
    match semantic_map_get(entries, key) {
        Some(SemanticValue::Str(value)) if !value.is_empty() => Some(value),
        _ => None,
    }
}

fn required_semantic_node_list(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Vec<NodeId>, String> {
    let Some(SemanticValue::List(items)) = semantic_map_get(entries, key) else {
        return Err(format!("rewrite provenance fact requires {key} list"));
    };
    items
        .iter()
        .map(|item| match item {
            SemanticValue::Int(value) if *value >= 0 && *value <= NodeId::MAX as i64 => {
                Ok(*value as NodeId)
            }
            _ => Err(format!(
                "rewrite provenance fact {key} must contain node ids"
            )),
        })
        .collect()
}

impl UnitSnapshot {
    pub fn unit_id(&self) -> &str {
        &self.unit_id
    }

    pub fn ir(&self) -> &IRGraph {
        &self.ir
    }

    pub fn semantics(&self) -> &UnifiedSemanticGraph {
        &self.semantics
    }

    pub fn syntax_state(&self) -> &UnitSyntaxState {
        &self.syntax_state
    }

    pub fn lifecycle_events(&self) -> &[UnitLifecycleEvent] {
        &self.lifecycle_events
    }
}

impl UnitTransaction {
    pub fn snapshot(&self) -> &UnitSnapshot {
        &self.snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_attribute_snapshot_restore_bumps_version() {
        let mut unit = Unit::empty("test.unit").unwrap();
        unit.set_attribute("answer", SemanticValue::Int(42))
            .unwrap();
        let snapshot = unit.capture_attribute_snapshot();
        unit.set_attribute("answer", SemanticValue::Int(7)).unwrap();
        let changed_version = unit.version();

        unit.restore_attribute_snapshot(snapshot);
        assert_eq!(
            unit.attributes().get("answer"),
            Some(&SemanticValue::Int(42))
        );
        assert_eq!(unit.version(), changed_version);
    }

    #[test]
    fn unit_snapshot_restore_restores_identity_and_attributes() {
        let mut unit = Unit::empty("before").unwrap();
        unit.set_attribute("mode", SemanticValue::Str("old".into()))
            .unwrap();
        let snapshot = unit.snapshot();

        unit.set_unit_id("after").unwrap();
        unit.set_attribute("mode", SemanticValue::Str("new".into()))
            .unwrap();
        unit.restore_snapshot(snapshot);

        assert_eq!(unit.unit_id(), "before");
        assert_eq!(
            unit.attributes().get("mode"),
            Some(&SemanticValue::Str("old".into()))
        );
    }
}

impl UnitTemplate {
    pub fn validate(&self) -> Result<(), String> {
        if self.unit_id.is_empty() {
            return Err("unit template id must be non-empty".to_string());
        }
        if self.stable_id.as_str().is_empty() {
            return Err("unit template stable id must be non-empty".to_string());
        }
        let mut attribute_keys = std::collections::HashSet::new();
        for (key, _) in &self.attributes {
            if key.is_empty() {
                return Err("unit template attribute key must be non-empty".to_string());
            }
            if !attribute_keys.insert(key) {
                return Err("unit template attribute keys must be unique".to_string());
            }
        }
        if self.syntax_state.language.is_empty() {
            return Err("unit template syntax language must be non-empty".to_string());
        }
        if self.syntax_state.grammar_rules.keys().any(String::is_empty) {
            return Err("unit template syntax rule names must be non-empty".to_string());
        }
        if self
            .syntax_state
            .grammar_metadata
            .keys()
            .any(String::is_empty)
        {
            return Err("unit template syntax metadata keys must be non-empty".to_string());
        }
        if self
            .lifecycle_events
            .iter()
            .any(|event| event.kind.is_empty() || event.detail.is_empty())
        {
            return Err("unit template lifecycle events must be non-empty".to_string());
        }
        let mut tombstone_keys = std::collections::HashSet::new();
        for (stable_id, tombstone) in &self.erased_rewrite_tombstones {
            if stable_id.is_empty() || tombstone.stable_id.is_empty() {
                return Err(
                    "unit template erased rewrite tombstone stable ids must be non-empty"
                        .to_string(),
                );
            }
            if stable_id != &tombstone.stable_id {
                return Err(
                    "unit template erased rewrite tombstone key must match payload stable id"
                        .to_string(),
                );
            }
            if !tombstone_keys.insert(stable_id) {
                return Err(
                    "unit template erased rewrite tombstone stable ids must be unique".to_string(),
                );
            }
            tombstone.validate()?;
        }
        Ok(self.ir.validate()?)
    }
}

impl RewriteRecord {
    fn validate(&self) -> Result<(), String> {
        if self.provider_name.is_empty() {
            return Err("rewrite record provider name must be non-empty".to_string());
        }
        if self.stage.is_empty() {
            return Err("rewrite record stage must be non-empty".to_string());
        }
        if self.family_label.as_ref().is_some_and(String::is_empty) {
            return Err("rewrite record family label must be non-empty when present".to_string());
        }
        if self.operation.is_empty() {
            return Err("rewrite record operation must be non-empty".to_string());
        }
        Ok(())
    }
}

impl RewriteTombstone {
    fn validate(&self) -> Result<(), String> {
        if self.stable_id.is_empty() {
            return Err("rewrite tombstone stable id must be non-empty".to_string());
        }
        self.latest.validate()?;
        if self.chain.is_empty() {
            return Err("rewrite tombstone chain must be non-empty".to_string());
        }
        for record in &self.chain {
            record.validate()?;
        }
        Ok(())
    }
}
