//! [`GrammarRegistry`] — a namespaced store of grammars used to resolve
//! cross-grammar imports (`grammar::rule`, `scope(…)`) at parse time, plus the
//! [`GrammarId`] identifier type and JSON-loading helpers.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;

use serde::Deserialize;
use serde_json::Value;

use crate::grammar::{try_parse_rules_from_text, Grammar, GrammarRule, GrammarState};
use crate::validation::validate_grammar;

/// A `serde_json::Value` carrying grammar source data.
pub type GrammarDataSource = Value;
/// A `serde_json::Value` carrying a JSON grammar payload.
pub type JsonGrammarPayload = Value;

/// A namespaced grammar identifier (`namespace.name`, or just `name`).
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct GrammarId {
    /// Optional dotted namespace.
    pub namespace: Option<String>,
    /// Local grammar name.
    pub name: String,
}

impl GrammarId {
    /// Build an id from an optional namespace and a name, validating both.
    pub fn new(namespace: Option<String>, name: impl Into<String>) -> Result<Self, RegistryError> {
        let name = normalize_local_name(&name.into())?;
        let namespace = namespace
            .map(|value| normalize_namespace(&value))
            .transpose()?;
        Ok(Self { namespace, name })
    }

    /// Parse a `namespace.name` (or bare `name`) string into a [`GrammarId`].
    pub fn parse(raw: &str) -> Result<Self, RegistryError> {
        let raw = normalize_grammar_id_text(raw)?;
        if raw.is_empty() {
            return Err(RegistryError::InvalidIdentifier(
                "grammar id must be non-empty".to_string(),
            ));
        }
        if raw.contains("..") {
            return Err(RegistryError::InvalidIdentifier(
                "grammar id must not contain empty namespace segments".to_string(),
            ));
        }
        if raw.starts_with('.') || raw.ends_with('.') {
            return Err(RegistryError::InvalidIdentifier(
                "grammar id must not start or end with '.'".to_string(),
            ));
        }
        match raw.rsplit_once('.') {
            Some((namespace, name)) => {
                if namespace.is_empty() || name.is_empty() {
                    return Err(RegistryError::InvalidIdentifier(
                        "grammar id cannot contain empty namespace or name".to_string(),
                    ));
                }
                if name.contains('.') {
                    return Err(RegistryError::InvalidIdentifier(
                        "grammar local name cannot contain '.'".to_string(),
                    ));
                }
                Ok(Self {
                    namespace: Some(normalize_namespace(namespace)?),
                    name: normalize_local_name(name)?,
                })
            }
            None => Ok(Self {
                namespace: None,
                name: normalize_local_name(raw)?,
            }),
        }
    }

    /// The fully-qualified `namespace.name` string (or bare name).
    pub fn qualified_name(&self) -> String {
        self.namespace
            .as_ref()
            .map_or_else(|| self.name.clone(), |ns| format!("{}.{}", ns, self.name))
    }
}

impl fmt::Display for GrammarId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.qualified_name())
    }
}

/// A registered grammar plus its canonical id, aliases, and origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryEntry {
    /// The canonical id this grammar is registered under.
    pub identifier: GrammarId,
    /// The stored grammar.
    pub grammar: Grammar,
    /// Additional ids that resolve to this entry.
    pub aliases: Vec<GrammarId>,
    /// Optional origin marker (e.g. a source path).
    pub origin: Option<String>,
}

/// An error returned by [`GrammarRegistry`] operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// A bare name resolved to more than one namespaced grammar.
    AmbiguousLookup {
        /// The looked-up name.
        name: String,
        /// The matching fully-qualified candidates.
        candidates: Vec<String>,
    },
    /// A grammar id was malformed.
    InvalidIdentifier(String),
    /// A grammar failed validation on registration.
    InvalidGrammar(String),
    /// A lookup referenced an unregistered grammar.
    UnknownGrammar(String),
    /// An alias collided with an existing id.
    AliasConflict(String),
}

