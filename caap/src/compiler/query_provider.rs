// Thin facade: re-exports everything from the two focused sub-modules so that
// other compiler siblings can continue to import via `use super::query_provider::X`.

// Public items (re-exported as pub so compiler/mod.rs can re-export them further)
pub use super::query_provider_types::{
    NativeProviderContext, ProviderCacheEntry, QueryExecutionOptions, QueryPlan, QueryPlanStep,
    QueryProvider, QueryProviderCacheScope, QueryProviderCallback, QueryProviderCallbackOutcome,
    QueryProviderContext, QueryProviderContractSpec, QueryProviderExecutionRecord,
    QueryProviderRegistrationSpec, QueryProviderResumePolicy, QueryProviderSchedule,
    QueryStageSpec, QueryTransactionMode,
};
pub use crate::semantic::EffectSet;

pub(crate) use super::query_provider_types::{
    annotation_tracking_predicate, ANNOTATION_PREDICATE_PREFIX,
};

// Compiler-internal items (pub(super) = visible to `compiler` module only)
pub(super) use super::query_provider_types::{
    enforce_query_effect_policy, extend_available_data, extend_unique, extend_unique_artifact_keys,
    merge_provider_context_tracking, normalize_cache_scope, normalize_resume_policy,
    normalize_stage_name, normalize_virtual_path, semantic_cell_tracking_key,
    semantic_subject_tracking_key, ProviderRollbackSnapshot, ProviderTransactionMode,
    QueryInnerRequest,
};

// Registry (public type)
pub use super::query_provider_registry::QueryProviderRegistry;

#[cfg(test)]
mod tests {
    use crate::semantic::{BuiltinEffectTag, EffectSet, EffectTag};

    #[test]
    fn effect_tags_are_typed_and_require_canonical_kebab_case() {
        let tag = EffectTag::new("read_ir").unwrap();
        assert_eq!(tag.as_str(), "read_ir");

        let set = EffectSet::from_unique_strings(
            ["write_ir".to_string(), "emit_diagnostics".to_string()],
            "query provider effect tag",
        )
        .unwrap();
        assert_eq!(
            set.to_strings(),
            vec!["emit_diagnostics".to_string(), "write_ir".to_string()]
        );
        assert!(set.contains_str("write_ir"));
        assert!(set.contains_builtin(BuiltinEffectTag::WriteIr));
        assert!(set.contains_builtin(BuiltinEffectTag::EmitDiagnostics));
        assert!(!set.contains_str("write-ir"));
    }

    #[test]
    fn effect_sets_reject_duplicates_and_invalid_tags() {
        let duplicate = EffectSet::from_unique_strings(
            ["read_ir".to_string(), "read_ir".to_string()],
            "query provider effect tag",
        )
        .unwrap_err()
        .to_string();
        assert!(duplicate.contains("duplicated"));
        assert!(duplicate.contains("read_ir"));

        let invalid = EffectTag::new("read files").unwrap_err().to_string();
        assert!(invalid.contains("unsupported characters"));

        let legacy_kebab = EffectTag::new("read-ir").unwrap_err().to_string();
        assert!(legacy_kebab.contains("unsupported characters"));

        let legacy_uppercase = EffectTag::new("READ_ir").unwrap_err().to_string();
        assert!(legacy_uppercase.contains("unsupported characters"));
    }

    #[test]
    fn normalize_unique_labels_preserves_exact_stage_labels() {
        use super::super::query_provider_types::normalize_unique_labels;

        let labels = normalize_unique_labels(
            ["parse_surface".to_string(), "resolve_names".to_string()],
            "compiler stage dependency",
        )
        .unwrap();
        assert_eq!(
            labels,
            vec!["parse_surface".to_string(), "resolve_names".to_string()]
        );

        let error = normalize_unique_labels(
            ["parse_surface".to_string(), "parse_surface".to_string()],
            "compiler stage dependency",
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("duplicated"));
        assert!(error.contains("parse_surface"));
    }

    #[test]
    fn normalize_data_keys_rejects_canonical_duplicates() {
        use super::super::query_provider_types::normalize_data_keys;
        let error = normalize_data_keys(["facts.module".to_string(), "facts:module".to_string()])
            .unwrap_err()
            .to_string();

        assert!(error.contains("duplicated"));
        assert!(error.contains("facts.module"));
    }

    #[test]
    fn provider_data_keys_accept_extension_domains_without_storage_registration() {
        use super::super::query_provider_types::normalize_data_key;

        assert_eq!(
            normalize_data_key("types.root").unwrap(),
            "types.root".to_string()
        );
        assert_eq!(
            normalize_data_key("analysis:call-graph").unwrap(),
            "analysis.call-graph".to_string()
        );

        let error = normalize_data_key("bad domain.value")
            .unwrap_err()
            .to_string();
        assert!(error.contains("provider data key domain"));
    }

    #[test]
    fn provider_storage_domains_reject_noncanonical_aliases() {
        use super::super::query_provider_types::{normalize_data_key, normalize_provider_domains};

        for domain in [
            "Fact",
            "annotations",
            "fact",
            "attribute",
            "diagnostic",
            "file",
            "fs",
            "host_service",
            "symbol",
            "type",
            "types",
        ] {
            let error =
                normalize_provider_domains([domain.to_string()], "query provider storage domain")
                    .unwrap_err()
                    .to_string();
            assert!(error.contains("unsupported provider domain"));
            assert!(error.contains("supported values"));
        }

        let data_key = normalize_data_key("host_service.module").unwrap();
        assert_eq!(data_key, "host_service.module");

        let read_error =
            normalize_provider_domains(["file".to_string()], "query provider read domain")
                .unwrap_err()
                .to_string();
        assert!(read_error.contains("query provider read domain"));
        assert!(read_error.contains("file"));
    }

    #[test]
    fn require_non_empty_labels_rejects_duplicate_provider_requirements() {
        use super::super::query_provider_types::require_non_empty_labels;
        let error = require_non_empty_labels(
            ["provider_a".to_string(), "provider_a".to_string()],
            "query provider requirement",
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("duplicated"));
        assert!(error.contains("provider_a"));
    }

    #[test]
    fn provider_cache_and_resume_policies_reject_legacy_normalized_aliases() {
        use super::super::query_provider_types::{
            normalize_cache_scope, normalize_resume_policy, QueryProviderCacheScope,
            QueryProviderResumePolicy,
        };

        let cache_error = normalize_cache_scope("Unit".to_string())
            .unwrap_err()
            .to_string();
        assert!(cache_error.contains("unsupported query provider cache_scope"));

        let resume_error = normalize_resume_policy("bootstrap-safe".to_string())
            .unwrap_err()
            .to_string();
        assert!(resume_error.contains("unsupported query provider resume_policy"));

        assert_eq!(
            normalize_cache_scope("unit".to_string()).unwrap(),
            QueryProviderCacheScope::Unit
        );
        assert_eq!(
            normalize_resume_policy("bootstrap_safe".to_string()).unwrap(),
            QueryProviderResumePolicy::BootstrapSafe
        );
    }
}
