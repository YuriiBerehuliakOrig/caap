//! Generic artifact cache and invalidation substrate for the Rust CAAP port.
//!
//! The cache is intentionally compiler-agnostic: it knows about artifact keys,
//! values, dependencies, dirty state, and snapshots, but not about module,
//! provider, or bootstrap policy.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{CaapError, CaapResult};
use crate::semantic::{PhasePolicy, SemanticValue};
use crate::unit::UnitTemplate;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct ArtifactKey(Vec<String>);

impl ArtifactKey {
    pub fn new(parts: impl IntoIterator<Item = String>) -> CaapResult<Self> {
        let parts: Vec<String> = parts.into_iter().collect();
        if parts.is_empty() {
            return Err(CaapError::artifacts(
                "artifact key must contain at least one part",
            ));
        }
        if parts.iter().any(String::is_empty) {
            return Err(CaapError::artifacts("artifact key parts must be non-empty"));
        }
        Ok(Self(parts))
    }

    pub fn single(part: impl Into<String>) -> CaapResult<Self> {
        Self::new([part.into()])
    }

    pub fn pair(left: impl Into<String>, right: impl Into<String>) -> CaapResult<Self> {
        Self::new([left.into(), right.into()])
    }

    pub fn parts(&self) -> &[String] {
        &self.0
    }

    pub fn part(&self, index: usize) -> Option<&str> {
        self.0.get(index).map(String::as_str)
    }

    pub fn kind(&self) -> &str {
        &self.0[0]
    }
}

