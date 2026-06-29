//! SourceTemplateArtifact and SourceTemplateCache — parse-surface caching layer.

use std::collections::BTreeMap;

use crate::error::{CaapError, CaapResult};
use crate::semantic::PhasePolicy;
use crate::unit::UnitTemplate;

use super::cache::ArtifactCache;
use super::defs::{ArtifactKey, ArtifactValue, SourceArtifact, SourceTemplateArtifactValue};

#[derive(Clone, Debug)]
pub struct SourceTemplateArtifact {
    pub source: SourceArtifact,
    pub key: ArtifactKey,
    pub lineage_id: ArtifactKey,
    pub template: UnitTemplate,
    pub cache_hit: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SourceTemplateCache {
    cache: ArtifactCache,
    templates: BTreeMap<ArtifactKey, UnitTemplate>,
}

impl SourceTemplateCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn artifact_cache(&self) -> &ArtifactCache {
        &self.cache
    }

    pub fn artifact_cache_mut(&mut self) -> &mut ArtifactCache {
        &mut self.cache
    }

    pub fn load(
        &mut self,
        source: SourceArtifact,
        stage: impl Into<String>,
        phase: PhasePolicy,
        mut materialize: impl FnMut(&SourceArtifact) -> CaapResult<UnitTemplate>,
    ) -> CaapResult<SourceTemplateArtifact> {
        let stage = stage.into();
        if stage.is_empty() {
            return Err(CaapError::artifacts(
                "source template cache stage must be non-empty",
            ));
        }
        let key = source.parse_surface_key(stage, phase)?;
        let lineage_id = source.parse_surface_lineage_id(phase)?;

        if !self.cache.is_dirty(&key)
            && self.cache.contains(&key)
            && self.templates.contains_key(&key)
        {
            if let Some(template) = self.templates.get(&key).cloned() {
                self.cache.record_cache_hit();
                return Ok(SourceTemplateArtifact {
                    source,
                    key,
                    lineage_id,
                    template,
                    cache_hit: true,
                });
            }
        }
        if !self.cache.is_dirty(&key) {
            if let Some(ArtifactValue::SourceTemplate(cached)) = self.cache.peek(&key).cloned() {
                let cached = *cached;
                if cached.source == source {
                    self.templates.insert(key.clone(), cached.template.clone());
                    self.cache.record_cache_hit();
                    return Ok(SourceTemplateArtifact {
                        source,
                        key,
                        lineage_id,
                        template: cached.template,
                        cache_hit: true,
                    });
                }
            }
        }

        self.cache.record_cache_miss();
        let template = materialize(&source)?;
        self.cache.store_with_lineage(
            key.clone(),
            ArtifactValue::SourceTemplate(Box::new(SourceTemplateArtifactValue {
                source: source.clone(),
                template: template.clone(),
            })),
            [],
            lineage_id.clone(),
        )?;
        self.templates.insert(key.clone(), template.clone());

        Ok(SourceTemplateArtifact {
            source,
            key,
            lineage_id,
            template,
            cache_hit: false,
        })
    }
}
