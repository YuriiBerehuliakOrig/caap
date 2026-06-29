//! Rewrite provenance types: RewriteRecord and RewriteTombstone.

use serde::{Deserialize, Serialize};

use crate::error::{CaapError, CaapResult};
use crate::ir::NodeId;
use crate::semantic::SemanticValue;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RewriteRecord {
    pub provider_name: String,
    pub stage: String,
    pub family_label: Option<String>,
    pub operation: String,
    pub sources: Vec<NodeId>,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RewriteTombstone {
    pub stable_id: String,
    pub latest: RewriteRecord,
    pub chain: Vec<RewriteRecord>,
}

impl RewriteRecord {
    pub(super) fn validate(&self) -> CaapResult<()> {
        if self.provider_name.is_empty() {
            return Err(CaapError::unit(
                "rewrite record provider name must be non-empty",
            ));
        }
        if self.stage.is_empty() {
            return Err(CaapError::unit("rewrite record stage must be non-empty"));
        }
        if self.family_label.as_ref().is_some_and(String::is_empty) {
            return Err(CaapError::unit(
                "rewrite record family label must be non-empty when present",
            ));
        }
        if self.operation.is_empty() {
            return Err(CaapError::unit(
                "rewrite record operation must be non-empty",
            ));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for RewriteRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RewriteRecordData {
            provider_name: String,
            stage: String,
            family_label: Option<String>,
            operation: String,
            sources: Vec<NodeId>,
            generation: u64,
        }

        let data = RewriteRecordData::deserialize(deserializer)?;
        let record = Self {
            provider_name: data.provider_name,
            stage: data.stage,
            family_label: data.family_label,
            operation: data.operation,
            sources: data.sources,
            generation: data.generation,
        };
        record
            .validate()
            .map_err(serde::de::Error::custom)
            .map(|()| record)
    }
}

impl RewriteTombstone {
    pub(super) fn validate(&self) -> CaapResult<()> {
        if self.stable_id.is_empty() {
            return Err(CaapError::unit(
                "rewrite tombstone stable id must be non-empty",
            ));
        }
        self.latest.validate()?;
        if self.chain.is_empty() {
            return Err(CaapError::unit("rewrite tombstone chain must be non-empty"));
        }
        for record in &self.chain {
            record.validate()?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for RewriteTombstone {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RewriteTombstoneData {
            stable_id: String,
            latest: RewriteRecord,
            chain: Vec<RewriteRecord>,
        }

        let data = RewriteTombstoneData::deserialize(deserializer)?;
        let tombstone = Self {
            stable_id: data.stable_id,
            latest: data.latest,
            chain: data.chain,
        };
        tombstone
            .validate()
            .map_err(serde::de::Error::custom)
            .map(|()| tombstone)
    }
}

// ---------------------------------------------------------------------------
// Semantic value helpers (used by Unit impl in core.rs)
// ---------------------------------------------------------------------------

pub(super) fn rewrite_record_to_semantic_value(
    record: &RewriteRecord,
) -> CaapResult<SemanticValue> {
    let mut entries = vec![
        (
            "provider_name".to_string(),
            SemanticValue::Str(record.provider_name.clone()),
        ),
        (
            "stage".to_string(),
            SemanticValue::Str(record.stage.clone()),
        ),
        (
            "operation".to_string(),
            SemanticValue::Str(record.operation.clone()),
        ),
        (
            "sources".to_string(),
            SemanticValue::List(
                record
                    .sources
                    .iter()
                    .map(|source| SemanticValue::Int(*source as i64))
                    .collect(),
            ),
        ),
        (
            "generation".to_string(),
            SemanticValue::Int(i64::try_from(record.generation).map_err(|_| {
                CaapError::unit("rewrite provenance generation exceeds semantic integer range")
            })?),
        ),
    ];
    if let Some(family_label) = &record.family_label {
        entries.push((
            "family_label".to_string(),
            SemanticValue::Str(family_label.clone()),
        ));
        entries.push((
            "family".to_string(),
            SemanticValue::Str(family_label.clone()),
        ));
    }
    SemanticValue::map(entries)
}

pub(super) fn rewrite_record_from_semantic_value(
    value: &SemanticValue,
) -> CaapResult<RewriteRecord> {
    let SemanticValue::Map(entries) = value else {
        return Err(CaapError::unit("rewrite provenance fact must be a map"));
    };
    let provider_name = required_semantic_str(entries, "provider_name")?.to_string();
    let stage = required_semantic_str(entries, "stage")?.to_string();
    let family_label = optional_semantic_str(entries, "family_label")
        .or_else(|| optional_semantic_str(entries, "family"))
        .map(str::to_string);
    let operation = required_semantic_str(entries, "operation")?.to_string();
    let sources = required_semantic_node_list(entries, "sources")?;
    let generation = match semantic_map_get(entries, "generation") {
        Some(SemanticValue::Int(value)) if *value >= 0 => u64::try_from(*value)
            .map_err(|_| CaapError::unit("rewrite provenance fact generation is too large"))?,
        _ => {
            return Err(CaapError::unit(
                "rewrite provenance fact requires non-negative generation",
            ));
        }
    };
    Ok(RewriteRecord {
        provider_name,
        stage,
        family_label,
        operation,
        sources,
        generation,
    })
}

fn semantic_map_get<'a>(
    entries: &'a [(String, SemanticValue)],
    key: &str,
) -> Option<&'a SemanticValue> {
    entries
        .iter()
        .find_map(|(entry_key, value)| (entry_key == key).then_some(value))
}

fn required_semantic_str<'a>(
    entries: &'a [(String, SemanticValue)],
    key: &str,
) -> CaapResult<&'a str> {
    match semantic_map_get(entries, key) {
        Some(SemanticValue::Str(value)) if !value.is_empty() => Ok(value),
        _ => Err(CaapError::unit(format!(
            "rewrite provenance fact requires non-empty {key}"
        ))),
    }
}

fn optional_semantic_str<'a>(entries: &'a [(String, SemanticValue)], key: &str) -> Option<&'a str> {
    match semantic_map_get(entries, key) {
        Some(SemanticValue::Str(value)) if !value.is_empty() => Some(value),
        _ => None,
    }
}

fn required_semantic_node_list(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> CaapResult<Vec<NodeId>> {
    let Some(SemanticValue::List(items)) = semantic_map_get(entries, key) else {
        return Err(CaapError::unit(format!(
            "rewrite provenance fact requires {key} list"
        )));
    };
    items
        .iter()
        .map(|item| match item {
            SemanticValue::Int(value) if *value >= 0 && *value <= NodeId::MAX as i64 => {
                Ok(*value as NodeId)
            }
            _ => Err(CaapError::unit(format!(
                "rewrite provenance fact {key} must contain node ids"
            ))),
        })
        .collect()
}