impl std::error::Error for RegistryError {}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AmbiguousLookup { name, candidates } => {
                write!(
                    f,
                    "Grammar name '{name}' is ambiguous; use one of: {}",
                    candidates.join(", ")
                )
            }
            Self::InvalidIdentifier(message) => write!(f, "{message}"),
            Self::InvalidGrammar(message) => write!(f, "{message}"),
            Self::UnknownGrammar(id) => write!(f, "Grammar '{id}' is not registered"),
            Self::AliasConflict(message) => write!(f, "{message}"),
        }
    }
}

#[derive(Debug, Default, Clone)]
struct RegistryStorage {
    entries: HashMap<GrammarId, RegistryEntry>,
    aliases: HashMap<GrammarId, GrammarId>,
}

fn normalize_grammar_id_text(raw: &str) -> Result<&str, RegistryError> {
    if raw.trim() != raw || raw.chars().any(char::is_whitespace) {
        return Err(RegistryError::InvalidIdentifier(
            "grammar id must not contain whitespace".to_string(),
        ));
    }
    Ok(raw)
}

fn normalize_namespace(value: &str) -> Result<String, RegistryError> {
    let normalized = normalize_grammar_id_text(value)?;
    if normalized.is_empty() {
        return Err(RegistryError::InvalidIdentifier(
            "registry namespace must be non-empty".to_string(),
        ));
    }
    if normalized.contains("..") {
        return Err(RegistryError::InvalidIdentifier(
            "registry namespace contains empty segments".to_string(),
        ));
    }
    if normalized.starts_with('.') || normalized.ends_with('.') {
        return Err(RegistryError::InvalidIdentifier(
            "registry namespace must not start or end with '.'".to_string(),
        ));
    }
    Ok(normalized.to_string())
}

fn normalize_local_name(value: &str) -> Result<String, RegistryError> {
    let normalized = normalize_grammar_id_text(value)?;
    if normalized.is_empty() {
        return Err(RegistryError::InvalidIdentifier(
            "grammar local name must be non-empty".to_string(),
        ));
    }
    if normalized.contains('.') {
        return Err(RegistryError::InvalidIdentifier(
            "grammar local name cannot contain '.'".to_string(),
        ));
    }
    Ok(normalized.to_string())
}

fn coerce_grammar_id(
    raw: &str,
    namespace: Option<&str>,
    require_local_name: bool,
) -> Result<GrammarId, RegistryError> {
    let parsed = GrammarId::parse(raw)?;
    let namespace = namespace.map(normalize_namespace).transpose()?;
    match (&parsed.namespace, namespace.as_deref()) {
        (Some(_), Some(_)) => {
            if require_local_name {
                Err(RegistryError::InvalidIdentifier(
                    "scoped registration expects an unqualified local name".to_string(),
                ))
            } else {
                Err(RegistryError::InvalidIdentifier(
                    "cannot mix scoped namespace and dotted grammar id".to_string(),
                ))
            }
        }
        (Some(_), None) => Ok(parsed),
        (None, Some(ns)) => Ok(GrammarId {
            namespace: Some(ns.to_string()),
            name: parsed.name,
        }),
        (None, None) => Ok(parsed),
    }
}

fn coerce_entry_grammar_id(
    value: &GrammarId,
    namespace: Option<&str>,
) -> Result<GrammarId, RegistryError> {
    let namespace = namespace.map(normalize_namespace).transpose()?;
    match (&value.namespace, namespace.as_deref()) {
        (Some(actual), Some(expected)) if actual == expected => Ok(value.clone()),
        (None, Some(expected)) => Ok(GrammarId {
            namespace: Some(expected.to_string()),
            name: value.name.clone(),
        }),
        (Some(_), Some(expected)) => Err(RegistryError::InvalidIdentifier(format!(
            "Registry entry namespace mismatch; expected {expected:?}, got {value:?}"
        ))),
        (None, None) | (Some(_), None) => Ok(value.clone()),
    }
}

fn parse_aliases<'a>(
    aliases: impl IntoIterator<Item = &'a str>,
    namespace: Option<&str>,
) -> Result<Vec<GrammarId>, RegistryError> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for alias in aliases {
        let normalized = coerce_grammar_id(alias, namespace, false)?;
        if seen.insert(normalized.clone()) {
            out.push(normalized);
        }
    }
    out.sort_unstable();
    Ok(out)
}

