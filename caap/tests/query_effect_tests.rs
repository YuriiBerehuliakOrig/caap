/// Integration tests for query provider effect and capability enforcement.
///
/// These scenarios validate declared effects, effect allowlists, and semantic
/// transaction behavior independently from query routing and cache replay.
use caap_core::{frontend::parse, Unit};
use std::rc::Rc;

mod common;

#[test]
fn test_native_provider_requires_write_ir_effect_for_ir_mutation() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("native_effect_ir", graph).unwrap();
    let before_forms = unit.top_level_form_ids().len();

    compiler.register_stage("compile_unit").unwrap();
    compiler
        .register_provider(
            "native_ir_mutator",
            "compile_unit",
            caap_core::PhasePolicy::CompileTime,
            |context| {
                context.append_ir_top_level_with_spec(&caap_core::ExprSpec::literal(
                    caap_core::IrLiteralData::Int(42),
                ))?;
                Ok(())
            },
        )
        .unwrap();

    let error = compiler
        .queries()
        .compile(&mut unit)
        .expect_err("native provider must declare write_ir");

    assert!(error
        .to_string()
        .contains("does not declare required effect write_ir"));
    assert_eq!(unit.top_level_form_ids().len(), before_forms);
}

#[test]
fn test_native_provider_requires_emit_diagnostics_effect_for_diagnostics() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("native_effect_diagnostics", graph).unwrap();

    compiler.register_stage("compile_unit").unwrap();
    compiler
        .register_provider(
            "native_diagnostic_emitter",
            "compile_unit",
            caap_core::PhasePolicy::CompileTime,
            |context| {
                context
                    .push_diagnostic(caap_core::Diagnostic::warning("native warning").unwrap())?;
                Ok(())
            },
        )
        .unwrap();

    let error = compiler
        .queries()
        .compile(&mut unit)
        .expect_err("native provider must declare emit_diagnostics");

    assert!(error
        .to_string()
        .contains("does not declare required effect emit_diagnostics"));
    assert!(compiler.diagnostics().is_empty());
}

#[test]
fn test_native_provider_requires_write_attributes_effect_for_attribute_mutation() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("native_effect_attributes", graph).unwrap();

    compiler.register_stage("compile_unit").unwrap();
    compiler
        .register_provider(
            "native_attribute_mutator",
            "compile_unit",
            caap_core::PhasePolicy::CompileTime,
            |context| context.set_unit_attribute("compiled", caap_core::SemanticValue::Bool(true)),
        )
        .unwrap();

    let error = compiler
        .queries()
        .compile(&mut unit)
        .expect_err("native provider must declare write_attributes");

    assert!(error
        .to_string()
        .contains("does not declare required effect write_attributes"));
    assert!(unit.attributes().get("compiled").is_none());
}

#[test]
fn test_native_provider_requires_read_attributes_effect_for_attribute_read() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("native_effect_read_attributes", graph).unwrap();
    unit.set_attribute("compiled", caap_core::SemanticValue::Bool(true))
        .unwrap();

    compiler.register_stage("compile_unit").unwrap();
    compiler
        .register_provider(
            "native_attribute_reader",
            "compile_unit",
            caap_core::PhasePolicy::CompileTime,
            |context| context.unit_attribute("compiled").map(|_| ()),
        )
        .unwrap();

    let error = compiler
        .queries()
        .compile(&mut unit)
        .expect_err("native provider must declare read_attributes");

    assert!(error
        .to_string()
        .contains("does not declare required effect read_attributes"));
}

#[test]
fn test_native_provider_requires_write_facts_effect_for_fact_mutation() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("native_effect_facts", graph).unwrap();
    let subject = caap_core::subject_id("demo", "fact").unwrap();

    compiler.register_stage("compile_unit").unwrap();
    compiler
        .register_provider(
            "native_fact_mutator",
            "compile_unit",
            caap_core::PhasePolicy::CompileTime,
            {
                let subject = subject.clone();
                move |context| {
                    context.set_unit_fact(
                        subject.clone(),
                        "value",
                        caap_core::SemanticValue::Int(1),
                    )
                }
            },
        )
        .unwrap();

    let error = compiler
        .queries()
        .compile(&mut unit)
        .expect_err("native provider must declare write_facts");

    assert!(error
        .to_string()
        .contains("does not declare required effect write_facts"));
    assert!(unit
        .semantics()
        .get_fact(&subject, "value")
        .unwrap()
        .is_none());
}

