//! Generic semantic state substrate for the Rust CAAP port.
//!
//! This module intentionally stores symbols, semantic entries, stable ids, and
//! versioned facts without assigning language-specific import/export semantics.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{CaapError, CaapResult};
use crate::ir::NodeId;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct StableId(String);

impl StableId {
    pub fn new(value: impl Into<String>) -> CaapResult<Self> {
        let value = value.into();
        if value.is_empty() {
            return Err(CaapError::semantic("stable id must be non-empty"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct SemanticSubjectId {
    pub kind: String,
    pub value: String,
}

impl SemanticSubjectId {
    pub fn new(kind: impl Into<String>, value: impl Into<String>) -> CaapResult<Self> {
        let kind = kind.into();
        let value = value.into();
        if kind.is_empty() {
            return Err(CaapError::semantic(
                "semantic subject kind must be non-empty",
            ));
        }
        if value.is_empty() {
            return Err(CaapError::semantic(
                "semantic subject value must be non-empty",
            ));
        }
        Ok(Self { kind, value })
    }

    pub fn parse(value: &str) -> CaapResult<Self> {
        let Some((kind, rest)) = value.split_once(':') else {
            return Err(CaapError::semantic("semantic subject id must contain ':'"));
        };
        Self::new(kind, rest)
    }
}

pub fn subject_id(
    kind: impl Into<String>,
    value: impl Into<String>,
) -> CaapResult<SemanticSubjectId> {
    SemanticSubjectId::new(kind, value)
}

pub fn node_subject_id(node_id: NodeId) -> SemanticSubjectId {
    SemanticSubjectId {
        kind: "node".to_string(),
        value: node_id.to_string(),
    }
}

pub fn symbol_subject_id(name: impl Into<String>) -> CaapResult<SemanticSubjectId> {
    SemanticSubjectId::new("symbol", name)
}

pub fn semantic_entity_id(
    source: impl AsRef<str>,
    name: impl AsRef<str>,
    unit_id: Option<&str>,
) -> CaapResult<StableId> {
    let source = source.as_ref();
    let name = name.as_ref();
    if source.is_empty() {
        return Err(CaapError::semantic(
            "semantic entity source must be non-empty",
        ));
    }
    if name.is_empty() {
        return Err(CaapError::semantic(
            "semantic entity name must be non-empty",
        ));
    }
    if unit_id.is_some_and(str::is_empty) {
        return Err(CaapError::semantic(
            "semantic entity unit id must be non-empty when present",
        ));
    }
    let prefix = unit_id.map_or_else(|| "semantic".to_string(), |unit| format!("semantic:{unit}"));
    StableId::new(format!("{prefix}:{source}:{name}"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum PhasePolicy {
    Runtime,
    CompileTime,
    Dual,
}

impl PhasePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::CompileTime => "compile_time",
            Self::Dual => "dual",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum EvalPolicy {
    Eager,
    LazyIf,
    Sequential,
    SpecialForm,
}

impl EvalPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eager => "eager",
            Self::LazyIf => "lazy_if",
            Self::Sequential => "sequential",
            Self::SpecialForm => "special_form",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum ControlPolicy {
    Plain,
    ConditionalBranch,
    StructuredExit,
}

impl ControlPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::ConditionalBranch => "conditional_branch",
            Self::StructuredExit => "structured_exit",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum ScopePolicy {
    None,
    LexicalBinding,
}

impl ScopePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::LexicalBinding => "lexical_binding",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EffectPolicy {
    tags: Vec<String>,
}

impl EffectPolicy {
    pub fn new(tags: impl IntoIterator<Item = String>) -> CaapResult<Self> {
        let mut tags: Vec<String> = tags.into_iter().collect();
        if tags.iter().any(String::is_empty) {
            return Err(CaapError::semantic("effect policy tags must be non-empty"));
        }
        tags.sort();
        tags.dedup();
        Ok(Self { tags })
    }

    pub fn pure() -> Self {
        Self { tags: Vec::new() }
    }

    pub fn single(tag: impl Into<String>) -> CaapResult<Self> {
        Self::new([tag.into()])
    }

    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    pub fn is_pure(&self) -> bool {
        self.tags.is_empty()
    }

    pub fn allows(&self, tag: &str) -> bool {
        self.tags.iter().any(|candidate| candidate == tag)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    TopLevel,
    Parameter,
    Local,
    Injected,
    Builtin,
    External,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SymbolEntry {
    pub name: String,
    pub kind: SymbolKind,
    pub phase_policy: PhasePolicy,
    pub node_id: Option<NodeId>,
    pub public: bool,
    pub public_names: Vec<String>,
}

impl SymbolEntry {
    pub fn new(
        name: impl Into<String>,
        kind: SymbolKind,
        phase_policy: PhasePolicy,
        node_id: Option<NodeId>,
    ) -> CaapResult<Self> {
        let name = name.into();
        if name.is_empty() {
            return Err(CaapError::semantic("symbol name must be non-empty"));
        }
        Ok(Self {
            name,
            kind,
            phase_policy,
            node_id,
            public: false,
            public_names: Vec::new(),
        })
    }

    pub fn with_public_names(
        mut self,
        public_names: impl IntoIterator<Item = String>,
    ) -> CaapResult<Self> {
        let mut names: Vec<String> = public_names.into_iter().collect();
        if names.iter().any(String::is_empty) {
            return Err(CaapError::semantic("public symbol names must be non-empty"));
        }
        names.sort();
        names.dedup();
        self.public = !names.is_empty();
        self.public_names = names;
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum EntrySource {
    Builtin,
    TopLevel,
    Registered,
    Parameter,
    Local,
    External,
}

impl EntrySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::TopLevel => "top-level",
            Self::Registered => "registered",
            Self::Parameter => "parameter",
            Self::Local => "local",
            Self::External => "external",
        }
    }

    pub fn symbol_kind(self) -> SymbolKind {
        match self {
            Self::Builtin => SymbolKind::Builtin,
            Self::TopLevel => SymbolKind::TopLevel,
            Self::Registered => SymbolKind::Injected,
            Self::Parameter => SymbolKind::Parameter,
            Self::Local => SymbolKind::Local,
            Self::External => SymbolKind::External,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SemanticValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Node(NodeId),
    List(Vec<SemanticValue>),
    Map(Vec<(String, SemanticValue)>),
}

impl SemanticValue {
    pub fn map(entries: impl IntoIterator<Item = (String, SemanticValue)>) -> CaapResult<Self> {
        let mut entries: Vec<(String, SemanticValue)> = entries.into_iter().collect();
        if entries.iter().any(|(key, _)| key.is_empty()) {
            return Err(CaapError::semantic(
                "semantic value map keys must be non-empty",
            ));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(Self::Map(entries))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticEntry {
    pub name: String,
    pub source: EntrySource,
    pub phase_policy: PhasePolicy,
    pub effect_policy: EffectPolicy,
    pub eval_policy: EvalPolicy,
    pub control_policy: ControlPolicy,
    pub scope_policy: ScopePolicy,
    pub node_id: Option<NodeId>,
    pub unit_id: Option<String>,
    pub value: SemanticValue,
    pub stable_id: Option<StableId>,
}

impl SemanticEntry {
    pub fn new(name: impl Into<String>, source: EntrySource) -> CaapResult<Self> {
        let name = name.into();
        if name.is_empty() {
            return Err(CaapError::semantic("semantic entry name must be non-empty"));
        }
        Ok(Self {
            name,
            source,
            phase_policy: PhasePolicy::Runtime,
            effect_policy: EffectPolicy::pure(),
            eval_policy: EvalPolicy::Eager,
            control_policy: ControlPolicy::Plain,
            scope_policy: ScopePolicy::None,
            node_id: None,
            unit_id: None,
            value: SemanticValue::Null,
            stable_id: None,
        })
    }

    pub fn symbol(&self) -> CaapResult<SymbolEntry> {
        SymbolEntry::new(
            self.name.clone(),
            self.source.symbol_kind(),
            self.phase_policy,
            self.node_id,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticRegistrySnapshot {
    pub entries: Vec<(String, SemanticEntry)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SemanticRegistry {
    entries: BTreeMap<String, SemanticEntry>,
    parent: Option<Box<SemanticRegistry>>,
}

impl SemanticRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn define(&mut self, mut entry: SemanticEntry) -> CaapResult<()> {
        if entry.stable_id.is_none() {
            entry.stable_id = Some(semantic_entity_id(
                entry.source.as_str(),
                &entry.name,
                entry.unit_id.as_deref(),
            )?);
        }
        self.entries.insert(entry.name.clone(), entry);
        Ok(())
    }

    pub fn lookup(&self, name: &str) -> CaapResult<Option<&SemanticEntry>> {
        if name.is_empty() {
            return Err(CaapError::semantic(
                "semantic registry lookup name must be non-empty",
            ));
        }
        if let Some(entry) = self.entries.get(name) {
            return Ok(Some(entry));
        }
        match &self.parent {
            Some(parent) => parent.lookup(name),
            None => Ok(None),
        }
    }

    pub fn fork(&self) -> Self {
        Self {
            entries: BTreeMap::new(),
            parent: Some(Box::new(self.clone())),
        }
    }

    pub fn snapshot(&self) -> SemanticRegistrySnapshot {
        SemanticRegistrySnapshot {
            entries: self
                .entries
                .iter()
                .map(|(name, entry)| (name.clone(), entry.clone()))
                .collect(),
        }
    }

    pub fn restore_snapshot(&mut self, snapshot: SemanticRegistrySnapshot) -> CaapResult<()> {
        if self.parent.is_some() {
            return Err(CaapError::semantic(
                "cannot restore semantic registry snapshot into a forked registry",
            ));
        }
        let mut entries = BTreeMap::new();
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
            entries.insert(name, entry);
        }
        self.entries = entries;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SemanticGraphSnapshot {
    pub triples: Vec<(u64, SemanticSubjectId, String, SemanticValue)>,
    pub current_version: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SemanticGraph {
    current_version: u64,
    facts: BTreeMap<(SemanticSubjectId, String), Vec<(u64, SemanticValue)>>,
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
        let history = self.facts.entry((subject, predicate)).or_default();
        if history
            .last()
            .is_some_and(|(_, previous)| previous == &value)
        {
            return Ok(false);
        }
        history.push((self.current_version, value));
        Ok(true)
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
        Ok(self
            .facts
            .get(&(subject.clone(), predicate.to_string()))
            .and_then(|history| latest_at_or_before(history, version)))
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
        for ((fact_subject, fact_predicate), history) in &self.facts {
            if subject.is_some_and(|wanted| wanted != fact_subject) {
                continue;
            }
            if predicate.is_some_and(|wanted| wanted != fact_predicate) {
                continue;
            }
            if let Some(value) = latest_at_or_before(history, version) {
                results.push((fact_subject.clone(), fact_predicate.clone(), value.clone()));
            }
        }
        Ok(results)
    }

    pub fn commit(&mut self) -> u64 {
        self.current_version += 1;
        self.current_version
    }

    pub fn rollback_subject(&mut self, subject: &SemanticSubjectId) -> bool {
        let before = self.facts.len();
        self.facts
            .retain(|(fact_subject, _), _| fact_subject != subject);
        before != self.facts.len()
    }

    pub fn snapshot(&self) -> SemanticGraphSnapshot {
        let mut triples = Vec::new();
        for ((subject, predicate), history) in &self.facts {
            for (version, value) in history {
                triples.push((*version, subject.clone(), predicate.clone(), value.clone()));
            }
        }
        SemanticGraphSnapshot {
            triples,
            current_version: self.current_version,
        }
    }

    pub fn restore_snapshot(&mut self, snapshot: SemanticGraphSnapshot) -> CaapResult<()> {
        let mut facts: BTreeMap<(SemanticSubjectId, String), Vec<(u64, SemanticValue)>> =
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
            facts
                .entry((subject, predicate))
                .or_default()
                .push((version, value));
        }
        for history in facts.values_mut() {
            history.sort_by_key(|(version, _)| *version);
        }
        self.facts = facts;
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

    pub fn define_symbol(&mut self, entry: SymbolEntry) -> bool {
        let changed = self.symbols.get(&entry.name) != Some(&entry);
        if changed {
            let subject = symbol_subject_id(&entry.name).expect("symbol entry name is validated");
            self.bump_cell(&subject, "symbol.entry");
            self.symbols.insert(entry.name.clone(), entry);
            self.name_generation += 1;
            self.version += 1;
        }
        changed
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
        let removed = self.symbols.remove(name).is_some();
        if removed {
            let subject = symbol_subject_id(name).expect("symbol removal name is validated");
            self.bump_cell(&subject, "symbol.entry");
            self.name_generation += 1;
            self.version += 1;
        }
        Ok(removed)
    }

    pub fn define_semantic(&mut self, entry: SemanticEntry) -> CaapResult<()> {
        if self.semantics.lookup(&entry.name)? == Some(&entry) {
            return Ok(());
        }
        let subject = subject_id("semantic", entry.name.clone())?;
        self.semantics.define(entry)?;
        self.bump_cell(&subject, "semantic.entry");
        self.name_generation += 1;
        self.version += 1;
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
        let changed = self.stable_ids.get(&key) != Some(&stable_id);
        if changed {
            let subject = subject_id("stable-id", key.clone())?;
            self.bump_cell(&subject, "stable-id");
            self.stable_ids.insert(key, stable_id);
            self.version += 1;
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
        let facts = self.facts.as_mut().ok_or_else(|| {
            CaapError::semantic("unified semantic graph does not have an attached fact store")
        })?;
        let changed = facts.add_fact(subject.clone(), predicate.clone(), value)?;
        if changed {
            self.bump_cell(&subject, &predicate);
            self.fact_generation += 1;
            self.version += 1;
        }
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
        let version = self
            .facts
            .as_mut()
            .ok_or_else(|| {
                CaapError::semantic("unified semantic graph does not have an attached fact store")
            })?
            .commit();
        self.version += 1;
        Ok(version)
    }

    pub fn rollback_facts_for_subject(&mut self, subject: &SemanticSubjectId) -> CaapResult<bool> {
        let changed = self
            .facts
            .as_mut()
            .ok_or_else(|| {
                CaapError::semantic("unified semantic graph does not have an attached fact store")
            })?
            .rollback_subject(subject);
        if changed {
            self.bump_cell(subject, "_rollback");
            self.fact_generation += 1;
            self.version += 1;
        }
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

    pub fn commit_transaction(&mut self, _transaction: UnifiedSemanticTransaction) -> u64 {
        self.version += 1;
        self.version
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
        self.symbols = snapshot.symbols.into_iter().collect();
        self.semantics.restore_snapshot(snapshot.semantics)?;
        self.facts = match snapshot.facts {
            Some(facts_snapshot) => {
                let mut facts = SemanticGraph::new();
                facts.restore_snapshot(facts_snapshot)?;
                Some(facts)
            }
            None => None,
        };
        self.stable_ids = snapshot.stable_ids.into_iter().collect();
        self.cell_generations = snapshot.cell_generations.into_iter().collect();
        self.version = snapshot.version + 1;
        self.name_generation = snapshot.name_generation + 1;
        self.fact_generation = snapshot.fact_generation + 1;
        self.next_cell_generation = snapshot.next_cell_generation.max(1);
        Ok(())
    }

    fn bump_cell(&mut self, subject: &SemanticSubjectId, predicate: &str) {
        let key = cell_key(subject, predicate);
        let generation = self.next_cell_generation;
        self.cell_generations.insert(key, generation);
        self.next_cell_generation += 1;
    }
}

impl UnifiedSemanticTransaction {
    pub fn snapshot(&self) -> &UnifiedSemanticGraphSnapshot {
        &self.snapshot
    }
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
    fn unified_semantic_graph_tracks_symbol_versions_and_facts() {
        let mut graph = UnifiedSemanticGraph::new();
        let symbol =
            SymbolEntry::new("x", SymbolKind::TopLevel, PhasePolicy::Runtime, Some(1)).unwrap();
        assert!(graph.define_symbol(symbol));
        assert_eq!(graph.version(), 1);
        assert_eq!(graph.name_generation(), 1);

        let subject = node_subject_id(1);
        assert!(graph
            .set_fact(subject.clone(), "type", SemanticValue::Str("int".into()))
            .unwrap());
        assert_eq!(graph.fact_generation(), 1);
        assert!(graph.cell_generation(&subject, "type").is_some());
    }
}

impl Default for UnifiedSemanticGraph {
    fn default() -> Self {
        Self::new()
    }
}

fn latest_at_or_before(history: &[(u64, SemanticValue)], version: u64) -> Option<&SemanticValue> {
    history
        .iter()
        .rev()
        .find(|(entry_version, _)| *entry_version <= version)
        .map(|(_, value)| value)
}

fn cell_key(subject: &SemanticSubjectId, predicate: &str) -> String {
    format!("{}:{}:{predicate}", subject.kind, subject.value)
}