fn coerce_entry_aliases(
    aliases: impl IntoIterator<Item = GrammarId>,
    namespace: Option<&str>,
) -> Result<Vec<GrammarId>, RegistryError> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for alias in aliases {
        let normalized = coerce_entry_grammar_id(&alias, namespace)?;
        if seen.insert(normalized.clone()) {
            out.push(normalized);
        }
    }
    out.sort_unstable();
    Ok(out)
}

impl RegistryEntry {
    fn build(
        identifier: GrammarId,
        grammar: Grammar,
        aliases: Vec<GrammarId>,
        origin: Option<String>,
        _lint: bool,
    ) -> Result<Self, RegistryError> {
        let mut grammar = grammar;
        grammar.seal();
        ensure_valid_registry_grammar(&grammar)?;

        Ok(Self {
            identifier,
            grammar,
            aliases,
            origin,
        })
    }
}

/// A namespaced, reference-counted store of grammars for import resolution.
#[derive(Debug, Clone)]
pub struct GrammarRegistry {
    storage: Rc<RefCell<RegistryStorage>>,
    default_namespace: Option<String>,
}

/// A [`GrammarRegistry`] view pinned to a default namespace.
#[derive(Debug, Clone)]
pub struct ScopedGrammarRegistry {
    storage: Rc<RefCell<RegistryStorage>>,
    namespace: String,
}

impl Default for GrammarRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl GrammarRegistry {
    /// An empty registry with no default namespace.
    pub fn new() -> Self {
        Self {
            storage: Rc::new(RefCell::new(RegistryStorage::default())),
            default_namespace: None,
        }
    }

    /// Build a registry from a map of `name -> grammar`.
    pub fn from_entries(entries: HashMap<String, Grammar>) -> Result<Self, RegistryError> {
        let mut registry = Self::new();
        registry.replace(entries, None)?;
        Ok(registry)
    }

    fn namespace_scope(&self, namespace: Option<&str>) -> Option<String> {
        namespace
            .map(str::to_string)
            .or_else(|| self.default_namespace.clone())
    }

    /// Register `grammar` under `name` with default options.
    pub fn register(&mut self, name: &str, grammar: Grammar) -> Result<(), RegistryError> {
        self.register_with_options(name, grammar, false, None, &[], None)
    }

    /// Register `grammar` under `name`, optionally linting, namespacing, aliasing,
    /// and recording an origin.
    pub fn register_with_options(
        &mut self,
        name: &str,
        grammar: Grammar,
        lint: bool,
        namespace: Option<&str>,
        aliases: &[&str],
        origin: Option<String>,
    ) -> Result<(), RegistryError> {
        let namespace = self.namespace_scope(namespace);
        let identifier = coerce_grammar_id(name, namespace.as_deref(), namespace.is_some())?;
        let aliases = parse_aliases(aliases.iter().copied(), namespace.as_deref())?
            .into_iter()
            .filter(|alias| alias != &identifier)
            .collect();
        let entry = RegistryEntry::build(identifier.clone(), grammar, aliases, origin, lint)?;

        let mut storage = self.storage.borrow_mut();
        if let Some(entry) = storage.entries.remove(&identifier) {
            for alias in entry.aliases {
                storage.aliases.remove(&alias);
            }
        }
        for alias in &entry.aliases {
            storage.aliases.remove(alias);
        }
        Self::check_aliases(&storage, &entry)?;
        for alias in &entry.aliases {
            storage.aliases.insert(alias.clone(), identifier.clone());
        }
        storage.entries.insert(identifier, entry);
        Ok(())
    }

