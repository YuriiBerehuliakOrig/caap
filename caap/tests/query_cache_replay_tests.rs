/// Integration tests for query cache keys, CTFE replay, and cache invalidation.
///
/// These scenarios validate query cache/replay behavior independently from
/// provider ordering and restart planning.
use caap_core::{frontend::parse, RuntimeValue, Unit};
use std::rc::Rc;

mod common;

#[test]
fn test_query_atomic_transaction_rolls_back_unit_and_cache_on_error() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    compiler
        .register_stage_spec(caap_core::QueryStageSpec::new("lower").unwrap())
        .unwrap();
    compiler
        .register_stage_spec(
            caap_core::QueryStageSpec::new("validate")
                .unwrap()
                .with_requires(["lower".to_string()])
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_provider_with_effects(
            "lower_provider",
            "lower",
            caap_core::PhasePolicy::CompileTime,
            ["write_attributes".to_string()],
            |context| context.set_unit_attribute("lowered", caap_core::SemanticValue::Bool(true)),
        )
        .unwrap();
    compiler
        .register_provider_with_effects(
            "validate_provider",
            "validate",
            caap_core::PhasePolicy::CompileTime,
            ["write_attributes".to_string()],
            |context| {
                context.set_unit_attribute("validated", caap_core::SemanticValue::Bool(false))?;
                Err("validation failed".to_string())
            },
        )
        .unwrap();

    let err = compiler
        .queries()
        .query_with_transaction_mode(
            "validate",
            &mut unit,
            caap_core::PhasePolicy::CompileTime,
            caap_core::QueryTransactionMode::AtomicUnit,
        )
        .expect_err("atomic query should propagate provider failure");

    assert_eq!(err.to_string(), "compiler error: validation failed");
    assert!(unit.attributes().get("lowered").is_none());
    assert!(unit.attributes().get("validated").is_none());
    assert_eq!(compiler.artifact_cache().stats().generation, 0);
    assert!(compiler.active_provider_context().is_none());
    assert!(!compiler.catalog().contains_unit("main"));
}

#[test]
fn test_query_atomic_transaction_rolls_back_provider_ctfe_cache() {
    let mut compiler = common::session();
    let mut unit = Unit::from_graph("main", parse("(int_add 1 2)").unwrap()).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));

    compiler
        .register_stage_spec(caap_core::QueryStageSpec::new("validate").unwrap())
        .unwrap();
    let mut spec = caap_core::QueryProviderRegistrationSpec::new();
    spec.cache_scope = "unit".to_string();
    spec.writes = vec!["attributes".to_string()];
    compiler
        .register_provider_contract(
            "cacheable_writer",
            "validate",
            None,
            caap_core::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            vec!["write_attributes".to_string()],
            spec,
            {
                let runs = Rc::clone(&runs);
                move |context| {
                    runs.set(runs.get() + 1);
                    context.set_unit_attribute("rolled_back", caap_core::SemanticValue::Bool(false))
                }
            },
        )
        .unwrap();
    compiler
        .register_provider(
            "failing_provider",
            "validate",
            caap_core::PhasePolicy::CompileTime,
            |_context| Err("validation failed".to_string()),
        )
        .unwrap();

    for _ in 0..2 {
        compiler
            .queries()
            .query_with_transaction_mode(
                "validate",
                &mut unit,
                caap_core::PhasePolicy::CompileTime,
                caap_core::QueryTransactionMode::AtomicUnit,
            )
            .expect_err("atomic query should fail");
        assert!(unit.attributes().get("rolled_back").is_none());
    }

    assert_eq!(runs.get(), 2);
}

#[test]
fn test_query_provider_rejects_unknown_cache_scope_and_resume_policy() {
    let host = caap_core::CompilerHost::new();

    let mut compiler = host.new_session();
    compiler
        .register_stage_spec(caap_core::QueryStageSpec::new("validate").unwrap())
        .unwrap();
    let cache_err = compiler
        .register_provider_contract(
            "bad_cache_provider",
            "validate",
            None,
            caap_core::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            Vec::<String>::new(),
            caap_core::QueryProviderRegistrationSpec {
                cache_scope: "session".to_string(),
                ..Default::default()
            },
            |_context| Ok(()),
        )
        .unwrap_err();
    assert!(cache_err
        .to_string()
        .contains("unsupported query provider cache_scope"));

    let mut compiler = host.new_session();
    compiler
        .register_stage_spec(caap_core::QueryStageSpec::new("validate").unwrap())
        .unwrap();
    let resume_err = compiler
        .register_provider_contract(
            "bad_resume_provider",
            "validate",
            None,
            caap_core::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            Vec::<String>::new(),
            caap_core::QueryProviderRegistrationSpec {
                resume_policy: "restart".to_string(),
                ..Default::default()
            },
            |_context| Ok(()),
        )
        .unwrap_err();
    assert!(resume_err
        .to_string()
        .contains("unsupported query provider resume_policy"));
}

