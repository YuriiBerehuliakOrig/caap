/// Integration tests for compiler event, artifact-cache, and source-template cache behavior.
use caap_core::{frontend::parse, Unit};

mod common;

#[test]
fn test_compiler_event_log_filters_by_kind() {
    let mut compiler = caap_core::CompilerHost::new().new_session();
    compiler
        .emit_event(
            caap_core::CompilerEvent::with_target(
                "query.plan",
                Some("compile_unit".to_string()),
                "planned compile query",
                [("stage".to_string(), "compile_unit".to_string())],
            )
            .unwrap(),
        )
        .unwrap();
    compiler
        .emit_event(caap_core::CompilerEvent::new("bootstrap.raw", "executed bootstrap").unwrap())
        .unwrap();

    assert_eq!(compiler.events().events().len(), 2);
    let query_events = compiler.events().by_kind("query.plan").unwrap();
    assert_eq!(query_events.len(), 1);
    assert_eq!(query_events[0].target.as_deref(), Some("compile_unit"));
    assert_eq!(
        query_events[0].metadata,
        vec![("stage".to_string(), "compile_unit".to_string())]
    );
}

#[test]
fn test_artifact_cache_stores_values_and_tracks_hits() {
    let mut cache = caap_core::ArtifactCache::new();
    let key = caap_core::ArtifactKey::pair("source", "main").unwrap();

    cache
        .store(
            key.clone(),
            caap_core::ArtifactValue::Text("(int_add 1 2)".to_string()),
            [],
        )
        .unwrap();

    assert_eq!(
        cache.get(&key),
        Some(&caap_core::ArtifactValue::Text("(int_add 1 2)".to_string()))
    );
    assert_eq!(cache.stats().hits, 1);
    assert_eq!(cache.stats().misses, 0);

    let missing = caap_core::ArtifactKey::pair("source", "missing").unwrap();
    assert_eq!(cache.get(&missing), None);
    assert_eq!(cache.stats().misses, 1);
}

#[test]
fn test_artifact_cache_hit_miss_stats_saturate() {
    let mut cache = caap_core::ArtifactCache::new();
    let mut snapshot = cache.snapshot();
    snapshot.stats.hits = u64::MAX;
    snapshot.stats.misses = u64::MAX;
    cache.restore_snapshot(snapshot).unwrap();
    let key = caap_core::ArtifactKey::pair("source", "main").unwrap();
    cache
        .store(
            key.clone(),
            caap_core::ArtifactValue::Text("source".to_string()),
            [],
        )
        .unwrap();

    assert!(cache.get(&key).is_some());
    assert_eq!(
        cache.get(&caap_core::ArtifactKey::pair("source", "missing").unwrap()),
        None
    );

    assert_eq!(cache.stats().hits, u64::MAX);
    assert_eq!(cache.stats().misses, u64::MAX);
}

#[test]
fn test_artifact_cache_store_rejects_generation_overflow_without_mutating() {
    let mut cache = caap_core::ArtifactCache::new();
    let mut snapshot = cache.snapshot();
    snapshot.stats.generation = u64::MAX;
    cache.restore_snapshot(snapshot).unwrap();
    let key = caap_core::ArtifactKey::pair("source", "overflow").unwrap();

    let error = cache
        .store(
            key.clone(),
            caap_core::ArtifactValue::Text("source".to_string()),
            [],
        )
        .unwrap_err()
        .to_string();

    assert!(error.contains("artifact cache generation overflow"));
    assert!(!cache.contains(&key));
    assert_eq!(cache.stats().generation, u64::MAX);
}

