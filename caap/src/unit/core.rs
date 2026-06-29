//! Core unit types: Unit, UnitAttributeSnapshot, UnitLinkState, CrossUnitGraph.

const REWRITE_PROVENANCE_FACT: &str = "caap.kernel.rewrite_provenance";

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::artifacts::ArtifactFingerprint;
use crate::error::{CaapError, CaapResult};
use crate::eval::Evaluator;
use crate::graph::IRGraph;
use crate::ir::{ExprSpec, Node, NodeId};
use crate::semantic::{SemanticValue, StableId, SymbolEntry, SymbolKind, UnifiedSemanticGraph};
use crate::values::EvalResult;

use super::lifecycle::{LinkBinding, UnitLifecycleEvent, UnitSyntaxState};
use super::rewrite::{
    rewrite_record_from_semantic_value, rewrite_record_to_semantic_value, RewriteRecord,
    RewriteTombstone,
};
use super::serial::{UnitSnapshot, UnitTemplate, UnitTransaction};

pub const DEFAULT_REWRITE_TOMBSTONE_LIMIT: usize = 4096;

// ---------------------------------------------------------------------------
// Unit
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Unit {
    pub(super) unit_id: String,
    pub(super) ir: IRGraph,
    pub(super) semantics: UnifiedSemanticGraph,
    pub(super) link_bindings: Vec<LinkBinding>,
    pub(super) attributes: BTreeMap<String, SemanticValue>,
    pub(super) syntax_state: UnitSyntaxState,
    pub(super) lifecycle_events: Vec<UnitLifecycleEvent>,
    pub(super) erased_rewrite_tombstones: BTreeMap<String, RewriteTombstone>,
    pub(super) stable_id: StableId,
    pub(super) rewrite_generation: u64,
    pub(super) version: u64,
}

impl Unit {
    pub fn empty(unit_id: impl Into<String>) -> CaapResult<Self> {
        Self::from_graph(unit_id, IRGraph::new())
    }

    pub fn from_graph(unit_id: impl Into<String>, ir: IRGraph) -> CaapResult<Self> {
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err(CaapError::unit("unit id must be non-empty"));
        }
        let stable_id = StableId::new(format!("unit:{unit_id}")).map_err(CaapError::semantic)?;
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

