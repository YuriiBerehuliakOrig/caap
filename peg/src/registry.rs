use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;

use serde::Deserialize;
use serde_json::Value;

use crate::analysis;
use crate::grammar::{Grammar, GrammarRule, GrammarState};

pub type GrammarDataSource = Value;
pub type JsonGrammarPayload = Value;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct GrammarId {
    pub namespace: Option<String>,
    pub name: String,
}

impl GrammarId {
    pub fn new(namespace: Option<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: namespace.map(|value| normalize_identifier(&value)),
            name: normalize_identifier(&name.into()),
        }
    }

    pub fn parse(raw: &str) -> Result<Self, RegistryError> {
        let raw = normalize_identifier(raw);
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
                    namespace: Some(namespace.to_string()),
                    name: normalize_identifier(name),
                })
            }
            None => Ok(Self {
                namespace: None,
                name: raw,
            }),
        }
    }

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryEntry {
    pub identifier: GrammarId,
    pub grammar: Grammar,
    pub aliases: Vec<GrammarId>,
    pub origin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    AmbiguousLookup {
        name: String,
        candidates: Vec<String>,
    },
    InvalidIdentifier(String),
    InvalidGrammar(String),
    UnknownGrammar(String),
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

fn normalize_identifier(raw: &str) -> String {
    raw.trim().to_string()
}

fn normalize_namespace(value: &str) -> Result<String, RegistryError> {
    let normalized = normalize_identifier(value);
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
    Ok(normalized)
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
        let report = analysis::analyze_grammar(&grammar);
        if !report.errors.is_empty() {
            return Err(RegistryError::InvalidGrammar(report.errors.join(", ")));
        }

        Ok(Self {
            identifier,
            grammar,
            aliases,
            origin,
        })
    }
}

