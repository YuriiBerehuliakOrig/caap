//! Compiler value registry — stores named `RuntimeValue`s that survive session boundaries.
//! Also houses the validation helpers for provider dependency graphs and semantic entries.
use std::collections::{BTreeMap, BTreeSet};

use crate::error::{CaapError, CaapResult};
use crate::semantic::{EntrySource, SemanticEntry};
use crate::values::RuntimeValue;

use super::super::fact_schema::{require_registry_name, validate_registry_name};

/// Named runtime values registered by bootstrap code and available to every stage.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompilerRegistry {
    pub(super) values: BTreeMap<String, RuntimeValue>,
    pub(super) version: u64,
}

/// Point-in-time snapshot of the registry used for transactional rollback.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompilerRegistrySnapshot {
    pub(super) values: BTreeMap<String, RuntimeValue>,
    pub(super) version: u64,
}

impl CompilerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_value(
        &mut self,
        name: impl Into<String>,
        value: RuntimeValue,
    ) -> CaapResult<RuntimeValue> {
        let name = require_registry_name(name.into())?;
        let version = self.next_version()?;
        let entry = match self.values.entry(name) {
            std::collections::btree_map::Entry::Occupied(e) => {
                return Err(CaapError::compiler(format!(
                    "compiler registry already contains {:?}",
                    e.key()
                )));
            }
            std::collections::btree_map::Entry::Vacant(e) => e,
        };
        let v = entry.insert(value).clone();
        self.version = version;
        Ok(v)
    }

    pub fn lookup_value(&self, name: &str) -> CaapResult<Option<&RuntimeValue>> {
        validate_registry_name(name)?;
        Ok(self.values.get(name))
    }

    pub fn require_value(&self, name: &str) -> CaapResult<&RuntimeValue> {
        self.lookup_value(name)?.ok_or_else(|| {
            CaapError::compiler(format!("compiler registry does not contain {name:?}"))
        })
    }

    pub fn registered_names(&self) -> Vec<&str> {
        let mut names: Vec<_> = self.values.keys().map(String::as_str).collect();
        names.sort();
        names
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    fn next_version(&self) -> CaapResult<u64> {
        self.version
            .checked_add(1)
            .ok_or_else(|| CaapError::compiler("compiler registry version overflow"))
    }

    pub fn snapshot(&self) -> CompilerRegistrySnapshot {
        CompilerRegistrySnapshot {
            values: self.values.clone(),
            version: self.version,
        }
    }

    pub fn restore_snapshot(&mut self, snapshot: CompilerRegistrySnapshot) -> CaapResult<()> {
        snapshot.validate()?;
        self.values = snapshot.values;
        self.version = snapshot.version;
        Ok(())
    }
}

impl CompilerRegistrySnapshot {
    pub fn validate(&self) -> CaapResult<()> {
        for name in self.values.keys() {
            validate_registry_name(name)?;
        }
        Ok(())
    }
}

/// DFS cycle detection for dynamic provider require declarations.
pub fn validate_dynamic_provider_dependency(
    dynamic_requires: &BTreeMap<String, Vec<String>>,
    provider_name: &str,
    required_provider: &str,
) -> CaapResult<()> {
    if provider_name == required_provider {
        return Err(CaapError::compiler(format!(
            "dynamic provider dependency cycle: {provider_name} -> {required_provider}"
        )));
    }
    // Parent-pointer DFS: O(n) space vs. the previous O(n * depth) path-per-frame approach.
    // On the error path we reconstruct the cycle once by tracing back through `parent`.
    let mut visited = BTreeSet::new();
    let mut parent: BTreeMap<String, String> = BTreeMap::new();
    let mut stack = vec![required_provider.to_string()];
    parent.insert(required_provider.to_string(), provider_name.to_string());
    while let Some(current) = stack.pop() {
        if !visited.insert(current.clone()) {
            continue;
        }
        if current == provider_name {
            let mut path = vec![current.clone()];
            let mut node = current.as_str();
            while let Some(pred) = parent.get(node) {
                path.push(pred.clone());
                if pred == provider_name {
                    break;
                }
                node = pred.as_str();
            }
            path.reverse();
            return Err(CaapError::compiler(format!(
                "dynamic provider dependency cycle: {}",
                path.join(" -> ")
            )));
        }
        for next in dynamic_requires.get(&current).into_iter().flatten() {
            if !visited.contains(next) {
                parent
                    .entry(next.clone())
                    .or_insert_with(|| current.clone());
                stack.push(next.clone());
            }
        }
    }
    Ok(())
}

/// Validate that a `SemanticEntry` is safe to register as a base (cross-unit) entry.
pub fn validate_base_semantic_entry(entry: &SemanticEntry) -> CaapResult<()> {
    validate_registry_name(&entry.name)?;
    if entry.node_id.is_some() {
        return Err(CaapError::compiler(
            "base semantic entries must not carry unit-local node ids",
        ));
    }
    if entry.unit_id.is_some() {
        return Err(CaapError::compiler(
            "base semantic entries must not carry unit ids",
        ));
    }
    if !matches!(
        entry.source,
        EntrySource::Builtin | EntrySource::Registered | EntrySource::External
    ) {
        return Err(CaapError::compiler(
            "base semantic entry source must be builtin, registered, or external",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_value_rejects_version_overflow_without_mutating() {
        let mut registry = CompilerRegistry {
            version: u64::MAX,
            ..CompilerRegistry::new()
        };

        let error = registry
            .register_value("overflow.value", RuntimeValue::Null)
            .unwrap_err()
            .to_string();

        assert!(error.contains("compiler registry version overflow"));
        assert!(!registry.values.contains_key("overflow.value"));
        assert_eq!(registry.version, u64::MAX);
    }
}