#[test]
fn test_query_service_replays_exact_stage_cache_without_provider_rerun() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider(
            "count_provider",
            "analyze",
            caap_core::PhasePolicy::CompileTime,
            {
                let runs = Rc::clone(&runs);
                move |_context| {
                    runs.set(runs.get() + 1);
                    Ok(())
                }
            },
        )
        .unwrap();

    let first = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();
    let second = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();

    assert_eq!(runs.get(), 1);
    assert!(!first.steps[0].cached);
    assert!(second.steps[0].cached);
    assert!(first.steps[0].artifact_key.is_some());
    assert_eq!(first.steps[0].artifact_key, second.steps[0].artifact_key);
    let artifact_key = first.steps[0].artifact_key.as_ref().unwrap();
    let artifact = compiler
        .artifact_cache()
        .peek(artifact_key)
        .expect("query stage should store a semantic artifact");
    let entries = match artifact {
        caap_core::ArtifactValue::Semantic(caap_core::SemanticValue::Map(entries)) => entries,
        caap_core::ArtifactValue::QueryStage(cached) => match &cached.summary {
            caap_core::SemanticValue::Map(entries) => entries,
            _ => panic!("expected semantic query artifact"),
        },
        _ => panic!("expected semantic query artifact"),
    };
    assert!(entries.contains(&(
        "provider_count".to_string(),
        caap_core::SemanticValue::Int(1)
    )));
    assert!(entries.contains(&(
        "providers".to_string(),
        caap_core::SemanticValue::List(vec![caap_core::SemanticValue::Str(
            "count_provider".to_string()
        )])
    )));
    assert!(entries.contains(&(
        "artifact_key".to_string(),
        caap_core::SemanticValue::Str(artifact_key.to_string())
    )));
    assert_eq!(compiler.artifact_cache().stats().hits, 1);
    assert_eq!(
        compiler.events().by_kind("query.stage.cache_hit").unwrap()[0]
            .target
            .as_deref(),
        Some("analyze")
    );
}

#[test]
fn test_query_stage_cache_hit_replays_unit_snapshot() {
    let mut compiler = common::session();
    let mut first_unit = Unit::from_graph("main", parse("(int_add 1 2)").unwrap()).unwrap();
    let mut second_unit = Unit::from_graph("main", parse("(int_add 1 2)").unwrap()).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_with_effects(
            "annotate_provider",
            "analyze",
            caap_core::PhasePolicy::CompileTime,
            ["write_attributes".to_string()],
            {
                let runs = Rc::clone(&runs);
                move |context| {
                    runs.set(runs.get() + 1);
                    context.set_unit_attribute("stage_replayed", caap_core::SemanticValue::Int(42))
                }
            },
        )
        .unwrap();

    let first = compiler
        .queries()
        .query(
            "analyze",
            &mut first_unit,
            caap_core::PhasePolicy::CompileTime,
        )
        .unwrap();
    let second = compiler
        .queries()
        .query(
            "analyze",
            &mut second_unit,
            caap_core::PhasePolicy::CompileTime,
        )
        .unwrap();

    assert_eq!(runs.get(), 1);
    assert!(!first.steps[0].cached);
    assert!(second.steps[0].cached);
    assert_eq!(
        second_unit.attributes().get("stage_replayed"),
        Some(&caap_core::SemanticValue::Int(42))
    );
}

#[test]
fn test_query_stage_cache_key_tracks_unit_content_fingerprint() {
    let mut compiler = common::session();
    let mut first_unit = Unit::from_graph("main", parse("(int_add 1 2)").unwrap()).unwrap();
    let mut second_unit = Unit::from_graph("main", parse("(int_add 1 3)").unwrap()).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));

    assert_eq!(first_unit.version(), second_unit.version());
    assert_ne!(
        first_unit.content_fingerprint().unwrap(),
        second_unit.content_fingerprint().unwrap()
    );

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider(
            "count_provider",
            "analyze",
            caap_core::PhasePolicy::CompileTime,
            {
                let runs = Rc::clone(&runs);
                move |_context| {
                    runs.set(runs.get() + 1);
                    Ok(())
                }
            },
        )
        .unwrap();

    let first = compiler
        .queries()
        .query(
            "analyze",
            &mut first_unit,
            caap_core::PhasePolicy::CompileTime,
        )
        .unwrap();
    let second = compiler
        .queries()
        .query(
            "analyze",
            &mut second_unit,
            caap_core::PhasePolicy::CompileTime,
        )
        .unwrap();

    assert_eq!(runs.get(), 2);
    assert!(!first.steps[0].cached);
    assert!(!second.steps[0].cached);
    assert_ne!(first.steps[0].artifact_key, second.steps[0].artifact_key);
}