#[derive(Debug, Clone)]
pub struct GrammarRegistry {
    storage: Rc<RefCell<RegistryStorage>>,
    default_namespace: Option<String>,
}

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
    pub fn new() -> Self {
        Self {
            storage: Rc::new(RefCell::new(RegistryStorage::default())),
            default_namespace: None,
        }
    }

    pub fn from_entries(entries: HashMap<String, Grammar>) -> Result<Self, RegistryError> {
        let mut registry = Self::new();
        registry.replace(entries, None)?;
        Ok(registry)
    }

    fn namespace_scope(&self, namespace: Option<&str>) -> Option<String> {
        namespace
            .map(normalize_identifier)
            .or_else(|| self.default_namespace.clone())
    }

    pub fn register(&mut self, name: &str, grammar: Grammar) -> Result<(), RegistryError> {
        self.register_with_options(name, grammar, false, None, &[], None)
    }

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

    pub fn get(&self, name: &str, namespace: Option<&str>) -> Result<Grammar, RegistryError> {
        self.get_entry(name, namespace).map(|entry| entry.grammar)
    }

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

    pub fn list(&self, namespace: Option<&str>) -> Vec<String> {
        self.list_ids(namespace)
            .into_iter()
            .map(|id| id.qualified_name())
            .collect()
    }

    pub fn list_ids(&self, namespace: Option<&str>) -> Vec<GrammarId> {
        let namespace = self.namespace_scope(namespace);
        let mut ids: Vec<GrammarId> = self.storage.borrow().entries.keys().cloned().collect();
        if let Some(namespace) = namespace {
            ids.retain(|id| id.namespace.as_deref() == Some(namespace.as_str()));
        }
        ids.sort_unstable();
        ids
    }

    pub fn clear(&mut self, namespace: Option<&str>) {
        let namespace = self.namespace_scope(namespace);
        let mut storage = self.storage.borrow_mut();
        if namespace.is_none() {
            storage.entries.clear();
            storage.aliases.clear();
            return;
        }
        let namespaced = namespace.unwrap();
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

    pub fn snapshot(&self, namespace: Option<&str>) -> HashMap<String, Grammar> {
        self.snapshot_entries(namespace)
            .into_iter()
            .map(|(id, entry)| (id.qualified_name(), entry.grammar))
            .collect()
    }

    pub fn snapshot_entries(&self, namespace: Option<&str>) -> HashMap<GrammarId, RegistryEntry> {
        let namespace = self.namespace_scope(namespace);
        let storage = self.storage.borrow();
        self.list_ids(namespace.as_deref())
            .into_iter()
            .filter_map(|id| storage.entries.get(&id).cloned().map(|entry| (id, entry)))
            .collect()
    }

    pub fn scope(&self, namespace: &str) -> Result<ScopedGrammarRegistry, RegistryError> {
        Ok(ScopedGrammarRegistry {
            storage: Rc::clone(&self.storage),
            namespace: normalize_namespace(namespace)?,
        })
    }

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
            1 => Ok(matches.into_iter().next().unwrap()),
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
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    fn base(&self) -> GrammarRegistry {
        GrammarRegistry {
            storage: Rc::clone(&self.storage),
            default_namespace: Some(self.namespace.clone()),
        }
    }

    pub fn register(&mut self, name: &str, grammar: Grammar) -> Result<(), RegistryError> {
        self.base().register(name, grammar)
    }

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

    pub fn get(&self, name: &str, namespace: Option<&str>) -> Result<Grammar, RegistryError> {
        self.base().get(name, namespace)
    }

    pub fn list_ids(&self) -> Vec<GrammarId> {
        self.base().list_ids(Some(&self.namespace))
    }

    pub fn list(&self) -> Vec<String> {
        self.base().list(Some(&self.namespace))
    }

    pub fn clear(&mut self) {
        self.base().clear(Some(&self.namespace))
    }

    pub fn replace(&mut self, entries: HashMap<String, Grammar>) -> Result<(), RegistryError> {
        self.base().replace(entries, Some(&self.namespace))
    }

    pub fn snapshot(&self) -> HashMap<String, Grammar> {
        self.base().snapshot(Some(&self.namespace))
    }

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
    #[serde(rename = "start_rule", alias = "start")]
    start_rule: Option<String>,
    rules: Value,
    #[serde(default)]
    metadata: Option<HashMap<String, HashMap<String, GrammarDataSource>>>,
}

pub fn from_text(text: &str) -> Grammar {
    Grammar::new(text)
}

fn parse_rule_from_value(name: &str, node: &Value) -> Result<String, RegistryError> {
    match node {
        Value::String(source) => Ok(source.to_string()),
        Value::Object(obj) => {
            if let Some(Value::String(source)) = obj.get("body") {
                Ok(source.to_string())
            } else if let Some(source) = obj.get("source") {
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
                rules.push(GrammarRule::from_source(
                    rule.name.clone(),
                    rule.source.ok_or_else(|| {
                        RegistryError::InvalidGrammar(format!(
                            "Rule '{}' must define 'source'",
                            rule.name
                        ))
                    })?,
                    Vec::new(),
                ));
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
                rules.push(GrammarRule::from_source(
                    name.clone(),
                    parse_rule_from_value(&name, &node)?,
                    Vec::new(),
                ));
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
    };

    let report = analysis::analyze_grammar(&grammar);
    if !report.errors.is_empty() {
        return Err(RegistryError::InvalidGrammar(report.errors.join(", ")));
    }
    Ok(grammar)
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
    fn registry_replaces_rules_by_fully_qualified_name() {
        let mut registry = GrammarRegistry::new();
        registry
            .register(
                "core.Expr",
                Grammar::new("start <- [a]").with_start_rule("start"),
            )
            .expect("first register");
        registry
            .register(
                "core.Expr",
                Grammar::new("start <- [b]").with_start_rule("start"),
            )
            .expect("second register");
        let grammar = registry
            .get("core.Expr", None)
            .expect("get registered grammar");
        assert_eq!(grammar.rules[0].source, "[b]");
    }
}