    /// Register a pre-built [`RegistryEntry`], optionally linting/namespacing.
    pub fn register_entry(
        &mut self,
        entry: RegistryEntry,
        lint: bool,
        namespace: Option<&str>,
    ) -> Result<(), RegistryError> {
        let namespace = self.namespace_scope(namespace);
        let identifier = coerce_entry_grammar_id(&entry.identifier, namespace.as_deref())?;
        let aliases = coerce_entry_aliases(entry.aliases, namespace.as_deref())?
            .into_iter()
            .filter(|alias| alias != &identifier)
            .collect();
        let entry = RegistryEntry::build(identifier, entry.grammar, aliases, entry.origin, lint)?;

        let mut storage = self.storage.borrow_mut();
        if let Some(existing) = storage.entries.remove(&entry.identifier) {
            for alias in existing.aliases {
                storage.aliases.remove(&alias);
            }
        }
        Self::check_aliases(&storage, &entry)?;
        for alias in &entry.aliases {
            storage
                .aliases
                .insert(alias.clone(), entry.identifier.clone());
        }
        storage.entries.insert(entry.identifier.clone(), entry);
        Ok(())
    }

    fn check_aliases(
        storage: &RegistryStorage,
        entry: &RegistryEntry,
    ) -> Result<(), RegistryError> {
        for alias in &entry.aliases {
            if let Some(current) = storage.aliases.get(alias) {
                if *current != entry.identifier {
                    return Err(RegistryError::AliasConflict(format!(
                        "Grammar alias '{alias}' is already registered for '{current}'"
                    )));
                }
            }
            if let Some(existing_entry) = storage.entries.get(alias) {
                if existing_entry.identifier != entry.identifier {
                    return Err(RegistryError::AliasConflict(format!(
                        "Grammar alias '{alias}' conflicts with grammar '{}'",
                        existing_entry.identifier
                    )));
                }
            }
        }
        Ok(())
    }

    /// Look up a grammar by name (and optional namespace).
    pub fn get(&self, name: &str, namespace: Option<&str>) -> Result<Grammar, RegistryError> {
        self.get_entry(name, namespace).map(|entry| entry.grammar)
    }

    /// Look up a grammar's full [`RegistryEntry`] by name.
    pub fn get_entry(
        &self,
        name: &str,
        namespace: Option<&str>,
    ) -> Result<RegistryEntry, RegistryError> {
        let id = self.resolve_identifier(name, namespace)?;
        self.storage
            .borrow()
            .entries
            .get(&id)
            .cloned()
            .ok_or_else(|| RegistryError::UnknownGrammar(id.qualified_name()))
    }

    /// List qualified names of registered grammars (optionally in a namespace).
    pub fn list(&self, namespace: Option<&str>) -> Vec<String> {
        self.list_ids(namespace)
            .into_iter()
            .map(|id| id.qualified_name())
            .collect()
    }

    /// List ids of registered grammars (optionally in a namespace), sorted.
    pub fn list_ids(&self, namespace: Option<&str>) -> Vec<GrammarId> {
        let namespace = self.namespace_scope(namespace);
        let mut ids: Vec<GrammarId> = self.storage.borrow().entries.keys().cloned().collect();
        if let Some(namespace) = namespace {
            ids.retain(|id| id.namespace.as_deref() == Some(namespace.as_str()));
        }
        ids.sort_unstable();
        ids
    }

    /// Remove all grammars (or all within `namespace`).
    pub fn clear(&mut self, namespace: Option<&str>) {
        let namespace = self.namespace_scope(namespace);
        let mut storage = self.storage.borrow_mut();
        let Some(namespaced) = namespace else {
            storage.entries.clear();
            storage.aliases.clear();
            return;
        };
        let targets: Vec<GrammarId> = storage
            .entries
            .iter()
            .filter(|(id, _)| id.namespace.as_deref() == Some(namespaced.as_str()))
            .map(|(id, _)| id.clone())
            .collect();
        for target in targets {
            Self::remove_entry_locked(&mut storage, &target);
        }
    }

    /// Atomically replace the (namespaced) entries with `entries`.
    pub fn replace(
        &mut self,
        entries: HashMap<String, Grammar>,
        namespace: Option<&str>,
    ) -> Result<(), RegistryError> {
        let namespace = self.namespace_scope(namespace);
        let mut staged = GrammarRegistry {
            storage: Rc::new(RefCell::new(self.storage.borrow().clone())),
            default_namespace: self.default_namespace.clone(),
        };
        staged.clear(namespace.as_deref());
        for (name, grammar) in entries {
            staged.register_with_options(
                name.as_str(),
                grammar,
                false,
                namespace.as_deref(),
                &[],
                None,
            )?;
        }
        let next = staged.storage.borrow().clone();
        *self.storage.borrow_mut() = next;
        Ok(())
    }

