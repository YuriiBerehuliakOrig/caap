//! Snapshot/transaction/template types: UnitSnapshot, UnitTransaction, UnitTemplate.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::{CaapError, CaapResult};
use crate::graph::{IRGraph, IRGraphTemplate};
use crate::semantic::{
    SemanticValue, StableId, UnifiedSemanticGraph, UnifiedSemanticGraphSnapshot,
};

use super::lifecycle::{LinkBinding, UnitLifecycleEvent, UnitSyntaxState};
use super::rewrite::RewriteTombstone;

// ---------------------------------------------------------------------------
// UnitSnapshot
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct UnitSnapshot {
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

impl UnitSnapshot {
    pub fn validate(&self) -> CaapResult<()> {
        if self.unit_id.is_empty() {
            return Err(CaapError::unit("unit snapshot id must be non-empty"));
        }
        if self.stable_id.as_str().is_empty() {
            return Err(CaapError::unit("unit snapshot stable id must be non-empty"));
        }
        self.ir.validate_integrity()?;
        let mut semantic_graph = UnifiedSemanticGraph::without_facts();
        semantic_graph
            .restore_snapshot(self.semantics.snapshot())
            .map_err(|err| {
                CaapError::unit(format!("unit snapshot semantic graph is invalid: {err}"))
            })?;
        let mut binding_names = BTreeSet::new();
        for binding in &self.link_bindings {
            if binding.source_unit.is_empty() {
                return Err(CaapError::unit(
                    "unit snapshot link source unit must be non-empty",
                ));
            }
            if binding.source_name.is_empty() {
                return Err(CaapError::unit(
                    "unit snapshot link source name must be non-empty",
                ));
            }
            if binding.local_name.is_empty() {
                return Err(CaapError::unit(
                    "unit snapshot link local name must be non-empty",
                ));
            }
            if !binding_names.insert(binding.local_name.clone()) {
                return Err(CaapError::unit(format!(
                    "unit snapshot link local name is duplicated: {}",
                    binding.local_name
                )));
            }
        }
        for (key, value) in &self.attributes {
            if key.is_empty() {
                return Err(CaapError::unit(
                    "unit snapshot attribute key must be non-empty",
                ));
            }
            value.validate()?;
        }
        self.syntax_state.validate()?;
        if self
            .lifecycle_events
            .iter()
            .any(|event| event.kind.is_empty() || event.detail.is_empty())
        {
            return Err(CaapError::unit(
                "unit snapshot lifecycle events must be non-empty",
            ));
        }
        for (stable_id, tombstone) in &self.erased_rewrite_tombstones {
            if stable_id.is_empty() || tombstone.stable_id.is_empty() {
                return Err(CaapError::unit(
                    "unit snapshot erased rewrite tombstone stable ids must be non-empty",
                ));
            }
            if stable_id != &tombstone.stable_id {
                return Err(CaapError::unit(
                    "unit snapshot erased rewrite tombstone key must match payload stable id",
                ));
            }
            tombstone.validate()?;
        }
        Ok(())
    }

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

// ---------------------------------------------------------------------------
// UnitTransaction
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct UnitTransaction {
    pub(super) snapshot: UnitSnapshot,
}

impl UnitTransaction {
    pub fn snapshot(&self) -> &UnitSnapshot {
        &self.snapshot
    }
}

// ---------------------------------------------------------------------------
// UnitTemplate
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize)]
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

impl UnitTemplate {
    pub fn validate(&self) -> CaapResult<()> {
        if self.unit_id.is_empty() {
            return Err(CaapError::unit("unit template id must be non-empty"));
        }
        if self.stable_id.as_str().is_empty() {
            return Err(CaapError::unit("unit template stable id must be non-empty"));
        }
        let mut semantic_graph = UnifiedSemanticGraph::without_facts();
        semantic_graph
            .restore_snapshot(self.semantics.clone())
            .map_err(|err| {
                CaapError::unit(format!("unit template semantic snapshot is invalid: {err}"))
            })?;
        let mut binding_names = BTreeSet::new();
        for binding in &self.link_bindings {
            if binding.source_unit.is_empty() {
                return Err(CaapError::unit(
                    "unit template link source unit must be non-empty",
                ));
            }
            if binding.source_name.is_empty() {
                return Err(CaapError::unit(
                    "unit template link source name must be non-empty",
                ));
            }
            if binding.local_name.is_empty() {
                return Err(CaapError::unit(
                    "unit template link local name must be non-empty",
                ));
            }
            if !binding_names.insert(binding.local_name.clone()) {
                return Err(CaapError::unit(format!(
                    "unit template link local name is duplicated: {}",
                    binding.local_name
                )));
            }
        }
        let mut attribute_keys = std::collections::HashSet::new();
        for (key, value) in &self.attributes {
            if key.is_empty() {
                return Err(CaapError::unit(
                    "unit template attribute key must be non-empty",
                ));
            }
            if !attribute_keys.insert(key) {
                return Err(CaapError::unit(
                    "unit template attribute keys must be unique",
                ));
            }
            value.validate()?;
        }
        self.syntax_state.validate()?;
        if self
            .lifecycle_events
            .iter()
            .any(|event| event.kind.is_empty() || event.detail.is_empty())
        {
            return Err(CaapError::unit(
                "unit template lifecycle events must be non-empty",
            ));
        }
        let mut tombstone_keys = std::collections::HashSet::new();
        for (stable_id, tombstone) in &self.erased_rewrite_tombstones {
            if stable_id.is_empty() || tombstone.stable_id.is_empty() {
                return Err(CaapError::unit(
                    "unit template erased rewrite tombstone stable ids must be non-empty",
                ));
            }
            if stable_id != &tombstone.stable_id {
                return Err(CaapError::unit(
                    "unit template erased rewrite tombstone key must match payload stable id",
                ));
            }
            if !tombstone_keys.insert(stable_id) {
                return Err(CaapError::unit(
                    "unit template erased rewrite tombstone stable ids must be unique",
                ));
            }
            tombstone.validate()?;
        }
        self.ir.validate()
    }
}

impl<'de> Deserialize<'de> for UnitTemplate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct UnitTemplateData {
            unit_id: String,
            ir: IRGraphTemplate,
            semantics: UnifiedSemanticGraphSnapshot,
            link_bindings: Vec<LinkBinding>,
            attributes: Vec<(String, SemanticValue)>,
            syntax_state: UnitSyntaxState,
            lifecycle_events: Vec<UnitLifecycleEvent>,
            erased_rewrite_tombstones: Vec<(String, RewriteTombstone)>,
            stable_id: StableId,
            rewrite_generation: u64,
            version: u64,
        }

        let data = UnitTemplateData::deserialize(deserializer)?;
        let template = Self {
            unit_id: data.unit_id,
            ir: data.ir,
            semantics: data.semantics,
            link_bindings: data.link_bindings,
            attributes: data.attributes,
            syntax_state: data.syntax_state,
            lifecycle_events: data.lifecycle_events,
            erased_rewrite_tombstones: data.erased_rewrite_tombstones,
            stable_id: data.stable_id,
            rewrite_generation: data.rewrite_generation,
            version: data.version,
        };
        template
            .validate()
            .map_err(serde::de::Error::custom)
            .map(|()| template)
    }
}