#[test]
fn test_artifact_cache_batch_invalidation_rejects_generation_overflow_without_mutating() {
    let mut cache = caap_core::ArtifactCache::new();
    let mut snapshot = cache.snapshot();
    snapshot.stats.generation = u64::MAX - 1;
    cache.restore_snapshot(snapshot).unwrap();
    let first = caap_core::ArtifactKey::pair("source", "first").unwrap();
    let second = caap_core::ArtifactKey::pair("source", "second").unwrap();

    let error = cache
        .mark_dirty_batch([
            caap_core::ArtifactInvalidationRecord::new("source_change", first.clone()).unwrap(),
            caap_core::ArtifactInvalidationRecord::new("source_change", second.clone()).unwrap(),
        ])
        .unwrap_err()
        .to_string();

    assert!(error.contains("artifact cache generation overflow"));
    assert!(cache.dirty_record(&first).is_none());
    assert!(cache.dirty_record(&second).is_none());
    assert_eq!(cache.stats().generation, u64::MAX - 1);
}

#[test]
fn test_artifact_cache_lineage_replacement_preflights_generation_overflow() {
    let mut cache = caap_core::ArtifactCache::new();
    let lineage = caap_core::ArtifactKey::single("parse_lineage").unwrap();
    let old_key = caap_core::ArtifactKey::pair("source", "old").unwrap();
    let new_key = caap_core::ArtifactKey::pair("source", "new").unwrap();
    cache
        .store_with_lineage(
            old_key.clone(),
            caap_core::ArtifactValue::Text("old".to_string()),
            [],
            lineage.clone(),
        )
        .unwrap();
    let mut snapshot = cache.snapshot();
    snapshot.stats.generation = u64::MAX - 1;
    cache.restore_snapshot(snapshot).unwrap();

    let error = cache
        .store_with_lineage(
            new_key.clone(),
            caap_core::ArtifactValue::Text("new".to_string()),
            [],
            lineage.clone(),
        )
        .unwrap_err()
        .to_string();

    assert!(error.contains("artifact cache generation overflow"));
    assert_eq!(cache.lineage_head(&lineage), Some(&old_key));
    assert!(cache.lineage_id_for_key(&new_key).is_none());
    assert!(cache.dirty_record(&old_key).is_none());
    assert_eq!(cache.stats().generation, u64::MAX - 1);
}

#[test]
fn test_artifact_cache_dependency_invalidation_propagates() {
    let mut cache = caap_core::ArtifactCache::new();
    let source = caap_core::ArtifactKey::pair("source", "main.caap").unwrap();
    let parsed = caap_core::ArtifactKey::pair("parsed", "main.caap").unwrap();
    let checked = caap_core::ArtifactKey::pair("checked", "main.caap").unwrap();

    cache
        .store(
            source.clone(),
            caap_core::ArtifactValue::Text("source".to_string()),
            [],
        )
        .unwrap();
    cache
        .store(
            parsed.clone(),
            caap_core::ArtifactValue::Text("parsed".to_string()),
            [source.clone()],
        )
        .unwrap();
    cache
        .store(
            checked.clone(),
            caap_core::ArtifactValue::Text("checked".to_string()),
            [parsed.clone()],
        )
        .unwrap();

    let record = caap_core::ArtifactInvalidationRecord::new("source_change", source.clone())
        .unwrap()
        .with_changed_inputs(["main.caap".to_string()])
        .unwrap();
    cache.mark_dirty(record).unwrap();

    assert!(cache.is_dirty(&source));
    assert!(cache.is_dirty(&parsed));
    assert!(cache.is_dirty(&checked));
    assert_eq!(cache.get(&checked), None);
    assert_eq!(
        cache.dirty_record(&parsed).unwrap().upstream_key.as_ref(),
        Some(&source)
    );
    assert_eq!(
        cache.dirty_record(&checked).unwrap().upstream_key.as_ref(),
        Some(&parsed)
    );
}

