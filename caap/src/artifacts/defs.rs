//! Core artifact data types: keys, fingerprints, values, and source artifacts.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{CaapError, CaapResult};
use crate::semantic::PhasePolicy;
use crate::unit::UnitTemplate;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct ArtifactKey(Vec<String>);

impl ArtifactKey {
    pub fn new(parts: impl IntoIterator<Item = String>) -> CaapResult<Self> {
        let parts: Vec<String> = parts.into_iter().collect();
        if parts.is_empty() {
            return Err(CaapError::artifacts(
                "artifact key must contain at least one part",
            ));
        }
        if parts.iter().any(String::is_empty) {
            return Err(CaapError::artifacts("artifact key parts must be non-empty"));
        }
        Ok(Self(parts))
    }

    pub fn single(part: impl Into<String>) -> CaapResult<Self> {
        Self::new([part.into()])
    }

    pub fn pair(left: impl Into<String>, right: impl Into<String>) -> CaapResult<Self> {
        Self::new([left.into(), right.into()])
    }

    pub fn parts(&self) -> &[String] {
        &self.0
    }

    pub fn part(&self, index: usize) -> Option<&str> {
        self.0.get(index).map(String::as_str)
    }

    pub fn kind(&self) -> &str {
        &self.0[0]
    }
}