    /// Atomically replace the (namespaced) entries with full [`RegistryEntry`] values.
    pub fn replace_entries(
        &mut self,
        entries: Vec<RegistryEntry>,
        namespace: Option<&str>,
    ) -> Result<(), RegistryError> {
        let namespace = self.namespace_scope(namespace);
        let mut staged = GrammarRegistry {
            storage: Rc::new(RefCell::new(self.storage.borrow().clone())),
            default_namespace: self.default_namespace.clone(),
        };
        staged.clear(namespace.as_deref());
        for entry in entries {
            staged.register_entry(entry, false, namespace.as_deref())?;
        }
        let next = staged.storage.borrow().clone();
        *self.storage.borrow_mut() = next;
        Ok(())
    }

    /// Snapshot the (namespaced) grammars as a `name -> grammar` map.
    pub fn snapshot(&self, namespace: Option<&str>) -> HashMap<String, Grammar> {
        self.snapshot_entries(namespace)
            .into_iter()
            .map(|(id, entry)| (id.qualified_name(), entry.grammar))
            .collect()
    }

    /// Snapshot the (namespaced) entries as an `id -> entry` map.
    pub fn snapshot_entries(&self, namespace: Option<&str>) -> HashMap<GrammarId, RegistryEntry> {
        let namespace = self.namespace_scope(namespace);
        let storage = self.storage.borrow();
        self.list_ids(namespace.as_deref())
            .into_iter()
            .filter_map(|id| storage.entries.get(&id).cloned().map(|entry| (id, entry)))
            .collect()
    }

    /// A namespace-pinned view onto this registry.
    pub fn scope(&self, namespace: &str) -> Result<ScopedGrammarRegistry, RegistryError> {
        Ok(ScopedGrammarRegistry {
            storage: Rc::clone(&self.storage),
            namespace: normalize_namespace(namespace)?,
        })
    }

    /// Resolve a grammar by name in the default namespace.
    pub fn resolve_grammar(&self, name: &str) -> Result<Grammar, RegistryError> {
        self.get(name, None)
    }

    fn resolve_identifier(
        &self,
        raw: &str,
        namespace: Option<&str>,
    ) -> Result<GrammarId, RegistryError> {
        let namespace = self.namespace_scope(namespace);
        let requested = coerce_grammar_id(raw, namespace.as_deref(), false)?;
        let storage = self.storage.borrow();

        if let Some(target) = storage.aliases.get(&requested) {
            return Ok(target.clone());
        }
        if storage.entries.contains_key(&requested) {
            return Ok(requested);
        }
        if requested.namespace.is_some() {
            return Err(RegistryError::UnknownGrammar(requested.qualified_name()));
        }

        let mut matches: HashSet<GrammarId> = storage
            .entries
            .keys()
            .filter(|id| id.name == requested.name)
            .cloned()
            .collect();
        for alias in storage.aliases.keys() {
            if alias.name == requested.name {
                if let Some(target) = storage.aliases.get(alias) {
                    matches.insert(target.clone());
                }
            }
        }
        match matches.len() {
            0 => Err(RegistryError::UnknownGrammar(requested.name)),
            1 => matches
                .into_iter()
                .next()
                .ok_or(RegistryError::UnknownGrammar(requested.name)),
            _ => Err(RegistryError::AmbiguousLookup {
                name: requested.name,
                candidates: matches.into_iter().map(|id| id.qualified_name()).collect(),
            }),
        }
    }

    fn remove_entry_locked(storage: &mut RegistryStorage, id: &GrammarId) {
        if let Some(entry) = storage.entries.remove(id) {
            for alias in entry.aliases {
                storage.aliases.remove(&alias);
            }
        }
    }
}