#[test]
fn test_artifact_cache_snapshot_restore_rebuilds_dependency_index() {
    let mut cache = caap_core::ArtifactCache::new();
    let source = caap_core::ArtifactKey::pair("source", "main.caap").unwrap();
    let lowered = caap_core::ArtifactKey::pair("lowered", "main.caap").unwrap();

    cache
        .store(
            source.clone(),
            caap_core::ArtifactValue::Text("source".to_string()),
            [],
        )
        .unwrap();
    cache
        .store(
            lowered.clone(),
            caap_core::ArtifactValue::Semantic(caap_core::SemanticValue::Str(
                "lowered".to_string(),
            )),
            [source.clone()],
        )
        .unwrap();
    assert!(cache.get(&lowered).is_some());

    let snapshot = cache.snapshot();
    cache
        .store(
            caap_core::ArtifactKey::pair("extra", "main.caap").unwrap(),
            caap_core::ArtifactValue::Bytes(vec![1, 2, 3]),
            [],
        )
        .unwrap();
    cache
        .mark_dirty(
            caap_core::ArtifactInvalidationRecord::new("source_change", source.clone()).unwrap(),
        )
        .unwrap();

    cache.restore_snapshot(snapshot).unwrap();

    assert!(!cache.is_dirty(&source));
    assert!(!cache.is_dirty(&lowered));
    assert_eq!(cache.dependents_for(&source), vec![lowered.clone()]);
    assert_eq!(cache.stats().hits, 1);
}

#[test]
fn test_artifact_cache_snapshot_restore_rejects_missing_dependency_entry() {
    let mut cache = caap_core::ArtifactCache::new();
    let source = caap_core::ArtifactKey::pair("source", "main.caap").unwrap();
    let lowered = caap_core::ArtifactKey::pair("lowered", "main.caap").unwrap();
    let missing = caap_core::ArtifactKey::pair("source", "missing.caap").unwrap();

    cache
        .store(
            source,
            caap_core::ArtifactValue::Text("source".to_string()),
            [],
        )
        .unwrap();
    cache
        .store(
            lowered.clone(),
            caap_core::ArtifactValue::Text("lowered".to_string()),
            [],
        )
        .unwrap();
    let mut snapshot = cache.snapshot();
    snapshot.dependencies = vec![(lowered, vec![missing])];

    let err = caap_core::ArtifactCache::new()
        .restore_snapshot(snapshot)
        .expect_err("missing dependency entry must be rejected");
    assert!(err
        .message()
        .contains("artifact snapshot dependency is missing from entries"));
}

#[test]
fn test_artifact_cache_snapshot_restore_rejects_duplicate_entries() {
    let mut cache = caap_core::ArtifactCache::new();
    let key = caap_core::ArtifactKey::pair("source", "main.caap").unwrap();
    cache
        .store(
            key.clone(),
            caap_core::ArtifactValue::Text("source".to_string()),
            [],
        )
        .unwrap();
    let mut snapshot = cache.snapshot();
    snapshot
        .entries
        .push((key, caap_core::ArtifactValue::Text("duplicate".to_string())));

    let err = caap_core::ArtifactCache::new()
        .restore_snapshot(snapshot)
        .expect_err("duplicate snapshot entries must be rejected");

    assert!(err.message().contains("entries key is duplicated"));
}

#[test]
fn test_artifact_cache_snapshot_restore_rejects_duplicate_dependencies() {
    let mut cache = caap_core::ArtifactCache::new();
    let source = caap_core::ArtifactKey::pair("source", "main.caap").unwrap();
    let lowered = caap_core::ArtifactKey::pair("lowered", "main.caap").unwrap();
    cache
        .store(
            source.clone(),
            caap_core::ArtifactValue::Text("source".to_string()),
            [],
        )
        .unwrap();
    cache
        .store(
            lowered.clone(),
            caap_core::ArtifactValue::Text("lowered".to_string()),
            [source.clone()],
        )
        .unwrap();
    let mut snapshot = cache.snapshot();
    snapshot.dependencies = vec![(lowered, vec![source.clone(), source])];

    let err = caap_core::ArtifactCache::new()
        .restore_snapshot(snapshot)
        .expect_err("duplicate dependency edges must be rejected");

    assert!(err.message().contains("duplicate dependency"));
}