    pub fn set_unit_id(&mut self, unit_id: impl Into<String>) -> CaapResult<()> {
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err(CaapError::unit("unit id must be non-empty"));
        }
        if self.unit_id != unit_id {
            let version = self.next_version()?;
            self.unit_id = unit_id;
            self.version = version;
            self.record_lifecycle("unit_id", "set")?;
        }
        Ok(())
    }

    pub fn ir(&self) -> &IRGraph {
        &self.ir
    }

    pub fn replace_ir_subtree_with_spec(
        &mut self,
        target_id: NodeId,
        spec: &ExprSpec,
    ) -> CaapResult<NodeId> {
        let snapshot = self.snapshot();
        let result: CaapResult<NodeId> = (|| {
            let new_id = self.ir.insert_expr_spec(spec)?;
            self.ir.replace_subtree(target_id, new_id)?;
            Ok(new_id)
        })();
        match result {
            Ok(new_id) => {
                let version = match self.next_version() {
                    Ok(version) => version,
                    Err(error) => return self.restore_after_error(snapshot, error),
                };
                self.version = version;
                self.record_lifecycle("ir", "replace_subtree")?;
                Ok(new_id)
            }
            Err(error) => self.restore_after_error(snapshot, error),
        }
    }

    pub fn append_ir_top_level_with_spec(&mut self, spec: &ExprSpec) -> CaapResult<NodeId> {
        let snapshot = self.snapshot();
        let result: CaapResult<NodeId> = (|| {
            let new_id = self.ir.insert_expr_spec(spec)?;
            self.ir.add_top_level_form(new_id)?;
            Ok(new_id)
        })();
        match result {
            Ok(new_id) => {
                let version = match self.next_version() {
                    Ok(version) => version,
                    Err(error) => return self.restore_after_error(snapshot, error),
                };
                self.version = version;
                self.record_lifecycle("ir", "append_top_level")?;
                Ok(new_id)
            }
            Err(error) => self.restore_after_error(snapshot, error),
        }
    }

    pub fn set_ir_top_level_form_ids(&mut self, form_ids: Vec<NodeId>) -> CaapResult<()> {
        let snapshot = self.snapshot();
        match self.ir.set_top_level_form_ids(form_ids) {
            Ok(()) => {
                let version = match self.next_version() {
                    Ok(version) => version,
                    Err(error) => return self.restore_after_error(snapshot, error),
                };
                self.version = version;
                self.record_lifecycle("ir", "set_top_level_forms")?;
                Ok(())
            }
            Err(error) => self.restore_after_error(snapshot, error),
        }
    }

    pub fn set_ir_root_id(&mut self, root_id: NodeId) -> CaapResult<()> {
        if !self.ir.contains(root_id) {
            return Err(CaapError::unit(format!(
                "root node does not exist: {root_id}"
            )));
        }
        if self.ir.root_id != root_id {
            let version = self.next_version()?;
            self.ir.root_id = root_id;
            self.version = version;
            self.record_lifecycle("ir", "set_root")?;
        }
        Ok(())
    }

    pub fn erase_ir_subtree(&mut self, target_id: NodeId) -> CaapResult<Vec<NodeId>> {
        let snapshot = self.snapshot();
        let dropped = self.ir.erase_detached_subtree(target_id)?;
        if !dropped.is_empty() {
            let version = match self.next_version() {
                Ok(version) => version,
                Err(error) => return self.restore_after_error(snapshot, error),
            };
            self.version = version;
            self.record_lifecycle("ir", "erase_subtree")?;
        }
        Ok(dropped)
    }

    pub fn node_stable_id(&self, node_id: NodeId) -> CaapResult<StableId> {
        if !self.ir.contains(node_id) {
            return Err(CaapError::unit(format!("node does not exist: {node_id}")));
        }
        let key = format!("node:{node_id}");
        if let Some(stable_id) = self.semantics.stable_ids().get(&key) {
            return Ok(stable_id.clone());
        }
        let fingerprint = self.node_structural_fingerprint(node_id)?;
        let occurrence = self.node_fingerprint_occurrence(node_id, &fingerprint)?;
        StableId::new(format!(
            "unit:{}:node:{}:occurrence:{occurrence}",
            self.unit_id, fingerprint
        ))
    }

    fn node_fingerprint_occurrence(&self, node_id: NodeId, fingerprint: &str) -> CaapResult<usize> {
        let mut occurrence = 0usize;
        for candidate in self.ir.node_ids() {
            if candidate == node_id {
                return Ok(occurrence);
            }
            if self.node_structural_fingerprint(candidate)? == fingerprint {
                occurrence += 1;
            }
        }
        Err(CaapError::unit(format!("node does not exist: {node_id}")))
    }

    fn node_structural_fingerprint(&self, node_id: NodeId) -> CaapResult<String> {
        let node = self
            .ir
            .node(node_id)
            .ok_or_else(|| CaapError::unit(format!("node does not exist: {node_id}")))?;
        match node {
            Node::Name(node) => Ok(format!("name:{}", node.identifier)),
            Node::Literal(node) => Ok(format!("literal:{:?}", node.value)),
            Node::Call(node) => {
                let callee = self.node_structural_fingerprint(node.callee)?;
                let args = node
                    .args
                    .iter()
                    .map(|arg| self.node_structural_fingerprint(*arg))
                    .collect::<CaapResult<Vec<_>>>()?
                    .join(",");
                Ok(format!("call:{callee}({args})"))
            }
        }
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
    ) -> CaapResult<Option<RewriteRecord>> {
        let provider_name = provider_name.into();
        let stage = stage.into();
        let operation = operation.into();
        if provider_name.is_empty() {
            return Err(CaapError::unit("rewrite provider name must be non-empty"));
        }
        if stage.is_empty() {
            return Err(CaapError::unit("rewrite stage must be non-empty"));
        }
        if family_label.as_ref().is_some_and(String::is_empty) {
            return Err(CaapError::unit(
                "rewrite family label must be non-empty when present",
            ));
        }
        if operation.is_empty() {
            return Err(CaapError::unit("rewrite operation must be non-empty"));
        }
        let node_ids: Vec<NodeId> = node_ids.into_iter().collect();
        if node_ids.is_empty() {
            return Ok(None);
        }
        let sources: Vec<NodeId> = sources.into_iter().collect();
        for node_id in &node_ids {
            if !self.ir.contains(*node_id) {
                return Err(CaapError::unit(format!(
                    "rewrite target node does not exist: {node_id}"
                )));
            }
        }
        let version = self.next_version()?;
        let generation = self.prepare_rewrite_generation()?;
        let record = RewriteRecord {
            provider_name,
            stage,
            family_label,
            operation,
            sources,
            generation,
        };
        let fact = rewrite_record_to_semantic_value(&record)?;
        let semantics_snapshot = self.semantics.snapshot();
        let result: CaapResult<()> = (|| {
            for node_id in node_ids {
                self.semantics.set_fact(
                    crate::semantic::node_subject_id(node_id),
                    REWRITE_PROVENANCE_FACT,
                    fact.clone(),
                )?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            if let Err(rollback_error) = self.semantics.restore_snapshot(semantics_snapshot) {
                return Err(CaapError::unit(format!(
                    "{error}; semantic rewrite provenance rollback failed: {rollback_error}"
                )));
            }
            return Err(error);
        }
        self.rewrite_generation = generation;
        self.version = version;
        self.record_lifecycle("rewrite", record.operation.clone())?;
        Ok(Some(record))
    }

    pub fn record_erase_rewrite_tombstones(
        &mut self,
        provider_name: impl Into<String>,
        stage: impl Into<String>,
        family_label: Option<String>,
        root_id: NodeId,
    ) -> CaapResult<Vec<RewriteTombstone>> {
        let provider_name = provider_name.into();
        let stage = stage.into();
        if provider_name.is_empty() {
            return Err(CaapError::unit("rewrite provider name must be non-empty"));
        }
        if stage.is_empty() {
            return Err(CaapError::unit("rewrite stage must be non-empty"));
        }
        if family_label.as_ref().is_some_and(String::is_empty) {
            return Err(CaapError::unit(
                "rewrite family label must be non-empty when present",
            ));
        }
        let subtree = self.subtree_ids(root_id)?;
        if subtree.is_empty() {
            return Ok(Vec::new());
        }
        let version = self.next_version()?;
        let generation = self.prepare_rewrite_generation()?;
        let mut tombstones = Vec::with_capacity(subtree.len());
        for node_id in subtree {
            let stable_id = self.node_stable_id(node_id)?.as_str().to_string();
            let erase_record = RewriteRecord {
                provider_name: provider_name.clone(),
                stage: stage.clone(),
                family_label: family_label.clone(),
                operation: "erase".to_string(),
                sources: vec![node_id],
                generation,
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
        self.prune_erased_rewrite_tombstones(DEFAULT_REWRITE_TOMBSTONE_LIMIT)?;
        self.rewrite_generation = generation;
        self.version = version;
        self.record_lifecycle("rewrite", "erase")?;
        Ok(tombstones)
    }

    fn prepare_rewrite_generation(&self) -> CaapResult<u64> {
        let next = self
            .rewrite_generation
            .checked_add(1)
            .ok_or_else(|| CaapError::unit("rewrite generation overflow"))?;
        i64::try_from(next)
            .map_err(|_| CaapError::unit("rewrite generation exceeds semantic integer range"))?;
        Ok(next)
    }

    pub fn prune_erased_rewrite_tombstones(&mut self, max_entries: usize) -> CaapResult<usize> {
        let current_len = self.erased_rewrite_tombstones.len();
        if current_len <= max_entries {
            return Ok(0);
        }
        let version = self.next_version()?;
        if max_entries == 0 {
            self.erased_rewrite_tombstones.clear();
            self.version = version;
            return Ok(current_len);
        }

        let remove_count = current_len - max_entries;
        let mut by_age: Vec<(u64, String)> = self
            .erased_rewrite_tombstones
            .iter()
            .map(|(stable_id, tombstone)| (tombstone.latest.generation, stable_id.clone()))
            .collect();
        by_age.sort();
        for (_, stable_id) in by_age.into_iter().take(remove_count) {
            self.erased_rewrite_tombstones.remove(&stable_id);
        }
        self.version = version;
        Ok(remove_count)
    }

    pub fn live_rewrite_chain(&self, node_id: NodeId) -> CaapResult<Vec<RewriteRecord>> {
        let mut chain = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut current_id = Some(node_id);
        while let Some(id) = current_id {
            let Some(value) = self.semantics.get_fact(
                &crate::semantic::node_subject_id(id),
                REWRITE_PROVENANCE_FACT,
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

    fn subtree_ids(&self, root_id: NodeId) -> CaapResult<Vec<NodeId>> {
        if !self.ir.contains(root_id) {
            return Err(CaapError::unit(format!(
                "subtree root does not exist: {root_id}"
            )));
        }
        let mut ids = Vec::new();
        let mut stack = vec![root_id];
        while let Some(node_id) = stack.pop() {
            let node = self.ir.node(node_id).ok_or_else(|| {
                CaapError::unit(format!("subtree node does not exist: {node_id}"))
            })?;
            ids.push(node_id);
            stack.extend(node.children());
        }
        Ok(ids)
    }

    pub fn semantics(&self) -> &UnifiedSemanticGraph {
        &self.semantics
    }

    pub fn semantics_mut(&mut self) -> CaapResult<&mut UnifiedSemanticGraph> {
        self.version = self.next_version()?;
        Ok(&mut self.semantics)
    }

    pub fn link_bindings(&self) -> &[LinkBinding] {
        &self.link_bindings
    }

    pub fn add_link_binding(&mut self, binding: LinkBinding) -> CaapResult<()> {
        if self
            .link_bindings
            .iter()
            .any(|existing| existing.local_name == binding.local_name)
        {
            return Err(CaapError::unit(format!(
                "unit link binding local name is duplicated: {}",
                binding.local_name
            )));
        }
        let version = self.next_version()?;
        self.link_bindings.push(binding);
        self.version = version;
        self.record_lifecycle("link_binding", "added")?;
        Ok(())
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

    pub fn restore_attribute_snapshot(
        &mut self,
        snapshot: UnitAttributeSnapshot,
    ) -> CaapResult<()> {
        let version = snapshot
            .version
            .checked_add(1)
            .ok_or_else(|| CaapError::unit("unit version overflow"))?;
        self.attributes = snapshot.attributes;
        self.lifecycle_events = snapshot.lifecycle_events;
        self.version = version;
        Ok(())
    }

    pub fn set_attribute(
        &mut self,
        key: impl Into<String>,
        value: SemanticValue,
    ) -> CaapResult<()> {
        let key = key.into();
        if key.is_empty() {
            return Err(CaapError::unit("unit attribute key must be non-empty"));
        }
        value.validate()?;
        let version = self.next_version()?;
        self.attributes.insert(key, value);
        self.version = version;
        self.record_lifecycle("attribute", "set")?;
        Ok(())
    }

    pub fn syntax_state(&self) -> &UnitSyntaxState {
        &self.syntax_state
    }

    pub fn set_syntax_state(&mut self, syntax_state: UnitSyntaxState) -> CaapResult<()> {
        let version = self.next_version()?;
        self.syntax_state = syntax_state;
        self.version = version;
        self.record_lifecycle("syntax_state", "updated")?;
        Ok(())
    }

    pub fn lifecycle_events(&self) -> &[UnitLifecycleEvent] {
        &self.lifecycle_events
    }

    pub(super) fn record_lifecycle(
        &mut self,
        kind: impl Into<String>,
        detail: impl Into<String>,
    ) -> CaapResult<()> {
        self.lifecycle_events
            .push(UnitLifecycleEvent::new(kind, detail, self.version)?);
        Ok(())
    }

    pub fn stable_id(&self) -> &StableId {
        &self.stable_id
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    fn next_version(&self) -> CaapResult<u64> {
        self.version
            .checked_add(1)
            .ok_or_else(|| CaapError::unit("unit version overflow"))
    }

    fn bump_version(
        &mut self,
        kind: impl Into<String>,
        detail: impl Into<String>,
    ) -> CaapResult<()> {
        self.version = self.next_version()?;
        self.record_lifecycle(kind, detail)
    }

    fn restore_after_error<T>(
        &mut self,
        snapshot: UnitSnapshot,
        error: CaapError,
    ) -> CaapResult<T> {
        if let Err(rollback_error) = self.restore_snapshot(snapshot) {
            return Err(CaapError::unit(format!(
                "{error}; unit rollback failed: {rollback_error}"
            )));
        }
        Err(error)
    }

    pub fn content_fingerprint(&self) -> CaapResult<ArtifactFingerprint> {
        let bytes = serde_json::to_vec(&self.to_template()).map_err(|error| {
            CaapError::unit(format!("failed to fingerprint unit template: {error}"))
        })?;
        Ok(ArtifactFingerprint::sha256(bytes))
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

    pub fn restore_snapshot(&mut self, snapshot: UnitSnapshot) -> CaapResult<()> {
        snapshot.validate()?;
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
        Ok(())
    }

    pub fn begin_transaction(&self) -> UnitTransaction {
        UnitTransaction {
            snapshot: self.snapshot(),
        }
    }

    pub fn rollback_transaction(&mut self, transaction: UnitTransaction) -> CaapResult<()> {
        self.restore_snapshot(transaction.snapshot)
    }

    pub fn commit_transaction(&mut self, _transaction: UnitTransaction) -> CaapResult<u64> {
        self.bump_version("transaction", "committed")?;
        Ok(self.version)
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

    pub fn from_template(template: UnitTemplate) -> CaapResult<Self> {
        template.validate()?;
        let mut semantics = UnifiedSemanticGraph::without_facts();
        semantics.restore_snapshot(template.semantics)?;
        Ok(Self {
            unit_id: template.unit_id,
            ir: crate::graph::IRGraph::from_template(template.ir)?,
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

// ---------------------------------------------------------------------------
// UnitAttributeSnapshot
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct UnitAttributeSnapshot {
    pub(super) attributes: BTreeMap<String, SemanticValue>,
    pub(super) lifecycle_events: Vec<UnitLifecycleEvent>,
    pub(super) version: u64,
}

// ---------------------------------------------------------------------------
// UnitLinkState
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UnitLinkState {
    pub unit_id: String,
    pub bindings: Vec<LinkBinding>,
    pub public_names: Vec<String>,
}

impl UnitLinkState {
    pub fn new(
        unit_id: impl Into<String>,
        bindings: impl IntoIterator<Item = LinkBinding>,
        public_names: impl IntoIterator<Item = String>,
    ) -> CaapResult<Self> {
        let unit_id = unit_id.into();
        if unit_id.is_empty() {
            return Err(CaapError::unit("unit link state id must be non-empty"));
        }
        let mut seen_binding_names = BTreeSet::new();
        let mut normalized_bindings = Vec::new();
        for binding in bindings {
            if !seen_binding_names.insert(binding.local_name.clone()) {
                return Err(CaapError::unit(format!(
                    "unit link state local binding name is duplicated: {}",
                    binding.local_name
                )));
            }
            normalized_bindings.push(binding);
        }
        let mut seen_public_names = BTreeSet::new();
        let mut normalized_public_names = Vec::new();
        for name in public_names {
            if name.is_empty() {
                return Err(CaapError::unit(
                    "unit link state public names must be non-empty",
                ));
            }
            if !seen_public_names.insert(name.clone()) {
                return Err(CaapError::unit(format!(
                    "unit link state public name is duplicated: {name}"
                )));
            }
            normalized_public_names.push(name);
        }
        normalized_public_names.sort();
        Ok(Self {
            unit_id,
            bindings: normalized_bindings,
            public_names: normalized_public_names,
        })
    }
}

impl<'de> Deserialize<'de> for UnitLinkState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct UnitLinkStateData {
            unit_id: String,
            bindings: Vec<LinkBinding>,
            public_names: Vec<String>,
        }

        let data = UnitLinkStateData::deserialize(deserializer)?;
        Self::new(data.unit_id, data.bindings, data.public_names).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// CrossUnitGraph
// ---------------------------------------------------------------------------

pub struct CrossUnitGraph<'a> {
    units: &'a BTreeMap<String, Unit>,
}

impl<'a> CrossUnitGraph<'a> {
    pub fn new(units: &'a BTreeMap<String, Unit>) -> Self {
        Self { units }
    }

    pub fn outgoing(&self, unit_id: &str) -> CaapResult<&'a [LinkBinding]> {
        if unit_id.is_empty() {
            return Err(CaapError::unit(
                "cross-unit graph unit id must be non-empty",
            ));
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
    ) -> CaapResult<Option<&'a LinkBinding>> {
        if local_name.is_empty() {
            return Err(CaapError::unit(
                "cross-unit graph local name must be non-empty",
            ));
        }
        Ok(self
            .outgoing(unit_id)?
            .iter()
            .find(|binding| binding.local_name == local_name))
    }

    pub fn endpoint_unit(&self, binding: &LinkBinding) -> Option<&'a Unit> {
        self.units.get(&binding.source_unit)
    }

    pub fn resolve_binding(&self, binding: &LinkBinding) -> CaapResult<Option<&'a SymbolEntry>> {
        let Some(unit) = self.endpoint_unit(binding) else {
            return Ok(None);
        };
        unit.semantics()
            .lookup_symbol(&binding.source_name)
            .map_err(CaapError::semantic)
    }
}