impl ScopedGrammarRegistry {
    /// The namespace this scoped view is pinned to.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    fn base(&self) -> GrammarRegistry {
        GrammarRegistry {
            storage: Rc::clone(&self.storage),
            default_namespace: Some(self.namespace.clone()),
        }
    }

    /// Register `grammar` under `name` in this scope's namespace.
    pub fn register(&mut self, name: &str, grammar: Grammar) -> Result<(), RegistryError> {
        self.base().register(name, grammar)
    }

    /// Register `grammar` with full options, defaulting to this scope's namespace.
    pub fn register_with_options(
        &mut self,
        name: &str,
        grammar: Grammar,
        lint: bool,
        namespace: Option<&str>,
        aliases: &[&str],
        origin: Option<String>,
    ) -> Result<(), RegistryError> {
        self.base()
            .register_with_options(name, grammar, lint, namespace, aliases, origin)
    }

    /// Look up a grammar by name within this scope.
    pub fn get(&self, name: &str, namespace: Option<&str>) -> Result<Grammar, RegistryError> {
        self.base().get(name, namespace)
    }

    /// List grammar ids in this scope's namespace.
    pub fn list_ids(&self) -> Vec<GrammarId> {
        self.base().list_ids(Some(&self.namespace))
    }

    /// List qualified grammar names in this scope's namespace.
    pub fn list(&self) -> Vec<String> {
        self.base().list(Some(&self.namespace))
    }

    /// Remove all grammars in this scope's namespace.
    pub fn clear(&mut self) {
        self.base().clear(Some(&self.namespace))
    }

    /// Atomically replace this scope's grammars.
    pub fn replace(&mut self, entries: HashMap<String, Grammar>) -> Result<(), RegistryError> {
        self.base().replace(entries, Some(&self.namespace))
    }

    /// Snapshot this scope's grammars as a `name -> grammar` map.
    pub fn snapshot(&self) -> HashMap<String, Grammar> {
        self.base().snapshot(Some(&self.namespace))
    }

    /// Snapshot this scope's entries as an `id -> entry` map.
    pub fn snapshot_entries(&self) -> HashMap<GrammarId, RegistryEntry> {
        self.base().snapshot_entries(Some(&self.namespace))
    }
}