#[test]
fn test_artifact_cache_file_validate_rejects_invalid_snapshot_payload() {
    let mut cache = caap_core::ArtifactCache::new();
    let key = caap_core::ArtifactKey::pair("source", "main.caap").unwrap();
    cache
        .store(
            key.clone(),
            caap_core::ArtifactValue::Text("source".to_string()),
            [],
        )
        .unwrap();
    let mut snapshot = cache.snapshot();
    snapshot
        .entries
        .push((key, caap_core::ArtifactValue::Text("duplicate".to_string())));
    let file = caap_core::ArtifactCacheFile::new(snapshot);

    let err = file
        .validate()
        .expect_err("cache file validation must reject invalid snapshot payload");

    assert!(err.message().contains("entries key is duplicated"));
}

#[test]
fn test_artifact_cache_snapshot_restore_rejects_inconsistent_lineage_graph() {
    let mut cache = caap_core::ArtifactCache::new();
    let key = caap_core::ArtifactKey::pair("parsed", "main.caap").unwrap();
    let lineage = caap_core::ArtifactKey::pair("parse_lineage", "main.caap").unwrap();

    cache
        .store_with_lineage(
            key.clone(),
            caap_core::ArtifactValue::Text("parsed".to_string()),
            [],
            lineage.clone(),
        )
        .unwrap();

    let mut missing_head = cache.snapshot();
    missing_head.lineage_heads = vec![(
        lineage.clone(),
        caap_core::ArtifactKey::pair("parsed", "missing.caap").unwrap(),
    )];
    let err = caap_core::ArtifactCache::new()
        .restore_snapshot(missing_head)
        .expect_err("missing lineage head entry must be rejected");
    assert!(err
        .message()
        .contains("artifact snapshot lineage head is missing from entries"));

    let mut missing_backref = cache.snapshot();
    missing_backref.lineages.clear();
    let err = caap_core::ArtifactCache::new()
        .restore_snapshot(missing_backref)
        .expect_err("missing lineage back-reference must be rejected");
    assert!(err
        .message()
        .contains("artifact snapshot lineage head is missing lineages back-reference"));

    let mut missing_lineage_head = cache.snapshot();
    missing_lineage_head.lineage_heads.clear();
    missing_lineage_head.lineages = vec![(key, lineage)];
    let err = caap_core::ArtifactCache::new()
        .restore_snapshot(missing_lineage_head)
        .expect_err("lineage without head must be rejected");
    assert!(err
        .message()
        .contains("artifact snapshot lineage entry references missing lineage head"));

    let cycle_a = caap_core::ArtifactKey::pair("cycle_a", "main.caap").unwrap();
    let cycle_b = caap_core::ArtifactKey::pair("cycle_b", "main.caap").unwrap();
    let mut cycle = cache.snapshot();
    cycle.entries.push((
        cycle_a.clone(),
        caap_core::ArtifactValue::Text("cycle_a".to_string()),
    ));
    cycle.entries.push((
        cycle_b.clone(),
        caap_core::ArtifactValue::Text("cycle_b".to_string()),
    ));
    cycle.lineage_heads = vec![
        (cycle_a.clone(), cycle_b.clone()),
        (cycle_b.clone(), cycle_a.clone()),
    ];
    cycle.lineages = vec![
        (cycle_a.clone(), cycle_b.clone()),
        (cycle_b.clone(), cycle_a.clone()),
    ];
    let err = caap_core::ArtifactCache::new()
        .restore_snapshot(cycle)
        .expect_err("lineage cycle must be rejected");
    assert!(err
        .message()
        .contains("artifact snapshot lineage graph contains a cycle"));
}

