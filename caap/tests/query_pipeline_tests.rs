/// Integration tests for query planning, provider ordering, transactions, and restarts.
use caap_core::{frontend::parse, Unit};
use std::rc::Rc;

mod common;

#[test]
fn test_query_provider_requires_schedule_before_dependents() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let order = Rc::new(std::cell::RefCell::new(Vec::<String>::new()));

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_contract(
            "consumer",
            "analyze",
            Some("analysis".to_string()),
            caap_core::PhasePolicy::CompileTime,
            ["producer".to_string()],
            Vec::<String>::new(),
            caap_core::QueryProviderRegistrationSpec::new(),
            {
                let order = Rc::clone(&order);
                move |_context| {
                    order.borrow_mut().push("consumer".to_string());
                    Ok(())
                }
            },
        )
        .unwrap();
    compiler
        .register_provider_contract(
            "producer",
            "analyze",
            Some("analysis".to_string()),
            caap_core::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            Vec::<String>::new(),
            caap_core::QueryProviderRegistrationSpec::new(),
            {
                let order = Rc::clone(&order);
                move |_context| {
                    order.borrow_mut().push("producer".to_string());
                    Ok(())
                }
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();

    assert_eq!(plan.steps[0].provider_names, vec!["producer", "consumer"]);
    assert_eq!(
        &*order.borrow(),
        &vec!["producer".to_string(), "consumer".to_string()]
    );
    assert_eq!(
        plan.executed
            .iter()
            .map(|record| record.provider_name.as_str())
            .collect::<Vec<_>>(),
        vec!["producer", "consumer"]
    );
}

#[test]
fn test_query_provider_requires_missing_provider_fails_before_execution() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let ran = Rc::new(std::cell::Cell::new(false));

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_contract(
            "consumer",
            "analyze",
            Some("analysis".to_string()),
            caap_core::PhasePolicy::CompileTime,
            ["missing".to_string()],
            Vec::<String>::new(),
            caap_core::QueryProviderRegistrationSpec::new(),
            {
                let ran = Rc::clone(&ran);
                move |_context| {
                    ran.set(true);
                    Ok(())
                }
            },
        )
        .unwrap();

    let err = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .expect_err("missing provider requirement should fail planning");

    assert!(err.to_string().contains("requires missing provider"));
    assert!(!ran.get());
}

#[test]
fn test_query_provider_receives_active_context_only_during_callback() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let captured = Rc::new(std::cell::RefCell::new(None));

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_with_effects(
            "context_provider",
            "analyze",
            caap_core::PhasePolicy::CompileTime,
            ["write_attributes".to_string()],
            {
                let captured = Rc::clone(&captured);
                move |context| {
                    *captured.borrow_mut() = context.active_provider_context().cloned();
                    context.set_unit_attribute("context_seen", caap_core::SemanticValue::Bool(true))
                }
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();

    let context = captured
        .borrow()
        .clone()
        .expect("provider context should be visible during callback");
    assert_eq!(context.provider, "context_provider");
    assert_eq!(context.stage, "analyze");
    assert_eq!(context.phase, caap_core::PhasePolicy::CompileTime);
    assert_eq!(context.unit_id, "main");
    assert_eq!(context.registration_index, 0);
    assert!(compiler.active_provider_context().is_none());
    assert_eq!(
        unit.attributes().get("context_seen"),
        Some(&caap_core::SemanticValue::Bool(true))
    );
    assert!(plan.executed[0]
        .write_cells
        .contains(&"unit:main@attributes".to_string()));
}

#[test]
fn test_query_provider_context_is_restored_after_error() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider(
            "failing_provider",
            "analyze",
            caap_core::PhasePolicy::CompileTime,
            |context| {
                assert_eq!(
                    context
                        .active_provider_context()
                        .map(|context| context.provider.as_str()),
                    Some("failing_provider")
                );
                Err("provider failed".to_string())
            },
        )
        .unwrap();

    let err = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .expect_err("query should propagate provider failure");

    assert_eq!(err.to_string(), "compiler error: provider failed");
    assert!(compiler.active_provider_context().is_none());
}