#[test]
fn test_query_stage_cache_key_tracks_provider_registry_version() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let first_runs = Rc::new(std::cell::Cell::new(0));
    let second_runs = Rc::new(std::cell::Cell::new(0));

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider(
            "first_provider",
            "analyze",
            caap_core::PhasePolicy::CompileTime,
            {
                let first_runs = Rc::clone(&first_runs);
                move |_context| {
                    first_runs.set(first_runs.get() + 1);
                    Ok(())
                }
            },
        )
        .unwrap();

    let first = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();

    compiler
        .register_provider(
            "second_provider",
            "analyze",
            caap_core::PhasePolicy::CompileTime,
            {
                let second_runs = Rc::clone(&second_runs);
                move |_context| {
                    second_runs.set(second_runs.get() + 1);
                    Ok(())
                }
            },
        )
        .unwrap();

    let second = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();

    assert_eq!(first_runs.get(), 2);
    assert_eq!(second_runs.get(), 1);
    assert!(!first.steps[0].cached);
    assert!(!second.steps[0].cached);
    assert_ne!(first.steps[0].artifact_key, second.steps[0].artifact_key);
    assert_eq!(
        second.steps[0].provider_names,
        vec!["first_provider".to_string(), "second_provider".to_string()]
    );
}

#[test]
fn test_query_service_replays_provider_ctfe_cache_when_stage_artifact_is_dirty() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));
    let provider_runs = runs.clone();

    compiler.register_stage("analyze").unwrap();
    let mut spec = caap_core::QueryProviderRegistrationSpec::new();
    spec.cache_scope = "unit".to_string();
    compiler
        .register_provider_contract(
            "cacheable_provider",
            "analyze",
            None,
            caap_core::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            Vec::<String>::new(),
            spec,
            move |_context| {
                provider_runs.set(provider_runs.get() + 1);
                Ok(())
            },
        )
        .unwrap();

    let first = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();
    let artifact_key = first.steps[0].artifact_key.as_ref().unwrap().clone();
    compiler
        .artifact_cache_mut()
        .mark_dirty(
            caap_core::ArtifactInvalidationRecord::new("force_stage_cache_miss", artifact_key)
                .unwrap(),
        )
        .unwrap();

    let second = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();

    assert_eq!(runs.get(), 1);
    assert_eq!(second.executed.len(), 1);
    assert_eq!(second.executed[0].provider_name, "cacheable_provider");
    assert_eq!(second.executed[0].outcome_kind, "cached");
    assert_eq!(
        compiler
            .events()
            .by_kind("query.provider.cache_hit")
            .unwrap()[0]
            .target
            .as_deref(),
        Some("cacheable_provider")
    );
}

#[test]
fn test_query_service_provider_ctfe_cache_replays_unit_snapshot() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut first_unit = Unit::from_graph("main", graph.clone()).unwrap();
    let mut second_unit = Unit::from_graph("main", graph).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));
    let provider_runs = runs.clone();

    compiler.register_stage("analyze").unwrap();
    let mut spec = caap_core::QueryProviderRegistrationSpec::new();
    spec.cache_scope = "unit".to_string();
    spec.writes = vec!["attributes".to_string()];
    compiler
        .register_provider_contract(
            "snapshot_provider",
            "analyze",
            None,
            caap_core::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            vec!["write_attributes".to_string()],
            spec,
            move |context| {
                provider_runs.set(provider_runs.get() + 1);
                context.set_unit_attribute("from_provider", caap_core::SemanticValue::Int(42))
            },
        )
        .unwrap();

    let first = compiler
        .queries()
        .query(
            "analyze",
            &mut first_unit,
            caap_core::PhasePolicy::CompileTime,
        )
        .unwrap();
    let artifact_key = first.steps[0].artifact_key.as_ref().unwrap().clone();
    compiler
        .artifact_cache_mut()
        .mark_dirty(
            caap_core::ArtifactInvalidationRecord::new("force_stage_cache_miss", artifact_key)
                .unwrap(),
        )
        .unwrap();

    let second = compiler
        .queries()
        .query(
            "analyze",
            &mut second_unit,
            caap_core::PhasePolicy::CompileTime,
        )
        .unwrap();

    assert_eq!(runs.get(), 1);
    assert_eq!(second.executed[0].outcome_kind, "cached");
    assert_eq!(
        second_unit.attributes().get("from_provider"),
        Some(&caap_core::SemanticValue::Int(42))
    );
}

