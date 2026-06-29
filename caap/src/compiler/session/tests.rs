//! Tests for the compiler session (split out of the module root).
use super::super::bootstrap::bootstrap_execution_memo_key;
use super::super::bridge::CompilerBridgeValue;
use super::super::bridges::QueryArtifactSource;
use super::super::query_provider::{
    EffectSet, ProviderCacheEntry, QueryExecutionOptions, QueryProvider,
    QueryProviderExecutionRecord,
};
use super::super::query_service::{
    cached_execution_records, cached_execution_records_for_request,
    initial_bindings_identity_token, provider_effect_policy_violation,
    provider_execution_record_to_semantic_value, query_stage_cache_key,
    CachedQueryStageReplayRequest, ProviderIrChangeStats, QueryStageCacheKeyInput,
    QueryStageCacheVersions, SemanticWriteSummary,
};
use super::*;
use crate::artifacts::ArtifactValue;
use crate::compiler::{QueryProviderCacheScope, QueryProviderResumePolicy};
use crate::semantic::{CapabilityName, SemanticValue};
use crate::values::HostFunction;

fn test_query_provider(name: &str, effect_tags: Vec<String>) -> QueryProvider {
    QueryProvider {
        name: name.to_string(),
        stage: "test_stage".to_string(),
        family: None,
        phase_policy: PhasePolicy::CompileTime,
        requires: Vec::new(),
        requires_data: Vec::new(),
        provides_data: Vec::new(),
        provides: Vec::new(),
        effect_tags: EffectSet::from_unique_strings(effect_tags, "test effect tag").unwrap(),
        input_schema: None,
        reads: Vec::new(),
        writes: Vec::new(),
        cache_scope: QueryProviderCacheScope::Unit,
        resume_policy: QueryProviderResumePolicy::Safe,
        registration_index: 0,
        enforce_effect_postconditions: true,
        callback: Rc::new(|_| Ok(QueryProviderCallbackOutcome::default())),
    }
}

fn test_provider_context(provider: &str) -> QueryProviderContext {
    QueryProviderContext {
        provider: provider.to_string(),
        stage: "test_stage".to_string(),
        family: None,
        phase: PhasePolicy::CompileTime,
        unit_id: "test.unit".to_string(),
        effect_tags: EffectSet::empty(),
        initial_bindings: Vec::new(),
        registration_index: 0,
        reads_subjects: Vec::new(),
        writes_subjects: Vec::new(),
        read_cells: Vec::new(),
        write_cells: Vec::new(),
        reads_files: Vec::new(),
        writes_files: Vec::new(),
        artifact_dependencies: Vec::new(),
    }
}

#[test]
fn compiler_register_unit_advances_unit_registry_version() {
    let host = CompilerHost::new();
    let mut compiler = host.new_session();
    let initial_session_version = compiler.session_version;
    let initial_unit_registry_version = compiler.unit_registry_version;

    compiler
        .register_unit(Unit::empty("registered.unit").unwrap())
        .unwrap();

    assert_eq!(
        compiler.unit_registry_version,
        initial_unit_registry_version + 1
    );
    assert!(compiler.session_version > initial_session_version);

    let after_first_register = compiler.unit_registry_version;
    compiler
        .register_unit(Unit::empty("registered.unit").unwrap())
        .unwrap();
    assert_eq!(compiler.unit_registry_version, after_first_register);
}

#[test]
fn compiler_register_unit_rejects_unit_registry_version_overflow_without_mutating() {
    let host = CompilerHost::new();
    let mut compiler = host.new_session();
    compiler.unit_registry_version = u64::MAX;

    let error = compiler
        .register_unit(Unit::empty("overflow.unit").unwrap())
        .unwrap_err()
        .to_string();

    assert!(error.contains("compiler unit registry version overflow"));
    assert!(!compiler.units.contains_key("overflow.unit"));
    assert_eq!(compiler.unit_registry_version, u64::MAX);
    assert_eq!(compiler.session_version, 0);
}

#[test]
fn compiler_diagnostic_and_event_recording_reject_session_version_overflow_atomically() {
    let host = CompilerHost::new();
    let mut compiler = host.new_session();
    compiler.session_version = u64::MAX;

    let diagnostic_error = compiler
        .push_diagnostic(Diagnostic::warning("session version overflow diagnostic").unwrap())
        .unwrap_err()
        .to_string();
    assert!(diagnostic_error.contains("compiler session version overflow"));
    assert!(compiler.diagnostics().is_empty());
    assert_eq!(compiler.session_version(), u64::MAX);

    let event_error = compiler
        .emit_event(
            CompilerEvent::new("session.overflow", "session version overflow event").unwrap(),
        )
        .unwrap_err()
        .to_string();
    assert!(event_error.contains("compiler session version overflow"));
    assert!(compiler.events().events().is_empty());
    assert_eq!(compiler.session_version(), u64::MAX);
}