#[derive(Debug, Deserialize)]
struct JsonGrammarRule {
    name: String,
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JsonGrammarPayloadObject {
    #[serde(rename = "start_rule")]
    start_rule: Option<String>,
    rules: Value,
    #[serde(default)]
    metadata: Option<HashMap<String, HashMap<String, GrammarDataSource>>>,
}

/// Parse grammar rule text into a [`Grammar`].
pub fn from_text(text: &str) -> Result<Grammar, RegistryError> {
    let rules = try_parse_rules_from_text(text)
        .map_err(|error| RegistryError::InvalidGrammar(error.message.to_string()))?;
    let grammar = Grammar {
        start_rule: "root".to_string(),
        text: text.to_string(),
        rules,
        metadata: HashMap::new(),
        imports: HashMap::new(),
        version: 1,
        state: GrammarState {
            sealed: false,
            analysis_state: None,
            version: 0,
        },
        compiled: Default::default(),
    };
    ensure_valid_registry_grammar(&grammar)?;
    Ok(grammar)
}

fn parse_rule_from_value(name: &str, node: &Value) -> Result<String, RegistryError> {
    match node {
        Value::String(source) => Ok(source.to_string()),
        Value::Object(obj) => {
            if let Some(source) = obj.get("source") {
                source.as_str().map(ToString::to_string).ok_or_else(|| {
                    RegistryError::InvalidGrammar(format!("Rule '{name}' must be a string source"))
                })
            } else {
                Err(RegistryError::InvalidGrammar(format!(
                    "Rule '{name}' has no supported string source"
                )))
            }
        }
        _ => Err(RegistryError::InvalidGrammar(format!(
            "Rule '{name}' has unsupported payload form"
        ))),
    }
}

/// Load a grammar from a JSON payload (`{start_rule, rules, metadata}`).
pub fn load_json_grammar(payload: &str) -> Result<Grammar, RegistryError> {
    let root: Value = serde_json::from_str(payload)
        .map_err(|error| RegistryError::InvalidGrammar(error.to_string()))?;
    let payload: JsonGrammarPayloadObject = serde_json::from_value(root)
        .map_err(|error| RegistryError::InvalidGrammar(error.to_string()))?;

    let start_rule = payload.start_rule.ok_or_else(|| {
        RegistryError::InvalidGrammar("JSON payload missing required 'start_rule'".to_string())
    })?;

    let mut rules = Vec::new();
    let mut seen = HashSet::new();
    match payload.rules {
        Value::Array(items) => {
            for raw_rule in items {
                let rule: JsonGrammarRule = serde_json::from_value(raw_rule)
                    .map_err(|error| RegistryError::InvalidGrammar(error.to_string()))?;
                if !seen.insert(rule.name.clone()) {
                    return Err(RegistryError::InvalidGrammar(format!(
                        "Duplicate grammar rule definition for '{}'",
                        rule.name
                    )));
                }
                let source = rule.source.ok_or_else(|| {
                    RegistryError::InvalidGrammar(format!(
                        "Rule '{}' must define 'source'",
                        rule.name
                    ))
                })?;
                rules.push(
                    GrammarRule::try_from_source(rule.name.clone(), source, Vec::new()).map_err(
                        |error| {
                            RegistryError::InvalidGrammar(format!(
                                "Rule '{}' has invalid source: {}",
                                rule.name, error.message
                            ))
                        },
                    )?,
                );
            }
        }
        Value::Object(mapping) => {
            for (name, node) in mapping {
                if !seen.insert(name.clone()) {
                    return Err(RegistryError::InvalidGrammar(format!(
                        "Duplicate grammar rule definition for '{}'",
                        name
                    )));
                }
                let source = parse_rule_from_value(&name, &node)?;
                rules.push(
                    GrammarRule::try_from_source(name.clone(), source, Vec::new()).map_err(
                        |error| {
                            RegistryError::InvalidGrammar(format!(
                                "Rule '{name}' has invalid source: {}",
                                error.message
                            ))
                        },
                    )?,
                );
            }
        }
        _ => {
            return Err(RegistryError::InvalidGrammar(
                "JSON payload 'rules' must be an array or object".to_string(),
            ))
        }
    }

    let text = rules
        .iter()
        .map(|rule| format!("{} <- {}", rule.name, rule.source))
        .collect::<Vec<_>>()
        .join("\n");
    let metadata = payload
        .metadata
        .unwrap_or_default()
        .into_iter()
        .map(|(owner, table)| (owner, table.into_iter().collect()))
        .collect();

    let grammar = Grammar {
        start_rule,
        text,
        rules,
        metadata,
        imports: std::collections::HashMap::new(),
        version: 1,
        state: GrammarState {
            sealed: true,
            analysis_state: None,
            version: 0,
        },
        compiled: Default::default(),
    };

    ensure_valid_registry_grammar(&grammar)?;
    Ok(grammar)
}

fn ensure_valid_registry_grammar(grammar: &Grammar) -> Result<(), RegistryError> {
    let report = validate_grammar(grammar);
    let errors: Vec<String> = report.errors().map(|issue| issue.message.clone()).collect();
    if errors.is_empty() {
        return Ok(());
    }
    Err(RegistryError::InvalidGrammar(errors.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_json_grammar_with_array_rules() {
        let payload = r#"{"start_rule":"root","rules":[{"name":"root","source":"\"x\""}]}"#;
        let grammar = load_json_grammar(payload).expect("json grammar loads");
        assert_eq!(grammar.start_rule, "root");
        assert_eq!(grammar.rules.len(), 1);
    }

    #[test]
    fn grammar_ids_reject_whitespace_instead_of_trimming() {
        for raw in [" root", "root ", "core. Expr", "core\t.Expr"] {
            let err = GrammarId::parse(raw).expect_err("whitespace must be rejected");
            assert!(err.to_string().contains("must not contain whitespace"));
        }

        let err = GrammarId::new(Some(" core".to_string()), "root")
            .expect_err("namespace whitespace must be rejected");
        assert!(err.to_string().contains("must not contain whitespace"));

        let err = GrammarId::new(None, " root").expect_err("name whitespace must be rejected");
        assert!(err.to_string().contains("must not contain whitespace"));
    }

    #[test]
    fn load_json_grammar_rejects_legacy_start_alias() {
        let payload = r#"{"start":"root","rules":[{"name":"root","source":"\"x\""}]}"#;
        let err = load_json_grammar(payload).expect_err("legacy start alias is rejected");
        assert!(err.to_string().contains("missing required 'start_rule'"));
    }

    #[test]
    fn load_json_grammar_rejects_legacy_rule_body_alias() {
        let payload = r#"{"start_rule":"root","rules":{"root":{"body":"\"x\""}}}"#;
        let err = load_json_grammar(payload).expect_err("legacy rule body alias is rejected");
        assert!(err.to_string().contains("no supported string source"));
    }

    #[test]
    fn from_text_loads_valid_root_grammar() {
        let grammar = from_text("root <- \"x\"").expect("text grammar loads");
        assert_eq!(grammar.start_rule, "root");
        assert_eq!(grammar.rules.len(), 1);
    }

    #[test]
    fn from_text_rejects_invalid_rule_source() {
        let err = from_text("root <- [a").expect_err("invalid source is rejected");
        assert!(matches!(err, RegistryError::InvalidGrammar(_)));
        assert!(err.to_string().contains("unterminated"));
    }

    #[test]
    fn from_text_rejects_missing_default_start_rule() {
        let err = from_text("item <- \"x\"").expect_err("missing root is rejected");
        assert!(matches!(err, RegistryError::InvalidGrammar(_)));
        assert!(err.to_string().contains("start rule 'root' is not defined"));
    }

    #[test]
    fn load_json_grammar_rejects_invalid_array_rule_source() {
        let payload = r#"{"start_rule":"root","rules":[{"name":"root","source":"[a"}]}"#;
        let err = load_json_grammar(payload).expect_err("invalid rule source is rejected");
        assert!(matches!(err, RegistryError::InvalidGrammar(_)));
        assert!(err.to_string().contains("invalid source"));
    }

    #[test]
    fn load_json_grammar_rejects_invalid_object_rule_source() {
        let payload = r#"{"start_rule":"root","rules":{"root":"[a"}}"#;
        let err = load_json_grammar(payload).expect_err("invalid rule source is rejected");
        assert!(matches!(err, RegistryError::InvalidGrammar(_)));
        assert!(err.to_string().contains("invalid source"));
    }

    #[test]
    fn load_json_grammar_rejects_invalid_metadata_types() {
        let payload = r#"{
            "start_rule":"root",
            "rules":[{"name":"root","source":"\"x\""}],
            "metadata":{"__grammar__":{"indentation":"off"}}
        }"#;
        let err = load_json_grammar(payload).expect_err("invalid metadata type is rejected");
        assert!(err
            .to_string()
            .contains("__grammar__.indentation metadata must be bool"));
    }

    #[test]
    fn registry_entry_rejects_invalid_metadata_types() {
        let mut grammar = Grammar::trusted_new("root <- \"x\"").with_start_rule("root");
        grammar.set_metadata_value("__grammar__", "trivia", serde_json::json!(false));
        let err = RegistryEntry::build(
            GrammarId::new(None, "root").unwrap(),
            grammar,
            Vec::new(),
            None,
            false,
        )
        .expect_err("invalid metadata type is rejected");
        assert!(err
            .to_string()
            .contains("__grammar__.trivia metadata must be string"));
    }

    #[test]
    fn registry_replaces_rules_by_fully_qualified_name() {
        let mut registry = GrammarRegistry::new();
        registry
            .register(
                "core.Expr",
                Grammar::trusted_new("start <- [a]").with_start_rule("start"),
            )
            .expect("first register");
        registry
            .register(
                "core.Expr",
                Grammar::trusted_new("start <- [b]").with_start_rule("start"),
            )
            .expect("second register");
        let grammar = registry
            .get("core.Expr", None)
            .expect("get registered grammar");
        assert_eq!(grammar.rules[0].source, "[b]");
    }
}
