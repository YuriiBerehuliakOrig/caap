use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{CaapError, CaapResult};
use crate::semantic::SemanticValue;

pub(super) fn require_registry_name(name: String) -> CaapResult<String> {
    validate_registry_name(&name)?;
    Ok(name)
}

pub(super) fn validate_registry_name(name: &str) -> CaapResult<()> {
    if name.is_empty() {
        return Err(CaapError::compiler(
            "compiler registry names must be non-empty strings",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FactSchemaTypeBridgeKind {
    ResolvedName,
    ResolvedBlock,
    CallSemantics,
    String,
    Map,
    Object,
}

impl FactSchemaTypeBridgeKind {
    pub fn from_bridge_name(name: &str) -> CaapResult<Self> {
        match name {
            "resolved_name" => Ok(Self::ResolvedName),
            "resolved_block" => Ok(Self::ResolvedBlock),
            "call_semantics" => Ok(Self::CallSemantics),
            "string" => Ok(Self::String),
            "map" => Ok(Self::Map),
            "object" => Ok(Self::Object),
            _ => Err(CaapError::compiler(format!(
                "unknown fact schema type bridge {name:?}; known bridges: \
                 call-semantics, map, object, resolved-block, resolved-name, string"
            ))),
        }
    }

    fn accepts(self, value: &SemanticValue) -> bool {
        match self {
            Self::String => matches!(value, SemanticValue::Str(_)),
            Self::Map | Self::ResolvedName | Self::ResolvedBlock | Self::CallSemantics => {
                matches!(value, SemanticValue::Map(_))
            }
            Self::Object => true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FactSchemaTypeBridge {
    pub label: String,
    pub bridge_name: String,
    pub kind: FactSchemaTypeBridgeKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FactSchemaEntry {
    pub predicate: String,
    pub type_label: String,
    pub bridge_name: String,
    pub allow_none: bool,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct FactSchemaRegistry {
    #[serde(default)]
    type_bridges: BTreeMap<String, FactSchemaTypeBridge>,
    #[serde(default)]
    schemas: BTreeMap<String, FactSchemaEntry>,
    #[serde(default)]
    version: u64,
}

impl FactSchemaTypeBridge {
    fn validate(&self) -> CaapResult<()> {
        validate_registry_name(&self.label)?;
        validate_registry_name(&self.bridge_name)?;
        let kind = FactSchemaTypeBridgeKind::from_bridge_name(&self.bridge_name)?;
        if kind != self.kind {
            return Err(CaapError::compiler(format!(
                "compiler fact schema bridge {:?} kind does not match bridge name {:?}",
                self.label, self.bridge_name
            )));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for FactSchemaTypeBridge {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct FactSchemaTypeBridgeData {
            label: String,
            bridge_name: String,
            kind: FactSchemaTypeBridgeKind,
        }

        let data = FactSchemaTypeBridgeData::deserialize(deserializer)?;
        let bridge = Self {
            label: data.label,
            bridge_name: data.bridge_name,
            kind: data.kind,
        };
        bridge
            .validate()
            .map_err(serde::de::Error::custom)
            .map(|()| bridge)
    }
}

impl FactSchemaEntry {
    fn validate_fields(&self) -> CaapResult<()> {
        validate_registry_name(&self.predicate)?;
        validate_registry_name(&self.type_label)?;
        validate_registry_name(&self.bridge_name)?;
        if self.description.as_deref().is_some_and(str::is_empty) {
            return Err(CaapError::compiler(format!(
                "compiler fact schema {:?} description must be non-empty",
                self.predicate
            )));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for FactSchemaEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct FactSchemaEntryData {
            predicate: String,
            type_label: String,
            bridge_name: String,
            allow_none: bool,
            #[serde(default)]
            description: Option<String>,
        }

        let data = FactSchemaEntryData::deserialize(deserializer)?;
        let entry = Self {
            predicate: data.predicate,
            type_label: data.type_label,
            bridge_name: data.bridge_name,
            allow_none: data.allow_none,
            description: data.description,
        };
        entry
            .validate_fields()
            .map_err(serde::de::Error::custom)
            .map(|()| entry)
    }
}

impl FactSchemaRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_type_bridge(
        &mut self,
        label: impl Into<String>,
        bridge_name: impl Into<String>,
    ) -> CaapResult<()> {
        let label = require_registry_name(label.into())?;
        let bridge_name = require_registry_name(bridge_name.into())?;
        let kind = FactSchemaTypeBridgeKind::from_bridge_name(&bridge_name)?;
        let entry = FactSchemaTypeBridge {
            label: label.clone(),
            bridge_name,
            kind,
        };
        if self.type_bridges.get(&label) != Some(&entry) {
            let version = self.next_version()?;
            self.type_bridges.insert(label, entry);
            self.version = version;
        }
        Ok(())
    }

    pub fn register_schema(
        &mut self,
        predicate: impl Into<String>,
        type_label: impl Into<String>,
        allow_none: bool,
        description: Option<String>,
    ) -> CaapResult<()> {
        let predicate = require_registry_name(predicate.into())?;
        let type_label = require_registry_name(type_label.into())?;
        let bridge = self.type_bridges.get(&type_label).ok_or_else(|| {
            CaapError::compiler(format!(
                "unknown compiler fact schema type {type_label:?}; register a type bridge \
                 with ctfe-compiler-fact-schema-type-bridge-register first"
            ))
        })?;
        if description.as_deref().is_some_and(str::is_empty) {
            return Err(CaapError::compiler(
                "compiler fact schema description must be non-empty",
            ));
        }
        let entry = FactSchemaEntry {
            predicate: predicate.clone(),
            type_label,
            bridge_name: bridge.bridge_name.clone(),
            allow_none,
            description,
        };
        if self.schemas.get(&predicate) != Some(&entry) {
            let version = self.next_version()?;
            self.schemas.insert(predicate, entry);
            self.version = version;
        }
        Ok(())
    }

    pub fn lookup(&self, predicate: &str) -> CaapResult<Option<&FactSchemaEntry>> {
        validate_registry_name(predicate)?;
        Ok(self.schemas.get(predicate))
    }

    pub fn validate_value(&self, predicate: &str, value: &SemanticValue) -> CaapResult<()> {
        let Some(schema) = self.lookup(predicate)? else {
            return Ok(());
        };
        if matches!(value, SemanticValue::Null) {
            if schema.allow_none {
                return Ok(());
            }
            return Err(CaapError::compiler(format!(
                "fact {predicate:?} does not allow null values"
            )));
        }
        let bridge = self.type_bridges.get(&schema.type_label).ok_or_else(|| {
            CaapError::compiler(format!(
                "compiler fact schema type {:?} has no bridge",
                schema.type_label
            ))
        })?;
        if bridge.kind.accepts(value) {
            Ok(())
        } else {
            Err(CaapError::compiler(format!(
                "fact {predicate:?} expects value compatible with schema type {:?}",
                schema.type_label
            )))
        }
    }

    pub fn validate(&self) -> CaapResult<()> {
        for (label, bridge) in &self.type_bridges {
            validate_registry_name(label)?;
            validate_registry_name(&bridge.label)?;
            validate_registry_name(&bridge.bridge_name)?;
            if label != &bridge.label {
                return Err(CaapError::compiler(format!(
                    "compiler fact schema bridge key {label:?} does not match entry label {:?}",
                    bridge.label
                )));
            }
            let kind = FactSchemaTypeBridgeKind::from_bridge_name(&bridge.bridge_name)?;
            if kind != bridge.kind {
                return Err(CaapError::compiler(format!(
                    "compiler fact schema bridge {:?} kind does not match bridge name {:?}",
                    bridge.label, bridge.bridge_name
                )));
            }
        }
        for (predicate, schema) in &self.schemas {
            validate_registry_name(predicate)?;
            validate_registry_name(&schema.predicate)?;
            validate_registry_name(&schema.type_label)?;
            validate_registry_name(&schema.bridge_name)?;
            if predicate != &schema.predicate {
                return Err(CaapError::compiler(format!(
                    "compiler fact schema key {predicate:?} does not match entry predicate {:?}",
                    schema.predicate
                )));
            }
            let bridge = self.type_bridges.get(&schema.type_label).ok_or_else(|| {
                CaapError::compiler(format!(
                    "compiler fact schema {:?} references unknown type {:?}",
                    schema.predicate, schema.type_label
                ))
            })?;
            if bridge.bridge_name != schema.bridge_name {
                return Err(CaapError::compiler(format!(
                    "compiler fact schema {:?} bridge name {:?} does not match type bridge {:?}",
                    schema.predicate, schema.bridge_name, bridge.bridge_name
                )));
            }
            if schema.description.as_deref().is_some_and(str::is_empty) {
                return Err(CaapError::compiler(format!(
                    "compiler fact schema {:?} description must be non-empty",
                    schema.predicate
                )));
            }
        }
        Ok(())
    }

    pub fn type_bridge(&self, label: &str) -> CaapResult<Option<&FactSchemaTypeBridge>> {
        validate_registry_name(label)?;
        Ok(self.type_bridges.get(label))
    }

    pub fn schemas(&self) -> Vec<&FactSchemaEntry> {
        self.schemas.values().collect()
    }

    pub fn predicates_by_bridge_kind(&self, kind: FactSchemaTypeBridgeKind) -> Vec<&str> {
        let matching_labels: std::collections::BTreeSet<&str> = self
            .type_bridges
            .values()
            .filter(|b| b.kind == kind)
            .map(|b| b.label.as_str())
            .collect();
        self.schemas
            .values()
            .filter(|e| matching_labels.contains(e.type_label.as_str()))
            .map(|e| e.predicate.as_str())
            .collect()
    }

    pub fn type_bridges(&self) -> Vec<&FactSchemaTypeBridge> {
        self.type_bridges.values().collect()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    fn next_version(&self) -> CaapResult<u64> {
        self.version
            .checked_add(1)
            .ok_or_else(|| CaapError::compiler("compiler fact schema version overflow"))
    }
}

impl<'de> Deserialize<'de> for FactSchemaRegistry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct FactSchemaRegistryData {
            #[serde(default)]
            type_bridges: BTreeMap<String, FactSchemaTypeBridge>,
            #[serde(default)]
            schemas: BTreeMap<String, FactSchemaEntry>,
            #[serde(default)]
            version: u64,
        }

        let data = FactSchemaRegistryData::deserialize(deserializer)?;
        let registry = Self {
            type_bridges: data.type_bridges,
            schemas: data.schemas,
            version: data.version,
        };
        registry
            .validate()
            .map_err(serde::de::Error::custom)
            .map(|()| registry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_type_bridge_rejects_version_overflow_without_mutating() {
        let mut registry = FactSchemaRegistry {
            version: u64::MAX,
            ..FactSchemaRegistry::new()
        };

        let error = registry
            .register_type_bridge("demo_string", "string")
            .unwrap_err()
            .to_string();

        assert!(error.contains("compiler fact schema version overflow"));
        assert!(registry.type_bridges.is_empty());
        assert_eq!(registry.version, u64::MAX);
    }

    #[test]
    fn register_schema_rejects_version_overflow_without_mutating() {
        let mut registry = FactSchemaRegistry::new();
        registry
            .register_type_bridge("demo_string", "string")
            .unwrap();
        registry.version = u64::MAX;

        let error = registry
            .register_schema("demo.fact", "demo_string", false, None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("compiler fact schema version overflow"));
        assert!(registry.schemas.is_empty());
        assert_eq!(registry.version, u64::MAX);
    }

    #[test]
    fn duplicate_registration_is_version_neutral_at_max_version() {
        let mut registry = FactSchemaRegistry::new();
        registry
            .register_type_bridge("demo_string", "string")
            .unwrap();
        registry
            .register_schema("demo.fact", "demo_string", false, None)
            .unwrap();
        registry.version = u64::MAX;

        registry
            .register_type_bridge("demo_string", "string")
            .unwrap();
        registry
            .register_schema("demo.fact", "demo_string", false, None)
            .unwrap();

        assert_eq!(registry.type_bridges.len(), 1);
        assert_eq!(registry.schemas.len(), 1);
        assert_eq!(registry.version, u64::MAX);
    }
}