#[test]
fn test_artifact_cache_project_snapshot_by_kind_is_restore_ready() {
    let mut cache = caap_core::ArtifactCache::new();
    let source = caap_core::ArtifactKey::pair("source", "main.caap").unwrap();
    let parsed = caap_core::ArtifactKey::pair("parsed", "main.caap").unwrap();
    let checked = caap_core::ArtifactKey::pair("checked", "main.caap").unwrap();

    cache
        .store(
            source.clone(),
            caap_core::ArtifactValue::Text("source".to_string()),
            [],
        )
        .unwrap();
    cache
        .store(
            parsed.clone(),
            caap_core::ArtifactValue::Text("parsed".to_string()),
            [source],
        )
        .unwrap();
    cache
        .store(
            checked.clone(),
            caap_core::ArtifactValue::Text("checked".to_string()),
            [parsed.clone()],
        )
        .unwrap();

    let projection = cache.project_snapshot_by_kind("parsed").unwrap();
    assert_eq!(projection.entries.len(), 1);
    assert_eq!(projection.entries[0].0, parsed);
    assert_eq!(projection.dependencies, vec![(parsed.clone(), Vec::new())]);

    let mut restored = caap_core::ArtifactCache::new();
    restored.restore_snapshot(projection).unwrap();
    assert!(restored.peek(&parsed).is_some());
    assert!(restored.peek(&checked).is_none());
    assert!(restored.dependents_for(&parsed).is_empty());
}

#[test]
fn test_artifact_cache_file_payload_validates_format() {
    let mut cache = caap_core::ArtifactCache::new();
    let key = caap_core::ArtifactKey::pair("source", "main.caap").unwrap();
    cache
        .store(
            key,
            caap_core::ArtifactValue::Text("source".to_string()),
            [],
        )
        .unwrap();

    let cache_file = cache.cache_file();
    cache_file.validate().unwrap();
    assert_eq!(
        cache_file.format_name,
        caap_core::ArtifactCacheFile::FORMAT_NAME
    );
    assert_eq!(
        cache_file.format_version,
        caap_core::ArtifactCacheFile::FORMAT_VERSION
    );

    let mut invalid = cache_file;
    invalid.format_version += 1;
    assert!(invalid.validate().is_err());
}

#[test]
fn test_artifact_cache_file_roundtrips_through_json_file() {
    let mut cache = caap_core::ArtifactCache::new();
    let source = caap_core::ArtifactKey::pair("source", "main.caap").unwrap();
    let lowered = caap_core::ArtifactKey::pair("lowered", "main.caap").unwrap();
    let lineage = caap_core::ArtifactKey::pair("lineage", "main.caap").unwrap();

    cache
        .store_with_lineage(
            source.clone(),
            caap_core::ArtifactValue::Source(
                caap_core::SourceArtifact::inline_with_label("(int_add 1 2)", "main.caap").unwrap(),
            ),
            [],
            lineage.clone(),
        )
        .unwrap();
    cache
        .store(
            lowered.clone(),
            caap_core::ArtifactValue::Semantic(
                caap_core::SemanticValue::map([(
                    "status".to_string(),
                    caap_core::SemanticValue::Str("ok".to_string()),
                )])
                .unwrap(),
            ),
            [source.clone()],
        )
        .unwrap();
    cache
        .mark_dirty(
            caap_core::ArtifactInvalidationRecord::new("source_change", source.clone())
                .unwrap()
                .with_lineage(lineage.clone(), "lineage")
                .unwrap()
                .with_changed_inputs(["main.caap".to_string()])
                .unwrap(),
        )
        .unwrap();

    let path = std::env::temp_dir().join(format!(
        "caap-artifact-cache-{}-{}.json",
        std::process::id(),
        line!()
    ));
    cache.save_cache_file(&path).unwrap();

    let mut restored = caap_core::ArtifactCache::new();
    restored.load_cache_file(&path).unwrap();
    std::fs::remove_file(&path).ok();

    assert_eq!(restored.lineage_head(&lineage), Some(&source));
    assert!(restored.is_dirty(&source));
    assert!(restored.is_dirty(&lowered));
    assert_eq!(
        restored.dirty_record(&source).unwrap().changed_inputs,
        vec!["main.caap".to_string()]
    );
    assert_eq!(restored.dependents_for(&source), vec![lowered]);
}

