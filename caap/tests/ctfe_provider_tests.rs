/// Integration tests for CTFE provider context, effect, and unit mutation mechanisms.
use caap_core::{frontend::parse, RuntimeValue, Unit};

mod common;

#[test]
fn test_ctfe_provider_builtin_metadata_matches_declared_effects() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-metadata-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(ctfe_provider_node_replace ctx target replacement)
(ctfe_provider_node_rewrite ctx target pattern callback)
(ctfe_provider_node_erase ctx target)
(ctfe_provider_fold_compile_time_call ctx target)
(ctfe_provider_diagnostics_warning ctx target message)
(ctfe_provider_fact_set ctx namespace target value)
(ctfe_provider_traversal_walk ctx target callback)
",
    )
    .unwrap();
    let source = format!(
        "(bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
          (list_of
            (get (ctfe_node_call_semantics (get (ctfe_unit_top_level_forms unit) 0) \"caap.fact.call_semantics\") \"effect_policy\")
            (get (ctfe_node_call_semantics (get (ctfe_unit_top_level_forms unit) 1) \"caap.fact.call_semantics\") \"effect_policy\")
            (get (ctfe_node_call_semantics (get (ctfe_unit_top_level_forms unit) 2) \"caap.fact.call_semantics\") \"effect_policy\")
            (get (ctfe_node_call_semantics (get (ctfe_unit_top_level_forms unit) 3) \"caap.fact.call_semantics\") \"effect_policy\")
            (get (ctfe_node_call_semantics (get (ctfe_unit_top_level_forms unit) 4) \"caap.fact.call_semantics\") \"effect_policy\")
            (get (ctfe_node_call_semantics (get (ctfe_unit_top_level_forms unit) 5) \"caap.fact.call_semantics\") \"effect_policy\")
            (get (ctfe_node_call_semantics (get (ctfe_unit_top_level_forms unit) 6) \"caap.fact.call_semantics\") \"eval_policy\")))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_builtin_effect_metadata", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("write_ir".into()));
    let RuntimeValue::List(rewrite_effects) = &items[1] else {
        panic!("expected rewrite effect policy list");
    };
    assert_eq!(
        rewrite_effects.borrow().as_slice(),
        &[
            RuntimeValue::Str("read_ir".into()),
            RuntimeValue::Str("write_ir".into()),
        ]
    );
    assert_eq!(items[2], RuntimeValue::Str("write_ir".into()));
    let RuntimeValue::List(traversal_effects) = &items[3] else {
        panic!("expected traversal effect policy list");
    };
    assert_eq!(
        traversal_effects.borrow().as_slice(),
        &[
            RuntimeValue::Str("read_facts".into()),
            RuntimeValue::Str("read_ir".into()),
            RuntimeValue::Str("read_symbols".into()),
            RuntimeValue::Str("write_ir".into()),
        ]
    );
    assert_eq!(
        &items[4..],
        [
            RuntimeValue::Str("emit_diagnostics".into()),
            RuntimeValue::Str("impure".into()),
            RuntimeValue::Str("special_form".into()),
        ]
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_node_replace_uses_typed_expr_specs() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-node-replace-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "old_value
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"replace_provider\"
            \"compile_unit\"
            (lambda (ctx _root)
              (bind unit (ctfe_provider_unit ctx)
              (bind root (get (ctfe_unit_top_level_forms unit) 0)
                (ctfe_provider_node_replace
                  ctx
                  root
                  (ctfe_ir_literal (map_of \"value\" \"new_value\"))))))
            null
            (list_of \"read_ir\" \"write_ir\"))
          (bind source_unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind execution (get (ctfe_compiler_query_execution compiler \"compile_unit\" source_unit \"compile_time\") \"result\")
              (bind executed (get (get execution \"execution_summary\") 0)
                (bind result_value (get (get execution \"value\") \"value\")
                  (list_of
                    (get executed \"changed\")
                    (get executed \"rewrite_count\")
                    (get executed \"erased_count\")
                    (get (get executed \"touched_node_kinds\") 0)
                    (get (get executed \"touched_node_kinds\") 1)
                    (get (get executed \"change_domains\") 0)
                    (get result_value \"unit_version\")
                    (get result_value \"provider_count\")))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_node_replace_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Int(1));
    assert_eq!(items[2], RuntimeValue::Int(1));
    assert_eq!(items[3], RuntimeValue::Str("Literal".into()));
    assert_eq!(items[4], RuntimeValue::Str("Name".into()));
    assert_eq!(items[5], RuntimeValue::Str("ir".into()));
    assert!(matches!(items[6], RuntimeValue::Int(version) if version > 0));
    assert_eq!(items[7], RuntimeValue::Int(1));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_ir_mutation_builtins_enforce_effect_contracts() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-missing-provider-ir-effect-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "old_value
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"missing_ir_effect_provider\"
            \"compile_unit\"
            (lambda (ctx _root)
              (bind unit (ctfe_provider_unit ctx)
              (bind root (get (ctfe_unit_top_level_forms unit) 0)
                (ctfe_provider_node_erase ctx root))))
            null
            (list_of \"read_ir\"))
          (get
            (ctfe_compiler_query_execution
              compiler
              \"compile_unit\"
              (ctfe_compiler_load_surface_file_template compiler {:?})
              \"compile_time\")
            \"result\"))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_ir_effect_enforcement", graph).unwrap();

    let error = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .expect_err("IR mutation without write_ir effect should fail");
    assert!(format!("{error}").contains("does not declare required effect write_ir"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_unit_top_level_forms_requires_read_ir_effect() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-missing-provider-unit-read-effect-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "old_value
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"missing_unit_read_provider\"
            \"compile_unit\"
            (lambda (ctx _root)
              (bind unit (ctfe_provider_unit ctx)
                (ctfe_unit_top_level_forms unit))))
          (get
            (ctfe_compiler_query_execution
              compiler
              \"compile_unit\"
              (ctfe_compiler_load_surface_file_template compiler {:?})
              \"compile_time\")
            \"result\"))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_unit_read_effect_enforcement", graph).unwrap();

    let error = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .expect_err("provider unit IR read without read_ir effect should fail");
    assert!(format!("{error}").contains("does not declare required effect read_ir"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_traversal_requires_read_ir_effect() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-missing-provider-traversal-effect-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "old_value
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"missing_traversal_read_provider\"
            \"compile_unit\"
            (lambda (ctx _root)
              (bind unit (ctfe_provider_unit ctx)
              (bind root (get (ctfe_unit_top_level_forms unit) 0)
                (ctfe_provider_traversal_walk ctx root (lambda (node) null))))))
          (get
            (ctfe_compiler_query_execution
              compiler
              \"compile_unit\"
              (ctfe_compiler_load_surface_file_template compiler {:?})
              \"compile_time\")
            \"result\"))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_traversal_read_effect_enforcement", graph).unwrap();

    let error = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .expect_err("provider traversal without read_ir effect should fail");
    assert!(format!("{error}").contains("does not declare required effect read_ir"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_callback_return_value_marks_changed() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-reported-change-{}.caap",
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
            \"reported_change_provider\"
            \"compile_unit\"
            (lambda (ctx root) true))
          (bind source_unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind execution
              (get
                (ctfe_compiler_query_execution
                  compiler
                  \"compile_unit\"
                  source_unit
                  \"compile_time\")
                \"result\")
              (bind executed (get (get execution \"execution_summary\") 0)
                (list_of
                  (get executed \"provider_name\")
                  (get executed \"changed\")
                  (get executed \"diagnostics_emitted\"))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_reported_change", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(
        items[0],
        RuntimeValue::Str("reported_change_provider".into())
    );
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Int(0));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_context_builtins_run_inside_query_callback() {
    let mut compiler = common::session();
    let path =
        std::env::temp_dir().join(format!("caap-provider-context-{}.caap", std::process::id()));
    let file_text = "provided_name
";
    std::fs::write(&path, file_text).unwrap();
    let parsed = parse(file_text).unwrap();
    let name_node_id = parsed.top_level_form_ids()[0];
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"context_provider\"
            \"compile_unit\"
            (lambda (ctx root)
              (ctfe_provider_require_effect ctx \"ctx_effect\")
              (ctfe_provider_fact_set ctx \"demo.fact\" {} \"provided_name\")
              (ctfe_unit_add_exposed_name!
                (ctfe_provider_unit ctx)
                (ctfe_provider_fact_get ctx \"demo.fact\" {}))
              (ctfe_provider_diagnostics_error ctx {} \"provider diagnostic\" \"demo.error\"))
            null
            (list_of \"ctx_effect\" \"read_facts\" \"write_facts\" \"write_symbols\"))
          (bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind query_unit
              (get (ctfe_compiler_query_execution compiler \"compile_unit\" unit \"compile_time\") \"unit\")
            (bind summary
              (sequence_find
                (ctfe_unit_symbols query_unit)
                (lambda (entry)
                  (eq (get entry \"name\") \"provided_name\")))
              (list_of
                (not (eq summary null))
                (get summary \"kind\")
                (get summary \"name\"))))))",
        name_node_id,
        name_node_id,
        name_node_id,
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_context_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Str("top_level".into()));
    assert_eq!(items[2], RuntimeValue::Str("provided_name".into()));
    assert_eq!(compiler.diagnostics().len(), 1);
    assert_eq!(
        compiler.diagnostics()[0].code.as_deref(),
        Some("demo.error")
    );
    assert_eq!(compiler.diagnostics()[0].message, "provider diagnostic");

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_require_effect_rejects_non_canonical_effect_tag() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-invalid-effect-tag-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "effect_source
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"invalid_effect_check_provider\"
            \"compile_unit\"
            (lambda (ctx root)
              (ctfe_provider_require_effect ctx \"read-ir\"))
            null
            (list_of \"read_ir\"))
          (ctfe_compiler_query_execution
            compiler
            \"compile_unit\"
            (ctfe_compiler_load_surface_file_template compiler {:?})
            \"compile_time\"))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_invalid_effect_tag", graph).unwrap();

    let error = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .expect_err("effect tag boundary must reject legacy underscore aliases");

    assert!(format!("{error}").contains("invalid effect tag"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_fact_builtins_enforce_effect_contracts() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-missing-provider-fact-effect-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "fact_source
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"missing_fact_effect_provider\"
            \"compile_unit\"
            (lambda (ctx root)
                (ctfe_provider_fact_set ctx \"demo.fact\" root 1)))
          (get
            (ctfe_compiler_query_execution
              compiler
              \"compile_unit\"
              (ctfe_compiler_load_surface_file_template compiler {:?})
              \"compile_time\")
            \"result\"))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_fact_effect_enforcement", graph).unwrap();

    let error = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .expect_err("fact write without write_facts effect should fail");
    assert!(format!("{error}").contains("does not declare required effect write_facts"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_execution_records_runtime_fact_dependencies() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-runtime-dependencies-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "fact_source
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"tracked_writer\"
            \"compile_unit\"
            (lambda (ctx root)
              (ctfe_provider_fact_set ctx \"demo.fact\" root 7))
            null
            (list_of \"write_facts\"))
          (ctfe_compiler_provider_register
            compiler
            \"tracked_reader\"
            \"compile_unit\"
            (lambda (ctx root)
              (ctfe_provider_fact_get ctx \"demo.fact\" root null))
            (list_of \"tracked_writer\")
            (list_of \"read_facts\"))
          (bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
              (bind root (get (ctfe_unit_top_level_forms unit) 0)
                (bind expected_cell
                (string_concat_many \"node:\" (int_to_string (ctfe_node_id root)) \"@demo.fact\")
                (bind artifact
                  (get (ctfe_compiler_query_execution compiler \"compile_unit\" unit \"compile_time\") \"result\")
                  (bind writer (get (get artifact \"execution_summary\") 0)
                    (bind reader (get (get artifact \"execution_summary\") 1)
                      (list_of
                        expected_cell
                        (get (get writer \"write_cells\") 0)
                        (get (get reader \"read_cells\") 0)
                        (get (get artifact \"read_cells\") 0)
                        (get (get artifact \"write_cells\") 0)))))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_runtime_dependency_tracking", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[1], items[0]);
    assert_eq!(items[2], items[0]);
    assert_eq!(items[3], items[0]);
    assert_eq!(items[4], items[0]);

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_execution_records_direct_semantic_writes() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-direct-semantic-writes-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "direct_write_source
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"direct_writer\"
            \"compile_unit\"
            (lambda (ctx root)
              (ctfe_meta_fact_set_by_key root \"demo.fact\" 7))
            null
            (list_of \"write_facts\"))
          (ctfe_compiler_provider_register
            compiler
            \"direct_reader\"
            \"compile_unit\"
            (lambda (ctx root)
              (ctfe_meta_fact_get_by_key root \"demo.fact\" null))
            (list_of \"direct_writer\")
            (list_of \"read_facts\"))
          (bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind root (get (ctfe_unit_top_level_forms unit) 0)
              (bind expected_cell
                (string_concat_many \"node:\" (int_to_string (ctfe_node_id root)) \"@demo.fact\")
                (bind artifact
                  (get (ctfe_compiler_query_execution compiler \"compile_unit\" unit \"compile_time\") \"result\")
                  (bind writer (get (get artifact \"execution_summary\") 0)
                    (bind reader (get (get artifact \"execution_summary\") 1)
                    (list_of
                      expected_cell
                      (get (get writer \"write_cells\") 0)
                      (get (get reader \"read_cells\") 0)
                      (get (get artifact \"read_cells\") 0)
                      (get (get artifact \"write_cells\") 0)))))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_direct_semantic_write_tracking", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[1], items[0]);
    assert_eq!(items[2], items[0]);
    assert_eq!(items[3], items[0]);
    assert_eq!(items[4], items[0]);

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_annotations_require_attribute_effects() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-annotations-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "annotated_source
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"provider_annotations\"
            \"compile_unit\"
            (lambda (ctx root)
              (ctfe_provider_require_effect ctx \"read_attributes\")
              (ctfe_provider_require_effect ctx \"write_attributes\")
                (do
                  (ctfe_provider_annotation_set ctx root \"demo\" \"annotation_value\")
                  (ctfe_provider_diagnostics_note
                    ctx
                    root
                    (string_concat_many
                      (ctfe_provider_annotation_get ctx root \"demo\")
                      \":\"
                      (ctfe_provider_annotation_get ctx root \"missing\" \"default_value\"))
                    \"demo.provider.annotation\")))
            null
            (list_of \"read_attributes\" \"write_attributes\"))
          (bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind artifact (get (ctfe_compiler_query_execution compiler \"compile_unit\" unit \"compile_time\") \"result\")
              (list_of
                (get (get (get artifact \"diagnostics\") 0) \"message\")
                (get (get (get artifact \"diagnostics\") 0) \"code\")))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_annotations", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(
        items[0],
        RuntimeValue::Str("annotation_value:default_value".into())
    );
    assert_eq!(
        items[1],
        RuntimeValue::Str("demo.provider.annotation".into())
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_traversal_walk_and_callback_invocation() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-traversal-{}.caap",
        std::process::id()
    ));
    let file_text = "(int_add 1 2)
";
    std::fs::write(&path, file_text).unwrap();
    let parsed = parse(file_text).unwrap();
    let root_node_id = parsed.top_level_form_ids()[0];
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"traversal_provider\"
            \"compile_unit\"
            (lambda (ctx root)
                (bind names
                  (ctfe_provider_traversal_walk
                    ctx
                    root
                    (lambda (node) true)
                    (map_of \"mode\" \"filter\" \"kind\" \"name\"))
                  (bind sum
                    (ctfe_provider_invoke_callback
                      ctx
                      (lambda (a b) (int_add a b))
                      2
                      3)
                    (ctfe_provider_fact_set ctx \"walk.count\" root (size names))
                    (if (and (eq (size names) 1) (eq sum 5))
                      (ctfe_provider_diagnostics_note ctx root \"walk_ok\" \"demo.walk\")
                      (ctfe_provider_diagnostics_error ctx root \"walk_bad\" \"demo.walk\")))))
            null
            (list_of \"read_ir\" \"write_facts\"))
          (bind source_unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind execution (get (ctfe_compiler_query_execution compiler \"compile_unit\" source_unit \"compile_time\") \"result\")
              (bind executed (get (get execution \"execution_summary\") 0)
                (list_of
                  (get executed \"changed\")
                  (get executed \"diagnostics_emitted\")
                  (get (get executed \"diagnostic_codes\") 0)
                  (get executed \"read_cells\"))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_traversal_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Int(1));
    assert_eq!(items[2], RuntimeValue::Str("demo.walk".into()));
    let RuntimeValue::Tuple(read_cells) = &items[3] else {
        panic!("expected provider read cells");
    };
    assert!(read_cells.contains(&RuntimeValue::Str(
        format!("node:{root_node_id}@$ir").into()
    )));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_stateful_traversal_matches_provider_option_boundary() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-stateful-traversal-{}.caap",
        std::process::id()
    ));
    let file_text = "(int_add 1 2)
";
    std::fs::write(&path, file_text).unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"stateful_traversal_provider\"
            \"compile_unit\"
            (lambda (ctx root)
                (ctfe_provider_fact_set ctx \"walk.count\" root 0)
                (ctfe_provider_traversal_walk
                  ctx
                  root
                  (lambda (node depth)
                    (do
                      (ctfe_provider_fact_set
                        ctx
                        \"walk.count\"
                        root
                        (int_add (ctfe_provider_fact_get ctx \"walk.count\" root 0) 1))
                      (if (ctfe_node_is_call node)
                        (sequence_map
                          (ctfe_node_children node)
                          (lambda (child) (list_of child (int_add depth 1))))
                        null)))
                  (map_of
                    \"mode\" \"stateful\"
                    \"initial_state\" 0
                    \"order\" \"not_a_valid_order_for_non_stateful\"
                    \"kind\" 42))
                (if (eq (ctfe_provider_fact_get ctx \"walk.count\" root 0) 4)
                  (ctfe_provider_diagnostics_note ctx root \"stateful_walk_ok\" \"demo.stateful.walk\")
                  (ctfe_provider_diagnostics_error ctx root \"stateful_walk_bad\" \"demo.stateful.walk\")))
            null
            (list_of \"read_ir\" \"read_facts\" \"write_facts\"))
          (bind source_unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind artifact (get (ctfe_compiler_query_execution compiler \"compile_unit\" source_unit \"compile_time\") \"result\")
              (list_of
                (size (get artifact \"diagnostics\"))
                (get (get (get artifact \"diagnostics\") 0) \"message\")
                (get (get (get artifact \"diagnostics\") 0) \"code\")))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_stateful_traversal_options", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1));
    assert_eq!(items[1], RuntimeValue::Str("stateful_walk_ok".into()));
    assert_eq!(items[2], RuntimeValue::Str("demo.stateful.walk".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_resolution_scope_and_semantic_entry_builtins() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-resolution-{}.caap",
        std::process::id()
    ));
    let file_text = "existing
";
    std::fs::write(&path, file_text).unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"resolution_provider\"
            \"compile_unit\"
            (lambda (ctx root)
              (ctfe_provider_require_effect ctx \"read_symbols\")
              (bind unit (ctfe_provider_unit ctx)
                (ctfe_unit_declare_symbol! unit \"existing\" \"compile_time\" root \"top_level\")
                (bind scope (ctfe_provider_base_resolution_scope ctx)
                  (bind child (ctfe_resolution_scope_fork scope)
                    (bind entry_descriptor
                      (map_of
                        \"name\" \"local.ctfe\"
                        \"source\" \"registered\"
                        \"node_id\" root
                        \"phase_policy\" \"compile_time\"
                        \"unit_id\" (ctfe_unit_id unit))
                      (ctfe_resolution_scope_define! child entry_descriptor)
                      (bind entry (ctfe_resolution_scope_lookup child \"local.ctfe\" null)
                      (bind resolved (map_of \"entry\" (ctfe_semantic_entry_to_map entry))
                        (ctfe_provider_fact_set ctx \"caap.fact.resolved_name\" root resolved)
                        (bind resolved_entry (ctfe_node_resolved_name_entry root \"caap.fact.resolved_name\" null)
                        (bind entry_map (ctfe_semantic_entry_to_map entry)
                          (bind semantics (ctfe_call_semantics_from_entry entry)
                            (if
                              (and
                                (not (eq (value_type (ctfe_resolution_scope_lookup scope \"existing\")) \"null\"))
                                (eq (value_type (ctfe_resolution_scope_lookup scope \"local.ctfe\")) \"null\")
                                (not (eq (value_type (ctfe_resolution_scope_lookup child \"local.ctfe\")) \"null\"))
                                (eq (get entry_map \"source\") \"registered\")
                                (eq (get entry_map \"name\") \"local.ctfe\")
                                (eq (get (ctfe_semantic_entry_to_map resolved_entry) \"name\") \"local.ctfe\")
                                (eq (get entry_map \"phase_policy\") \"compile_time\")
                                (eq (get semantics \"callee_class\") \"registered\")
                                (eq (get semantics \"phase_policy\") \"compile_time\")
                                (not (eq (value_type (get entry_map \"unit_id\")) \"null\"))
                                (eq
                                  (ctfe_node_id (ctfe_semantic_entry_node entry unit null))
                                  (ctfe_node_id root)))
                              (ctfe_provider_diagnostics_note ctx root \"scope_ok\" \"demo.scope\")
                              (ctfe_provider_diagnostics_error ctx root \"scope_bad\" \"demo.scope\"))))))))))))
            null
            (list_of \"read_symbols\" \"write_facts\" \"write_symbols\"))
          (bind source_unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind execution (get (ctfe_compiler_query_execution compiler \"compile_unit\" source_unit \"compile_time\") \"result\")
              (bind executed (get (get execution \"execution_summary\") 0)
                (list_of
                  (get executed \"diagnostics_emitted\")
                  (get (get executed \"diagnostic_codes\") 0))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_resolution_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1));
    assert_eq!(items[1], RuntimeValue::Str("demo.scope".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_diagnostics_notes_and_suggestions() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-diagnostics-fixes-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "bad_name
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"diagnostics_provider\"
            \"compile_unit\"
            (lambda (ctx root)
                (ctfe_provider_diagnostics_warning
                  ctx
                  root
                  \"warning with notes\"
                  \"demo.notes\"
                  (list_of \"first note\" \"second note\"))
                (ctfe_provider_diagnostics_note
                  ctx
                  root
                  \"suggested fix\"
                  \"demo.fix\"
                  null
                  (list_of
                    (map_of
                      \"label\" \"Apply replacement\"
                      \"kind\" \"replace\"
                      \"metadata\" (map_of \"replacement\" \"good_name\"))))))
          (bind source_unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (get (ctfe_compiler_query_execution compiler \"compile_unit\" source_unit \"compile_time\") \"result\")))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_diagnostics_fixes", graph).unwrap();

    compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let diagnostics = compiler.diagnostics();
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(diagnostics[0].code.as_deref(), Some("demo.notes"));
    assert_eq!(diagnostics[0].notes, vec!["first note", "second note"]);
    assert_eq!(diagnostics[1].severity, caap_core::DiagnosticSeverity::Note);
    assert_eq!(diagnostics[1].fixes.len(), 1);
    assert_eq!(diagnostics[1].fixes[0].label, "Apply replacement");
    assert_eq!(diagnostics[1].fixes[0].kind, "replace");
    assert_eq!(
        diagnostics[1].fixes[0].metadata,
        vec![("replacement".to_string(), "good_name".to_string())]
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_uses_generic_unit_linkage_primitives() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-generic-linkage-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "exported
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"linkage_provider\"
            \"compile_unit\"
            (lambda (ctx root)
              (bind unit (ctfe_provider_unit ctx)
                (ctfe_unit_set_id! unit \"projected.unit\")
                (ctfe_unit_add_dependency_binding!
                  unit
                  (map_of
                    \"source_unit\" \"dep.unit\"
                    \"source_name\" \"dep_name\"
                    \"local_name\" \"local_name\"
                    \"syntax\" true))
                (ctfe_unit_add_exposed_name! unit \"local_name\")
                (bind links (ctfe_unit_dependency_bindings unit)
                  (bind public (ctfe_unit_exposed_names unit)
                    (if
                      (and
                        (eq (ctfe_unit_id unit) \"projected.unit\")
                        (eq (get (get links 0) \"source_unit\") \"dep.unit\")
                        (eq (get (get links 0) \"syntax\") true)
                        (eq (get public 0) \"local_name\"))
                      (ctfe_provider_diagnostics_note ctx root \"linkage_ok\" \"demo.linkage\")
                      (ctfe_provider_diagnostics_error ctx root \"linkage_bad\" \"demo.linkage\"))))))
            null
            (list_of \"read_symbols\" \"write_symbols\" \"emit_diagnostics\"))
          (bind source_unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind execution (get (ctfe_compiler_query_execution compiler \"compile_unit\" source_unit \"compile_time\") \"result\")
              (bind executed (get (get execution \"execution_summary\") 0)
                (list_of
                  (get executed \"changed\")
                  (get executed \"diagnostics_emitted\")
                  (get (get executed \"diagnostic_codes\") 0)
                  (get executed \"read_cells\"))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_generic_linkage", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Int(1));
    assert_eq!(items[2], RuntimeValue::Str("demo.linkage".into()));
    let RuntimeValue::Tuple(read_cells) = &items[3] else {
        panic!("expected read cells");
    };
    assert!(read_cells.iter().any(|cell| matches!(
        cell,
        RuntimeValue::Str(value) if value.starts_with("unit:") && value.ends_with("@symbols")
    )));
    assert!(read_cells.contains(&RuntimeValue::Str("symbol:local_name@symbol.entry".into())));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_folds_registered_compile_time_call() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-provider-ctfe-fold-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(demo_ctfe 41)
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_register_value
            compiler
            \"demo_ctfe\"
            (lambda (ctx node)
              (ctfe_ir_literal (map_of \"value\" 42))))
          (ctfe_compiler_register_base_semantic_entries
            compiler
            (list_of
              (map_of
                \"name\" \"demo_ctfe\"
                \"source\" \"registered\"
                \"phase_policy\" \"compile_time\")))
          (ctfe_compiler_provider_register
            compiler
            \"ctfe_fold_provider\"
            \"compile_unit\"
            (lambda (ctx root)
              (bind unit (ctfe_provider_unit ctx)
                (do
                  (ctfe_unit_declare_symbol! unit \"demo_ctfe\" \"compile_time\" null \"top_level\")
                  (bind folded
                    (ctfe_provider_fold_compile_time_call
                      ctx
                      root)
                    (if
                      (and
                        (ctfe_node_is_literal folded)
                        (eq (ctfe_node_literal_value folded) 42))
                      (ctfe_provider_diagnostics_note ctx folded \"ctfe_fold_ok\" \"demo.ctfe.fold\")
                      (ctfe_provider_diagnostics_error ctx root \"ctfe_fold_bad\" \"demo.ctfe.fold\"))))))
            null
            (list_of \"read_ir\" \"read_facts\" \"read_symbols\" \"write_ir\" \"write_symbols\"))
          (bind source_unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind execution (get (ctfe_compiler_query_execution compiler \"compile_unit\" source_unit \"compile_time\") \"result\")
              (bind executed (get (get execution \"execution_summary\") 0)
                (list_of
                  (get executed \"diagnostics_emitted\")
                  (get (get executed \"diagnostic_codes\") 0))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider_ctfe_fold", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1));
    assert_eq!(items[1], RuntimeValue::Str("demo.ctfe.fold".into()));

    let _ = std::fs::remove_file(path);
}