impl fmt::Display for ArtifactKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.join(":"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct ArtifactFingerprint(String);

impl ArtifactFingerprint {
    pub fn sha256(bytes: impl AsRef<[u8]>) -> Self {
        let mut digest = Sha256::new();
        digest.update(bytes.as_ref());
        Self(format!("{:x}", digest.finalize()))
    }

    pub fn new(value: impl Into<String>) -> CaapResult<Self> {
        let value = value.into();
        if value.is_empty() {
            return Err(CaapError::artifacts(
                "artifact fingerprint must be non-empty",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArtifactFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SourceOrigin {
    Inline { label: String },
    Path { path: String, source_token: String },
}

impl SourceOrigin {
    pub fn inline(label: impl Into<String>) -> CaapResult<Self> {
        let label = label.into();
        if label.is_empty() {
            return Err(CaapError::artifacts(
                "inline source label must be non-empty",
            ));
        }
        Ok(Self::Inline { label })
    }

    pub fn path(path: impl Into<String>, source_token: impl Into<String>) -> CaapResult<Self> {
        let path = path.into();
        let source_token = source_token.into();
        if path.is_empty() {
            return Err(CaapError::artifacts(
                "source artifact path must be non-empty",
            ));
        }
        if source_token.is_empty() {
            return Err(CaapError::artifacts(
                "source artifact path token must be non-empty",
            ));
        }
        Ok(Self::Path { path, source_token })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceArtifact {
    pub origin: SourceOrigin,
    pub text: String,
    pub fingerprint: ArtifactFingerprint,
}

impl SourceArtifact {
    pub fn inline(text: impl Into<String>) -> CaapResult<Self> {
        Self::inline_with_label(text, "<inline.caap>")
    }

    pub fn inline_with_label(
        text: impl Into<String>,
        label: impl Into<String>,
    ) -> CaapResult<Self> {
        let text = text.into();
        Ok(Self {
            fingerprint: ArtifactFingerprint::sha256(text.as_bytes()),
            origin: SourceOrigin::inline(label)?,
            text,
        })
    }

    pub fn path(
        path: impl Into<String>,
        source_token: impl Into<String>,
        text: impl Into<String>,
    ) -> CaapResult<Self> {
        let text = text.into();
        Ok(Self {
            fingerprint: ArtifactFingerprint::sha256(text.as_bytes()),
            origin: SourceOrigin::path(path, source_token)?,
            text,
        })
    }

    pub fn parse_surface_key(
        &self,
        stage: impl Into<String>,
        phase: PhasePolicy,
    ) -> CaapResult<ArtifactKey> {
        let stage = stage.into();
        match &self.origin {
            SourceOrigin::Inline { .. } => {
                parse_surface_inline_key(stage, phase, &self.fingerprint)
            }
            SourceOrigin::Path { path, source_token } => {
                parse_surface_path_key(stage, phase, path, source_token)
            }
        }
    }

    pub fn parse_surface_lineage_id(&self, phase: PhasePolicy) -> CaapResult<ArtifactKey> {
        match &self.origin {
            SourceOrigin::Inline { .. } => ArtifactKey::new([
                "parse-surface-inline".to_string(),
                phase.as_str().to_string(),
                self.fingerprint.as_str().to_string(),
            ]),
            SourceOrigin::Path { path, .. } => ArtifactKey::new([
                "parse-surface-source".to_string(),
                phase.as_str().to_string(),
                path.clone(),
            ]),
        }
    }
}

pub fn parse_surface_inline_key(
    stage: impl Into<String>,
    phase: PhasePolicy,
    fingerprint: &ArtifactFingerprint,
) -> CaapResult<ArtifactKey> {
    ArtifactKey::new([
        "parse-surface-inline".to_string(),
        stage.into(),
        phase.as_str().to_string(),
        fingerprint.as_str().to_string(),
    ])
}

pub fn parse_surface_path_key(
    stage: impl Into<String>,
    phase: PhasePolicy,
    path: impl Into<String>,
    source_token: impl Into<String>,
) -> CaapResult<ArtifactKey> {
    ArtifactKey::new([
        "parse-surface".to_string(),
        stage.into(),
        phase.as_str().to_string(),
        path.into(),
        source_token.into(),
    ])
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ArtifactValue {
    Text(String),
    Bytes(Vec<u8>),
    Source(SourceArtifact),
    Semantic(SemanticValue),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactInvalidationRecord {
    pub reason_kind: String,
    pub lineage_id: Option<ArtifactKey>,
    pub lineage_kind: Option<String>,
    pub invalidated_key: ArtifactKey,
    pub replacement_key: Option<ArtifactKey>,
    pub upstream_key: Option<ArtifactKey>,
    pub changed_inputs: Vec<String>,
}

impl ArtifactInvalidationRecord {
    pub fn new(reason_kind: impl Into<String>, invalidated_key: ArtifactKey) -> CaapResult<Self> {
        let reason_kind = reason_kind.into();
        if reason_kind.is_empty() {
            return Err(CaapError::artifacts(
                "artifact invalidation reason kind must be non-empty",
            ));
        }
        Ok(Self {
            reason_kind,
            lineage_id: None,
            lineage_kind: None,
            invalidated_key,
            replacement_key: None,
            upstream_key: None,
            changed_inputs: Vec::new(),
        })
    }

    pub fn with_replacement_key(mut self, replacement_key: ArtifactKey) -> Self {
        self.replacement_key = Some(replacement_key);
        self
    }

    pub fn with_lineage(
        mut self,
        lineage_id: ArtifactKey,
        lineage_kind: impl Into<String>,
    ) -> CaapResult<Self> {
        let lineage_kind = lineage_kind.into();
        if lineage_kind.is_empty() {
            return Err(CaapError::artifacts(
                "artifact invalidation lineage kind must be non-empty",
            ));
        }
        self.lineage_id = Some(lineage_id);
        self.lineage_kind = Some(lineage_kind);
        Ok(self)
    }

    pub fn with_upstream_key(mut self, upstream_key: ArtifactKey) -> Self {
        self.upstream_key = Some(upstream_key);
        self
    }

    pub fn with_changed_inputs(
        mut self,
        changed_inputs: impl IntoIterator<Item = String>,
    ) -> CaapResult<Self> {
        let mut changed_inputs: Vec<String> = changed_inputs.into_iter().collect();
        if changed_inputs.iter().any(String::is_empty) {
            return Err(CaapError::artifacts(
                "artifact invalidation changed inputs must be non-empty",
            ));
        }
        changed_inputs.sort();
        changed_inputs.dedup();
        self.changed_inputs = changed_inputs;
        Ok(self)
    }

    fn dependency(
        invalidated_key: ArtifactKey,
        upstream_key: ArtifactKey,
        lineage_id: Option<ArtifactKey>,
        changed_inputs: Vec<String>,
    ) -> Self {
        let lineage_kind = lineage_id.as_ref().map(|id| id.kind().to_string());
        Self {
            reason_kind: "dependency_invalidated".to_string(),
            lineage_id,
            lineage_kind,
            invalidated_key,
            replacement_key: None,
            upstream_key: Some(upstream_key),
            changed_inputs,
        }
    }
}

pub fn changed_inputs_for_lineage(
    lineage_id: &ArtifactKey,
    previous_key: &ArtifactKey,
    replacement_key: &ArtifactKey,
) -> Vec<String> {
    let labels: &[(&str, &[usize])] = match lineage_id.kind() {
        "parse-surface-source" => &[
            ("stage", &[1]),
            ("phase", &[2]),
            ("source_path", &[3]),
            ("source_token", &[4]),
        ],
        "parse-surface-inline" => &[("stage", &[1]), ("phase", &[2]), ("source_digest", &[3])],
        "unit-input" => &[
            ("stage", &[1]),
            ("unit_fingerprint", &[2]),
            ("phase", &[3]),
            ("names_version", &[4]),
        ],
        "stage-unit" => &[
            ("stage", &[1]),
            ("phase", &[2]),
            ("dependency_key", &[3]),
            ("builtin_version", &[4]),
            ("names_version", &[5]),
            ("session_version", &[6]),
            ("initial_bindings", &[7]),
        ],
        _ if previous_key.parts().len() == 7 && replacement_key.parts().len() == 7 => &[
            ("stage", &[0]),
            ("phase", &[1]),
            ("dependency_key", &[2]),
            ("builtin_version", &[3]),
            ("names_version", &[4]),
            ("session_version", &[5]),
            ("initial_bindings", &[6]),
        ],
        _ => &[],
    };

    let mut changed = Vec::new();
    for (label, positions) in labels {
        if positions
            .iter()
            .any(|position| previous_key.part(*position) != replacement_key.part(*position))
        {
            changed.push((*label).to_string());
        }
    }
    changed
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArtifactCacheSnapshot {
    pub entries: Vec<(ArtifactKey, ArtifactValue)>,
    pub dependencies: Vec<(ArtifactKey, Vec<ArtifactKey>)>,
    pub lineage_heads: Vec<(ArtifactKey, ArtifactKey)>,
    pub lineages: Vec<(ArtifactKey, ArtifactKey)>,
    pub invalidation_by_key: Vec<(ArtifactKey, ArtifactInvalidationRecord)>,
    pub invalidation_by_lineage: Vec<(ArtifactKey, ArtifactInvalidationRecord)>,
    pub dirty: Vec<(ArtifactKey, ArtifactInvalidationRecord)>,
    pub dirty_lineages: Vec<(ArtifactKey, ArtifactInvalidationRecord)>,
    pub stats: ArtifactCacheStats,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArtifactCacheFile {
    pub format_name: String,
    pub format_version: u32,
    pub snapshot: ArtifactCacheSnapshot,
}

impl ArtifactCacheFile {
    pub const FORMAT_NAME: &'static str = "caap-rust-artifact-cache";
    pub const FORMAT_VERSION: u32 = 1;

    pub fn new(snapshot: ArtifactCacheSnapshot) -> Self {
        Self {
            format_name: Self::FORMAT_NAME.to_string(),
            format_version: Self::FORMAT_VERSION,
            snapshot,
        }
    }

    pub fn validate(&self) -> CaapResult<()> {
        if self.format_name != Self::FORMAT_NAME {
            return Err(CaapError::artifacts(
                "artifact cache file format name is unsupported",
            ));
        }
        if self.format_version != Self::FORMAT_VERSION {
            return Err(CaapError::artifacts(
                "artifact cache file format version is unsupported",
            ));
        }
        Ok(())
    }

    pub fn to_json_string(&self) -> CaapResult<String> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(|error| {
            CaapError::artifacts(format!("failed to serialize artifact cache file: {error}"))
        })
    }

    pub fn from_json_str(text: &str) -> CaapResult<Self> {
        let cache_file: Self = serde_json::from_str(text).map_err(|error| {
            CaapError::artifacts(format!(
                "failed to deserialize artifact cache file: {error}"
            ))
        })?;
        cache_file.validate()?;
        Ok(cache_file)
    }

    pub fn write_json_file(&self, path: impl AsRef<Path>) -> CaapResult<()> {
        let path = path.as_ref();
        fs::write(path, self.to_json_string()?).map_err(|error| {
            CaapError::artifacts(format!(
                "failed to write artifact cache file {}: {error}",
                path.display()
            ))
        })
    }

    pub fn read_json_file(path: impl AsRef<Path>) -> CaapResult<Self> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|error| {
            CaapError::artifacts(format!(
                "failed to read artifact cache file {}: {error}",
                path.display()
            ))
        })?;
        Self::from_json_str(&text)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReusableArtifactCacheSnapshot {
    pub entries: BTreeMap<ArtifactKey, ArtifactValue>,
    pub dependencies: BTreeMap<ArtifactKey, Vec<ArtifactKey>>,
    pub dependents: BTreeMap<ArtifactKey, BTreeSet<ArtifactKey>>,
    pub lineage_heads: BTreeMap<ArtifactKey, ArtifactKey>,
    pub lineages: BTreeMap<ArtifactKey, ArtifactKey>,
    pub invalidation_by_key: BTreeMap<ArtifactKey, ArtifactInvalidationRecord>,
    pub invalidation_by_lineage: BTreeMap<ArtifactKey, ArtifactInvalidationRecord>,
    pub dirty: BTreeMap<ArtifactKey, ArtifactInvalidationRecord>,
    pub dirty_lineages: BTreeMap<ArtifactKey, ArtifactInvalidationRecord>,
    pub stats: ArtifactCacheStats,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArtifactCache {
    entries: BTreeMap<ArtifactKey, ArtifactValue>,
    dependencies: BTreeMap<ArtifactKey, Vec<ArtifactKey>>,
    dependents: BTreeMap<ArtifactKey, BTreeSet<ArtifactKey>>,
    lineage_heads: BTreeMap<ArtifactKey, ArtifactKey>,
    lineages: BTreeMap<ArtifactKey, ArtifactKey>,
    invalidation_by_key: BTreeMap<ArtifactKey, ArtifactInvalidationRecord>,
    invalidation_by_lineage: BTreeMap<ArtifactKey, ArtifactInvalidationRecord>,
    dirty: BTreeMap<ArtifactKey, ArtifactInvalidationRecord>,
    dirty_lineages: BTreeMap<ArtifactKey, ArtifactInvalidationRecord>,
    stats: ArtifactCacheStats,
}

impl ArtifactCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn store(
        &mut self,
        key: ArtifactKey,
        value: ArtifactValue,
        dependencies: impl IntoIterator<Item = ArtifactKey>,
    ) -> CaapResult<()> {
        self.store_internal(key, value, dependencies)
    }

    pub fn store_with_lineage(
        &mut self,
        key: ArtifactKey,
        value: ArtifactValue,
        dependencies: impl IntoIterator<Item = ArtifactKey>,
        lineage_id: ArtifactKey,
    ) -> CaapResult<()> {
        if let Some(previous) = self.lineage_heads.get(&lineage_id).cloned() {
            if previous != key {
                let changed_inputs = changed_inputs_for_lineage(&lineage_id, &previous, &key);
                let record = ArtifactInvalidationRecord::new("lineage_replaced", previous.clone())?
                    .with_replacement_key(key.clone())
                    .with_lineage(lineage_id.clone(), lineage_id.kind())?
                    .with_changed_inputs(changed_inputs)?;
                self.mark_dirty(record);
            }
        }

        self.lineage_heads.insert(lineage_id.clone(), key.clone());
        self.lineages.insert(key.clone(), lineage_id.clone());
        self.dirty_lineages.remove(&lineage_id);
        self.store_internal(key, value, dependencies)
    }

    fn store_internal(
        &mut self,
        key: ArtifactKey,
        value: ArtifactValue,
        dependencies: impl IntoIterator<Item = ArtifactKey>,
    ) -> CaapResult<()> {
        let mut dependencies: Vec<ArtifactKey> = dependencies.into_iter().collect();
        dependencies.sort();
        dependencies.dedup();
        if dependencies.iter().any(|dependency| dependency == &key) {
            return Err(CaapError::artifacts("artifact cannot depend on itself"));
        }

        if let Some(old_dependencies) = self.dependencies.remove(&key) {
            for dependency in old_dependencies {
                if let Some(dependents) = self.dependents.get_mut(&dependency) {
                    dependents.remove(&key);
                    if dependents.is_empty() {
                        self.dependents.remove(&dependency);
                    }
                }
            }
        }

        for dependency in &dependencies {
            self.dependents
                .entry(dependency.clone())
                .or_default()
                .insert(key.clone());
        }

        self.dependencies.insert(key.clone(), dependencies);
        self.entries.insert(key.clone(), value);
        self.dirty.remove(&key);
        self.stats.generation += 1;
        Ok(())
    }

    pub fn get(&mut self, key: &ArtifactKey) -> Option<&ArtifactValue> {
        if self.is_dirty(key) || !self.entries.contains_key(key) {
            self.stats.misses += 1;
            return None;
        }
        self.stats.hits += 1;
        self.entries.get(key)
    }

    pub fn peek(&self, key: &ArtifactKey) -> Option<&ArtifactValue> {
        if self.is_dirty(key) {
            return None;
        }
        self.entries.get(key)
    }

    pub fn contains(&self, key: &ArtifactKey) -> bool {
        self.entries.contains_key(key)
    }

    pub fn is_dirty(&self, key: &ArtifactKey) -> bool {
        self.dirty.contains_key(key)
            || self
                .lineages
                .get(key)
                .is_some_and(|lineage_id| self.dirty_lineages.contains_key(lineage_id))
    }

    pub fn dirty_record(&self, key: &ArtifactKey) -> Option<&ArtifactInvalidationRecord> {
        self.dirty.get(key).or_else(|| {
            self.lineages
                .get(key)
                .and_then(|lineage_id| self.dirty_lineages.get(lineage_id))
        })
    }

    pub fn lineage_head(&self, lineage_id: &ArtifactKey) -> Option<&ArtifactKey> {
        self.lineage_heads.get(lineage_id)
    }

    pub fn lineage_id_for_key(&self, key: &ArtifactKey) -> Option<&ArtifactKey> {
        self.lineages.get(key)
    }

    pub fn latest_invalidation_for_key(
        &self,
        key: &ArtifactKey,
    ) -> Option<&ArtifactInvalidationRecord> {
        self.invalidation_by_key.get(key)
    }

    pub fn latest_invalidation_for_lineage(
        &self,
        lineage_id: &ArtifactKey,
    ) -> Option<&ArtifactInvalidationRecord> {
        self.invalidation_by_lineage.get(lineage_id)
    }

    pub fn is_lineage_dirty(&self, lineage_id: &ArtifactKey) -> bool {
        self.dirty_lineages.contains_key(lineage_id)
    }

    pub fn dependencies_for(&self, key: &ArtifactKey) -> Option<&[ArtifactKey]> {
        self.dependencies.get(key).map(Vec::as_slice)
    }

    pub fn dependents_for(&self, key: &ArtifactKey) -> Vec<ArtifactKey> {
        self.dependents
            .get(key)
            .map(|dependents| dependents.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn mark_dirty(&mut self, record: ArtifactInvalidationRecord) {
        let root = record.invalidated_key.clone();
        self.record_invalidation(&record);
        self.dirty.insert(root.clone(), record);

        self.mark_dependents_dirty(
            root,
            ChangedArtifactInputs::default(),
            DependentInvalidationMode::Conservative,
        );
        self.stats.generation += 1;
    }

    pub fn mark_dirty_with_changes(
        &mut self,
        record: ArtifactInvalidationRecord,
        changed_subjects: impl IntoIterator<Item = String>,
        changed_cells: impl IntoIterator<Item = String>,
        changed_files: impl IntoIterator<Item = String>,
    ) -> CaapResult<()> {
        let root = record.invalidated_key.clone();
        let changes = ChangedArtifactInputs::new(changed_subjects, changed_cells, changed_files)?;
        self.record_invalidation(&record);
        self.dirty.insert(root.clone(), record);

        self.mark_dependents_dirty(root, changes, DependentInvalidationMode::ReadAware);
        self.stats.generation += 1;
        Ok(())
    }

    fn mark_dependents_dirty(
        &mut self,
        root: ArtifactKey,
        changes: ChangedArtifactInputs,
        mode: DependentInvalidationMode,
    ) {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<(ArtifactKey, ArtifactKey)> = self
            .dependents_for(&root)
            .into_iter()
            .map(|dependent| (dependent, root.clone()))
            .collect();

        while let Some((key, upstream_key)) = stack.pop() {
            if !seen.insert(key.clone()) {
                continue;
            }
            if mode == DependentInvalidationMode::ReadAware
                && artifact_change_is_irrelevant(self.entries.get(&key), &changes)
            {
                continue;
            }

            self.dirty.insert(
                key.clone(),
                ArtifactInvalidationRecord::dependency(
                    key.clone(),
                    upstream_key.clone(),
                    self.lineages.get(&key).cloned(),
                    self.dirty
                        .get(&upstream_key)
                        .map(|record| record.changed_inputs.clone())
                        .unwrap_or_default(),
                ),
            );
            let record = self
                .dirty
                .get(&key)
                .cloned()
                .expect("dirty record inserted");
            self.record_invalidation(&record);

            for dependent in self.dependents_for(&key) {
                stack.push((dependent, key.clone()));
            }
        }
    }

    fn record_invalidation(&mut self, record: &ArtifactInvalidationRecord) {
        self.invalidation_by_key
            .insert(record.invalidated_key.clone(), record.clone());
        if let Some(lineage_id) = &record.lineage_id {
            self.invalidation_by_lineage
                .insert(lineage_id.clone(), record.clone());
            self.dirty_lineages
                .insert(lineage_id.clone(), record.clone());
        }
    }

    pub fn invalidate_all(&mut self, reason_kind: impl Into<String>) -> CaapResult<()> {
        let reason_kind = reason_kind.into();
        if reason_kind.is_empty() {
            return Err(CaapError::artifacts(
                "artifact invalidation reason kind must be non-empty",
            ));
        }
        for key in self.entries.keys().cloned().collect::<Vec<_>>() {
            let record = ArtifactInvalidationRecord::new(reason_kind.clone(), key)
                .expect("validated non-empty reason kind");
            self.mark_dirty(record);
        }
        Ok(())
    }

    pub fn stats(&self) -> &ArtifactCacheStats {
        &self.stats
    }

    pub fn record_cache_hit(&mut self) {
        self.stats.hits += 1;
    }

    pub fn record_cache_miss(&mut self) {
        self.stats.misses += 1;
    }

    pub fn snapshot(&self) -> ArtifactCacheSnapshot {
        ArtifactCacheSnapshot {
            entries: self
                .entries
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            dependencies: self
                .dependencies
                .iter()
                .map(|(key, dependencies)| (key.clone(), dependencies.clone()))
                .collect(),
            lineage_heads: self
                .lineage_heads
                .iter()
                .map(|(lineage_id, key)| (lineage_id.clone(), key.clone()))
                .collect(),
            lineages: self
                .lineages
                .iter()
                .map(|(key, lineage_id)| (key.clone(), lineage_id.clone()))
                .collect(),
            invalidation_by_key: self
                .invalidation_by_key
                .iter()
                .map(|(key, record)| (key.clone(), record.clone()))
                .collect(),
            invalidation_by_lineage: self
                .invalidation_by_lineage
                .iter()
                .map(|(lineage_id, record)| (lineage_id.clone(), record.clone()))
                .collect(),
            dirty: self
                .dirty
                .iter()
                .map(|(key, record)| (key.clone(), record.clone()))
                .collect(),
            dirty_lineages: self
                .dirty_lineages
                .iter()
                .map(|(lineage_id, record)| (lineage_id.clone(), record.clone()))
                .collect(),
            stats: self.stats.clone(),
        }
    }

    pub fn project_snapshot_by_kind(&self, kind: &str) -> CaapResult<ArtifactCacheSnapshot> {
        if kind.is_empty() {
            return Err(CaapError::artifacts(
                "artifact projection kind must be non-empty",
            ));
        }
        let projected_keys: BTreeSet<ArtifactKey> = self
            .entries
            .keys()
            .filter(|key| key.kind() == kind)
            .cloned()
            .collect();
        let projected_lineages: BTreeSet<ArtifactKey> = self
            .lineages
            .iter()
            .filter(|(key, _)| projected_keys.contains(*key))
            .map(|(_, lineage_id)| lineage_id.clone())
            .collect();

        Ok(ArtifactCacheSnapshot {
            entries: self
                .entries
                .iter()
                .filter(|(key, _)| projected_keys.contains(*key))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            dependencies: self
                .dependencies
                .iter()
                .filter(|(key, _)| projected_keys.contains(*key))
                .map(|(key, dependencies)| {
                    (
                        key.clone(),
                        dependencies
                            .iter()
                            .filter(|dependency| projected_keys.contains(*dependency))
                            .cloned()
                            .collect(),
                    )
                })
                .collect(),
            lineage_heads: self
                .lineage_heads
                .iter()
                .filter(|(lineage_id, key)| {
                    projected_lineages.contains(*lineage_id) && projected_keys.contains(*key)
                })
                .map(|(lineage_id, key)| (lineage_id.clone(), key.clone()))
                .collect(),
            lineages: self
                .lineages
                .iter()
                .filter(|(key, lineage_id)| {
                    projected_keys.contains(*key) && projected_lineages.contains(*lineage_id)
                })
                .map(|(key, lineage_id)| (key.clone(), lineage_id.clone()))
                .collect(),
            invalidation_by_key: self
                .invalidation_by_key
                .iter()
                .filter(|(key, _)| projected_keys.contains(*key))
                .map(|(key, record)| (key.clone(), record.clone()))
                .collect(),
            invalidation_by_lineage: self
                .invalidation_by_lineage
                .iter()
                .filter(|(lineage_id, _)| projected_lineages.contains(*lineage_id))
                .map(|(lineage_id, record)| (lineage_id.clone(), record.clone()))
                .collect(),
            dirty: self
                .dirty
                .iter()
                .filter(|(key, _)| projected_keys.contains(*key))
                .map(|(key, record)| (key.clone(), record.clone()))
                .collect(),
            dirty_lineages: self
                .dirty_lineages
                .iter()
                .filter(|(lineage_id, _)| projected_lineages.contains(*lineage_id))
                .map(|(lineage_id, record)| (lineage_id.clone(), record.clone()))
                .collect(),
            stats: self.stats.clone(),
        })
    }

    pub fn cache_file(&self) -> ArtifactCacheFile {
        ArtifactCacheFile::new(self.snapshot())
    }

    pub fn restore_cache_file(&mut self, cache_file: ArtifactCacheFile) -> CaapResult<()> {
        cache_file.validate()?;
        self.restore_snapshot(cache_file.snapshot)
    }

    pub fn save_cache_file(&self, path: impl AsRef<Path>) -> CaapResult<()> {
        self.cache_file().write_json_file(path)
    }

    pub fn load_cache_file(&mut self, path: impl AsRef<Path>) -> CaapResult<()> {
        self.restore_cache_file(ArtifactCacheFile::read_json_file(path)?)
    }

    pub fn reusable_snapshot(&self) -> ReusableArtifactCacheSnapshot {
        ReusableArtifactCacheSnapshot {
            entries: self.entries.clone(),
            dependencies: self.dependencies.clone(),
            dependents: self.dependents.clone(),
            lineage_heads: self.lineage_heads.clone(),
            lineages: self.lineages.clone(),
            invalidation_by_key: self.invalidation_by_key.clone(),
            invalidation_by_lineage: self.invalidation_by_lineage.clone(),
            dirty: self.dirty.clone(),
            dirty_lineages: self.dirty_lineages.clone(),
            stats: self.stats.clone(),
        }
    }

    pub fn restore_reusable_snapshot(
        &mut self,
        snapshot: ReusableArtifactCacheSnapshot,
    ) -> CaapResult<()> {
        let snapshot = ArtifactCacheSnapshot {
            entries: snapshot.entries.into_iter().collect(),
            dependencies: snapshot.dependencies.into_iter().collect(),
            lineage_heads: snapshot.lineage_heads.into_iter().collect(),
            lineages: snapshot.lineages.into_iter().collect(),
            invalidation_by_key: snapshot.invalidation_by_key.into_iter().collect(),
            invalidation_by_lineage: snapshot.invalidation_by_lineage.into_iter().collect(),
            dirty: snapshot.dirty.into_iter().collect(),
            dirty_lineages: snapshot.dirty_lineages.into_iter().collect(),
            stats: snapshot.stats,
        };
        self.restore_snapshot(snapshot)
    }

    pub fn restore_snapshot(&mut self, snapshot: ArtifactCacheSnapshot) -> CaapResult<()> {
        let mut entries = BTreeMap::new();
        for (key, value) in snapshot.entries {
            entries.insert(key, value);
        }

        let mut dependencies = BTreeMap::new();
        let mut dependents: BTreeMap<ArtifactKey, BTreeSet<ArtifactKey>> = BTreeMap::new();
        for (key, mut key_dependencies) in snapshot.dependencies {
            if key_dependencies.iter().any(|dependency| dependency == &key) {
                return Err(CaapError::artifacts(
                    "artifact snapshot cannot contain self dependency",
                ));
            }
            key_dependencies.sort();
            key_dependencies.dedup();
            for dependency in &key_dependencies {
                dependents
                    .entry(dependency.clone())
                    .or_default()
                    .insert(key.clone());
            }
            dependencies.insert(key, key_dependencies);
        }

        let mut dirty = BTreeMap::new();
        for (key, record) in snapshot.dirty {
            if record.invalidated_key != key {
                return Err(CaapError::artifacts(
                    "artifact dirty snapshot key must match record key",
                ));
            }
            dirty.insert(key, record);
        }

        let lineage_heads = snapshot.lineage_heads.into_iter().collect();
        let lineages = snapshot.lineages.into_iter().collect();
        let invalidation_by_key = snapshot.invalidation_by_key.into_iter().collect();
        let invalidation_by_lineage = snapshot.invalidation_by_lineage.into_iter().collect();
        let dirty_lineages = snapshot.dirty_lineages.into_iter().collect();

        self.entries = entries;
        self.dependencies = dependencies;
        self.dependents = dependents;
        self.lineage_heads = lineage_heads;
        self.lineages = lineages;
        self.invalidation_by_key = invalidation_by_key;
        self.invalidation_by_lineage = invalidation_by_lineage;
        self.dirty = dirty;
        self.dirty_lineages = dirty_lineages;
        self.stats = snapshot.stats;
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct ChangedArtifactInputs {
    subjects: Vec<String>,
    cells: Vec<String>,
    files: Vec<String>,
}

impl ChangedArtifactInputs {
    fn new(
        subjects: impl IntoIterator<Item = String>,
        cells: impl IntoIterator<Item = String>,
        files: impl IntoIterator<Item = String>,
    ) -> CaapResult<Self> {
        Ok(Self {
            subjects: normalize_changed_inputs(subjects, "changed subjects")?,
            cells: normalize_changed_inputs(cells, "changed cells")?,
            files: normalize_changed_inputs(files, "changed files")?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DependentInvalidationMode {
    Conservative,
    ReadAware,
}

fn normalize_changed_inputs(
    values: impl IntoIterator<Item = String>,
    label: &str,
) -> CaapResult<Vec<String>> {
    let mut values: Vec<String> = values.into_iter().collect();
    if values.iter().any(String::is_empty) {
        return Err(CaapError::artifacts(format!(
            "artifact invalidation {label} must be non-empty"
        )));
    }
    values.sort();
    values.dedup();
    Ok(values)
}

fn artifact_change_is_irrelevant(
    artifact: Option<&ArtifactValue>,
    changes: &ChangedArtifactInputs,
) -> bool {
    let Some(ArtifactValue::Semantic(SemanticValue::Map(entries))) = artifact else {
        return false;
    };
    reads_are_disjoint(
        &semantic_artifact_reads(entries, "reads_subjects"),
        &changes.subjects,
    ) || reads_are_disjoint(
        &semantic_artifact_reads(entries, "read_cells"),
        &changes.cells,
    ) || reads_are_disjoint(
        &semantic_artifact_reads(entries, "reads_files"),
        &changes.files,
    )
}

fn semantic_artifact_reads(entries: &[(String, SemanticValue)], key: &str) -> Vec<String> {
    if let Some(reads) = semantic_map_string_list(entries, key) {
        return reads;
    }
    let mut reads = BTreeSet::new();
    let Some(SemanticValue::List(records)) = semantic_map_get(entries, "execution_summary") else {
        return Vec::new();
    };
    for record in records {
        let SemanticValue::Map(record_entries) = record else {
            continue;
        };
        if let Some(record_reads) = semantic_map_string_list(record_entries, key) {
            reads.extend(record_reads);
        }
    }
    reads.into_iter().collect()
}

fn semantic_map_get<'a>(
    entries: &'a [(String, SemanticValue)],
    key: &str,
) -> Option<&'a SemanticValue> {
    entries
        .iter()
        .find_map(|(entry_key, value)| (entry_key == key).then_some(value))
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

fn reads_are_disjoint(reads: &[String], changed: &[String]) -> bool {
    !changed.is_empty()
        && !reads.is_empty()
        && reads
            .iter()
            .collect::<BTreeSet<_>>()
            .is_disjoint(&changed.iter().collect::<BTreeSet<_>>())
}

#[derive(Clone, Debug)]
pub struct SourceTemplateArtifact {
    pub source: SourceArtifact,
    pub key: ArtifactKey,
    pub lineage_id: ArtifactKey,
    pub template: UnitTemplate,
    pub cache_hit: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SourceTemplateCache {
    cache: ArtifactCache,
    templates: BTreeMap<ArtifactKey, UnitTemplate>,
}

impl SourceTemplateCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn artifact_cache(&self) -> &ArtifactCache {
        &self.cache
    }

    pub fn artifact_cache_mut(&mut self) -> &mut ArtifactCache {
        &mut self.cache
    }

    pub fn load(
        &mut self,
        source: SourceArtifact,
        stage: impl Into<String>,
        phase: PhasePolicy,
        mut materialize: impl FnMut(&SourceArtifact) -> Result<UnitTemplate, String>,
    ) -> CaapResult<SourceTemplateArtifact> {
        let stage = stage.into();
        if stage.is_empty() {
            return Err(CaapError::artifacts(
                "source template cache stage must be non-empty",
            ));
        }
        let key = source.parse_surface_key(stage, phase)?;
        let lineage_id = source.parse_surface_lineage_id(phase)?;

        if !self.cache.is_dirty(&key)
            && self.cache.contains(&key)
            && self.templates.contains_key(&key)
        {
            self.cache.record_cache_hit();
            let template = self
                .templates
                .get(&key)
                .expect("checked template presence")
                .clone();
            return Ok(SourceTemplateArtifact {
                source,
                key,
                lineage_id,
                template,
                cache_hit: true,
            });
        }

        self.cache.record_cache_miss();
        let template = materialize(&source).map_err(CaapError::artifacts)?;
        self.cache.store_with_lineage(
            key.clone(),
            ArtifactValue::Source(source.clone()),
            [],
            lineage_id.clone(),
        )?;
        self.templates.insert(key.clone(), template.clone());

        Ok(SourceTemplateArtifact {
            source,
            key,
            lineage_id,
            template,
            cache_hit: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_key_rejects_empty_parts_and_displays_path() {
        assert!(ArtifactKey::new(Vec::<String>::new()).is_err());
        assert!(ArtifactKey::new(["source".to_string(), "".to_string()]).is_err());
        let key = ArtifactKey::new(["source".to_string(), "demo".to_string()]).unwrap();
        assert_eq!(key.kind(), "source");
        assert_eq!(key.to_string(), "source:demo");
    }

    #[test]
    fn artifact_cache_marks_dependents_dirty() {
        let input = ArtifactKey::single("input").unwrap();
        let output = ArtifactKey::single("output").unwrap();
        let mut cache = ArtifactCache::new();
        cache
            .store(input.clone(), ArtifactValue::Text("in".into()), [])
            .unwrap();
        cache
            .store(
                output.clone(),
                ArtifactValue::Text("out".into()),
                [input.clone()],
            )
            .unwrap();

        cache.mark_dirty(ArtifactInvalidationRecord::new("source_changed", input.clone()).unwrap());
        assert!(cache.is_dirty(&input));
        assert!(cache.is_dirty(&output));
        assert!(cache.peek(&output).is_none());
    }
}