#[test]
fn test_compiler_session_saves_and_loads_artifact_cache_file() {
    // Keeps an explicit host: this test opens a *second* session from the same
    // host to verify the artifact cache round-trips across sessions.
    let host = caap_core::CompilerHost::new();
    let mut compiler = host.new_session();
    let key = caap_core::ArtifactKey::pair("source", "main.caap").unwrap();
    compiler
        .artifact_cache_mut()
        .store(
            key.clone(),
            caap_core::ArtifactValue::Text("(int_add 1 2)".to_string()),
            [],
        )
        .unwrap();
    let save_version = compiler.session_version();
    let path = std::env::temp_dir().join(format!(
        "caap-compiler-artifact-cache-{}-{}.json",
        std::process::id(),
        line!()
    ));

    compiler.save_artifact_cache_file(&path).unwrap();
    assert!(compiler.session_version() > save_version);
    assert_eq!(
        compiler
            .events()
            .by_kind("compiler.artifact_cache.save")
            .unwrap()[0]
            .target
            .as_deref(),
        Some(path.to_string_lossy().as_ref())
    );

    let mut restored = host.new_session();
    restored.load_artifact_cache_file(&path).unwrap();
    std::fs::remove_file(&path).ok();

    assert_eq!(
        restored.artifact_cache().peek(&key),
        Some(&caap_core::ArtifactValue::Text("(int_add 1 2)".to_string()))
    );
    assert_eq!(
        restored
            .events()
            .by_kind("compiler.artifact_cache.load")
            .unwrap()[0]
            .target
            .as_deref(),
        Some(path.to_string_lossy().as_ref())
    );
}

#[test]
fn test_artifact_cache_reusable_snapshot_restores_lineage_state() {
    let mut cache = caap_core::ArtifactCache::new();
    let lineage = caap_core::ArtifactKey::new([
        "parse_surface_source".to_string(),
        "compile_time".to_string(),
        "/workspace/main.caap".to_string(),
    ])
    .unwrap();
    let key = caap_core::ArtifactKey::new([
        "parse_surface".to_string(),
        "parse".to_string(),
        "compile_time".to_string(),
        "/workspace/main.caap".to_string(),
        "token_a".to_string(),
    ])
    .unwrap();

    cache
        .store_with_lineage(
            key.clone(),
            caap_core::ArtifactValue::Text("template".to_string()),
            [],
            lineage.clone(),
        )
        .unwrap();
    let snapshot = cache.reusable_snapshot();
    cache.invalidate_all("invalidate_all").unwrap();

    cache.restore_reusable_snapshot(snapshot).unwrap();

    assert_eq!(cache.lineage_head(&lineage), Some(&key));
    assert!(!cache.is_dirty(&key));
    assert!(cache.peek(&key).is_some());
}

