use super::*;

use serde::{Deserialize, Serialize};

use crate::error::{CaapError, CaapResult};
use crate::ir::NodeId;

#[derive(Clone, Debug, PartialEq)]
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

impl Serialize for SemanticValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        enum SemanticValueData<'a> {
            Null,
            Bool(bool),
            Int(String),
            Float(f64),
            Str(&'a str),
            Node(NodeId),
            List(&'a [SemanticValue]),
            Map(&'a [(String, SemanticValue)]),
        }

        let value = match self {
            Self::Null => SemanticValueData::Null,
            Self::Bool(value) => SemanticValueData::Bool(*value),
            Self::Int(value) => SemanticValueData::Int(value.to_string()),
            Self::Float(value) => SemanticValueData::Float(*value),
            Self::Str(value) => SemanticValueData::Str(value),
            Self::Node(value) => SemanticValueData::Node(*value),
            Self::List(value) => SemanticValueData::List(value),
            Self::Map(value) => SemanticValueData::Map(value),
        };
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SemanticValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        enum SemanticValueData {
            Null,
            Bool(bool),
            Int(String),
            Float(f64),
            Str(String),
            Node(NodeId),
            List(Vec<SemanticValue>),
            Map(Vec<(String, SemanticValue)>),
        }

        let value = match SemanticValueData::deserialize(deserializer)? {
            SemanticValueData::Null => Self::Null,
            SemanticValueData::Bool(value) => Self::Bool(value),
            SemanticValueData::Int(value) => Self::Int(value.parse().map_err(|_| {
                serde::de::Error::custom("semantic int value must be a base-10 i64 string")
            })?),
            SemanticValueData::Float(value) => Self::Float(value),
            SemanticValueData::Str(value) => Self::Str(value),
            SemanticValueData::Node(value) => Self::Node(value),
            SemanticValueData::List(value) => Self::List(value),
            SemanticValueData::Map(value) => Self::Map(value),
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl SemanticValue {
    pub fn map(entries: impl IntoIterator<Item = (String, SemanticValue)>) -> CaapResult<Self> {
        let mut entries: Vec<(String, SemanticValue)> = entries.into_iter().collect();
        validate_semantic_map_entries(&mut entries)?;
        Ok(Self::Map(entries))
    }

    pub fn validate(&self) -> CaapResult<()> {
        match self {
            Self::List(values) => {
                for value in values {
                    value.validate()?;
                }
            }
            Self::Map(entries) => {
                let mut entries = entries.clone();
                validate_semantic_map_entries(&mut entries)?;
            }
            Self::Null
            | Self::Bool(_)
            | Self::Int(_)
            | Self::Float(_)
            | Self::Str(_)
            | Self::Node(_) => {}
        }
        Ok(())
    }
}

fn validate_semantic_map_entries(entries: &mut Vec<(String, SemanticValue)>) -> CaapResult<()> {
    if entries.iter().any(|(key, _)| key.is_empty()) {
        return Err(CaapError::semantic(
            "semantic value map keys must be non-empty",
        ));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    if entries.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(CaapError::semantic(
            "semantic value map keys must be unique",
        ));
    }
    for (_, value) in entries {
        value.validate()?;
    }
    Ok(())
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
    pub fold_policy: FoldPolicy,
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
            fold_policy: FoldPolicy::Never,
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