#[test]
fn test_query_mutating_provider_rolls_back_unit_on_error() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    compiler.register_stage("analyze").unwrap();
    let mut spec = caap_core::QueryProviderRegistrationSpec::new();
    spec.writes = vec!["attributes".to_string()];
    compiler
        .register_provider_contract(
            "failing_mutator",
            "analyze",
            None,
            caap_core::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            vec![
                "write_attributes".to_string(),
                "emit_diagnostics".to_string(),
            ],
            spec,
            |context| {
                context.set_unit_attribute("partial", caap_core::SemanticValue::Bool(true))?;
                Err("provider failed after mutation".to_string())
            },
        )
        .unwrap();

    let err = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .expect_err("query should propagate provider failure");

    assert_eq!(
        err.to_string(),
        "compiler error: provider failed after mutation"
    );
    assert!(unit.attributes().get("partial").is_none());
    assert!(compiler.active_provider_context().is_none());
}

#[test]
fn test_query_error_diagnostic_rolls_back_and_stops_pipeline() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let after_runs = Rc::new(std::cell::Cell::new(0));
    let after_runs_provider = after_runs.clone();

    compiler.register_stage("analyze").unwrap();
    let mut spec = caap_core::QueryProviderRegistrationSpec::new();
    spec.writes = vec!["attributes".to_string()];
    compiler
        .register_provider_contract(
            "diagnostic_mutator",
            "analyze",
            None,
            caap_core::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            vec![
                "write_attributes".to_string(),
                "emit_diagnostics".to_string(),
            ],
            spec,
            |context| {
                context.set_unit_attribute("partial", caap_core::SemanticValue::Bool(true))?;
                context.push_diagnostic(
                    caap_core::Diagnostic::error("provider emitted an error")
                        .unwrap()
                        .with_code("demo.provider.error")
                        .unwrap(),
                )?;
                Ok(())
            },
        )
        .unwrap();
    compiler
        .register_provider(
            "after_error_provider",
            "analyze",
            caap_core::PhasePolicy::CompileTime,
            move |_context| {
                after_runs_provider.set(after_runs_provider.get() + 1);
                Ok(())
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();

    assert_eq!(compiler.diagnostics().len(), 1);
    assert!(unit.attributes().get("partial").is_none());
    assert_eq!(after_runs.get(), 0);
    assert_eq!(plan.executed.len(), 1);
    assert_eq!(plan.executed[0].provider_name, "diagnostic_mutator");
    assert!(plan.executed[0].rolled_back);
    assert!(plan.executed[0].stopped_by_error);
    assert_eq!(plan.executed[0].outcome_kind, "stopped_by_error");
    assert_eq!(
        plan.executed[0].diagnostic_codes,
        vec!["demo.provider.error".to_string()]
    );
    let artifact_key = plan.steps[0]
        .artifact_key
        .as_ref()
        .expect("stage should compute an artifact key");
    assert!(
        compiler.artifact_cache().peek(artifact_key).is_some(),
        "stopped-by-error stage artifact should remain available to the current query"
    );

    let second_plan = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();
    assert!(!second_plan.steps[0].cached);
    assert_eq!(compiler.diagnostics().len(), 2);
    assert_eq!(after_runs.get(), 0);
}

#[test]
fn test_query_stage_alias_resolves_to_registered_stage() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    compiler.register_stage("compile_unit").unwrap();
    compiler
        .register_stage_alias("compile_unit", "compile")
        .unwrap();
    compiler
        .register_provider_with_effects(
            "alias_provider",
            "compile",
            caap_core::PhasePolicy::CompileTime,
            ["write_attributes".to_string()],
            |context| context.set_unit_attribute("alias_ran", caap_core::SemanticValue::Bool(true)),
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query("compile", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();

    assert_eq!(plan.target, "compile_unit");
    assert_eq!(plan.steps[0].provider_names, vec!["alias_provider"]);
    assert_eq!(
        unit.attributes().get("alias_ran"),
        Some(&caap_core::SemanticValue::Bool(true))
    );
}

#[test]
fn test_query_plan_routes_stage_dependencies_before_target() {
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
                .unwrap()
                .with_aliases(["compile".to_string()])
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
            [
                "read_attributes".to_string(),
                "write_attributes".to_string(),
            ],
            |context| {
                assert_eq!(
                    context.unit_attribute("lowered")?,
                    Some(caap_core::SemanticValue::Bool(true))
                );
                context.set_unit_attribute("validated", caap_core::SemanticValue::Bool(true))
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query("compile", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();

    assert_eq!(plan.target, "validate");
    assert_eq!(
        plan.steps
            .iter()
            .map(|step| step.stage.as_str())
            .collect::<Vec<_>>(),
        vec!["lower", "validate"]
    );
    assert_eq!(plan.steps[0].provider_names, vec!["lower_provider"]);
    assert_eq!(plan.steps[1].provider_names, vec!["validate_provider"]);
    assert_eq!(
        unit.attributes().get("validated"),
        Some(&caap_core::SemanticValue::Bool(true))
    );
    assert!(plan.executed[0]
        .write_cells
        .contains(&"unit:main@attributes".to_string()));
    assert!(plan.executed[1]
        .read_cells
        .contains(&"unit:main@attributes".to_string()));
    assert!(plan.executed[1]
        .write_cells
        .contains(&"unit:main@attributes".to_string()));
}

#[test]
fn test_query_provider_can_request_bounded_restart_from_stage() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let validate_runs = Rc::new(std::cell::Cell::new(0));

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
            [
                "write_attributes".to_string(),
                "request_restart".to_string(),
            ],
            {
                let validate_runs = Rc::clone(&validate_runs);
                move |context| {
                    validate_runs.set(validate_runs.get() + 1);
                    if validate_runs.get() == 1 {
                        context.request_query_restart("lower")?;
                    }
                    context.set_unit_attribute("validated", caap_core::SemanticValue::Bool(true))
                }
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query("validate", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();

    assert_eq!(validate_runs.get(), 2);
    assert_eq!(
        plan.steps
            .iter()
            .map(|step| step.stage.as_str())
            .collect::<Vec<_>>(),
        vec!["lower", "validate", "lower", "validate"]
    );
    assert_eq!(plan.steps[1].restart_target.as_deref(), Some("lower"));
    assert!(plan.steps[2].restarted);
    assert!(plan.steps[3].restarted);
    assert_eq!(
        compiler.events().by_kind("query.restart").unwrap()[0]
            .target
            .as_deref(),
        Some("lower")
    );
}

#[test]
fn test_query_provider_requires_request_restart_effect() {
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
        .register_provider(
            "lower_provider",
            "lower",
            caap_core::PhasePolicy::CompileTime,
            |_context| Ok(()),
        )
        .unwrap();
    compiler
        .register_provider_with_effects(
            "validate_provider",
            "validate",
            caap_core::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            |context| context.request_query_restart("lower"),
        )
        .unwrap();

    let error = compiler
        .queries()
        .query("validate", &mut unit, caap_core::PhasePolicy::CompileTime)
        .expect_err("provider restart requests must declare request_restart");

    assert!(error
        .to_string()
        .contains("does not declare required effect request_restart"));
    assert!(
        compiler
            .events()
            .by_kind("query.restart")
            .unwrap()
            .is_empty(),
        "unauthorized restart request must not emit restart event"
    );
}

#[test]
fn test_query_changed_provider_uses_stage_restart_policy() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let validate_runs = Rc::new(std::cell::Cell::new(0));

    compiler
        .register_stage_spec(caap_core::QueryStageSpec::new("lower").unwrap())
        .unwrap();
    compiler
        .register_stage_spec(
            caap_core::QueryStageSpec::new("validate")
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
            caap_core::PhasePolicy::CompileTime,
            |_context| Ok(()),
        )
        .unwrap();
    compiler
        .register_provider_contract_with_outcome(
            "validate_provider",
            "validate",
            None,
            caap_core::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            vec!["write_attributes".to_string()],
            caap_core::QueryProviderRegistrationSpec::default(),
            {
                let validate_runs = Rc::clone(&validate_runs);
                move |context| {
                    validate_runs.set(validate_runs.get() + 1);
                    if validate_runs.get() == 1 {
                        context.set_unit_attribute(
                            "validated",
                            caap_core::SemanticValue::Bool(true),
                        )?;
                        Ok(caap_core::QueryProviderCallbackOutcome::changed(true))
                    } else {
                        Ok(caap_core::QueryProviderCallbackOutcome::changed(false))
                    }
                }
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query("validate", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();

    assert_eq!(validate_runs.get(), 2);
    assert_eq!(
        plan.steps
            .iter()
            .map(|step| step.stage.as_str())
            .collect::<Vec<_>>(),
        vec!["lower", "validate", "lower", "validate"]
    );
    assert_eq!(plan.steps[1].restart_target.as_deref(), Some("lower"));
    assert!(plan.executed[1].restart_requested);
    assert_eq!(plan.executed[1].restart_stage.as_deref(), Some("lower"));
    assert!(plan.steps[2].restarted);
    assert!(plan.steps[3].restarted);
}

#[test]
fn test_query_resume_policy_never_suppresses_stage_restart_policy() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let spec = caap_core::QueryProviderRegistrationSpec {
        resume_policy: "never".to_string(),
        ..Default::default()
    };

    compiler
        .register_stage_spec(caap_core::QueryStageSpec::new("lower").unwrap())
        .unwrap();
    compiler
        .register_stage_spec(
            caap_core::QueryStageSpec::new("validate")
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
            caap_core::PhasePolicy::CompileTime,
            |_context| Ok(()),
        )
        .unwrap();
    compiler
        .register_provider_contract(
            "validate_provider",
            "validate",
            None,
            caap_core::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            vec!["write_attributes".to_string()],
            spec,
            |context| context.set_unit_attribute("validated", caap_core::SemanticValue::Bool(true)),
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query("validate", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();

    assert_eq!(
        plan.steps
            .iter()
            .map(|step| step.stage.as_str())
            .collect::<Vec<_>>(),
        vec!["lower", "validate"]
    );
    assert_eq!(plan.steps[1].restart_target, None);
    assert!(!plan.executed[1].restart_requested);
}

#[test]
fn test_query_restart_limit_can_be_disabled_explicitly() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let validate_runs = Rc::new(std::cell::Cell::new(0));

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
            [
                "write_attributes".to_string(),
                "request_restart".to_string(),
            ],
            {
                let validate_runs = Rc::clone(&validate_runs);
                move |context| {
                    validate_runs.set(validate_runs.get() + 1);
                    context.request_query_restart("lower")?;
                    context.set_unit_attribute("validated", caap_core::SemanticValue::Bool(true))
                }
            },
        )
        .unwrap();

    let err = compiler
        .queries()
        .query_with_options(
            "validate",
            &mut unit,
            caap_core::PhasePolicy::CompileTime,
            caap_core::QueryExecutionOptions::new().with_restart_limit(0),
        )
        .expect_err("disabled restart budget should reject provider restart requests");

    assert!(err.to_string().contains("query restart budget exhausted"));
    assert_eq!(validate_runs.get(), 1);
    assert_eq!(
        unit.attributes().get("validated"),
        Some(&caap_core::SemanticValue::Bool(true))
    );
}

#[test]
fn test_query_plan_reports_missing_stage_dependency() {
    let mut compiler = common::session();
    compiler
        .register_stage_spec(
            caap_core::QueryStageSpec::new("validate")
                .unwrap()
                .with_requires(["lower".to_string()])
                .unwrap(),
        )
        .unwrap();

    let err = compiler
        .queries()
        .plan_query("validate", caap_core::PhasePolicy::CompileTime)
        .expect_err("missing dependency should fail planning");

    assert!(err.to_string().contains("depends on missing stage"));
}
