//! Generic artifact cache and invalidation substrate for CAAP.
//!
//! The cache is intentionally compiler-agnostic: it knows about artifact keys,
//! values, dependencies, dirty state, and snapshots, but not about module,
//! provider, or bootstrap policy.

pub mod cache;
pub mod defs;
pub mod source_template;

pub use cache::{
    ArtifactCache, ArtifactCacheFile, ArtifactCacheSnapshot, ArtifactInvalidationRecord,
    ReusableArtifactCacheSnapshot,
};
pub use defs::{
    changed_inputs_for_lineage, parse_surface_inline_key, parse_surface_path_key,
    ArtifactCacheStats, ArtifactFingerprint, ArtifactKey, ArtifactValue, QueryStageArtifactValue,
    SourceArtifact, SourceOrigin, SourceTemplateArtifactValue,
};
pub use source_template::{SourceTemplateArtifact, SourceTemplateCache};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_key_rejects_empty_parts_and_displays_path() {
        assert!(ArtifactKey::new(Vec::<String>::new()).is_err());
        assert!(ArtifactKey::new(["source".to_string(), "".to_string()]).is_err());
        let key = ArtifactKey::new(["source".to_string(), "demo".to_string()]).unwrap();
        assert_eq!(key.kind(), "source");
        assert_eq!(key.to_string(), "source:demo");
    }

    #[test]
    fn artifact_key_deserialize_rejects_invalid_parts() {
        let err = serde_json::from_str::<ArtifactKey>(r#"["source",""]"#).unwrap_err();
        assert!(err.to_string().contains("non-empty"));
    }

    #[test]
    fn artifact_fingerprint_deserialize_rejects_empty_value() {
        let err = serde_json::from_str::<ArtifactFingerprint>(r#""""#).unwrap_err();
        assert!(err.to_string().contains("non-empty"));
    }

    #[test]
    fn source_origin_deserialize_rejects_empty_path() {
        let err =
            serde_json::from_str::<SourceOrigin>(r#"{"Path":{"path":"","source_token":"token"}}"#)
                .unwrap_err();
        assert!(err.to_string().contains("path must be non-empty"));
    }

    #[test]
    fn source_artifact_deserialize_rejects_mismatched_fingerprint() {
        let err = serde_json::from_str::<SourceArtifact>(
            r#"{"origin":{"Inline":{"label":"demo"}},"text":"actual","fingerprint":"sha256:wrong"}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("fingerprint must match text"));
    }

    #[test]
    fn artifact_cache_marks_dependents_dirty() {
        let input = ArtifactKey::single("input").unwrap();
        let output = ArtifactKey::single("output").unwrap();
        let mut cache = ArtifactCache::new();
        cache
            .store(input.clone(), ArtifactValue::Text("in".into()), [])
            .unwrap();
        cache
            .store(
                output.clone(),
                ArtifactValue::Text("out".into()),
                [input.clone()],
            )
            .unwrap();

        cache
            .mark_dirty(ArtifactInvalidationRecord::new("source_changed", input.clone()).unwrap())
            .unwrap();
        assert!(cache.is_dirty(&input));
        assert!(cache.is_dirty(&output));
        assert!(cache.peek(&output).is_none());
    }

    #[test]
    fn artifact_cache_batch_invalidation_is_validated_before_mutation() {
        let first = ArtifactKey::single("first").unwrap();
        let second = ArtifactKey::single("second").unwrap();
        let mut cache = ArtifactCache::new();
        cache
            .store(first.clone(), ArtifactValue::Text("first".into()), [])
            .unwrap();
        cache
            .store(second.clone(), ArtifactValue::Text("second".into()), [])
            .unwrap();
        let valid = ArtifactInvalidationRecord::new("source_changed", first.clone()).unwrap();
        let mut invalid =
            ArtifactInvalidationRecord::new("source_changed", second.clone()).unwrap();
        invalid.changed_inputs = vec!["".to_string()];

        let error = cache
            .mark_dirty_batch([valid, invalid])
            .unwrap_err()
            .to_string();

        assert!(error.contains("changed inputs must be non-empty"));
        assert!(!cache.is_dirty(&first));
        assert!(!cache.is_dirty(&second));
        assert!(cache.latest_invalidation_for_key(&first).is_none());
        assert!(cache.latest_invalidation_for_key(&second).is_none());
    }
}