#[test]
fn test_query_service_read_only_provider_cache_does_not_replay_unit_snapshot() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut first_unit = Unit::from_graph("main", graph.clone()).unwrap();
    let mut second_unit = Unit::from_graph("main", graph).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));
    let provider_runs = runs.clone();
    let mut spec = caap_core::QueryProviderRegistrationSpec::new();
    spec.cache_scope = "unit".to_string();

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_contract_with_outcome(
            "reported_change_readonly_provider",
            "analyze",
            None,
            caap_core::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            vec!["write_attributes".to_string()],
            spec,
            move |context| {
                provider_runs.set(provider_runs.get() + 1);
                context.set_unit_attribute(
                    "should_not_replay",
                    caap_core::SemanticValue::Bool(true),
                )?;
                Ok(caap_core::QueryProviderCallbackOutcome::changed(true))
            },
        )
        .unwrap();

    let first = compiler
        .queries()
        .query(
            "analyze",
            &mut first_unit,
            caap_core::PhasePolicy::CompileTime,
        )
        .unwrap();
    let artifact_key = first.steps[0].artifact_key.as_ref().unwrap().clone();
    compiler
        .artifact_cache_mut()
        .mark_dirty(
            caap_core::ArtifactInvalidationRecord::new("force_stage_cache_miss", artifact_key)
                .unwrap(),
        )
        .unwrap();

    let second = compiler
        .queries()
        .query(
            "analyze",
            &mut second_unit,
            caap_core::PhasePolicy::CompileTime,
        )
        .unwrap();

    assert_eq!(runs.get(), 1);
    assert_eq!(second.executed[0].outcome_kind, "cached");
    assert!(second.executed[0].changed);
    assert!(first_unit.attributes().contains_key("should_not_replay"));
    assert!(second_unit.attributes().get("should_not_replay").is_none());
}

#[test]
fn test_query_service_file_reading_provider_is_not_ctfe_cached() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));
    let provider_runs = runs.clone();
    let mut spec = caap_core::QueryProviderRegistrationSpec::new();
    spec.cache_scope = "unit".to_string();
    spec.reads = vec!["files".to_string()];

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_contract(
            "file_reading_provider",
            "analyze",
            None,
            caap_core::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            vec!["read_files".to_string()],
            spec,
            move |_context| {
                provider_runs.set(provider_runs.get() + 1);
                Ok(())
            },
        )
        .unwrap();

    let first = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();
    let artifact_key = first.steps[0].artifact_key.as_ref().unwrap().clone();
    compiler
        .artifact_cache_mut()
        .mark_dirty(
            caap_core::ArtifactInvalidationRecord::new("force_stage_cache_miss", artifact_key)
                .unwrap(),
        )
        .unwrap();

    let second = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();

    assert_eq!(runs.get(), 2);
    assert_eq!(second.executed[0].provider_name, "file_reading_provider");
    assert_ne!(second.executed[0].outcome_kind, "cached");
}

#[test]
fn test_query_stage_cache_key_tracks_initial_bindings() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("initial_cache", graph).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider(
            "count_initial_provider",
            "analyze",
            caap_core::PhasePolicy::CompileTime,
            {
                let runs = Rc::clone(&runs);
                move |_context| {
                    runs.set(runs.get() + 1);
                    Ok(())
                }
            },
        )
        .unwrap();

    let first = compiler
        .queries()
        .query_with_options(
            "analyze",
            &mut unit,
            caap_core::PhasePolicy::CompileTime,
            caap_core::QueryExecutionOptions::new()
                .with_initial_bindings([("env".to_string(), RuntimeValue::Str("one".into()))]),
        )
        .unwrap();
    let second = compiler
        .queries()
        .query_with_options(
            "analyze",
            &mut unit,
            caap_core::PhasePolicy::CompileTime,
            caap_core::QueryExecutionOptions::new()
                .with_initial_bindings([("env".to_string(), RuntimeValue::Str("two".into()))]),
        )
        .unwrap();
    let third = compiler
        .queries()
        .query_with_options(
            "analyze",
            &mut unit,
            caap_core::PhasePolicy::CompileTime,
            caap_core::QueryExecutionOptions::new()
                .with_initial_bindings([("env".to_string(), RuntimeValue::Str("one".into()))]),
        )
        .unwrap();

    assert_eq!(runs.get(), 2);
    assert_ne!(first.steps[0].artifact_key, second.steps[0].artifact_key);
    assert_eq!(first.steps[0].artifact_key, third.steps[0].artifact_key);
    assert!(!first.steps[0].cached);
    assert!(!second.steps[0].cached);
    assert!(third.steps[0].cached);
}
