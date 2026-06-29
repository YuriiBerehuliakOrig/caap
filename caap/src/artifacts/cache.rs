//! ArtifactCache implementation: LRU-free cache with lineage tracking and invalidation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{CaapError, CaapResult};

use super::defs::{changed_inputs_for_lineage, ArtifactCacheStats, ArtifactKey, ArtifactValue};

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

    pub(super) fn dependency(
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
    pub const FORMAT_NAME: &'static str = "caap_artifact_cache";
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
        let mut cache = ArtifactCache::new();
        cache.restore_snapshot(self.snapshot.clone())?;
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
                self.generation_after(2)?;
                self.mark_dirty(record)?;
            }
        }

        self.next_generation()?;
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
        let generation = self.next_generation()?;

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
        self.stats.generation = generation;
        Ok(())
    }

    pub fn get(&mut self, key: &ArtifactKey) -> Option<&ArtifactValue> {
        if self.is_dirty(key) {
            self.stats.record_miss();
            return None;
        }
        match self.entries.get(key) {
            Some(v) => {
                self.stats.record_hit();
                Some(v)
            }
            None => {
                self.stats.record_miss();
                None
            }
        }
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

    pub fn mark_dirty(&mut self, record: ArtifactInvalidationRecord) -> CaapResult<()> {
        validate_invalidation_record(&record)?;
        let generation = self.next_generation()?;
        let root = record.invalidated_key.clone();
        self.record_invalidation(&record);
        self.dirty.insert(root.clone(), record);

        self.mark_dependents_dirty(root);
        self.stats.generation = generation;
        Ok(())
    }

    pub fn mark_dirty_batch(
        &mut self,
        records: impl IntoIterator<Item = ArtifactInvalidationRecord>,
    ) -> CaapResult<()> {
        let records: Vec<ArtifactInvalidationRecord> = records.into_iter().collect();
        for record in &records {
            validate_invalidation_record(record)?;
        }
        let generation = self.generation_after(records.len())?;
        for record in records {
            self.mark_dirty_validated(record);
        }
        self.stats.generation = generation;
        Ok(())
    }

    fn mark_dirty_validated(&mut self, record: ArtifactInvalidationRecord) {
        let root = record.invalidated_key.clone();
        self.record_invalidation(&record);
        self.dirty.insert(root.clone(), record);
        self.mark_dependents_dirty(root);
    }

    fn next_generation(&self) -> CaapResult<u64> {
        self.generation_after(1)
    }

    fn generation_after(&self, increments: usize) -> CaapResult<u64> {
        let increments = u64::try_from(increments).map_err(|_| {
            CaapError::artifacts("artifact cache generation increment exceeds u64 range")
        })?;
        self.stats
            .generation
            .checked_add(increments)
            .ok_or_else(|| CaapError::artifacts("artifact cache generation overflow"))
    }

    fn mark_dependents_dirty(&mut self, root: ArtifactKey) {
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
            let record = ArtifactInvalidationRecord::dependency(
                key.clone(),
                upstream_key.clone(),
                self.lineages.get(&key).cloned(),
                self.dirty
                    .get(&upstream_key)
                    .map(|record| record.changed_inputs.clone())
                    .unwrap_or_default(),
            );
            self.dirty.insert(key.clone(), record.clone());
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
        let records = self
            .entries
            .keys()
            .cloned()
            .map(|key| ArtifactInvalidationRecord::new(reason_kind.clone(), key))
            .collect::<CaapResult<Vec<_>>>()?;
        self.mark_dirty_batch(records)
    }

    pub fn stats(&self) -> &ArtifactCacheStats {
        &self.stats
    }

    pub fn record_cache_hit(&mut self) {
        self.stats.record_hit();
    }

    pub fn record_cache_miss(&mut self) {
        self.stats.record_miss();
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
        let entries = restore_unique_artifact_map(snapshot.entries, "entries")?;

        let mut dependencies = BTreeMap::new();
        let mut dependents: BTreeMap<ArtifactKey, BTreeSet<ArtifactKey>> = BTreeMap::new();
        for (key, mut key_dependencies) in snapshot.dependencies {
            if !entries.contains_key(&key) {
                return Err(CaapError::artifacts(
                    "artifact snapshot dependency owner is missing from entries",
                ));
            }
            if dependencies.contains_key(&key) {
                return Err(CaapError::artifacts(format!(
                    "artifact snapshot dependencies key is duplicated: {key}"
                )));
            }
            if key_dependencies.iter().any(|dependency| dependency == &key) {
                return Err(CaapError::artifacts(
                    "artifact snapshot cannot contain self dependency",
                ));
            }
            key_dependencies.sort();
            if key_dependencies.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(CaapError::artifacts(format!(
                    "artifact snapshot dependencies for {key} contain duplicate dependency"
                )));
            }
            for dependency in &key_dependencies {
                if !entries.contains_key(dependency) {
                    return Err(CaapError::artifacts(
                        "artifact snapshot dependency is missing from entries",
                    ));
                }
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
            validate_invalidation_record(&record)?;
            if dirty.insert(key.clone(), record).is_some() {
                return Err(CaapError::artifacts(format!(
                    "artifact dirty snapshot key is duplicated: {key}"
                )));
            }
        }

        let lineage_heads = restore_unique_artifact_map(snapshot.lineage_heads, "lineage heads")?;
        let lineages = restore_unique_artifact_map(snapshot.lineages, "lineages")?;
        validate_snapshot_lineage_graph(&entries, &lineage_heads, &lineages)?;
        let invalidation_by_key =
            restore_invalidation_map(snapshot.invalidation_by_key, "invalidation by key", false)?;
        let invalidation_by_lineage = restore_invalidation_map(
            snapshot.invalidation_by_lineage,
            "invalidation by lineage",
            true,
        )?;
        let dirty_lineages =
            restore_invalidation_map(snapshot.dirty_lineages, "dirty lineages", true)?;

        for (key, record) in &invalidation_by_key {
            if &record.invalidated_key != key {
                return Err(CaapError::artifacts(
                    "artifact invalidation snapshot key must match record key",
                ));
            }
        }
        for (lineage_id, record) in &invalidation_by_lineage {
            if record.lineage_id.as_ref() != Some(lineage_id) {
                return Err(CaapError::artifacts(
                    "artifact lineage invalidation snapshot key must match record lineage id",
                ));
            }
        }
        for (lineage_id, record) in &dirty_lineages {
            if record.lineage_id.as_ref() != Some(lineage_id) {
                return Err(CaapError::artifacts(
                    "artifact dirty lineage snapshot key must match record lineage id",
                ));
            }
        }

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

fn validate_snapshot_lineage_graph(
    entries: &BTreeMap<ArtifactKey, ArtifactValue>,
    lineage_heads: &BTreeMap<ArtifactKey, ArtifactKey>,
    lineages: &BTreeMap<ArtifactKey, ArtifactKey>,
) -> CaapResult<()> {
    for (lineage_id, head_key) in lineage_heads {
        if !entries.contains_key(head_key) {
            return Err(CaapError::artifacts(
                "artifact snapshot lineage head is missing from entries",
            ));
        }
        match lineages.get(head_key) {
            Some(recorded_lineage) if recorded_lineage == lineage_id => {}
            Some(_) => {
                return Err(CaapError::artifacts(
                    "artifact snapshot lineage head does not match lineages back-reference",
                ))
            }
            None => {
                return Err(CaapError::artifacts(
                    "artifact snapshot lineage head is missing lineages back-reference",
                ))
            }
        }
    }

    for (key, lineage_id) in lineages {
        if !entries.contains_key(key) {
            return Err(CaapError::artifacts(
                "artifact snapshot lineage entry is missing from entries",
            ));
        }
        if !lineage_heads.contains_key(lineage_id) {
            return Err(CaapError::artifacts(
                "artifact snapshot lineage entry references missing lineage head",
            ));
        }
    }
    for key in lineages.keys() {
        let mut seen = BTreeSet::new();
        let mut current = key;
        while let Some(next) = lineages.get(current) {
            if !seen.insert(current.clone()) {
                return Err(CaapError::artifacts(
                    "artifact snapshot lineage graph contains a cycle",
                ));
            }
            current = next;
        }
    }
    Ok(())
}

fn restore_unique_artifact_map<V>(
    entries: Vec<(ArtifactKey, V)>,
    label: &str,
) -> CaapResult<BTreeMap<ArtifactKey, V>> {
    let mut map = BTreeMap::new();
    for (key, value) in entries {
        if map.insert(key.clone(), value).is_some() {
            return Err(CaapError::artifacts(format!(
                "artifact snapshot {label} key is duplicated: {key}"
            )));
        }
    }
    Ok(map)
}

fn restore_invalidation_map(
    entries: Vec<(ArtifactKey, ArtifactInvalidationRecord)>,
    label: &str,
    require_lineage: bool,
) -> CaapResult<BTreeMap<ArtifactKey, ArtifactInvalidationRecord>> {
    let mut map = BTreeMap::new();
    for (key, record) in entries {
        validate_invalidation_record(&record)?;
        if require_lineage && record.lineage_id.is_none() {
            return Err(CaapError::artifacts(format!(
                "artifact snapshot {label} records must include lineage id"
            )));
        }
        if map.insert(key.clone(), record).is_some() {
            return Err(CaapError::artifacts(format!(
                "artifact snapshot {label} key is duplicated: {key}"
            )));
        }
    }
    Ok(map)
}

fn validate_invalidation_record(record: &ArtifactInvalidationRecord) -> CaapResult<()> {
    if record.reason_kind.is_empty() {
        return Err(CaapError::artifacts(
            "artifact invalidation reason kind must be non-empty",
        ));
    }
    if record.changed_inputs.iter().any(String::is_empty) {
        return Err(CaapError::artifacts(
            "artifact invalidation changed inputs must be non-empty",
        ));
    }
    if record
        .changed_inputs
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return Err(CaapError::artifacts(
            "artifact invalidation changed inputs must be sorted and unique",
        ));
    }
    match (&record.lineage_id, &record.lineage_kind) {
        (Some(lineage_id), Some(lineage_kind)) => {
            if lineage_kind.is_empty() {
                return Err(CaapError::artifacts(
                    "artifact invalidation lineage kind must be non-empty",
                ));
            }
            if lineage_kind != lineage_id.kind() {
                return Err(CaapError::artifacts(
                    "artifact invalidation lineage kind must match lineage id kind",
                ));
            }
        }
        (None, None) => {}
        _ => {
            return Err(CaapError::artifacts(
                "artifact invalidation lineage id and kind must be present together",
            ))
        }
    }
    Ok(())
}
