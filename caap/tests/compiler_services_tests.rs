/// Integration tests for compiler service builtins, fact schema registration, and evaluation service.
use caap_core::{frontend::parse, graph::GraphBuilder, ir::IrLiteralData, RuntimeValue, Unit};

trait TestGraphBuilderExt {
    fn name(&mut self, identifier: impl Into<String>) -> u32;
    fn literal(&mut self, value: IrLiteralData) -> u32;
    fn call(&mut self, callee: u32, args: Vec<u32>) -> u32;
}

impl TestGraphBuilderExt for GraphBuilder {
    fn name(&mut self, identifier: impl Into<String>) -> u32 {
        self.try_name(identifier)
            .expect("test graph name must be valid")
    }

    fn literal(&mut self, value: IrLiteralData) -> u32 {
        self.try_literal(value)
            .expect("test graph literal must be valid")
    }

    fn call(&mut self, callee: u32, args: Vec<u32>) -> u32 {
        self.try_call(callee, args)
            .expect("test graph call must reference existing nodes")
    }
}

mod common;

#[test]
fn test_ctfe_compiler_directory_query_builtins_project_entries() {
    let mut compiler = common::session();
    let root = std::env::temp_dir().join(format!("caap-dir-builtins-{}", std::process::id()));
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(root.join("a.caap"), "null").unwrap();
    std::fs::write(nested.join("b.caap"), "null").unwrap();
    let source = format!(
        "(list_of
          (size (ctfe_compiler_list_dir compiler {:?}))
          (get (get (ctfe_compiler_list_dir compiler {:?}) 0) \"name\")
          (ctfe_compiler_is_file compiler {:?})
          (ctfe_compiler_is_file compiler {:?}))",
        root.display().to_string(),
        root.display().to_string(),
        root.join("a.caap").display().to_string(),
        root.join("missing.caap").display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("directory_query_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(2));
    assert_eq!(items[1], RuntimeValue::Str("a.caap".into()));
    assert_eq!(items[2], RuntimeValue::Bool(true));
    assert_eq!(items[3], RuntimeValue::Bool(false));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn test_ctfe_compiler_register_unit_builtin_updates_catalog() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-register-unit-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(&path, "(int_add 1 2)").unwrap();
    let source = format!(
        "(bind (
          (unit
            (ctfe_compiler_load_surface_file_template
              compiler
              {:?}
              (map_of \"unit_id\" \"demo.source_unit\")))
        )
          (do
            (ctfe_compiler_register_unit compiler \"demo.catalog_unit\" unit)
            (list_of
              (ctfe_unit_id
                (ctfe_compiler_lookup_unit compiler \"demo.catalog_unit\"))
              (ctfe_compiler_lookup_unit compiler \"missing.unit\" \"missing_default\"))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("register_unit_builtin", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("demo.catalog_unit".into()));
    assert_eq!(items[1], RuntimeValue::Str("missing_default".into()));
    assert!(compiler.catalog().contains_unit("demo.catalog_unit"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_register_and_list_semantic_policy_builtins() {
    let mut compiler = common::session();
    let graph = parse(
        "(do
          (ctfe_compiler_register_semantic_policy
            compiler
            \"demo_special\"
            (map_of
              \"phase_policy\" \"compile_time\"
              \"eval_policy\" \"special_form\"
              \"control_policy\" \"structured_exit\"
              \"scope_policy\" \"lexical_binding\"
              \"effect_policy\" (list_of \"macro\" \"read_ir\")
              \"form_policy\" \"control_region\")
            (lambda (form) form))
          (bind policies (ctfe_compiler_list_semantic_policies compiler)
            (bind policy (get policies 0)
              (list_of
                (size policies)
                (get policy \"name\")
                (get policy \"phase_policy\")
                (get policy \"effect_policy\")
                (get policy \"eval_policy\")
                (get policy \"control_policy\")
                (get policy \"scope_policy\")
                (get policy \"form_policy\")
                (get policy \"has_normalizer\")))))",
    )
    .unwrap();
    let unit = Unit::from_graph("semantic_policy_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1));
    assert_eq!(items[1], RuntimeValue::Str("demo_special".into()));
    assert_eq!(items[2], RuntimeValue::Str("compile_time".into()));
    let RuntimeValue::List(effect_policy) = &items[3] else {
        panic!("expected effect policy list");
    };
    assert_eq!(
        effect_policy.borrow().as_slice(),
        &[
            RuntimeValue::Str("macro".into()),
            RuntimeValue::Str("read_ir".into())
        ]
    );
    assert_eq!(items[4], RuntimeValue::Str("special_form".into()));
    assert_eq!(items[5], RuntimeValue::Str("structured_exit".into()));
    assert_eq!(items[6], RuntimeValue::Str("lexical_binding".into()));
    assert_eq!(items[7], RuntimeValue::Str("control_region".into()));
    assert_eq!(items[8], RuntimeValue::Bool(true));
}

#[test]
fn test_ctfe_compiler_fact_schema_and_base_semantic_entry_registration() {
    let mut compiler = common::session();
    let graph = parse(
        "(do
          (ctfe_compiler_fact_schema_type_bridge_register compiler \"demo_string\" \"string\")
          (ctfe_compiler_fact_schema_register compiler \"demo.fact\" \"demo_string\" false \"demo fact\")
          (ctfe_compiler_register_base_semantic_entries
            compiler
            (list_of
              (map_of \"name\" \"if\" \"source\" \"builtin\" \"phase_policy\" \"dual\")
              (map_of \"name\" \"int_add\" \"source\" \"builtin\" \"phase_policy\" \"dual\")))
          (ctfe_compiler_lookup_value compiler \"caap.fact_schema.type_bridge.demo_string\"))",
    )
    .unwrap();
    let unit = Unit::from_graph("fact_schema_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Str("string".into()));
    let schema = compiler
        .fact_schema()
        .lookup("demo.fact")
        .unwrap()
        .cloned()
        .expect("expected registered fact schema");
    assert_eq!(schema.type_label, "demo_string");
    assert_eq!(schema.bridge_name, "string");
    assert!(!schema.allow_none);
    assert_eq!(schema.description.as_deref(), Some("demo fact"));
    let entry_names: Vec<_> = compiler
        .base_semantic_entries()
        .into_iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(entry_names, vec!["if", "int_add"]);
}

#[test]
fn test_ctfe_compiler_builtin_semantic_entries_project_registered_builtin_policies() {
    let mut compiler = common::session();
    let graph = parse(
        "(bind entries (ctfe_compiler_builtin_semantic_entries compiler)
          (bind find_entry
            (lambda (name)
              (sequence_find
                entries
                (lambda (entry) (eq (get entry \"name\") name))))
            (bind macro_entry (find_entry \"macro\")
              (bind internal_entry (find_entry \"assign_lexical\")
                (bind catalog_entry (find_entry \"ctfe_compiler_builtin_semantic_entries\")
                  (bind stale_entry (find_entry \"host_import\")
                    (list_of
                      (get macro_entry \"eval_policy\")
                      (get macro_entry \"scope_policy\")
                      internal_entry
                      (get catalog_entry \"phase_policy\")
                      stale_entry)))))))",
    )
    .unwrap();
    let unit = Unit::from_graph("builtin_semantic_entry_catalog", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("special_form".into()));
    assert_eq!(items[1], RuntimeValue::Str("lexical_binding".into()));
    assert_eq!(items[2], RuntimeValue::Null);
    assert_eq!(items[3], RuntimeValue::Str("compile_time".into()));
    assert_eq!(items[4], RuntimeValue::Null);
}

#[test]
fn test_ctfe_compiler_fact_schema_rejects_unknown_bridge_and_wrong_fact_value() {
    let mut compiler = common::session();
    let bad_bridge = Unit::from_graph(
        "bad_fact_schema_bridge",
        parse("(ctfe_compiler_fact_schema_type_bridge_register compiler \"bad\" \"missing\")")
            .unwrap(),
    )
    .unwrap();
    let error = compiler
        .evaluation()
        .evaluate(&bad_bridge, caap_core::PhasePolicy::CompileTime, [])
        .expect_err("unknown bridge should fail");
    assert!(format!("{error}").contains("unknown fact schema type bridge"));

    let setup = Unit::from_graph(
        "fact_schema_provider_validation",
        parse(
            "(do
              (ctfe_compiler_stage_register compiler \"compile_unit\")
              (ctfe_compiler_fact_schema_type_bridge_register compiler \"demo_string\" \"string\")
              (ctfe_compiler_fact_schema_register compiler \"demo.fact\" \"demo_string\")
              (ctfe_compiler_provider_register
                compiler
                \"bad_fact_provider\"
                \"compile_unit\"
                (lambda (ctx root)
                  (ctfe_provider_fact_set ctx \"demo.fact\" root 42))
                null
                (list_of \"write_facts\")))",
        )
        .unwrap(),
    )
    .unwrap();
    compiler
        .evaluation()
        .evaluate(&setup, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();
    let mut source_unit = Unit::from_graph("fact_schema_source", parse("x").unwrap()).unwrap();
    let error = compiler
        .queries()
        .query(
            "compile_unit",
            &mut source_unit,
            caap_core::PhasePolicy::CompileTime,
        )
        .expect_err("schema mismatch should fail provider query");
    assert!(error
        .to_string()
        .contains("expects value compatible with schema type"));
}

#[test]
fn test_compiler_catalog_reads_registered_units_without_module_resolution() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let unit = Unit::from_graph("main", graph).unwrap();

    compiler.register_unit(unit).unwrap();
    let catalog = compiler.catalog();

    assert!(catalog.contains_unit("main"));
    assert!(catalog.get_compiled_unit("main").unwrap().is_some());
    assert!(catalog.get_compiled_unit("missing").unwrap().is_none());
    assert_eq!(catalog.unit_ids(), vec!["main"]);
}

#[test]
fn test_compiler_evaluation_service_uses_initial_bindings() {
    let mut compiler = common::session();
    let graph = parse("(int_add external 2)").unwrap();
    let unit = Unit::from_graph("main", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(
            &unit,
            caap_core::PhasePolicy::Runtime,
            [("external".to_string(), RuntimeValue::Int(40))],
        )
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(42));
    assert_eq!(compiler.diagnostics().len(), 0);
}

#[test]
fn test_compiler_evaluation_service_exports_explicit_host_libraries() {
    let mut host = caap_core::CompilerHost::new();
    host.register_default_runtime_system_libraries().unwrap();
    let mut compiler = host.new_session();
    let mut builder = GraphBuilder::new();
    let callee = builder.name("path.basename");
    let path = builder.literal(IrLiteralData::Str("/tmp/demo.caap".to_string()));
    let root = builder.call(callee, vec![path]);
    builder.graph.root_id = root;
    builder.graph.add_top_level_form(root).unwrap();
    let unit = Unit::from_graph("main.host_eval", std::mem::take(&mut builder.graph)).unwrap();

    let value = compiler
        .evaluation()
        .evaluate_with_host_libraries(
            &unit,
            caap_core::PhasePolicy::Runtime,
            ["path".to_string()],
            [],
        )
        .unwrap();

    assert_eq!(value, RuntimeValue::Str("demo.caap".into()));
}

#[test]
fn test_compiler_evaluation_capture_records_runtime_error_diagnostic() {
    let mut compiler = common::session();
    let graph = parse("(runtime_error \"boom\")").unwrap();
    let unit = Unit::from_graph("main", graph).unwrap();

    let capture = compiler
        .evaluation()
        .evaluate_capture(&unit, caap_core::PhasePolicy::Runtime, [], 0)
        .unwrap();

    assert_eq!(capture.unit_id, "main");
    assert_eq!(capture.value, None);
    assert_eq!(capture.diagnostics.len(), 1);
    assert_eq!(compiler.diagnostics().len(), 1);
    assert_eq!(
        capture.diagnostics[0].code.as_deref(),
        Some("CAAP-RUNTIME-001")
    );
}

#[test]
fn test_compiler_evaluation_registered_unit_uses_catalog_storage() {
    let mut compiler = common::session();
    let graph = parse("(int_add 20 22)").unwrap();
    let unit = Unit::from_graph("main", graph).unwrap();
    compiler.register_unit(unit).unwrap();

    let value = compiler
        .evaluation()
        .evaluate_registered(
            "main",
            caap_core::PhasePolicy::Runtime,
            Vec::<(String, RuntimeValue)>::new(),
        )
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(42));
}

#[test]
fn test_compiler_query_service_requires_registered_stages() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    let err = compiler
        .queries()
        .plan_query("compile_unit", caap_core::PhasePolicy::CompileTime)
        .expect_err("query planning should require registered stages");

    assert_eq!(
        err.to_string(),
        "compiler error: no compiler stages registered"
    );
    assert!(compiler.queries().compile(&mut unit).is_err());
}

#[test]
fn test_compiler_query_service_runs_registered_provider() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    compiler.register_stage("compile_unit").unwrap();
    compiler
        .register_provider_with_effects(
            "mark_compiled",
            "compile_unit",
            caap_core::PhasePolicy::CompileTime,
            ["write_attributes".to_string()],
            |context| context.set_unit_attribute("compiled", caap_core::SemanticValue::Bool(true)),
        )
        .unwrap();

    let plan = compiler.queries().compile(&mut unit).unwrap();

    assert_eq!(plan.target, "compile_unit");
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].provider_names, vec!["mark_compiled"]);
    assert_eq!(
        unit.attributes().get("compiled"),
        Some(&caap_core::SemanticValue::Bool(true))
    );
    assert!(compiler.catalog().contains_unit("main"));
    assert_eq!(compiler.events().by_kind("query.plan").unwrap().len(), 1);
    let provider_event = compiler.events().by_kind("query.provider.finish").unwrap()[0];
    assert_eq!(provider_event.target.as_deref(), Some("mark_compiled"));
    assert!(provider_event
        .metadata
        .iter()
        .any(|(key, value)| key == "elapsed_ms" && value.parse::<f64>().is_ok()));
    let stage_event = compiler.events().by_kind("query.stage.finish").unwrap()[0];
    assert_eq!(stage_event.target.as_deref(), Some("compile_unit"));
    assert!(stage_event
        .metadata
        .iter()
        .any(|(key, value)| key == "elapsed_ms" && value.parse::<f64>().is_ok()));
}
