use super::*;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use crate::error::{CaapError, CaapResult};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticRegistrySnapshot {
    pub entries: Vec<(String, SemanticEntry)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct SemanticRegistryScope {
    entries: HashMap<String, SemanticEntry>,
    parent: Option<Rc<SemanticRegistryScope>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SemanticRegistry {
    scope: Rc<SemanticRegistryScope>,
}

impl SemanticRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn define(&mut self, mut entry: SemanticEntry) -> CaapResult<()> {
        if entry.name.is_empty() {
            return Err(CaapError::semantic("semantic entry name must be non-empty"));
        }
        entry.value.validate()?;
        if entry.stable_id.is_none() {
            entry.stable_id = Some(semantic_entity_id(
                entry.source.as_str(),
                &entry.name,
                entry.unit_id.as_deref(),
            )?);
        }
        if let Some(stable_id) = &entry.stable_id {
            if let Some(owner) = self.stable_id_owner(stable_id) {
                if owner != entry.name {
                    return Err(CaapError::semantic(format!(
                        "stable id {:?} is already owned by semantic entry {:?}",
                        stable_id.as_str(),
                        owner
                    )));
                }
            }
        }
        Rc::make_mut(&mut self.scope)
            .entries
            .insert(entry.name.clone(), entry);
        Ok(())
    }

    pub fn lookup(&self, name: &str) -> CaapResult<Option<&SemanticEntry>> {
        if name.is_empty() {
            return Err(CaapError::semantic(
                "semantic registry lookup name must be non-empty",
            ));
        }
        Ok(Self::lookup_scope(&self.scope, name))
    }

    pub fn fork(&self) -> Self {
        Self {
            scope: Rc::new(SemanticRegistryScope {
                entries: HashMap::new(),
                parent: Some(Rc::clone(&self.scope)),
            }),
        }
    }

    pub fn snapshot(&self) -> SemanticRegistrySnapshot {
        let mut entries: Vec<_> = self
            .scope
            .entries
            .iter()
            .map(|(name, entry)| (name.clone(), entry.clone()))
            .collect();
        entries.sort_by(|(left, _), (right, _)| left.cmp(right));
        SemanticRegistrySnapshot { entries }
    }

    pub fn restore_snapshot(&mut self, snapshot: SemanticRegistrySnapshot) -> CaapResult<()> {
        if self.scope.parent.is_some() {
            return Err(CaapError::semantic(
                "cannot restore semantic registry snapshot into a forked registry",
            ));
        }
        let mut entries = HashMap::new();
        let mut stable_id_owners = BTreeMap::new();
        for (name, entry) in snapshot.entries {
            if name.is_empty() {
                return Err(CaapError::semantic(
                    "semantic registry snapshot entry name must be non-empty",
                ));
            }
            if entry.name != name {
                return Err(CaapError::semantic(
                    "semantic registry snapshot key must match entry name",
                ));
            }
            entry.value.validate()?;
            let stable_id = entry.stable_id.clone().ok_or_else(|| {
                CaapError::semantic("semantic registry snapshot entry stable id must be present")
            })?;
            if let Some(previous) = stable_id_owners.insert(stable_id.clone(), name.clone()) {
                return Err(CaapError::semantic(format!(
                    "semantic registry snapshot stable id {:?} is duplicated by entries {:?} and {:?}",
                    stable_id.as_str(),
                    previous,
                    name
                )));
            }
            entries.insert(name, entry);
        }
        Rc::make_mut(&mut self.scope).entries = entries;
        Ok(())
    }

    fn lookup_scope<'a>(
        scope: &'a Rc<SemanticRegistryScope>,
        name: &str,
    ) -> Option<&'a SemanticEntry> {
        scope.entries.get(name).or_else(|| {
            scope
                .parent
                .as_ref()
                .and_then(|parent| Self::lookup_scope(parent, name))
        })
    }

    fn stable_id_owner(&self, stable_id: &StableId) -> Option<&str> {
        Self::stable_id_owner_in_scope(&self.scope, stable_id)
    }

    fn stable_id_owner_in_scope<'a>(
        scope: &'a Rc<SemanticRegistryScope>,
        stable_id: &StableId,
    ) -> Option<&'a str> {
        scope
            .entries
            .values()
            .find(|entry| entry.stable_id.as_ref() == Some(stable_id))
            .map(|entry| entry.name.as_str())
            .or_else(|| {
                scope
                    .parent
                    .as_ref()
                    .and_then(|parent| Self::stable_id_owner_in_scope(parent, stable_id))
            })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticGraphSnapshot {
    pub triples: Vec<(u64, SemanticSubjectId, String, SemanticValue)>,
    /// Fact retractions as (version, subject, predicate) — see
    /// [`SemanticGraph::remove_fact`]. Defaults empty so snapshots produced
    /// before retraction support deserialize unchanged.
    #[serde(default)]
    pub retractions: Vec<(u64, SemanticSubjectId, String)>,
    pub current_version: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SemanticGraph {
    current_version: u64,
    // Two-level map: subject → predicate → history.
    // Outer lookup is O(log n) with no key clone; BTreeMap::get accepts
    // &SemanticSubjectId directly.  Inner lookup uses String::borrow() so
    // &str can be passed without allocation.
    facts: BTreeMap<SemanticSubjectId, BTreeMap<String, Vec<(u64, SemanticValue)>>>,
    // Retraction tombstones: subject → predicate → versions at which the fact
    // was deleted. History stays append-only (version queries and provenance
    // keep working); visibility at version V = latest fact entry ≤ V exists AND
    // is strictly newer than the latest retraction ≤ V. Within one uncommitted
    // batch (same version) the later mutation wins: add_fact pops a same-version
    // retraction, remove_fact records one.
    retractions: BTreeMap<SemanticSubjectId, BTreeMap<String, Vec<u64>>>,
}

fn checked_generation_increment(value: u64, label: &str) -> CaapResult<u64> {
    value
        .checked_add(1)
        .ok_or_else(|| CaapError::semantic(format!("{label} overflow")))
}

impl SemanticGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current_version(&self) -> u64 {
        self.current_version
    }

    pub fn add_fact(
        &mut self,
        subject: SemanticSubjectId,
        predicate: impl Into<String>,
        value: SemanticValue,
    ) -> CaapResult<bool> {
        let predicate = predicate.into();
        if predicate.is_empty() {
            return Err(CaapError::semantic(
                "semantic graph predicate must be non-empty",
            ));
        }
        value.validate()?;
        let history_key = (subject.clone(), predicate.clone());
        let history = self
            .facts
            .entry(subject)
            .or_default()
            .entry(predicate)
            .or_default();
        let retracted_now = self
            .retractions
            .get(&history_key.0)
            .and_then(|inner| inner.get(&history_key.1))
            .and_then(|versions| versions.last())
            .is_some_and(|v| *v == self.current_version);
        if !retracted_now
            && history
                .last()
                .is_some_and(|(_, previous)| previous == &value)
        {
            return Ok(false);
        }
        if retracted_now {
            // The add supersedes a retraction made in the same (uncommitted)
            // batch — last mutation wins within a version.
            if let Some(versions) = self
                .retractions
                .get_mut(&history_key.0)
                .and_then(|inner| inner.get_mut(&history_key.1))
            {
                versions.pop();
            }
        }
        history.push((self.current_version, value));
        Ok(true)
    }

    /// Retract the fact for `(subject, predicate)` from the current version
    /// onward. Returns `false` when no fact is currently visible. History is
    /// untouched (queries at older versions still see the fact); a re-add at a
    /// later version becomes visible again.
    pub fn remove_fact(
        &mut self,
        subject: &SemanticSubjectId,
        predicate: &str,
    ) -> CaapResult<bool> {
        if predicate.is_empty() {
            return Err(CaapError::semantic(
                "semantic graph predicate must be non-empty",
            ));
        }
        if self.get_fact(subject, predicate, None)?.is_none() {
            return Ok(false);
        }
        let versions = self
            .retractions
            .entry(subject.clone())
            .or_default()
            .entry(predicate.to_string())
            .or_default();
        if versions.last() != Some(&self.current_version) {
            versions.push(self.current_version);
        }
        Ok(true)
    }

    fn latest_retraction_at_or_before(
        &self,
        subject: &SemanticSubjectId,
        predicate: &str,
        version: u64,
    ) -> Option<u64> {
        let versions = self.retractions.get(subject)?.get(predicate)?;
        let idx = versions.partition_point(|v| *v <= version);
        (idx > 0).then(|| versions[idx - 1])
    }

    pub fn get_fact(
        &self,
        subject: &SemanticSubjectId,
        predicate: &str,
        version: Option<u64>,
    ) -> CaapResult<Option<&SemanticValue>> {
        if predicate.is_empty() {
            return Err(CaapError::semantic(
                "semantic graph predicate must be non-empty",
            ));
        }
        let version = version.unwrap_or(self.current_version);
        // No allocation: BTreeMap::get accepts &SemanticSubjectId directly;
        // inner get uses String's Borrow<str> impl to accept &str.
        let latest = self
            .facts
            .get(subject)
            .and_then(|inner| inner.get(predicate))
            .and_then(|history| latest_entry_at_or_before(history, version));
        Ok(latest.and_then(|(fact_version, value)| {
            match self.latest_retraction_at_or_before(subject, predicate, version) {
                Some(retracted_at) if retracted_at >= fact_version => None,
                _ => Some(value),
            }
        }))
    }

    pub fn query(
        &self,
        subject: Option<&SemanticSubjectId>,
        predicate: Option<&str>,
        version: Option<u64>,
    ) -> CaapResult<Vec<(SemanticSubjectId, String, SemanticValue)>> {
        if predicate.is_some_and(str::is_empty) {
            return Err(CaapError::semantic(
                "semantic graph predicate must be non-empty",
            ));
        }
        let version = version.unwrap_or(self.current_version);
        let mut results = Vec::new();
        for (fact_subject, inner) in &self.facts {
            if subject.is_some_and(|wanted| wanted != fact_subject) {
                continue;
            }
            for (fact_predicate, history) in inner {
                if predicate.is_some_and(|wanted| wanted != fact_predicate.as_str()) {
                    continue;
                }
                if let Some((fact_version, value)) = latest_entry_at_or_before(history, version) {
                    let retracted = self
                        .latest_retraction_at_or_before(fact_subject, fact_predicate, version)
                        .is_some_and(|retracted_at| retracted_at >= fact_version);
                    if !retracted {
                        results.push((fact_subject.clone(), fact_predicate.clone(), value.clone()));
                    }
                }
            }
        }
        Ok(results)
    }

    pub fn commit(&mut self) -> CaapResult<u64> {
        self.current_version =
            checked_generation_increment(self.current_version, "semantic fact graph version")?;
        Ok(self.current_version)
    }

    pub fn rollback_subject(&mut self, subject: &SemanticSubjectId) -> bool {
        self.facts.remove(subject).is_some()
    }

    pub fn snapshot(&self) -> SemanticGraphSnapshot {
        let mut triples = Vec::new();
        for (subject, inner) in &self.facts {
            for (predicate, history) in inner {
                for (version, value) in history {
                    triples.push((*version, subject.clone(), predicate.clone(), value.clone()));
                }
            }
        }
        let mut retractions = Vec::new();
        for (subject, inner) in &self.retractions {
            for (predicate, versions) in inner {
                for version in versions {
                    retractions.push((*version, subject.clone(), predicate.clone()));
                }
            }
        }
        SemanticGraphSnapshot {
            triples,
            retractions,
            current_version: self.current_version,
        }
    }

    pub fn restore_snapshot(&mut self, snapshot: SemanticGraphSnapshot) -> CaapResult<()> {
        let mut facts: BTreeMap<SemanticSubjectId, BTreeMap<String, Vec<(u64, SemanticValue)>>> =
            BTreeMap::new();
        for (version, subject, predicate, value) in snapshot.triples {
            if version > snapshot.current_version {
                return Err(CaapError::semantic(
                    "semantic graph triple version must not exceed current version",
                ));
            }
            if predicate.is_empty() {
                return Err(CaapError::semantic(
                    "semantic graph predicate must be non-empty",
                ));
            }
            value.validate()?;
            facts
                .entry(subject)
                .or_default()
                .entry(predicate)
                .or_default()
                .push((version, value));
        }
        for inner in facts.values_mut() {
            for history in inner.values_mut() {
                history.sort_by_key(|(version, _)| *version);
            }
        }
        let mut retractions: BTreeMap<SemanticSubjectId, BTreeMap<String, Vec<u64>>> =
            BTreeMap::new();
        for (version, subject, predicate) in snapshot.retractions {
            if version > snapshot.current_version {
                return Err(CaapError::semantic(
                    "semantic graph retraction version must not exceed current version",
                ));
            }
            if predicate.is_empty() {
                return Err(CaapError::semantic(
                    "semantic graph predicate must be non-empty",
                ));
            }
            retractions
                .entry(subject)
                .or_default()
                .entry(predicate)
                .or_default()
                .push(version);
        }
        for inner in retractions.values_mut() {
            for versions in inner.values_mut() {
                versions.sort_unstable();
            }
        }
        self.facts = facts;
        self.retractions = retractions;
        self.current_version = snapshot.current_version;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UnifiedSemanticGraphSnapshot {
    pub symbols: Vec<(String, SymbolEntry)>,
    pub semantics: SemanticRegistrySnapshot,
    pub facts: Option<SemanticGraphSnapshot>,
    pub stable_ids: Vec<(String, StableId)>,
    pub cell_generations: Vec<(String, u64)>,
    pub version: u64,
    pub name_generation: u64,
    pub fact_generation: u64,
    pub next_cell_generation: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnifiedSemanticTransaction {
    snapshot: UnifiedSemanticGraphSnapshot,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnifiedSemanticGraph {
    symbols: BTreeMap<String, SymbolEntry>,
    semantics: SemanticRegistry,
    facts: Option<SemanticGraph>,
    stable_ids: BTreeMap<String, StableId>,
    cell_generations: BTreeMap<String, u64>,
    version: u64,
    name_generation: u64,
    fact_generation: u64,
    next_cell_generation: u64,
}

impl UnifiedSemanticGraph {
    pub fn new() -> Self {
        Self::with_facts(true)
    }

    pub fn without_facts() -> Self {
        Self::with_facts(false)
    }

    pub fn with_facts(include_facts: bool) -> Self {
        Self {
            symbols: BTreeMap::new(),
            semantics: SemanticRegistry::new(),
            facts: include_facts.then(SemanticGraph::new),
            stable_ids: BTreeMap::new(),
            cell_generations: BTreeMap::new(),
            version: 0,
            name_generation: 0,
            fact_generation: 0,
            next_cell_generation: 1,
        }
    }

    pub fn symbols(&self) -> &BTreeMap<String, SymbolEntry> {
        &self.symbols
    }

    pub fn stable_ids(&self) -> &BTreeMap<String, StableId> {
        &self.stable_ids
    }

    pub fn cell_generations(&self) -> &BTreeMap<String, u64> {
        &self.cell_generations
    }

    pub fn cell_generation(&self, subject: &SemanticSubjectId, predicate: &str) -> Option<u64> {
        self.cell_generations
            .get(&cell_key(subject, predicate))
            .copied()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn name_generation(&self) -> u64 {
        self.name_generation
    }

    pub fn fact_generation(&self) -> u64 {
        self.fact_generation
    }

    pub fn define_symbol(&mut self, entry: SymbolEntry) -> CaapResult<bool> {
        validate_symbol_entry(&entry)?;
        for public_name in &entry.public_names {
            for (existing_name, existing) in &self.symbols {
                if existing_name != &entry.name
                    && existing
                        .public_names
                        .iter()
                        .any(|candidate| candidate == public_name)
                {
                    return Err(CaapError::semantic(format!(
                        "public symbol name {public_name:?} is already owned by {existing_name:?}"
                    )));
                }
            }
        }
        let changed = self.symbols.get(&entry.name) != Some(&entry);
        if changed {
            let subject = symbol_subject_id(&entry.name)?;
            let cell = self.prepare_cell_bump()?;
            let name_generation =
                checked_generation_increment(self.name_generation, "semantic name generation")?;
            let version = checked_generation_increment(self.version, "semantic graph version")?;
            self.bump_cell_prepared(&subject, "symbol.entry", cell);
            self.symbols.insert(entry.name.clone(), entry);
            self.name_generation = name_generation;
            self.version = version;
        }
        Ok(changed)
    }

    pub fn lookup_symbol(&self, name: &str) -> CaapResult<Option<&SymbolEntry>> {
        if name.is_empty() {
            return Err(CaapError::semantic("symbol lookup name must be non-empty"));
        }
        Ok(self.symbols.get(name))
    }

    pub fn remove_symbol(&mut self, name: &str) -> CaapResult<bool> {
        if name.is_empty() {
            return Err(CaapError::semantic("symbol removal name must be non-empty"));
        }
        let removed = self.symbols.contains_key(name);
        if removed {
            let subject = symbol_subject_id(name)?;
            let cell = self.prepare_cell_bump()?;
            let name_generation =
                checked_generation_increment(self.name_generation, "semantic name generation")?;
            let version = checked_generation_increment(self.version, "semantic graph version")?;
            self.bump_cell_prepared(&subject, "symbol.entry", cell);
            self.symbols.remove(name);
            self.name_generation = name_generation;
            self.version = version;
        }
        Ok(removed)
    }

    pub fn define_semantic(&mut self, entry: SemanticEntry) -> CaapResult<()> {
        if self.semantics.lookup(&entry.name)? == Some(&entry) {
            return Ok(());
        }
        let subject = subject_id("semantic", entry.name.clone())?;
        let cell = self.prepare_cell_bump()?;
        let name_generation =
            checked_generation_increment(self.name_generation, "semantic name generation")?;
        let version = checked_generation_increment(self.version, "semantic graph version")?;
        self.semantics.define(entry)?;
        self.bump_cell_prepared(&subject, "semantic.entry", cell);
        self.name_generation = name_generation;
        self.version = version;
        Ok(())
    }

    pub fn lookup_semantic(&self, name: &str) -> CaapResult<Option<&SemanticEntry>> {
        self.semantics.lookup(name)
    }

    pub fn record_stable_id(
        &mut self,
        key: impl Into<String>,
        stable_id: StableId,
    ) -> CaapResult<bool> {
        let key = key.into();
        if key.is_empty() {
            return Err(CaapError::semantic("stable id key must be non-empty"));
        }
        if let Some((existing_key, _)) = self
            .stable_ids
            .iter()
            .find(|(existing_key, existing_id)| *existing_key != &key && *existing_id == &stable_id)
        {
            return Err(CaapError::semantic(format!(
                "stable id {:?} is already recorded for key {:?}",
                stable_id.as_str(),
                existing_key
            )));
        }
        let changed = self.stable_ids.get(&key) != Some(&stable_id);
        if changed {
            let subject = subject_id("stable_id", key.clone())?;
            let cell = self.prepare_cell_bump()?;
            let version = checked_generation_increment(self.version, "semantic graph version")?;
            self.bump_cell_prepared(&subject, "stable_id", cell);
            self.stable_ids.insert(key, stable_id);
            self.version = version;
        }
        Ok(changed)
    }

    pub fn set_fact(
        &mut self,
        subject: SemanticSubjectId,
        predicate: impl Into<String>,
        value: SemanticValue,
    ) -> CaapResult<bool> {
        let predicate = predicate.into();
        value.validate()?;
        let changed = self
            .facts
            .as_ref()
            .ok_or_else(|| {
                CaapError::semantic("unified semantic graph does not have an attached fact store")
            })?
            .get_fact(&subject, &predicate, None)?
            != Some(&value);
        if !changed {
            return Ok(false);
        }
        let cell = self.prepare_cell_bump()?;
        let fact_generation =
            checked_generation_increment(self.fact_generation, "semantic fact generation")?;
        let version = checked_generation_increment(self.version, "semantic graph version")?;
        let facts = self.facts.as_mut().ok_or_else(|| {
            CaapError::semantic("unified semantic graph does not have an attached fact store")
        })?;
        let changed = facts.add_fact(subject.clone(), predicate.clone(), value)?;
        debug_assert!(changed);
        self.bump_cell_prepared(&subject, &predicate, cell);
        self.fact_generation = fact_generation;
        self.version = version;
        Ok(changed)
    }

    pub fn get_fact(
        &self,
        subject: &SemanticSubjectId,
        predicate: &str,
    ) -> CaapResult<Option<&SemanticValue>> {
        self.facts
            .as_ref()
            .ok_or_else(|| {
                CaapError::semantic("unified semantic graph does not have an attached fact store")
            })?
            .get_fact(subject, predicate, None)
    }

    /// Retract a fact: the delete twin of [`Self::set_fact`] — same cell bump
    /// and generation discipline, so dependency tracking sees the write.
    /// Returns `false` (and mutates nothing) when no fact is visible.
    pub fn remove_fact(&mut self, subject: SemanticSubjectId, predicate: &str) -> CaapResult<bool> {
        let visible = self
            .facts
            .as_ref()
            .ok_or_else(|| {
                CaapError::semantic("unified semantic graph does not have an attached fact store")
            })?
            .get_fact(&subject, predicate, None)?
            .is_some();
        if !visible {
            return Ok(false);
        }
        let cell = self.prepare_cell_bump()?;
        let fact_generation =
            checked_generation_increment(self.fact_generation, "semantic fact generation")?;
        let version = checked_generation_increment(self.version, "semantic graph version")?;
        let facts = self.facts.as_mut().ok_or_else(|| {
            CaapError::semantic("unified semantic graph does not have an attached fact store")
        })?;
        let changed = facts.remove_fact(&subject, predicate)?;
        debug_assert!(changed);
        self.bump_cell_prepared(&subject, predicate, cell);
        self.fact_generation = fact_generation;
        self.version = version;
        Ok(changed)
    }

    pub fn query_facts(
        &self,
        subject: Option<&SemanticSubjectId>,
        predicate: Option<&str>,
    ) -> CaapResult<Vec<(SemanticSubjectId, String, SemanticValue)>> {
        self.facts
            .as_ref()
            .ok_or_else(|| {
                CaapError::semantic("unified semantic graph does not have an attached fact store")
            })?
            .query(subject, predicate, None)
    }

    pub fn commit_facts(&mut self) -> CaapResult<u64> {
        let graph_version = checked_generation_increment(self.version, "semantic graph version")?;
        let version = self
            .facts
            .as_mut()
            .ok_or_else(|| {
                CaapError::semantic("unified semantic graph does not have an attached fact store")
            })?
            .commit()?;
        self.version = graph_version;
        Ok(version)
    }

    pub fn rollback_facts_for_subject(&mut self, subject: &SemanticSubjectId) -> CaapResult<bool> {
        let changed = self
            .facts
            .as_ref()
            .ok_or_else(|| {
                CaapError::semantic("unified semantic graph does not have an attached fact store")
            })?
            .facts
            .contains_key(subject);
        if !changed {
            return Ok(false);
        }
        let cell = self.prepare_cell_bump()?;
        let fact_generation =
            checked_generation_increment(self.fact_generation, "semantic fact generation")?;
        let version = checked_generation_increment(self.version, "semantic graph version")?;
        let changed = self
            .facts
            .as_mut()
            .ok_or_else(|| {
                CaapError::semantic("unified semantic graph does not have an attached fact store")
            })?
            .rollback_subject(subject);
        debug_assert!(changed);
        self.bump_cell_prepared(subject, "_rollback", cell);
        self.fact_generation = fact_generation;
        self.version = version;
        Ok(changed)
    }

    pub fn begin_transaction(&self) -> UnifiedSemanticTransaction {
        UnifiedSemanticTransaction {
            snapshot: self.snapshot(),
        }
    }

    pub fn rollback_transaction(
        &mut self,
        transaction: UnifiedSemanticTransaction,
    ) -> CaapResult<()> {
        self.restore_snapshot(transaction.snapshot)
    }

    pub fn commit_transaction(
        &mut self,
        _transaction: UnifiedSemanticTransaction,
    ) -> CaapResult<u64> {
        self.version = checked_generation_increment(self.version, "semantic graph version")?;
        Ok(self.version)
    }

    pub fn snapshot(&self) -> UnifiedSemanticGraphSnapshot {
        UnifiedSemanticGraphSnapshot {
            symbols: self
                .symbols
                .iter()
                .map(|(name, entry)| (name.clone(), entry.clone()))
                .collect(),
            semantics: self.semantics.snapshot(),
            facts: self.facts.as_ref().map(SemanticGraph::snapshot),
            stable_ids: self
                .stable_ids
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            cell_generations: self
                .cell_generations
                .iter()
                .map(|(key, generation)| (key.clone(), *generation))
                .collect(),
            version: self.version,
            name_generation: self.name_generation,
            fact_generation: self.fact_generation,
            next_cell_generation: self.next_cell_generation,
        }
    }

    pub fn restore_snapshot(&mut self, snapshot: UnifiedSemanticGraphSnapshot) -> CaapResult<()> {
        let symbols = restore_symbol_map(snapshot.symbols)?;
        let mut semantics = SemanticRegistry::new();
        semantics.restore_snapshot(snapshot.semantics)?;
        let facts = match snapshot.facts {
            Some(facts_snapshot) => {
                let mut facts = SemanticGraph::new();
                facts.restore_snapshot(facts_snapshot)?;
                Some(facts)
            }
            None => None,
        };
        let stable_ids = restore_stable_id_map(snapshot.stable_ids)?;
        let cell_generations = restore_generation_map(snapshot.cell_generations)?;
        let version = checked_generation_increment(snapshot.version, "semantic graph version")?;
        let name_generation =
            checked_generation_increment(snapshot.name_generation, "semantic name generation")?;
        let fact_generation =
            checked_generation_increment(snapshot.fact_generation, "semantic fact generation")?;
        let next_cell_generation = snapshot.next_cell_generation.max(1);
        self.symbols = symbols;
        self.semantics = semantics;
        self.facts = facts;
        self.stable_ids = stable_ids;
        self.cell_generations = cell_generations;
        self.version = version;
        self.name_generation = name_generation;
        self.fact_generation = fact_generation;
        self.next_cell_generation = next_cell_generation;
        Ok(())
    }

    fn prepare_cell_bump(&self) -> CaapResult<(u64, u64)> {
        let generation = self.next_cell_generation.max(1);
        let next_generation = checked_generation_increment(generation, "semantic cell generation")?;
        Ok((generation, next_generation))
    }

    fn bump_cell_prepared(
        &mut self,
        subject: &SemanticSubjectId,
        predicate: &str,
        (generation, next_generation): (u64, u64),
    ) {
        let key = cell_key(subject, predicate);
        self.cell_generations.insert(key, generation);
        self.next_cell_generation = next_generation;
    }
}

impl UnifiedSemanticTransaction {
    pub fn snapshot(&self) -> &UnifiedSemanticGraphSnapshot {
        &self.snapshot
    }
}

fn restore_symbol_map(
    entries: Vec<(String, SymbolEntry)>,
) -> CaapResult<BTreeMap<String, SymbolEntry>> {
    let mut symbols = BTreeMap::new();
    let mut public_name_owners = BTreeMap::new();
    for (key, entry) in entries {
        if key.is_empty() {
            return Err(CaapError::semantic(
                "semantic snapshot symbol key must be non-empty",
            ));
        }
        validate_symbol_entry(&entry)?;
        if key != entry.name {
            return Err(CaapError::semantic(format!(
                "semantic snapshot symbol key {key:?} does not match entry name {:?}",
                entry.name
            )));
        }
        if symbols.contains_key(&key) {
            return Err(CaapError::semantic(format!(
                "semantic snapshot symbol key is duplicated: {key}"
            )));
        }
        for public_name in &entry.public_names {
            if let Some(owner) = public_name_owners.insert(public_name.clone(), key.clone()) {
                return Err(CaapError::semantic(format!(
                    "semantic snapshot public symbol name {public_name:?} is shared by {owner:?} and {key:?}"
                )));
            }
        }
        symbols.insert(key, entry);
    }
    Ok(symbols)
}

fn validate_symbol_entry(entry: &SymbolEntry) -> CaapResult<()> {
    if entry.name.is_empty() {
        return Err(CaapError::semantic("symbol name must be non-empty"));
    }
    if entry.public == entry.public_names.is_empty() {
        return Err(CaapError::semantic(
            "symbol public flag must match public symbol names",
        ));
    }
    let mut local_public_names = BTreeSet::new();
    let mut previous_name: Option<&str> = None;
    for public_name in &entry.public_names {
        if public_name.is_empty() {
            return Err(CaapError::semantic("public symbol names must be non-empty"));
        }
        if !local_public_names.insert(public_name.clone()) {
            return Err(CaapError::semantic(format!(
                "public symbol name is duplicated: {public_name}"
            )));
        }
        if previous_name.is_some_and(|previous| previous > public_name.as_str()) {
            return Err(CaapError::semantic(
                "public symbol names must be sorted deterministically",
            ));
        }
        previous_name = Some(public_name.as_str());
    }
    Ok(())
}

fn restore_stable_id_map(
    entries: Vec<(String, StableId)>,
) -> CaapResult<BTreeMap<String, StableId>> {
    let mut stable_ids = BTreeMap::new();
    let mut stable_id_owners = BTreeMap::new();
    for (key, stable_id) in entries {
        if key.is_empty() {
            return Err(CaapError::semantic(
                "semantic snapshot stable id key must be non-empty",
            ));
        }
        if let Some(previous_key) = stable_id_owners.insert(stable_id.clone(), key.clone()) {
            return Err(CaapError::semantic(format!(
                "semantic snapshot stable id {:?} is duplicated by keys {:?} and {:?}",
                stable_id.as_str(),
                previous_key,
                key
            )));
        }
        if stable_ids.insert(key.clone(), stable_id).is_some() {
            return Err(CaapError::semantic(format!(
                "semantic snapshot stable id key is duplicated: {key}"
            )));
        }
    }
    Ok(stable_ids)
}

fn restore_generation_map(entries: Vec<(String, u64)>) -> CaapResult<BTreeMap<String, u64>> {
    let mut generations = BTreeMap::new();
    for (key, generation) in entries {
        if key.is_empty() {
            return Err(CaapError::semantic(
                "semantic snapshot cell generation key must be non-empty",
            ));
        }
        if generations.insert(key.clone(), generation).is_some() {
            return Err(CaapError::semantic(format!(
                "semantic snapshot cell generation key is duplicated: {key}"
            )));
        }
    }
    Ok(generations)
}

impl Default for UnifiedSemanticGraph {
    fn default() -> Self {
        Self::new()
    }
}

fn latest_entry_at_or_before(
    history: &[(u64, SemanticValue)],
    version: u64,
) -> Option<(u64, &SemanticValue)> {
    let idx = history.partition_point(|(v, _)| *v <= version);
    (idx > 0).then(|| (history[idx - 1].0, &history[idx - 1].1))
}

fn cell_key(subject: &SemanticSubjectId, predicate: &str) -> String {
    format!("{}:{}:{predicate}", subject.kind, subject.value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_registry_fork_reads_parent_without_mutating_it() {
        let mut parent = SemanticRegistry::new();
        parent
            .define(SemanticEntry::new("answer", EntrySource::Builtin).unwrap())
            .unwrap();

        let mut child = parent.fork();
        child
            .define(SemanticEntry::new("local", EntrySource::Local).unwrap())
            .unwrap();

        assert!(child.lookup("answer").unwrap().is_some());
        assert!(child.lookup("local").unwrap().is_some());
        assert!(parent.lookup("local").unwrap().is_none());
    }

    #[test]
    fn stable_id_deserialize_rejects_empty_value() {
        let err = serde_json::from_str::<StableId>(r#""""#).unwrap_err();
        assert!(err.to_string().contains("non-empty"));
    }

    #[test]
    fn semantic_subject_id_deserialize_rejects_empty_fields() {
        let err =
            serde_json::from_str::<SemanticSubjectId>(r#"{"kind":"","value":"x"}"#).unwrap_err();
        assert!(err.to_string().contains("kind must be non-empty"));

        let err =
            serde_json::from_str::<SemanticSubjectId>(r#"{"kind":"node","value":""}"#).unwrap_err();
        assert!(err.to_string().contains("value must be non-empty"));
    }

    #[test]
    fn semantic_registry_rejects_duplicate_stable_ids() {
        let mut registry = SemanticRegistry::new();
        let mut first = SemanticEntry::new("first", EntrySource::Local).unwrap();
        first.stable_id = Some(StableId::new("stable:shared").unwrap());
        let mut second = SemanticEntry::new("second", EntrySource::Local).unwrap();
        second.stable_id = Some(StableId::new("stable:shared").unwrap());

        registry.define(first).unwrap();
        let error = registry.define(second).unwrap_err().to_string();

        assert!(error.contains("stable:shared"));
        assert!(error.contains("first"));
    }

    #[test]
    fn semantic_registry_snapshot_restore_rejects_duplicate_stable_ids() {
        let mut registry = SemanticRegistry::new();
        let mut first = SemanticEntry::new("first", EntrySource::Local).unwrap();
        first.stable_id = Some(StableId::new("stable:shared").unwrap());
        let mut second = SemanticEntry::new("second", EntrySource::Local).unwrap();
        second.stable_id = Some(StableId::new("stable:shared").unwrap());
        let snapshot = SemanticRegistrySnapshot {
            entries: vec![("first".to_string(), first), ("second".to_string(), second)],
        };

        let error = registry.restore_snapshot(snapshot).unwrap_err().to_string();

        assert!(error.contains("stable:shared"));
        assert!(error.contains("duplicated"));
    }

    #[test]
    fn unified_semantic_graph_tracks_symbol_versions_and_facts() {
        let mut graph = UnifiedSemanticGraph::new();
        let symbol =
            SymbolEntry::new("x", SymbolKind::TopLevel, PhasePolicy::Runtime, Some(1)).unwrap();
        assert!(graph.define_symbol(symbol).unwrap());
        assert_eq!(graph.version(), 1);
        assert_eq!(graph.name_generation(), 1);

        let subject = node_subject_id(1);
        assert!(graph
            .set_fact(subject.clone(), "type", SemanticValue::Str("int".into()))
            .unwrap());
        assert_eq!(graph.fact_generation(), 1);
        assert!(graph.cell_generation(&subject, "type").is_some());
    }

    #[test]
    fn semantic_graph_commit_rejects_version_overflow_without_mutating() {
        let mut graph = SemanticGraph::new();
        graph.current_version = u64::MAX;

        let error = graph.commit().unwrap_err().to_string();

        assert!(error.contains("semantic fact graph version overflow"));
        assert_eq!(graph.current_version(), u64::MAX);
    }

    #[test]
    fn unified_semantic_graph_set_fact_rejects_cell_generation_overflow_without_mutating() {
        let mut graph = UnifiedSemanticGraph::new();
        graph.next_cell_generation = u64::MAX;
        let subject = node_subject_id(1);

        let error = graph
            .set_fact(subject.clone(), "type", SemanticValue::Str("int".into()))
            .unwrap_err()
            .to_string();

        assert!(error.contains("semantic cell generation overflow"));
        assert_eq!(graph.fact_generation(), 0);
        assert_eq!(graph.get_fact(&subject, "type").unwrap(), None);
        assert_eq!(graph.cell_generation(&subject, "type"), None);
    }

    #[test]
    fn unified_semantic_graph_commit_facts_rejects_version_overflow_without_mutating_facts() {
        let mut graph = UnifiedSemanticGraph::new();
        let subject = node_subject_id(1);
        graph
            .set_fact(subject, "type", SemanticValue::Str("int".into()))
            .unwrap();
        let facts_version = graph.facts.as_ref().unwrap().current_version();
        graph.version = u64::MAX;

        let error = graph.commit_facts().unwrap_err().to_string();

        assert!(error.contains("semantic graph version overflow"));
        assert_eq!(
            graph.facts.as_ref().unwrap().current_version(),
            facts_version
        );
        assert_eq!(graph.version(), u64::MAX);
    }

    #[test]
    fn unified_semantic_graph_rejects_duplicate_stable_id_values() {
        let mut graph = UnifiedSemanticGraph::new();

        graph
            .record_stable_id("node:1", StableId::new("stable:shared").unwrap())
            .unwrap();
        let error = graph
            .record_stable_id("node:2", StableId::new("stable:shared").unwrap())
            .unwrap_err()
            .to_string();

        assert!(error.contains("stable:shared"));
        assert!(error.contains("node:1"));
    }

    #[test]
    fn semantic_graph_rejects_invalid_fact_values() {
        let mut graph = SemanticGraph::new();
        let subject = node_subject_id(1);
        let invalid = SemanticValue::Map(vec![
            ("answer".to_string(), SemanticValue::Int(1)),
            ("answer".to_string(), SemanticValue::Int(2)),
        ]);

        let error = graph
            .add_fact(subject, "type", invalid)
            .unwrap_err()
            .to_string();

        assert!(error.contains("map keys must be unique"));
    }

    #[test]
    fn unified_semantic_graph_define_symbol_rejects_invalid_public_names() {
        let mut graph = UnifiedSemanticGraph::new();
        let mut symbol =
            SymbolEntry::new("x", SymbolKind::TopLevel, PhasePolicy::Runtime, None).unwrap();
        symbol.public = true;
        symbol.public_names = vec!["alias".to_string(), "alias".to_string()];

        let error = graph.define_symbol(symbol).unwrap_err().to_string();

        assert!(error.contains("duplicated"));
        assert!(error.contains("alias"));
    }

    #[test]
    fn unified_semantic_graph_define_symbol_rejects_shared_public_names() {
        let mut graph = UnifiedSemanticGraph::new();
        let first = SymbolEntry::new("first", SymbolKind::TopLevel, PhasePolicy::Runtime, None)
            .unwrap()
            .with_public_names(["shared".to_string()])
            .unwrap();
        let second = SymbolEntry::new("second", SymbolKind::TopLevel, PhasePolicy::Runtime, None)
            .unwrap()
            .with_public_names(["shared".to_string()])
            .unwrap();

        graph.define_symbol(first).unwrap();
        let error = graph.define_symbol(second).unwrap_err().to_string();

        assert!(error.contains("shared"));
        assert!(error.contains("first"));
    }

    #[test]
    fn unified_semantic_graph_define_symbol_rejects_public_flag_mismatch() {
        let mut graph = UnifiedSemanticGraph::new();
        let mut symbol =
            SymbolEntry::new("x", SymbolKind::TopLevel, PhasePolicy::Runtime, None).unwrap();
        symbol.public = true;

        let error = graph.define_symbol(symbol).unwrap_err().to_string();

        assert!(error.contains("public flag"));
    }

    #[test]
    fn unified_semantic_graph_snapshot_restore_rejects_duplicate_symbol_keys() {
        let mut graph = UnifiedSemanticGraph::new();
        let symbol =
            SymbolEntry::new("x", SymbolKind::TopLevel, PhasePolicy::Runtime, None).unwrap();
        let mut snapshot = graph.snapshot();
        snapshot.symbols = vec![("x".to_string(), symbol.clone()), ("x".to_string(), symbol)];

        let error = graph.restore_snapshot(snapshot).unwrap_err().to_string();

        assert!(error.contains("symbol key is duplicated"));
        assert!(error.contains("x"));
    }

    #[test]
    fn unified_semantic_graph_snapshot_restore_rejects_shared_public_names() {
        let mut graph = UnifiedSemanticGraph::new();
        let first = SymbolEntry::new("first", SymbolKind::TopLevel, PhasePolicy::Runtime, None)
            .unwrap()
            .with_public_names(["shared".to_string()])
            .unwrap();
        let second = SymbolEntry::new("second", SymbolKind::TopLevel, PhasePolicy::Runtime, None)
            .unwrap()
            .with_public_names(["shared".to_string()])
            .unwrap();
        let mut snapshot = graph.snapshot();
        snapshot.symbols = vec![("first".to_string(), first), ("second".to_string(), second)];

        let error = graph.restore_snapshot(snapshot).unwrap_err().to_string();

        assert!(error.contains("shared"));
        assert!(error.contains("first"));
        assert!(error.contains("second"));
    }

    #[test]
    fn unified_semantic_graph_snapshot_restore_rejects_duplicate_stable_id_keys() {
        let mut graph = UnifiedSemanticGraph::new();
        let mut snapshot = graph.snapshot();
        snapshot.stable_ids = vec![
            ("node:1".to_string(), StableId::new("stable:a").unwrap()),
            ("node:1".to_string(), StableId::new("stable:b").unwrap()),
        ];

        let error = graph.restore_snapshot(snapshot).unwrap_err().to_string();

        assert!(error.contains("stable id key is duplicated"));
        assert!(error.contains("node:1"));
    }

    #[test]
    fn unified_semantic_graph_snapshot_restore_rejects_duplicate_stable_id_values() {
        let mut graph = UnifiedSemanticGraph::new();
        let mut snapshot = graph.snapshot();
        snapshot.stable_ids = vec![
            (
                "node:1".to_string(),
                StableId::new("stable:shared").unwrap(),
            ),
            (
                "node:2".to_string(),
                StableId::new("stable:shared").unwrap(),
            ),
        ];

        let error = graph.restore_snapshot(snapshot).unwrap_err().to_string();

        assert!(error.contains("stable:shared"));
        assert!(error.contains("duplicated"));
    }

    #[test]
    fn unified_semantic_graph_snapshot_restore_rejects_duplicate_cell_generation_keys() {
        let mut graph = UnifiedSemanticGraph::new();
        let mut snapshot = graph.snapshot();
        snapshot.cell_generations = vec![
            ("node:1/type".to_string(), 1),
            ("node:1/type".to_string(), 2),
        ];

        let error = graph.restore_snapshot(snapshot).unwrap_err().to_string();

        assert!(error.contains("cell generation key is duplicated"));
        assert!(error.contains("node:1/type"));
    }

    #[test]
    fn unified_semantic_graph_snapshot_restore_rejects_version_overflow_without_mutating() {
        let mut graph = UnifiedSemanticGraph::new();
        graph
            .define_symbol(
                SymbolEntry::new("x", SymbolKind::TopLevel, PhasePolicy::Runtime, Some(1)).unwrap(),
            )
            .unwrap();
        let original = graph.snapshot();
        let mut snapshot = original.clone();
        snapshot.version = u64::MAX;

        let error = graph.restore_snapshot(snapshot).unwrap_err().to_string();

        assert!(error.contains("semantic graph version overflow"));
        assert_eq!(graph.snapshot(), original);
    }
}
