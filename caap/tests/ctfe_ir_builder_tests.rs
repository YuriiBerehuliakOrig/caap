/// Integration tests for CTFE IR construction and builder-facing surface helpers.
///
/// These scenarios exercise compile-time IR mechanisms without mixing them into
/// provider scheduling/effect tests.
use caap_core::{frontend::parse, RuntimeValue, Unit};

mod common;

#[test]
fn per_kind_builders_build_typed_expr_specs() {
    let mut compiler = common::session();
    let graph = parse(
        "(bind name_spec (ctfe_ir_name (map_of \"identifier\" \"x\"))
          (bind literal_spec (ctfe_ir_literal (map_of \"value\" 42))
            (bind call_spec
              (ctfe_ir_call
                (map_of \"callee\" name_spec \"args\" (list_of literal_spec)))
              (bind leave_name (ctfe_ir_name (map_of \"identifier\" \"leave\"))
                (bind block_name (ctfe_ir_name (map_of \"identifier\" \"block\"))
                (bind block_spec
                  (ctfe_ir_call
                    (map_of
                      \"callee\"
                      block_name
                      \"args\"
                      (list_of literal_spec call_spec)))
                  (ctfe_ir_call
                    (map_of
                      \"callee\"
                      leave_name
                      \"args\"
                      (list_of
                        (ctfe_ir_literal (map_of \"value\" \"exit\"))
                        block_spec)))))))))",
    )
    .unwrap();
    let unit = Unit::from_graph("ir_instantiate_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::HostObject(object) = value else {
        panic!("expected ExprSpec host object");
    };
    let spec = object
        .as_any()
        .downcast_ref::<caap_core::builtins::ir_builders::ExprSpecBridgeValue>()
        .expect("expected ExprSpecBridgeValue")
        .spec();
    let caap_core::ExprSpec::Call(leave_call) = spec else {
        panic!("expected leave call spec");
    };
    let caap_core::ExprSpec::Name(callee) = leave_call.callee.as_ref() else {
        panic!("expected leave callee name");
    };
    assert_eq!(callee.identifier, "leave");
    assert_eq!(leave_call.args.len(), 2);
    assert_eq!(
        leave_call.args[0],
        caap_core::ExprSpec::literal(caap_core::IrLiteralData::Str("exit".to_string()))
    );
    let caap_core::ExprSpec::Call(block_call) = &leave_call.args[1] else {
        panic!("expected block call spec");
    };
    let caap_core::ExprSpec::Name(block_callee) = block_call.callee.as_ref() else {
        panic!("expected block callee name");
    };
    assert_eq!(block_callee.identifier, "block");
}

#[test]
fn test_ctfe_ir_detached_expr_specs_support_source_span_annotations() {
    let mut compiler = common::session();
    let graph = parse(
        r#"(bind span (map_of
            "start" 2
            "end" 9
            "start_line" 1
            "start_col" 3
            "end_line" 1
            "end_col" 10)
          (bind name_spec
            (ctfe_ir_name
              (map_of "identifier" "spanned_name")
              (map_of "source_span" span))
            (bind literal_spec
              (ctfe_ir_literal (map_of "value" 42))
              (bind set_result (ctfe_meta_annotation_set literal_spec "source_span" span)
              (bind literal_span (ctfe_meta_annotation_get literal_spec "source_span")
                (bind literal_has
                  (not (eq (value_type (ctfe_meta_annotation_get literal_spec "source_span")) "null"))
                  (ctfe_meta_annotation_set literal_spec "source_span" null)
                  (list_of
                    (not (eq (value_type (ctfe_meta_annotation_get name_spec "source_span")) "null"))
                    (get (ctfe_meta_annotation_get name_spec "source_span") "start")
                    literal_has
                    (get literal_span "end_col")
                    (not (eq (value_type (ctfe_meta_annotation_get literal_spec "source_span")) "null"))
                    (ctfe_node_is_literal set_result))))))))"#,
    )
    .unwrap();
    let unit = Unit::from_graph("detached_expr_spec_source_span", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    assert_eq!(
        items.borrow().as_slice(),
        [
            RuntimeValue::Bool(true),
            RuntimeValue::Int(2),
            RuntimeValue::Bool(true),
            RuntimeValue::Int(10),
            RuntimeValue::Bool(false),
            RuntimeValue::Bool(true),
        ]
    );
}

#[test]
fn test_ctfe_node_builtins_accept_detached_expr_specs() {
    let mut compiler = common::session();
    let graph = parse(
        "(bind name_spec (ctfe_ir_name (map_of \"identifier\" \"demo_call\"))
          (bind literal_spec (ctfe_ir_literal (map_of \"value\" 42))
            (bind call_spec
              (ctfe_ir_call
                (map_of \"callee\" name_spec \"args\" (list_of literal_spec)))
              (list_of
                (ctfe_node_kind call_spec)
                (ctfe_node_is_call call_spec)
                (ctfe_node_is_name name_spec)
                (ctfe_node_is_literal literal_spec)
                (ctfe_node_id call_spec)
                (ctfe_node_parent call_spec)
                (size (ctfe_node_children call_spec))
                (ctfe_node_name_identifier (ctfe_node_call_callee call_spec))
                (ctfe_node_literal_value (get (ctfe_node_call_args call_spec) 0 null))))))",
    )
    .unwrap();
    let unit = Unit::from_graph("detached_expr_spec_node_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    assert_eq!(
        items.borrow().as_slice(),
        [
            RuntimeValue::Str("Call".into()),
            RuntimeValue::Bool(true),
            RuntimeValue::Bool(true),
            RuntimeValue::Bool(true),
            RuntimeValue::Null,
            RuntimeValue::Null,
            RuntimeValue::Int(2),
            RuntimeValue::Str("demo_call".into()),
            RuntimeValue::Int(42),
        ]
    );
}

#[test]
fn test_ctfe_eval_node_runs_constructed_ir_at_compile_time() {
    let mut compiler = common::session();
    let graph = parse(
        "(ctfe_eval_node
           (ctfe_ir_call
             (map_of
               \"callee\" (ctfe_ir_name (map_of \"identifier\" \"int_add\"))
               \"args\" (list_of
                          (ctfe_ir_literal (map_of \"value\" 2))
                          (ctfe_ir_literal (map_of \"value\" 3))))))",
    )
    .unwrap();
    let unit = Unit::from_graph("eval_node_add", graph).unwrap();
    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();
    assert_eq!(value, RuntimeValue::Int(5));
}

#[test]
fn test_ctfe_eval_node_sees_current_environment() {
    let mut compiler = common::session();
    let graph = parse(
        "(bind x 7
           (ctfe_eval_node (ctfe_ir_name (map_of \"identifier\" \"x\"))))",
    )
    .unwrap();
    let unit = Unit::from_graph("eval_node_env", graph).unwrap();
    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();
    assert_eq!(value, RuntimeValue::Int(7));
}

#[test]
fn test_ctfe_eval_node_rejects_non_ir_node() {
    let mut compiler = common::session();
    let graph = parse("(ctfe_eval_node 42)").unwrap();
    let unit = Unit::from_graph("eval_node_bad", graph).unwrap();
    let error = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap_err()
        .to_string();
    assert!(error.contains("IR node"), "{error}");
}

/// The per-kind IR builders (`ctfe_ir_name` / `ctfe_ir_literal` / `ctfe_ir_call`)
/// are the only IR-construction spelling (the string-selector wrapper is gone).
/// Build `(int_add 2 3)`
/// from them and evaluate it, proving all three construct real IR.
#[test]
fn per_kind_ir_builders_construct_and_evaluate() {
    let mut compiler = common::session();
    let graph = parse(
        "(ctfe_eval_node
           (ctfe_ir_call
             (map_of
               \"callee\" (ctfe_ir_name (map_of \"identifier\" \"int_add\"))
               \"args\" (list_of
                          (ctfe_ir_literal (map_of \"value\" 2))
                          (ctfe_ir_literal (map_of \"value\" 3))))))",
    )
    .unwrap();
    let unit = Unit::from_graph("per_kind_ir_builders", graph).unwrap();
    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();
    assert_eq!(value, RuntimeValue::Int(5));
}

/// C3 span-setter: synthesized (data-built) specs are span-less; the setter
/// gives them a location — from a span map, from a donor spec, or clears it.
#[test]
fn ctfe_spec_with_span_sets_copies_and_clears_root_spans() {
    let mut compiler = common::session();
    let graph = parse(
        "(bind bare (ctfe_ir_literal (map_of \"value\" 1))
          (bind span_map (map_of
              \"start\" 10 \"end\" 14
              \"start_line\" 2 \"start_col\" 3
              \"end_line\" 2 \"end_col\" 7
              \"path\" \"demo.clike\")
            (bind located (ctfe_spec_with_span bare span_map)
              (bind donated (ctfe_spec_with_span (ctfe_ir_name (map_of \"identifier\" \"x\")) located)
                (bind cleared (ctfe_spec_with_span located null)
                  (list_of
                    (ctfe_spec_span bare)
                    (get (ctfe_spec_span located) \"start\")
                    (get (ctfe_spec_span located) \"path\")
                    (get (ctfe_spec_span donated) \"end_col\")
                    (ctfe_spec_span cleared)))))))",
    )
    .unwrap();
    let unit = Unit::from_graph("spec_with_span", graph).unwrap();
    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();
    let RuntimeValue::List(items) = value else {
        panic!("expected list");
    };
    let items = items.borrow();
    assert_eq!(
        items[0],
        RuntimeValue::Null,
        "data-built specs are span-less"
    );
    assert_eq!(items[1], RuntimeValue::Int(10), "span map applied");
    assert_eq!(
        items[2],
        RuntimeValue::Str("demo.clike".into()),
        "path kept"
    );
    assert_eq!(items[3], RuntimeValue::Int(7), "donor span copied");
    assert_eq!(items[4], RuntimeValue::Null, "null clears the span");
}

#[test]
fn ctfe_spec_with_span_survives_eval_and_rejects_garbage() {
    let mut compiler = common::session();
    // The located spec still evaluates like the bare one (spans are metadata).
    let graph = parse(
        "(ctfe_eval_node
           (ctfe_spec_with_span
             (ctfe_ir_literal (map_of \"value\" 42))
             (map_of \"start\" 0 \"end\" 1 \"start_line\" 1 \"start_col\" 1
                     \"end_line\" 1 \"end_col\" 2)))",
    )
    .unwrap();
    let unit = Unit::from_graph("spec_with_span_eval", graph).unwrap();
    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();
    assert_eq!(value, RuntimeValue::Int(42));

    let graph = parse("(ctfe_spec_with_span (ctfe_ir_literal (map_of \"value\" 1)) 99)").unwrap();
    let unit = Unit::from_graph("spec_with_span_bad", graph).unwrap();
    let error = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap_err()
        .to_string();
    assert!(error.contains("span map"), "{error}");
}
