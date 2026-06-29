//! Generic semantic state substrate for CAAP.
//!
//! This module intentionally stores symbols, semantic entries, stable ids, and
//! versioned facts without assigning language-specific import/export semantics.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::error::{CaapError, CaapResult};
use crate::ir::NodeId;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
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

impl<'de> Deserialize<'de> for StableId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// A semantic subject identifier.
///
/// `kind` uses `Cow<'static, str>` so that the common static kinds (`"node"`,
/// `"symbol"`, `"unit"`, etc.) are stored as a borrowed pointer with zero
/// allocation.  Dynamic kinds (created via `parse` or `subject_id`) use the
/// `Owned` variant and allocate exactly once.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct SemanticSubjectId {
    pub kind: Cow<'static, str>,
    pub value: String,
}

impl SemanticSubjectId {
    pub fn new(kind: impl Into<Cow<'static, str>>, value: impl Into<String>) -> CaapResult<Self> {
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
        Self::new(kind.to_string(), rest)
    }
}

impl<'de> Deserialize<'de> for SemanticSubjectId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SemanticSubjectIdData {
            kind: String,
            value: String,
        }

        let data = SemanticSubjectIdData::deserialize(deserializer)?;
        // data.kind is String; String: Into<Cow<'static, str>> via Cow::Owned
        Self::new(data.kind, data.value).map_err(serde::de::Error::custom)
    }
}

pub fn subject_id(
    kind: impl Into<Cow<'static, str>>,
    value: impl Into<String>,
) -> CaapResult<SemanticSubjectId> {
    SemanticSubjectId::new(kind, value.into())
}

pub fn node_subject_id(node_id: NodeId) -> SemanticSubjectId {
    SemanticSubjectId {
        kind: Cow::Borrowed("node"),
        value: node_id.to_string(),
    }
}

pub fn symbol_subject_id(name: impl Into<String>) -> CaapResult<SemanticSubjectId> {
    SemanticSubjectId::new("symbol", name.into())
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

mod graph;
mod policy;
mod symbol;
mod value;

pub use graph::*;
pub use policy::*;
pub use symbol::*;
pub use value::*;