#[test]
fn fact_schema_type_bridge_deserialize_rejects_unknown_bridge() {
    let err = serde_json::from_str::<FactSchemaTypeBridge>(
        r#"{"label":"name","bridge_name":"unknown","kind":"string"}"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("unknown fact schema type bridge"));
}

#[test]
fn fact_schema_registry_deserialize_rejects_key_mismatch() {
    let err = serde_json::from_str::<FactSchemaRegistry>(
            r#"{"type_bridges":{"other":{"label":"name","bridge_name":"string","kind":"string"}},"schemas":{},"version":1}"#,
        )
        .unwrap_err();
    assert!(err.to_string().contains("does not match entry label"));
}

#[test]
fn bootstrap_image_deserialize_rejects_empty_name() {
    let err = serde_json::from_str::<BootstrapImage>(
        r#"{"name":"","units":[],"capabilities":{"grants":{},"version":0},"session_version":0}"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("image name must be non-empty"));
}

#[test]
fn bootstrap_image_validation_rejects_duplicate_units() {
    let unit = Unit::empty("bootstrap.unit").unwrap().to_template();
    let image = BootstrapImage {
        name: "base".to_string(),
        units: vec![unit.clone(), unit],
        capabilities: BootstrapCapabilityGraph::new(),
        fact_schema: FactSchemaRegistry::default(),
        base_semantic_entries: Vec::new(),
        session_version: 0,
    };

    let error = image.validate().unwrap_err().to_string();

    assert!(error.contains("duplicate unit id"));
    assert!(error.contains("bootstrap.unit"));
}

#[test]
fn bootstrap_image_validation_rejects_dangling_capability_grants() {
    let unit = Unit::empty("bootstrap.unit").unwrap().to_template();
    let mut capabilities = BootstrapCapabilityGraph::new();
    capabilities.grant("missing.unit", "sys").unwrap();
    let image = BootstrapImage {
        name: "base".to_string(),
        units: vec![unit],
        capabilities,
        fact_schema: FactSchemaRegistry::default(),
        base_semantic_entries: Vec::new(),
        session_version: 0,
    };

    let error = image.validate().unwrap_err().to_string();

    assert!(error.contains("capability grant references missing unit"));
    assert!(error.contains("missing.unit"));
}

#[test]
fn bootstrap_image_file_deserialize_rejects_bad_format() {
    let err = serde_json::from_str::<BootstrapImageFile>(
            r#"{"format_name":"bad","format_version":1,"image":{"name":"base","units":[],"capabilities":{"grants":{},"version":0},"session_version":0}}"#,
        )
        .unwrap_err();
    assert!(err.to_string().contains("format name is unsupported"));
}

#[test]
fn bootstrap_vfs_deserialize_rejects_empty_path() {
    let err = serde_json::from_str::<BootstrapVirtualFileSystem>(
        r#"{"files":{"   ":"text"},"version":1}"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("path must be non-empty"));
}

#[test]
fn bootstrap_capability_graph_deserialize_rejects_empty_capability() {
    let err =
        serde_json::from_str::<BootstrapCapabilityGraph>(r#"{"grants":{"unit":[""]},"version":1}"#)
            .unwrap_err();
    assert!(err
        .to_string()
        .contains("capability name must be non-empty"));
}

#[test]
fn bootstrap_capability_graph_rejects_malformed_capability_names() {
    let mut graph = BootstrapCapabilityGraph::new();

    let error = graph.grant("unit", "fs..read").unwrap_err().to_string();

    assert!(error.contains("segments must be non-empty"));
}

#[test]
fn bootstrap_capability_graph_rejects_control_characters() {
    let mut graph = BootstrapCapabilityGraph::new();

    let error = graph
        .grant("unit", "fs.read\0secret")
        .unwrap_err()
        .to_string();

    assert!(error.contains("control characters"));
}

#[test]
fn bootstrap_execution_memo_key_is_segment_delimited() {
    let left = bootstrap_execution_memo_key("a", "bc", "d", &[CapabilityName::new("ef").unwrap()]);
    let right = bootstrap_execution_memo_key("ab", "c", "de", &[CapabilityName::new("f").unwrap()]);

    assert_ne!(left, right);
    assert_eq!(left, "1:a2:bc1:d2:ef");
}

#[test]
fn bootstrap_capability_graph_matches_only_exact_grants() {
    let mut graph = BootstrapCapabilityGraph::new();
    graph.grant("unit", "sys.fs.read_text").unwrap();

    assert!(graph.allows("unit", "sys.fs.read_text"));
    assert!(!graph.allows("unit", "sys.fs.write_text"));
}

#[test]
fn bootstrap_capability_graph_rejects_wildcard_grants() {
    let mut graph = BootstrapCapabilityGraph::new();

    let prefix_error = graph.grant("unit", "sys.fs.*").unwrap_err().to_string();
    assert!(prefix_error.contains("wildcard grants are not supported"));

    let global_error = graph.grant("unit", "*").unwrap_err().to_string();
    assert!(global_error.contains("wildcard grants are not supported"));
}

#[test]
fn bootstrap_capability_graph_revokes_exact_grants() {
    let mut graph = BootstrapCapabilityGraph::new();
    assert!(graph.grant("unit", "sys.fs.read_text").unwrap());
    assert!(!graph.grant("unit", "sys.fs.read_text").unwrap());
    assert_eq!(graph.version(), 1);

    assert!(graph.revoke("unit", "sys.fs.read_text").unwrap());
    assert_eq!(graph.version(), 2);
    assert!(!graph.allows("unit", "sys.fs.read_text"));
    assert!(graph.capabilities_for("unit").is_empty());
    assert!(graph.unit_ids().is_empty());

    assert!(!graph.revoke("unit", "sys.fs.read_text").unwrap());
    assert_eq!(graph.version(), 2);
}

#[test]
fn dynamic_provider_dependency_validation_rejects_self_cycles() {
    let dynamic_requires = BTreeMap::new();

    let error = validate_dynamic_provider_dependency(&dynamic_requires, "provider.a", "provider.a")
        .unwrap_err()
        .to_string();

    assert!(error.contains("dynamic provider dependency cycle"));
    assert!(error.contains("provider.a -> provider.a"));
}

#[test]
fn dynamic_provider_dependency_validation_rejects_transitive_cycles() {
    let dynamic_requires = BTreeMap::from([
        ("provider.b".to_string(), vec!["provider.c".to_string()]),
        ("provider.c".to_string(), vec!["provider.a".to_string()]),
    ]);

    let error = validate_dynamic_provider_dependency(&dynamic_requires, "provider.a", "provider.b")
        .unwrap_err()
        .to_string();

    assert!(error.contains("dynamic provider dependency cycle"));
    assert!(error.contains("provider.a -> provider.b"));
    assert!(error.contains("provider.c -> provider.a"));
}

#[test]
fn provider_effect_policy_requires_declared_file_effects() {
    let provider = test_query_provider("file_reader", Vec::new());
    let mut context = test_provider_context("file_reader");
    context.reads_files = vec!["/tmp/input.caap".to_string()];

    let error = provider_effect_policy_violation(
        &provider,
        0,
        &ProviderIrChangeStats::default(),
        false,
        &SemanticWriteSummary::default(),
        Some(&context),
    )
    .unwrap();

    assert!(error.contains("read files without declaring read-files effect"));

    let provider = test_query_provider("file_reader", vec!["read_files".to_string()]);
    assert!(provider_effect_policy_violation(
        &provider,
        0,
        &ProviderIrChangeStats::default(),
        false,
        &SemanticWriteSummary::default(),
        Some(&context),
    )
    .is_none());

    let provider = test_query_provider("semantic_writer", vec!["write_attributes".to_string()]);
    assert!(provider_effect_policy_violation(
        &provider,
        0,
        &ProviderIrChangeStats::default(),
        false,
        &SemanticWriteSummary {
            facts: false,
            attributes: true,
            symbols: false,
        },
        None,
    )
    .is_none());

    let provider = test_query_provider("semantic_writer", vec!["write_symbols".to_string()]);
    assert!(provider_effect_policy_violation(
        &provider,
        0,
        &ProviderIrChangeStats::default(),
        false,
        &SemanticWriteSummary {
            facts: false,
            attributes: false,
            symbols: true,
        },
        None,
    )
    .is_none());
}

#[test]
fn provider_effect_policy_requires_declared_file_write_effects() {
    let provider = test_query_provider("file_writer", Vec::new());
    let mut context = test_provider_context("file_writer");
    context.writes_files = vec!["/tmp/output.caap".to_string()];

    let error = provider_effect_policy_violation(
        &provider,
        0,
        &ProviderIrChangeStats::default(),
        false,
        &SemanticWriteSummary::default(),
        Some(&context),
    )
    .unwrap();

    assert!(error.contains("wrote files without declaring write-files effect"));

    let provider = test_query_provider("file_writer", vec!["write_files".to_string()]);
    assert!(provider_effect_policy_violation(
        &provider,
        0,
        &ProviderIrChangeStats::default(),
        false,
        &SemanticWriteSummary::default(),
        Some(&context),
    )
    .is_none());
}

#[test]
fn provider_effect_policy_requires_declared_semantic_write_effects() {
    let provider = test_query_provider("semantic_writer", Vec::new());

    let error = provider_effect_policy_violation(
        &provider,
        0,
        &ProviderIrChangeStats::default(),
        false,
        &SemanticWriteSummary {
            facts: true,
            attributes: false,
            symbols: false,
        },
        None,
    )
    .unwrap();

    assert!(error.contains("modified facts without declaring write-facts effect"));

    for effect in ["write_attributes", "write_symbols"] {
        let provider = test_query_provider("semantic_writer", vec![effect.to_string()]);
        assert!(provider_effect_policy_violation(
            &provider,
            0,
            &ProviderIrChangeStats::default(),
            false,
            &SemanticWriteSummary {
                facts: true,
                attributes: false,
                symbols: false,
            },
            None,
        )
        .is_some());
    }
    let provider = test_query_provider("semantic_writer", vec!["write_facts".to_string()]);
    assert!(provider_effect_policy_violation(
        &provider,
        0,
        &ProviderIrChangeStats::default(),
        false,
        &SemanticWriteSummary {
            facts: true,
            attributes: false,
            symbols: false,
        },
        None,
    )
    .is_none());
}

#[test]
fn provider_effect_policy_requires_declared_attribute_write_effect() {
    let provider = test_query_provider("attribute_writer", Vec::new());

    let error = provider_effect_policy_violation(
        &provider,
        0,
        &ProviderIrChangeStats::default(),
        true,
        &SemanticWriteSummary::default(),
        None,
    )
    .unwrap();

    assert!(error.contains("modified unit attributes without declaring write-attributes"));

    let provider = test_query_provider("attribute_writer", vec!["write_attributes".to_string()]);
    assert!(provider_effect_policy_violation(
        &provider,
        0,
        &ProviderIrChangeStats::default(),
        true,
        &SemanticWriteSummary::default(),
        None,
    )
    .is_none());
}

#[test]
fn provider_fact_schema_validation_catches_direct_unit_fact_writes() {
    let host = CompilerHost::new();
    let mut compiler = host.new_session();
    let mut unit = Unit::empty("fact_schema_unit").unwrap();
    Rc::make_mut(&mut compiler.fact_schema)
        .register_type_bridge("demo_string", "string")
        .unwrap();
    Rc::make_mut(&mut compiler.fact_schema)
        .register_schema("demo.fact", "demo_string", false, None)
        .unwrap();
    compiler.register_stage("check").unwrap();
    compiler
        .register_provider_with_effects(
            "invalid_fact_writer",
            "check",
            PhasePolicy::CompileTime,
            ["write_facts".to_string()],
            |context| {
                context.set_unit_fact(
                    crate::semantic::SemanticSubjectId::new("unit", "fact_schema_unit")
                        .map_err(|error| error.to_string())?,
                    "demo.fact",
                    SemanticValue::Int(1),
                )?;
                Ok(())
            },
        )
        .unwrap();

    let error = compiler
        .queries()
        .query("check", &mut unit, PhasePolicy::CompileTime)
        .unwrap_err()
        .to_string();

    assert!(error.contains("violates compiler fact schema"));
    assert!(unit
        .semantics()
        .query_facts(None, Some("demo.fact"))
        .unwrap()
        .is_empty());
    assert!(compiler
        .events()
        .by_kind("query.provider.fact_schema_violation")
        .is_ok());
}

#[test]
fn query_provider_panic_is_reported_and_rolls_back_declared_unit_writes() {
    let host = CompilerHost::new();
    let mut compiler = host.new_session();
    let mut unit = Unit::empty("panic_provider_unit").unwrap();
    compiler.register_stage("check").unwrap();
    compiler
        .register_provider_contract_spec(
            QueryProviderContractSpec {
                name: "panic_provider".to_string(),
                stage: "check".to_string(),
                family: None,
                phase_policy: PhasePolicy::CompileTime,
                requires: Vec::new(),
                effect_tags: vec!["write_attributes".to_string()],
                registration: QueryProviderRegistrationSpec {
                    family: None,
                    input_schema: None,
                    requires_data: Vec::new(),
                    provides_data: Vec::new(),
                    reads: Vec::new(),
                    writes: vec!["attributes".to_string()],
                    cache_scope: "none".to_string(),
                    resume_policy: "safe".to_string(),
                },
            },
            |context| {
                context.set_unit_attribute("panic.touched", SemanticValue::Bool(true))?;
                panic!("callback exploded");
            },
        )
        .unwrap();

    let error = compiler
        .queries()
        .query("check", &mut unit, PhasePolicy::CompileTime)
        .unwrap_err()
        .to_string();

    assert!(error.contains("query provider 'panic_provider' panicked"));
    assert!(error.contains("callback exploded"));
    assert!(!unit.attributes().contains_key("panic.touched"));
    assert!(compiler.events().by_kind("query.provider.error").is_ok());
}

#[test]
fn compiler_registry_restore_rejects_invalid_snapshot_without_mutation() {
    let mut registry = CompilerRegistry::new();
    registry
        .register_value("valid.name", RuntimeValue::Int(1))
        .unwrap();
    let mut snapshot = registry.snapshot();
    snapshot.values.insert("".to_string(), RuntimeValue::Int(2));

    let error = registry.restore_snapshot(snapshot).unwrap_err().to_string();

    assert!(error.contains("compiler registry names must be non-empty"));
    assert_eq!(
        registry.lookup_value("valid.name").unwrap(),
        Some(&RuntimeValue::Int(1))
    );
    assert_eq!(registry.registered_names(), vec!["valid.name"]);
}

#[test]
fn compiler_bridge_registry_registration_marks_session_dirty() {
    let host = CompilerHost::new();
    let mut compiler = host.new_session();
    let initial_session_version = compiler.session_version();
    let bridge = CompilerBridgeValue::from_session_state(compiler.clone());

    bridge
        .register_value("bridge.value", RuntimeValue::Int(1))
        .unwrap();
    bridge.commit_session_into(&mut compiler);

    assert_eq!(compiler.registry().version(), 1);
    assert!(compiler.session_version() > initial_session_version);
    assert_eq!(
        compiler.lookup_registered_value("bridge.value").unwrap(),
        Some(&RuntimeValue::Int(1))
    );
}

#[test]
fn compiler_bridge_applies_provider_ctfe_cache_back_to_session() {
    let host = CompilerHost::new();
    let mut compiler = host.new_session();
    let bridge = CompilerBridgeValue::from_session_state(compiler.clone());
    let key = ArtifactKey::single("provider_cache_entry").unwrap();
    bridge.session.borrow_mut().cache.ctfe_cache.insert(
        key.clone(),
        ProviderCacheEntry {
            recorded_at_unix_ns: 1,
            snapshot: None,
            diagnostics: Vec::new(),
            reads_subjects: Vec::new(),
            writes_subjects: Vec::new(),
            read_cells: Vec::new(),
            write_cells: Vec::new(),
            reads_files: Vec::new(),
            writes_files: Vec::new(),
            artifact_dependencies: Vec::new(),
            dynamic_requires: Vec::new(),
            changed: false,
            restart_requested: false,
            restart_stage: None,
        },
    );

    bridge.commit_session_into(&mut compiler);

    assert!(compiler.cache.ctfe_cache.contains_key(&key));
}

#[test]
fn compiler_bridge_applies_bootstrap_execution_memo_back_to_session() {
    let host = CompilerHost::new();
    let mut compiler = host.new_session();
    let bridge = CompilerBridgeValue::from_session_state(compiler.clone());
    bridge
        .session
        .borrow()
        .bootstrap
        .execution_memo
        .borrow_mut()
        .insert("memo_key".to_string());

    bridge.commit_session_into(&mut compiler);

    assert!(compiler
        .bootstrap
        .execution_memo
        .borrow()
        .contains("memo_key"));
}

#[test]
fn default_compile_time_host_starts_without_ambient_read_root() {
    let mut host = CompilerHost::new();
    host.register_default_compile_time_system_libraries()
        .unwrap();

    let policy = host.compile_time_services().system_policy();
    assert_eq!(policy.fs.read_roots, Some(Vec::new()));
    assert_eq!(policy.fs.write_roots, Some(Vec::new()));
}

#[test]
fn source_backed_query_requires_explicit_surface_input_stage() {
    let host = CompilerHost::new();
    let mut compiler = host.new_session();
    compiler.register_stage("compile_unit").unwrap();
    let bridge = CompilerBridgeValue::from_session_state(compiler.clone());

    let err = bridge
        .query_execution_projection_with_options(
            "compile_unit",
            QueryArtifactSource::Text("42".to_string()),
            PhasePolicy::CompileTime,
            QueryExecutionOptions::default(),
        )
        .unwrap_err();

    assert!(err
        .to_string()
        .contains("query source input kind \"surface\" must be registered"));
}

#[test]
fn source_backed_query_artifact_executes_from_surface_origin_stage() {
    let host = CompilerHost::new();
    let mut compiler = host.new_session();
    compiler
        .register_stage_spec(
            QueryStageSpec::new("parse_surface")
                .unwrap()
                .with_input_kinds(["surface".to_string()])
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_stage_spec(
            QueryStageSpec::new("compile_unit")
                .unwrap()
                .with_requires(["parse_surface".to_string()])
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_provider(
            "parse_provider_should_not_run",
            "parse_surface",
            PhasePolicy::CompileTime,
            |_context| Err("parse provider should not run".to_string()),
        )
        .unwrap();
    compiler
        .register_provider(
            "compile_provider",
            "compile_unit",
            PhasePolicy::CompileTime,
            |_context| Ok(()),
        )
        .unwrap();
    let bridge = CompilerBridgeValue::from_session_state(compiler.clone());

    let artifact = bridge
        .query_execution_projection_with_options(
            "compile_unit",
            QueryArtifactSource::Text("42".to_string()),
            PhasePolicy::CompileTime,
            QueryExecutionOptions::default(),
        )
        .unwrap()
        .artifact
        .unwrap();

    assert_eq!(artifact.stage, "compile_unit");
    assert_eq!(artifact.iterations, 1);
    let entries = match artifact.value {
        ArtifactValue::Semantic(SemanticValue::Map(entries)) => entries,
        ArtifactValue::QueryStage(cached) => match cached.summary {
            SemanticValue::Map(entries) => entries,
            _ => panic!("expected semantic query artifact value"),
        },
        _ => panic!("expected semantic query artifact value"),
    };
    assert_eq!(
        entries.iter().find(|(key, _)| key == "provider_count"),
        Some(&("provider_count".to_string(), SemanticValue::Int(1)))
    );
}

#[test]
fn observed_unit_change_uses_stage_restart_policy_without_reported_change() {
    let host = CompilerHost::new();
    let mut compiler = host.new_session();
    let mut unit = Unit::empty("restart_observed_change").unwrap();
    let validate_runs = Rc::new(std::cell::Cell::new(0));
    compiler
        .register_stage_spec(QueryStageSpec::new("lower").unwrap())
        .unwrap();
    compiler
        .register_stage_spec(
            QueryStageSpec::new("validate")
                .unwrap()
                .with_requires(["lower".to_string()])
                .unwrap()
                .with_restart_stage("lower")
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_provider(
            "lower_provider",
            "lower",
            PhasePolicy::CompileTime,
            |_context| Ok(()),
        )
        .unwrap();
    compiler
        .register_provider_with_effects(
            "validate_provider",
            "validate",
            PhasePolicy::CompileTime,
            ["write_attributes".to_string()],
            {
                let validate_runs = Rc::clone(&validate_runs);
                move |context| {
                    validate_runs.set(validate_runs.get() + 1);
                    if validate_runs.get() == 1 {
                        context.set_unit_attribute("validated", SemanticValue::Bool(true))?;
                    }
                    Ok(())
                }
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query("validate", &mut unit, PhasePolicy::CompileTime)
        .unwrap();

    assert_eq!(validate_runs.get(), 2);
    assert_eq!(
        plan.steps
            .iter()
            .map(|step| step.stage.as_str())
            .collect::<Vec<_>>(),
        vec!["lower", "validate", "lower", "validate"]
    );
    assert!(plan.executed[1].changed);
    assert_eq!(plan.executed[1].restart_stage.as_deref(), Some("lower"));
}

#[test]
fn cached_execution_records_reject_missing_summary() {
    let value = ArtifactValue::Semantic(SemanticValue::map([]).unwrap());
    let err = cached_execution_records(&value).unwrap_err();
    assert!(err
        .to_string()
        .contains("missing or malformed execution_summary"));
}

#[test]
fn cached_execution_records_reject_missing_cache_timestamp() {
    let record = cached_execution_record_fixture();
    let value = ArtifactValue::Semantic(
        SemanticValue::map([(
            "execution_summary".to_string(),
            SemanticValue::List(vec![
                provider_execution_record_to_semantic_value(&record).unwrap()
            ]),
        )])
        .unwrap(),
    );

    let err = cached_execution_records(&value).unwrap_err();

    assert!(err
        .to_string()
        .contains("missing or malformed cache_written_at_unix_ns"));
}

#[test]
fn cached_execution_records_reject_unknown_summary_fields() {
    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    let ArtifactValue::Semantic(SemanticValue::Map(entries)) = &mut value else {
        panic!("expected semantic execution summary");
    };
    entries.push(("legacy_cache_marker".to_string(), SemanticValue::Bool(true)));

    let err = cached_execution_records(&value).unwrap_err();

    assert!(err
        .to_string()
        .contains("summary contains unknown field 'legacy_cache_marker'"));
}

#[test]
fn cached_execution_records_reject_duplicate_summary_fields() {
    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    let ArtifactValue::Semantic(SemanticValue::Map(entries)) = &mut value else {
        panic!("expected semantic execution summary");
    };
    entries.push((
        "stage".to_string(),
        SemanticValue::Str("duplicate".to_string()),
    ));

    let err = cached_execution_records(&value).unwrap_err();

    assert!(err
        .to_string()
        .contains("summary field 'stage' must be present exactly once"));
}

#[test]
fn cached_execution_records_reject_partial_summary_schema() {
    let record = cached_execution_record_fixture();
    let value = ArtifactValue::Semantic(
        SemanticValue::map([
            (
                "cache_written_at_unix_ns".to_string(),
                SemanticValue::Int(1_700_000_000_000_000_000),
            ),
            (
                "execution_summary".to_string(),
                SemanticValue::List(vec![
                    provider_execution_record_to_semantic_value(&record).unwrap()
                ]),
            ),
        ])
        .unwrap(),
    );

    let err = cached_execution_records(&value).unwrap_err();

    assert!(err
        .to_string()
        .contains("summary field 'stage' must be present exactly once"));
}

#[test]
fn cached_execution_records_reject_malformed_summary_fields() {
    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    set_cached_execution_summary_field(
        &mut value,
        "phase",
        SemanticValue::Str("compile-time".to_string()),
    );
    let err = cached_execution_records(&value).unwrap_err();
    assert!(err
        .to_string()
        .contains("summary field 'phase' must be one of: runtime, compile_time, dual"));

    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    set_cached_execution_summary_field(
        &mut value,
        "providers",
        SemanticValue::List(vec![
            SemanticValue::Str("p".to_string()),
            SemanticValue::Str("p".to_string()),
        ]),
    );
    let err = cached_execution_records(&value).unwrap_err();
    assert!(err
        .to_string()
        .contains("summary field 'providers' must be a list of unique non-empty strings"));
}

#[test]
fn cached_execution_records_reject_provider_count_mismatch() {
    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    set_cached_execution_summary_field(&mut value, "provider_count", SemanticValue::Int(2));

    let err = cached_execution_records(&value).unwrap_err();

    assert!(err
        .to_string()
        .contains("summary field 'provider_count' must be equal to providers length"));
}

#[test]
fn cached_execution_records_reject_summary_record_stage_mismatch() {
    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    set_cached_execution_summary_field(
        &mut value,
        "stage",
        SemanticValue::Str("other_stage".to_string()),
    );

    let err = cached_execution_records(&value).unwrap_err();

    assert!(err
        .to_string()
        .contains("summary field 'stage' must be consistent with all execution_summary stages"));
}

#[test]
fn cached_execution_records_reject_summary_record_provider_mismatch() {
    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    set_cached_execution_summary_field(
        &mut value,
        "providers",
        SemanticValue::List(vec![SemanticValue::Str("other_provider".to_string())]),
    );

    let err = cached_execution_records(&value).unwrap_err();

    assert!(err.to_string().contains(
        "summary field 'providers' must be consistent with execution_summary provider order",
    ));
}

#[test]
fn cached_execution_records_reject_summary_record_projection_mismatch() {
    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    set_cached_execution_summary_field(
        &mut value,
        "effect_tags",
        SemanticValue::List(vec![SemanticValue::Str("write_ir".to_string())]),
    );
    let err = cached_execution_records(&value).unwrap_err();
    assert!(err.to_string().contains(
        "summary field 'effect_tags' must be consistent with execution_summary effect tags",
    ));

    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    set_cached_execution_summary_field(
        &mut value,
        "read_cells",
        SemanticValue::List(vec![SemanticValue::Str("cell:stale".to_string())]),
    );
    let err = cached_execution_records(&value).unwrap_err();
    assert!(err.to_string().contains(
        "summary field 'read_cells' must be consistent with execution_summary projection",
    ));
}

#[test]
fn cached_execution_records_reject_malformed_record_fields() {
    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    let ArtifactValue::Semantic(SemanticValue::Map(entries)) = &mut value else {
        panic!("expected semantic execution summary");
    };
    let Some((_, SemanticValue::List(records))) = entries
        .iter_mut()
        .find(|(key, _)| key == "execution_summary")
    else {
        panic!("expected execution_summary");
    };
    let Some(SemanticValue::Map(record_entries)) = records.get_mut(0) else {
        panic!("expected execution record map");
    };
    let Some((_, provider_name)) = record_entries
        .iter_mut()
        .find(|(key, _)| key == "provider_name")
    else {
        panic!("expected provider_name");
    };
    *provider_name = SemanticValue::Int(1);

    let err = cached_execution_records(&value).unwrap_err();
    assert!(err
        .to_string()
        .contains("cached query execution record[0] field 'provider_name'"));
}

#[test]
fn cached_execution_records_reject_duplicate_record_fields() {
    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    let ArtifactValue::Semantic(SemanticValue::Map(entries)) = &mut value else {
        panic!("expected semantic execution summary");
    };
    let Some((_, SemanticValue::List(records))) = entries
        .iter_mut()
        .find(|(key, _)| key == "execution_summary")
    else {
        panic!("expected execution_summary");
    };
    let Some(SemanticValue::Map(record_entries)) = records.get_mut(0) else {
        panic!("expected execution record map");
    };
    record_entries.push((
        "provider_name".to_string(),
        SemanticValue::Str("duplicate".to_string()),
    ));

    let err = cached_execution_records(&value).unwrap_err();

    assert!(err
        .to_string()
        .contains("cached query execution record[0] field 'provider_name'"));
}

#[test]
fn cached_execution_records_reject_unknown_record_fields() {
    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    let ArtifactValue::Semantic(SemanticValue::Map(entries)) = &mut value else {
        panic!("expected semantic execution summary");
    };
    let Some((_, SemanticValue::List(records))) = entries
        .iter_mut()
        .find(|(key, _)| key == "execution_summary")
    else {
        panic!("expected execution_summary");
    };
    let Some(SemanticValue::Map(record_entries)) = records.get_mut(0) else {
        panic!("expected execution record map");
    };
    record_entries.push(("unexpected".to_string(), SemanticValue::Bool(true)));

    let err = cached_execution_records(&value).unwrap_err();

    assert!(err.to_string().contains("unknown field 'unexpected'"));
}

#[test]
fn cached_execution_records_reject_duplicate_structural_lists() {
    let record = cached_execution_record_fixture();
    let artifact = cached_execution_artifact_fixture(&record);
    let mut entries = match artifact {
        ArtifactValue::Semantic(SemanticValue::Map(entries)) => entries,
        _ => panic!("cached execution fixture must be semantic map"),
    };
    let summary = entries
        .iter_mut()
        .find(|(key, _)| key == "execution_summary")
        .expect("fixture has execution summary");
    if let SemanticValue::List(records) = &mut summary.1 {
        if let SemanticValue::Map(record_entries) = &mut records[0] {
            let effect_tags = record_entries
                .iter_mut()
                .find(|(key, _)| key == "effect_tags")
                .expect("fixture has effect tags");
            effect_tags.1 = SemanticValue::List(vec![
                SemanticValue::Str("write_ir".to_string()),
                SemanticValue::Str("write_ir".to_string()),
            ]);
        }
    }

    let err = cached_execution_records(&ArtifactValue::Semantic(SemanticValue::Map(entries)))
        .unwrap_err();

    assert!(err
        .to_string()
        .contains("cached query execution record[0] field 'effect_tags'"));
}

#[test]
fn cached_execution_records_explain_malformed_artifact_keys() {
    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    set_cached_execution_record_field(
        &mut value,
        "artifact_dependencies",
        SemanticValue::List(vec![SemanticValue::List(Vec::new())]),
    );

    let err = cached_execution_records(&value).unwrap_err();

    assert!(err
        .to_string()
        .contains("artifact key must contain at least one part"));
}

#[test]
fn cached_execution_records_reject_duplicate_outcome_summary_keys() {
    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    set_cached_execution_record_field(
        &mut value,
        "outcome_summary",
        SemanticValue::Map(vec![
            ("detail".to_string(), SemanticValue::Str("a".to_string())),
            ("detail".to_string(), SemanticValue::Str("b".to_string())),
        ]),
    );

    let err = cached_execution_records(&value).unwrap_err();

    let message = err.to_string();
    assert!(message.contains("outcome_summary"));
    assert!(message.contains("unique keys"));
}

#[test]
fn cached_execution_records_explain_invalid_scalar_constraints() {
    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    set_cached_execution_record_field(&mut value, "iteration", SemanticValue::Int(-1));
    let err = cached_execution_records(&value).unwrap_err();
    assert!(err.to_string().contains("a non-negative integer"));

    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    set_cached_execution_record_field(
        &mut value,
        "phase_policy",
        SemanticValue::Str("compile-time".to_string()),
    );
    let err = cached_execution_records(&value).unwrap_err();
    assert!(err
        .to_string()
        .contains("one of: runtime, compile_time, dual"));
}

#[test]
fn cached_execution_records_roundtrip_strict_summary() {
    let record = cached_execution_record_fixture();
    let value = cached_execution_artifact_fixture(&record);

    let records = cached_execution_records(&value).unwrap();
    assert_eq!(records, vec![record]);
}

#[test]
fn cached_execution_records_for_request_rejects_request_mismatch() {
    let record = cached_execution_record_fixture();
    let value = cached_execution_artifact_fixture(&record);

    let err = cached_execution_records_for_request(
        &value,
        CachedQueryStageReplayRequest {
            stage: "other_stage",
            phase: PhasePolicy::CompileTime,
            unit_id: "cached_unit",
        },
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("summary field 'stage' must be requested stage 'other_stage'"));

    let err = cached_execution_records_for_request(
        &value,
        CachedQueryStageReplayRequest {
            stage: "check",
            phase: PhasePolicy::Runtime,
            unit_id: "cached_unit",
        },
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("summary field 'phase' must be requested phase 'runtime'"));

    let err = cached_execution_records_for_request(
        &value,
        CachedQueryStageReplayRequest {
            stage: "check",
            phase: PhasePolicy::CompileTime,
            unit_id: "other_unit",
        },
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("summary field 'unit' must be requested unit 'other_unit'"));
}

#[test]
fn cached_execution_records_reject_unknown_cache_scope_and_resume_policy() {
    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    set_cached_execution_record_field(
        &mut value,
        "cache_scope",
        SemanticValue::Str("session".to_string()),
    );
    let err = cached_execution_records(&value).unwrap_err();
    assert!(err
        .to_string()
        .contains("cached query execution record[0] field 'cache_scope'"));

    let mut value = cached_execution_artifact_fixture(&cached_execution_record_fixture());
    set_cached_execution_record_field(
        &mut value,
        "resume_policy",
        SemanticValue::Str("restart".to_string()),
    );
    let err = cached_execution_records(&value).unwrap_err();
    assert!(err
        .to_string()
        .contains("cached query execution record[0] field 'resume_policy'"));
}

fn cached_execution_record_fixture() -> QueryProviderExecutionRecord {
    QueryProviderExecutionRecord {
        recorded_at_unix_ns: 1_700_000_000_000_000_000,
        provider_name: "p".to_string(),
        stage: "check".to_string(),
        family: None,
        phase_policy: PhasePolicy::CompileTime,
        effect_tags: EffectSet::empty(),
        requires: Vec::new(),
        requires_data: Vec::new(),
        provides_data: Vec::new(),
        provides: Vec::new(),
        reads: Vec::new(),
        writes: Vec::new(),
        reads_subjects: Vec::new(),
        writes_subjects: Vec::new(),
        read_cells: Vec::new(),
        write_cells: Vec::new(),
        reads_files: Vec::new(),
        writes_files: Vec::new(),
        artifact_dependencies: Vec::new(),
        cache_scope: QueryProviderCacheScope::Unit,
        resume_policy: QueryProviderResumePolicy::Safe,
        iteration: 3,
        changed: true,
        diagnostics_emitted: 0,
        rolled_back: false,
        stopped_by_error: false,
        outcome_kind: "success".to_string(),
        diagnostic_codes: Vec::new(),
        rewrite_count: 1,
        erased_count: 0,
        touched_node_kinds: vec!["call".to_string()],
        change_domains: vec!["ir".to_string()],
        restart_requested: false,
        restart_stage: None,
        outcome_summary: Vec::new(),
    }
}

fn cached_execution_artifact_fixture(record: &QueryProviderExecutionRecord) -> ArtifactValue {
    ArtifactValue::Semantic(
        SemanticValue::map([
            (
                "stage".to_string(),
                SemanticValue::Str(record.stage.clone()),
            ),
            (
                "unit".to_string(),
                SemanticValue::Str("cached_unit".to_string()),
            ),
            (
                "phase".to_string(),
                SemanticValue::Str(record.phase_policy.as_str().to_string()),
            ),
            ("unit_version".to_string(), SemanticValue::Int(1)),
            (
                "cache_written_at_unix_ns".to_string(),
                SemanticValue::Int(1_700_000_000_000_000_000),
            ),
            (
                "providers".to_string(),
                SemanticValue::List(vec![SemanticValue::Str(record.provider_name.clone())]),
            ),
            ("provider_count".to_string(), SemanticValue::Int(1)),
            (
                "effect_tags".to_string(),
                SemanticValue::List(
                    record
                        .effect_tags
                        .iter_strs()
                        .map(|value| SemanticValue::Str(value.to_string()))
                        .collect(),
                ),
            ),
            (
                "reads_subjects".to_string(),
                test_semantic_string_list(&record.reads_subjects),
            ),
            (
                "writes_subjects".to_string(),
                test_semantic_string_list(&record.writes_subjects),
            ),
            (
                "read_cells".to_string(),
                test_semantic_string_list(&record.read_cells),
            ),
            (
                "write_cells".to_string(),
                test_semantic_string_list(&record.write_cells),
            ),
            (
                "reads_files".to_string(),
                test_semantic_string_list(&record.reads_files),
            ),
            (
                "writes_files".to_string(),
                test_semantic_string_list(&record.writes_files),
            ),
            ("restarted".to_string(), SemanticValue::Bool(false)),
            ("restart_target".to_string(), SemanticValue::Null),
            (
                "artifact_key".to_string(),
                SemanticValue::Str(String::new()),
            ),
            (
                "execution_summary".to_string(),
                SemanticValue::List(vec![
                    provider_execution_record_to_semantic_value(record).unwrap()
                ]),
            ),
        ])
        .unwrap(),
    )
}

fn test_semantic_string_list(values: &[String]) -> SemanticValue {
    SemanticValue::List(values.iter().cloned().map(SemanticValue::Str).collect())
}

fn set_cached_execution_summary_field(
    artifact: &mut ArtifactValue,
    field: &str,
    value: SemanticValue,
) {
    let ArtifactValue::Semantic(SemanticValue::Map(entries)) = artifact else {
        panic!("expected semantic cached execution artifact");
    };
    let Some((_, existing)) = entries.iter_mut().find(|(key, _)| key == field) else {
        panic!("expected cached execution summary field {field}");
    };
    *existing = value;
}

fn set_cached_execution_record_field(
    artifact: &mut ArtifactValue,
    field: &str,
    value: SemanticValue,
) {
    let ArtifactValue::Semantic(SemanticValue::Map(entries)) = artifact else {
        panic!("expected semantic cached execution artifact");
    };
    let Some((_, SemanticValue::List(summary))) = entries
        .iter_mut()
        .find(|(key, _)| key == "execution_summary")
    else {
        panic!("expected cached execution summary");
    };
    let Some(SemanticValue::Map(record)) = summary.first_mut() else {
        panic!("expected cached execution record");
    };
    let Some((_, existing)) = record.iter_mut().find(|(key, _)| key == field) else {
        panic!("expected cached execution record field {field}");
    };
    *existing = value;
}

#[test]
fn query_stage_cache_key_tracks_session_state_versions() {
    let unit = Unit::empty("cache_key_unit").unwrap();
    let base = test_query_stage_cache_key(&unit, &[], test_cache_versions(1, 1)).unwrap();
    let changed_capabilities =
        test_query_stage_cache_key(&unit, &[], test_cache_versions(2, 1)).unwrap();
    let changed_images = test_query_stage_cache_key(&unit, &[], test_cache_versions(1, 2)).unwrap();

    assert_ne!(base, changed_capabilities);
    assert_ne!(base, changed_images);
}

#[test]
fn initial_bindings_with_host_values_disable_cache_keys() {
    let unit = Unit::empty("opaque_binding_unit").unwrap();
    let host_function = RuntimeValue::HostFunction(Rc::new(
        HostFunction::new(
            "opaque.host",
            0,
            Some(0),
            Box::new(|_| Ok(RuntimeValue::Null)),
        )
        .unwrap(),
    ));
    let initial_bindings = vec![("opaque".to_string(), host_function)];

    let key =
        test_query_stage_cache_key(&unit, &initial_bindings, test_cache_versions(1, 1)).unwrap();

    assert_eq!(key, None);
    assert_eq!(initial_bindings_identity_token(&initial_bindings), None);
}

fn test_query_stage_cache_key(
    unit: &Unit,
    initial_bindings: &[(String, RuntimeValue)],
    versions: QueryStageCacheVersions,
) -> CaapResult<Option<ArtifactKey>> {
    query_stage_cache_key(QueryStageCacheKeyInput {
        unit,
        stage: "check",
        phase: PhasePolicy::CompileTime,
        initial_bindings,
        versions,
    })
}

fn test_cache_versions(bootstrap_capability: u64, bootstrap_image: u64) -> QueryStageCacheVersions {
    QueryStageCacheVersions {
        provider_registry: 1,
        compiler_registry: 1,
        host: 1,
        bootstrap_capability,
        bootstrap_image,
    }
}
