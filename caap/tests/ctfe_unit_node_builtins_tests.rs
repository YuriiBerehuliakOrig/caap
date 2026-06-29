/// Integration tests for CTFE unit, node, metadata, and surface-form builtins.
///
/// These scenarios validate generic CTFE projection/mutation helpers separately
/// from provider context scheduling and effect enforcement.
use caap_core::{frontend::parse, RuntimeValue, Unit};

mod common;

#[test]
fn test_ctfe_unit_rewrite_report_projects_provider_provenance() {
    let mut compiler = common::session();
    let path =
        std::env::temp_dir().join(format!("caap-rewrite-report-{}.caap", std::process::id()));
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
            (bind query_unit
              (get
                (ctfe_compiler_query_execution
                  compiler
                  \"compile_unit\"
                  source_unit
                  \"compile_time\")
                \"unit\")
            (bind compiler_summary
              (ctfe_unit_rewrite_report query_unit 1)
              (list_of
                (get compiler_summary \"rewritten\")
                (get compiler_summary \"erased\")
                (get (get compiler_summary \"latest\") \"provider_name\")
                (get (get compiler_summary \"latest\") \"stage\")
                (get (get compiler_summary \"latest\") \"operation\")
                (size (get (get compiler_summary \"latest\") \"sources\"))
                (get (get compiler_summary \"latest\") \"provider_name\"))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("rewrite_report_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Bool(false));
    assert_eq!(items[2], RuntimeValue::Str("replace_provider".into()));
    assert_eq!(items[3], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[4], RuntimeValue::Str("replace".into()));
    assert_eq!(items[5], RuntimeValue::Int(1));
    assert_eq!(items[6], RuntimeValue::Str("replace_provider".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_node_rewrite_matches_and_replaces_declaratively() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!("caap-node-rewrite-{}.caap", std::process::id()));
    std::fs::write(
        &path,
        "(int_add 1 2)
",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe_compiler_stage_register compiler \"compile_unit\")
          (ctfe_compiler_provider_register
            compiler
            \"rewrite_provider\"
            \"compile_unit\"
            (lambda (ctx _root)
              (bind unit (ctfe_provider_unit ctx)
              (bind root (get (ctfe_unit_top_level_forms unit) 0)
                (ctfe_provider_node_rewrite
                  ctx
                  root
                  (map_of
                    \"kind\" \"Call\"
                    \"callee\" (map_of \"kind\" \"Name\" \"identifier\" \"int_add\")
                    \"args\"
                      (list_of
                        (map_of \"kind\" \"Literal\" \"bind_value\" \"left\")
                        (map_of \"kind\" \"Literal\" \"bind_value\" \"right\")))
                  (lambda (bindings _node)
                    (ctfe_ir_literal
                      (map_of
                        \"value\"
                        (int_add
                          (get bindings \"left\")
                          (get bindings \"right\")))))))))
            null
            (list_of \"read_ir\" \"write_ir\"))
          (bind source_unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (bind query_unit
              (get
                (ctfe_compiler_query_execution
                  compiler
                  \"compile_unit\"
                  source_unit
                  \"compile_time\")
                \"unit\")
            (bind root (get (ctfe_unit_top_level_forms query_unit) 0)
            (bind report (ctfe_unit_rewrite_report query_unit root)
              (list_of
                (ctfe_node_kind root)
                (ctfe_node_literal_value root)
                (get report \"rewritten\")
                (get (get report \"latest\") \"operation\")
                (get (get report \"latest\") \"provider_name\")))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("node_rewrite_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("Literal".into()));
    assert_eq!(items[1], RuntimeValue::Int(3));
    assert_eq!(items[2], RuntimeValue::Bool(true));
    assert_eq!(items[3], RuntimeValue::Str("rewrite".into()));
    assert_eq!(items[4], RuntimeValue::Str("rewrite_provider".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_unit_facts_and_symbol_projection_builtins() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!("caap-unit-builtins-{}.caap", std::process::id()));
    let file_text = "public_value
";
    std::fs::write(&path, file_text).unwrap();
    let source = format!(
        "(do
          (bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
            (ctfe_unit_set_id! unit \"renamed_unit\")
            (ctfe_unit_add_dependency_binding!
              unit
              (map_of
                \"source_unit\" \"dep\"
                \"source_name\" \"exported\"
                \"local_name\" \"local\"
                \"syntax\" true))
            (ctfe_unit_add_exposed_name! unit \"public_value\")
            (ctfe_unit_syntax_rule_set! unit \"demo_rule\" (map_of \"kind\" \"literal\"))
            (ctfe_unit_syntax_metadata_set! unit \"precedence\" 7)
            (ctfe_unit_syntax_authoring_source_apply!
              unit
              \"add rule authored = symbol -> surface.symbol\")
            (ctfe_unit_syntax_rule_define!
              unit
              \"add rule named_rule = symbol\"
              \"lower_named_rule\")
            (bind symbol
              (sequence_find
                (ctfe_unit_symbols unit)
                (lambda (entry)
                  (eq (get entry \"name\") \"public_value\")))
              (bind facts (ctfe_unit_facts unit)
                (list_of
                  (get symbol \"name\")
                  (not (eq symbol null))
                  (get symbol \"kind\")
                  (ctfe_unit_id unit)
                  (get symbol \"node\")
                  (get (get facts 0) 0)
                  (get (get facts 0) 1)
                  (ctfe_unit_syntax_metadata_get unit \"precedence\")
                  (get (get (get (ctfe_unit_syntax_metadata_get unit \"authored\") \"semantic_hooks\") 0) 0)
                  (get (get (get (ctfe_unit_syntax_metadata_get unit \"named_rule\") \"semantic_hooks\") 0) 1)
                  (get (ctfe_unit_syntax_metadata_get unit \"semantic_hook_functions\") \"lower_named_rule\"))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("unit_builtins", graph).unwrap();

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
    assert_eq!(items[3], RuntimeValue::Str("renamed_unit".into()));
    assert_eq!(items[4], RuntimeValue::Null);
    assert_eq!(items[5], RuntimeValue::Str("symbol:public_value".into()));
    assert_eq!(items[6], RuntimeValue::Str("symbol.entry".into()));
    assert_eq!(items[7], RuntimeValue::Int(7));
    assert_eq!(items[8], RuntimeValue::Str("surface.symbol".into()));
    assert_eq!(items[9], RuntimeValue::Str("lower_named_rule".into()));
    assert_eq!(items[10], RuntimeValue::Str("lower_named_rule".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_unit_syntax_rule_define_inline_node_reads_file_span_source() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-inline-syntax-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &path,
        "(lambda (form) form)
",
    )
    .unwrap();
    let source = format!(
        "(bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
          (bind implementation (get (ctfe_unit_top_level_forms unit) 0)
            (do
              (ctfe_unit_syntax_rule_define_inline_node!
                unit
                \"add rule inline_rule = symbol\"
                implementation)
              (bind metadata (ctfe_unit_syntax_metadata_get unit \"inline_rule\")
                (bind hook_ref (get (get (get metadata \"semantic_hooks\") 0) 0)
                  (list_of
                    hook_ref
                    (get
                      (ctfe_unit_syntax_metadata_get unit \"semantic_hook_inline_sources\")
                      hook_ref)))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("inline_syntax_rule", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    let RuntimeValue::Str(hook_ref) = &items[0] else {
        panic!("expected inline hook ref");
    };
    assert!(hook_ref.starts_with("inline.syntax."));
    assert_eq!(items[1], RuntimeValue::Str("(lambda (form) form)".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_unit_graph_ir_builtins_project_and_mutate_unit_state() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-unit-graph-ir-builtins-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "root_name
",
    )
    .unwrap();
    let source = format!(
        "(bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
          (ctfe_unit_set_id! unit \"graph_unit\")
          (bind root (get (ctfe_unit_top_level_forms unit) 0)
            (ctfe_unit_declare_symbol! unit \"root_name\" \"compile_time\" root \"top_level\")
            (ctfe_unit_set_symbol_semantics! unit \"root_name\" (map_of \"phase_policy\" \"runtime\" \"effect_policy\" \"pure\") root)
            (ctfe_unit_add_exposed_name! unit \"root_name\")
            (ctfe_unit_add_dependency_binding!
              unit
              (map_of
                \"source_unit\" \"dep.unit\"
                \"source_name\" \"dep_name\"
                \"local_name\" \"local_name\"
                \"syntax\" true))
            (bind symbols (ctfe_unit_symbols unit)
              (bind links (ctfe_unit_dependency_bindings unit)
	                (list_of
	                  (ctfe_unit_id unit)
	                  (eq (ctfe_node_id root) (ctfe_node_id (get (ctfe_unit_top_level_forms unit) 0)))
	                  (size (ctfe_unit_top_level_forms unit))
	                  (get (ctfe_unit_node_location unit root) 0)
	                  (get (ctfe_unit_node_location unit root) 1)
	                  (host_value_kind root)
	                  (ctfe_node_kind root)
	                  (ctfe_node_is_name root)
	                  (ctfe_node_name_identifier root)
	                  (ctfe_node_live? root)
	                  (ctfe_meta_fact_set_by_key root \"demo.fact\" \"ok\")
	                  (ctfe_meta_fact_get_by_key root \"demo.fact\")
	                  (ctfe_meta_fact_has_by_key root \"demo.fact\")
	                  (ctfe_meta_annotation_set root \"demo\" \"ann\")
	                  (ctfe_meta_annotation_get root \"demo\")
	                  (not (eq (value_type (ctfe_meta_annotation_get root \"demo\")) \"null\"))
	                  (do
                      (ctfe_meta_annotation_set root \"second\" 2)
                      (ctfe_meta_annotation_set root \"third\" \"v\")
                      (host_value_kind root))
	                  (ctfe_meta_annotation_get root \"second\")
	                  (ctfe_meta_annotation_get root \"missing\" \"fallback\")
	                  (get (get symbols 0) \"name\")
	                  (get (get symbols 0) \"phase_policy\")
	                  (get (get symbols 0) \"public\")
	                  (get (get links 0) \"source_unit\")
	                  (get (get links 0) \"syntax\")
	                  (size (ctfe_unit_exposed_names unit))
	                  (size
                      (sequence_map
                        (ctfe_unit_top_level_symbols unit)
                        (lambda (entry)
                          (get entry \"name\"))))
	                  (size (ctfe_unit_facts unit))
	                  (ctfe_unit_version unit)
	                  (host_value_kind (ctfe_unit_to_template unit))
	                  (ctfe_unit_id (ctfe_unit_template_instantiate (ctfe_unit_to_template unit)))
                    (host_value_kind (ctfe_meta_fact_set_by_key root \"demo.node\" root))
                    (host_value_kind (ctfe_meta_fact_get_by_key root \"demo.node\"))
                    (eq
                      (ctfe_node_id (ctfe_meta_fact_get_by_key root \"demo.node\"))
                      (ctfe_node_id root))
                    (ctfe_meta_annotation_set root \"demo_node\" root)
                    (host_value_kind (ctfe_meta_annotation_get root \"demo_node\"))
                    (eq
                      (ctfe_node_id (ctfe_meta_annotation_get root \"demo_node\"))
                      (ctfe_node_id root)))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("unit_graph_ir_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("graph_unit".into()));
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Int(1));
    assert_eq!(items[3], RuntimeValue::Str("graph_unit".into()));
    assert!(matches!(items[4], RuntimeValue::Int(_)));
    assert_eq!(items[5], RuntimeValue::Str("node".into()));
    assert_eq!(items[6], RuntimeValue::Str("Name".into()));
    assert_eq!(items[7], RuntimeValue::Bool(true));
    assert_eq!(items[8], RuntimeValue::Str("root_name".into()));
    assert_eq!(items[9], RuntimeValue::Bool(true));
    assert_eq!(items[10], RuntimeValue::Str("ok".into()));
    assert_eq!(items[11], RuntimeValue::Str("ok".into()));
    assert_eq!(items[12], RuntimeValue::Bool(true));
    assert_eq!(items[13], RuntimeValue::Str("ann".into()));
    assert_eq!(items[14], RuntimeValue::Str("ann".into()));
    assert_eq!(items[15], RuntimeValue::Bool(true));
    assert_eq!(items[16], RuntimeValue::Str("node".into()));
    assert_eq!(items[17], RuntimeValue::Int(2));
    assert_eq!(items[18], RuntimeValue::Str("fallback".into()));
    assert_eq!(items[19], RuntimeValue::Str("root_name".into()));
    assert_eq!(items[20], RuntimeValue::Str("runtime".into()));
    assert_eq!(items[21], RuntimeValue::Bool(true));
    assert_eq!(items[22], RuntimeValue::Str("dep.unit".into()));
    assert_eq!(items[23], RuntimeValue::Bool(true));
    assert_eq!(items[24], RuntimeValue::Int(1));
    assert_eq!(items[25], RuntimeValue::Int(1));
    match &items[26] {
        RuntimeValue::Int(count) => assert!(*count >= 2),
        other => panic!("expected fact count int, got {other:?}"),
    }
    match &items[27] {
        RuntimeValue::Int(version) => assert!(*version > 0),
        other => panic!("expected unit version int, got {other:?}"),
    }
    assert_eq!(items[28], RuntimeValue::Str("unit_template".into()));
    assert_eq!(items[29], RuntimeValue::Str("graph_unit".into()));
    assert_eq!(items[30], RuntimeValue::Str("node".into()));
    assert_eq!(items[31], RuntimeValue::Str("node".into()));
    assert_eq!(items[32], RuntimeValue::Bool(true));
    assert!(matches!(&items[33], RuntimeValue::HostObject(_)));
    assert_eq!(items[34], RuntimeValue::Str("node".into()));
    assert_eq!(items[35], RuntimeValue::Bool(true));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_meta_fact_has_by_key_does_not_collide_with_fact_value() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-meta-fact-has-collision-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "root_name
",
    )
    .unwrap();
    let source = format!(
        "(bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
          (bind root (get (ctfe_unit_top_level_forms unit) 0)
            (do
              (ctfe_meta_fact_set_by_key
                root
                \"demo.fact\"
                \"__missing_fact_marker__\")
              (list_of
                (ctfe_meta_fact_has_by_key root \"demo.fact\")
                (ctfe_meta_fact_has_by_key root \"demo.missing\")))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("meta_fact_has_collision", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Bool(false));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_unit_top_level_mutation_builtins() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-unit-top-level-mutation-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "first
second
",
    )
    .unwrap();
    let source = format!(
        "(bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
          (bind first (get (ctfe_unit_top_level_forms unit) 0)
            (bind second (get (ctfe_unit_top_level_forms unit) 1)
              (bind appended
                (ctfe_unit_append_top_level!
                  unit
                  (ctfe_ir_literal (map_of \"value\" null)))
                (do
                  (ctfe_unit_set_top_level_forms! unit (list_of second appended))
                  (ctfe_unit_set_root! unit appended)
                  (bind erased (ctfe_unit_erase_detached! unit first)
                    (list_of
                      (size (ctfe_unit_top_level_forms unit))
                      (eq (ctfe_node_id (ctfe_unit_root unit)) (ctfe_node_id appended))
                      (ctfe_node_live? first)
                      (size erased)
                      (ctfe_node_name_identifier (get (ctfe_unit_top_level_forms unit) 0))
                      (ctfe_node_is_literal (get (ctfe_unit_top_level_forms unit) 1)))))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("unit_top_level_mutation_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(2));
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Bool(false));
    assert_eq!(items[3], RuntimeValue::Int(1));
    assert_eq!(items[4], RuntimeValue::Str("second".into()));
    assert_eq!(items[5], RuntimeValue::Bool(true));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_node_graph_ir_builtins_project_call_tree() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-node-graph-ir-builtins-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(int_add 1 2)
",
    )
    .unwrap();
    let source = format!(
        "(bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
          (bind root (get (ctfe_unit_top_level_forms unit) 0)
            (bind callee (ctfe_node_call_callee root)
              (bind args (ctfe_node_call_args root)
                (bind first_arg (get args 0)
                  (list_of
                    (ctfe_node_kind root)
                    (ctfe_node_is_call root)
                    (ctfe_node_name_identifier callee)
                    (size (ctfe_node_children root))
                    (size args)
                    (ctfe_node_literal_value first_arg)
                    (ctfe_node_ancestor? first_arg root)
                    (eq (ctfe_node_id root) (ctfe_node_id (ctfe_node_parent first_arg)))
                    (not (eq (ctfe_node_call_semantics root \"caap.fact.call_semantics\") null))
                    (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"builtin_name\")
                    (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"eval_policy\")
                    (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"effect_policy\")
                    (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"short_circuit_policy\")
                    (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"min_arity\")
                    (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"max_arity\")
                    (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"phase_policy\")
                    (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"control_policy\")
                    (host_value_kind (ctfe_node_to_spec root))
                    (host_value_kind
                      (ctfe_meta_fact_set_by_key
                        first_arg
                        \"caap.fact.resolved_block\"
                        (map_of \"block_id\" (ctfe_node_id root))))
                    (eq
                      (ctfe_node_id (ctfe_node_resolved_block first_arg \"caap.fact.resolved_block\" null))
                      (ctfe_node_id root))))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("node_graph_ir_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("Call".into()));
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Str("int_add".into()));
    assert_eq!(items[3], RuntimeValue::Int(3));
    assert_eq!(items[4], RuntimeValue::Int(2));
    assert_eq!(items[5], RuntimeValue::Int(1));
    assert_eq!(items[6], RuntimeValue::Bool(true));
    assert_eq!(items[7], RuntimeValue::Bool(true));
    assert_eq!(items[8], RuntimeValue::Bool(true));
    assert_eq!(items[9], RuntimeValue::Str("int_add".into()));
    assert_eq!(items[10], RuntimeValue::Str("eager".into()));
    assert_eq!(items[11], RuntimeValue::Str("pure".into()));
    assert_eq!(items[12], RuntimeValue::Str("none".into()));
    assert_eq!(items[13], RuntimeValue::Int(2));
    assert_eq!(items[14], RuntimeValue::Int(2));
    assert_eq!(items[15], RuntimeValue::Str("dual".into()));
    assert_eq!(items[16], RuntimeValue::Str("plain".into()));
    assert_eq!(items[17], RuntimeValue::Str("expr_spec".into()));
    assert_eq!(items[18], RuntimeValue::Str("map".into()));
    assert_eq!(items[19], RuntimeValue::Bool(true));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_node_match_declaratively_matches_live_call_tree() {
    let mut compiler = common::session();
    let path =
        std::env::temp_dir().join(format!("caap-node-match-live-{}.caap", std::process::id()));
    std::fs::write(
        &path,
        "(int_add 1 2)
",
    )
    .unwrap();
    let source = format!(
        "(bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
          (bind root (get (ctfe_unit_top_level_forms unit) 0)
            (bind result
              (ctfe_node_match
                root
                (map_of
                  \"kind\" \"Call\"
                  \"bind\" \"call\"
                  \"callee\"
                    (map_of
                      \"kind\" \"Name\"
                      \"identifier\" \"int_add\"
                      \"bind\" \"callee\")
                  \"args\"
                    (list_of
                      (map_of
                        \"kind\" \"Literal\"
                        \"value\" 1
                        \"bind_value\" \"left\")
                      (map_of
                        \"kind\" \"Literal\"
                        \"bind\" \"right_node\"
                        \"bind_value\" \"right\"))))
              (bind bindings (get result \"bindings\")
                (list_of
                  (get result \"matched\")
                  (ctfe_node_kind (get bindings \"call\"))
                  (ctfe_node_name_identifier (get bindings \"callee\"))
                  (get bindings \"left\")
                  (get bindings \"right\")
                  (ctfe_node_kind (get bindings \"right_node\")))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("node_match_live", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Str("Call".into()));
    assert_eq!(items[2], RuntimeValue::Str("int_add".into()));
    assert_eq!(items[3], RuntimeValue::Int(1));
    assert_eq!(items[4], RuntimeValue::Int(2));
    assert_eq!(items[5], RuntimeValue::Str("Literal".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_node_match_supports_detached_specs_and_clears_failed_bindings() {
    let mut compiler = common::session();
    let path =
        std::env::temp_dir().join(format!("caap-node-match-spec-{}.caap", std::process::id()));
    std::fs::write(
        &path,
        "(int_add 1 2)
",
    )
    .unwrap();
    let source = format!(
        "(bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
          (bind spec (ctfe_node_to_spec (get (ctfe_unit_top_level_forms unit) 0))
            (bind ok
              (ctfe_node_match
                spec
                (map_of
                  \"kind\" \"Call\"
                  \"children\"
                    (list_of
                      (map_of \"kind\" \"Name\" \"bind_identifier\" \"callee_name\")
                      (map_of \"kind\" \"Literal\" \"value\" 1)
                      (map_of \"kind\" \"Literal\" \"bind_value\" \"rhs\"))))
              (bind fail
                (ctfe_node_match
                  spec
                  (map_of
                    \"kind\" \"Call\"
                    \"bind\" \"call\"
                    \"args\" (list_of (map_of \"kind\" \"Name\"))))
                (list_of
                  (get ok \"matched\")
                  (get (get ok \"bindings\") \"callee_name\")
                  (get (get ok \"bindings\") \"rhs\")
                  (get fail \"matched\")
                  (size (map_keys (get fail \"bindings\"))))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("node_match_spec", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Str("int_add".into()));
    assert_eq!(items[2], RuntimeValue::Int(2));
    assert_eq!(items[3], RuntimeValue::Bool(false));
    assert_eq!(items[4], RuntimeValue::Int(0));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_node_call_semantics_projects_stored_fact_for_non_builtin_call() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-node-call-semantics-fact-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(local_call 1 2)
",
    )
    .unwrap();
    let source = format!(
        "(bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
          (bind root (get (ctfe_unit_top_level_forms unit) 0)
            (bind semantics
              (assoc
                (map_of)
                \"callee_class\" \"function\"
                \"phase_policy\" \"runtime\"
                \"eval_policy\" \"eager\"
                \"control_policy\" \"plain\"
                \"scope_policy\" \"lexical\"
                \"effect_policy\" \"pure\"
                \"short_circuit_policy\" \"none\"
                \"builtin_name\" null
                \"min_arity\" null
                \"max_arity\" null)
              (do
                (ctfe_meta_fact_set_by_key
                  root
                  \"caap.fact.call_semantics\"
                  semantics)
                (list_of
                  (not (eq (ctfe_node_call_semantics root \"caap.fact.call_semantics\") null))
                  (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"callee_class\")
                  (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"builtin_name\")
                  (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"min_arity\")
                  (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"max_arity\")
                  (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"eval_policy\")
                  (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"effect_policy\")
                  (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"short_circuit_policy\")
                  (get (ctfe_node_call_semantics root \"caap.fact.call_semantics\") \"scope_policy\"))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("node_call_semantics_fact", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Str("function".into()));
    assert_eq!(items[2], RuntimeValue::Null);
    assert_eq!(items[3], RuntimeValue::Null);
    assert_eq!(items[4], RuntimeValue::Null);
    assert_eq!(items[5], RuntimeValue::Str("eager".into()));
    assert_eq!(items[6], RuntimeValue::Str("pure".into()));
    assert_eq!(items[7], RuntimeValue::Str("none".into()));
    assert_eq!(items[8], RuntimeValue::Str("lexical".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_surface_form_builtins_construct_parse_and_collect_bindings() {
    let mut compiler = common::session();
    let source = r#"
        (bind span (map_of
            "start" 0
            "end" 9
            "start_line" 1
            "start_col" 1
            "end_line" 1
            "end_col" 10)
          (bind head (ctfe_surface_form_symbol "head" span)
            (bind item (ctfe_surface_form_integer "42" span)
              (bind form (ctfe_surface_form_list (list_of head item) span)
                (bind prepended (ctfe_surface_form_list_prepend head (list_of item) span "brace")
                  (bind parsed (ctfe_surface_parse_form "(head 42)" null)
                    (bind reparsed_int (ctfe_surface_reparse_text "integer" "123")
                      (bind reparsed_list (ctfe_surface_reparse_text "list" "(head 9)")
                        (bind reparsed_forms (ctfe_surface_reparse_text "forms" "(a) (b)")
                          (bind group (map_of "first" head "rest" (list_of (map_of "item" item)))
                            (bind collected (ctfe_surface_binding_group_collect group "item")
                              (list_of
                                (get head "kind")
                                (get head "value")
                                (get form "head")
                                (size (get form "items"))
                                (get prepended "delimiter")
                                (get parsed "head")
                                (get (get (get parsed "items") 1) "value")
                                (get reparsed_int "kind")
                                (get reparsed_int "value")
                                (get reparsed_list "head")
                                (size reparsed_forms)
                                (get (get reparsed_forms 1) "head")
                                (size collected)
                                (get (get collected 1) "kind")
                                (ctfe_surface_binding_get group "missing" "fallback")
                                (ctfe_surface_binding_get (map_of "missing" null) "missing" "fallback_null")))))))))))))"#;
    let graph = parse(source).unwrap();
    let unit = Unit::from_graph("surface_form_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("symbol".into()));
    assert_eq!(items[1], RuntimeValue::Str("head".into()));
    assert_eq!(items[2], RuntimeValue::Str("head".into()));
    assert_eq!(items[3], RuntimeValue::Int(2));
    assert_eq!(items[4], RuntimeValue::Str("brace".into()));
    assert_eq!(items[5], RuntimeValue::Str("head".into()));
    assert_eq!(items[6], RuntimeValue::Int(42));
    assert_eq!(items[7], RuntimeValue::Str("integer".into()));
    assert_eq!(items[8], RuntimeValue::Int(123));
    assert_eq!(items[9], RuntimeValue::Str("head".into()));
    assert_eq!(items[10], RuntimeValue::Int(2));
    assert_eq!(items[11], RuntimeValue::Str("b".into()));
    assert_eq!(items[12], RuntimeValue::Int(2));
    assert_eq!(items[13], RuntimeValue::Str("integer".into()));
    assert_eq!(items[14], RuntimeValue::Str("fallback".into()));
    assert_eq!(items[15], RuntimeValue::Str("fallback_null".into()));
}

#[test]
fn ctfe_unit_node_span_reports_source_location_when_present() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!("caap-node-span-{}.caap", std::process::id()));
    // The form is on line 3, so its span's start_line is observable.
    std::fs::write(&path, "\n\n(int_add 1 2)\n").unwrap();
    let source = format!(
        "(bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
          (bind root (get (ctfe_unit_top_level_forms unit) 0)
            (bind span (ctfe_unit_node_span unit root)
              (list_of
                (value_type span)
                (get span \"start_line\" 0)
                (get span \"start_col\" 0)))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("node_span_test", graph).unwrap();
    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();
    let _ = std::fs::remove_file(&path);

    let RuntimeValue::List(items) = value else {
        panic!("expected list result, got {value:?}");
    };
    let items = items.borrow();
    // A span is present (a map, not null) for this hand-written form.
    assert_eq!(
        items[0],
        RuntimeValue::Str("map".into()),
        "span should be a map"
    );
    // start_line / start_col are positive 1-based coordinates.
    let RuntimeValue::Int(line) = items[1] else {
        panic!("start_line not an int: {:?}", items[1]);
    };
    assert!(line >= 1, "start_line should be >= 1, got {line}");
}