impl<'de> Deserialize<'de> for ArtifactKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let parts = Vec::<String>::deserialize(deserializer)?;
        Self::new(parts).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for ArtifactKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.join(":"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
pub struct ArtifactFingerprint(String);

impl ArtifactFingerprint {
    pub fn sha256(bytes: impl AsRef<[u8]>) -> Self {
        let mut digest = Sha256::new();
        digest.update(bytes.as_ref());
        Self(format!("{:x}", digest.finalize()))
    }

    pub fn new(value: impl Into<String>) -> CaapResult<Self> {
        let value = value.into();
        if value.is_empty() {
            return Err(CaapError::artifacts(
                "artifact fingerprint must be non-empty",
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ArtifactFingerprint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for ArtifactFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum SourceOrigin {
    Inline { label: String },
    Path { path: String, source_token: String },
}

impl SourceOrigin {
    pub fn inline(label: impl Into<String>) -> CaapResult<Self> {
        let label = label.into();
        if label.is_empty() {
            return Err(CaapError::artifacts(
                "inline source label must be non-empty",
            ));
        }
        Ok(Self::Inline { label })
    }

    pub fn path(path: impl Into<String>, source_token: impl Into<String>) -> CaapResult<Self> {
        let path = path.into();
        let source_token = source_token.into();
        if path.is_empty() {
            return Err(CaapError::artifacts(
                "source artifact path must be non-empty",
            ));
        }
        if source_token.is_empty() {
            return Err(CaapError::artifacts(
                "source artifact path token must be non-empty",
            ));
        }
        Ok(Self::Path { path, source_token })
    }
}

impl<'de> Deserialize<'de> for SourceOrigin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        enum SourceOriginData {
            Inline { label: String },
            Path { path: String, source_token: String },
        }

        match SourceOriginData::deserialize(deserializer)? {
            SourceOriginData::Inline { label } => {
                Self::inline(label).map_err(serde::de::Error::custom)
            }
            SourceOriginData::Path { path, source_token } => {
                Self::path(path, source_token).map_err(serde::de::Error::custom)
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceArtifact {
    pub origin: SourceOrigin,
    pub text: String,
    pub fingerprint: ArtifactFingerprint,
}

impl SourceArtifact {
    pub fn inline(text: impl Into<String>) -> CaapResult<Self> {
        Self::inline_with_label(text, "<inline.caap>")
    }

    pub fn inline_with_label(
        text: impl Into<String>,
        label: impl Into<String>,
    ) -> CaapResult<Self> {
        let text = text.into();
        Ok(Self {
            fingerprint: ArtifactFingerprint::sha256(text.as_bytes()),
            origin: SourceOrigin::inline(label)?,
            text,
        })
    }

    pub fn path(
        path: impl Into<String>,
        source_token: impl Into<String>,
        text: impl Into<String>,
    ) -> CaapResult<Self> {
        let text = text.into();
        Ok(Self {
            fingerprint: ArtifactFingerprint::sha256(text.as_bytes()),
            origin: SourceOrigin::path(path, source_token)?,
            text,
        })
    }

    pub fn parse_surface_key(
        &self,
        stage: impl Into<String>,
        phase: PhasePolicy,
    ) -> CaapResult<ArtifactKey> {
        let stage = stage.into();
        match &self.origin {
            SourceOrigin::Inline { .. } => {
                parse_surface_inline_key(stage, phase, &self.fingerprint)
            }
            SourceOrigin::Path { path, source_token } => {
                parse_surface_path_key(stage, phase, path, source_token)
            }
        }
    }

    pub fn parse_surface_lineage_id(&self, phase: PhasePolicy) -> CaapResult<ArtifactKey> {
        match &self.origin {
            SourceOrigin::Inline { .. } => ArtifactKey::new([
                "parse_surface_inline".to_string(),
                phase.as_str().to_string(),
                self.fingerprint.as_str().to_string(),
            ]),
            SourceOrigin::Path { path, .. } => ArtifactKey::new([
                "parse_surface_source".to_string(),
                phase.as_str().to_string(),
                path.clone(),
            ]),
        }
    }
}

impl<'de> Deserialize<'de> for SourceArtifact {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SourceArtifactData {
            origin: SourceOrigin,
            text: String,
            fingerprint: ArtifactFingerprint,
        }

        let data = SourceArtifactData::deserialize(deserializer)?;
        let expected = ArtifactFingerprint::sha256(data.text.as_bytes());
        if data.fingerprint != expected {
            return Err(serde::de::Error::custom(
                "source artifact fingerprint must match text",
            ));
        }
        Ok(Self {
            origin: data.origin,
            text: data.text,
            fingerprint: data.fingerprint,
        })
    }
}

pub fn parse_surface_inline_key(
    stage: impl Into<String>,
    phase: PhasePolicy,
    fingerprint: &ArtifactFingerprint,
) -> CaapResult<ArtifactKey> {
    ArtifactKey::new([
        "parse_surface_inline".to_string(),
        stage.into(),
        phase.as_str().to_string(),
        fingerprint.as_str().to_string(),
    ])
}

pub fn parse_surface_path_key(
    stage: impl Into<String>,
    phase: PhasePolicy,
    path: impl Into<String>,
    source_token: impl Into<String>,
) -> CaapResult<ArtifactKey> {
    ArtifactKey::new([
        "parse_surface".to_string(),
        stage.into(),
        phase.as_str().to_string(),
        path.into(),
        source_token.into(),
    ])
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceTemplateArtifactValue {
    pub source: SourceArtifact,
    pub template: UnitTemplate,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryStageArtifactValue {
    pub summary: crate::semantic::SemanticValue,
    pub unit_template: UnitTemplate,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ArtifactValue {
    Text(String),
    Bytes(Vec<u8>),
    Source(SourceArtifact),
    SourceTemplate(Box<SourceTemplateArtifactValue>),
    QueryStage(Box<QueryStageArtifactValue>),
    Semantic(crate::semantic::SemanticValue),
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub generation: u64,
}

impl ArtifactCacheStats {
    pub fn record_hit(&mut self) {
        self.hits = self.hits.saturating_add(1);
    }

    pub fn record_miss(&mut self) {
        self.misses = self.misses.saturating_add(1);
    }
}

pub fn changed_inputs_for_lineage(
    lineage_id: &ArtifactKey,
    previous_key: &ArtifactKey,
    replacement_key: &ArtifactKey,
) -> Vec<String> {
    let labels: &[(&str, &[usize])] = match lineage_id.kind() {
        "parse_surface_source" => &[
            ("stage", &[1]),
            ("phase", &[2]),
            ("source_path", &[3]),
            ("source_token", &[4]),
        ],
        "parse_surface_inline" => &[("stage", &[1]), ("phase", &[2]), ("source_digest", &[3])],
        "unit_input" => &[
            ("stage", &[1]),
            ("unit_fingerprint", &[2]),
            ("phase", &[3]),
            ("names_version", &[4]),
        ],
        "stage_unit" => &[
            ("stage", &[1]),
            ("phase", &[2]),
            ("dependency_key", &[3]),
            ("builtin_version", &[4]),
            ("names_version", &[5]),
            ("session_version", &[6]),
            ("initial_bindings", &[7]),
        ],
        _ if previous_key.parts().len() == 7 && replacement_key.parts().len() == 7 => &[
            ("stage", &[0]),
            ("phase", &[1]),
            ("dependency_key", &[2]),
            ("builtin_version", &[3]),
            ("names_version", &[4]),
            ("session_version", &[5]),
            ("initial_bindings", &[6]),
        ],
        _ => &[],
    };

    let mut changed = Vec::new();
    for (label, positions) in labels {
        if positions
            .iter()
            .any(|position| previous_key.part(*position) != replacement_key.part(*position))
        {
            changed.push((*label).to_string());
        }
    }
    changed
}