#[test]
fn test_source_artifact_inline_uses_sha256_digest() {
    let source = caap_core::SourceArtifact::inline("abc").unwrap();

    assert_eq!(
        source.fingerprint.as_str(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        source
            .parse_surface_key("parse", caap_core::PhasePolicy::CompileTime)
            .unwrap()
            .parts(),
        &[
            "parse_surface_inline".to_string(),
            "parse".to_string(),
            "compile_time".to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string(),
        ]
    );
    assert_eq!(
        source
            .parse_surface_lineage_id(caap_core::PhasePolicy::CompileTime)
            .unwrap()
            .parts(),
        &[
            "parse_surface_inline".to_string(),
            "compile_time".to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string(),
        ]
    );
}

#[test]
fn test_source_artifact_path_key_uses_path_token() {
    let source = caap_core::SourceArtifact::path(
        "/workspace/main.caap",
        "mtime:123:size:10",
        "(int_add 1 2)",
    )
    .unwrap();

    assert_eq!(
        source
            .parse_surface_key("parse", caap_core::PhasePolicy::CompileTime)
            .unwrap()
            .parts(),
        &[
            "parse_surface".to_string(),
            "parse".to_string(),
            "compile_time".to_string(),
            "/workspace/main.caap".to_string(),
            "mtime:123:size:10".to_string(),
        ]
    );
    assert_eq!(
        source
            .parse_surface_lineage_id(caap_core::PhasePolicy::CompileTime)
            .unwrap()
            .parts(),
        &[
            "parse_surface_source".to_string(),
            "compile_time".to_string(),
            "/workspace/main.caap".to_string(),
        ]
    );
}

#[test]
fn test_artifact_cache_lineage_replacement_invalidates_previous_head() {
    let mut cache = caap_core::ArtifactCache::new();
    let lineage = caap_core::ArtifactKey::new([
        "parse_surface_inline".to_string(),
        "compile_time".to_string(),
        "lineage".to_string(),
    ])
    .unwrap();
    let first = caap_core::ArtifactKey::new([
        "parse_surface_inline".to_string(),
        "parse".to_string(),
        "compile_time".to_string(),
        "digest_a".to_string(),
    ])
    .unwrap();
    let second = caap_core::ArtifactKey::new([
        "parse_surface_inline".to_string(),
        "parse".to_string(),
        "compile_time".to_string(),
        "digest_b".to_string(),
    ])
    .unwrap();
    let dependent = caap_core::ArtifactKey::pair("checked", "main").unwrap();

    cache
        .store_with_lineage(
            first.clone(),
            caap_core::ArtifactValue::Text("first".to_string()),
            [],
            lineage.clone(),
        )
        .unwrap();
    cache
        .store(
            dependent.clone(),
            caap_core::ArtifactValue::Text("dependent".to_string()),
            [first.clone()],
        )
        .unwrap();
    cache
        .store_with_lineage(
            second.clone(),
            caap_core::ArtifactValue::Text("second".to_string()),
            [],
            lineage.clone(),
        )
        .unwrap();

    assert_eq!(cache.lineage_head(&lineage), Some(&second));
    assert_eq!(cache.lineage_id_for_key(&second), Some(&lineage));
    assert!(cache.is_dirty(&first));
    assert!(cache.is_dirty(&dependent));
    assert!(!cache.is_dirty(&second));
    assert!(!cache.is_lineage_dirty(&lineage));

    let record = cache.latest_invalidation_for_lineage(&lineage).unwrap();
    assert_eq!(record.reason_kind, "lineage_replaced");
    assert_eq!(record.invalidated_key, first);
    assert_eq!(record.replacement_key.as_ref(), Some(&second));
    assert_eq!(record.changed_inputs, vec!["source_digest".to_string()]);
    assert_eq!(
        cache.dirty_record(&dependent).unwrap().changed_inputs,
        vec!["source_digest".to_string()]
    );
}

#[test]
fn test_changed_inputs_for_lineage_matches_reference_labels() {
    let lineage = caap_core::ArtifactKey::new([
        "unit_input".to_string(),
        "unit_id".to_string(),
        "stage".to_string(),
        "compile_time".to_string(),
    ])
    .unwrap();
    let previous = caap_core::ArtifactKey::new([
        "unit_input".to_string(),
        "parse".to_string(),
        "fingerprint_a".to_string(),
        "compile_time".to_string(),
        "names_a".to_string(),
    ])
    .unwrap();
    let replacement = caap_core::ArtifactKey::new([
        "unit_input".to_string(),
        "parse".to_string(),
        "fingerprint_b".to_string(),
        "compile_time".to_string(),
        "names_b".to_string(),
    ])
    .unwrap();

    assert_eq!(
        caap_core::changed_inputs_for_lineage(&lineage, &previous, &replacement),
        vec!["unit_fingerprint".to_string(), "names_version".to_string()]
    );
}

#[test]
fn test_source_template_cache_reuses_materialized_unit_template() {
    let mut cache = caap_core::SourceTemplateCache::new();
    let source = caap_core::SourceArtifact::inline("(int_add 1 2)").unwrap();
    let mut materialize_calls = 0;

    let first = cache
        .load(
            source.clone(),
            "parse",
            caap_core::PhasePolicy::CompileTime,
            |source| {
                materialize_calls += 1;
                let graph = parse(&source.text)?;
                Ok(Unit::from_graph("inline", graph)?.to_template())
            },
        )
        .unwrap();
    let second = cache
        .load(
            source,
            "parse",
            caap_core::PhasePolicy::CompileTime,
            |source| {
                materialize_calls += 1;
                let graph = parse(&source.text)?;
                Ok(Unit::from_graph("inline", graph)?.to_template())
            },
        )
        .unwrap();

    assert_eq!(first.key, second.key);
    assert!(!first.cache_hit);
    assert!(second.cache_hit);
    assert_eq!(first.template.ir.top_level_forms.len(), 1);
    assert_eq!(second.template.ir.top_level_forms.len(), 1);
    assert_eq!(materialize_calls, 1);
    assert_eq!(cache.artifact_cache().stats().misses, 1);
    assert_eq!(cache.artifact_cache().stats().hits, 1);
}

#[test]
fn test_source_template_cache_restores_template_from_artifact_snapshot() {
    let mut cache = caap_core::SourceTemplateCache::new();
    let source = caap_core::SourceArtifact::inline("(int_add 1 2)").unwrap();

    cache
        .load(
            source.clone(),
            "parse",
            caap_core::PhasePolicy::CompileTime,
            |source| {
                let graph = parse(&source.text)?;
                Ok(Unit::from_graph("inline", graph)?.to_template())
            },
        )
        .unwrap();
    let snapshot = cache.artifact_cache().snapshot();

    let mut restored = caap_core::SourceTemplateCache::new();
    restored
        .artifact_cache_mut()
        .restore_snapshot(snapshot)
        .unwrap();
    let mut materialize_calls = 0;
    let artifact = restored
        .load(
            source,
            "parse",
            caap_core::PhasePolicy::CompileTime,
            |source| {
                materialize_calls += 1;
                let graph = parse(&source.text)?;
                Ok(Unit::from_graph("inline", graph)?.to_template())
            },
        )
        .unwrap();

    assert!(artifact.cache_hit);
    assert_eq!(artifact.template.ir.top_level_forms.len(), 1);
    assert_eq!(materialize_calls, 0);
    assert_eq!(restored.artifact_cache().stats().hits, 1);
}

#[test]
fn test_source_template_cache_path_token_change_replaces_lineage_head() {
    let mut cache = caap_core::SourceTemplateCache::new();
    let first_source =
        caap_core::SourceArtifact::path("/workspace/main.caap", "token_a", "(int_add 1 2)")
            .unwrap();
    let second_source =
        caap_core::SourceArtifact::path("/workspace/main.caap", "token_b", "(int_add 1 3)")
            .unwrap();

    let first = cache
        .load(
            first_source,
            "parse",
            caap_core::PhasePolicy::CompileTime,
            |source| {
                let graph = parse(&source.text)?;
                Ok(Unit::from_graph("path", graph)?.to_template())
            },
        )
        .unwrap();
    let second = cache
        .load(
            second_source,
            "parse",
            caap_core::PhasePolicy::CompileTime,
            |source| {
                let graph = parse(&source.text)?;
                Ok(Unit::from_graph("path", graph)?.to_template())
            },
        )
        .unwrap();

    assert_ne!(first.key, second.key);
    assert_eq!(
        cache.artifact_cache().lineage_head(&first.lineage_id),
        Some(&second.key)
    );
    let record = cache
        .artifact_cache()
        .latest_invalidation_for_lineage(&first.lineage_id)
        .unwrap();
    assert_eq!(record.reason_kind, "lineage_replaced");
    assert_eq!(record.invalidated_key, first.key);
    assert_eq!(record.replacement_key.as_ref(), Some(&second.key));
    assert_eq!(record.changed_inputs, vec!["source_token".to_string()]);
}