#[test]
fn test_native_provider_declared_effects_allow_ir_and_diagnostics() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("native_effect_allowed", graph).unwrap();
    let before_forms = unit.top_level_form_ids().len();

    compiler.register_stage("compile_unit").unwrap();
    compiler
        .register_provider_with_effects(
            "native_declared_effects",
            "compile_unit",
            caap_core::PhasePolicy::CompileTime,
            ["write_ir".to_string(), "emit_diagnostics".to_string()],
            |context| {
                context.append_ir_top_level_with_spec(&caap_core::ExprSpec::literal(
                    caap_core::IrLiteralData::Int(42),
                ))?;
                context
                    .push_diagnostic(caap_core::Diagnostic::warning("native warning").unwrap())?;
                Ok(())
            },
        )
        .unwrap();

    let plan = compiler.queries().compile(&mut unit).unwrap();

    assert_eq!(unit.top_level_form_ids().len(), before_forms + 1);
    assert_eq!(compiler.diagnostics().len(), 1);
    assert_eq!(compiler.diagnostics()[0].message, "native warning");
    assert!(plan.executed[0]
        .write_cells
        .contains(&"unit:native_effect_allowed@ir".to_string()));
}

#[test]
fn test_query_provider_declared_fact_write_uses_semantic_transaction() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let subject = caap_core::subject_id("demo", "fact").unwrap();
    let mut spec = caap_core::QueryProviderRegistrationSpec::new();
    spec.writes = vec!["facts".to_string()];

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_contract(
            "failing_fact_writer",
            "analyze",
            None,
            caap_core::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            vec!["write_facts".to_string()],
            spec,
            {
                let subject = subject.clone();
                move |context| {
                    context.set_unit_fact(
                        subject.clone(),
                        "value",
                        caap_core::SemanticValue::Int(1),
                    )?;
                    Err("provider failed after fact write".to_string())
                }
            },
        )
        .unwrap();

    let err = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .expect_err("query should propagate provider failure");

    assert_eq!(
        err.to_string(),
        "compiler error: provider failed after fact write"
    );
    assert!(unit
        .semantics()
        .get_fact(&subject, "value")
        .unwrap()
        .is_none());
}

#[test]
fn test_query_provider_effect_tags_are_planned_and_exposed_in_context() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_with_effects(
            "effect_provider",
            "analyze",
            caap_core::PhasePolicy::CompileTime,
            [
                "reads_source".to_string(),
                "writes_semantics".to_string(),
                "write_attributes".to_string(),
            ],
            |context| {
                let provider_context = context
                    .active_provider_context()
                    .expect("provider context should be active");
                assert_eq!(
                    provider_context.effect_tags.to_strings(),
                    vec![
                        "reads_source".to_string(),
                        "write_attributes".to_string(),
                        "writes_semantics".to_string()
                    ]
                );
                context.set_unit_attribute("effect_provider", caap_core::SemanticValue::Bool(true))
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();

    assert_eq!(
        plan.steps[0].effect_tags.to_strings(),
        vec![
            "reads_source".to_string(),
            "write_attributes".to_string(),
            "writes_semantics".to_string(),
        ]
    );
    let artifact_key = plan.steps[0].artifact_key.as_ref().unwrap();
    let artifact = compiler.artifact_cache().peek(artifact_key).unwrap();
    let entries = match artifact {
        caap_core::ArtifactValue::Semantic(caap_core::SemanticValue::Map(entries)) => entries,
        caap_core::ArtifactValue::QueryStage(cached) => match &cached.summary {
            caap_core::SemanticValue::Map(entries) => entries,
            _ => panic!("expected semantic query artifact"),
        },
        _ => panic!("expected semantic query artifact"),
    };
    assert!(entries.contains(&(
        "effect_tags".to_string(),
        caap_core::SemanticValue::List(vec![
            caap_core::SemanticValue::Str("reads_source".to_string()),
            caap_core::SemanticValue::Str("write_attributes".to_string()),
            caap_core::SemanticValue::Str("writes_semantics".to_string()),
        ])
    )));
}

#[test]
fn test_query_execution_options_enforce_effect_allowlist_before_running_provider() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_with_effects(
            "effect_provider",
            "analyze",
            caap_core::PhasePolicy::CompileTime,
            [
                "writes_semantics".to_string(),
                "write_attributes".to_string(),
            ],
            {
                let runs = Rc::clone(&runs);
                move |context| {
                    runs.set(runs.get() + 1);
                    context.set_unit_attribute("ran", caap_core::SemanticValue::Bool(true))
                }
            },
        )
        .unwrap();

    let err = compiler
        .queries()
        .query_with_options(
            "analyze",
            &mut unit,
            caap_core::PhasePolicy::CompileTime,
            caap_core::QueryExecutionOptions::new()
                .with_allowed_effect_tags(["reads_source".to_string()])
                .unwrap(),
        )
        .expect_err("effect allowlist should reject disallowed provider effects");

    assert!(err.to_string().contains("not allowed"));
    assert_eq!(runs.get(), 0);
    assert!(unit.attributes().get("ran").is_none());
}
