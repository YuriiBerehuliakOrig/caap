/// Integration tests for CTFE compiler query, evaluation, and bootstrap builtins.
///
/// These scenarios exercise the compiler-facing query API separately from
/// provider context mutation/effect semantics.
use caap_core::{
    compiler::QueryArtifactSource, frontend::parse, QueryExecutionOptions, RuntimeValue,
    SemanticValue, Unit,
};
use std::rc::Rc;

mod common;

#[test]
fn test_ctfe_compiler_query_builtins_project_registered_stages() {
    let mut compiler = common::session();
    compiler
        .bootstrap()
        .execute_text("(ctfe_compiler_stage_register compiler \"parse\" null \"compile\" (list_of \"compile\"))", "trace_bootstrap")
        .unwrap();
    let graph = parse(
        "(list_of
          (get (get (ctfe_compiler_list_stages compiler) 0) \"name\")
          (get (get (ctfe_compiler_list_stages compiler) 0) \"family\"))",
    )
    .unwrap();
    let unit = Unit::from_graph("compiler_query_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("parse".into()));
    assert_eq!(items[1], RuntimeValue::Str("compile".into()));
}

#[test]
fn test_ctfe_compiler_query_execution_projects_steps_from_source() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-query-plan-source-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path, "null
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"parse\" null \"compile\" null null (list_of \"surface\"))
          (ctfe_compiler_stage_register compiler \"compile_unit\" (list_of \"parse\"))
          (ctfe_compiler_provider_register
            compiler
            \"query_plan_source_provider\"
            \"compile_unit\"
            (lambda (ctx _root)
              (bind unit (ctfe_provider_unit ctx)
              (ctfe_provider_diagnostics_note
                ctx
                (get (ctfe_unit_top_level_forms unit) 0)
                \"provider ran\"
                \"demo.query_plan.provider\")))
            null
            (list_of \"read_ir\" \"emit_diagnostics\"))
          (bind first_execution (ctfe_compiler_query_execution compiler \"compile_unit\" {:?} \"compile_time\")
          (bind first_plan (get first_execution \"steps\")
            (bind artifact
              (get
                (ctfe_compiler_query_execution compiler \"compile_unit\" {:?} \"compile_time\")
                \"result\")
              (bind second_execution (ctfe_compiler_query_execution compiler \"compile_unit\" {:?} \"compile_time\")
              (bind second_plan (get second_execution \"steps\")
                (list_of
                  (size first_plan)
                  (get (get first_plan 0) \"stage\")
                  (eq (value_type (get (get first_plan 0) \"key\")) \"tuple\")
                  (get (get first_plan 0) \"cached\")
                  (get artifact \"stage\")
                  (get (get second_plan 0) \"cached\")
                  (size (get artifact \"diagnostics\")))))))))",
        path.display().to_string(),
        path.display().to_string(),
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("query_plan_source_projection", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1));
    assert_eq!(items[1], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[2], RuntimeValue::Bool(true));
    assert_eq!(items[3], RuntimeValue::Bool(false));
    assert_eq!(items[4], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[5], RuntimeValue::Bool(true));
    assert_eq!(items[6], RuntimeValue::Int(1));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_query_pipeline_accepts_inline_source_text_like_inline_source_text() {
    let mut compiler = common::session();
    compiler
        .register_stage_spec(
            caap_core::QueryStageSpec::new("parse_surface")
                .unwrap()
                .with_input_kinds(vec!["surface".to_string()])
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_stage_spec(
            caap_core::QueryStageSpec::new("compile_unit")
                .unwrap()
                .with_requires(vec!["parse_surface".to_string()])
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_provider(
            "inline_source_provider",
            "compile_unit",
            caap_core::PhasePolicy::CompileTime,
            |_context| Ok(()),
        )
        .unwrap();
    let bridge =
        Rc::new(caap_core::compiler::CompilerBridgeValue::from_session_state(compiler.clone()));

    let first_execution = bridge
        .query_execution_projection_with_options(
            "compile_unit",
            QueryArtifactSource::Text("42".to_string()),
            caap_core::PhasePolicy::CompileTime,
            QueryExecutionOptions::default(),
        )
        .unwrap();
    assert_eq!(
        first_execution
            .plan
            .steps
            .iter()
            .map(|step| step.stage.as_str())
            .collect::<Vec<_>>(),
        vec!["compile_unit"]
    );
    assert!(first_execution.plan.steps[0].artifact_key.is_some());
    assert!(!first_execution.plan.steps[0].cached);

    let second_execution = bridge
        .query_execution_projection_with_options(
            "compile_unit",
            QueryArtifactSource::Text("42".to_string()),
            caap_core::PhasePolicy::CompileTime,
            QueryExecutionOptions::default(),
        )
        .unwrap();
    let artifact = second_execution.artifact.unwrap();
    assert_eq!(artifact.stage, "compile_unit");
    assert_eq!(artifact.phase, caap_core::PhasePolicy::CompileTime);
    let entries = match &artifact.value {
        caap_core::ArtifactValue::Semantic(SemanticValue::Map(entries)) => entries,
        caap_core::ArtifactValue::QueryStage(cached) => match &cached.summary {
            SemanticValue::Map(entries) => entries,
            _ => panic!("expected semantic query artifact value"),
        },
        _ => panic!("expected semantic query artifact value"),
    };
    assert_eq!(
        entries.iter().find(|(key, _)| key == "provider_count"),
        Some(&("provider_count".to_string(), SemanticValue::Int(1)))
    );

    assert!(second_execution.plan.steps[0].cached);
}

#[test]
fn test_ctfe_compiler_query_execution_result_runs_query_pipeline() {
    let mut compiler = common::session();
    let path =
        std::env::temp_dir().join(format!("caap-query-artifact-{}.caap", std::process::id()));
    std::fs::write(
        &path, "null
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"query_artifact_provider\"
            \"compile_unit\"
            (lambda (ctx _root) null)
            null
            null
            (map_of
              \"reads\" (list_of \"unit\")
              \"writes\" (list_of \"facts\")))
          (bind artifact
            (get
              (ctfe_compiler_query_execution
                compiler
                \"compile_unit\"
                (ctfe_compiler_load_surface_file_template compiler {:?})
                \"compile_time\")
              \"result\")
            (list_of
              (get artifact \"artifact_kind\")
              (get artifact \"stage\")
              (get artifact \"phase\")
              (get (get artifact \"value\") \"kind\")
              (get (get (get artifact \"value\") \"value\") \"provider_count\")
              (get artifact \"iterations\")
              (get (get (get artifact \"execution_summary\") 0) \"provider_name\")
              (get (get artifact \"reads_subjects\") 0)
              (get (get artifact \"write_cells\") 0))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("query_artifact_builtin", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("query".into()));
    assert_eq!(items[1], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[2], RuntimeValue::Str("compile_time".into()));
    assert_eq!(items[3], RuntimeValue::Str("semantic".into()));
    assert_eq!(items[4], RuntimeValue::Int(1));
    assert_eq!(items[5], RuntimeValue::Int(1));
    assert_eq!(
        items[6],
        RuntimeValue::Str("query_artifact_provider".into())
    );
    assert_eq!(items[7], RuntimeValue::Str("unit".into()));
    assert_eq!(items[8], RuntimeValue::Str("facts".into()));
    assert_eq!(compiler.artifact_cache().stats().generation, 1);

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_query_execution_accepts_initial_bindings() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-query-artifact-initial-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "placeholder
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"query_artifact_initial_provider\"
            \"compile_unit\"
            (lambda (ctx _root)
              (bind unit (ctfe_provider_unit ctx)
                (ctfe_unit_add_exposed_name! unit initial_public)))
            null
            (list_of \"write_symbols\"))
          (bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind artifact
              (get
                (ctfe_compiler_query_execution
                  compiler
                  \"compile_unit\"
                  unit
                  \"compile_time\"
                  (map_of \"initial_public\" \"public_from_initial\"))
                \"result\")
              (bind execution
                (ctfe_compiler_query_execution
                  compiler
                  \"compile_unit\"
                  unit
                  \"compile_time\"
                  (map_of \"initial_public\" \"public_from_initial\"))
              (bind plan (get execution \"steps\")
                (list_of
                  (get (get artifact \"key\") 17)
                  (get (get artifact \"key\") 18)
                  (get (get artifact \"key\") 19)
                  (get (get (get artifact \"value\") \"value\") \"provider_count\")
                  (get (get plan 0) \"cached\")))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("query_artifact_initial_bindings", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("initial_binding".into()));
    assert_eq!(items[1], RuntimeValue::Str("initial_public".into()));
    assert_eq!(
        items[2],
        RuntimeValue::Str("str:public_from_initial".into())
    );
    assert_eq!(items[3], RuntimeValue::Int(1));
    assert_eq!(items[4], RuntimeValue::Bool(true));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_explain_query_and_provider_schedule_builtins() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!("caap-explain-query-{}.caap", std::process::id()));
    std::fs::write(
        &path, "null
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"explain_provider\"
            \"compile_unit\"
            (lambda (ctx _root) null)
            null
            (list_of \"explain_effect\")
            (map_of
              \"reads\" (list_of \"unit\")
              \"writes\" (list_of \"diagnostics\")))
          (bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind execution (get (ctfe_compiler_query_execution compiler \"compile_unit\" unit \"compile_time\") \"result\")
              (bind artifact (get (ctfe_compiler_query_execution compiler \"compile_unit\" unit \"compile_time\") \"result\")
                (bind invalidation (ctfe_compiler_query_execution compiler \"compile_unit\" unit \"compile_time\")
                  (bind plan (get invalidation \"steps\")
                    (bind schedule (ctfe_compiler_provider_schedule compiler \"compile_unit\")
                      (bind group (get (get schedule \"groups\") 0)
                          (bind provider (get (get group \"providers\") 0)
                            (bind executed (get (get execution \"execution_summary\") 0)
                            (list_of
                              \"compile_unit\"
                              (get (get plan 0) \"stage\")
                              null
                              (get artifact \"stage\")
                              (get artifact \"artifact_kind\")
                              (get artifact \"stage\")
                              (size (get artifact \"dependencies\"))
                              (get (get (get invalidation \"steps\") 0) \"cached\")
                              (get (get (get (get invalidation \"steps\") 0) \"key\") 0)
                              (get (get (get invalidation \"steps\") 0) \"invalidation\")
                              (get schedule \"stage\")
                              (get provider \"name\")
                              (get (get (get provider \"effects\") \"emits\") 0)
                              (get executed \"provider_name\")
                              (get executed \"outcome_kind\")
                              (get (get (get (get executed \"provider_contract\") \"effects\") \"emits\") 0)
                              (get artifact \"iterations\")
                              (get (get artifact \"reads_subjects\") 0)
                              (get (get artifact \"write_cells\") 0))))))))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("explain_query_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[1], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[2], RuntimeValue::Null);
    assert_eq!(items[3], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[4], RuntimeValue::Str("query".into()));
    assert_eq!(items[5], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[6], RuntimeValue::Int(0));
    assert_eq!(items[7], RuntimeValue::Bool(true));
    assert_eq!(items[8], RuntimeValue::Str("query_stage".into()));
    assert_eq!(items[9], RuntimeValue::Null);
    assert_eq!(items[10], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[11], RuntimeValue::Str("explain_provider".into()));
    assert_eq!(items[12], RuntimeValue::Str("explain_effect".into()));
    assert_eq!(items[13], RuntimeValue::Str("explain_provider".into()));
    assert_eq!(items[14], RuntimeValue::Str("ok".into()));
    assert_eq!(items[15], RuntimeValue::Str("explain_effect".into()));
    assert_eq!(items[16], RuntimeValue::Int(1));
    assert_eq!(items[17], RuntimeValue::Str("unit".into()));
    assert_eq!(items[18], RuntimeValue::Str("diagnostics".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_explain_provider_schedule_honors_requires() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-schedule-requires-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path, "null
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"consumer\"
            \"compile_unit\"
            (lambda (ctx _root) null)
            (list_of \"producer\"))
          (ctfe_compiler_provider_register
            compiler
            \"producer\"
            \"compile_unit\"
            (lambda (ctx _root) null))
          (bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind schedule (ctfe_compiler_provider_schedule compiler \"compile_unit\")
              (bind groups (get schedule \"groups\")
                (list_of
                  (size groups)
                  (get (get (get (get groups 0) \"providers\") 0) \"name\")
                  (get (get (get (get groups 1) \"providers\") 0) \"name\"))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_schedule_requires", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(2));
    assert_eq!(items[1], RuntimeValue::Str("producer".into()));
    assert_eq!(items[2], RuntimeValue::Str("consumer".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_explain_provider_schedule_projects_effect_barriers() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-schedule-barrier-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path, "null
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"writer\"
            \"compile_unit\"
            (lambda (ctx _root) null)
            null
            null
            (map_of \"writes\" (list_of \"facts\")))
          (ctfe_compiler_provider_register
            compiler
            \"reader\"
            \"compile_unit\"
            (lambda (ctx _root) null)
            null
            null
            (map_of \"reads\" (list_of \"facts\")))
          (bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind schedule (ctfe_compiler_provider_schedule compiler \"compile_unit\")
              (bind groups (get schedule \"groups\")
                (bind barrier (get (get groups 0) \"barrier_after\")
                  (bind provider (get (get (get groups 0) \"providers\") 0)
                    (list_of
                      (size groups)
                      (get provider \"name\")
                      (get (get (get provider \"effects\") \"writes\") 0)
                      (get barrier \"next_group_index\")
                      (get (get barrier \"reasons\") 0)
                      (get (get (get (get groups 1) \"providers\") 0) \"name\"))))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_schedule_barrier", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(2));
    assert_eq!(items[1], RuntimeValue::Str("writer".into()));
    assert_eq!(items[2], RuntimeValue::Str("facts".into()));
    assert_eq!(items[3], RuntimeValue::Int(1));
    assert_eq!(
        items[4],
        RuntimeValue::Str("reads after writes on facts".into())
    );
    assert_eq!(items[5], RuntimeValue::Str("reader".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_provider_schedule_honors_data_requirements() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-schedule-data-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path, "null
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"consumer\"
            \"compile_unit\"
            (lambda (ctx _root) null)
            null
            null
            (map_of \"requires_data\" (list_of \"facts:demo.type_root\")))
          (ctfe_compiler_provider_register
            compiler
            \"producer\"
            \"compile_unit\"
            (lambda (ctx _root) null)
            null
            null
            (map_of \"provides_data\" (list_of \"facts:demo.type_root\")))
          (bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind schedule (ctfe_compiler_provider_schedule compiler \"compile_unit\")
              (bind groups (get schedule \"groups\")
                (list_of
                  (size groups)
                  (get (get (get (get groups 0) \"providers\") 0) \"name\")
                  (get (get (get (get groups 1) \"providers\") 0) \"name\")
                  (get (get (get (get (get groups 1) \"providers\") 0) \"requires_data\") 0)
                  (get (get (get (get (get groups 0) \"providers\") 0) \"provides_data\") 0))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_schedule_data", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(2));
    assert_eq!(items[1], RuntimeValue::Str("producer".into()));
    assert_eq!(items[2], RuntimeValue::Str("consumer".into()));
    assert_eq!(items[3], RuntimeValue::Str("facts.demo.type_root".into()));
    assert_eq!(items[4], RuntimeValue::Str("facts.demo.type_root".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_explain_name_runs_query_pipeline() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-compiler-explain-name-{}.caap",
        std::process::id()
    ));
    let file_text = "public_value
";
    std::fs::write(&path, file_text).unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"name_provider\"
            \"compile_unit\"
            (lambda (ctx root)
              (ctfe_unit_add_exposed_name! (ctfe_provider_unit ctx) \"public_value\"))
            null
            (list_of \"name_effect\" \"write_symbols\"))
          (bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind query_unit
              (get (ctfe_compiler_query_execution compiler \"compile_unit\" unit \"compile_time\") \"unit\")
            (bind summary
              (sequence_find
                (ctfe_unit_symbols query_unit)
                (lambda (entry)
                  (eq (get entry \"name\") \"public_value\")))
              (list_of
                (get summary \"name\")
                (not (eq summary null))
                (get summary \"kind\"))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("compiler_explain_name", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("public_value".into()));
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Str("top_level".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_surface_file_builtin_loads_template() {
    let mut compiler = common::session();
    let path =
        std::env::temp_dir().join(format!("caap-surface-builtins-{}.caap", std::process::id()));
    std::fs::write(
        &path,
        "(module \"demo.surface\")
(import_namespace \"module\" \"module\")
(int_add 1 2)
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_load_surface_file_template compiler {:?})
          (host_value_kind (ctfe_compiler_load_surface_file_template compiler {:?})))",
        path.display().to_string(),
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("surface_file_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Str("unit".into()));
    assert_eq!(
        compiler.source_templates().artifact_cache().stats().misses,
        1
    );
    assert_eq!(compiler.source_templates().artifact_cache().stats().hits, 1);

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_query_execution_returns_unit_handle() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-compile-unit-builtins-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(module \"demo.compile\")
(int_add 1 2)
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (host_value_kind
            (get
              (ctfe_compiler_query_execution
              compiler
                \"compile_unit\"
                (ctfe_compiler_load_surface_file_template compiler {:?})
                \"compile_time\")
              \"unit\")))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("compile_unit_builtin", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let unit_id = std::fs::canonicalize(&path)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(value, RuntimeValue::Str("unit".into()));
    assert!(compiler.get_unit(&unit_id).unwrap().is_some());

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_query_execution_passes_initial_bindings_to_provider() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-compile-unit-initial-{}.caap",
        std::process::id()
    ));
    let file_text = "placeholder
";
    std::fs::write(&path, file_text).unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"initial_provider\"
            \"compile_unit\"
            (lambda (ctx _root)
              (bind unit (ctfe_provider_unit ctx)
                (ctfe_unit_add_exposed_name! unit initial_public)))
            null
            (list_of \"write_symbols\"))
            (bind execution
              (ctfe_compiler_query_execution
                compiler
                \"compile_unit\"
                (ctfe_compiler_load_surface_file_template compiler {:?})
                \"compile_time\"
                (map_of \"initial_public\" \"public_from_initial\"))
            (bind compiled (get execution \"unit\")
            (bind facts
              (ctfe_unit_facts compiled)
              (get (get facts 0) 0)))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("compile_unit_initial_bindings", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(
        value,
        RuntimeValue::Str("symbol:public_from_initial".into())
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_evaluate_capture_builtin_projects_result_and_diagnostics() {
    let mut compiler = common::session();
    let ok_path = std::env::temp_dir().join(format!(
        "caap-evaluate-capture-ok-{}.caap",
        std::process::id()
    ));
    let err_path = std::env::temp_dir().join(format!(
        "caap-evaluate-capture-err-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &ok_path,
        "(runtime_error \"skipped\")
(int_add external 2)
",
    )
    .unwrap();
    std::fs::write(
        &err_path,
        "(runtime_error \"boom\")
",
    )
    .unwrap();
    let source = format!(
        "(bind ok_unit (ctfe_compiler_load_surface_file_template compiler {:?})
          (bind err_unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind ok_capture
              (ctfe_compiler_evaluate_capture
                compiler
                ok_unit
                \"runtime\"
                (map_of \"external\" 40)
                1)
              (list_of
                (get ok_capture \"result\")
                (get ok_capture \"skipped_forms\")
                (get
                  (get
                    (get
                      (ctfe_compiler_evaluate_capture compiler err_unit \"runtime\")
                      \"diagnostics\")
                    0)
                  \"code\")))))",
        ok_path.display().to_string(),
        err_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("evaluate_capture_builtin", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(42));
    assert_eq!(items[1], RuntimeValue::Int(1));
    assert_eq!(items[2], RuntimeValue::Str("CAAP-RUNTIME-001".into()));
    assert_eq!(compiler.diagnostics().len(), 1);

    let _ = std::fs::remove_file(ok_path);
    let _ = std::fs::remove_file(err_path);
}

#[test]
fn test_ctfe_compiler_evaluate_bootstrap_file_builtin_captures_result() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-evaluate-bootstrap-file-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(runtime_error \"skipped\")
         (list_of
           (int_add external 2)
           (size (get (ctfe_compiler_current_bootstrap_context compiler) \"capabilities\"))
           (get (ctfe_compiler_current_bootstrap_context compiler) \"path\"))
",
    )
    .unwrap();
    let source = format!(
        "(get
          (ctfe_compiler_evaluate_bootstrap_file
            compiler
            {:?}
            (map_of \"external\" 40)
            (list_of \"sys\")
            1)
          \"result\")",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("evaluate_bootstrap_file_builtin", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(42));
    assert_eq!(items[1], RuntimeValue::Int(1));
    assert_eq!(
        items[2],
        RuntimeValue::Str(
            std::fs::canonicalize(&path)
                .unwrap()
                .display()
                .to_string()
                .into()
        )
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_evaluate_bootstrap_file_uses_explicit_skip_count() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-evaluate-bootstrap-explicit-skip-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(module \"demo.explicit_skip\")
         external
",
    )
    .unwrap();
    let source = format!(
        "(list_of
          (size
            (get
              (ctfe_compiler_evaluate_bootstrap_file
                compiler
                {:?}
                (map_of \"external\" 41)
                (list_of)
                0)
              \"diagnostics\"))
          (get
            (ctfe_compiler_evaluate_bootstrap_file
              compiler
              {:?}
              (map_of \"external\" 41)
              (list_of)
              1)
            \"result\"))",
        path.display().to_string(),
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("evaluate_bootstrap_file_explicit_skip", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1));
    assert_eq!(items[1], RuntimeValue::Int(41));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_execute_bootstrap_file_builtin_runs_source() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-execute-bootstrap-file-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(ctfe_compiler_register_value compiler \"bootstrap.answer\" 42)
         (ctfe_compiler_register_value
           compiler
           \"bootstrap.path\"
           (get (ctfe_compiler_current_bootstrap_context compiler) \"path\"))
         (ctfe_compiler_register_value
           compiler
           \"bootstrap.capability_count\"
           (size (get (ctfe_compiler_current_bootstrap_context compiler) \"capabilities\")))
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_execute_bootstrap_file compiler {:?})
          (list_of
            (ctfe_compiler_lookup_value compiler \"bootstrap.answer\")
            (ctfe_compiler_lookup_value compiler \"bootstrap.path\")
            (ctfe_compiler_lookup_value compiler \"bootstrap.capability_count\")))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("execute_bootstrap_file_builtin", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(42));
    assert_eq!(
        items[1],
        RuntimeValue::Str(
            std::fs::canonicalize(&path)
                .unwrap()
                .display()
                .to_string()
                .into()
        )
    );
    assert_eq!(items[2], RuntimeValue::Int(0));
    assert_eq!(compiler.bootstrap_executions().len(), 1);
    assert_eq!(compiler.bootstrap_trace().len(), 1);
    assert_eq!(compiler.bootstrap_trace()[0].action, "bootstrap.raw");

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_execute_bootstrap_file_accepts_internal_capabilities() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-execute-bootstrap-file-capabilities-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(ctfe_compiler_register_value
           compiler
           \"bootstrap.capabilities\"
           (get (ctfe_compiler_current_bootstrap_context compiler) \"capabilities\"))
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_execute_bootstrap_file
            compiler
            {:?}
            (list_of \"sys\" \"sys.fs.read\"))
          (ctfe_compiler_lookup_value compiler \"bootstrap.capabilities\"))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("execute_bootstrap_file_capabilities", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();
    let _ = std::fs::remove_file(path);

    assert_eq!(
        value,
        RuntimeValue::Tuple(
            vec![
                RuntimeValue::Str("sys".into()),
                RuntimeValue::Str("sys.fs.read".into())
            ]
            .into()
        )
    );
}
