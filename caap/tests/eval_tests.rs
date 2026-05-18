/// Integration tests for the CAAP core Rust port evaluator.
///
/// Each test builds a small IR graph by hand and runs it through the evaluator,
/// mirroring the assertions in `caap/tests/` Python test suite.
use caap_core_port::{
    frontend::parse,
    graph::GraphBuilder,
    ir::{IrLiteralData, Node, NodeId},
    node_subject_id,
    values::Environment,
    Evaluator, MapKey, PhasePolicy, QueryArtifactSource, QueryExecutionOptions, RuntimeValue,
    SemanticValue, SourceSpan, Unit,
};
use std::collections::BTreeMap;
use std::rc::Rc;

// ── helpers ───────────────────────────────────────────────────────────────────

fn lit_int(v: i64) -> IrLiteralData {
    IrLiteralData::Int(v)
}

fn lit_bool(v: bool) -> IrLiteralData {
    IrLiteralData::Bool(v)
}

fn lit_null() -> IrLiteralData {
    IrLiteralData::Null
}

fn eval_one(b: &mut GraphBuilder, root_id: u32) -> RuntimeValue {
    b.graph.root_id = root_id;
    let env = Environment::new(None);
    let mut ev = Evaluator::new(std::mem::take(&mut b.graph));
    ev.eval(root_id, &env).expect("evaluation failed")
}

fn top_level_head(unit: &Unit, node_id: NodeId) -> Option<String> {
    let Node::Call(call) = unit.ir().node(node_id)? else {
        return None;
    };
    let Node::Name(callee) = unit.ir().node(call.callee)? else {
        return None;
    };
    Some(callee.identifier.to_string())
}

#[test]
fn test_unit_empty_has_no_top_level_forms() {
    let unit = Unit::empty("test.empty").expect("unit construction failed");
    assert_eq!(unit.unit_id(), "test.empty");
    assert!(unit.is_empty());
    assert_eq!(unit.top_level_form_ids().len(), 0);
}

#[test]
fn test_query_artifact_source_unit_variant_stays_boxed() {
    assert!(std::mem::size_of::<QueryArtifactSource>() < std::mem::size_of::<Unit>());
}

#[test]
fn test_runtime_value_is_single_threaded_by_design() {
    static_assertions::assert_not_impl_any!(RuntimeValue: Send, Sync);
}

#[test]
fn test_ir_name_constructor_rejects_empty_identifier() {
    assert!(caap_core_port::NameNode::new(0, "").is_err());
}

#[test]
fn test_ir_literal_dict_constructor_sorts_keys() {
    let data = IrLiteralData::dict(vec![
        ("z".to_string(), lit_int(1)),
        ("a".to_string(), lit_int(2)),
    ])
    .expect("dict literal construction failed");
    assert_eq!(
        data,
        IrLiteralData::Dict(vec![
            ("a".to_string(), lit_int(2)),
            ("z".to_string(), lit_int(1)),
        ])
    );
}

#[test]
fn test_ir_literal_dict_constructor_rejects_empty_key() {
    assert!(IrLiteralData::dict(vec![("".to_string(), lit_int(1))]).is_err());
}

#[test]
fn test_graph_builder_try_call_rejects_missing_children() {
    let mut b = GraphBuilder::new();
    let callee = b.name("id");
    assert!(b.try_call(callee, vec![999]).is_err());
}

#[test]
fn test_ir_graph_add_top_level_form_rejects_missing_node() {
    let mut b = GraphBuilder::new();
    let err = b
        .graph
        .add_top_level_form(999)
        .expect_err("missing top-level node should be reported");

    assert!(err.to_string().contains("does not exist"));
}

#[test]
fn test_expr_spec_rejects_empty_name() {
    assert!(caap_core_port::ExprSpec::name("").is_err());
}

#[test]
fn test_graph_builder_lowers_detached_expr_spec_with_spans() {
    let call_span = SourceSpan::new(0, 7, 1, 1, 1, 8).unwrap();
    let arg_span = SourceSpan::new(5, 6, 1, 6, 1, 7).unwrap();
    let spec = caap_core_port::ExprSpec::call_with_span(
        caap_core_port::ExprSpec::name("id").unwrap(),
        vec![caap_core_port::ExprSpec::literal_with_span(
            lit_int(1),
            Some(arg_span.clone()),
        )],
        Some(call_span.clone()),
    );
    let mut b = GraphBuilder::new();

    let root = b.graph.root_id;
    let lowered = b.lower_spec(&spec).unwrap();
    b.graph.root_id = lowered;
    b.graph.add_top_level_form(lowered).unwrap();

    assert_eq!(root, 0);
    assert_eq!(lowered, 0);
    assert_eq!(b.graph.source_span(lowered), Some(&call_span));
    b.graph.validate_integrity().unwrap();
    match b.graph.node(lowered).unwrap() {
        caap_core_port::Node::Call(call) => {
            assert_eq!(call.callee, 1);
            assert_eq!(call.args, vec![2]);
            assert_eq!(b.graph.parent(call.callee), Some(Some(lowered)));
            assert_eq!(b.graph.parent(call.args[0]), Some(Some(lowered)));
            assert_eq!(b.graph.source_span(call.args[0]), Some(&arg_span));
        }
        node => panic!("expected lowered call, got {node:?}"),
    }
}

#[test]
fn test_ir_graph_template_roundtrips_graph_state() {
    let mut b = GraphBuilder::new();
    let callee = b.name("id");
    let arg = b.literal(lit_int(42));
    let call = b.call(callee, vec![arg]);
    b.graph.root_id = call;
    b.graph.add_top_level_form(call).unwrap();
    b.graph
        .set_source_span(call, SourceSpan::new(0, 7, 1, 1, 1, 8).unwrap())
        .unwrap();

    let template = b.graph.to_template();
    template.validate().expect("template validation failed");
    let restored = caap_core_port::IRGraph::from_template(template).expect("restore failed");

    assert_eq!(restored.root_id, call);
    assert_eq!(restored.top_level_form_ids(), &[call]);
    assert!(restored.source_span(call).is_some());
    assert_eq!(restored.node_count(), 3);
}

#[test]
fn test_ir_graph_template_rejects_missing_call_child() {
    let template = caap_core_port::IRGraphTemplate {
        root_id: 0,
        nodes: vec![caap_core_port::Node::Call(caap_core_port::CallNode::new(
            0,
            99,
            vec![],
        ))],
        parents: vec![(0, None)],
        source_spans: vec![],
        top_level_forms: vec![0],
        next_id: 1,
    };

    assert!(template.validate().is_err());
}

#[test]
fn test_ir_graph_template_rejects_unlisted_parentless_nodes() {
    let template = caap_core_port::IRGraphTemplate {
        root_id: 0,
        nodes: vec![
            caap_core_port::Node::Literal(caap_core_port::LiteralNode::new(0, lit_int(1))),
            caap_core_port::Node::Literal(caap_core_port::LiteralNode::new(1, lit_int(2))),
        ],
        parents: vec![(0, None), (1, None)],
        source_spans: vec![],
        top_level_forms: vec![0],
        next_id: 2,
    };

    assert!(template.validate().is_err());
}

#[test]
fn test_ir_graph_template_rejects_parent_that_does_not_reference_child() {
    let template = caap_core_port::IRGraphTemplate {
        root_id: 0,
        nodes: vec![
            caap_core_port::Node::Literal(caap_core_port::LiteralNode::new(0, lit_int(1))),
            caap_core_port::Node::Literal(caap_core_port::LiteralNode::new(1, lit_int(2))),
        ],
        parents: vec![(0, None), (1, Some(0))],
        source_spans: vec![],
        top_level_forms: vec![0],
        next_id: 2,
    };

    assert!(template.validate().is_err());
}

#[test]
fn test_ir_graph_template_rejects_child_parent_mismatch() {
    let template = caap_core_port::IRGraphTemplate {
        root_id: 0,
        nodes: vec![
            caap_core_port::Node::Call(caap_core_port::CallNode::new(0, 1, vec![])),
            caap_core_port::Node::Literal(caap_core_port::LiteralNode::new(1, lit_int(1))),
        ],
        parents: vec![(0, None), (1, None)],
        source_spans: vec![],
        top_level_forms: vec![0, 1],
        next_id: 2,
    };

    assert!(template.validate().is_err());
}

#[test]
fn test_ir_graph_top_level_insert_replace_remove() {
    let mut b = GraphBuilder::new();
    let one = b.literal(lit_int(1));
    let two = b.literal(lit_int(2));
    let three = b.literal(lit_int(3));
    let four = b.literal(lit_int(4));

    b.graph.add_top_level_form(one).unwrap();
    b.graph.insert_top_level_after(one, three).unwrap();
    b.graph.insert_top_level_before(three, two).unwrap();
    assert_eq!(b.graph.top_level_form_ids(), &[one, two, three]);
    assert!(b.graph.has_top_level_form(two));

    b.graph.replace_top_level_form(two, four).unwrap();
    assert_eq!(b.graph.top_level_form_ids(), &[one, four, three]);
    assert!(!b.graph.has_top_level_form(two));
    assert!(b.graph.has_top_level_form(four));

    assert!(b.graph.remove_top_level_form(one));
    assert_eq!(b.graph.top_level_form_ids(), &[four, three]);
    assert!(!b.graph.has_top_level_form(one));
}

#[test]
fn test_ir_graph_top_level_insert_rejects_missing_anchor() {
    let mut b = GraphBuilder::new();
    let node = b.literal(lit_int(1));
    assert!(b.graph.insert_top_level_before(999, node).is_err());
}

#[test]
fn test_ir_graph_top_level_rejects_attached_or_duplicate_forms() {
    let mut b = GraphBuilder::new();
    let callee = b.name("id");
    let arg = b.literal(lit_int(1));
    let call = b.call(callee, vec![arg]);
    b.graph.add_top_level_form(call).unwrap();

    assert!(b.graph.set_top_level_form_ids(vec![call, call]).is_err());
    assert!(b.graph.insert_top_level_after(call, arg).is_err());
    assert!(b.graph.insert_top_level_after(call, call).is_err());
}

#[test]
fn test_ir_graph_replace_node_validates_id_and_child_links() {
    let mut b = GraphBuilder::new();
    let old = b.literal(lit_int(1));
    assert!(b
        .graph
        .replace_node(
            old,
            caap_core_port::Node::Literal(caap_core_port::LiteralNode::new(old, lit_int(2)))
        )
        .is_ok());
    assert!(b
        .graph
        .replace_node(
            old,
            caap_core_port::Node::Literal(caap_core_port::LiteralNode::new(old + 1, lit_int(3)))
        )
        .is_err());
}

#[test]
fn test_ir_graph_delete_node_rejects_attached_or_parent_nodes() {
    let mut b = GraphBuilder::new();
    let callee = b.name("id");
    let arg = b.literal(lit_int(1));
    let call = b.call(callee, vec![arg]);

    assert!(b.graph.delete_node(arg).is_err());
    assert!(b.graph.delete_node(call).is_err());

    let detached = b.literal(lit_int(9));
    assert_eq!(b.graph.delete_node(detached), Ok(true));
    assert!(!b.graph.contains(detached));
}

#[test]
fn test_ir_graph_erase_detached_subtree_drops_descendants() {
    let mut b = GraphBuilder::new();
    let callee = b.name("id");
    let arg = b.literal(lit_int(1));
    let call = b.call(callee, vec![arg]);
    b.graph.add_top_level_form(call).unwrap();

    let dropped = b.graph.erase_detached_subtree(call).unwrap();

    assert_eq!(dropped.len(), 3);
    assert!(!b.graph.contains(call));
    assert!(!b.graph.contains(callee));
    assert!(!b.graph.contains(arg));
    assert!(b.graph.top_level_form_ids().is_empty());
}

#[test]
fn test_ir_graph_erase_detached_subtree_rejects_attached_child() {
    let mut b = GraphBuilder::new();
    let callee = b.name("id");
    let arg = b.literal(lit_int(1));
    let _call = b.call(callee, vec![arg]);

    assert!(b.graph.erase_detached_subtree(arg).is_err());
}

#[test]
fn test_ir_graph_replace_subtree_updates_top_level_and_root() {
    let mut b = GraphBuilder::new();
    let callee = b.name("id");
    let arg = b.literal(lit_int(1));
    let old_call = b.call(callee, vec![arg]);
    let replacement = b.literal(lit_int(2));
    b.graph.root_id = old_call;
    b.graph.add_top_level_form(old_call).unwrap();

    let dropped = b.graph.replace_subtree(old_call, replacement).unwrap();

    assert_eq!(dropped.len(), 3);
    assert_eq!(b.graph.root_id, replacement);
    assert_eq!(b.graph.top_level_form_ids(), &[replacement]);
    assert!(!b.graph.contains(old_call));
    assert!(!b.graph.contains(callee));
    assert!(!b.graph.contains(arg));
    assert!(b.graph.contains(replacement));
}

#[test]
fn test_ir_graph_replace_subtree_updates_parent_call_child() {
    let mut b = GraphBuilder::new();
    let inner_callee = b.name("id");
    let inner_arg = b.literal(lit_int(1));
    let old_inner = b.call(inner_callee, vec![inner_arg]);
    let outer_callee = b.name("wrap");
    let outer = b.call(outer_callee, vec![old_inner]);
    let replacement = b.literal(lit_int(2));
    b.graph.root_id = outer;
    b.graph.add_top_level_form(outer).unwrap();

    let dropped = b.graph.replace_subtree(old_inner, replacement).unwrap();

    assert_eq!(dropped.len(), 3);
    assert_eq!(b.graph.parent(replacement), Some(Some(outer)));
    assert!(!b.graph.contains(old_inner));
    assert!(!b.graph.contains(inner_callee));
    assert!(!b.graph.contains(inner_arg));
    match b.graph.node(outer).unwrap() {
        caap_core_port::Node::Call(call) => assert_eq!(call.args, vec![replacement]),
        node => panic!("expected outer call node, got {node:?}"),
    }
}

#[test]
fn test_ir_graph_replace_subtree_rejects_invalid_roots() {
    let mut b = GraphBuilder::new();
    let old_detached = b.literal(lit_int(1));
    let replacement = b.literal(lit_int(2));
    assert!(b.graph.replace_subtree(old_detached, replacement).is_err());

    b.graph.add_top_level_form(old_detached).unwrap();
    b.graph.add_top_level_form(replacement).unwrap();
    assert!(b.graph.replace_subtree(old_detached, replacement).is_err());
}

#[test]
fn test_unit_rejects_empty_id() {
    assert!(Unit::empty("").is_err());
}

#[test]
fn test_unit_evaluates_top_level_sequence() {
    let mut b = GraphBuilder::new();
    let one = b.literal(lit_int(1));
    let two = b.literal(lit_int(2));
    b.graph.add_top_level_form(one).unwrap();
    b.graph.add_top_level_form(two).unwrap();

    let unit = Unit::from_graph("test.sequence", std::mem::take(&mut b.graph))
        .expect("unit construction failed");

    assert_eq!(
        unit.evaluate().expect("unit evaluation failed"),
        RuntimeValue::Int(2)
    );
}

#[test]
fn test_unit_evaluate_reports_uninitialized_top_level_symbol() {
    let mut b = GraphBuilder::new();
    let x = b.name("x");
    b.graph.add_top_level_form(x).unwrap();
    let mut unit = Unit::from_graph("test.uninitialized", std::mem::take(&mut b.graph))
        .expect("unit construction failed");
    unit.semantics_mut().define_symbol(
        caap_core_port::SymbolEntry::new(
            "x",
            caap_core_port::SymbolKind::TopLevel,
            caap_core_port::PhasePolicy::Runtime,
            Some(x),
        )
        .unwrap(),
    );

    let err = unit
        .evaluate()
        .expect_err("expected uninitialized top-level error");
    match err {
        caap_core_port::EvalSignal::Error(error) => {
            assert_eq!(
                error.message(),
                "name \"x\" was accessed before initialization"
            );
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn test_unit_snapshot_restore_reverts_graph_state() {
    let mut b = GraphBuilder::new();
    let one = b.literal(lit_int(1));
    b.graph.add_top_level_form(one).unwrap();
    let mut unit = Unit::from_graph("test.snapshot", std::mem::take(&mut b.graph))
        .expect("unit construction failed");
    let snapshot = unit.snapshot();

    let two = unit.ir_mut().allocate_id();
    unit.ir_mut().set_node(
        caap_core_port::Node::Literal(caap_core_port::LiteralNode {
            id: two,
            value: lit_int(2),
        }),
        None,
    );
    unit.ir_mut().add_top_level_form(two).unwrap();
    assert_eq!(unit.top_level_form_ids().len(), 2);

    unit.restore_snapshot(snapshot);
    assert_eq!(unit.unit_id(), "test.snapshot");
    assert_eq!(unit.top_level_form_ids(), &[one]);
    assert_eq!(unit.ir().node_count(), 1);
}

#[test]
fn test_unit_snapshot_restore_reverts_semantic_and_metadata_state() {
    let mut unit = Unit::empty("test.semantic-snapshot").expect("unit construction failed");
    unit.set_attribute(
        "mode",
        caap_core_port::SemanticValue::Str("initial".to_string()),
    )
    .unwrap();
    unit.add_link_binding(caap_core_port::LinkBinding::new("stdlib.core", "id", "id").unwrap());
    unit.semantics_mut().define_symbol(
        caap_core_port::SymbolEntry::new(
            "id",
            caap_core_port::SymbolKind::Builtin,
            caap_core_port::PhasePolicy::Runtime,
            None,
        )
        .unwrap(),
    );
    let snapshot = unit.snapshot();

    unit.set_attribute(
        "mode",
        caap_core_port::SemanticValue::Str("changed".to_string()),
    )
    .unwrap();
    unit.add_link_binding(caap_core_port::LinkBinding::new("stdlib.math", "add", "add").unwrap());
    unit.restore_snapshot(snapshot);

    assert_eq!(
        unit.attributes().get("mode"),
        Some(&caap_core_port::SemanticValue::Str("initial".to_string()))
    );
    assert_eq!(unit.link_bindings().len(), 1);
    assert!(unit.semantics().lookup_symbol("id").unwrap().is_some());
}

#[test]
fn test_unit_syntax_state_and_lifecycle_events_roundtrip() {
    let mut unit = Unit::empty("test.syntax-state").expect("unit construction failed");
    unit.set_syntax_state(
        caap_core_port::UnitSyntaxState::new("caap")
            .unwrap()
            .with_source("test.caap", "sha256:test")
            .unwrap(),
    );
    unit.set_attribute(
        "kind",
        caap_core_port::SemanticValue::Str("surface".to_string()),
    )
    .unwrap();
    unit.add_link_binding(
        caap_core_port::LinkBinding::with_syntax("stdlib.syntax", "quote", "quote", true).unwrap(),
    );

    assert_eq!(unit.syntax_state().language, "caap");
    assert_eq!(
        unit.syntax_state().source_path.as_deref(),
        Some("test.caap")
    );
    assert_eq!(
        unit.lifecycle_events()
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["syntax-state", "attribute", "link-binding"]
    );

    let snapshot = unit.snapshot();
    unit.set_syntax_state(caap_core_port::UnitSyntaxState::new("ir").unwrap());
    unit.restore_snapshot(snapshot);
    assert_eq!(unit.syntax_state().language, "caap");

    let restored = Unit::from_template(unit.to_template()).unwrap();
    assert_eq!(restored.syntax_state().language, "caap");
    assert_eq!(restored.lifecycle_events().len(), 3);
    assert!(restored.link_bindings()[0].syntax);
}

#[test]
fn test_unit_assembly_pipeline_runs_hooks_and_records_lifecycle() {
    let mut unit = Unit::empty("test.assembly").expect("unit construction failed");
    let mut pipeline = caap_core_port::UnitAssemblyPipeline::new();
    pipeline
        .register_hook("syntax", |unit| {
            unit.set_syntax_state(caap_core_port::UnitSyntaxState::new("caap")?);
            Ok(())
        })
        .unwrap();
    pipeline
        .register_hook("metadata", |unit| {
            unit.set_attribute("assembled", caap_core_port::SemanticValue::Bool(true))
        })
        .unwrap();

    assert_eq!(pipeline.hook_names(), vec!["syntax", "metadata"]);
    pipeline.apply(&mut unit).unwrap();

    assert_eq!(unit.syntax_state().language, "caap");
    assert_eq!(
        unit.attributes().get("assembled"),
        Some(&caap_core_port::SemanticValue::Bool(true))
    );
    assert_eq!(
        unit.lifecycle_events()
            .iter()
            .filter(|event| event.kind == "assembly-hook")
            .map(|event| event.detail.as_str())
            .collect::<Vec<_>>(),
        vec![
            "start:syntax",
            "finish:syntax",
            "start:metadata",
            "finish:metadata"
        ]
    );
}

#[test]
fn test_unit_assembly_pipeline_stops_on_hook_error() {
    let mut unit = Unit::empty("test.assembly-error").expect("unit construction failed");
    let mut pipeline = caap_core_port::UnitAssemblyPipeline::new();
    pipeline
        .register_hook("bad", |_unit| Err("assembly failed".to_string()))
        .unwrap();

    let err = pipeline
        .apply(&mut unit)
        .expect_err("failing hook should stop assembly");

    assert_eq!(err, "assembly failed");
    assert_eq!(
        unit.lifecycle_events()
            .last()
            .map(|event| event.kind.as_str()),
        Some("assembly-hook-error")
    );
}

#[test]
fn test_unit_transaction_commit_and_rollback_are_explicit() {
    let mut unit = Unit::empty("test.transaction").expect("unit construction failed");
    unit.set_attribute(
        "state",
        caap_core_port::SemanticValue::Str("base".to_string()),
    )
    .unwrap();

    let rollback_tx = unit.begin_transaction();
    unit.set_attribute(
        "state",
        caap_core_port::SemanticValue::Str("changed".to_string()),
    )
    .unwrap();
    unit.rollback_transaction(rollback_tx);
    assert_eq!(
        unit.attributes().get("state"),
        Some(&caap_core_port::SemanticValue::Str("base".to_string()))
    );

    let commit_tx = unit.begin_transaction();
    unit.set_attribute(
        "state",
        caap_core_port::SemanticValue::Str("committed".to_string()),
    )
    .unwrap();
    let version = unit.commit_transaction(commit_tx);
    assert_eq!(unit.version(), version);
    assert_eq!(
        unit.lifecycle_events()
            .last()
            .map(|event| event.kind.as_str()),
        Some("transaction")
    );
    assert_eq!(
        unit.attributes().get("state"),
        Some(&caap_core_port::SemanticValue::Str("committed".to_string()))
    );
}

#[test]
fn test_unit_template_roundtrips_unit_state() {
    let mut b = GraphBuilder::new();
    let one = b.literal(lit_int(1));
    b.graph.root_id = one;
    b.graph.add_top_level_form(one).unwrap();
    let mut unit = Unit::from_graph("test.template", std::mem::take(&mut b.graph))
        .expect("unit construction failed");
    unit.add_link_binding(caap_core_port::LinkBinding::new("stdlib.core", "id", "id").unwrap());
    unit.set_attribute(
        "kind",
        caap_core_port::SemanticValue::Str("test".to_string()),
    )
    .unwrap();

    let template = unit.to_template();
    template
        .validate()
        .expect("unit template validation failed");
    let restored = Unit::from_template(template).expect("unit restore failed");

    assert_eq!(restored.unit_id(), "test.template");
    assert_eq!(restored.root_id(), one);
    assert_eq!(restored.top_level_form_ids(), &[one]);
    assert_eq!(restored.link_bindings().len(), 1);
    assert!(restored.attributes().contains_key("kind"));
    assert_eq!(restored.syntax_state().language, "ir");
    assert_eq!(restored.lifecycle_events().len(), 2);
    assert_eq!(restored.stable_id().as_str(), "unit:test.template");
}

#[test]
fn test_unit_template_rejects_empty_unit_id() {
    let valid = Unit::empty("test.template-validation")
        .unwrap()
        .to_template();
    let template = caap_core_port::UnitTemplate {
        unit_id: String::new(),
        ..valid
    };
    assert!(template.validate().is_err());
}

#[test]
fn test_source_span_rejects_invalid_range() {
    assert!(SourceSpan::new(5, 4, 1, 6, 1, 5).is_err());
}

#[test]
fn test_frontend_attaches_source_spans_to_lowered_nodes() {
    let source = "(int-add 1\n 2)";
    let graph = parse(source).expect("parse failed");
    let top_id = graph.top_level_form_ids()[0];
    let top_span = graph
        .source_span(top_id)
        .expect("missing top-level source span");

    assert_eq!(top_span.start, 0);
    assert_eq!(top_span.end, source.len());
    assert_eq!(top_span.start_line, 1);
    assert_eq!(top_span.start_col, 1);
    assert_eq!(top_span.end_line, 2);

    let call = match graph.node(top_id).expect("missing top-level node") {
        caap_core_port::Node::Call(call) => call,
        other => panic!("expected call, got {other:?}"),
    };
    let last_arg = call.args[1];
    let literal_span = graph
        .source_span(last_arg)
        .expect("missing literal source span");
    assert_eq!(literal_span.start_line, 2);
    assert_eq!(literal_span.end_line, 2);
}

#[test]
fn test_frontend_attaches_source_path_to_file_spans() {
    let source = "(int-add 1 2)";
    let path = "/tmp/source-path-demo.caap";
    let graph = caap_core_port::parse_with_source_path(source, path).expect("parse failed");
    let top_id = graph.top_level_form_ids()[0];
    let top_span = graph
        .source_span(top_id)
        .expect("missing top-level source span");

    assert_eq!(top_span.path.as_deref(), Some(path));
}

#[test]
fn test_semantic_registry_assigns_stable_ids_and_forks() {
    let mut registry = caap_core_port::SemanticRegistry::new();
    registry
        .define(
            caap_core_port::SemanticEntry::new("add", caap_core_port::EntrySource::Builtin)
                .unwrap(),
        )
        .unwrap();

    let entry = registry.lookup("add").unwrap().unwrap();
    assert_eq!(
        entry.stable_id.as_ref().unwrap().as_str(),
        "semantic:builtin:add"
    );

    let mut child = registry.fork();
    assert!(child.lookup("add").unwrap().is_some());
    child
        .define(
            caap_core_port::SemanticEntry::new("local", caap_core_port::EntrySource::Local)
                .unwrap(),
        )
        .unwrap();
    assert!(registry.lookup("local").unwrap().is_none());
    assert!(child.lookup("local").unwrap().is_some());
}

#[test]
fn test_unified_semantic_graph_tracks_symbols_facts_and_snapshots() {
    let mut graph = caap_core_port::UnifiedSemanticGraph::new();
    let symbol = caap_core_port::SymbolEntry::new(
        "x",
        caap_core_port::SymbolKind::TopLevel,
        caap_core_port::PhasePolicy::Runtime,
        Some(1),
    )
    .unwrap();
    assert!(graph.define_symbol(symbol));
    assert_eq!(graph.lookup_symbol("x").unwrap().unwrap().node_id, Some(1));

    let subject = caap_core_port::node_subject_id(1);
    assert!(graph
        .set_fact(
            subject.clone(),
            "type",
            caap_core_port::SemanticValue::Str("int".to_string()),
        )
        .unwrap());
    assert_eq!(
        graph.get_fact(&subject, "type").unwrap(),
        Some(&caap_core_port::SemanticValue::Str("int".to_string()))
    );
    assert!(graph.cell_generation(&subject, "type").is_some());
    assert_eq!(
        graph
            .query_facts(Some(&subject), Some("type"))
            .unwrap()
            .len(),
        1
    );

    let snapshot = graph.snapshot();
    graph.remove_symbol("x").unwrap();
    assert!(graph.lookup_symbol("x").unwrap().is_none());
    graph.restore_snapshot(snapshot).unwrap();
    assert!(graph.lookup_symbol("x").unwrap().is_some());
}

#[test]
fn test_unified_semantic_graph_transaction_commit_and_rollback() {
    let mut graph = caap_core_port::UnifiedSemanticGraph::new();
    let subject = caap_core_port::node_subject_id(7);
    graph
        .set_fact(
            subject.clone(),
            "type",
            caap_core_port::SemanticValue::Str("int".to_string()),
        )
        .unwrap();
    let base_version = graph.version();

    let rollback_tx = graph.begin_transaction();
    graph.define_symbol(
        caap_core_port::SymbolEntry::new(
            "temp",
            caap_core_port::SymbolKind::Local,
            caap_core_port::PhasePolicy::Runtime,
            Some(7),
        )
        .unwrap(),
    );
    graph
        .set_fact(
            subject.clone(),
            "type",
            caap_core_port::SemanticValue::Str("string".to_string()),
        )
        .unwrap();
    graph.rollback_transaction(rollback_tx).unwrap();

    assert!(graph.lookup_symbol("temp").unwrap().is_none());
    assert_eq!(
        graph.get_fact(&subject, "type").unwrap(),
        Some(&caap_core_port::SemanticValue::Str("int".to_string()))
    );
    assert!(graph.version() > base_version);

    let commit_tx = graph.begin_transaction();
    graph.define_symbol(
        caap_core_port::SymbolEntry::new(
            "kept",
            caap_core_port::SymbolKind::Local,
            caap_core_port::PhasePolicy::Runtime,
            Some(8),
        )
        .unwrap(),
    );
    let committed_version = graph.commit_transaction(commit_tx);
    assert_eq!(graph.version(), committed_version);
    assert!(graph.lookup_symbol("kept").unwrap().is_some());
}

#[test]
fn test_unified_semantic_graph_without_facts_rejects_fact_access() {
    let mut graph = caap_core_port::UnifiedSemanticGraph::without_facts();
    assert!(graph
        .set_fact(
            caap_core_port::node_subject_id(1),
            "type",
            caap_core_port::SemanticValue::Str("int".to_string()),
        )
        .is_err());
}

#[test]
fn test_builtin_metadata_classifies_special_forms_and_effects() {
    let ev = Evaluator::new(caap_core_port::IRGraph::new());

    let if_meta = ev.builtin_metadata("if").expect("missing if builtin");
    assert_eq!(if_meta.eval_policy, caap_core_port::EvalPolicy::LazyIf);
    assert_eq!(
        if_meta.control_policy,
        caap_core_port::ControlPolicy::ConditionalBranch
    );
    assert!(!if_meta.eager_args);

    let bind_meta = ev.builtin_metadata("bind").expect("missing bind builtin");
    assert_eq!(
        bind_meta.scope_policy,
        caap_core_port::ScopePolicy::LexicalBinding
    );

    let append_meta = ev
        .builtin_metadata("append")
        .expect("missing append builtin");
    assert!(append_meta.effect_policy.allows("mutation"));

    let add_meta = ev
        .builtin_metadata("int-add")
        .expect("missing int-add builtin");
    assert_eq!(add_meta.eval_policy, caap_core_port::EvalPolicy::Eager);
    assert!(add_meta.effect_policy.is_pure());

    let instantiate_meta = ev
        .builtin_metadata("ctfe-ir-instantiate")
        .expect("missing ctfe-ir-instantiate builtin");
    assert_eq!(
        instantiate_meta.phase_policy,
        caap_core_port::PhasePolicy::CompileTime
    );
    assert!(instantiate_meta.effect_policy.is_pure());

    let node_meta = ev
        .builtin_metadata("ctfe-node-call-semantics")
        .expect("missing ctfe-node-call-semantics builtin");
    assert_eq!(
        node_meta.phase_policy,
        caap_core_port::PhasePolicy::CompileTime
    );
    assert!(node_meta.effect_policy.is_pure());

    let annotation_set_meta = ev
        .builtin_metadata("ctfe-meta-annotation-set-many")
        .expect("missing ctfe-meta-annotation-set-many builtin");
    assert_eq!(
        annotation_set_meta.phase_policy,
        caap_core_port::PhasePolicy::CompileTime
    );
    assert!(annotation_set_meta.effect_policy.allows("impure"));

    let unit_template_meta = ev
        .builtin_metadata("ctfe-unit-to-template")
        .expect("missing ctfe-unit-to-template builtin");
    assert_eq!(
        unit_template_meta.phase_policy,
        caap_core_port::PhasePolicy::CompileTime
    );
    assert!(unit_template_meta.effect_policy.allows("impure"));
}

// ── literal evaluation ────────────────────────────────────────────────────────

#[test]
fn test_literal_int() {
    let mut b = GraphBuilder::new();
    let id = b.literal(lit_int(42));
    assert_eq!(eval_one(&mut b, id), RuntimeValue::Int(42));
}

#[test]
fn test_literal_bool_true() {
    let mut b = GraphBuilder::new();
    let id = b.literal(lit_bool(true));
    assert_eq!(eval_one(&mut b, id), RuntimeValue::Bool(true));
}

#[test]
fn test_literal_null() {
    let mut b = GraphBuilder::new();
    let id = b.literal(lit_null());
    assert_eq!(eval_one(&mut b, id), RuntimeValue::Null);
}

#[test]
fn test_literal_tuple_runtime_value() {
    let mut b = GraphBuilder::new();
    let id = b.literal(IrLiteralData::Tuple(vec![
        IrLiteralData::Int(1),
        IrLiteralData::Str("two".to_string()),
        IrLiteralData::Bool(true),
    ]));
    match eval_one(&mut b, id) {
        RuntimeValue::Tuple(items) => {
            assert_eq!(items.len(), 3);
            assert_eq!(items[0], RuntimeValue::Int(1));
            assert_eq!(items[1], RuntimeValue::Str("two".into()));
            assert_eq!(items[2], RuntimeValue::Bool(true));
        }
        other => panic!("expected tuple, got {other}"),
    }
}

#[test]
fn test_literal_dict_runtime_value() {
    let mut b = GraphBuilder::new();
    let id = b.literal(IrLiteralData::Dict(vec![
        ("a".to_string(), IrLiteralData::Int(1)),
        (
            "nested".to_string(),
            IrLiteralData::Tuple(vec![IrLiteralData::Str("x".to_string())]),
        ),
    ]));
    match eval_one(&mut b, id) {
        RuntimeValue::Map(m) => {
            use caap_core_port::MapKey;
            let map = m.borrow();
            assert_eq!(
                map.get(&MapKey::Str("a".into())),
                Some(&RuntimeValue::Int(1))
            );
            match map.get(&MapKey::Str("nested".into())) {
                Some(RuntimeValue::Tuple(items)) => {
                    assert_eq!(items.as_ref(), [RuntimeValue::Str("x".into())]);
                }
                other => panic!("expected nested tuple, got {other:?}"),
            }
        }
        other => panic!("expected map, got {other}"),
    }
}

// ── arithmetic ────────────────────────────────────────────────────────────────

fn eval_arith(op: &str, left: i64, right: i64) -> RuntimeValue {
    let mut b = GraphBuilder::new();
    let fn_node = b.name(op);
    let l = b.literal(lit_int(left));
    let r = b.literal(lit_int(right));
    let call_id = b.call(fn_node, vec![l, r]);
    eval_one(&mut b, call_id)
}

#[test]
fn test_int_add() {
    assert_eq!(eval_arith("int-add", 3, 4), RuntimeValue::Int(7));
}

#[test]
fn test_int_sub() {
    assert_eq!(eval_arith("int-sub", 10, 3), RuntimeValue::Int(7));
}

#[test]
fn test_int_mul() {
    assert_eq!(eval_arith("int-mul", 6, 7), RuntimeValue::Int(42));
}

#[test]
fn test_int_div() {
    assert_eq!(eval_arith("int-div", 17, 5), RuntimeValue::Int(3));
}

#[test]
fn test_int_div_by_zero() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("int-div");
    let l = b.literal(lit_int(10));
    let r = b.literal(lit_int(0));
    let call_id = b.call(fn_node, vec![l, r]);
    let env = Environment::new(None);
    let graph = std::mem::take(&mut b.graph);
    let mut ev = Evaluator::new(graph);
    assert!(ev.eval(call_id, &env).is_err());
}

#[test]
fn test_int_rem() {
    assert_eq!(eval_arith("int-rem", 17, 5), RuntimeValue::Int(2));
}

#[test]
fn test_int_mod() {
    assert_eq!(eval_arith("int-mod", -1, 5), RuntimeValue::Int(4));
}

#[test]
fn test_int_abs_positive() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("int-abs");
    let v = b.literal(lit_int(-7));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(7));
}

#[test]
fn test_int_min() {
    assert_eq!(eval_arith("int-min", 3, 7), RuntimeValue::Int(3));
}

#[test]
fn test_int_max() {
    assert_eq!(eval_arith("int-max", 3, 7), RuntimeValue::Int(7));
}

#[test]
fn test_int_clamp() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("int-clamp");
    let v = b.literal(lit_int(15));
    let lo = b.literal(lit_int(0));
    let hi = b.literal(lit_int(10));
    let call_id = b.call(fn_node, vec![v, lo, hi]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(10));
}

#[test]
fn test_int_and() {
    assert_eq!(
        eval_arith("int-and", 0b1010, 0b1100),
        RuntimeValue::Int(0b1000)
    );
}

#[test]
fn test_int_xor() {
    assert_eq!(
        eval_arith("int-xor", 0b1010, 0b1100),
        RuntimeValue::Int(0b0110)
    );
}

#[test]
fn test_int_shr() {
    assert_eq!(eval_arith("int-shr", 16, 2), RuntimeValue::Int(4));
}

#[test]
fn test_int_to_float() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("int-to-float");
    let v = b.literal(lit_int(3));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Float(3.0));
}

#[test]
fn test_float_to_int() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("float-to-int");
    let v = b.literal(IrLiteralData::Float(3.7));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(3));
}

// ── comparison ────────────────────────────────────────────────────────────────

#[test]
fn test_eq_true() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("eq");
    let a = b.literal(lit_int(5));
    let c = b.literal(lit_int(5));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_eq_false() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("eq");
    let a = b.literal(lit_int(5));
    let c = b.literal(lit_int(6));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(false));
}

#[test]
fn test_lt_true() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("lt");
    let a = b.literal(lit_int(3));
    let c = b.literal(lit_int(7));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_gt_false() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("gt");
    let a = b.literal(lit_int(3));
    let c = b.literal(lit_int(7));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(false));
}

#[test]
fn test_not_false() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("not");
    let v = b.literal(lit_bool(true));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(false));
}

// ── control flow ─────────────────────────────────────────────────────────────

#[test]
fn test_if_then() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("if");
    let cond = b.literal(lit_bool(true));
    let then_br = b.literal(lit_int(1));
    let else_br = b.literal(lit_int(2));
    let call_id = b.call(fn_node, vec![cond, then_br, else_br]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(1));
}

#[test]
fn test_if_else() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("if");
    let cond = b.literal(lit_bool(false));
    let then_br = b.literal(lit_int(1));
    let else_br = b.literal(lit_int(2));
    let call_id = b.call(fn_node, vec![cond, then_br, else_br]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(2));
}

#[test]
fn test_if_no_else_returns_null() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("if");
    let cond = b.literal(lit_bool(false));
    let then_br = b.literal(lit_int(99));
    let call_id = b.call(fn_node, vec![cond, then_br]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Null);
}

#[test]
fn test_do_returns_last() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("do");
    let a = b.literal(lit_int(1));
    let c = b.literal(lit_int(2));
    let d = b.literal(lit_int(3));
    let call_id = b.call(fn_node, vec![a, c, d]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(3));
}

#[test]
fn test_or_short_circuits() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("or");
    let f = b.literal(lit_bool(false));
    let t = b.literal(lit_int(42));
    let call_id = b.call(fn_node, vec![f, t]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(42));
}

#[test]
fn test_and_short_circuits_falsey() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("and");
    let t = b.literal(lit_bool(true));
    let f = b.literal(lit_bool(false));
    let call_id = b.call(fn_node, vec![t, f]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(false));
}

// ── lambda + call ─────────────────────────────────────────────────────────────

/// Build (lambda (x) (int-add x 1)) then call it with 5 → 6.
///
/// Params list node convention: CallNode { callee: dummy, args: [NameNode param, ...] }
#[test]
fn test_lambda_and_call() {
    let mut b = GraphBuilder::new();

    // params list: (__ x) — callee is ignored, args are the param names
    let params_callee = b.name("__params__");
    let param_x = b.name("x");
    let params_call = b.call(params_callee, vec![param_x]);

    // body: (int-add x 1)
    let add_fn = b.name("int-add");
    let body_x = b.name("x");
    let one = b.literal(lit_int(1));
    let body = b.call(add_fn, vec![body_x, one]);

    // lambda: (lambda params_call body)
    let lambda_fn = b.name("lambda");
    let lambda_call = b.call(lambda_fn, vec![params_call, body]);

    // outer call: apply the lambda to 5
    let five = b.literal(lit_int(5));
    let apply_call = b.call(lambda_call, vec![five]);

    assert_eq!(eval_one(&mut b, apply_call), RuntimeValue::Int(6));
}

#[test]
fn test_lambda_rest_param_collects_extra_args() {
    let graph = parse(
        "(list-of
          ((lambda (first &rest) &rest) 1 2 3)
          ((lambda (&args) &args) 4 5)
          ((lambda (first &empty) &empty) 6))",
    )
    .unwrap();
    let mut ev = Evaluator::new(graph);

    let RuntimeValue::List(items) = ev.run().unwrap() else {
        panic!("expected list result");
    };
    let items = items.borrow();
    let RuntimeValue::List(first_rest) = &items[0] else {
        panic!("expected first rest list");
    };
    assert_eq!(
        first_rest.borrow().as_slice(),
        &[RuntimeValue::Int(2), RuntimeValue::Int(3)]
    );
    let RuntimeValue::List(all_args) = &items[1] else {
        panic!("expected all args list");
    };
    assert_eq!(
        all_args.borrow().as_slice(),
        &[RuntimeValue::Int(4), RuntimeValue::Int(5)]
    );
    let RuntimeValue::List(empty_rest) = &items[2] else {
        panic!("expected empty rest list");
    };
    assert!(empty_rest.borrow().is_empty());
}

#[test]
fn test_lambda_rest_param_requires_minimum_arity() {
    let graph = parse("((lambda (first second &rest) first) 1)").unwrap();
    let mut ev = Evaluator::new(graph);

    let err = ev.run().expect_err("expected arity error");
    assert!(format!("{err:?}").contains("lambda expected at least 2 args"));
}

// ── bind ──────────────────────────────────────────────────────────────────────

/// (bind ((x 10) (y 20)) (int-add x y)) → 30
///
/// Binding pair convention: CallNode { callee: dummy, args: [NameNode name, value_expr] }
/// Bindings list convention: CallNode { callee: dummy, args: [pair1, pair2, ...] }
#[test]
fn test_bind() {
    let mut b = GraphBuilder::new();

    // binding pair (x 10): args=[NameNode("x"), LiteralNode(10)]
    let pair_callee_x = b.name("__pair__");
    let name_x = b.name("x");
    let val_10 = b.literal(lit_int(10));
    let pair_x = b.call(pair_callee_x, vec![name_x, val_10]);

    let pair_callee_y = b.name("__pair__");
    let name_y = b.name("y");
    let val_20 = b.literal(lit_int(20));
    let pair_y = b.call(pair_callee_y, vec![name_y, val_20]);

    // bindings list
    let dummy_callee = b.name("__bindings__");
    let bindings = b.call(dummy_callee, vec![pair_x, pair_y]);

    // body: (int-add x y)
    let add_fn = b.name("int-add");
    let ref_x = b.name("x");
    let ref_y = b.name("y");
    let body = b.call(add_fn, vec![ref_x, ref_y]);

    let bind_fn = b.name("bind");
    let bind_call = b.call(bind_fn, vec![bindings, body]);

    assert_eq!(eval_one(&mut b, bind_call), RuntimeValue::Int(30));
}

#[test]
fn test_surface_multi_bind_evaluates_all_pairs() {
    let graph = parse("(bind ((x 10) (y 20)) (int-add x y))").unwrap();
    let mut ev = Evaluator::new(graph);

    assert_eq!(ev.run().unwrap(), RuntimeValue::Int(30));
}

#[test]
fn test_builtin_name_can_be_captured_as_value() {
    let graph = parse(
        "(bind get-ref get
          (invoke get-ref (map-of \"answer\" 42) \"answer\"))",
    )
    .unwrap();
    let mut ev = Evaluator::new(graph);

    assert_eq!(ev.run().unwrap(), RuntimeValue::Int(42));
}

// ── block + leave ─────────────────────────────────────────────────────────────

/// (block (leave <block-id> 99) (int-add 1 1)) → 99
/// The leave target is the block call's NodeId.
#[test]
fn test_block_leave() {
    let mut b = GraphBuilder::new();

    // We need to know the block's NodeId before creating the leave node.
    // Pre-allocate the block id.
    let block_id = b.graph.allocate_id();

    // leave: (leave block_id 99)
    let leave_fn = b.name("leave");
    let target_lit = b.literal(IrLiteralData::Int(block_id as i64));
    let value_99 = b.literal(lit_int(99));
    let leave_call = b.call(leave_fn, vec![target_lit, value_99]);

    // unreachable body after leave
    let unreachable = b.literal(lit_int(0));

    // Manually insert the block node with the pre-allocated id.
    let block_fn = b.name("block");
    let block_fn_call_node = caap_core_port::ir::CallNode {
        id: block_id,
        callee: block_fn,
        args: vec![leave_call, unreachable],
    };
    b.graph
        .set_node(caap_core_port::ir::Node::Call(block_fn_call_node), None);

    assert_eq!(eval_one(&mut b, block_id), RuntimeValue::Int(99));
}

// ── while ─────────────────────────────────────────────────────────────────────

/// Use bind + while to compute a sum 0..5 = 10.
/// (bind ((sum 0) (i 0))
///   (while (lt i 5)
///     (do (set! sum (int-add sum i))   ← we don't have set! yet, skip mutation for now
/// Instead test while never-executes when condition is false.
#[test]
fn test_while_false_returns_null() {
    let mut b = GraphBuilder::new();
    let while_fn = b.name("while");
    let cond = b.literal(lit_bool(false));
    let body = b.literal(lit_int(99));
    let call_id = b.call(while_fn, vec![cond, body]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Null);
}

// ── environment lookup ────────────────────────────────────────────────────────

#[test]
fn test_env_lookup_bound_name() {
    let mut b = GraphBuilder::new();
    let name_id = b.name("answer");
    let graph = std::mem::take(&mut b.graph);
    let env = Environment::new(None);
    Environment::define(&env, "answer", RuntimeValue::Int(42));
    let mut ev = Evaluator::new(graph);
    assert_eq!(ev.eval(name_id, &env).unwrap(), RuntimeValue::Int(42));
}

#[test]
fn test_env_lookup_unknown_name_errors() {
    let mut b = GraphBuilder::new();
    let name_id = b.name("unknown");
    let graph = std::mem::take(&mut b.graph);
    let env = Environment::new(None);
    let mut ev = Evaluator::new(graph);
    assert!(ev.eval(name_id, &env).is_err());
}

// ── mutable collections ───────────────────────────────────────────────────────

#[test]
fn test_list_of_empty() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("list-of");
    let call_id = b.call(fn_node, vec![]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => assert!(l.borrow().is_empty()),
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_list_of_values() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("list-of");
    let a = b.literal(lit_int(1));
    let c = b.literal(lit_int(2));
    let d = b.literal(lit_int(3));
    let call_id = b.call(fn_node, vec![a, c, d]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow.len(), 3);
            assert_eq!(borrow[0], RuntimeValue::Int(1));
            assert_eq!(borrow[2], RuntimeValue::Int(3));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_append() {
    let mut b = GraphBuilder::new();
    // (append (list-of 1 2) 3) → [1, 2, 3]
    let list_fn = b.name("list-of");
    let a = b.literal(lit_int(1));
    let c = b.literal(lit_int(2));
    let list_call = b.call(list_fn, vec![a, c]);
    let append_fn = b.name("append");
    let three = b.literal(lit_int(3));
    let call_id = b.call(append_fn, vec![list_call, three]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow.len(), 3);
            assert_eq!(borrow[2], RuntimeValue::Int(3));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_map_of() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("map-of");
    let k = b.literal(IrLiteralData::Str("x".to_string()));
    let v = b.literal(lit_int(99));
    let call_id = b.call(fn_node, vec![k, v]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::Map(m) => {
            let borrow = m.borrow();
            assert_eq!(borrow.len(), 1);
            use caap_core_port::MapKey;
            let key = MapKey::Str("x".into());
            assert_eq!(borrow[&key], RuntimeValue::Int(99));
        }
        other => panic!("expected map, got {other}"),
    }
}

#[test]
fn test_assoc() {
    let mut b = GraphBuilder::new();
    // (assoc (map-of) "key" 42)
    let map_fn = b.name("map-of");
    let empty_map = b.call(map_fn, vec![]);
    let assoc_fn = b.name("assoc");
    let k = b.literal(IrLiteralData::Str("key".to_string()));
    let v = b.literal(lit_int(42));
    let call_id = b.call(assoc_fn, vec![empty_map, k, v]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::Map(m) => {
            use caap_core_port::MapKey;
            assert_eq!(
                m.borrow()[&MapKey::Str("key".into())],
                RuntimeValue::Int(42)
            );
        }
        other => panic!("expected map, got {other}"),
    }
}

#[test]
fn test_set_list() {
    let mut b = GraphBuilder::new();
    // (set (list-of 0 0 0) 1 99) → [0, 99, 0]
    let list_fn = b.name("list-of");
    let z1 = b.literal(lit_int(0));
    let z2 = b.literal(lit_int(0));
    let z3 = b.literal(lit_int(0));
    let list = b.call(list_fn, vec![z1, z2, z3]);
    let set_fn = b.name("set");
    let idx = b.literal(lit_int(1));
    let val = b.literal(lit_int(99));
    let call_id = b.call(set_fn, vec![list, idx, val]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            assert_eq!(l.borrow()[1], RuntimeValue::Int(99));
        }
        other => panic!("expected list, got {other}"),
    }
}

// ── string builtins ───────────────────────────────────────────────────────────

fn lit_str(s: &str) -> IrLiteralData {
    IrLiteralData::Str(s.to_string())
}

#[test]
fn test_string_concat_many() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string-concat-many");
    let a = b.literal(lit_str("foo"));
    let c = b.literal(lit_str("bar"));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(
        eval_one(&mut b, call_id),
        RuntimeValue::Str("foobar".into())
    );
}

#[test]
fn test_string_split() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string-split");
    let s = b.literal(lit_str("a,b,c"));
    let sep = b.literal(lit_str(","));
    let call_id = b.call(fn_node, vec![s, sep]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow.len(), 3);
            assert_eq!(borrow[0], RuntimeValue::Str("a".into()));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_string_slice_accepts_null_end_and_negative_indexes() {
    let graph = parse(
        "(list-of
          (string-slice \"abcdef\" 2 null)
          (string-slice \"abcdef\" -3 null)
          (string-slice \"abcdef\" 4 2))",
    )
    .unwrap();
    let mut ev = Evaluator::new(graph);

    let RuntimeValue::List(items) = ev.run().unwrap() else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("cdef".into()));
    assert_eq!(items[1], RuntimeValue::Str("def".into()));
    assert_eq!(items[2], RuntimeValue::Str("".into()));
}

#[test]
fn test_string_trim() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string-trim");
    let s = b.literal(lit_str("  hello  "));
    let call_id = b.call(fn_node, vec![s]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("hello".into()));
}

#[test]
fn test_string_upcase() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string-upcase");
    let s = b.literal(lit_str("hello"));
    let call_id = b.call(fn_node, vec![s]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("HELLO".into()));
}

#[test]
fn test_string_replace() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string-replace");
    let s = b.literal(lit_str("hello world"));
    let old = b.literal(lit_str("world"));
    let new = b.literal(lit_str("rust"));
    let call_id = b.call(fn_node, vec![s, old, new]);
    assert_eq!(
        eval_one(&mut b, call_id),
        RuntimeValue::Str("hello rust".into())
    );
}

#[test]
fn test_string_contains_true() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string-contains");
    let s = b.literal(lit_str("foobar"));
    let sub = b.literal(lit_str("oba"));
    let call_id = b.call(fn_node, vec![s, sub]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_string_starts_with() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string-starts-with");
    let s = b.literal(lit_str("hello"));
    let prefix = b.literal(lit_str("hel"));
    let call_id = b.call(fn_node, vec![s, prefix]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_string_ends_with() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string-ends-with");
    let s = b.literal(lit_str("hello"));
    let suffix = b.literal(lit_str("llo"));
    let call_id = b.call(fn_node, vec![s, suffix]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_string_to_int() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string-to-int");
    let s = b.literal(lit_str("42"));
    let call_id = b.call(fn_node, vec![s]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(42));
}

#[test]
fn test_int_to_string() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("int-to-string");
    let v = b.literal(lit_int(123));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("123".into()));
}

#[test]
fn test_string_byte_at() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string-byte-at");
    let text = b.literal(lit_str("abc"));
    let index = b.literal(lit_int(1));
    let call_id = b.call(fn_node, vec![text, index]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(98));
}

#[test]
fn test_string_format() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string-format");
    let template = b.literal(lit_str("{}:{1}:{{ok}}"));
    let first = b.literal(lit_str("port"));
    let second = b.literal(lit_int(8080));
    let call_id = b.call(fn_node, vec![template, first, second]);
    assert_eq!(
        eval_one(&mut b, call_id),
        RuntimeValue::Str("port:8080:{ok}".into())
    );
}

#[test]
fn test_stable_hash_is_deterministic_for_structured_values() {
    let source = r#"
        (do
          (bind first (stable-hash (map-of "left" 1 "right" (list-of true null)))
            (bind second (stable-hash (map-of "right" (list-of true null) "left" 1))
              (bind ordered (stable-hash (list-of "left" "right"))
                (bind reversed (stable-hash (list-of "right" "left"))
                  (list-of (eq first second) (eq ordered reversed) (size first)))))))"#;
    let graph = parse(source).expect("parse failed");
    let root_id = graph.top_level_form_ids()[0];
    let env = Environment::new(None);
    let mut ev = Evaluator::new(graph);
    match ev.eval(root_id, &env).expect("evaluation failed") {
        RuntimeValue::List(items) => {
            let items = items.borrow();
            assert_eq!(items[0], RuntimeValue::Bool(true));
            assert_eq!(items[1], RuntimeValue::Bool(false));
            assert_eq!(items[2], RuntimeValue::Int(32));
        }
        other => panic!("expected list, got {other}"),
    }
}

// ── sequence builtins ─────────────────────────────────────────────────────────

#[test]
fn test_sequence_range() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("sequence-range");
    let s = b.literal(lit_int(0));
    let e = b.literal(lit_int(5));
    let call_id = b.call(fn_node, vec![s, e]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow.len(), 5);
            assert_eq!(borrow[0], RuntimeValue::Int(0));
            assert_eq!(borrow[4], RuntimeValue::Int(4));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_sequence_map() {
    // (sequence-map (list-of 1 2 3) (lambda (x) (int-add x 10)))  → [11, 12, 13]
    let mut b = GraphBuilder::new();

    // list
    let list_fn = b.name("list-of");
    let a = b.literal(lit_int(1));
    let c = b.literal(lit_int(2));
    let d = b.literal(lit_int(3));
    let list = b.call(list_fn, vec![a, c, d]);

    // lambda: (lambda (x) (int-add x 10))
    let params_callee = b.name("__params__");
    let param_x = b.name("x");
    let params = b.call(params_callee, vec![param_x]);
    let add_fn = b.name("int-add");
    let body_x = b.name("x");
    let ten = b.literal(lit_int(10));
    let body = b.call(add_fn, vec![body_x, ten]);
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, body]);

    let map_fn = b.name("sequence-map");
    let call_id = b.call(map_fn, vec![list, lam]);

    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow[0], RuntimeValue::Int(11));
            assert_eq!(borrow[2], RuntimeValue::Int(13));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_sequence_map_builtin_callback_uses_eager_dispatch() {
    let graph = parse("(sequence-map (list-of -3 4 -5) int-abs)").unwrap();
    let mut b = GraphBuilder { graph };
    let root_id = b.graph.root_id;

    match eval_one(&mut b, root_id) {
        RuntimeValue::List(items) => {
            let items = items.borrow();
            assert_eq!(
                items.as_slice(),
                &[
                    RuntimeValue::Int(3),
                    RuntimeValue::Int(4),
                    RuntimeValue::Int(5)
                ]
            );
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_sequence_filter() {
    // (sequence-filter (list-of 1 2 3 4 5) (lambda (x) (gt x 2))) → [3, 4, 5]
    let mut b = GraphBuilder::new();

    let list_fn = b.name("list-of");
    let vals: Vec<u32> = (1..=5).map(|i| b.literal(lit_int(i as i64))).collect();
    let list = b.call(list_fn, vals);

    let params_callee = b.name("__params__");
    let param_x = b.name("x");
    let params = b.call(params_callee, vec![param_x]);
    let gt_fn = b.name("gt");
    let body_x = b.name("x");
    let two = b.literal(lit_int(2));
    let body = b.call(gt_fn, vec![body_x, two]);
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, body]);

    let filter_fn = b.name("sequence-filter");
    let call_id = b.call(filter_fn, vec![list, lam]);

    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow.len(), 3);
            assert_eq!(borrow[0], RuntimeValue::Int(3));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_sequence_fold_left_sum() {
    // (sequence-fold-left (list-of 1 2 3 4 5) 0 (lambda (acc x) (int-add acc x))) → 15
    let mut b = GraphBuilder::new();

    let list_fn = b.name("list-of");
    let vals: Vec<u32> = (1..=5).map(|i| b.literal(lit_int(i as i64))).collect();
    let list = b.call(list_fn, vals);

    let params_callee = b.name("__params__");
    let param_acc = b.name("acc");
    let param_x = b.name("x");
    let params = b.call(params_callee, vec![param_acc, param_x]);
    let add_fn = b.name("int-add");
    let ref_acc = b.name("acc");
    let ref_x = b.name("x");
    let body = b.call(add_fn, vec![ref_acc, ref_x]);
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, body]);

    let fold_fn = b.name("sequence-fold-left");
    let zero = b.literal(lit_int(0));
    let call_id = b.call(fold_fn, vec![list, zero, lam]);

    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(15));
}

#[test]
fn test_sequence_reverse() {
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list-of");
    let a = b.literal(lit_int(1));
    let c = b.literal(lit_int(2));
    let d = b.literal(lit_int(3));
    let list = b.call(list_fn, vec![a, c, d]);
    let rev_fn = b.name("sequence-reverse");
    let call_id = b.call(rev_fn, vec![list]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow[0], RuntimeValue::Int(3));
            assert_eq!(borrow[2], RuntimeValue::Int(1));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_sequence_slice_accepts_null_end_and_negative_indexes() {
    let graph = parse(
        "(list-of
          (sequence-slice (list-of 1 2 3 4) 1 null)
          (sequence-slice (list-of 1 2 3 4) -2 null)
          (sequence-slice (list-of 1 2 3 4) 3 1))",
    )
    .unwrap();
    let mut ev = Evaluator::new(graph);

    let RuntimeValue::List(outer) = ev.run().unwrap() else {
        panic!("expected list result");
    };
    let outer = outer.borrow();
    let RuntimeValue::List(first) = &outer[0] else {
        panic!("expected nested list");
    };
    assert_eq!(
        first.borrow().as_slice(),
        &[
            RuntimeValue::Int(2),
            RuntimeValue::Int(3),
            RuntimeValue::Int(4)
        ]
    );
    let RuntimeValue::List(second) = &outer[1] else {
        panic!("expected nested list");
    };
    assert_eq!(
        second.borrow().as_slice(),
        &[RuntimeValue::Int(3), RuntimeValue::Int(4)]
    );
    let RuntimeValue::List(third) = &outer[2] else {
        panic!("expected nested list");
    };
    assert!(third.borrow().is_empty());
}

#[test]
fn test_sequence_join() {
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list-of");
    let a = b.literal(lit_str("a"));
    let c = b.literal(lit_str("b"));
    let d = b.literal(lit_str("c"));
    let list = b.call(list_fn, vec![a, c, d]);
    let join_fn = b.name("sequence-join");
    let sep = b.literal(lit_str(", "));
    let call_id = b.call(join_fn, vec![list, sep]);
    assert_eq!(
        eval_one(&mut b, call_id),
        RuntimeValue::Str("a, b, c".into())
    );
}

#[test]
fn test_size_list() {
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list-of");
    let a = b.literal(lit_int(1));
    let c = b.literal(lit_int(2));
    let list = b.call(list_fn, vec![a, c]);
    let size_fn = b.name("size");
    let call_id = b.call(size_fn, vec![list]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(2));
}

#[test]
fn test_get_list() {
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list-of");
    let a = b.literal(lit_int(10));
    let c = b.literal(lit_int(20));
    let list = b.call(list_fn, vec![a, c]);
    let get_fn = b.name("get");
    let idx = b.literal(lit_int(1));
    let call_id = b.call(get_fn, vec![list, idx]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(20));
}

#[test]
fn test_get_tuple() {
    let mut b = GraphBuilder::new();
    let tuple = b.literal(IrLiteralData::Tuple(vec![
        IrLiteralData::Int(10),
        IrLiteralData::Int(20),
    ]));
    let get_fn = b.name("get");
    let idx = b.literal(lit_int(1));
    let call_id = b.call(get_fn, vec![tuple, idx]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(20));
}

#[test]
fn test_size_tuple() {
    let mut b = GraphBuilder::new();
    let tuple = b.literal(IrLiteralData::Tuple(vec![
        IrLiteralData::Int(1),
        IrLiteralData::Int(2),
        IrLiteralData::Int(3),
    ]));
    let size_fn = b.name("size");
    let call_id = b.call(size_fn, vec![tuple]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(3));
}

#[test]
fn test_contains_list() {
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list-of");
    let a = b.literal(lit_int(1));
    let c = b.literal(lit_int(2));
    let d = b.literal(lit_int(3));
    let list = b.call(list_fn, vec![a, c, d]);
    let contains_fn = b.name("contains");
    let needle = b.literal(lit_int(2));
    let call_id = b.call(contains_fn, vec![list, needle]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_for_range() {
    // (for-range 0 5 (lambda (i) ...)) runs 5 iterations — returns null
    let mut b = GraphBuilder::new();
    let params_callee = b.name("__params__");
    let param_i = b.name("i");
    let params = b.call(params_callee, vec![param_i]);
    let null_body = b.literal(lit_null());
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, null_body]);
    let fr_fn = b.name("for-range");
    let start = b.literal(lit_int(0));
    let end = b.literal(lit_int(5));
    let call_id = b.call(fr_fn, vec![start, end, lam]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Null);
}

// ── reflect / type predicates ─────────────────────────────────────────────────

#[test]
fn test_value_is_int() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("value-is-int");
    let v = b.literal(lit_int(42));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_value_is_int_rejects_bool() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("value-is-int");
    let v = b.literal(lit_bool(true));
    let call_id = b.call(fn_node, vec![v]);
    // In Rust, bool is a distinct variant — not an int (unlike Python where bool subclasses int)
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(false));
}

#[test]
fn test_value_is_null() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("value-is-null");
    let v = b.literal(lit_null());
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_value_is_string() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("value-is-string");
    let v = b.literal(lit_str("hi"));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_value_is_tuple() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("value-is-tuple");
    let tuple = b.literal(IrLiteralData::Tuple(vec![IrLiteralData::Int(1)]));
    let call_id = b.call(fn_node, vec![tuple]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_value_is_error_predicate_for_runtime_values() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("value-is-error?");
    let v = b.literal(lit_null());
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(false));
}

#[test]
fn test_host_value_kind_int() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("host-value-kind");
    let v = b.literal(lit_int(5));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("int".into()));
}

#[test]
fn test_host_value_kind_tuple() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("host-value-kind");
    let tuple = b.literal(IrLiteralData::Tuple(vec![]));
    let call_id = b.call(fn_node, vec![tuple]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("tuple".into()));
}

#[test]
fn test_host_value_kind_null() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("host-value-kind");
    let v = b.literal(lit_null());
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("null".into()));
}

#[test]
fn test_runtime_error() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("runtime-error");
    let msg = b.literal(lit_str("oops"));
    let call_id = b.call(fn_node, vec![msg]);
    let graph = std::mem::take(&mut b.graph);
    let env = Environment::new(None);
    let mut ev = Evaluator::new(graph);
    assert!(ev.eval(call_id, &env).is_err());
}

#[test]
fn test_runtime_error_carries_call_frame() {
    let source = "(runtime-error \"oops\")";
    let graph = parse(source).expect("parse failed");
    let call_id = graph.top_level_form_ids()[0];
    let env = Environment::new(None);
    let mut ev = Evaluator::new(graph);

    let err = ev.eval(call_id, &env).expect_err("expected runtime error");
    match err {
        caap_core_port::EvalSignal::Error(error) => {
            assert_eq!(error.message(), "oops");
            let frames = error.frames();
            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].node_id, call_id);
            assert_eq!(frames[0].name.as_deref(), Some("runtime-error"));
            assert!(frames[0].span.is_some());
            let displayed = error.to_string();
            assert!(displayed.contains("Runtime frames:"));
            assert!(displayed.contains("runtime-error"));
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn test_eval_signal_exposes_inner_error_source() {
    let signal = caap_core_port::EvalSignal::Error(caap_core_port::EvaluationError::new("boom"));
    let source = std::error::Error::source(&signal).expect("inner error source");
    assert_eq!(source.to_string(), "EvaluationError: boom");
}

#[test]
fn test_runtime_error_accumulates_nested_frames() {
    let source = "(int-add missing 1)";
    let graph = parse(source).expect("parse failed");
    let call_id = graph.top_level_form_ids()[0];
    let env = Environment::new(None);
    let mut ev = Evaluator::new(graph);

    let err = ev
        .eval(call_id, &env)
        .expect_err("expected unknown-name error");
    match err {
        caap_core_port::EvalSignal::Error(error) => {
            let names: Vec<Option<&str>> = error
                .frames()
                .iter()
                .map(|frame| frame.name.as_deref())
                .collect();
            assert_eq!(names, vec![Some("missing"), Some("int-add")]);
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn test_diagnostic_from_runtime_error_renders_source_and_stack() {
    let source = "(runtime-error \"oops\")";
    let graph = parse(source).expect("parse failed");
    let call_id = graph.top_level_form_ids()[0];
    let env = Environment::new(None);
    let mut ev = Evaluator::new(graph);
    let err = ev.eval(call_id, &env).expect_err("expected runtime error");
    let caap_core_port::EvalSignal::Error(error) = err else {
        panic!("expected error signal");
    };

    let diagnostic = caap_core_port::Diagnostic::from_evaluation_error(&error);
    assert_eq!(diagnostic.code.as_deref(), Some("CAAP-RUNTIME-001"));
    assert_eq!(diagnostic.message, "oops");
    assert_eq!(diagnostic.stack_trace.len(), 1);

    let rendered = caap_core_port::render_diagnostic(&diagnostic, Some(source));
    assert!(rendered.contains("error[CAAP-RUNTIME-001]: oops"));
    assert!(rendered.contains("--> <input>:1:1"));
    assert!(rendered.contains("stack trace:"));
    assert!(rendered.contains("runtime-error"));
}

#[test]
fn test_diagnostic_explanation_registry_stores_help() {
    let mut registry = caap_core_port::DiagnosticExplanationRegistry::new();
    let explanation = caap_core_port::DiagnosticExplanation::new(
        "CAAP-COMPILER-001",
        "Missing compiler stages",
        "The compiler session has no registered stage graph.",
    )
    .unwrap()
    .with_help(["Run explicit bootstrap before compile.".to_string()])
    .unwrap();

    registry.register(explanation);

    let stored = registry
        .explain("CAAP-COMPILER-001")
        .unwrap()
        .expect("explanation should exist");
    assert_eq!(stored.title, "Missing compiler stages");
    assert_eq!(stored.help, vec!["Run explicit bootstrap before compile."]);
    assert_eq!(registry.codes(), vec!["CAAP-COMPILER-001"]);
}

#[test]
fn test_compiler_event_log_filters_by_kind() {
    let mut compiler = caap_core_port::CompilerHost::new().new_session();
    compiler.emit_event(
        caap_core_port::CompilerEvent::with_target(
            "query.plan",
            Some("compile_unit".to_string()),
            "planned compile query",
            [("stage".to_string(), "compile_unit".to_string())],
        )
        .unwrap(),
    );
    compiler.emit_event(
        caap_core_port::CompilerEvent::new("bootstrap.raw", "executed bootstrap").unwrap(),
    );

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
    let mut cache = caap_core_port::ArtifactCache::new();
    let key = caap_core_port::ArtifactKey::pair("source", "main").unwrap();

    cache
        .store(
            key.clone(),
            caap_core_port::ArtifactValue::Text("(int-add 1 2)".to_string()),
            [],
        )
        .unwrap();

    assert_eq!(
        cache.get(&key),
        Some(&caap_core_port::ArtifactValue::Text(
            "(int-add 1 2)".to_string()
        ))
    );
    assert_eq!(cache.stats().hits, 1);
    assert_eq!(cache.stats().misses, 0);

    let missing = caap_core_port::ArtifactKey::pair("source", "missing").unwrap();
    assert_eq!(cache.get(&missing), None);
    assert_eq!(cache.stats().misses, 1);
}

#[test]
fn test_artifact_cache_dependency_invalidation_propagates() {
    let mut cache = caap_core_port::ArtifactCache::new();
    let source = caap_core_port::ArtifactKey::pair("source", "main.caap").unwrap();
    let parsed = caap_core_port::ArtifactKey::pair("parsed", "main.caap").unwrap();
    let checked = caap_core_port::ArtifactKey::pair("checked", "main.caap").unwrap();

    cache
        .store(
            source.clone(),
            caap_core_port::ArtifactValue::Text("source".to_string()),
            [],
        )
        .unwrap();
    cache
        .store(
            parsed.clone(),
            caap_core_port::ArtifactValue::Text("parsed".to_string()),
            [source.clone()],
        )
        .unwrap();
    cache
        .store(
            checked.clone(),
            caap_core_port::ArtifactValue::Text("checked".to_string()),
            [parsed.clone()],
        )
        .unwrap();

    let record = caap_core_port::ArtifactInvalidationRecord::new("source-change", source.clone())
        .unwrap()
        .with_changed_inputs(["main.caap".to_string()])
        .unwrap();
    cache.mark_dirty(record);

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
fn test_artifact_cache_read_aware_invalidation_prunes_irrelevant_dependents() {
    let mut cache = caap_core_port::ArtifactCache::new();
    let source = caap_core_port::ArtifactKey::pair("source", "main.caap").unwrap();
    let reads_changed = caap_core_port::ArtifactKey::pair("query", "reads-changed").unwrap();
    let reads_other = caap_core_port::ArtifactKey::pair("query", "reads-other").unwrap();
    let downstream = caap_core_port::ArtifactKey::pair("query", "downstream").unwrap();

    cache
        .store(
            source.clone(),
            caap_core_port::ArtifactValue::Text("source".to_string()),
            [],
        )
        .unwrap();
    cache
        .store(
            reads_changed.clone(),
            query_artifact_semantic_with_record_reads(
                "node:1@demo.changed",
                "node:1@demo.fact",
                "main.caap",
            ),
            [source.clone()],
        )
        .unwrap();
    cache
        .store(
            reads_other.clone(),
            query_artifact_semantic_with_record_reads(
                "node:2@demo.other",
                "node:2@demo.fact",
                "other.caap",
            ),
            [source.clone()],
        )
        .unwrap();
    cache
        .store(
            downstream.clone(),
            caap_core_port::ArtifactValue::Text("downstream".to_string()),
            [reads_changed.clone()],
        )
        .unwrap();

    let record = caap_core_port::ArtifactInvalidationRecord::new("source-change", source.clone())
        .unwrap()
        .with_changed_inputs(["node:1@demo.fact".to_string()])
        .unwrap();
    cache
        .mark_dirty_with_changes(
            record,
            Vec::<String>::new(),
            ["node:1@demo.fact".to_string()],
            Vec::<String>::new(),
        )
        .unwrap();

    assert!(cache.is_dirty(&source));
    assert!(cache.is_dirty(&reads_changed));
    assert!(cache.is_dirty(&downstream));
    assert!(!cache.is_dirty(&reads_other));
    assert_eq!(
        cache
            .dirty_record(&reads_changed)
            .unwrap()
            .upstream_key
            .as_ref(),
        Some(&source)
    );
    assert_eq!(
        cache
            .dirty_record(&downstream)
            .unwrap()
            .upstream_key
            .as_ref(),
        Some(&reads_changed)
    );
}

fn query_artifact_semantic_with_record_reads(
    read_subject: &str,
    read_cell: &str,
    read_file: &str,
) -> caap_core_port::ArtifactValue {
    caap_core_port::ArtifactValue::Semantic(
        caap_core_port::SemanticValue::map([(
            "execution_summary".to_string(),
            caap_core_port::SemanticValue::List(vec![caap_core_port::SemanticValue::map([
                (
                    "reads_subjects".to_string(),
                    caap_core_port::SemanticValue::List(vec![caap_core_port::SemanticValue::Str(
                        read_subject.to_string(),
                    )]),
                ),
                (
                    "read_cells".to_string(),
                    caap_core_port::SemanticValue::List(vec![caap_core_port::SemanticValue::Str(
                        read_cell.to_string(),
                    )]),
                ),
                (
                    "reads_files".to_string(),
                    caap_core_port::SemanticValue::List(vec![caap_core_port::SemanticValue::Str(
                        read_file.to_string(),
                    )]),
                ),
            ])
            .unwrap()]),
        )])
        .unwrap(),
    )
}

#[test]
fn test_artifact_cache_snapshot_restore_rebuilds_dependency_index() {
    let mut cache = caap_core_port::ArtifactCache::new();
    let source = caap_core_port::ArtifactKey::pair("source", "main.caap").unwrap();
    let lowered = caap_core_port::ArtifactKey::pair("lowered", "main.caap").unwrap();

    cache
        .store(
            source.clone(),
            caap_core_port::ArtifactValue::Text("source".to_string()),
            [],
        )
        .unwrap();
    cache
        .store(
            lowered.clone(),
            caap_core_port::ArtifactValue::Semantic(caap_core_port::SemanticValue::Str(
                "lowered".to_string(),
            )),
            [source.clone()],
        )
        .unwrap();
    assert!(cache.get(&lowered).is_some());

    let snapshot = cache.snapshot();
    cache
        .store(
            caap_core_port::ArtifactKey::pair("extra", "main.caap").unwrap(),
            caap_core_port::ArtifactValue::Bytes(vec![1, 2, 3]),
            [],
        )
        .unwrap();
    cache.mark_dirty(
        caap_core_port::ArtifactInvalidationRecord::new("source-change", source.clone()).unwrap(),
    );

    cache.restore_snapshot(snapshot).unwrap();

    assert!(!cache.is_dirty(&source));
    assert!(!cache.is_dirty(&lowered));
    assert_eq!(cache.dependents_for(&source), vec![lowered.clone()]);
    assert_eq!(cache.stats().hits, 1);
}

#[test]
fn test_artifact_cache_project_snapshot_by_kind_is_restore_ready() {
    let mut cache = caap_core_port::ArtifactCache::new();
    let source = caap_core_port::ArtifactKey::pair("source", "main.caap").unwrap();
    let parsed = caap_core_port::ArtifactKey::pair("parsed", "main.caap").unwrap();
    let checked = caap_core_port::ArtifactKey::pair("checked", "main.caap").unwrap();

    cache
        .store(
            source.clone(),
            caap_core_port::ArtifactValue::Text("source".to_string()),
            [],
        )
        .unwrap();
    cache
        .store(
            parsed.clone(),
            caap_core_port::ArtifactValue::Text("parsed".to_string()),
            [source],
        )
        .unwrap();
    cache
        .store(
            checked.clone(),
            caap_core_port::ArtifactValue::Text("checked".to_string()),
            [parsed.clone()],
        )
        .unwrap();

    let projection = cache.project_snapshot_by_kind("parsed").unwrap();
    assert_eq!(projection.entries.len(), 1);
    assert_eq!(projection.entries[0].0, parsed);
    assert_eq!(projection.dependencies, vec![(parsed.clone(), Vec::new())]);

    let mut restored = caap_core_port::ArtifactCache::new();
    restored.restore_snapshot(projection).unwrap();
    assert!(restored.peek(&parsed).is_some());
    assert!(restored.peek(&checked).is_none());
    assert!(restored.dependents_for(&parsed).is_empty());
}

#[test]
fn test_artifact_cache_file_payload_validates_format() {
    let mut cache = caap_core_port::ArtifactCache::new();
    let key = caap_core_port::ArtifactKey::pair("source", "main.caap").unwrap();
    cache
        .store(
            key,
            caap_core_port::ArtifactValue::Text("source".to_string()),
            [],
        )
        .unwrap();

    let cache_file = cache.cache_file();
    cache_file.validate().unwrap();
    assert_eq!(
        cache_file.format_name,
        caap_core_port::ArtifactCacheFile::FORMAT_NAME
    );
    assert_eq!(
        cache_file.format_version,
        caap_core_port::ArtifactCacheFile::FORMAT_VERSION
    );

    let mut invalid = cache_file;
    invalid.format_version += 1;
    assert!(invalid.validate().is_err());
}

#[test]
fn test_artifact_cache_file_roundtrips_through_json_file() {
    let mut cache = caap_core_port::ArtifactCache::new();
    let source = caap_core_port::ArtifactKey::pair("source", "main.caap").unwrap();
    let lowered = caap_core_port::ArtifactKey::pair("lowered", "main.caap").unwrap();
    let lineage = caap_core_port::ArtifactKey::pair("lineage", "main.caap").unwrap();

    cache
        .store_with_lineage(
            source.clone(),
            caap_core_port::ArtifactValue::Source(
                caap_core_port::SourceArtifact::inline_with_label("(int-add 1 2)", "main.caap")
                    .unwrap(),
            ),
            [],
            lineage.clone(),
        )
        .unwrap();
    cache
        .store(
            lowered.clone(),
            caap_core_port::ArtifactValue::Semantic(
                caap_core_port::SemanticValue::map([(
                    "status".to_string(),
                    caap_core_port::SemanticValue::Str("ok".to_string()),
                )])
                .unwrap(),
            ),
            [source.clone()],
        )
        .unwrap();
    cache.mark_dirty(
        caap_core_port::ArtifactInvalidationRecord::new("source-change", source.clone())
            .unwrap()
            .with_lineage(lineage.clone(), "lineage")
            .unwrap()
            .with_changed_inputs(["main.caap".to_string()])
            .unwrap(),
    );

    let path = std::env::temp_dir().join(format!(
        "caap-artifact-cache-{}-{}.json",
        std::process::id(),
        line!()
    ));
    cache.save_cache_file(&path).unwrap();

    let mut restored = caap_core_port::ArtifactCache::new();
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
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let key = caap_core_port::ArtifactKey::pair("source", "main.caap").unwrap();
    compiler
        .artifact_cache_mut()
        .store(
            key.clone(),
            caap_core_port::ArtifactValue::Text("(int-add 1 2)".to_string()),
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
            .by_kind("compiler.artifact-cache.save")
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
        Some(&caap_core_port::ArtifactValue::Text(
            "(int-add 1 2)".to_string()
        ))
    );
    assert_eq!(
        restored
            .events()
            .by_kind("compiler.artifact-cache.load")
            .unwrap()[0]
            .target
            .as_deref(),
        Some(path.to_string_lossy().as_ref())
    );
}

#[test]
fn test_artifact_cache_reusable_snapshot_restores_lineage_state() {
    let mut cache = caap_core_port::ArtifactCache::new();
    let lineage = caap_core_port::ArtifactKey::new([
        "parse-surface-source".to_string(),
        "compile_time".to_string(),
        "/workspace/main.caap".to_string(),
    ])
    .unwrap();
    let key = caap_core_port::ArtifactKey::new([
        "parse-surface".to_string(),
        "parse".to_string(),
        "compile_time".to_string(),
        "/workspace/main.caap".to_string(),
        "token-a".to_string(),
    ])
    .unwrap();

    cache
        .store_with_lineage(
            key.clone(),
            caap_core_port::ArtifactValue::Text("template".to_string()),
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
    let source = caap_core_port::SourceArtifact::inline("abc").unwrap();

    assert_eq!(
        source.fingerprint.as_str(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        source
            .parse_surface_key("parse", caap_core_port::PhasePolicy::CompileTime)
            .unwrap()
            .parts(),
        &[
            "parse-surface-inline".to_string(),
            "parse".to_string(),
            "compile_time".to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string(),
        ]
    );
    assert_eq!(
        source
            .parse_surface_lineage_id(caap_core_port::PhasePolicy::CompileTime)
            .unwrap()
            .parts(),
        &[
            "parse-surface-inline".to_string(),
            "compile_time".to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".to_string(),
        ]
    );
}

#[test]
fn test_source_artifact_path_key_uses_path_token() {
    let source = caap_core_port::SourceArtifact::path(
        "/workspace/main.caap",
        "mtime:123:size:10",
        "(int-add 1 2)",
    )
    .unwrap();

    assert_eq!(
        source
            .parse_surface_key("parse", caap_core_port::PhasePolicy::CompileTime)
            .unwrap()
            .parts(),
        &[
            "parse-surface".to_string(),
            "parse".to_string(),
            "compile_time".to_string(),
            "/workspace/main.caap".to_string(),
            "mtime:123:size:10".to_string(),
        ]
    );
    assert_eq!(
        source
            .parse_surface_lineage_id(caap_core_port::PhasePolicy::CompileTime)
            .unwrap()
            .parts(),
        &[
            "parse-surface-source".to_string(),
            "compile_time".to_string(),
            "/workspace/main.caap".to_string(),
        ]
    );
}

#[test]
fn test_artifact_cache_lineage_replacement_invalidates_previous_head() {
    let mut cache = caap_core_port::ArtifactCache::new();
    let lineage = caap_core_port::ArtifactKey::new([
        "parse-surface-inline".to_string(),
        "compile_time".to_string(),
        "lineage".to_string(),
    ])
    .unwrap();
    let first = caap_core_port::ArtifactKey::new([
        "parse-surface-inline".to_string(),
        "parse".to_string(),
        "compile_time".to_string(),
        "digest-a".to_string(),
    ])
    .unwrap();
    let second = caap_core_port::ArtifactKey::new([
        "parse-surface-inline".to_string(),
        "parse".to_string(),
        "compile_time".to_string(),
        "digest-b".to_string(),
    ])
    .unwrap();
    let dependent = caap_core_port::ArtifactKey::pair("checked", "main").unwrap();

    cache
        .store_with_lineage(
            first.clone(),
            caap_core_port::ArtifactValue::Text("first".to_string()),
            [],
            lineage.clone(),
        )
        .unwrap();
    cache
        .store(
            dependent.clone(),
            caap_core_port::ArtifactValue::Text("dependent".to_string()),
            [first.clone()],
        )
        .unwrap();
    cache
        .store_with_lineage(
            second.clone(),
            caap_core_port::ArtifactValue::Text("second".to_string()),
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
fn test_changed_inputs_for_lineage_matches_python_labels() {
    let lineage = caap_core_port::ArtifactKey::new([
        "unit-input".to_string(),
        "unit-id".to_string(),
        "stage".to_string(),
        "compile_time".to_string(),
    ])
    .unwrap();
    let previous = caap_core_port::ArtifactKey::new([
        "unit-input".to_string(),
        "parse".to_string(),
        "fingerprint-a".to_string(),
        "compile_time".to_string(),
        "names-a".to_string(),
    ])
    .unwrap();
    let replacement = caap_core_port::ArtifactKey::new([
        "unit-input".to_string(),
        "parse".to_string(),
        "fingerprint-b".to_string(),
        "compile_time".to_string(),
        "names-b".to_string(),
    ])
    .unwrap();

    assert_eq!(
        caap_core_port::changed_inputs_for_lineage(&lineage, &previous, &replacement),
        vec!["unit_fingerprint".to_string(), "names_version".to_string()]
    );
}

#[test]
fn test_source_template_cache_reuses_materialized_unit_template() {
    let mut cache = caap_core_port::SourceTemplateCache::new();
    let source = caap_core_port::SourceArtifact::inline("(int-add 1 2)").unwrap();
    let mut materialize_calls = 0;

    let first = cache
        .load(
            source.clone(),
            "parse",
            caap_core_port::PhasePolicy::CompileTime,
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
            caap_core_port::PhasePolicy::CompileTime,
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
fn test_source_template_cache_path_token_change_replaces_lineage_head() {
    let mut cache = caap_core_port::SourceTemplateCache::new();
    let first_source =
        caap_core_port::SourceArtifact::path("/workspace/main.caap", "token-a", "(int-add 1 2)")
            .unwrap();
    let second_source =
        caap_core_port::SourceArtifact::path("/workspace/main.caap", "token-b", "(int-add 1 3)")
            .unwrap();

    let first = cache
        .load(
            first_source,
            "parse",
            caap_core_port::PhasePolicy::CompileTime,
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
            caap_core_port::PhasePolicy::CompileTime,
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

#[test]
fn test_compiler_host_new_session_is_bare() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    assert!(compiler.units().is_empty());
    assert!(!compiler.has_bootstrap_executions());
    assert!(compiler.registered_stages().is_empty());

    let err = compiler
        .compile(&mut unit)
        .expect_err("compile should require bootstrap stages");
    assert_eq!(err, "no compiler stages registered");
    assert_eq!(compiler.diagnostics().len(), 1);
    assert_eq!(
        compiler.diagnostics()[0].code.as_deref(),
        Some("CAAP-COMPILER-001")
    );
}

#[test]
fn test_compiler_host_registers_system_libraries_explicitly() {
    let mut host = caap_core_port::CompilerHost::new();
    assert!(host.runtime_services().library_names().is_empty());

    host.register_default_runtime_system_libraries().unwrap();
    assert!(host.host_version() > 0);
    assert_eq!(
        host.runtime_services().library_names(),
        vec!["env", "format", "fs", "io", "net", "os", "path", "process", "time"]
    );

    let compiler = host.new_session();
    assert!(compiler.registered_stages().is_empty());
    assert!(compiler
        .host()
        .runtime_services()
        .export("path", "basename", caap_core_port::PhasePolicy::Runtime)
        .is_ok());
}

#[test]
fn test_compiler_session_loads_surface_text_template_through_cache() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();

    let first = compiler
        .load_surface_text_template("(int-add 1 2)", "inline")
        .unwrap();
    let second = compiler
        .load_surface_text_template("(int-add 1 2)", "inline")
        .unwrap();

    assert_eq!(first.key, second.key);
    assert!(!first.cache_hit);
    assert!(second.cache_hit);
    assert_eq!(first.template.unit_id, "inline");
    assert_eq!(second.template.ir.top_level_forms.len(), 1);
    assert_eq!(
        compiler.source_templates().artifact_cache().stats().misses,
        1
    );
    assert_eq!(compiler.source_templates().artifact_cache().stats().hits, 1);
    assert!(!compiler.has_bootstrap_executions());
    let events = compiler.events().by_kind("source.template.load").unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].target.as_deref(), Some("inline"));
    assert!(events[0]
        .metadata
        .contains(&("origin".to_string(), "inline".to_string())));
    assert!(events[0]
        .metadata
        .contains(&("cache_hit".to_string(), "false".to_string())));
    assert!(events[1]
        .metadata
        .contains(&("cache_hit".to_string(), "true".to_string())));
    assert!(events[0]
        .metadata
        .iter()
        .any(|(key, _)| key == "elapsed_ms"));
}

#[test]
fn test_compiler_session_loads_surface_path_template_through_cache() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-source-template-{}-{}.caap",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, "(int-add 1 2)").unwrap();

    let first = compiler
        .load_surface_path_template(&path, "path-unit")
        .unwrap();
    let second = compiler
        .load_surface_path_template(&path, "path-unit")
        .unwrap();

    assert_eq!(first.key, second.key);
    assert!(!first.cache_hit);
    assert!(second.cache_hit);
    assert_eq!(first.template.unit_id, "path-unit");
    assert_eq!(
        compiler.source_templates().artifact_cache().stats().misses,
        1
    );
    assert_eq!(compiler.source_templates().artifact_cache().stats().hits, 1);
    assert_eq!(
        compiler.events().by_kind("source.template.load").unwrap()[0]
            .target
            .as_deref(),
        Some("path-unit")
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_compiler_surface_path_template_records_syntax_source_and_span_paths() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-source-span-path-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(&path, "(int-add 1 2)").unwrap();

    let artifact = compiler
        .load_surface_path_template(&path, "demo.path.syntax")
        .unwrap();
    let expected_path = std::fs::canonicalize(&path)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let top_id = artifact.template.ir.top_level_forms[0];
    let span = artifact
        .template
        .ir
        .source_spans
        .iter()
        .find(|(node_id, _)| *node_id == top_id)
        .map(|(_, span)| span)
        .expect("missing top-level source span");

    assert_eq!(artifact.template.syntax_state.language, "caap");
    assert_eq!(
        artifact.template.syntax_state.source_path.as_deref(),
        Some(expected_path.as_str())
    );
    assert!(artifact.template.syntax_state.source_fingerprint.is_some());
    assert_eq!(span.path.as_deref(), Some(expected_path.as_str()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_compiler_surface_path_template_rejects_syntax_import_body() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-syntax-import-body-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &path,
        r#"
          (module "demo.syntax.body")
          (syntax-import "demo.syntax")
          (int-add 1 2)
        "#,
    )
    .unwrap();

    let err = compiler
        .load_surface_path_template(&path, "demo.syntax.body")
        .expect_err("syntax-import source with body must use dynamic syntax loader");

    assert!(err.contains("declares syntax imports"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn test_compiler_register_stage_allows_compile_to_record_unit() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    compiler.register_stage("compile_unit").unwrap();
    compiler.compile(&mut unit).unwrap();

    assert!(compiler.get_unit("main").unwrap().is_some());
    assert!(compiler.name_service().contains("main"));
    assert_eq!(compiler.diagnostics().len(), 0);
    assert_eq!(
        compiler.events().by_kind("compiler.compile").unwrap()[0]
            .target
            .as_deref(),
        Some("main")
    );
    assert_eq!(
        compiler.events().by_kind("compiler.unit.register").unwrap()[0]
            .target
            .as_deref(),
        Some("main")
    );
}

#[test]
fn test_compiler_registry_registers_and_looks_up_values() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();

    let value = compiler
        .register_value("demo.value", RuntimeValue::Int(42))
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(42));
    assert_eq!(
        compiler.lookup_registered_value("demo.value").unwrap(),
        Some(&RuntimeValue::Int(42))
    );
    assert_eq!(
        compiler.require_registered_value("demo.value").unwrap(),
        &RuntimeValue::Int(42)
    );
    assert_eq!(compiler.registry().registered_names(), vec!["demo.value"]);
    assert_eq!(compiler.registry().version(), 1);
    assert_eq!(
        compiler
            .events()
            .by_kind("compiler.registry.value.register")
            .unwrap()[0]
            .target
            .as_deref(),
        Some("demo.value")
    );

    let missing = compiler.require_registered_value("missing").unwrap_err();
    assert_eq!(missing, "compiler registry does not contain \"missing\"");

    let duplicate = compiler
        .register_value("demo.value", RuntimeValue::Int(43))
        .unwrap_err();
    assert_eq!(
        duplicate,
        "compiler registry already contains \"demo.value\""
    );
}

#[test]
fn test_compiler_registry_marks_compile_time_functions_and_snapshots() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();

    compiler
        .register_compile_time_function("demo.fn", RuntimeValue::Str("callable".into()))
        .unwrap();
    assert_eq!(
        compiler.lookup_registered_value("demo.fn").unwrap(),
        Some(&RuntimeValue::Str("callable".into()))
    );
    assert!(compiler
        .registry()
        .is_compile_time_function("demo.fn")
        .unwrap());
    assert_eq!(
        compiler.registry().compile_time_function_names(),
        vec!["demo.fn"]
    );

    let snapshot = compiler.registry_snapshot();
    compiler
        .register_value("demo.extra", RuntimeValue::Bool(true))
        .unwrap();
    assert!(compiler
        .lookup_registered_value("demo.extra")
        .unwrap()
        .is_some());

    compiler.restore_registry_snapshot(snapshot).unwrap();

    assert!(compiler
        .lookup_registered_value("demo.extra")
        .unwrap()
        .is_none());
    assert!(compiler
        .registry()
        .is_compile_time_function("demo.fn")
        .unwrap());
    assert_eq!(compiler.registry().registered_names(), vec!["demo.fn"]);
    assert_eq!(
        compiler
            .events()
            .by_kind("compiler.registry.restore")
            .unwrap()[0]
            .metadata,
        vec![
            ("registered_count".to_string(), "1".to_string()),
            ("registry_version".to_string(), "1".to_string()),
        ]
    );
}

#[test]
fn test_compiler_registry_rejects_empty_names() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();

    assert_eq!(
        compiler
            .register_value("", RuntimeValue::Null)
            .expect_err("empty registry name should be rejected"),
        "compiler registry names must be non-empty strings"
    );
    assert_eq!(
        compiler
            .lookup_registered_value("")
            .expect_err("empty registry lookup should be rejected"),
        "compiler registry names must be non-empty strings"
    );
    assert_eq!(
        compiler
            .register_compile_time_function("", RuntimeValue::Null)
            .expect_err("empty compile-time function name should be rejected"),
        "compiler registry names must be non-empty strings"
    );
}

#[test]
fn test_ctfe_compiler_registry_builtins_mutate_session_registry() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(ctfe-compiler-register-value compiler \"demo.value\" 42)").unwrap();
    let unit = Unit::from_graph("registry-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(42));
    assert_eq!(
        compiler.lookup_registered_value("demo.value").unwrap(),
        Some(&RuntimeValue::Int(42))
    );
    assert_eq!(
        compiler
            .events()
            .by_kind("compiler.registry.value.register")
            .unwrap()[0]
            .target
            .as_deref(),
        Some("demo.value")
    );
}

#[test]
fn test_ctfe_compiler_lookup_value_supports_default_and_missing_error() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    compiler
        .register_value("demo.value", RuntimeValue::Str("stored".into()))
        .unwrap();
    let graph = parse(
        "(do
          (ctfe-compiler-lookup-value compiler \"missing\" \"fallback\")
          (ctfe-compiler-lookup-value compiler \"demo.value\"))",
    )
    .unwrap();
    let unit = Unit::from_graph("registry-lookup", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Str("stored".into()));

    let missing_graph = parse("(ctfe-compiler-lookup-value compiler \"missing\")").unwrap();
    let missing_unit = Unit::from_graph("registry-missing", missing_graph).unwrap();
    let err = compiler
        .evaluation()
        .evaluate(&missing_unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap_err();
    let caap_core_port::EvalSignal::Error(error) = err else {
        panic!("expected lookup error");
    };
    assert_eq!(
        error.message(),
        "compiler registry does not contain \"missing\""
    );
}

#[test]
fn test_ctfe_compiler_register_compile_time_function_and_emit_event_builtins() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse(
        "(do
          (ctfe-compiler-register-compile-time-function compiler \"demo.fn\" \"callable\")
          (ctfe-compiler-emit-event compiler \"demo\" \"action\" \"hello\" (map-of \"k\" \"v\")))",
    )
    .unwrap();
    let unit = Unit::from_graph("registry-function-event", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Null);
    assert!(compiler
        .registry()
        .is_compile_time_function("demo.fn")
        .unwrap());
    assert_eq!(
        compiler.lookup_registered_value("demo.fn").unwrap(),
        Some(&RuntimeValue::Str("callable".into()))
    );
    let event = &compiler.events().by_kind("demo.action").unwrap()[0];
    assert_eq!(event.message, "hello");
    assert_eq!(event.metadata, vec![("k".to_string(), "v".to_string())]);
}

#[test]
fn test_ctfe_compiler_stage_register_builtin_builds_stage_contract() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse(
        "(do
          (ctfe-compiler-stage-register compiler \"parse\" null \"compile\" (list-of \"source\") null (list-of \"caap-source\"))
          (ctfe-compiler-stage-register compiler \"lower\" (list-of \"parse\") \"compile\" (list-of \"compile-unit\") \"parse\" (list-of \"unit\")))",
    )
    .unwrap();
    let unit = Unit::from_graph("stage-register", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Null);
    assert_eq!(compiler.registered_stages(), vec!["lower", "parse"]);
    assert_eq!(
        compiler
            .provider_registry()
            .default_stage_for_family("compile")
            .unwrap(),
        "parse"
    );
    assert_eq!(
        compiler
            .provider_registry()
            .stage_for_input_kind("caap-source")
            .unwrap(),
        "parse"
    );
    assert_eq!(
        compiler
            .provider_registry()
            .resolve_stage("compile-unit")
            .unwrap(),
        "lower"
    );
    assert_eq!(
        compiler
            .provider_registry()
            .restart_stage_for("lower")
            .unwrap(),
        "parse"
    );
    let lower = compiler
        .provider_registry()
        .stage_spec("lower")
        .unwrap()
        .unwrap();
    assert_eq!(lower.requires, vec!["parse"]);
    assert_eq!(lower.family_label.as_deref(), Some("compile"));
    assert_eq!(lower.input_kinds, vec!["unit"]);
}

#[test]
fn test_ctfe_compiler_stage_alias_and_restart_policy_register_builtins() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse(
        "(do
          (ctfe-compiler-stage-register compiler \"parse\")
          (ctfe-compiler-stage-register compiler \"validate\" (list-of \"parse\"))
          (ctfe-compiler-stage-alias-register compiler \"validate\" \"compile\")
          (ctfe-compiler-stage-restart-policy-register compiler \"validate\" \"parse\"))",
    )
    .unwrap();
    let unit = Unit::from_graph("stage-alias-restart", graph).unwrap();

    compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(
        compiler
            .provider_registry()
            .resolve_stage("compile")
            .unwrap(),
        "validate"
    );
    assert_eq!(
        compiler
            .provider_registry()
            .restart_stage_for("validate")
            .unwrap(),
        "parse"
    );
    assert_eq!(
        compiler
            .queries()
            .plan_query("compile", caap_core_port::PhasePolicy::CompileTime)
            .unwrap()
            .steps
            .iter()
            .map(|step| step.stage.as_str())
            .collect::<Vec<_>>(),
        vec!["parse", "validate"]
    );
}

#[test]
fn test_evaluate_bootstrap_file_prepare_pipeline_runs_unit_input_route() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    compiler
        .register_stage_spec(
            caap_core_port::QueryStageSpec::new("parse_surface")
                .unwrap()
                .with_input_kinds(vec!["surface".to_string()])
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_stage_spec(
            caap_core_port::QueryStageSpec::new("unit_input")
                .unwrap()
                .with_input_kinds(vec!["unit".to_string()])
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_stage_spec(
            caap_core_port::QueryStageSpec::new("normalize_before_resolve")
                .unwrap()
                .with_requires(vec!["parse_surface".to_string(), "unit_input".to_string()])
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_stage_spec(
            caap_core_port::QueryStageSpec::new("compile_unit")
                .unwrap()
                .with_requires(vec!["normalize_before_resolve".to_string()])
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_provider(
            "parse-provider-should-not-run",
            "parse_surface",
            caap_core_port::PhasePolicy::CompileTime,
            |_compiler, _unit| Err("parse_surface provider should not run for unit input".into()),
        )
        .unwrap();
    compiler
        .register_provider(
            "prepare-marker-provider",
            "normalize_before_resolve",
            caap_core_port::PhasePolicy::CompileTime,
            |compiler, _unit| {
                compiler.register_value("prepared.value", RuntimeValue::Str("prepared".into()))?;
                Ok(())
            },
        )
        .unwrap();

    let source_path = std::env::temp_dir().join(format!(
        "caap-rust-prepare-pipeline-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &source_path,
        r#"(ctfe-compiler-lookup-value compiler "prepared.value")"#,
    )
    .unwrap();

    let bridge = Rc::new(caap_core_port::CompilerBridgeValue::from_compiler(
        &compiler,
    ));
    let capture = bridge
        .evaluate_bootstrap_file(
            &source_path,
            Vec::<(String, RuntimeValue)>::new(),
            Vec::<String>::new(),
            0,
            true,
            RuntimeValue::HostObject(bridge.clone()),
        )
        .unwrap();
    std::fs::remove_file(&source_path).ok();

    assert_eq!(capture.value, Some(RuntimeValue::Str("prepared".into())));
    assert!(capture.diagnostics.is_empty());
}

#[test]
fn test_ctfe_compiler_provider_register_runs_later_query_callback() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let bootstrap_graph = parse(
        "(do
          (ctfe-compiler-stage-register compiler \"analyze\" null \"analysis\")
          (ctfe-compiler-provider-register
            compiler
            \"event-provider\"
            \"analyze\"
            (lambda (ctx root)
              (ctfe-compiler-emit-event
                compiler
                \"provider\"
                \"ran\"
                (host-value-kind (ctfe-provider-unit ctx))
                (map-of \"unit_kind\" (host-value-kind (ctfe-provider-unit ctx)))))
            null
            (list-of \"emit-events\")
            (map-of
              \"reads\" (list-of \"unit\")
              \"writes\" (list-of \"facts\")
              \"cache_scope\" \"unit\"
              \"resume_policy\" \"safe\"
              \"input_schema\" null)))",
    )
    .unwrap();
    let bootstrap_unit = Unit::from_graph("provider-register", bootstrap_graph).unwrap();
    compiler
        .evaluation()
        .evaluate(
            &bootstrap_unit,
            caap_core_port::PhasePolicy::CompileTime,
            [],
        )
        .unwrap();

    let providers = compiler.provider_registry().ordered_providers();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].name, "event-provider");
    assert_eq!(providers[0].stage, "analyze");
    assert_eq!(providers[0].family.as_deref(), Some("analysis"));
    assert_eq!(providers[0].effect_tags, vec!["emit_events"]);
    assert_eq!(providers[0].reads, vec!["unit"]);
    assert_eq!(providers[0].writes, vec!["facts"]);
    assert_eq!(providers[0].cache_scope, "unit");
    assert_eq!(providers[0].input_schema, None);

    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("provider-target", graph).unwrap();
    let plan = compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();

    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].provider_names, vec!["event-provider"]);
    assert_eq!(plan.steps[0].effect_tags, vec!["emit_events"]);
    let event = &compiler.events().by_kind("provider.ran").unwrap()[0];
    assert_eq!(event.message, "unit");
    assert_eq!(
        event.metadata,
        vec![("unit_kind".to_string(), "unit".to_string())]
    );
}

#[test]
fn test_ctfe_compiler_lists_registered_stages_and_providers() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse(
        "(do
          (ctfe-compiler-stage-register compiler \"parse\" null \"compile\" (list-of \"source\"))
          (ctfe-compiler-provider-register
            compiler
            \"noop-provider\"
            \"source\"
            (lambda (compiler unit) null)
            null
            (list-of \"emit-events\"))
          (list-of
            (size (ctfe-compiler-list-stages compiler))
            (get (get (ctfe-compiler-list-stages compiler) 0) \"name\")
            (get (get (ctfe-compiler-list-stages compiler) 0) \"family\")
            (size (ctfe-compiler-list-providers compiler))
            (get (get (ctfe-compiler-list-providers compiler \"source\") 0) \"name\")
            (get (get (get (ctfe-compiler-list-providers compiler) 0) \"effects\") \"emits\")))",
    )
    .unwrap();
    let unit = Unit::from_graph("compiler-lists", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1));
    assert_eq!(items[1], RuntimeValue::Str("parse".into()));
    assert_eq!(items[2], RuntimeValue::Str("compile".into()));
    assert_eq!(items[3], RuntimeValue::Int(1));
    assert_eq!(items[4], RuntimeValue::Str("noop-provider".into()));
    assert_eq!(
        items[5],
        RuntimeValue::Tuple(vec![RuntimeValue::Str("emit_events".into())].into())
    );
}

#[test]
fn test_ctfe_compiler_graph_projection_helpers() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse(
        "(bind graph
          (map-of
            \"nodes\"
            (list-of
              (map-of \"id\" \"n1\" \"kind\" \"stage\" \"label\" \"parse\")
              (map-of \"id\" \"n2\" \"kind\" \"stage\" \"label\" \"lower\")
              (map-of
                \"id\" \"l1\"
                \"kind\" \"lineage\"
                \"label\" \"source-lineage\"
                \"data\"
                (map-of
                  \"lineage_id\" \"lineage-1\"
                  \"stage\" \"parse\"
                  \"invalidation\"
                  (map-of
                    \"reason_kind\" \"input_changed\"
                    \"changed_inputs\" (list-of \"source\")))))
            \"edges\"
            (list-of
              (map-of \"from\" \"n1\" \"to\" \"n2\" \"kind\" \"requires\" \"data\" (map-of \"why\" \"demo\"))))
          (bind deps (ctfe-compiler-graph-dependencies graph \"requires\")
            (bind changed (ctfe-compiler-graph-changed-lineages graph)
              (list-of
                (get (ctfe-compiler-graph-node-labels graph \"stage\") 0)
                (get (ctfe-compiler-graph-node-labels graph \"stage\") 1)
                (get (get deps 0) \"from\")
                (get (get deps 0) \"to\")
                (get (get (get deps 0) \"data\") \"why\")
                (get (get changed 0) \"lineage_id\")
                (get (get changed 0) \"reason_kind\")
                (get (get (get changed 0) \"changed_inputs\") 0)))))",
    )
    .unwrap();
    let unit = Unit::from_graph("compiler-graph-projection-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("parse".into()));
    assert_eq!(items[1], RuntimeValue::Str("lower".into()));
    assert_eq!(items[2], RuntimeValue::Str("parse".into()));
    assert_eq!(items[3], RuntimeValue::Str("lower".into()));
    assert_eq!(items[4], RuntimeValue::Str("demo".into()));
    assert_eq!(items[5], RuntimeValue::Str("lineage-1".into()));
    assert_eq!(items[6], RuntimeValue::Str("input_changed".into()));
    assert_eq!(items[7], RuntimeValue::Str("source".into()));
}

#[test]
fn test_ctfe_compiler_graph_query_and_lineage_builtins_run_pipeline() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-compiler-graph-query-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "null\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"graph-provider\"
            \"compile_unit\"
            (lambda (compiler unit) null)
            null
            (list-of \"graph-effect\"))
          (bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind query-graph (ctfe-compiler-graph-query compiler \"compile_unit\" unit \"compile_time\")
              (bind lineage-graph (ctfe-compiler-graph-lineage compiler \"compile_unit\" unit \"compile_time\")
                (bind executes (ctfe-compiler-graph-dependencies query-graph \"executes\")
                  (list-of
                    (get query-graph \"graph_kind\")
                    (get (get query-graph \"subject\") \"target\")
                    (get (get query-graph \"metadata\") \"execution_required\")
                    (get (ctfe-compiler-graph-node-labels query-graph \"stage\") 0)
                    (get (ctfe-compiler-graph-node-labels query-graph \"provider\") 0)
                    (get (ctfe-compiler-graph-node-labels query-graph \"artifact\") 0)
                    (get (get executes 0) \"from\")
                    (get (get executes 0) \"to\")
                    (get lineage-graph \"graph_kind\")
                    (get (get lineage-graph \"metadata\") \"step_count\")
                    (get (ctfe-compiler-graph-node-labels lineage-graph \"lineage\") 1)))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("compiler-graph-query-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("query".into()));
    assert_eq!(items[1], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[2], RuntimeValue::Bool(true));
    assert_eq!(items[3], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[4], RuntimeValue::Str("graph-provider".into()));
    assert_eq!(items[5], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[6], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[7], RuntimeValue::Str("graph-provider".into()));
    assert_eq!(items[8], RuntimeValue::Str("lineage".into()));
    assert_eq!(items[9], RuntimeValue::Int(1));
    assert_eq!(items[10], RuntimeValue::Str("compile_unit".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_graph_builtins_accept_initial_bindings() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-compiler-graph-initial-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "placeholder\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"graph-initial-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (ctfe-unit-add-public-name! unit initial-public)))
          (bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind query-graph
              (ctfe-compiler-graph-query
                compiler
                \"compile_unit\"
                unit
                \"compile_time\"
                (map-of \"initial-public\" \"public-from-initial\"))
              (bind lineage-graph
                (ctfe-compiler-graph-lineage
                  compiler
                  \"compile_unit\"
                  unit
                  \"compile_time\"
                  (map-of \"initial-public\" \"public-from-initial\"))
                (bind result-key (get (get query-graph \"metadata\") \"result_key\")
                  (list-of
                    (get result-key 11)
                    (get result-key 12)
                    (get result-key 13)
                    (get lineage-graph \"graph_kind\")
                    (get (get lineage-graph \"metadata\") \"step_count\")))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("compiler-graph-initial-bindings", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("initial-binding".into()));
    assert_eq!(items[1], RuntimeValue::Str("initial-public".into()));
    assert_eq!(
        items[2],
        RuntimeValue::Str("str:public-from-initial".into())
    );
    assert_eq!(items[3], RuntimeValue::Str("lineage".into()));
    assert_eq!(items[4], RuntimeValue::Int(1));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_ir_instantiate_builds_typed_expr_specs() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse(
        "(bind name-spec (ctfe-ir-instantiate \"name\" (map-of \"identifier\" \"x\"))
          (bind literal-spec (ctfe-ir-instantiate \"literal\" (map-of \"value\" 42))
            (bind call-spec
              (ctfe-ir-instantiate
                \"call\"
                (map-of \"callee\" name-spec \"args\" (list-of literal-spec)))
              (bind lambda-spec
                (ctfe-ir-instantiate
                  \"lambda\"
                  (map-of \"params\" (list-of \"x\") \"body\" call-spec))
                (bind bind-spec
                  (ctfe-ir-instantiate
                    \"bind\"
                    (map-of
                      \"bindings\"
                      (list-of (map-of \"name\" \"local\" \"value\" literal-spec))
                      \"body\"
                      lambda-spec))
                  (bind do-spec
                    (ctfe-ir-instantiate
                      \"do\"
                      (map-of \"forms\" (list-of bind-spec call-spec)))
                    (bind if-spec
                      (ctfe-ir-instantiate
                        \"if\"
                        (map-of \"condition\" name-spec \"then\" do-spec \"else\" literal-spec))
                      (bind block-spec
                        (ctfe-ir-instantiate
                          \"block\"
                          (map-of \"label\" \"exit\" \"body\" if-spec))
                        (ctfe-ir-instantiate
                          \"leave\"
                          (map-of \"target\" \"exit\" \"value\" block-spec))))))))))",
    )
    .unwrap();
    let unit = Unit::from_graph("ir-instantiate-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::HostObject(object) = value else {
        panic!("expected ExprSpec host object");
    };
    let spec = object
        .as_any()
        .downcast_ref::<caap_core_port::builtins::ir_builders::ExprSpecBridgeValue>()
        .expect("expected ExprSpecBridgeValue")
        .spec();
    let caap_core_port::ExprSpec::Call(leave_call) = spec else {
        panic!("expected leave call spec");
    };
    let caap_core_port::ExprSpec::Name(callee) = leave_call.callee.as_ref() else {
        panic!("expected leave callee name");
    };
    assert_eq!(callee.identifier, "leave");
    assert_eq!(leave_call.args.len(), 2);
    assert_eq!(
        leave_call.args[0],
        caap_core_port::ExprSpec::literal(caap_core_port::IrLiteralData::Str("exit".to_string()))
    );
    let caap_core_port::ExprSpec::Call(block_call) = &leave_call.args[1] else {
        panic!("expected block call spec");
    };
    let caap_core_port::ExprSpec::Name(block_callee) = block_call.callee.as_ref() else {
        panic!("expected block callee name");
    };
    assert_eq!(block_callee.identifier, "block");
}

#[test]
fn test_ctfe_ir_detached_expr_specs_support_source_span_annotations() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse(
        r#"(bind span (map-of
            "start" 2
            "end" 9
            "start_line" 1
            "start_col" 3
            "end_line" 1
            "end_col" 10)
          (bind name-spec
            (ctfe-ir-instantiate
              "name"
              (map-of "identifier" "spanned-name")
              (map-of "source_span" span))
            (bind literal-spec
              (ctfe-ir-instantiate "literal" (map-of "value" 42))
              (bind set-result (ctfe-meta-annotation-set literal-spec "source_span" span)
              (bind literal-span (ctfe-meta-annotation-get literal-spec "source_span")
                (bind literal-has (ctfe-node-has-annotation literal-spec "source_span")
                  (ctfe-meta-annotation-set-many literal-spec "source_span" null)
                  (list-of
                    (ctfe-node-has-annotation name-spec "source_span")
                    (get (ctfe-meta-annotation-get name-spec "source_span") "start")
                    literal-has
                    (get literal-span "end_col")
                    (ctfe-node-has-annotation literal-spec "source_span")
                    (ctfe-node-is-literal set-result))))))))"#,
    )
    .unwrap();
    let unit = Unit::from_graph("detached-expr-spec-source-span", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
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
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse(
        "(bind name-spec (ctfe-ir-instantiate \"name\" (map-of \"identifier\" \"demo-call\"))
          (bind literal-spec (ctfe-ir-instantiate \"literal\" (map-of \"value\" 42))
            (bind call-spec
              (ctfe-ir-instantiate
                \"call\"
                (map-of \"callee\" name-spec \"args\" (list-of literal-spec)))
              (list-of
                (ctfe-node-kind call-spec)
                (ctfe-node-is-call call-spec)
                (ctfe-node-is-name name-spec)
                (ctfe-node-is-literal literal-spec)
                (ctfe-node-id call-spec)
                (ctfe-node-parent call-spec)
                (size (ctfe-node-children call-spec))
                (ctfe-node-name-identifier (ctfe-node-call-callee call-spec))
                (ctfe-node-literal-value (get (ctfe-node-call-args call-spec) 0 null))))))",
    )
    .unwrap();
    let unit = Unit::from_graph("detached-expr-spec-node-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
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
            RuntimeValue::Str("demo-call".into()),
            RuntimeValue::Int(42),
        ]
    );
}

#[test]
fn test_ctfe_provider_builtin_metadata_matches_python_effects() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-metadata-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(ctfe-provider-node-replace ctx target replacement)
(ctfe-provider-node-replace-many ctx target replacements)
(ctfe-provider-node-wrap ctx target callee)
(ctfe-provider-node-erase ctx target)
(ctfe-provider-fold-compile-time-call ctx target)
(ctfe-provider-materialize-ctfe-result ctx target entry result)
(ctfe-provider-diagnostics-warning ctx target message)
(ctfe-provider-fact-set ctx namespace target value)
(ctfe-provider-traversal-walk ctx target callback)
",
    )
    .unwrap();
    let source = format!(
        "(bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
          (list-of
            (get (ctfe-node-call-semantics (ctfe-unit-top-level-form-at unit 0)) \"effect_policy\")
            (get (ctfe-node-call-semantics (ctfe-unit-top-level-form-at unit 1)) \"effect_policy\")
            (get (ctfe-node-call-semantics (ctfe-unit-top-level-form-at unit 2)) \"effect_policy\")
            (get (ctfe-node-call-semantics (ctfe-unit-top-level-form-at unit 3)) \"effect_policy\")
            (get (ctfe-node-call-semantics (ctfe-unit-top-level-form-at unit 4)) \"effect_policy\")
            (get (ctfe-node-call-semantics (ctfe-unit-top-level-form-at unit 5)) \"effect_policy\")
            (get (ctfe-node-call-semantics (ctfe-unit-top-level-form-at unit 6)) \"effect_policy\")
            (get (ctfe-node-call-semantics (ctfe-unit-top-level-form-at unit 7)) \"effect_policy\")
            (get (ctfe-node-call-semantics (ctfe-unit-top-level-form-at unit 8)) \"eval_policy\")))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-builtin-effect-metadata", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    assert_eq!(
        items.borrow().as_slice(),
        [
            RuntimeValue::Str("write-ir".into()),
            RuntimeValue::Str("write-ir".into()),
            RuntimeValue::Str("write-ir".into()),
            RuntimeValue::Str("write-ir".into()),
            RuntimeValue::Str("write-ir".into()),
            RuntimeValue::Str("emit-diagnostics".into()),
            RuntimeValue::Str("emit-diagnostics".into()),
            RuntimeValue::Str("impure".into()),
            RuntimeValue::Str("special_form".into()),
        ]
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_node_replace_uses_typed_expr_specs() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-node-replace-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "old-value\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"replace-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (ctfe-provider-node-replace
                  ctx
                  root
                  (ctfe-ir-instantiate \"literal\" (map-of \"value\" \"new-value\")))))
            null
            (list-of \"write-ir\"))
          (bind source-unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind execution (ctfe-compiler-explain-provider-execution compiler \"compile_unit\" source-unit \"compile_time\")
              (bind executed (get (get execution \"executed\") 0)
                (bind result-value (get (get (get execution \"result\") \"value\") \"value\")
                  (list-of
                    (get executed \"changed\")
                    (get executed \"rewrite_count\")
                    (get executed \"erased_count\")
                    (get (get executed \"touched_node_kinds\") 0)
                    (get (get executed \"touched_node_kinds\") 1)
                    (get (get executed \"change_domains\") 0)
                    (get result-value \"unit_version\")
                    (get result-value \"provider_count\")))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-node-replace-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
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
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-missing-provider-ir-effect-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "old-value\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"missing-ir-effect-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (ctfe-provider-node-erase ctx root))))
          (ctfe-compiler-query-artifact
            compiler
            \"compile_unit\"
            (ctfe-compiler-load-surface-file-template compiler {:?})
            \"compile_time\"))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-ir-effect-enforcement", graph).unwrap();

    let error = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .expect_err("IR mutation without write-ir effect should fail");
    assert!(format!("{error}").contains("does not declare required effect write_ir"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_node_replace_many_returns_language_sequence_node() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-node-replace-many-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "old-value\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"replace-many-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (bind replaced
                  (ctfe-provider-node-replace-many
                    ctx
                    root
                    (list-of
                      (ctfe-ir-instantiate \"literal\" (map-of \"value\" \"first\"))
                      (ctfe-ir-instantiate \"literal\" (map-of \"value\" \"second\"))))
                  (if
                    (and
                      (ctfe-node-is-call replaced)
                      (eq (ctfe-node-name-identifier (ctfe-node-call-callee replaced)) \"do\")
                      (eq (size (ctfe-node-call-args replaced)) 2))
                    replaced
                    (ctfe-provider-diagnostics-error
                      ctx
                      root
                      \"replace-many did not return a sequence node\"
                      \"demo.replace_many\")))))
            null
            (list-of \"write-ir\" \"emit-diagnostics\"))
          (bind source-unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind execution
              (ctfe-compiler-explain-provider-execution
                compiler
                \"compile_unit\"
                source-unit
                \"compile_time\")
              (bind executed (get (get execution \"executed\") 0)
                (list-of
                  (get executed \"changed\")
                  (get executed \"erased_count\")
                  (get (get executed \"change_domains\") 0))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-node-replace-many-sequence", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    assert_eq!(
        items.borrow().as_slice(),
        [
            RuntimeValue::Bool(true),
            RuntimeValue::Int(1),
            RuntimeValue::Str("ir".into()),
        ]
    );
    assert!(compiler.diagnostics().is_empty());

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_explain_rewrite_projects_provider_provenance() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-explain-rewrite-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "old-value\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"replace-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (ctfe-provider-node-replace
                  ctx
                  root
                  (ctfe-ir-instantiate \"literal\" (map-of \"value\" \"new-value\")))))
            null
            (list-of \"write-ir\"))
          (bind source-unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind compiler-summary
              (ctfe-compiler-explain-rewrite
                compiler
                \"compile_unit\"
                source-unit
                1
                \"compile_time\")
              (list-of
                (get compiler-summary \"rewritten\")
                (get compiler-summary \"erased\")
                (get (get compiler-summary \"latest\") \"provider_name\")
                (get (get compiler-summary \"latest\") \"stage\")
                (get (get compiler-summary \"latest\") \"operation\")
                (size (get (get compiler-summary \"latest\") \"sources\"))
                (get (get compiler-summary \"query\") \"target\")
                (get (get compiler-summary \"query\") \"phase\")
                (get (get compiler-summary \"latest\") \"provider_name\")))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("explain-rewrite-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Bool(false));
    assert_eq!(items[2], RuntimeValue::Str("replace-provider".into()));
    assert_eq!(items[3], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[4], RuntimeValue::Str("replace".into()));
    assert_eq!(items[5], RuntimeValue::Int(1));
    assert_eq!(items[6], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[7], RuntimeValue::Str("compile_time".into()));
    assert_eq!(items[8], RuntimeValue::Str("replace-provider".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_query_builtins_project_stats_trace_and_plan() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    compiler
        .bootstrap()
        .execute_text("(ctfe-compiler-stage-register compiler \"parse\" null \"compile\" (list-of \"compile\"))", "trace-bootstrap")
        .unwrap();
    let graph = parse(
        "(list-of
          (get (ctfe-compiler-cache-stats compiler) \"generation\")
          (get (get (ctfe-compiler-bootstrap-trace compiler) 0) \"action\")
          (get (get (ctfe-compiler-query-plan compiler \"compile\") 0) \"stage\")
          (get (get (ctfe-compiler-query-plan compiler \"compile\" null \"compile_time\") 0) \"cached\"))",
    )
    .unwrap();
    let unit = Unit::from_graph("compiler-query-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(0));
    assert_eq!(items[1], RuntimeValue::Str("bootstrap.raw".into()));
    assert_eq!(items[2], RuntimeValue::Str("parse".into()));
    assert_eq!(items[3], RuntimeValue::Bool(false));
}

#[test]
fn test_ctfe_compiler_query_plan_rejects_phase_in_source_position() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    compiler
        .bootstrap()
        .execute_text(
            "(ctfe-compiler-stage-register compiler \"parse\" null \"compile\" (list-of \"compile\"))",
            "query-plan-source-position-bootstrap",
        )
        .unwrap();
    let graph = parse("(ctfe-compiler-query-plan compiler \"compile\" \"compile_time\")").unwrap();
    let unit = Unit::from_graph("query-plan-source-position", graph).unwrap();

    let err = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .expect_err("phase in source position should be rejected");

    assert!(err
        .to_string()
        .contains("phase must be the fourth argument"));
}

#[test]
fn test_ctfe_compiler_query_plan_projects_from_source_without_running_providers() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-query-plan-source-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "null\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"parse\" null \"compile\" null null (list-of \"surface\"))
          (ctfe-compiler-stage-register compiler \"compile_unit\" (list-of \"parse\"))
          (ctfe-compiler-provider-register
            compiler
            \"query-plan-source-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (ctfe-provider-diagnostics-note
                ctx
                (ctfe-unit-top-level-form-at unit 0)
                \"provider ran\"
                \"demo.query-plan.provider\")))
          (bind first-plan (ctfe-compiler-query-plan compiler \"compile_unit\" {:?} \"compile_time\")
            (bind artifact (ctfe-compiler-query-artifact compiler \"compile_unit\" {:?} \"compile_time\")
              (bind second-plan (ctfe-compiler-query-plan compiler \"compile_unit\" {:?} \"compile_time\")
                (list-of
                  (size first-plan)
                  (get (get first-plan 0) \"stage\")
                  (value-is-tuple (get (get first-plan 0) \"key\"))
                  (get (get first-plan 0) \"cached\")
                  (get artifact \"stage\")
                  (get (get second-plan 0) \"cached\")
                  (size (get artifact \"diagnostics\")))))))",
        path.display().to_string(),
        path.display().to_string(),
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("query-plan-source-projection", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
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
fn test_query_pipeline_accepts_inline_source_text_like_python_source_text() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    compiler
        .register_stage_spec(
            caap_core_port::QueryStageSpec::new("parse_surface")
                .unwrap()
                .with_input_kinds(vec!["surface".to_string()])
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_stage_spec(
            caap_core_port::QueryStageSpec::new("compile_unit")
                .unwrap()
                .with_requires(vec!["parse_surface".to_string()])
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_provider(
            "inline-source-provider",
            "compile_unit",
            caap_core_port::PhasePolicy::CompileTime,
            |_compiler, _unit| Ok(()),
        )
        .unwrap();
    let bridge = Rc::new(caap_core_port::CompilerBridgeValue::from_compiler(
        &compiler,
    ));

    let before = bridge
        .plan_query_with_source_options(
            "compile_unit",
            QueryArtifactSource::Text("42".to_string()),
            caap_core_port::PhasePolicy::CompileTime,
            QueryExecutionOptions::default(),
        )
        .unwrap();
    assert_eq!(
        before
            .steps
            .iter()
            .map(|step| step.stage.as_str())
            .collect::<Vec<_>>(),
        vec!["compile_unit"]
    );
    assert!(before.steps[0].artifact_key.is_some());
    assert!(!before.steps[0].cached);

    let artifact = bridge
        .query_artifact_with_options(
            "compile_unit",
            QueryArtifactSource::Text("42".to_string()),
            caap_core_port::PhasePolicy::CompileTime,
            QueryExecutionOptions::default(),
        )
        .unwrap();
    assert_eq!(artifact.stage, "compile_unit");
    assert_eq!(artifact.phase, caap_core_port::PhasePolicy::CompileTime);
    let caap_core_port::ArtifactValue::Semantic(SemanticValue::Map(entries)) = &artifact.value
    else {
        panic!("expected semantic query artifact value");
    };
    assert_eq!(
        entries.iter().find(|(key, _)| key == "provider_count"),
        Some(&("provider_count".to_string(), SemanticValue::Int(1)))
    );

    let after = bridge
        .plan_query_with_source_options(
            "compile_unit",
            QueryArtifactSource::Text("42".to_string()),
            caap_core_port::PhasePolicy::CompileTime,
            QueryExecutionOptions::default(),
        )
        .unwrap();
    assert!(after.steps[0].cached);
}

#[test]
fn test_ctfe_compiler_query_artifact_builtin_runs_query_pipeline() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-query-artifact-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "null\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"query-artifact-provider\"
            \"compile_unit\"
            (lambda (compiler unit) null)
            null
            null
            (map-of
              \"reads\" (list-of \"unit\")
              \"writes\" (list-of \"facts\")))
          (bind artifact
            (ctfe-compiler-query-artifact
              compiler
              \"compile_unit\"
              (ctfe-compiler-load-surface-file-template compiler {:?})
              \"compile_time\")
            (bind explained (ctfe-compiler-explain-artifact artifact)
              (list-of
                (get artifact \"artifact_kind\")
                (get artifact \"stage\")
                (get artifact \"phase\")
                (get (get artifact \"value\") \"kind\")
                (get (get (get artifact \"value\") \"value\") \"provider_count\")
                (get artifact \"iterations\")
                (get (get (get artifact \"execution_summary\") 0) \"provider_name\")
                (get (get artifact \"reads_subjects\") 0)
                (get (get artifact \"write_cells\") 0)
                (get explained \"iterations\")
                (get (get explained \"reads_subjects\") 0)
                (get (get explained \"write_cells\") 0)))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("query-artifact-builtin", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
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
        RuntimeValue::Str("query-artifact-provider".into())
    );
    assert_eq!(items[7], RuntimeValue::Str("unit".into()));
    assert_eq!(items[8], RuntimeValue::Str("facts".into()));
    assert_eq!(items[9], RuntimeValue::Int(1));
    assert_eq!(items[10], RuntimeValue::Str("unit".into()));
    assert_eq!(items[11], RuntimeValue::Str("facts".into()));
    assert_eq!(compiler.artifact_cache().stats().generation, 1);

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_query_artifact_accepts_initial_bindings() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-query-artifact-initial-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "placeholder\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"query-artifact-initial-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (ctfe-unit-add-public-name! unit initial-public)))
          (bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind artifact
              (ctfe-compiler-query-artifact
                compiler
                \"compile_unit\"
                unit
                \"compile_time\"
                (map-of \"initial-public\" \"public-from-initial\"))
              (bind explain
                (ctfe-compiler-explain-query
                  compiler
                  \"compile_unit\"
                  unit
                  \"compile_time\"
                  (map-of \"initial-public\" \"public-from-initial\"))
                (list-of
                  (get (get artifact \"key\") 11)
                  (get (get artifact \"key\") 12)
                  (get (get artifact \"key\") 13)
                  (get (get (get artifact \"value\") \"value\") \"provider_count\")
                  (get (get (get explain \"steps\") 0) \"cached\"))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("query-artifact-initial-bindings", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("initial-binding".into()));
    assert_eq!(items[1], RuntimeValue::Str("initial-public".into()));
    assert_eq!(
        items[2],
        RuntimeValue::Str("str:public-from-initial".into())
    );
    assert_eq!(items[3], RuntimeValue::Int(1));
    assert_eq!(items[4], RuntimeValue::Bool(true));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_explain_query_and_provider_schedule_builtins() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-explain-query-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "null\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"explain-provider\"
            \"compile_unit\"
            (lambda (compiler unit) null)
            null
            (list-of \"explain-effect\")
            (map-of
              \"reads\" (list-of \"unit\")
              \"writes\" (list-of \"diagnostics\")))
          (bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind execution (ctfe-compiler-explain-provider-execution compiler \"compile_unit\" unit \"compile_time\")
              (bind query (ctfe-compiler-explain-query compiler \"compile_unit\" unit \"compile_time\")
                (bind artifact (ctfe-compiler-explain-artifact (get query \"result\"))
                  (bind invalidation (ctfe-compiler-explain-invalidation compiler \"compile_unit\" unit \"compile_time\")
                    (bind schedule (ctfe-compiler-explain-provider-schedule compiler \"compile_unit\" unit \"compile_time\")
                      (bind family (get (get schedule \"families\") 0)
                        (bind group (get (get family \"groups\") 0)
                          (bind provider (get (get group \"providers\") 0)
                            (bind executed (get (get execution \"executed\") 0)
                            (list-of
                              (get query \"target\")
                              (get (get (get query \"steps\") 0) \"stage\")
                              (get (get (get query \"steps\") 0) \"cached_artifact\")
                              (get (get query \"result\") \"stage\")
                              (get artifact \"artifact_kind\")
                              (get artifact \"stage\")
                              (get artifact \"dependency_count\")
                              (get (get (get invalidation \"steps\") 0) \"cached\")
                              (get (get (get (get invalidation \"steps\") 0) \"key\") 0)
                              (get (get (get invalidation \"steps\") 0) \"invalidation\")
                              (get family \"stage\")
                              (get provider \"name\")
                              (get (get (get provider \"effects\") \"emits\") 0)
                              (get executed \"provider_name\")
                              (get executed \"outcome_kind\")
                              (get (get (get (get executed \"provider_contract\") \"effects\") \"emits\") 0)
                              (get artifact \"iterations\")
                              (get (get artifact \"reads_subjects\") 0)
                              (get (get artifact \"write_cells\") 0)))))))))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("explain-query-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
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
    assert_eq!(items[8], RuntimeValue::Str("query-stage".into()));
    assert_eq!(items[9], RuntimeValue::Null);
    assert_eq!(items[10], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[11], RuntimeValue::Str("explain-provider".into()));
    assert_eq!(items[12], RuntimeValue::Str("explain_effect".into()));
    assert_eq!(items[13], RuntimeValue::Str("explain-provider".into()));
    assert_eq!(items[14], RuntimeValue::Str("ok".into()));
    assert_eq!(items[15], RuntimeValue::Str("explain_effect".into()));
    assert_eq!(items[16], RuntimeValue::Int(1));
    assert_eq!(items[17], RuntimeValue::Str("unit".into()));
    assert_eq!(items[18], RuntimeValue::Str("diagnostics".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_explain_provider_schedule_honors_requires() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-schedule-requires-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "null\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"consumer\"
            \"compile_unit\"
            (lambda (compiler unit) null)
            (list-of \"producer\"))
          (ctfe-compiler-provider-register
            compiler
            \"producer\"
            \"compile_unit\"
            (lambda (compiler unit) null))
          (bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind schedule (ctfe-compiler-explain-provider-schedule compiler \"compile_unit\" unit \"compile_time\")
              (bind groups (get (get (get schedule \"families\") 0) \"groups\")
                (list-of
                  (size groups)
                  (get (get (get (get groups 0) \"providers\") 0) \"name\")
                  (get (get (get (get groups 1) \"providers\") 0) \"name\"))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-schedule-requires", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
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
fn test_ctfe_provider_callback_return_value_marks_changed() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-reported-change-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "null\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"reported-change-provider\"
            \"compile_unit\"
            (lambda (ctx root) true))
          (bind source-unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind execution
              (ctfe-compiler-explain-provider-execution
                compiler
                \"compile_unit\"
                source-unit
                \"compile_time\")
              (bind executed (get (get execution \"executed\") 0)
                (list-of
                  (get executed \"provider_name\")
                  (get executed \"changed\")
                  (get executed \"diagnostics_emitted\"))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-reported-change", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(
        items[0],
        RuntimeValue::Str("reported-change-provider".into())
    );
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Int(0));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_explain_provider_schedule_projects_effect_barriers() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-schedule-barrier-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "null\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"writer\"
            \"compile_unit\"
            (lambda (compiler unit) null)
            null
            null
            (map-of \"writes\" (list-of \"facts\")))
          (ctfe-compiler-provider-register
            compiler
            \"reader\"
            \"compile_unit\"
            (lambda (compiler unit) null)
            null
            null
            (map-of \"reads\" (list-of \"facts\")))
          (bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind schedule (ctfe-compiler-explain-provider-schedule compiler \"compile_unit\" unit \"compile_time\")
              (bind groups (get (get (get schedule \"families\") 0) \"groups\")
                (bind barrier (get (get groups 0) \"barrier_after\")
                  (bind provider (get (get (get groups 0) \"providers\") 0)
                    (list-of
                      (size groups)
                      (get provider \"name\")
                      (get (get (get provider \"effects\") \"writes\") 0)
                      (get barrier \"next_group_index\")
                      (get (get barrier \"reasons\") 0)
                      (get (get (get (get groups 1) \"providers\") 0) \"name\"))))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-schedule-barrier", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
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
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-schedule-data-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "null\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"consumer\"
            \"compile_unit\"
            (lambda (compiler unit) null)
            null
            null
            (map-of \"requires_data\" (list-of \"types.root\")))
          (ctfe-compiler-provider-register
            compiler
            \"producer\"
            \"compile_unit\"
            (lambda (compiler unit) null)
            null
            null
            (map-of \"provides_data\" (list-of \"types.root\")))
          (bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind schedule (ctfe-compiler-explain-provider-schedule compiler \"compile_unit\" unit \"compile_time\")
              (bind groups (get (get (get schedule \"families\") 0) \"groups\")
                (list-of
                  (size groups)
                  (get (get (get (get groups 0) \"providers\") 0) \"name\")
                  (get (get (get (get groups 1) \"providers\") 0) \"name\")
                  (get (get (get (get (get groups 1) \"providers\") 0) \"requires_data\") 0)
                  (get (get (get (get (get groups 0) \"providers\") 0) \"provides_data\") 0))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-schedule-data", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(2));
    assert_eq!(items[1], RuntimeValue::Str("producer".into()));
    assert_eq!(items[2], RuntimeValue::Str("consumer".into()));
    assert_eq!(items[3], RuntimeValue::Str("types.root".into()));
    assert_eq!(items[4], RuntimeValue::Str("types.root".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_unit_query_mutation_and_explain_name_builtins() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-unit-builtins-{}.caap",
        std::process::id()
    ));
    let file_text = "public-value\n";
    std::fs::write(&path, file_text).unwrap();
    let parsed = parse(file_text).unwrap();
    let name_node_id = parsed.top_level_form_ids()[0];
    let source = format!(
        "(do
          (bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (ctfe-unit-set-id! unit \"renamed-unit\")
            (ctfe-unit-add-link-binding!
              unit
              (map-of
                \"source_unit\" \"dep\"
                \"source_name\" \"exported\"
                \"local_name\" \"local\"
                \"syntax\" true))
            (ctfe-unit-add-public-name! unit \"public-value\")
            (ctfe-unit-syntax-rule-set! unit \"demo-rule\" (map-of \"kind\" \"literal\"))
            (ctfe-unit-syntax-metadata-set! unit \"precedence\" 7)
            (ctfe-unit-syntax-hook-set! unit \"demo-hook\" \"run-demo-hook\")
            (ctfe-unit-syntax-authoring-source-apply!
              unit
              \"add rule authored = symbol -> surface.symbol\")
            (ctfe-unit-syntax-rule-define!
              unit
              \"add rule named_rule = symbol\"
              \"lower-named-rule\")
            (bind explain (ctfe-unit-explain-name unit {})
              (bind facts (ctfe-unit-query unit (list-of (list-of \"has\" \"symbol:public-value\" \"symbol.entry\")))
                (list-of
                  (get explain \"identifier\")
                  (get explain \"resolved\")
                  (get explain \"binding_kind\")
                  (get explain \"unit_id\")
                  (get explain \"node_id\")
                  (get (get (get facts 0) \"subject\") \"value\")
                  (get (get facts 0) \"predicate\")
                  (ctfe-unit-origin-stage (map-of \"stage\" \"compile_unit\"))
                  (ctfe-unit-syntax-metadata-get unit \"precedence\")
                  (get (ctfe-unit-syntax-metadata-get unit \"semantic_hook_functions\") \"demo-hook\")
                  (get (get (get (ctfe-unit-syntax-metadata-get unit \"authored\") \"semantic_hooks\") 0) 0)
                  (get (get (get (ctfe-unit-syntax-metadata-get unit \"named_rule\") \"semantic_hooks\") 0) 1)
                  (get (ctfe-unit-syntax-metadata-get unit \"semantic_hook_functions\") \"lower-named-rule\"))))))",
        path.display().to_string(),
        name_node_id,
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("unit-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("public-value".into()));
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Str("top_level".into()));
    assert_eq!(items[3], RuntimeValue::Str("renamed-unit".into()));
    assert_eq!(items[4], RuntimeValue::Null);
    assert_eq!(items[5], RuntimeValue::Str("public-value".into()));
    assert_eq!(items[6], RuntimeValue::Str("symbol.entry".into()));
    assert_eq!(items[7], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[8], RuntimeValue::Int(7));
    assert_eq!(items[9], RuntimeValue::Str("run-demo-hook".into()));
    assert_eq!(items[10], RuntimeValue::Str("surface.symbol".into()));
    assert_eq!(items[11], RuntimeValue::Str("lower-named-rule".into()));
    assert_eq!(items[12], RuntimeValue::Str("lower-named-rule".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_unit_syntax_rule_define_inline_node_reads_file_span_source() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-inline-syntax-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(&path, "(lambda (form) form)\n").unwrap();
    let source = format!(
        "(bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
          (bind implementation (ctfe-unit-top-level-form-at unit 0)
            (do
              (ctfe-unit-syntax-rule-define-inline-node!
                unit
                \"add rule inline_rule = symbol\"
                implementation)
              (bind metadata (ctfe-unit-syntax-metadata-get unit \"inline_rule\")
                (bind hook-ref (get (get (get metadata \"semantic_hooks\") 0) 0)
                  (list-of
                    hook-ref
                    (get
                      (ctfe-unit-syntax-metadata-get unit \"semantic_hook_inline_sources\")
                      hook-ref)))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("inline-syntax-rule", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
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
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-unit-graph-ir-builtins-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "root-name\n").unwrap();
    let source = format!(
        "(bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
          (ctfe-unit-set-id! unit \"graph-unit\")
          (bind root (ctfe-unit-top-level-form-at unit 0)
            (ctfe-unit-declare-symbol! unit \"root-name\" \"compile_time\" root \"top_level\")
            (ctfe-unit-set-symbol-semantics! unit \"root-name\" (map-of \"phase\" \"runtime\" \"pure\" true) root)
            (ctfe-unit-add-public-name! unit \"root-name\")
            (ctfe-unit-add-link-binding!
              unit
              (ctfe-unit-link-binding-new \"dep.unit\" \"dep-name\" \"local-name\" true))
            (bind symbols (ctfe-unit-symbols unit)
              (bind links (ctfe-unit-link-bindings unit)
	                (list-of
	                  (ctfe-unit-id unit)
	                  (eq (ctfe-node-id root) (ctfe-node-id (ctfe-unit-top-level-form-at unit 0)))
	                  (size (ctfe-unit-top-level-forms unit))
	                  (get (ctfe-unit-node-location unit root) 0)
	                  (get (ctfe-unit-node-location unit root) 1)
	                  (host-value-kind root)
	                  (ctfe-node-kind root)
	                  (ctfe-node-is-name root)
	                  (ctfe-node-name-identifier root)
	                  (ctfe-node-live? root)
	                  (ctfe-meta-fact-set-by-key root \"demo.fact\" \"ok\")
	                  (ctfe-meta-fact-get-by-key root \"demo.fact\")
	                  (ctfe-meta-fact-has-by-key root \"demo.fact\")
	                  (ctfe-meta-annotation-set root \"demo\" \"ann\")
	                  (ctfe-meta-annotation-get root \"demo\")
	                  (ctfe-node-has-annotation root \"demo\")
	                  (host-value-kind (ctfe-meta-annotation-set-many root \"second\" 2 \"third\" \"v\"))
	                  (ctfe-meta-annotation-get root \"second\")
	                  (ctfe-meta-annotation-get root \"missing\" \"fallback\")
	                  (get (get symbols 0) \"name\")
	                  (get (get symbols 0) \"phase\")
	                  (get (get symbols 0) \"public\")
	                  (get (get links 0) \"source_unit\")
	                  (get (get links 0) \"syntax\")
	                  (size (ctfe-unit-public-names unit))
	                  (size (ctfe-unit-top-level-symbol-names unit))
	                  (size (ctfe-unit-facts unit))
	                  (ctfe-unit-version unit)
	                  (host-value-kind (ctfe-unit-to-template unit))
	                  (ctfe-unit-id (ctfe-unit-template-instantiate (ctfe-unit-to-template unit))))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("unit-graph-ir-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("graph-unit".into()));
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Int(1));
    assert_eq!(items[3], RuntimeValue::Str("graph-unit".into()));
    assert!(matches!(items[4], RuntimeValue::Int(_)));
    assert_eq!(items[5], RuntimeValue::Str("node".into()));
    assert_eq!(items[6], RuntimeValue::Str("Name".into()));
    assert_eq!(items[7], RuntimeValue::Bool(true));
    assert_eq!(items[8], RuntimeValue::Str("root-name".into()));
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
    assert_eq!(items[19], RuntimeValue::Str("root-name".into()));
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
    assert_eq!(items[28], RuntimeValue::Str("unit-template".into()));
    assert_eq!(items[29], RuntimeValue::Str("graph-unit".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_node_graph_ir_builtins_project_call_tree() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-node-graph-ir-builtins-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "(int-add 1 2)\n").unwrap();
    let source = format!(
        "(bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
          (bind root (ctfe-unit-top-level-form-at unit 0)
            (bind callee (ctfe-node-call-callee root)
              (bind args (ctfe-node-call-args root)
                (bind first-arg (get args 0)
                  (list-of
                    (ctfe-node-kind root)
                    (ctfe-node-is-call root)
                    (ctfe-node-name-identifier callee)
                    (size (ctfe-node-children root))
                    (size args)
                    (ctfe-node-literal-value first-arg)
                    (ctfe-node-ancestor? first-arg root)
                    (eq (ctfe-node-id root) (ctfe-node-id (ctfe-node-parent first-arg)))
                    (ctfe-node-call-has-semantics root)
                    (get (ctfe-node-call-semantics root) \"builtin_name\")
                    (get (ctfe-node-call-semantics root) \"eval_policy\")
                    (get (ctfe-node-call-semantics root) \"effect_policy\")
                    (get (ctfe-node-call-semantics root) \"short_circuit_policy\")
                    (ctfe-node-call-builtin-name root)
                    (ctfe-node-call-min-arity root)
                    (ctfe-node-call-max-arity root)
                    (ctfe-node-call-eval-policy root)
                    (ctfe-node-call-effect-policy root)
                    (ctfe-node-call-short-circuit-policy root)
                    (get (ctfe-node-call-callee-policy root) \"phase_policy\")
                    (get (ctfe-node-call-callee-policy root) \"control_policy\")
                    (host-value-kind (ctfe-node-to-spec root))
                    (host-value-kind
                      (ctfe-meta-fact-set-by-key
                        first-arg
                        \"caap.fact.resolved_block\"
                        (ctfe-resolved-block-new root)))
                    (eq
                      (ctfe-node-id (ctfe-node-resolved-block first-arg null))
                      (ctfe-node-id root))))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("node-graph-ir-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("Call".into()));
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Str("int-add".into()));
    assert_eq!(items[3], RuntimeValue::Int(3));
    assert_eq!(items[4], RuntimeValue::Int(2));
    assert_eq!(items[5], RuntimeValue::Int(1));
    assert_eq!(items[6], RuntimeValue::Bool(true));
    assert_eq!(items[7], RuntimeValue::Bool(true));
    assert_eq!(items[8], RuntimeValue::Bool(true));
    assert_eq!(items[9], RuntimeValue::Str("int-add".into()));
    assert_eq!(items[10], RuntimeValue::Str("eager".into()));
    assert_eq!(items[11], RuntimeValue::Str("pure".into()));
    assert_eq!(items[12], RuntimeValue::Str("none".into()));
    assert_eq!(items[13], RuntimeValue::Str("int-add".into()));
    assert_eq!(items[14], RuntimeValue::Int(2));
    assert_eq!(items[15], RuntimeValue::Int(2));
    assert_eq!(items[16], RuntimeValue::Str("eager".into()));
    assert_eq!(items[17], RuntimeValue::Str("pure".into()));
    assert_eq!(items[18], RuntimeValue::Str("none".into()));
    assert_eq!(items[19], RuntimeValue::Str("runtime".into()));
    assert_eq!(items[20], RuntimeValue::Str("plain".into()));
    assert_eq!(items[21], RuntimeValue::Str("expr_spec".into()));
    assert_eq!(items[22], RuntimeValue::Str("map".into()));
    assert_eq!(items[23], RuntimeValue::Bool(true));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_node_call_semantics_projects_stored_fact_for_non_builtin_call() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-node-call-semantics-fact-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "(local-call 1 2)\n").unwrap();
    let source = format!(
        "(bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
          (bind root (ctfe-unit-top-level-form-at unit 0)
            (bind semantics
              (assoc-many
                (map-of)
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
                (ctfe-meta-fact-set-by-key
                  root
                  \"caap.fact.call_semantics\"
                  semantics)
                (list-of
                  (ctfe-node-call-has-semantics root)
                  (get (ctfe-node-call-semantics root) \"callee_class\")
                  (ctfe-node-call-builtin-name root \"not-builtin\")
                  (ctfe-node-call-min-arity root \"no-min\")
                  (ctfe-node-call-max-arity root \"no-max\")
                  (ctfe-node-call-eval-policy root \"missing\")
                  (ctfe-node-call-effect-policy root \"missing\")
                  (ctfe-node-call-short-circuit-policy root \"missing\")
                  (get (ctfe-node-call-callee-policy root) \"scope_policy\"))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("node-call-semantics-fact", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Str("function".into()));
    assert_eq!(items[2], RuntimeValue::Str("not-builtin".into()));
    assert_eq!(items[3], RuntimeValue::Str("no-min".into()));
    assert_eq!(items[4], RuntimeValue::Str("no-max".into()));
    assert_eq!(items[5], RuntimeValue::Str("eager".into()));
    assert_eq!(items[6], RuntimeValue::Str("pure".into()));
    assert_eq!(items[7], RuntimeValue::Str("none".into()));
    assert_eq!(items[8], RuntimeValue::Str("lexical".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_node_call_scope_descriptors() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-node-scope-descriptors-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(bind x 1 (int-add x 2))\n(lambda (a b) (int-add a b))\n",
    )
    .unwrap();
    let source = format!(
        "(bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
          (bind bind-node (ctfe-unit-top-level-form-at unit 0)
            (bind scope (ctfe-node-call-scope-descriptor bind-node)
              (bind lambda-node (ctfe-unit-top-level-form-at unit 1)
                (bind lambda-scope (ctfe-node-call-scope-descriptor lambda-node)
                  (list-of
                    (get (get (get scope \"bindings\") 0) \"name\")
                    (size (get scope \"binding_value_ids\"))
                    (size (get scope \"body_ids\"))
                    (get scope \"result_kind\")
                    (get (get (get lambda-scope \"bindings\") 0) \"name\")
                    (get (get (get lambda-scope \"bindings\") 1) \"name\")
                    (get lambda-scope \"result_kind\")
                    (size (get lambda-scope \"body_ids\"))))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("node-scope-descriptors", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("x".into()));
    assert_eq!(items[1], RuntimeValue::Int(1));
    assert_eq!(items[2], RuntimeValue::Int(1));
    assert_eq!(items[3], RuntimeValue::Str("scoped_eval".into()));
    assert_eq!(items[4], RuntimeValue::Str("a".into()));
    assert_eq!(items[5], RuntimeValue::Str("b".into()));
    assert_eq!(items[6], RuntimeValue::Str("closure".into()));
    assert_eq!(items[7], RuntimeValue::Int(1));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_node_call_control_descriptors() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-node-control-descriptors-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "(block exit (leave exit 9))\n").unwrap();
    let source = format!(
        "(bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
          (bind block-node (ctfe-unit-top-level-form-at unit 0)
            (bind block-desc (ctfe-node-call-control-descriptor block-node)
              (bind leave-node (get (get block-desc \"values\") 0)
                (bind leave-desc (ctfe-node-call-control-descriptor leave-node)
                  (list-of
                    (get block-desc \"kind\")
                    (size (get block-desc \"value_ids\"))
                    (get leave-desc \"kind\")
                    (eq (get leave-desc \"label\") (ctfe-node-id block-node))
                    (size (get leave-desc \"value_ids\"))))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("node-control-descriptors", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("block".into()));
    assert_eq!(items[1], RuntimeValue::Int(1));
    assert_eq!(items[2], RuntimeValue::Str("leave".into()));
    assert_eq!(items[3], RuntimeValue::Bool(true));
    assert_eq!(items[4], RuntimeValue::Int(1));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_explain_name_runs_query_pipeline() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-compiler-explain-name-{}.caap",
        std::process::id()
    ));
    let file_text = "public-value\n";
    std::fs::write(&path, file_text).unwrap();
    let parsed = parse(file_text).unwrap();
    let name_node_id = parsed.top_level_form_ids()[0];
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"name-provider\"
            \"compile_unit\"
            (lambda (ctx root)
              (ctfe-unit-add-public-name! (ctfe-provider-unit ctx) \"public-value\"))
            null
            (list-of \"name-effect\"))
          (bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind summary (ctfe-compiler-explain-name compiler \"compile_unit\" unit {} \"compile_time\")
              (list-of
                (get summary \"identifier\")
                (get summary \"resolved\")
                (get summary \"binding_kind\")
                (get (get summary \"query\") \"target\")
                (get (get summary \"query\") \"phase\")))))",
        path.display().to_string(),
        name_node_id,
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("compiler-explain-name", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("public-value".into()));
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Str("top_level".into()));
    assert_eq!(items[3], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[4], RuntimeValue::Str("compile_time".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_context_builtins_run_inside_query_callback() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-context-{}.caap",
        std::process::id()
    ));
    let file_text = "provided-name\n";
    std::fs::write(&path, file_text).unwrap();
    let parsed = parse(file_text).unwrap();
    let name_node_id = parsed.top_level_form_ids()[0];
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"context-provider\"
            \"compile_unit\"
            (lambda (ctx root)
              (ctfe-provider-require-effect ctx \"ctx-effect\")
              (ctfe-provider-fact-set ctx \"demo.fact\" {} \"provided-name\")
              (ctfe-unit-add-public-name!
                (ctfe-provider-unit ctx)
                (ctfe-provider-fact-get ctx \"demo.fact\" {}))
              (ctfe-compiler-error ctx {} \"provider diagnostic\" \"demo.error\"))
            null
            (list-of \"ctx-effect\" \"read-facts\" \"write-facts\"))
          (bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind summary (ctfe-compiler-explain-name compiler \"compile_unit\" unit {} \"compile_time\")
              (list-of
                (get summary \"resolved\")
                (get summary \"binding_kind\")
                (get summary \"identifier\")))))",
        name_node_id,
        name_node_id,
        name_node_id,
        path.display().to_string(),
        name_node_id,
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-context-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Str("top_level".into()));
    assert_eq!(items[2], RuntimeValue::Str("provided-name".into()));
    assert_eq!(compiler.diagnostics().len(), 1);
    assert_eq!(
        compiler.diagnostics()[0].code.as_deref(),
        Some("demo.error")
    );
    assert_eq!(compiler.diagnostics()[0].message, "provider diagnostic");

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_fact_builtins_enforce_effect_contracts() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-missing-provider-fact-effect-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "fact-source\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"missing-fact-effect-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (ctfe-provider-fact-set ctx \"demo.fact\" root 1))))
          (ctfe-compiler-query-artifact
            compiler
            \"compile_unit\"
            (ctfe-compiler-load-surface-file-template compiler {:?})
            \"compile_time\"))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-fact-effect-enforcement", graph).unwrap();

    let error = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .expect_err("fact write without write-facts effect should fail");
    assert!(format!("{error}").contains("does not declare required effect write_facts"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_execution_records_runtime_fact_dependencies() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-runtime-dependencies-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "fact-source\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"tracked-writer\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (ctfe-provider-fact-set ctx \"demo.fact\" root 7)))
            null
            (list-of \"write-facts\"))
          (ctfe-compiler-provider-register
            compiler
            \"tracked-reader\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (ctfe-provider-fact-get ctx \"demo.fact\" root null)))
            (list-of \"tracked-writer\")
            (list-of \"read-facts\"))
          (bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (bind expected-cell
                (string-concat-many \"node:\" (int-to-string (ctfe-node-id root)) \"@demo.fact\")
                (bind artifact
                  (ctfe-compiler-query-artifact compiler \"compile_unit\" unit \"compile_time\")
                  (bind writer (get (get artifact \"execution_summary\") 0)
                    (bind reader (get (get artifact \"execution_summary\") 1)
                      (list-of
                        expected-cell
                        (get (get writer \"write_cells\") 0)
                        (get (get reader \"read_cells\") 0)
                        (get (get artifact \"read_cells\") 0)
                        (get (get artifact \"write_cells\") 0)))))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-runtime-dependency-tracking", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
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
fn test_ctfe_provider_query_artifact_runs_nested_query() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-nested-query-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "nested-source\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"resolve_names\")
          (ctfe-compiler-provider-register
            compiler
            \"nested-query-inner\"
            \"resolve_names\"
            (lambda (compiler unit ctx) null))
          (ctfe-compiler-stage-register compiler \"compile_unit\" (list-of \"resolve_names\"))
          (ctfe-compiler-provider-register
            compiler
            \"nested-query-outer\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (ctfe-provider-require-effect ctx \"read-registry\")
              (bind artifact
                (ctfe-provider-query-artifact ctx \"resolve_names\" unit \"compile_time\")
                (do
                  (ctfe-compiler-emit-event
                    compiler
                    \"provider\"
                    \"nested-artifact\"
                    (get artifact \"stage\")
                    (map-of \"artifact_kind\" (get artifact \"artifact_kind\")))
                  (ctfe-provider-diagnostics-note
                    ctx
                    (ctfe-unit-top-level-form-at unit 0)
                    (get artifact \"stage\")
                    \"demo.nested.query\"))))
            null
            (list-of \"read-registry\" \"emit-events\"))
          (bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind artifact (ctfe-compiler-query-artifact compiler \"compile_unit\" unit \"compile_time\")
              (bind schedule (ctfe-compiler-explain-provider-schedule compiler \"compile_unit\" unit \"compile_time\")
                (bind family (get (get schedule \"families\") 1)
                  (bind provider (get (get (get (get family \"groups\") 0) \"providers\") 0)
                    (list-of
                      (get artifact \"stage\")
                      (get (get (get artifact \"value\") \"value\") \"provider_count\")
                      (get (get (get artifact \"diagnostics\") 0) \"message\")
                      (get (get (get artifact \"diagnostics\") 0) \"code\")
                      (get family \"stage\")
                      (get provider \"name\")
                      (get (get provider \"dynamic_requires\") 0)
                      (size (get artifact \"dependencies\"))
                      (size (get (get (get artifact \"execution_summary\") 1) \"artifact_dependencies\")))))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-nested-query-artifact", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[1], RuntimeValue::Int(1));
    assert_eq!(items[2], RuntimeValue::Str("resolve_names".into()));
    assert_eq!(items[3], RuntimeValue::Str("demo.nested.query".into()));
    assert_eq!(items[4], RuntimeValue::Str("compile_unit".into()));
    assert_eq!(items[5], RuntimeValue::Str("nested-query-outer".into()));
    assert_eq!(items[6], RuntimeValue::Str("nested-query-inner".into()));
    assert_eq!(items[7], RuntimeValue::Int(1));
    assert_eq!(items[8], RuntimeValue::Int(1));
    let event = &compiler
        .events()
        .by_kind("provider.nested-artifact")
        .unwrap()[0];
    assert_eq!(event.message, "resolve_names");
    assert_eq!(
        event.metadata,
        vec![("artifact_kind".to_string(), "query".to_string())]
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_registry_and_catalog_reads() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let dep_path = std::env::temp_dir().join(format!(
        "caap-rust-provider-catalog-dep-{}.caap",
        std::process::id()
    ));
    let source_path = std::env::temp_dir().join(format!(
        "caap-rust-provider-catalog-source-{}.caap",
        std::process::id()
    ));
    std::fs::write(&dep_path, "compiled-dependency\n").unwrap();
    std::fs::write(&source_path, "catalog-source\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-register-value compiler \"provider.answer\" \"registry-answer\")
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (bind dep-unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind dep-id (ctfe-unit-id dep-unit)
              (do
                (ctfe-compiler-compile-unit compiler dep-unit)
                (ctfe-compiler-provider-register
                  compiler
                  \"provider-registry-reader\"
                  \"compile_unit\"
                  (lambda (compiler unit ctx)
                    (ctfe-provider-require-effect ctx \"read-registry\")
                    (bind registered (ctfe-provider-lookup-value ctx \"provider.answer\")
                      (bind compiled (ctfe-provider-compiled-unit ctx dep-id)
                        (bind missing-unit (ctfe-provider-compiled-unit ctx \"missing.unit\" \"missing-default\")
                          (ctfe-provider-diagnostics-note
                            ctx
                            (ctfe-unit-top-level-form-at unit 0)
                            (string-concat-many registered \":\" (ctfe-unit-id compiled) \":\" missing-unit)
                            (ctfe-provider-lookup-value ctx \"missing.code\" \"demo.provider.registry\"))))))
                  null
                  (list-of \"read-registry\"))
                (bind source-unit (ctfe-compiler-load-surface-file-template compiler {:?})
                  (bind artifact (ctfe-compiler-query-artifact compiler \"compile_unit\" source-unit \"compile_time\")
                    (list-of
                      dep-id
                      (eq
                        (get (get (get artifact \"diagnostics\") 0) \"message\")
                        (string-concat-many \"registry-answer:\" dep-id \":missing-default\"))
                      (get (get (get artifact \"diagnostics\") 0) \"code\"))))))))",
        dep_path.display().to_string(),
        source_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-registry-catalog-reads", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert!(matches!(&items[0], RuntimeValue::Str(text) if !text.is_empty()));
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Str("demo.provider.registry".into()));

    let _ = std::fs::remove_file(dep_path);
    let _ = std::fs::remove_file(source_path);
}

#[test]
fn test_ctfe_provider_file_reads_require_effect() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let dir = std::env::temp_dir().join(format!("caap-rust-provider-files-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let payload_path = dir.join("payload.txt");
    let source_path = dir.join("source.caap");
    std::fs::write(&payload_path, "file payload").unwrap();
    std::fs::write(&source_path, "file-source\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"provider-file-reader\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (ctfe-provider-require-effect ctx \"read-files\")
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (do
                  (ctfe-provider-diagnostics-note
                    ctx
                    root
                    (ctfe-provider-resolve-path ctx \"payload.txt\" {:?})
                    \"demo.provider.path\")
                  (ctfe-provider-diagnostics-note
                    ctx
                    root
                    (ctfe-provider-read-text ctx \"payload.txt\" {:?})
                    \"demo.provider.text\"))))
            null
            (list-of \"read-files\"))
          (bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind artifact (ctfe-compiler-query-artifact compiler \"compile_unit\" unit \"compile_time\")
              (list-of
                (get (get (get artifact \"diagnostics\") 0) \"message\")
                (get (get (get artifact \"diagnostics\") 0) \"code\")
                (get (get (get artifact \"diagnostics\") 1) \"message\")
                (get (get (get artifact \"diagnostics\") 1) \"code\")))))",
        dir.display().to_string(),
        dir.display().to_string(),
        source_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-file-reads", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert!(matches!(&items[0], RuntimeValue::Str(text) if text.ends_with("payload.txt")));
    assert_eq!(items[1], RuntimeValue::Str("demo.provider.path".into()));
    assert_eq!(items[2], RuntimeValue::Str("file payload".into()));
    assert_eq!(items[3], RuntimeValue::Str("demo.provider.text".into()));

    let _ = std::fs::remove_file(payload_path);
    let _ = std::fs::remove_file(source_path);
    let _ = std::fs::remove_dir(dir);
}

#[test]
fn test_ctfe_provider_annotations_require_attribute_effects() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-annotations-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "annotated-source\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"provider-annotations\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (ctfe-provider-require-effect ctx \"read-attributes\")
              (ctfe-provider-require-effect ctx \"write-attributes\")
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (do
                  (ctfe-provider-annotation-set ctx root \"demo\" \"annotation-value\")
                  (ctfe-provider-diagnostics-note
                    ctx
                    root
                    (string-concat-many
                      (ctfe-provider-annotation-get ctx root \"demo\")
                      \":\"
                      (ctfe-provider-annotation-get ctx root \"missing\" \"default-value\"))
                    \"demo.provider.annotation\"))))
            null
            (list-of \"read-attributes\" \"write-attributes\"))
          (bind unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind artifact (ctfe-compiler-query-artifact compiler \"compile_unit\" unit \"compile_time\")
              (list-of
                (get (get (get artifact \"diagnostics\") 0) \"message\")
                (get (get (get artifact \"diagnostics\") 0) \"code\")))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-annotations", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(
        items[0],
        RuntimeValue::Str("annotation-value:default-value".into())
    );
    assert_eq!(
        items[1],
        RuntimeValue::Str("demo.provider.annotation".into())
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_traversal_walk_and_callback_invocation() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-traversal-{}.caap",
        std::process::id()
    ));
    let file_text = "(int-add 1 2)\n";
    std::fs::write(&path, file_text).unwrap();
    let parsed = parse(file_text).unwrap();
    let root_node_id = parsed.top_level_form_ids()[0];
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"traversal-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (bind names
                  (ctfe-provider-traversal-walk
                    ctx
                    root
                    (lambda (node) true)
                    (map-of \"mode\" \"filter\" \"kind\" \"name\"))
                  (bind sum
                    (ctfe-provider-invoke-callback
                      ctx
                      (lambda (a b) (int-add a b))
                      2
                      3)
                    (ctfe-provider-fact-set ctx \"walk.count\" root (size names))
                    (if (and (eq (size names) 1) (eq sum 5))
                      (ctfe-provider-diagnostics-note ctx root \"walk-ok\" \"demo.walk\")
                      (ctfe-provider-diagnostics-error ctx root \"walk-bad\" \"demo.walk\"))))))
            null
            (list-of \"write-facts\"))
          (bind source-unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind execution (ctfe-compiler-explain-provider-execution compiler \"compile_unit\" source-unit \"compile_time\")
              (bind executed (get (get execution \"executed\") 0)
                (list-of
                  (get executed \"changed\")
                  (get executed \"diagnostics_emitted\")
                  (get (get executed \"diagnostic_codes\") 0)
                  (get executed \"read_cells\"))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-traversal-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
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
fn test_ctfe_provider_stateful_traversal_matches_python_option_boundary() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-stateful-traversal-{}.caap",
        std::process::id()
    ));
    let file_text = "(int-add 1 2)\n";
    std::fs::write(&path, file_text).unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"stateful-traversal-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (ctfe-provider-fact-set ctx \"walk.count\" root 0)
                (ctfe-provider-traversal-walk
                  ctx
                  root
                  (lambda (node depth)
                    (do
                      (ctfe-provider-fact-set
                        ctx
                        \"walk.count\"
                        root
                        (int-add (ctfe-provider-fact-get ctx \"walk.count\" root 0) 1))
                      (if (ctfe-node-is-call node)
                        (sequence-map
                          (ctfe-node-children node)
                          (lambda (child) (list-of child (int-add depth 1))))
                        null)))
                  (map-of
                    \"mode\" \"stateful\"
                    \"initial_state\" 0
                    \"order\" \"not-a-valid-order-for-non-stateful\"
                    \"kind\" 42))
                (if (eq (ctfe-provider-fact-get ctx \"walk.count\" root 0) 4)
                  (ctfe-provider-diagnostics-note ctx root \"stateful-walk-ok\" \"demo.stateful.walk\")
                  (ctfe-provider-diagnostics-error ctx root \"stateful-walk-bad\" \"demo.stateful.walk\"))))
            null
            (list-of \"read-facts\" \"write-facts\"))
          (bind source-unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind artifact (ctfe-compiler-query-artifact compiler \"compile_unit\" source-unit \"compile_time\")
              (list-of
                (size (get artifact \"diagnostics\"))
                (get (get (get artifact \"diagnostics\") 0) \"message\")
                (get (get (get artifact \"diagnostics\") 0) \"code\")))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-stateful-traversal-options", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1));
    assert_eq!(items[1], RuntimeValue::Str("stateful-walk-ok".into()));
    assert_eq!(items[2], RuntimeValue::Str("demo.stateful.walk".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_resolution_scope_and_semantic_entry_builtins() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-resolution-{}.caap",
        std::process::id()
    ));
    let file_text = "existing\n";
    std::fs::write(&path, file_text).unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"resolution-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (ctfe-provider-require-effect ctx \"read-symbols\")
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (ctfe-unit-declare-symbol! unit \"existing\" \"compile_time\" root \"top_level\")
                (bind scope (ctfe-provider-base-resolution-scope ctx)
                  (bind child (ctfe-resolution-scope-fork scope)
                    (bind entry (ctfe-provider-semantic-entry-new ctx \"local.ctfe\" \"registered\" root \"compile_time\")
                      (ctfe-resolution-scope-define! child entry)
                      (bind resolved (ctfe-resolved-name-new entry)
                        (ctfe-provider-fact-set ctx \"caap.fact.resolved_name\" root resolved)
                        (bind resolved-entry (ctfe-node-resolved-name-entry root null)
                          (bind semantics (ctfe-call-semantics-from-entry entry)
                            (if
                              (and
                                (not (value-is-null (ctfe-resolution-scope-lookup scope \"existing\")))
                                (value-is-null (ctfe-resolution-scope-lookup scope \"local.ctfe\"))
                                (not (value-is-null (ctfe-resolution-scope-lookup child \"local.ctfe\")))
                                (eq (ctfe-semantic-entry-source entry) \"registered\")
                                (eq (ctfe-semantic-entry-name entry) \"local.ctfe\")
                                (eq (ctfe-semantic-entry-name resolved-entry) \"local.ctfe\")
                                (eq (get semantics \"callee_class\") \"registered\")
                                (eq (get semantics \"phase_policy\") \"compile_time\")
                                (not (value-is-null (ctfe-semantic-entry-unit entry)))
                                (eq
                                  (ctfe-node-id (ctfe-semantic-entry-node entry unit null))
                                  (ctfe-node-id root)))
                              (ctfe-provider-diagnostics-note ctx root \"scope-ok\" \"demo.scope\")
                              (ctfe-provider-diagnostics-error ctx root \"scope-bad\" \"demo.scope\"))))))))))
            null
            (list-of \"read-symbols\" \"write-facts\"))
          (bind source-unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind execution (ctfe-compiler-explain-provider-execution compiler \"compile_unit\" source-unit \"compile_time\")
              (bind executed (get (get execution \"executed\") 0)
                (list-of
                  (get executed \"diagnostics_emitted\")
                  (get (get executed \"diagnostic_codes\") 0))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-resolution-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
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
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-diagnostics-suggest-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "bad-name\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"diagnostics-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (ctfe-provider-diagnostics-warning
                  ctx
                  root
                  \"warning with notes\"
                  \"demo.notes\"
                  (list-of \"first note\" \"second note\"))
                (ctfe-provider-diagnostics-suggest
                  ctx
                  root
                  \"suggested fix\"
                  \"demo.fix\"
                  \"Apply replacement\"
                  \"replace\"
                  (map-of \"replacement\" \"good-name\")
                  \"note\"))))
          (bind source-unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (ctfe-compiler-explain-provider-execution compiler \"compile_unit\" source-unit \"compile_time\")))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-diagnostics-suggest", graph).unwrap();

    compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let diagnostics = compiler.diagnostics();
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(diagnostics[0].code.as_deref(), Some("demo.notes"));
    assert_eq!(diagnostics[0].notes, vec!["first note", "second note"]);
    assert_eq!(
        diagnostics[1].severity,
        caap_core_port::DiagnosticSeverity::Note
    );
    assert_eq!(diagnostics[1].fixes.len(), 1);
    assert_eq!(diagnostics[1].fixes[0].label, "Apply replacement");
    assert_eq!(diagnostics[1].fixes[0].kind, "replace");
    assert_eq!(
        diagnostics[1].fixes[0].metadata,
        vec![("replacement".to_string(), "good-name".to_string())]
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_linkage_project_mutates_unit_links() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-linkage-project-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "exported\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"linkage-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (ctfe-provider-linkage-project
                  ctx
                  \"projected.unit\"
                  (list-of (list-of \"dep.unit\" \"dep-name\" \"local-name\" true))
                  (list-of (list-of \"local-name\" \"public-name\")))
                (bind links (ctfe-unit-link-bindings unit)
                  (bind public (ctfe-unit-public-names unit)
                    (if
                      (and
                        (eq (ctfe-unit-id unit) \"projected.unit\")
                        (eq (get (get links 0) \"source_unit\") \"dep.unit\")
                        (eq (get (get links 0) \"syntax\") true)
                        (eq (get public 0) \"local-name\"))
                      (ctfe-provider-diagnostics-note ctx root \"linkage-ok\" \"demo.linkage\")
                      (ctfe-provider-diagnostics-error ctx root \"linkage-bad\" \"demo.linkage\")))))))
          (bind source-unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind execution (ctfe-compiler-explain-provider-execution compiler \"compile_unit\" source-unit \"compile_time\")
              (bind executed (get (get execution \"executed\") 0)
                (list-of
                  (get executed \"changed\")
                  (get executed \"diagnostics_emitted\")
                  (get (get executed \"diagnostic_codes\") 0))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-linkage-project", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Int(1));
    assert_eq!(items[2], RuntimeValue::Str("demo.linkage".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_call_scope_descriptors() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-scope-descriptors-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(bind x 1 (int-add x 2))\n(lambda (a b) (int-add a b))\n",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"scope-descriptor-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (bind bind-node (ctfe-unit-top-level-form-at unit 0)
                (bind scope (ctfe-provider-node-call-scope-descriptor ctx bind-node)
                  (bind lambda-node (ctfe-unit-top-level-form-at unit 1)
                    (bind lambda-scope (ctfe-provider-node-call-scope-descriptor ctx lambda-node)
                        (if
                          (and
                            (eq (get (get (get scope \"bindings\") 0) \"name\") \"x\")
                            (eq (size (get scope \"binding_value_ids\")) 1)
                            (eq (size (get scope \"body_ids\")) 1)
                            (eq (get scope \"result_kind\") \"scoped_eval\")
                            (eq (get (get (get lambda-scope \"bindings\") 0) \"name\") \"a\")
                            (eq (get (get (get lambda-scope \"bindings\") 1) \"name\") \"b\")
                            (eq (get lambda-scope \"result_kind\") \"closure\")
	                          (eq (size (get lambda-scope \"body_ids\")) 1))
                          (ctfe-provider-diagnostics-note ctx bind-node \"descriptors-ok\" \"demo.descriptor\")
                          (ctfe-provider-diagnostics-error ctx bind-node \"descriptors-bad\" \"demo.descriptor\"))))))))
          (bind source-unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind execution (ctfe-compiler-explain-provider-execution compiler \"compile_unit\" source-unit \"compile_time\")
              (bind executed (get (get execution \"executed\") 0)
                (list-of
                  (get executed \"diagnostics_emitted\")
                  (get (get executed \"diagnostic_codes\") 0))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-scope-descriptors", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1));
    assert_eq!(items[1], RuntimeValue::Str("demo.descriptor".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_call_control_descriptors() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-control-descriptors-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "(block exit (leave exit 9))\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"control-descriptor-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (bind block-node (ctfe-unit-top-level-form-at unit 0)
                (bind block-desc (ctfe-provider-node-call-control-descriptor ctx block-node)
                  (bind leave-desc (ctfe-provider-node-call-control-descriptor ctx (get (get block-desc \"values\") 0))
                    (if
                      (and
                        (eq (get block-desc \"kind\") \"block\")
                        (eq (size (get block-desc \"value_ids\")) 1)
                        (eq (get leave-desc \"kind\") \"leave\")
	                      (eq (size (get leave-desc \"value_ids\")) 1))
                      (ctfe-provider-diagnostics-note ctx block-node \"control-ok\" \"demo.control\")
                      (ctfe-provider-diagnostics-error ctx block-node \"control-bad\" \"demo.control\")))))))
          (bind source-unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind execution (ctfe-compiler-explain-provider-execution compiler \"compile_unit\" source-unit \"compile_time\")
              (bind executed (get (get execution \"executed\") 0)
                (list-of
                  (get executed \"diagnostics_emitted\")
                  (get (get executed \"diagnostic_codes\") 0))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-control-descriptors", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1));
    assert_eq!(items[1], RuntimeValue::Str("demo.control".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_lookup_and_explain_binding_reconstruct_bind_scope() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-binding-lookup-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "(bind lookup \"demo.value\" (int-add 1 2))\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"binding-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (bind bind-node (ctfe-unit-top-level-form-at unit 0)
                (bind scope (ctfe-provider-node-call-scope-descriptor ctx bind-node)
                  (bind target (get (get scope \"bodies\") 0)
                    (bind found (ctfe-provider-lookup-binding ctx target \"lookup\" \"fallback\")
                      (bind missing (ctfe-provider-explain-binding ctx target \"missing\" \"fallback\")
                        (if
                          (and
                            (eq found \"demo.value\")
                            (eq (get missing \"reconstructed\") true)
                            (eq (get missing \"found\") false)
                            (eq (get missing \"reason\") \"binding_missing\")
	                          (eq (get missing \"value\") \"fallback\"))
                          (ctfe-provider-diagnostics-note ctx bind-node \"binding-ok\" \"demo.binding\")
                          (ctfe-provider-diagnostics-error ctx bind-node \"binding-bad\" \"demo.binding\")))))))))
          (bind source-unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind execution (ctfe-compiler-explain-provider-execution compiler \"compile_unit\" source-unit \"compile_time\")
              (bind executed (get (get execution \"executed\") 0)
                (list-of
                  (get executed \"diagnostics_emitted\")
                  (get (get executed \"diagnostic_codes\") 0))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-binding-lookup", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1));
    assert_eq!(items[1], RuntimeValue::Str("demo.binding".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_provider_folds_registered_compile_time_call() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-provider-ctfe-fold-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "(demo-ctfe 41)\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-register-compile-time-function
            compiler
            \"demo-ctfe\"
            (lambda (ctx node)
              (ctfe-ir-instantiate \"literal\" (map-of \"value\" 42))))
          (ctfe-compiler-provider-register
            compiler
            \"ctfe-fold-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (bind root (ctfe-unit-top-level-form-at unit 0)
                (ctfe-unit-declare-symbol! unit \"demo-ctfe\" \"compile_time\" null \"top_level\")
                (bind entry (ctfe-provider-call-callee-entry ctx root)
                  (bind normalizable (ctfe-provider-normalizable-entry? entry)
                    (bind folded (ctfe-provider-fold-compile-time-call ctx root)
                      (if
                        (and
                          normalizable
                          (ctfe-node-is-literal folded)
                          (eq (ctfe-node-literal-value folded) 42))
                        (ctfe-provider-diagnostics-note ctx folded \"ctfe-fold-ok\" \"demo.ctfe.fold\")
                        (ctfe-provider-diagnostics-error ctx root \"ctfe-fold-bad\" \"demo.ctfe.fold\")))))))
            null
            (list-of \"read_facts\" \"write-ir\"))
          (bind source-unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (bind execution (ctfe-compiler-explain-provider-execution compiler \"compile_unit\" source-unit \"compile_time\")
              (bind executed (get (get execution \"executed\") 0)
                (list-of
                  (get executed \"diagnostics_emitted\")
                  (get (get executed \"diagnostic_codes\") 0))))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("provider-ctfe-fold", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1));
    assert_eq!(items[1], RuntimeValue::Str("demo.ctfe.fold".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_surface_file_builtins_load_template_and_form_records() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-surface-builtins-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(module demo.surface)\n(import-namespace module)\n(int-add 1 2)\n",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-load-surface-file-template compiler {:?})
          (list-of
            (host-value-kind (ctfe-compiler-load-surface-file-template compiler {:?}))
            (size (ctfe-compiler-parse-surface-file-forms compiler {:?} (list-of \"module\" \"import-namespace\")))
            (get (get (ctfe-compiler-parse-surface-file-forms compiler {:?} (list-of \"module\" \"import-namespace\")) 0) \"head\")))",
        path.display().to_string(),
        path.display().to_string(),
        path.display().to_string(),
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("surface-file-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("unit".into()));
    assert_eq!(items[1], RuntimeValue::Int(2));
    assert_eq!(items[2], RuntimeValue::Str("module".into()));
    assert_eq!(
        compiler.source_templates().artifact_cache().stats().misses,
        1
    );
    assert_eq!(compiler.source_templates().artifact_cache().stats().hits, 1);

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_surface_form_builtins_construct_parse_and_collect_bindings() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let source = r#"
        (bind span (map-of
            "start" 0
            "end" 9
            "start_line" 1
            "start_col" 1
            "end_line" 1
            "end_col" 10)
          (bind head (ctfe-surface-form-symbol "head" span)
            (bind item (ctfe-surface-form-integer "42" span)
              (bind form (ctfe-surface-form-list (list-of head item) span)
                (bind prepended (ctfe-surface-form-list-prepend head (list-of item) span "brace")
                  (bind parsed (ctfe-surface-parse-form "(head 42)" null)
                    (bind reparsed-int (ctfe-compiler-grammar-reparse-text null "integer" "123")
                      (bind reparsed-list (ctfe-compiler-grammar-reparse-text null "list" "(head 9)")
                        (bind reparsed-forms (ctfe-compiler-grammar-reparse-text null "forms" "(a) (b)")
                          (bind group (map-of "first" head "rest" (list-of (map-of "item" item)))
                            (bind collected (ctfe-surface-binding-group-collect group "item")
                              (list-of
                                (get head "kind")
                                (get head "value")
                                (get form "head")
                                (size (get form "items"))
                                (get prepended "delimiter")
                                (get parsed "head")
                                (get (get (get parsed "items") 1) "value")
                                (get reparsed-int "kind")
                                (get reparsed-int "value")
                                (get reparsed-list "head")
                                (size reparsed-forms)
                                (get (get reparsed-forms 1) "head")
                                (size collected)
                                (get (get collected 1) "kind")
                                (ctfe-surface-binding-get group "missing" "fallback")
                                (ctfe-surface-binding-get (map-of "missing" null) "missing" "fallback-null")))))))))))))"#;
    let graph = parse(source).unwrap();
    let unit = Unit::from_graph("surface-form-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
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
    assert_eq!(items[15], RuntimeValue::Str("fallback-null".into()));
}

#[test]
fn test_ctfe_compiler_compile_unit_builtin_registers_unit_handle() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-compile-unit-builtins-{}.caap",
        std::process::id()
    ));
    std::fs::write(&path, "(module \"demo.compile\")\n(int-add 1 2)\n").unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (host-value-kind
            (ctfe-compiler-compile-unit
              compiler
              (ctfe-compiler-load-surface-file-template compiler {:?}))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("compile-unit-builtin", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Str("unit".into()));
    assert!(compiler.get_unit("demo.compile").unwrap().is_some());

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_compile_unit_passes_initial_bindings_to_provider() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-compile-unit-initial-{}.caap",
        std::process::id()
    ));
    let file_text = "placeholder\n";
    std::fs::write(&path, file_text).unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-stage-register compiler \"compile_unit\")
          (ctfe-compiler-provider-register
            compiler
            \"initial-provider\"
            \"compile_unit\"
            (lambda (compiler unit ctx)
              (ctfe-unit-add-public-name! unit initial-public)))
          (bind compiled
            (ctfe-compiler-compile-unit
              compiler
              (ctfe-compiler-load-surface-file-template compiler {:?})
              (map-of \"initial-public\" \"public-from-initial\"))
            (bind facts
              (ctfe-unit-query
                compiled
                (list-of (list-of \"has\" \"symbol:public-from-initial\" \"symbol.entry\")))
              (get (get (get facts 0) \"subject\") \"value\"))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("compile-unit-initial-bindings", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Str("public-from-initial".into()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_evaluate_capture_builtin_projects_result_and_diagnostics() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let ok_path = std::env::temp_dir().join(format!(
        "caap-rust-evaluate-capture-ok-{}.caap",
        std::process::id()
    ));
    let err_path = std::env::temp_dir().join(format!(
        "caap-rust-evaluate-capture-err-{}.caap",
        std::process::id()
    ));
    std::fs::write(&ok_path, "(int-add external 2)\n").unwrap();
    std::fs::write(&err_path, "(runtime-error \"boom\")\n").unwrap();
    let source = format!(
        "(bind ok-unit (ctfe-compiler-load-surface-file-template compiler {:?})
          (bind err-unit (ctfe-compiler-load-surface-file-template compiler {:?})
            (list-of
              (get
                (ctfe-compiler-evaluate-capture
                  compiler
                  ok-unit
                  \"runtime\"
                  (map-of \"external\" 40))
                \"result\")
              (get
                (get
                  (get
                    (ctfe-compiler-evaluate-capture compiler err-unit \"runtime\")
                    \"diagnostics\")
                  0)
                \"code\"))))",
        ok_path.display().to_string(),
        err_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("evaluate-capture-builtin", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(42));
    assert_eq!(items[1], RuntimeValue::Str("CAAP-RUNTIME-001".into()));
    assert_eq!(compiler.diagnostics().len(), 1);

    let _ = std::fs::remove_file(ok_path);
    let _ = std::fs::remove_file(err_path);
}

#[test]
fn test_ctfe_compiler_evaluate_bootstrap_file_builtin_captures_result() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-evaluate-bootstrap-file-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(runtime-error \"skipped\")\n\
         (list-of
           (int-add external 2)
           (size (ctfe-compiler-current-bootstrap-capabilities compiler))
           (ctfe-compiler-current-bootstrap-path compiler))\n",
    )
    .unwrap();
    let source = format!(
        "(get
          (ctfe-compiler-evaluate-bootstrap-file
            compiler
            {:?}
            (map-of \"external\" 40)
            (list-of \"host_services\")
            1
            false)
          \"result\")",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("evaluate-bootstrap-file-builtin", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
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
fn test_ctfe_compiler_execute_bootstrap_file_builtin_runs_source() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-execute-bootstrap-file-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(ctfe-compiler-register-value compiler \"bootstrap.answer\" 42)\n\
         (ctfe-compiler-register-value
           compiler
           \"bootstrap.path\"
           (ctfe-compiler-current-bootstrap-path compiler))\n\
         (ctfe-compiler-register-value
           compiler
           \"bootstrap.capability-count\"
           (size (ctfe-compiler-current-bootstrap-capabilities compiler)))\n",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (list-of
            (ctfe-compiler-lookup-value compiler \"bootstrap.answer\")
            (ctfe-compiler-lookup-value compiler \"bootstrap.path\")
            (ctfe-compiler-lookup-value compiler \"bootstrap.capability-count\")))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("execute-bootstrap-file-builtin", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
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
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-execute-bootstrap-file-capabilities-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(ctfe-compiler-register-value
           compiler
           \"bootstrap.capabilities\"
           (ctfe-compiler-current-bootstrap-capabilities compiler))\n",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file
            compiler
            {:?}
            (list-of \"host_services\"))
          (ctfe-compiler-lookup-value compiler \"bootstrap.capabilities\"))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("execute-bootstrap-file-capabilities", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();
    let _ = std::fs::remove_file(path);

    assert_eq!(
        value,
        RuntimeValue::Tuple(vec![RuntimeValue::Str("host_services".into())].into())
    );
}

#[test]
fn test_ctfe_compiler_execute_bootstrap_file_skips_package_declarations() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-execute-bootstrap-file-declarations-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "(module \"demo.bootstrap\")\n\
         (export \"demo.bootstrap\")\n\
         (ctfe-compiler-register-value compiler \"bootstrap.declaration.skip\" 42)\n",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (ctfe-compiler-lookup-value compiler \"bootstrap.declaration.skip\"))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("execute-bootstrap-file-declarations", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(42));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_ctfe_compiler_execute_bootstrap_file_runs_real_sequence_stdlib() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../stdlib/lang/sequence/bootstrap.caap");
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind seq (ctfe-compiler-lookup-value compiler \"stdlib.sequence\")
            (list-of
              (invoke (get seq \"length\") (list-of 1 2 3))
              (invoke (get seq \"at\") (list-of \"a\" \"b\") 1)
              (invoke (get seq \"is-null?\") null))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("execute-real-sequence-bootstrap-file", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(3));
    assert_eq!(items[1], RuntimeValue::Str("b".into()));
    assert_eq!(items[2], RuntimeValue::Bool(true));
}

#[test]
fn test_ctfe_compiler_order_bootstrap_plan_builtin_toposorts() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse(
        r#"(ctfe-compiler-order-bootstrap-plan
             (list-of
               (map-of "path" "runtime.caap" "depends" (list-of "common.caap"))
               (map-of "path" "common.caap" "depends" (list-of))
               (map-of "path" "api.caap" "depends" (list-of "runtime.caap"))))"#,
    )
    .unwrap();
    let unit = Unit::from_graph("bootstrap-plan-order", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(
        value,
        RuntimeValue::Tuple(
            vec![
                RuntimeValue::Str("common.caap".into()),
                RuntimeValue::Str("runtime.caap".into()),
                RuntimeValue::Str("api.caap".into()),
            ]
            .into()
        )
    );
}

#[test]
fn test_ctfe_compiler_execute_bootstrap_file_runs_real_compiler_kit_toolchain() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../stdlib/kits/compiler_kit/toolchain.caap");
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (stages (ctfe-compiler-list-stages compiler))
            (providers (ctfe-compiler-list-providers compiler))
            (resolve-providers (ctfe-compiler-list-providers compiler \"resolve_names\"))
          )
            (list-of
              (value-lt 6 (size stages))
              (not
                (eq
                  (sequence-find
                    stages
                    (lambda (stage)
                      (eq (get stage \"name\" null) \"parse_surface\")))
                  null))
              (value-lt 7 (size providers))
              (not
                (eq
                  (sequence-find
                    resolve-providers
                    (lambda (provider)
                      (eq (get provider \"stage\" null) \"resolve_names\")))
                  null)))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("execute-real-compiler-kit-toolchain", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Bool(true));
    assert_eq!(items[3], RuntimeValue::Bool(true));
}

#[test]
fn test_ctfe_compiler_execute_bootstrap_file_runs_real_module_seed() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let plan_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../stdlib/module/bootstrap_plan.caap");
    let seed_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/module/seed.caap");
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (api (ctfe-compiler-lookup-value compiler \"stdlib.module.seed.api\"))
            (catalog (ctfe-compiler-lookup-value compiler \"stdlib.module.seed.module-catalog\"))
            (modules (ctfe-compiler-lookup-value compiler \"stdlib.module.seed.list-modules\"))
          )
            (list-of
              (value-is-map api)
              (value-is-callable (get api \"load-module\" null))
              (value-is-callable catalog)
              (value-is-callable modules))))",
        plan_path.display().to_string(),
        seed_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("execute-real-module-seed", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Bool(true));
    assert_eq!(items[3], RuntimeValue::Bool(true));
}

#[test]
fn test_ctfe_compiler_execute_bootstrap_file_runs_real_stdlib_root_bootstrap() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/bootstrap.caap");
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (seed-api (ctfe-compiler-lookup-value compiler \"stdlib.module.seed.api\"))
            (module-api (ctfe-compiler-lookup-value compiler \"stdlib.module.api\"))
            (register-root (ctfe-compiler-lookup-value compiler \"stdlib.module.register-root\"))
            (loaded (ctfe-compiler-lookup-value compiler \"stdlib.module.seed.materialized-modules\"))
          )
            (list-of
              (value-is-map seed-api)
              (value-is-map module-api)
              (value-is-callable register-root)
              (contains ((get seed-api \"module-catalog\")) \"stdlib.module\")
              (contains (loaded) \"stdlib.module\"))))",
        path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("execute-real-stdlib-root-bootstrap", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Bool(true));
    assert_eq!(items[3], RuntimeValue::Bool(true));
    assert_eq!(items[4], RuntimeValue::Bool(true));
}

#[test]
fn test_stdlib_system_module_load_registers_public_registry_value() {
    let mut host = caap_core_port::CompilerHost::new();
    host.register_default_runtime_system_libraries().unwrap();
    host.register_default_compile_time_system_libraries()
        .unwrap();
    let mut compiler = host.new_session();
    let bootstrap_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/bootstrap.caap");
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (seed-api (ctfe-compiler-lookup-value compiler \"stdlib.module.seed.api\"))
            (load-module (get seed-api \"load-module\" null))
          )
            (do
              (load-module \"sys.io\")
              (bind io-module (ctfe-compiler-lookup-value compiler \"sys.io\")
                (list-of
                  (value-is-map io-module)
                  (value-is-callable (get io-module \"println\" null)))))))",
        bootstrap_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("load-system-io-module", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Bool(true));
}

#[test]
fn test_stdlib_system_os_and_format_modules_use_python_shaped_host_catalog() {
    let mut host = caap_core_port::CompilerHost::new();
    host.register_default_runtime_system_libraries().unwrap();
    host.register_default_compile_time_system_libraries()
        .unwrap();
    let mut compiler = host.new_session();
    let bootstrap_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/bootstrap.caap");
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (seed-api (ctfe-compiler-lookup-value compiler \"stdlib.module.seed.api\"))
            (load-module (get seed-api \"load-module\" null))
          )
            (do
              (load-module \"sys.os\")
              (load-module \"sys.fmt\")
              (bind (
                (env-module (ctfe-compiler-lookup-value compiler \"sys.env\"))
                (path-module (ctfe-compiler-lookup-value compiler \"sys.path\"))
                (os-module (ctfe-compiler-lookup-value compiler \"sys.os\"))
                (fmt-module (ctfe-compiler-lookup-value compiler \"sys.fmt\"))
              )
                (list-of
                  (value-is-callable (get env-module \"env-get\" null))
                  (value-is-callable (get path-module \"getcwd\" null))
                  (value-is-callable (get os-module \"platform\" null))
                  (value-is-callable (get fmt-module \"format\" null))
                  ((get fmt-module \"format\") \"hello {{}}\" \"caap\")
                  (value-is-null (get path-module \"basename\" null))
                  (value-is-null (get os-module \"current-dir\" null)))))))",
        bootstrap_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("load-system-os-format-modules", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Bool(true));
    assert_eq!(items[3], RuntimeValue::Bool(true));
    assert_eq!(items[4], RuntimeValue::Str("hello caap".into()));
    assert_eq!(items[5], RuntimeValue::Bool(true));
    assert_eq!(items[6], RuntimeValue::Bool(true));
}

#[test]
fn test_stdlib_types_checker_load_registers_public_registry_value_and_providers() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let bootstrap_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/bootstrap.caap");
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (seed-api (ctfe-compiler-lookup-value compiler \"stdlib.module.seed.api\"))
            (load-module (get seed-api \"load-module\" null))
          )
            (do
              (load-module \"stdlib.types.checker\")
              (bind (
                (descriptor (load-module \"stdlib.types.checker\"))
                (checker (ctfe-compiler-lookup-value compiler \"stdlib.types.checker\" null))
              )
                (list-of
                  (get descriptor \"state\" null)
                  (host-value-kind (get descriptor \"result\" null))
                  (host-value-kind checker)
                  (value-is-map checker)
                  (if (value-is-map checker)
                    (value-is-callable (get checker \"type-inference-provider\" null))
                    false)
                  (if (value-is-map checker)
                    (value-is-callable (get checker \"type-check-provider\" null))
                    false)
                  (if (value-is-map checker)
                    (get checker \"function-type-summary-fact\" null)
                    null))))))",
        bootstrap_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("load-types-checker-module", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("loaded".into()));
    assert_eq!(items[1], RuntimeValue::Str("null".into()));
    assert_eq!(items[2], RuntimeValue::Str("map".into()));
    assert_eq!(items[3], RuntimeValue::Bool(true));
    assert_eq!(items[4], RuntimeValue::Bool(true));
    assert_eq!(items[5], RuntimeValue::Bool(true));
    assert_eq!(
        items[6],
        RuntimeValue::Str("caap.fact.function_type_summary".into())
    );
    let provider_names: Vec<String> = compiler
        .provider_registry()
        .ordered_providers()
        .iter()
        .map(|provider| provider.name.clone())
        .collect();
    assert!(provider_names.contains(&"stdlib.types.checker.infer-types".to_string()));
    assert!(provider_names.contains(&"stdlib.types.checker.validate-type-facts".to_string()));
}

#[test]
fn test_stdlib_types_checker_provider_infers_integer_arithmetic_root_type() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let bootstrap_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/bootstrap.caap");
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (seed-api (ctfe-compiler-lookup-value compiler \"stdlib.module.seed.api\"))
            (load-module (get seed-api \"load-module\" null))
          )
            (load-module \"stdlib.types.checker\")))",
        bootstrap_path.display().to_string(),
    );
    let bootstrap_graph = parse(&source).unwrap();
    let bootstrap_unit = Unit::from_graph("load-types-checker-for-query", bootstrap_graph).unwrap();
    compiler
        .evaluation()
        .evaluate(&bootstrap_unit, PhasePolicy::CompileTime, [])
        .unwrap();

    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("types-checker-target", graph).unwrap();
    let root_id = unit.root_id();

    let plan = compiler
        .queries()
        .query_with_options(
            "validate_graph",
            &mut unit,
            PhasePolicy::CompileTime,
            QueryExecutionOptions::new(),
        )
        .unwrap();

    assert!(plan
        .executed
        .iter()
        .any(|record| record.provider_name == "stdlib.types.checker.infer-types"));
    assert!(plan
        .executed
        .iter()
        .any(|record| record.provider_name == "stdlib.types.checker.validate-type-facts"));
    let inferred = unit
        .semantics()
        .get_fact(&node_subject_id(root_id), "caap.fact.inferred_type")
        .unwrap();
    assert_eq!(inferred, Some(&SemanticValue::Str("primitive:i32".into())));
}

#[test]
fn test_stdlib_module_check_source_uses_rust_module_root_callbacks() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let bootstrap_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/bootstrap.caap");
    let source_path = std::env::temp_dir().join(format!(
        "caap-rust-module-root-source-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &source_path,
        r#"
          (module "demo.root_source")
          (int-add 1 2)
        "#,
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (callbacks (ctfe-compiler-module-root-callbacks compiler))
            (check-source (ctfe-compiler-lookup-value compiler \"stdlib.module.check-source\"))
          )
            (do
              (check-source {:?} callbacks)
              (list-of
                (value-is-map callbacks)
                (value-is-callable (get callbacks \"load-source-unit\" null))
                (value-is-callable check-source)))))",
        bootstrap_path.display().to_string(),
        source_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("execute-stdlib-module-check-source", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, []);
    std::fs::remove_file(&source_path).ok();
    let value = value.unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Bool(true));
    assert_eq!(items[2], RuntimeValue::Bool(true));
    assert!(compiler.catalog().contains_unit("demo.root_source"));
}

#[test]
fn test_stdlib_module_check_source_registers_source_compile_time_function() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let bootstrap_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/bootstrap.caap");
    let source_path = std::env::temp_dir().join(format!(
        "caap-rust-module-root-source-ctfe-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &source_path,
        r#"
          (module "demo.source_ctfe_registration")
          (import-symbols "stdlib.pass-kit" "stdlib.pass-kit")
          (stdlib.pass-kit.register-compile-time-function
            "demo.source-ctfe-now"
            (lambda (ctx node)
              null))
          (bind ((main (lambda () 0))) (main))
        "#,
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (callbacks (ctfe-compiler-module-root-callbacks compiler))
            (check-source (ctfe-compiler-lookup-value compiler \"stdlib.module.check-source\"))
          )
            (do
              (check-source {:?} callbacks)
              (if
                (ctfe-compiler-lookup-value compiler \"demo.source-ctfe-now\" null)
                \"registered\"
                \"missing\"))))",
        bootstrap_path.display().to_string(),
        source_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("source-ctfe-registration", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, []);
    std::fs::remove_file(&source_path).ok();

    assert_eq!(value.unwrap(), RuntimeValue::Str("registered".into()));
}

#[test]
fn test_stdlib_module_run_source_folds_source_registered_compile_time_call() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let bootstrap_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/bootstrap.caap");
    let source_path = std::env::temp_dir().join(format!(
        "caap-rust-module-root-source-ctfe-run-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &source_path,
        r#"
          (module "demo.source_ctfe_run")
          (import-symbols "stdlib.pass-kit" "stdlib.pass-kit")
          (stdlib.pass-kit.register-compile-time-function
            "demo.source-ctfe-value"
            (lambda (ctx node)
              (ctfe-ir-instantiate "literal" (map-of "value" 42))))
          (bind ((main (lambda () (demo.source-ctfe-value)))) (main))
        "#,
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (callbacks (ctfe-compiler-module-root-callbacks compiler))
            (run-source (ctfe-compiler-lookup-value compiler \"stdlib.module.run-source\"))
          )
            (run-source {:?} callbacks)))",
        bootstrap_path.display().to_string(),
        source_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("source-ctfe-run", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, []);
    std::fs::remove_file(&source_path).ok();

    assert_eq!(value.unwrap(), RuntimeValue::Int(42));
}

#[test]
fn test_stdlib_module_source_ctfe_registration_allows_provider_setup_side_effect() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let bootstrap_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/bootstrap.caap");
    let source_path = std::env::temp_dir().join(format!(
        "caap-rust-module-root-source-ctfe-provider-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &source_path,
        r#"
          (module "demo.source_ctfe_provider_setup")
          (import-symbols "stdlib.pass-kit" "stdlib.pass-kit")
          (stdlib.pass-kit.register-compile-time-function
            "demo.source-ctfe-provider-value"
            (bind (
              (callback
                (lambda (ctx node)
                  (ctfe-ir-instantiate "literal" (map-of "value" 77))))
            )
              (do
                (stdlib.pass-kit.register-provider
                  "demo.source-provider"
                  "validate_graph"
                  (lambda (ctx root) false)
                  (list-of "validate_graph")
                  (list-of "read-ir")
                  (stdlib.pass-kit.provider-spec
                    "validate_graph"
                    (list-of "ir")
                    (list-of)
                    "none"
                    "safe"
                    null))
                callback)))
          (bind ((main (lambda () (demo.source-ctfe-provider-value)))) (main))
        "#,
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (callbacks (ctfe-compiler-module-root-callbacks compiler))
            (run-source (ctfe-compiler-lookup-value compiler \"stdlib.module.run-source\"))
          )
            (run-source {:?} callbacks)))",
        bootstrap_path.display().to_string(),
        source_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("source-ctfe-provider-setup", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, []);
    std::fs::remove_file(&source_path).ok();

    assert_eq!(value.unwrap(), RuntimeValue::Int(77));
}

#[test]
fn test_module_root_callback_prepares_inline_syntax_rule() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let source_path = std::env::temp_dir().join(format!(
        "caap-rust-module-root-inline-syntax-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &source_path,
        r#"
          (module "demo.inline_syntax")
          (define-syntax-rule
            "add rule inline_rule = symbol"
            (lambda (form span) form))
          (int-add 1 2)
        "#,
    )
    .unwrap();
    let source = format!(
        "(bind (
          (callbacks (ctfe-compiler-module-root-callbacks compiler))
          (load-source-unit (get callbacks \"load-source-unit\"))
          (prepare-source-syntax-unit (get callbacks \"prepare-source-syntax-unit\"))
        )
          (bind unit (load-source-unit {:?})
            (do
              (prepare-source-syntax-unit {:?} unit)
              (bind metadata (ctfe-unit-syntax-metadata-get unit \"inline_rule\")
                (bind hook-ref (get (get (get metadata \"semantic_hooks\") 0) 1)
                  (list-of
                    hook-ref
                    (get
                      (ctfe-unit-syntax-metadata-get unit \"semantic_hook_inline_sources\")
                      hook-ref)))))))",
        source_path.display().to_string(),
        source_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("prepare-module-root-inline-syntax", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, []);
    std::fs::remove_file(&source_path).ok();

    let RuntimeValue::List(items) = value.unwrap() else {
        panic!("expected list result");
    };
    let items = items.borrow();
    let RuntimeValue::Str(hook_ref) = &items[0] else {
        panic!("expected inline hook ref");
    };
    assert!(hook_ref.starts_with("inline.syntax."));
    assert_eq!(
        items[1],
        RuntimeValue::Str("(lambda (form span) form)".into()),
    );
}

#[test]
fn test_stdlib_module_check_source_accepts_declaration_only_source() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let bootstrap_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/bootstrap.caap");
    let source_path = std::env::temp_dir().join(format!(
        "caap-rust-module-root-declaration-only-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &source_path,
        r#"
          (module "demo.declaration_only")
          (define-syntax-rule
            "add rule declaration_only = symbol"
            (lambda (form span) form))
        "#,
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (callbacks (ctfe-compiler-module-root-callbacks compiler))
            (check-source (ctfe-compiler-lookup-value compiler \"stdlib.module.check-source\"))
          )
            (check-source {:?} callbacks)))",
        bootstrap_path.display().to_string(),
        source_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("check-module-root-declaration-only", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, []);
    std::fs::remove_file(&source_path).ok();

    assert_eq!(value.unwrap(), RuntimeValue::Null);
    assert!(compiler.catalog().contains_unit("demo.declaration_only"));
}

#[test]
fn test_module_root_compile_source_unit_updates_declaration_only_unit_handle() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let bootstrap_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/bootstrap.caap");
    let source_path = std::env::temp_dir().join(format!(
        "caap-rust-module-root-compile-declaration-only-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &source_path,
        r#"
          (module "demo.compile_declaration_only")
          (syntax-metadata "demo" "value")
        "#,
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (callbacks (ctfe-compiler-module-root-callbacks compiler))
            (load-source-unit (get callbacks \"load-source-unit\"))
            (compile-source-unit (get callbacks \"compile-source-unit\"))
          )
            (bind unit (load-source-unit {:?})
              (bind before-version (ctfe-unit-version unit)
                (do
                  (bind compiled (compile-source-unit unit (map-of))
                  (list-of
                    before-version
                    (ctfe-unit-version unit)
                    (ctfe-unit-version compiled)
                    (size (ctfe-unit-top-level-forms unit))
                    (size (ctfe-unit-top-level-forms compiled)))))))))",
        bootstrap_path.display().to_string(),
        source_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("compile-source-unit-mutates-runtime-entry", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, []);
    std::fs::remove_file(&source_path).ok();

    let RuntimeValue::List(items) = value.unwrap() else {
        panic!("expected list result");
    };
    let items = items.borrow();
    let RuntimeValue::Int(before_version) = items[0] else {
        panic!("expected before version int");
    };
    let RuntimeValue::Int(after_version) = items[1] else {
        panic!("expected after version int");
    };
    let RuntimeValue::Int(returned_version) = items[2] else {
        panic!("expected returned version int");
    };
    assert!(after_version > before_version);
    assert_eq!(after_version, returned_version);
    assert_eq!(items[3], items[4]);
}

#[test]
fn test_module_root_compile_source_unit_removes_source_setup_forms() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let bootstrap_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/bootstrap.caap");
    let source_path = std::env::temp_dir().join(format!(
        "caap-rust-module-root-clean-source-setup-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &source_path,
        r#"
          (module "demo.clean_source_setup")
          (define-syntax-rule
            "add rule clean_source_setup = symbol"
            (lambda (form span) form))
          (int-add 1 2)
        "#,
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (callbacks (ctfe-compiler-module-root-callbacks compiler))
            (check-source (ctfe-compiler-lookup-value compiler \"stdlib.module.check-source\"))
          )
            (check-source {:?} callbacks)))",
        bootstrap_path.display().to_string(),
        source_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("compile-source-unit-clean-source-setup", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, []);
    std::fs::remove_file(&source_path).ok();
    assert_eq!(value.unwrap(), RuntimeValue::Null);

    let compiled = compiler
        .catalog()
        .get_compiled_unit("demo.clean_source_setup")
        .unwrap()
        .expect("compiled source unit should be registered");
    let heads: Vec<_> = compiled
        .top_level_form_ids()
        .iter()
        .filter_map(|node_id| top_level_head(compiled, *node_id))
        .collect();
    assert!(!heads.contains(&"define-syntax-rule".to_string()));
    assert!(heads.contains(&"int-add".to_string()));
    assert_eq!(
        top_level_head(compiled, compiled.root_id()).as_deref(),
        Some("int-add")
    );
}

#[test]
fn test_module_root_compile_source_unit_normalizes_runtime_declarations() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let bootstrap_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/bootstrap.caap");
    let source_path = std::env::temp_dir().join(format!(
        "caap-rust-module-root-runtime-decls-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &source_path,
        r#"
          (module "demo.runtime_decls")
          (bind "helper" (lambda () 40) null)
          (int-add (helper) 2)
        "#,
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (callbacks (ctfe-compiler-module-root-callbacks compiler))
            (run-source (ctfe-compiler-lookup-value compiler \"stdlib.module.run-source\"))
          )
            (run-source {:?} callbacks)))",
        bootstrap_path.display().to_string(),
        source_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("compile-source-unit-normalize-runtime-decls", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, []);
    std::fs::remove_file(&source_path).ok();
    assert_eq!(value.unwrap(), RuntimeValue::Int(42));

    let compiled = compiler
        .catalog()
        .get_compiled_unit("demo.runtime_decls")
        .unwrap()
        .expect("compiled source unit should be registered");
    let heads: Vec<_> = compiled
        .top_level_form_ids()
        .iter()
        .filter_map(|node_id| top_level_head(compiled, *node_id))
        .collect();
    assert_eq!(heads, vec!["bind".to_string()]);
    assert_eq!(
        top_level_head(compiled, compiled.root_id()).as_deref(),
        Some("bind")
    );
}

#[test]
fn test_module_root_emit_source_unit_dispatches_registered_emitter() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let source_path = std::env::temp_dir().join(format!(
        "caap-rust-module-root-emit-source-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &source_path,
        r#"
          (module "demo.emit_source")
          (int-add 1 2)
        "#,
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-register-value
            compiler
            \"demo.emit-source\"
            (lambda (unit)
              (map-of
                \"text\"
                  (ctfe-unit-id unit)
                \"diagnostics\"
                  (list-of))))
          (bind (
            (callbacks (ctfe-compiler-module-root-callbacks compiler))
            (load-source-unit (get callbacks \"load-source-unit\"))
            (emit-source-unit (get callbacks \"emit-source-unit\"))
          )
            (bind unit (load-source-unit {:?})
              (emit-source-unit unit \"demo.emit-source\"))))",
        source_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("emit-module-root-source-unit", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, []);
    std::fs::remove_file(&source_path).ok();

    assert_eq!(value.unwrap(), RuntimeValue::Str("demo.emit_source".into()));
}

#[test]
fn test_module_root_emit_source_unit_rejects_emitter_diagnostics() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let source_path = std::env::temp_dir().join(format!(
        "caap-rust-module-root-emit-source-diagnostics-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &source_path,
        r#"
          (module "demo.emit_source_diagnostics")
          (int-add 1 2)
        "#,
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-register-value
            compiler
            \"demo.emit-source-diagnostic\"
            (lambda (unit)
              (map-of
                \"text\" \"\"
                \"diagnostics\"
                  (list-of
                    (map-of
                      \"severity\" \"error\"
                      \"code\" \"caap.llvm.error.test\"
                      \"message\" \"test codegen failure\"
                      \"path\" \"entry\")))))
          (bind (
            (callbacks (ctfe-compiler-module-root-callbacks compiler))
            (load-source-unit (get callbacks \"load-source-unit\"))
            (emit-source-unit (get callbacks \"emit-source-unit\"))
          )
            (bind unit (load-source-unit {:?})
              (emit-source-unit unit \"demo.emit-source-diagnostic\"))))",
        source_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("emit-module-root-source-unit-diagnostics", graph).unwrap();

    let err = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .expect_err("diagnostic emitter result should fail");
    std::fs::remove_file(&source_path).ok();

    assert!(err
        .to_string()
        .contains("caap.llvm.error.test: test codegen failure (entry)"));
}

#[test]
fn test_module_root_file_callbacks_resolve_bootstrap_relative_paths() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let dir = std::env::temp_dir().join(format!(
        "caap-rust-module-root-relative-{}-{}",
        std::process::id(),
        line!()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let driver_path = dir.join("driver.caap");
    let neighbor_path = dir.join("neighbor.caap");
    std::fs::write(
        &neighbor_path,
        r#"
          (module "demo.relative_neighbor")
          (import-namespace "demo.runtime_dep" "runtime_dep")
          (syntax-import "demo.syntax_dep")
        "#,
    )
    .unwrap();
    std::fs::write(
        &driver_path,
        r#"
          (bind (
            (callbacks (ctfe-compiler-module-root-callbacks compiler))
            (is-file (get callbacks "is-file"))
            (load-source-unit (get callbacks "load-source-unit"))
            (module-name (get callbacks "collect-source-module-name"))
            (imports (get callbacks "collect-source-imports"))
            (syntax-imports (get callbacks "collect-source-syntax-imports"))
          )
            (list-of
              (is-file "neighbor.caap")
              (ctfe-unit-id (load-source-unit "neighbor.caap"))
              (module-name "neighbor.caap")
              (imports "neighbor.caap")
              (syntax-imports "neighbor.caap")))
        "#,
    )
    .unwrap();
    let source = format!(
        "(ctfe-compiler-evaluate-bootstrap-file
            compiler
            {:?}
            (map-of)
            (list-of)
            0
            false)",
        driver_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("module-root-bootstrap-relative-callbacks", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, []);
    std::fs::remove_dir_all(&dir).ok();

    let RuntimeValue::Map(capture) = value.unwrap() else {
        panic!("expected capture map");
    };
    let value = capture
        .borrow()
        .iter()
        .find_map(|(key, value)| {
            if key == &caap_core_port::MapKey::Str("result".into()) {
                Some(value.clone())
            } else {
                None
            }
        })
        .expect("expected result field");
    let RuntimeValue::List(items) = value else {
        panic!("expected list result: {value:?}; capture={capture:?}");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Str("demo.relative_neighbor".into()));
    assert_eq!(items[2], RuntimeValue::Str("demo.relative_neighbor".into()));
    let RuntimeValue::Tuple(imports) = &items[3] else {
        panic!("expected runtime imports tuple");
    };
    assert_eq!(
        imports.as_ref(),
        &[
            RuntimeValue::Str("demo.runtime_dep".into()),
            RuntimeValue::Str("demo.syntax_dep".into()),
        ]
    );
    let RuntimeValue::Tuple(syntax_imports) = &items[4] else {
        panic!("expected syntax imports tuple");
    };
    assert_eq!(
        syntax_imports.as_ref(),
        &[RuntimeValue::Str("demo.syntax_dep".into())]
    );
}

#[test]
fn test_stdlib_module_run_source_uses_rust_module_root_callbacks() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let bootstrap_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/bootstrap.caap");
    let source_path = std::env::temp_dir().join(format!(
        "caap-rust-module-root-run-source-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &source_path,
        r#"
          (module "demo.run_source")
          (int-add 1 2)
        "#,
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-file compiler {:?})
          (bind (
            (callbacks (ctfe-compiler-module-root-callbacks compiler))
            (run-source (ctfe-compiler-lookup-value compiler \"stdlib.module.run-source\"))
          )
            (run-source {:?} callbacks)))",
        bootstrap_path.display().to_string(),
        source_path.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("execute-stdlib-module-run-source", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, []);
    std::fs::remove_file(&source_path).ok();

    assert_eq!(value.unwrap(), RuntimeValue::Int(3));
    assert!(compiler.catalog().contains_unit("demo.run_source"));
}

#[test]
fn test_ctfe_compiler_execute_bootstrap_files_builtin_runs_sources_in_order() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let first = std::env::temp_dir().join(format!(
        "caap-rust-execute-bootstrap-files-first-{}.caap",
        std::process::id()
    ));
    let second = std::env::temp_dir().join(format!(
        "caap-rust-execute-bootstrap-files-second-{}.caap",
        std::process::id()
    ));
    std::fs::write(
        &first,
        "(ctfe-compiler-register-value compiler \"bootstrap.first\" 10)\n",
    )
    .unwrap();
    std::fs::write(
        &second,
        "(ctfe-compiler-register-value
          compiler
          \"bootstrap.second\"
          (int-add (ctfe-compiler-lookup-value compiler \"bootstrap.first\") 5))\n",
    )
    .unwrap();
    let source = format!(
        "(do
          (ctfe-compiler-execute-bootstrap-files
            compiler
            (list-of {:?} {:?}))
          (ctfe-compiler-lookup-value compiler \"bootstrap.second\"))",
        first.display().to_string(),
        second.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("execute-bootstrap-files-builtin", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(15));
    assert_eq!(compiler.bootstrap_executions().len(), 2);
    assert_eq!(compiler.bootstrap_trace().len(), 2);

    let _ = std::fs::remove_file(first);
    let _ = std::fs::remove_file(second);
}

#[test]
fn test_ctfe_compiler_directory_query_builtins_project_entries() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let root = std::env::temp_dir().join(format!("caap-rust-dir-builtins-{}", std::process::id()));
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(root.join("a.caap"), "null").unwrap();
    std::fs::write(nested.join("b.caap"), "null").unwrap();
    let source = format!(
        "(list-of
          (size (ctfe-compiler-list-dir compiler {:?}))
          (get (get (ctfe-compiler-list-dir compiler {:?}) 0) \"name\")
          (size (ctfe-compiler-walk-dir compiler {:?}))
          (get (get (ctfe-compiler-walk-dir compiler {:?}) 1) \"kind\"))",
        root.display().to_string(),
        root.display().to_string(),
        root.display().to_string(),
        root.display().to_string(),
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("directory-query-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(2));
    assert_eq!(items[1], RuntimeValue::Str("a.caap".into()));
    assert_eq!(items[2], RuntimeValue::Int(3));
    assert_eq!(items[3], RuntimeValue::Str("dir".into()));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn test_ctfe_compiler_diagnostic_explanation_register_builtin() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse(
        "(ctfe-compiler-diagnostic-explanation-register
          compiler
          \"CAAP-DEMO-001\"
          (map-of
            \"title\" \"Demo error\"
            \"body\" \"This is a demo diagnostic.\"
            \"help\" (list-of \"Fix the demo.\")))",
    )
    .unwrap();
    let unit = Unit::from_graph("diagnostic-explanation-register", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Null);
    let explanation = compiler
        .explanations()
        .explain("CAAP-DEMO-001")
        .unwrap()
        .unwrap();
    assert_eq!(explanation.title, "Demo error");
    assert_eq!(explanation.body, "This is a demo diagnostic.");
    assert_eq!(explanation.help, vec!["Fix the demo."]);
}

#[test]
fn test_ctfe_compiler_register_and_describe_semantic_policy_builtins() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse(
        "(do
          (ctfe-compiler-register-semantic-policy
            compiler
            \"demo-special\"
            (map-of
              \"phase\" \"compile_time\"
              \"eval\" \"special_form\"
              \"control\" \"structured_exit\"
              \"scope\" \"lexical_binding\"
              \"effect\" (list-of \"macro\")
              \"form\" \"control_region\")
            (lambda (form) form))
          (list-of
            (size (ctfe-compiler-list-semantic-policies compiler))
            (get (ctfe-compiler-describe-semantic-policy compiler \"demo-special\") \"name\")
            (get (ctfe-compiler-describe-semantic-policy compiler \"demo-special\") \"phase\")
            (get (ctfe-compiler-describe-semantic-policy compiler \"demo-special\") \"effect\")
            (get (ctfe-compiler-describe-semantic-policy compiler \"demo-special\") \"eval\")
            (get (ctfe-compiler-describe-semantic-policy compiler \"demo-special\") \"control\")
            (get (ctfe-compiler-describe-semantic-policy compiler \"demo-special\") \"scope\")
            (get (ctfe-compiler-describe-semantic-policy compiler \"demo-special\") \"form\")
            (get (ctfe-compiler-describe-semantic-policy compiler \"demo-special\") \"has_normalizer\")))",
    )
    .unwrap();
    let unit = Unit::from_graph("semantic-policy-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1));
    assert_eq!(items[1], RuntimeValue::Str("demo-special".into()));
    assert_eq!(items[2], RuntimeValue::Str("compile_time".into()));
    assert_eq!(items[3], RuntimeValue::Str("macro".into()));
    assert_eq!(items[4], RuntimeValue::Str("special_form".into()));
    assert_eq!(items[5], RuntimeValue::Str("structured_exit".into()));
    assert_eq!(items[6], RuntimeValue::Str("lexical_binding".into()));
    assert_eq!(items[7], RuntimeValue::Str("control_region".into()));
    assert_eq!(items[8], RuntimeValue::Bool(true));
}

#[test]
fn test_ctfe_compiler_fact_schema_and_language_bridge_builtins() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse(
        "(do
          (ctfe-compiler-fact-schema-type-bridge-register compiler \"demo-string\" \"string\")
          (ctfe-compiler-fact-schema-register compiler \"demo.fact\" \"demo-string\" false \"demo fact\")
          (ctfe-compiler-register-python-language-builtin-bridge compiler \"core-special\")
          (ctfe-compiler-lookup-value compiler \"caap.fact_schema.type_bridge.demo-string\"))",
    )
    .unwrap();
    let unit = Unit::from_graph("fact-schema-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Str("string".into()));
    let schema = compiler
        .fact_schema()
        .lookup("demo.fact")
        .unwrap()
        .cloned()
        .expect("expected registered fact schema");
    assert_eq!(schema.type_label, "demo-string");
    assert_eq!(schema.bridge_name, "string");
    assert!(!schema.allow_none);
    assert_eq!(schema.description.as_deref(), Some("demo fact"));
    assert_eq!(compiler.language_builtin_bridges(), vec!["core-special"]);
}

#[test]
fn test_ctfe_compiler_fact_schema_rejects_unknown_bridge_and_wrong_fact_value() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let bad_bridge = Unit::from_graph(
        "bad-fact-schema-bridge",
        parse("(ctfe-compiler-fact-schema-type-bridge-register compiler \"bad\" \"missing\")")
            .unwrap(),
    )
    .unwrap();
    let error = compiler
        .evaluation()
        .evaluate(&bad_bridge, caap_core_port::PhasePolicy::CompileTime, [])
        .expect_err("unknown bridge should fail");
    assert!(format!("{error}").contains("unknown Python fact schema type bridge"));

    let setup = Unit::from_graph(
        "fact-schema-provider-validation",
        parse(
            "(do
              (ctfe-compiler-stage-register compiler \"compile_unit\")
              (ctfe-compiler-fact-schema-type-bridge-register compiler \"demo-string\" \"string\")
              (ctfe-compiler-fact-schema-register compiler \"demo.fact\" \"demo-string\")
              (ctfe-compiler-provider-register
                compiler
                \"bad-fact-provider\"
                \"compile_unit\"
                (lambda (compiler unit ctx)
                  (bind root (ctfe-unit-top-level-form-at unit 0)
                    (ctfe-provider-fact-set ctx \"demo.fact\" root 42)))
                null
                (list-of \"write-facts\")))",
        )
        .unwrap(),
    )
    .unwrap();
    compiler
        .evaluation()
        .evaluate(&setup, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();
    let mut source_unit = Unit::from_graph("fact-schema-source", parse("x").unwrap()).unwrap();
    let error = compiler
        .queries()
        .query(
            "compile_unit",
            &mut source_unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .expect_err("schema mismatch should fail provider query");
    assert!(error.contains("expects value compatible with schema type"));
}

#[test]
fn test_compiler_catalog_reads_registered_units_without_module_resolution() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let unit = Unit::from_graph("main", graph).unwrap();

    compiler.register_unit(unit).unwrap();
    let catalog = compiler.catalog();

    assert!(catalog.contains_unit("main"));
    assert!(catalog.get_compiled_unit("main").unwrap().is_some());
    assert!(catalog.get_compiled_unit("missing").unwrap().is_none());
    assert_eq!(catalog.unit_ids(), vec!["main"]);
}

#[test]
fn test_compiler_module_catalog_records_generic_materialization() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let unit = Unit::from_graph("main-unit", graph).unwrap();
    compiler.register_unit(unit).unwrap();

    compiler
        .record_module_materialization(
            "demo.main",
            "main-unit",
            Some("/workspace/main.caap".to_string()),
        )
        .unwrap();

    let catalog = compiler.module_catalog();
    assert_eq!(catalog.module_names(), vec!["demo.main"]);
    assert_eq!(
        catalog.unit_id_for_module("demo.main").unwrap(),
        Some("main-unit")
    );
    let materialization = catalog.get("demo.main").unwrap().unwrap();
    assert_eq!(
        materialization.source_path.as_deref(),
        Some("/workspace/main.caap")
    );
    assert_eq!(materialization.catalog_version, catalog.version());
    assert_eq!(
        compiler
            .events()
            .by_kind("compiler.module.materialize")
            .unwrap()[0]
            .target
            .as_deref(),
        Some("demo.main")
    );
}

#[test]
fn test_compiler_module_catalog_requires_registered_unit() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();

    let err = compiler
        .record_module_materialization("demo.missing", "missing-unit", None)
        .expect_err("module materialization should require an existing unit");

    assert!(err.contains("unit is not registered"));
    assert!(compiler.module_catalog().module_names().is_empty());
}

#[test]
fn test_source_module_name_discovers_top_level_module_declaration() {
    assert_eq!(
        caap_core_port::source_module_name(
            r#"
              (module "demo.package")
              (int-add 1 2)
            "#,
        )
        .unwrap(),
        "demo.package"
    );
    assert!(caap_core_port::source_module_name("(int-add 1 2)")
        .unwrap_err()
        .contains("is missing module declaration"));
    assert!(caap_core_port::source_module_name("(module)")
        .unwrap_err()
        .contains("module declaration requires a name"));
    assert!(caap_core_port::source_module_name("(module demo)")
        .unwrap_err()
        .contains("module declaration requires a name"));
    assert!(caap_core_port::source_module_name(
        r#"
              (int-add 1 2)
              (module "demo.package")
            "#,
    )
    .unwrap_err()
    .contains("package declaration after implementation body"));
}

#[test]
fn test_parse_package_declarations_matches_python_seed_descriptor_shape() {
    let descriptor = caap_core_port::parse_package_declarations(
        r#"
          (module "demo.pkg")
          (module-capability "host_services")
          (import-namespace "dep.namespace" "dep")
          (import-symbols "dep.symbols" "alpha" "beta")
          (import-as "dep.aliases" "source-name" "local-name")
          (syntax-import "dep.syntax")
          (export-as "exports" "demo.pkg")
          (export "public-a" "public-b")

          (bind ((exports (map-of))) exports)
        "#,
        "/tmp/demo/bootstrap.caap",
    )
    .unwrap();

    assert_eq!(descriptor.name, "demo.pkg");
    assert_eq!(descriptor.index_path, "/tmp/demo/bootstrap.caap");
    assert_eq!(descriptor.base_dir, "/tmp/demo");
    assert_eq!(descriptor.capabilities, vec!["host_services"]);
    assert_eq!(descriptor.declaration_count, 8);
    assert_eq!(descriptor.state, "unloaded");
    assert_eq!(
        descriptor.imports,
        vec![
            caap_core_port::PackageImport {
                module_name: "dep.namespace".to_string(),
                alias: "dep".to_string(),
                symbols: vec![],
                syntax: false,
            },
            caap_core_port::PackageImport {
                module_name: "dep.symbols".to_string(),
                alias: "dep.symbols".to_string(),
                symbols: vec![
                    caap_core_port::PackageImportSymbol {
                        name: "alpha".to_string(),
                        alias: "alpha".to_string(),
                    },
                    caap_core_port::PackageImportSymbol {
                        name: "beta".to_string(),
                        alias: "beta".to_string(),
                    },
                ],
                syntax: false,
            },
            caap_core_port::PackageImport {
                module_name: "dep.aliases".to_string(),
                alias: "dep.aliases".to_string(),
                symbols: vec![caap_core_port::PackageImportSymbol {
                    name: "source-name".to_string(),
                    alias: "local-name".to_string(),
                }],
                syntax: false,
            },
            caap_core_port::PackageImport {
                module_name: "dep.syntax".to_string(),
                alias: "dep.syntax".to_string(),
                symbols: vec![],
                syntax: true,
            },
        ]
    );
    assert_eq!(
        descriptor.syntax_imports,
        vec![caap_core_port::PackageImport {
            module_name: "dep.syntax".to_string(),
            alias: "dep.syntax".to_string(),
            symbols: vec![],
            syntax: true,
        }]
    );
    assert_eq!(
        descriptor.exports,
        vec![
            caap_core_port::PackageExport {
                name: "demo.pkg".to_string(),
                path: Some("exports".to_string()),
                registry: "demo.pkg".to_string(),
            },
            caap_core_port::PackageExport {
                name: "public-a".to_string(),
                path: None,
                registry: "demo.pkg.public-a".to_string(),
            },
            caap_core_port::PackageExport {
                name: "public-b".to_string(),
                path: None,
                registry: "demo.pkg.public-b".to_string(),
            },
        ]
    );
}

#[test]
fn test_parse_package_declarations_or_none_matches_python_callback_boundary() {
    assert_eq!(
        caap_core_port::parse_package_declarations_or_none(
            "(int-add 1 2)\n(module \"late\")",
            "/tmp/demo/bootstrap.caap",
        )
        .unwrap(),
        None
    );

    let descriptor = caap_core_port::parse_package_declarations_or_none(
        r#"
          (module "demo.pkg")
          (import-namespace "dep.one" "one")
          (import-symbols "dep.two" "a")
          (import-namespace "dep.one" "again")
          (syntax-import "dep.syntax")
          (bind ((value 1)) value)
        "#,
        "/tmp/demo/bootstrap.caap",
    )
    .unwrap()
    .unwrap();

    assert_eq!(descriptor.name, "demo.pkg");
    assert_eq!(
        caap_core_port::package_dependency_module_names(&descriptor.imports),
        vec!["dep.one", "dep.two", "dep.syntax"]
    );
    assert_eq!(
        caap_core_port::package_dependency_module_names(&descriptor.syntax_imports),
        vec!["dep.syntax"]
    );
}

#[test]
fn test_parse_package_declarations_or_none_stops_before_imported_surface_body() {
    let descriptor = caap_core_port::parse_package_declarations_or_none(
        r#"
          (module "demo.c_like")
          (syntax-import "demo.c_like.syntax")
          (import-symbols "sys.io" "println")

          auto value = println("hello");
          int main() { return 0; }
        "#,
        "/tmp/demo/bootstrap.caap",
    )
    .unwrap()
    .unwrap();

    assert_eq!(descriptor.name, "demo.c_like");
    assert_eq!(descriptor.declaration_count, 3);
    assert_eq!(
        caap_core_port::package_dependency_module_names(&descriptor.imports),
        vec!["demo.c_like.syntax", "sys.io"]
    );
    assert_eq!(
        caap_core_port::package_dependency_module_names(&descriptor.syntax_imports),
        vec!["demo.c_like.syntax"]
    );
}

#[test]
fn test_compiler_discover_package_file_has_no_materialization_side_effects() {
    let host = caap_core_port::CompilerHost::new();
    let compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-package-discover-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &path,
        r#"
          (module "demo.package")
          (import-namespace "stdlib.sequence" "sequence")
          (export "demo.package")
          (int-add 1 2)
        "#,
    )
    .unwrap();

    let descriptor = compiler.discover_package_file(&path).unwrap();
    std::fs::remove_file(&path).ok();

    assert_eq!(descriptor.name, "demo.package");
    assert_eq!(descriptor.declaration_count, 3);
    assert!(compiler.units().is_empty());
    assert!(compiler.module_catalog().module_names().is_empty());
}

#[test]
fn test_compiler_collects_source_module_and_import_callbacks_without_materializing() {
    let host = caap_core_port::CompilerHost::new();
    let compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-package-callbacks-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &path,
        r#"
          (module "demo.package")
          (import-namespace "stdlib.sequence" "sequence")
          (import-symbols "stdlib.sequence" "length")
          (import-as "stdlib.string" "trim" "trim")
          (syntax-import "stdlib.special-forms")
          (export "demo.package")
          (int-add 1 2)
        "#,
    )
    .unwrap();

    assert_eq!(
        compiler.collect_source_module_name(&path).unwrap(),
        Some("demo.package".to_string())
    );
    assert_eq!(
        compiler.collect_source_imports(&path).unwrap(),
        vec![
            "stdlib.sequence".to_string(),
            "stdlib.string".to_string(),
            "stdlib.special-forms".to_string()
        ]
    );
    assert_eq!(
        compiler.collect_source_syntax_imports(&path).unwrap(),
        vec!["stdlib.special-forms".to_string()]
    );
    std::fs::remove_file(&path).ok();

    assert!(compiler.units().is_empty());
    assert!(compiler.module_catalog().module_names().is_empty());
}

#[test]
fn test_compiler_materialize_package_file_records_declared_module() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-package-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &path,
        r#"
          (module "demo.package")
          (import-namespace "stdlib.sequence" "sequence")
          (export "demo.package")
          (int-add 1 2)
        "#,
    )
    .unwrap();

    let materialization = compiler.materialize_package_file(&path).unwrap();
    std::fs::remove_file(&path).ok();

    assert_eq!(materialization.module_name, "demo.package");
    assert_eq!(materialization.unit_id, "demo.package");
    assert!(compiler.catalog().contains_unit("demo.package"));
    assert_eq!(
        compiler
            .module_catalog()
            .unit_id_for_module("demo.package")
            .unwrap(),
        Some("demo.package")
    );
    assert_eq!(
        compiler
            .events()
            .by_kind("compiler.package.materialize")
            .unwrap()[0]
            .target
            .as_deref(),
        Some("demo.package")
    );
}

#[test]
fn test_compiler_discover_package_file_rejects_missing_module_declaration() {
    let host = caap_core_port::CompilerHost::new();
    let compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-package-missing-module-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(&path, "(int-add 1 2)").unwrap();

    let err = compiler
        .discover_package_file(&path)
        .expect_err("package files should require a module declaration");
    std::fs::remove_file(&path).ok();

    assert!(err.contains("is missing module declaration"));
    assert!(compiler.units().is_empty());
    assert!(compiler.module_catalog().module_names().is_empty());
}

#[test]
fn test_compiler_loads_real_stdlib_module_package_source_without_evaluating_semantics() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/module/bootstrap.caap");

    let materialization = compiler.materialize_package_file(&path).unwrap();

    assert_eq!(materialization.module_name, "stdlib.module");
    assert!(compiler.catalog().contains_unit("stdlib.module"));
    assert_eq!(
        compiler
            .module_catalog()
            .unit_id_for_module("stdlib.module")
            .unwrap(),
        Some("stdlib.module")
    );
}

#[test]
fn test_compiler_evaluation_service_uses_initial_bindings() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add external 2)").unwrap();
    let unit = Unit::from_graph("main", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(
            &unit,
            caap_core_port::PhasePolicy::Runtime,
            [("external".to_string(), RuntimeValue::Int(40))],
        )
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(42));
    assert_eq!(compiler.diagnostics().len(), 0);
}

#[test]
fn test_compiler_evaluation_service_exports_explicit_host_libraries() {
    let mut host = caap_core_port::CompilerHost::new();
    host.register_default_runtime_system_libraries().unwrap();
    let mut compiler = host.new_session();
    let mut builder = GraphBuilder::new();
    let callee = builder.name("path.basename");
    let path = builder.literal(IrLiteralData::Str("/tmp/demo.caap".to_string()));
    let root = builder.call(callee, vec![path]);
    builder.graph.root_id = root;
    builder.graph.add_top_level_form(root).unwrap();
    let unit = Unit::from_graph("main.host-eval", std::mem::take(&mut builder.graph)).unwrap();

    let value = compiler
        .evaluation()
        .evaluate_with_host_libraries(
            &unit,
            caap_core_port::PhasePolicy::Runtime,
            ["path".to_string()],
            [],
        )
        .unwrap();

    assert_eq!(value, RuntimeValue::Str("demo.caap".into()));
}

#[test]
fn test_compiler_evaluation_capture_records_runtime_error_diagnostic() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(runtime-error \"boom\")").unwrap();
    let unit = Unit::from_graph("main", graph).unwrap();

    let capture = compiler
        .evaluation()
        .evaluate_capture(&unit, caap_core_port::PhasePolicy::Runtime, [], 0)
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
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 20 22)").unwrap();
    let unit = Unit::from_graph("main", graph).unwrap();
    compiler.register_unit(unit).unwrap();

    let value = compiler
        .evaluation()
        .evaluate_registered(
            "main",
            caap_core_port::PhasePolicy::Runtime,
            Vec::<(String, RuntimeValue)>::new(),
        )
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(42));
}

#[test]
fn test_compiler_query_service_requires_registered_stages() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    let err = compiler
        .queries()
        .plan_query("compile_unit", caap_core_port::PhasePolicy::CompileTime)
        .expect_err("query planning should require registered stages");

    assert_eq!(err, "no compiler stages registered");
    assert!(compiler.queries().compile(&mut unit).is_err());
}

#[test]
fn test_compiler_query_service_runs_registered_provider() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    compiler.register_stage("compile-unit").unwrap();
    compiler
        .register_provider(
            "mark-compiled",
            "compile_unit",
            caap_core_port::PhasePolicy::CompileTime,
            |_compiler, unit| {
                unit.set_attribute("compiled", caap_core_port::SemanticValue::Bool(true))
            },
        )
        .unwrap();

    let plan = compiler.queries().compile(&mut unit).unwrap();

    assert_eq!(plan.target, "compile_unit");
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].provider_names, vec!["mark-compiled"]);
    assert_eq!(
        unit.attributes().get("compiled"),
        Some(&caap_core_port::SemanticValue::Bool(true))
    );
    assert!(compiler.catalog().contains_unit("main"));
    assert_eq!(compiler.events().by_kind("query.plan").unwrap().len(), 1);
    let provider_event = compiler.events().by_kind("query.provider.finish").unwrap()[0];
    assert_eq!(provider_event.target.as_deref(), Some("mark-compiled"));
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

#[test]
fn test_query_provider_requires_schedule_before_dependents() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let order = Rc::new(std::cell::RefCell::new(Vec::<String>::new()));

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_contract(
            "consumer",
            "analyze",
            Some("analysis".to_string()),
            caap_core_port::PhasePolicy::CompileTime,
            ["producer".to_string()],
            Vec::<String>::new(),
            caap_core_port::QueryProviderRegistrationSpec::new(),
            {
                let order = Rc::clone(&order);
                move |_compiler, _unit| {
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
            caap_core_port::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            Vec::<String>::new(),
            caap_core_port::QueryProviderRegistrationSpec::new(),
            {
                let order = Rc::clone(&order);
                move |_compiler, _unit| {
                    order.borrow_mut().push("producer".to_string());
                    Ok(())
                }
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
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
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let ran = Rc::new(std::cell::Cell::new(false));

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_contract(
            "consumer",
            "analyze",
            Some("analysis".to_string()),
            caap_core_port::PhasePolicy::CompileTime,
            ["missing".to_string()],
            Vec::<String>::new(),
            caap_core_port::QueryProviderRegistrationSpec::new(),
            {
                let ran = Rc::clone(&ran);
                move |_compiler, _unit| {
                    ran.set(true);
                    Ok(())
                }
            },
        )
        .unwrap();

    let err = compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .expect_err("missing provider requirement should fail planning");

    assert!(err.contains("requires missing provider"));
    assert!(!ran.get());
}

#[test]
fn test_query_provider_receives_active_context_only_during_callback() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let captured = Rc::new(std::cell::RefCell::new(None));

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider(
            "context-provider",
            "analyze",
            caap_core_port::PhasePolicy::CompileTime,
            {
                let captured = Rc::clone(&captured);
                move |compiler, unit| {
                    *captured.borrow_mut() = compiler.active_provider_context().cloned();
                    unit.set_attribute("context-seen", caap_core_port::SemanticValue::Bool(true))
                }
            },
        )
        .unwrap();

    compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();

    let context = captured
        .borrow()
        .clone()
        .expect("provider context should be visible during callback");
    assert_eq!(context.provider, "context-provider");
    assert_eq!(context.stage, "analyze");
    assert_eq!(context.phase, caap_core_port::PhasePolicy::CompileTime);
    assert_eq!(context.unit_id, "main");
    assert_eq!(context.registration_index, 0);
    assert!(compiler.active_provider_context().is_none());
    assert_eq!(
        unit.attributes().get("context-seen"),
        Some(&caap_core_port::SemanticValue::Bool(true))
    );
}

#[test]
fn test_query_provider_context_is_restored_after_error() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider(
            "failing-provider",
            "analyze",
            caap_core_port::PhasePolicy::CompileTime,
            |compiler, _unit| {
                assert_eq!(
                    compiler
                        .active_provider_context()
                        .map(|context| context.provider.as_str()),
                    Some("failing-provider")
                );
                Err("provider failed".to_string())
            },
        )
        .unwrap();

    let err = compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .expect_err("query should propagate provider failure");

    assert_eq!(err, "provider failed");
    assert!(compiler.active_provider_context().is_none());
}

#[test]
fn test_query_mutating_provider_rolls_back_unit_on_error() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    compiler.register_stage("analyze").unwrap();
    let mut spec = caap_core_port::QueryProviderRegistrationSpec::new();
    spec.writes = vec!["attributes".to_string()];
    compiler
        .register_provider_contract(
            "failing-mutator",
            "analyze",
            None,
            caap_core_port::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            Vec::<String>::new(),
            spec,
            |_compiler, unit| {
                unit.set_attribute("partial", caap_core_port::SemanticValue::Bool(true))?;
                Err("provider failed after mutation".to_string())
            },
        )
        .unwrap();

    let err = compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .expect_err("query should propagate provider failure");

    assert_eq!(err, "provider failed after mutation");
    assert!(unit.attributes().get("partial").is_none());
    assert!(compiler.active_provider_context().is_none());
}

#[test]
fn test_query_provider_declared_fact_write_uses_semantic_transaction() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let subject = caap_core_port::subject_id("demo", "fact").unwrap();
    let mut spec = caap_core_port::QueryProviderRegistrationSpec::new();
    spec.writes = vec!["facts".to_string()];

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_contract(
            "failing-fact-writer",
            "analyze",
            None,
            caap_core_port::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            Vec::<String>::new(),
            spec,
            {
                let subject = subject.clone();
                move |_compiler, unit| {
                    unit.semantics_mut().set_fact(
                        subject.clone(),
                        "value",
                        caap_core_port::SemanticValue::Int(1),
                    )?;
                    Err("provider failed after fact write".to_string())
                }
            },
        )
        .unwrap();

    let err = compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .expect_err("query should propagate provider failure");

    assert_eq!(err, "provider failed after fact write");
    assert!(unit
        .semantics()
        .get_fact(&subject, "value")
        .unwrap()
        .is_none());
}

#[test]
fn test_query_error_diagnostic_rolls_back_and_stops_pipeline() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let after_runs = Rc::new(std::cell::Cell::new(0));
    let after_runs_provider = after_runs.clone();

    compiler.register_stage("analyze").unwrap();
    let mut spec = caap_core_port::QueryProviderRegistrationSpec::new();
    spec.writes = vec!["attributes".to_string()];
    compiler
        .register_provider_contract(
            "diagnostic-mutator",
            "analyze",
            None,
            caap_core_port::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            Vec::<String>::new(),
            spec,
            |_compiler, unit| {
                unit.set_attribute("partial", caap_core_port::SemanticValue::Bool(true))?;
                _compiler.push_diagnostic(
                    caap_core_port::Diagnostic::error("provider emitted an error")
                        .unwrap()
                        .with_code("demo.provider.error")
                        .unwrap(),
                );
                Ok(())
            },
        )
        .unwrap();
    compiler
        .register_provider(
            "after-error-provider",
            "analyze",
            caap_core_port::PhasePolicy::CompileTime,
            move |_compiler, _unit| {
                after_runs_provider.set(after_runs_provider.get() + 1);
                Ok(())
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();

    assert_eq!(compiler.diagnostics().len(), 1);
    assert!(unit.attributes().get("partial").is_none());
    assert_eq!(after_runs.get(), 0);
    assert_eq!(plan.executed.len(), 1);
    assert_eq!(plan.executed[0].provider_name, "diagnostic-mutator");
    assert!(plan.executed[0].rolled_back);
    assert!(plan.executed[0].stopped_by_error);
    assert_eq!(plan.executed[0].outcome_kind, "stopped_by_error");
    assert_eq!(
        plan.executed[0].diagnostic_codes,
        vec!["demo.provider.error".to_string()]
    );
}

#[test]
fn test_query_atomic_transaction_rolls_back_unit_and_cache_on_error() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    compiler
        .register_stage_spec(caap_core_port::QueryStageSpec::new("lower").unwrap())
        .unwrap();
    compiler
        .register_stage_spec(
            caap_core_port::QueryStageSpec::new("validate")
                .unwrap()
                .with_requires(["lower".to_string()])
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_provider(
            "lower-provider",
            "lower",
            caap_core_port::PhasePolicy::CompileTime,
            |_compiler, unit| {
                unit.set_attribute("lowered", caap_core_port::SemanticValue::Bool(true))
            },
        )
        .unwrap();
    compiler
        .register_provider(
            "validate-provider",
            "validate",
            caap_core_port::PhasePolicy::CompileTime,
            |_compiler, unit| {
                unit.set_attribute("validated", caap_core_port::SemanticValue::Bool(false))?;
                Err("validation failed".to_string())
            },
        )
        .unwrap();

    let err = compiler
        .queries()
        .query_with_transaction_mode(
            "validate",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
            caap_core_port::QueryTransactionMode::AtomicUnit,
        )
        .expect_err("atomic query should propagate provider failure");

    assert_eq!(err, "validation failed");
    assert!(unit.attributes().get("lowered").is_none());
    assert!(unit.attributes().get("validated").is_none());
    assert_eq!(compiler.artifact_cache().stats().generation, 0);
    assert!(compiler.active_provider_context().is_none());
    assert!(!compiler.catalog().contains_unit("main"));
}

#[test]
fn test_query_stage_alias_resolves_to_registered_stage() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    compiler.register_stage("compile_unit").unwrap();
    compiler
        .register_stage_alias("compile_unit", "compile")
        .unwrap();
    compiler
        .register_provider(
            "alias-provider",
            "compile",
            caap_core_port::PhasePolicy::CompileTime,
            |_compiler, unit| {
                unit.set_attribute("alias-ran", caap_core_port::SemanticValue::Bool(true))
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query(
            "compile",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();

    assert_eq!(plan.target, "compile_unit");
    assert_eq!(plan.steps[0].provider_names, vec!["alias-provider"]);
    assert_eq!(
        unit.attributes().get("alias-ran"),
        Some(&caap_core_port::SemanticValue::Bool(true))
    );
}

#[test]
fn test_query_plan_routes_stage_dependencies_before_target() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    compiler
        .register_stage_spec(caap_core_port::QueryStageSpec::new("lower").unwrap())
        .unwrap();
    compiler
        .register_stage_spec(
            caap_core_port::QueryStageSpec::new("validate")
                .unwrap()
                .with_requires(["lower".to_string()])
                .unwrap()
                .with_aliases(["compile".to_string()])
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_provider(
            "lower-provider",
            "lower",
            caap_core_port::PhasePolicy::CompileTime,
            |_compiler, unit| {
                unit.set_attribute("lowered", caap_core_port::SemanticValue::Bool(true))
            },
        )
        .unwrap();
    compiler
        .register_provider(
            "validate-provider",
            "validate",
            caap_core_port::PhasePolicy::CompileTime,
            |_compiler, unit| {
                assert_eq!(
                    unit.attributes().get("lowered"),
                    Some(&caap_core_port::SemanticValue::Bool(true))
                );
                unit.set_attribute("validated", caap_core_port::SemanticValue::Bool(true))
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query(
            "compile",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();

    assert_eq!(plan.target, "validate");
    assert_eq!(
        plan.steps
            .iter()
            .map(|step| step.stage.as_str())
            .collect::<Vec<_>>(),
        vec!["lower", "validate"]
    );
    assert_eq!(plan.steps[0].provider_names, vec!["lower-provider"]);
    assert_eq!(plan.steps[1].provider_names, vec!["validate-provider"]);
    assert_eq!(
        unit.attributes().get("validated"),
        Some(&caap_core_port::SemanticValue::Bool(true))
    );
}

#[test]
fn test_query_provider_can_request_bounded_restart_from_stage() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let validate_runs = Rc::new(std::cell::Cell::new(0));

    compiler
        .register_stage_spec(caap_core_port::QueryStageSpec::new("lower").unwrap())
        .unwrap();
    compiler
        .register_stage_spec(
            caap_core_port::QueryStageSpec::new("validate")
                .unwrap()
                .with_requires(["lower".to_string()])
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_provider(
            "lower-provider",
            "lower",
            caap_core_port::PhasePolicy::CompileTime,
            |_compiler, unit| {
                unit.set_attribute("lowered", caap_core_port::SemanticValue::Bool(true))
            },
        )
        .unwrap();
    compiler
        .register_provider(
            "validate-provider",
            "validate",
            caap_core_port::PhasePolicy::CompileTime,
            {
                let validate_runs = Rc::clone(&validate_runs);
                move |compiler, unit| {
                    validate_runs.set(validate_runs.get() + 1);
                    if validate_runs.get() == 1 {
                        compiler.request_query_restart("lower")?;
                    }
                    unit.set_attribute("validated", caap_core_port::SemanticValue::Bool(true))
                }
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query(
            "validate",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
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
fn test_query_changed_provider_uses_stage_restart_policy() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let validate_runs = Rc::new(std::cell::Cell::new(0));

    compiler
        .register_stage_spec(caap_core_port::QueryStageSpec::new("lower").unwrap())
        .unwrap();
    compiler
        .register_stage_spec(
            caap_core_port::QueryStageSpec::new("validate")
                .unwrap()
                .with_requires(["lower".to_string()])
                .unwrap()
                .with_restart_stage("lower")
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_provider(
            "lower-provider",
            "lower",
            caap_core_port::PhasePolicy::CompileTime,
            |_compiler, _unit| Ok(()),
        )
        .unwrap();
    compiler
        .register_provider_contract_with_outcome(
            "validate-provider",
            "validate",
            None,
            caap_core_port::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            Vec::<String>::new(),
            caap_core_port::QueryProviderRegistrationSpec::default(),
            {
                let validate_runs = Rc::clone(&validate_runs);
                move |_compiler, unit| {
                    validate_runs.set(validate_runs.get() + 1);
                    if validate_runs.get() == 1 {
                        unit.set_attribute("validated", caap_core_port::SemanticValue::Bool(true))?;
                        Ok(caap_core_port::QueryProviderCallbackOutcome::changed(true))
                    } else {
                        Ok(caap_core_port::QueryProviderCallbackOutcome::changed(false))
                    }
                }
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query(
            "validate",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
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
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let spec = caap_core_port::QueryProviderRegistrationSpec {
        resume_policy: "never".to_string(),
        ..Default::default()
    };

    compiler
        .register_stage_spec(caap_core_port::QueryStageSpec::new("lower").unwrap())
        .unwrap();
    compiler
        .register_stage_spec(
            caap_core_port::QueryStageSpec::new("validate")
                .unwrap()
                .with_requires(["lower".to_string()])
                .unwrap()
                .with_restart_stage("lower")
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_provider(
            "lower-provider",
            "lower",
            caap_core_port::PhasePolicy::CompileTime,
            |_compiler, _unit| Ok(()),
        )
        .unwrap();
    compiler
        .register_provider_contract(
            "validate-provider",
            "validate",
            None,
            caap_core_port::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            Vec::<String>::new(),
            spec,
            |_compiler, unit| {
                unit.set_attribute("validated", caap_core_port::SemanticValue::Bool(true))
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query(
            "validate",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
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
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let validate_runs = Rc::new(std::cell::Cell::new(0));

    compiler
        .register_stage_spec(caap_core_port::QueryStageSpec::new("lower").unwrap())
        .unwrap();
    compiler
        .register_stage_spec(
            caap_core_port::QueryStageSpec::new("validate")
                .unwrap()
                .with_requires(["lower".to_string()])
                .unwrap(),
        )
        .unwrap();
    compiler
        .register_provider(
            "lower-provider",
            "lower",
            caap_core_port::PhasePolicy::CompileTime,
            |_compiler, unit| {
                unit.set_attribute("lowered", caap_core_port::SemanticValue::Bool(true))
            },
        )
        .unwrap();
    compiler
        .register_provider(
            "validate-provider",
            "validate",
            caap_core_port::PhasePolicy::CompileTime,
            {
                let validate_runs = Rc::clone(&validate_runs);
                move |compiler, unit| {
                    validate_runs.set(validate_runs.get() + 1);
                    compiler.request_query_restart("lower")?;
                    unit.set_attribute("validated", caap_core_port::SemanticValue::Bool(true))
                }
            },
        )
        .unwrap();

    let err = compiler
        .queries()
        .query_with_options(
            "validate",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
            caap_core_port::QueryExecutionOptions::new().with_restart_limit(0),
        )
        .expect_err("disabled restart budget should reject provider restart requests");

    assert!(err.contains("query restart budget exhausted"));
    assert_eq!(validate_runs.get(), 1);
    assert_eq!(
        unit.attributes().get("validated"),
        Some(&caap_core_port::SemanticValue::Bool(true))
    );
}

#[test]
fn test_query_plan_reports_missing_stage_dependency() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    compiler
        .register_stage_spec(
            caap_core_port::QueryStageSpec::new("validate")
                .unwrap()
                .with_requires(["lower".to_string()])
                .unwrap(),
        )
        .unwrap();

    let err = compiler
        .queries()
        .plan_query("validate", caap_core_port::PhasePolicy::CompileTime)
        .expect_err("missing dependency should fail planning");

    assert!(err.contains("depends on missing stage"));
}

#[test]
fn test_query_provider_effect_tags_are_planned_and_exposed_in_context() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_with_effects(
            "effect-provider",
            "analyze",
            caap_core_port::PhasePolicy::CompileTime,
            ["reads-source".to_string(), "writes-semantics".to_string()],
            |compiler, unit| {
                let context = compiler
                    .active_provider_context()
                    .expect("provider context should be active");
                assert_eq!(
                    context.effect_tags,
                    vec!["reads_source".to_string(), "writes_semantics".to_string()]
                );
                unit.set_attribute("effect-provider", caap_core_port::SemanticValue::Bool(true))
            },
        )
        .unwrap();

    let plan = compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();

    assert_eq!(
        plan.steps[0].effect_tags,
        vec!["reads_source".to_string(), "writes_semantics".to_string()]
    );
    let artifact_key = plan.steps[0].artifact_key.as_ref().unwrap();
    let artifact = compiler.artifact_cache().peek(artifact_key).unwrap();
    let caap_core_port::ArtifactValue::Semantic(caap_core_port::SemanticValue::Map(entries)) =
        artifact
    else {
        panic!("expected semantic query artifact");
    };
    assert!(entries.contains(&(
        "effect_tags".to_string(),
        caap_core_port::SemanticValue::List(vec![
            caap_core_port::SemanticValue::Str("reads_source".to_string()),
            caap_core_port::SemanticValue::Str("writes_semantics".to_string()),
        ])
    )));
}

#[test]
fn test_query_execution_options_enforce_effect_allowlist_before_running_provider() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_with_effects(
            "effect-provider",
            "analyze",
            caap_core_port::PhasePolicy::CompileTime,
            ["writes-semantics".to_string()],
            {
                let runs = Rc::clone(&runs);
                move |_compiler, unit| {
                    runs.set(runs.get() + 1);
                    unit.set_attribute("ran", caap_core_port::SemanticValue::Bool(true))
                }
            },
        )
        .unwrap();

    let err = compiler
        .queries()
        .query_with_options(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
            caap_core_port::QueryExecutionOptions::new()
                .with_allowed_effect_tags(["reads-source".to_string()]),
        )
        .expect_err("effect allowlist should reject disallowed provider effects");

    assert!(err.contains("not allowed"));
    assert_eq!(runs.get(), 0);
    assert!(unit.attributes().get("ran").is_none());
}

#[test]
fn test_query_service_replays_exact_stage_cache_without_provider_rerun() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider(
            "count-provider",
            "analyze",
            caap_core_port::PhasePolicy::CompileTime,
            {
                let runs = Rc::clone(&runs);
                move |_compiler, _unit| {
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
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();
    let second = compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
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
    let caap_core_port::ArtifactValue::Semantic(caap_core_port::SemanticValue::Map(entries)) =
        artifact
    else {
        panic!("expected semantic query artifact");
    };
    assert!(entries.contains(&(
        "provider_count".to_string(),
        caap_core_port::SemanticValue::Int(1)
    )));
    assert!(entries.contains(&(
        "providers".to_string(),
        caap_core_port::SemanticValue::List(vec![caap_core_port::SemanticValue::Str(
            "count-provider".to_string()
        )])
    )));
    assert!(entries.contains(&(
        "artifact_key".to_string(),
        caap_core_port::SemanticValue::Str(artifact_key.to_string())
    )));
    assert_eq!(compiler.artifact_cache().stats().hits, 1);
    assert_eq!(
        compiler.events().by_kind("query.stage.cache-hit").unwrap()[0]
            .target
            .as_deref(),
        Some("analyze")
    );
}

#[test]
fn test_query_stage_cache_key_tracks_provider_registry_version() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let first_runs = Rc::new(std::cell::Cell::new(0));
    let second_runs = Rc::new(std::cell::Cell::new(0));

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider(
            "first-provider",
            "analyze",
            caap_core_port::PhasePolicy::CompileTime,
            {
                let first_runs = Rc::clone(&first_runs);
                move |_compiler, _unit| {
                    first_runs.set(first_runs.get() + 1);
                    Ok(())
                }
            },
        )
        .unwrap();

    let first = compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();

    compiler
        .register_provider(
            "second-provider",
            "analyze",
            caap_core_port::PhasePolicy::CompileTime,
            {
                let second_runs = Rc::clone(&second_runs);
                move |_compiler, _unit| {
                    second_runs.set(second_runs.get() + 1);
                    Ok(())
                }
            },
        )
        .unwrap();

    let second = compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();

    assert_eq!(first_runs.get(), 2);
    assert_eq!(second_runs.get(), 1);
    assert!(!first.steps[0].cached);
    assert!(!second.steps[0].cached);
    assert_ne!(first.steps[0].artifact_key, second.steps[0].artifact_key);
    assert_eq!(
        second.steps[0].provider_names,
        vec!["first-provider".to_string(), "second-provider".to_string()]
    );
}

#[test]
fn test_query_service_replays_provider_ctfe_cache_when_stage_artifact_is_dirty() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));
    let provider_runs = runs.clone();

    compiler.register_stage("analyze").unwrap();
    let mut spec = caap_core_port::QueryProviderRegistrationSpec::new();
    spec.cache_scope = "unit".to_string();
    compiler
        .register_provider_contract(
            "cacheable-provider",
            "analyze",
            None,
            caap_core_port::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            Vec::<String>::new(),
            spec,
            move |_, _| {
                provider_runs.set(provider_runs.get() + 1);
                Ok(())
            },
        )
        .unwrap();

    let first = compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();
    let artifact_key = first.steps[0].artifact_key.as_ref().unwrap().clone();
    compiler.artifact_cache_mut().mark_dirty(
        caap_core_port::ArtifactInvalidationRecord::new("force-stage-cache-miss", artifact_key)
            .unwrap(),
    );

    let second = compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();

    assert_eq!(runs.get(), 1);
    assert_eq!(second.executed.len(), 1);
    assert_eq!(second.executed[0].provider_name, "cacheable-provider");
    assert_eq!(second.executed[0].outcome_kind, "cached");
    assert_eq!(
        compiler
            .events()
            .by_kind("query.provider.cache-hit")
            .unwrap()[0]
            .target
            .as_deref(),
        Some("cacheable-provider")
    );
}

#[test]
fn test_query_service_provider_ctfe_cache_replays_unit_snapshot() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut first_unit = Unit::from_graph("main", graph.clone()).unwrap();
    let mut second_unit = Unit::from_graph("main", graph).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));
    let provider_runs = runs.clone();

    compiler.register_stage("analyze").unwrap();
    let mut spec = caap_core_port::QueryProviderRegistrationSpec::new();
    spec.cache_scope = "unit".to_string();
    spec.writes = vec!["attributes".to_string()];
    compiler
        .register_provider_contract(
            "snapshot-provider",
            "analyze",
            None,
            caap_core_port::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            Vec::<String>::new(),
            spec,
            move |_, unit| {
                provider_runs.set(provider_runs.get() + 1);
                unit.set_attribute("from-provider", caap_core_port::SemanticValue::Int(42))
            },
        )
        .unwrap();

    let first = compiler
        .queries()
        .query(
            "analyze",
            &mut first_unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();
    let artifact_key = first.steps[0].artifact_key.as_ref().unwrap().clone();
    compiler.artifact_cache_mut().mark_dirty(
        caap_core_port::ArtifactInvalidationRecord::new("force-stage-cache-miss", artifact_key)
            .unwrap(),
    );

    let second = compiler
        .queries()
        .query(
            "analyze",
            &mut second_unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();

    assert_eq!(runs.get(), 1);
    assert_eq!(second.executed[0].outcome_kind, "cached");
    assert_eq!(
        second_unit.attributes().get("from-provider"),
        Some(&caap_core_port::SemanticValue::Int(42))
    );
}

#[test]
fn test_query_service_read_only_provider_cache_does_not_replay_unit_snapshot() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut first_unit = Unit::from_graph("main", graph.clone()).unwrap();
    let mut second_unit = Unit::from_graph("main", graph).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));
    let provider_runs = runs.clone();
    let mut spec = caap_core_port::QueryProviderRegistrationSpec::new();
    spec.cache_scope = "unit".to_string();

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_contract_with_outcome(
            "reported-change-readonly-provider",
            "analyze",
            None,
            caap_core_port::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            Vec::<String>::new(),
            spec,
            move |_compiler, unit| {
                provider_runs.set(provider_runs.get() + 1);
                unit.set_attribute(
                    "should-not-replay",
                    caap_core_port::SemanticValue::Bool(true),
                )?;
                Ok(caap_core_port::QueryProviderCallbackOutcome::changed(true))
            },
        )
        .unwrap();

    let first = compiler
        .queries()
        .query(
            "analyze",
            &mut first_unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();
    let artifact_key = first.steps[0].artifact_key.as_ref().unwrap().clone();
    compiler.artifact_cache_mut().mark_dirty(
        caap_core_port::ArtifactInvalidationRecord::new("force-stage-cache-miss", artifact_key)
            .unwrap(),
    );

    let second = compiler
        .queries()
        .query(
            "analyze",
            &mut second_unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();

    assert_eq!(runs.get(), 1);
    assert_eq!(second.executed[0].outcome_kind, "cached");
    assert!(second.executed[0].changed);
    assert!(first_unit.attributes().contains_key("should-not-replay"));
    assert!(second_unit.attributes().get("should-not-replay").is_none());
}

#[test]
fn test_query_service_file_reading_provider_is_not_ctfe_cached() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));
    let provider_runs = runs.clone();
    let mut spec = caap_core_port::QueryProviderRegistrationSpec::new();
    spec.cache_scope = "unit".to_string();
    spec.reads = vec!["files".to_string()];

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider_contract(
            "file-reading-provider",
            "analyze",
            None,
            caap_core_port::PhasePolicy::CompileTime,
            Vec::<String>::new(),
            vec!["read-files".to_string()],
            spec,
            move |_compiler, _unit| {
                provider_runs.set(provider_runs.get() + 1);
                Ok(())
            },
        )
        .unwrap();

    let first = compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();
    let artifact_key = first.steps[0].artifact_key.as_ref().unwrap().clone();
    compiler.artifact_cache_mut().mark_dirty(
        caap_core_port::ArtifactInvalidationRecord::new("force-stage-cache-miss", artifact_key)
            .unwrap(),
    );

    let second = compiler
        .queries()
        .query(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
        )
        .unwrap();

    assert_eq!(runs.get(), 2);
    assert_eq!(second.executed[0].provider_name, "file-reading-provider");
    assert_ne!(second.executed[0].outcome_kind, "cached");
}

#[test]
fn test_query_stage_cache_key_tracks_initial_bindings() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let graph = parse("(int-add 1 2)").unwrap();
    let mut unit = Unit::from_graph("initial-cache", graph).unwrap();
    let runs = Rc::new(std::cell::Cell::new(0));

    compiler.register_stage("analyze").unwrap();
    compiler
        .register_provider(
            "count-initial-provider",
            "analyze",
            caap_core_port::PhasePolicy::CompileTime,
            {
                let runs = Rc::clone(&runs);
                move |_compiler, _unit| {
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
            caap_core_port::PhasePolicy::CompileTime,
            caap_core_port::QueryExecutionOptions::new()
                .with_initial_bindings([("env".to_string(), RuntimeValue::Str("one".into()))]),
        )
        .unwrap();
    let second = compiler
        .queries()
        .query_with_options(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
            caap_core_port::QueryExecutionOptions::new()
                .with_initial_bindings([("env".to_string(), RuntimeValue::Str("two".into()))]),
        )
        .unwrap();
    let third = compiler
        .queries()
        .query_with_options(
            "analyze",
            &mut unit,
            caap_core_port::PhasePolicy::CompileTime,
            caap_core_port::QueryExecutionOptions::new()
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

#[test]
fn test_bootstrap_execute_text_is_explicit_and_records_trace() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();

    assert!(!compiler.has_bootstrap_executions());
    let value = compiler
        .bootstrap()
        .execute_text("(int-add 1 2)", "stdlib.bootstrap")
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(3));
    assert!(compiler.has_bootstrap_executions());
    assert_eq!(
        compiler.bootstrap_executions(),
        &["<inline:stdlib.bootstrap>".to_string()]
    );
    assert!(compiler.catalog().contains_unit("stdlib.bootstrap"));
    assert_eq!(compiler.bootstrap_trace().len(), 1);
    assert_eq!(compiler.bootstrap_trace()[0].action, "bootstrap.raw");
    assert!(compiler.bootstrap_trace()[0].succeeded);
    let event = compiler.events().by_kind("bootstrap.execute").unwrap()[0];
    assert_eq!(event.target.as_deref(), Some("stdlib.bootstrap"));
    assert!(event
        .metadata
        .contains(&("succeeded".to_string(), "true".to_string())));
    assert!(event
        .metadata
        .iter()
        .any(|(key, value)| key == "elapsed_ms" && value.parse::<f64>().is_ok()));
}

#[test]
fn test_bootstrap_execute_text_reports_parse_failure_without_implicit_stage_registration() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();

    let err = compiler
        .bootstrap()
        .execute_text("(", "broken.bootstrap")
        .expect_err("invalid bootstrap source should fail");

    assert!(!format!("{err:?}").is_empty());
    assert!(compiler.registered_stages().is_empty());
    assert_eq!(compiler.bootstrap_trace().len(), 1);
    assert!(!compiler.bootstrap_trace()[0].succeeded);
}

#[test]
fn test_bootstrap_execute_virtual_file_uses_explicit_vfs_source() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let mut vfs = caap_core_port::BootstrapVirtualFileSystem::new();
    vfs.insert("/stdlib/bootstrap.caap", "(int-add 5 6)")
        .unwrap();

    let value = compiler
        .bootstrap()
        .execute_virtual_file(&vfs, "stdlib/bootstrap.caap", "stdlib.vfs")
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(11));
    assert_eq!(
        compiler.bootstrap_executions(),
        &["<vfs:stdlib/bootstrap.caap>".to_string()]
    );
    assert!(compiler.catalog().contains_unit("stdlib.vfs"));
    assert_eq!(compiler.bootstrap_trace().len(), 1);
    assert_eq!(compiler.bootstrap_trace()[0].action, "bootstrap.vfs");
    assert_eq!(
        compiler.bootstrap_trace()[0].target,
        "vfs:stdlib/bootstrap.caap"
    );
    assert!(compiler.bootstrap_trace()[0].succeeded);
    assert!(vfs.contains("stdlib/bootstrap.caap"));
    assert_eq!(vfs.paths(), vec!["stdlib/bootstrap.caap"]);
}

#[test]
fn test_bootstrap_execute_virtual_file_reports_missing_source() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let vfs = caap_core_port::BootstrapVirtualFileSystem::new();

    let err = compiler
        .bootstrap()
        .execute_virtual_file(&vfs, "stdlib/missing.caap", "stdlib.missing")
        .expect_err("missing virtual bootstrap source should fail");

    assert!(format!("{err:?}").contains("virtual bootstrap file does not exist"));
    assert!(!compiler.has_bootstrap_executions());
    assert_eq!(compiler.bootstrap_trace().len(), 1);
    assert_eq!(compiler.bootstrap_trace()[0].action, "bootstrap.vfs");
    assert!(!compiler.bootstrap_trace()[0].succeeded);
}

#[test]
fn test_bootstrap_capability_graph_is_explicit_and_queryable() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();

    assert_eq!(compiler.bootstrap_capabilities().version(), 0);
    assert!(!compiler
        .bootstrap_capabilities()
        .allows("stdlib.fs", "fs.read-text"));

    compiler
        .bootstrap()
        .grant_capabilities(
            "stdlib.fs",
            ["fs.read-text".to_string(), "path.*".to_string()],
        )
        .unwrap();

    assert!(compiler
        .bootstrap()
        .require_capability("stdlib.fs", "fs.read-text")
        .is_ok());
    assert!(compiler
        .bootstrap_capabilities()
        .allows("stdlib.fs", "path.basename"));
    assert!(!compiler
        .bootstrap_capabilities()
        .allows("stdlib.fs", "process.args"));
    assert_eq!(
        compiler
            .bootstrap_capabilities()
            .capabilities_for("stdlib.fs"),
        vec!["fs.read-text", "path.*"]
    );
    assert_eq!(
        compiler.bootstrap_capabilities().unit_ids(),
        vec!["stdlib.fs"]
    );
}

#[test]
fn test_bootstrap_execute_text_with_capabilities_records_grants() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();

    let value = compiler
        .bootstrap()
        .execute_text_with_capabilities(
            "(int-add 2 8)",
            "stdlib.cap",
            ["fs.*".to_string(), "env.get".to_string()],
        )
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(10));
    assert!(compiler
        .bootstrap_capabilities()
        .allows("stdlib.cap", "fs.write-text"));
    assert!(compiler
        .bootstrap_capabilities()
        .allows("stdlib.cap", "env.get"));
    assert!(compiler.catalog().contains_unit("stdlib.cap"));
}

#[test]
fn test_bootstrap_image_store_snapshots_units_and_capabilities() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();

    compiler
        .bootstrap()
        .execute_text_with_capabilities("(int-add 1 1)", "stdlib.one", ["fs.read-text".to_string()])
        .unwrap();
    let image = compiler.store_bootstrap_image("base").unwrap();
    assert_eq!(image.name, "base");
    assert_eq!(image.unit_ids(), vec!["stdlib.one"]);
    assert!(image.capabilities.allows("stdlib.one", "fs.read-text"));
    assert_eq!(compiler.bootstrap_images().image_names(), vec!["base"]);

    compiler
        .register_unit(Unit::empty("scratch.extra").unwrap())
        .unwrap();
    compiler
        .bootstrap()
        .grant_capability("scratch.extra", "process.args")
        .unwrap();
    assert!(compiler.catalog().contains_unit("scratch.extra"));

    compiler.restore_bootstrap_image("base").unwrap();
    assert!(compiler.catalog().contains_unit("stdlib.one"));
    assert!(!compiler.catalog().contains_unit("scratch.extra"));
    assert!(compiler
        .bootstrap_capabilities()
        .allows("stdlib.one", "fs.read-text"));
    assert!(!compiler
        .bootstrap_capabilities()
        .allows("scratch.extra", "process.args"));
}

#[test]
fn test_bootstrap_image_file_roundtrips_through_compiler_store() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    compiler
        .bootstrap()
        .execute_text_with_capabilities(
            "(int-add 2 3)",
            "stdlib.persisted",
            ["fs.read-text".to_string()],
        )
        .unwrap();
    compiler.store_bootstrap_image("base").unwrap();
    let path = std::env::temp_dir().join(format!(
        "caap-bootstrap-image-{}-{}.json",
        std::process::id(),
        line!()
    ));

    compiler.save_bootstrap_image_file("base", &path).unwrap();
    assert_eq!(
        compiler.events().by_kind("bootstrap.image.save").unwrap()[0]
            .target
            .as_deref(),
        Some("base")
    );

    let mut restored = host.new_session();
    let image_name = restored.load_bootstrap_image_file(&path).unwrap();
    std::fs::remove_file(&path).ok();

    assert_eq!(image_name, "base");
    assert_eq!(restored.bootstrap_images().image_names(), vec!["base"]);
    restored.restore_bootstrap_image("base").unwrap();
    assert!(restored.catalog().contains_unit("stdlib.persisted"));
    assert!(restored
        .bootstrap_capabilities()
        .allows("stdlib.persisted", "fs.read-text"));
    assert_eq!(
        restored.events().by_kind("bootstrap.image.load").unwrap()[0]
            .target
            .as_deref(),
        Some("base")
    );
}

#[test]
fn test_bootstrap_image_persists_compiler_fact_schema_and_language_bridges() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let unit = Unit::from_graph(
        "bootstrap-image-compiler-state",
        parse(
            "(do
              (ctfe-compiler-fact-schema-type-bridge-register compiler \"demo-string\" \"string\")
              (ctfe-compiler-fact-schema-register compiler \"demo.fact\" \"demo-string\" false \"demo fact\")
              (ctfe-compiler-register-python-language-builtin-bridge compiler \"core-special\"))",
        )
        .unwrap(),
    )
    .unwrap();
    compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let image = compiler.store_bootstrap_image("compiler-state").unwrap();
    assert_eq!(
        image
            .fact_schema
            .lookup("demo.fact")
            .unwrap()
            .unwrap()
            .bridge_name,
        "string"
    );
    assert_eq!(image.language_builtin_bridges, vec!["core-special"]);

    let path = std::env::temp_dir().join(format!(
        "caap-bootstrap-image-compiler-state-{}-{}.json",
        std::process::id(),
        line!()
    ));
    compiler
        .save_bootstrap_image_file("compiler-state", &path)
        .unwrap();

    let mut restored = host.new_session();
    restored.load_bootstrap_image_file(&path).unwrap();
    std::fs::remove_file(&path).ok();
    restored.restore_bootstrap_image("compiler-state").unwrap();

    let restored_schema = restored
        .fact_schema()
        .lookup("demo.fact")
        .unwrap()
        .cloned()
        .expect("expected restored fact schema");
    assert_eq!(restored_schema.type_label, "demo-string");
    assert_eq!(restored_schema.bridge_name, "string");
    assert_eq!(restored_schema.description.as_deref(), Some("demo fact"));
    assert_eq!(restored.language_builtin_bridges(), vec!["core-special"]);
}

#[test]
fn test_bootstrap_image_file_defaults_missing_compiler_bridge_state() {
    let image_file = caap_core_port::BootstrapImageFile::from_json_str(
        r#"{
          "format_name": "caap-rust-bootstrap-image",
          "format_version": 1,
          "image": {
            "name": "legacy",
            "units": [],
            "capabilities": {
              "grants": {},
              "version": 0
            },
            "session_version": 0
          }
        }"#,
    )
    .unwrap();

    assert_eq!(image_file.image.name, "legacy");
    assert!(image_file.image.fact_schema.schemas().is_empty());
    assert!(image_file.image.language_builtin_bridges.is_empty());
}

#[test]
fn test_bootstrap_image_file_load_can_require_trusted_fingerprint() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    compiler
        .bootstrap()
        .execute_text("(int-add 3 4)", "stdlib.trusted")
        .unwrap();
    compiler.store_bootstrap_image("trusted-base").unwrap();
    let path = std::env::temp_dir().join(format!(
        "caap-bootstrap-trusted-image-{}-{}.json",
        std::process::id(),
        line!()
    ));
    compiler
        .save_bootstrap_image_file("trusted-base", &path)
        .unwrap();

    let mut rejected = host.new_session();
    let err = rejected
        .load_trusted_bootstrap_image_file(&path, &caap_core_port::BootstrapImageTrustPolicy::new())
        .expect_err("empty trust policy should reject persisted bootstrap image");
    assert!(err.contains("not trusted"));

    let mut policy = caap_core_port::BootstrapImageTrustPolicy::new();
    let trusted_fingerprint = policy.trust_file(&path).unwrap();
    assert_eq!(
        policy.trusted_fingerprints(),
        vec![trusted_fingerprint.as_str()]
    );

    let mut restored = host.new_session();
    let image_name = restored
        .load_trusted_bootstrap_image_file(&path, &policy)
        .unwrap();
    std::fs::remove_file(&path).ok();

    assert_eq!(image_name, "trusted-base");
    assert_eq!(
        restored.bootstrap_images().image_names(),
        vec!["trusted-base"]
    );
    assert!(
        restored.events().by_kind("bootstrap.image.load").unwrap()[0]
            .metadata
            .contains(&("trusted".to_string(), "true".to_string()))
    );
}

#[test]
fn test_bootstrap_execute_file_records_resolved_path_trace() {
    let host = caap_core_port::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-rust-bootstrap-{}-{}.caap",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, "(int-add 1 2)").unwrap();
    let resolved = std::fs::canonicalize(&path)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let value = compiler
        .bootstrap()
        .execute_file(&path, "file.bootstrap")
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(3));
    assert_eq!(
        compiler.bootstrap_executions(),
        std::slice::from_ref(&resolved)
    );
    assert_eq!(compiler.bootstrap_trace().len(), 1);
    assert_eq!(compiler.bootstrap_trace()[0].target, resolved);
    assert!(compiler.catalog().contains_unit("file.bootstrap"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_cross_unit_graph_resolves_projected_link_bindings() {
    let mut dependency = Unit::empty("dep").unwrap();
    dependency.semantics_mut().define_symbol(
        caap_core_port::SymbolEntry::new(
            "public-value",
            caap_core_port::SymbolKind::TopLevel,
            caap_core_port::PhasePolicy::Runtime,
            None,
        )
        .unwrap(),
    );

    let mut main = Unit::empty("main").unwrap();
    main.add_link_binding(
        caap_core_port::LinkBinding::new("dep", "public-value", "local-value").unwrap(),
    );

    let mut units = BTreeMap::new();
    units.insert("main".to_string(), main);
    units.insert("dep".to_string(), dependency);
    let graph = caap_core_port::CrossUnitGraph::new(&units);

    let binding = graph
        .resolve_local("main", "local-value")
        .unwrap()
        .expect("binding should resolve");
    let symbol = graph
        .resolve_binding(binding)
        .unwrap()
        .expect("source symbol should resolve");

    assert_eq!(binding.source_unit, "dep");
    assert_eq!(symbol.name, "public-value");
}

#[test]
fn test_cross_unit_graph_missing_endpoint_is_degraded_none() {
    let mut main = Unit::empty("main").unwrap();
    main.add_link_binding(
        caap_core_port::LinkBinding::new("missing", "value", "local-value").unwrap(),
    );

    let mut units = BTreeMap::new();
    units.insert("main".to_string(), main);
    let graph = caap_core_port::CrossUnitGraph::new(&units);
    let binding = graph
        .resolve_local("main", "local-value")
        .unwrap()
        .expect("binding should exist");

    assert!(graph.resolve_binding(binding).unwrap().is_none());
}

#[test]
fn test_unit_link_state_validates_public_names() {
    let binding = caap_core_port::LinkBinding::new("dep", "value", "local").unwrap();
    let state = caap_core_port::UnitLinkState::new(
        "main",
        [binding],
        ["z".to_string(), "a".to_string(), "a".to_string()],
    )
    .unwrap();

    assert_eq!(state.public_names, vec!["a".to_string(), "z".to_string()]);
    assert!(caap_core_port::UnitLinkState::new("main", [], ["".to_string()]).is_err());
}

#[test]
fn test_invoke() {
    // (invoke (lambda (x) (int-add x 1)) 5) → 6
    let mut b = GraphBuilder::new();
    let params_callee = b.name("__params__");
    let param_x = b.name("x");
    let params = b.call(params_callee, vec![param_x]);
    let add_fn = b.name("int-add");
    let body_x = b.name("x");
    let one = b.literal(lit_int(1));
    let body = b.call(add_fn, vec![body_x, one]);
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, body]);
    let invoke_fn = b.name("invoke");
    let five = b.literal(lit_int(5));
    let call_id = b.call(invoke_fn, vec![lam, five]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(6));
}

#[test]
fn test_host_function_value_invokes_explicit_host_callable() {
    let mut b = GraphBuilder::new();
    let host_name = b.name("host-inc");
    let arg = b.literal(lit_int(41));
    let call_id = b.call(host_name, vec![arg]);
    let env = Environment::new(None);
    Environment::define(
        &env,
        "host-inc",
        RuntimeValue::HostFunction(Rc::new(
            caap_core_port::HostFunction::new(
                "host-inc",
                1,
                Some(1),
                Box::new(|args| {
                    let value = caap_core_port::require_int_strict(&args[0], "host-inc")?;
                    Ok(RuntimeValue::Int(value + 1))
                }),
            )
            .unwrap(),
        )),
    );
    let graph = std::mem::take(&mut b.graph);
    let mut ev = Evaluator::new(graph);

    assert_eq!(ev.eval(call_id, &env).unwrap(), RuntimeValue::Int(42));
}

#[test]
fn test_host_function_value_checks_arity() {
    let mut b = GraphBuilder::new();
    let host_name = b.name("host-zero");
    let arg = b.literal(lit_int(1));
    let call_id = b.call(host_name, vec![arg]);
    let env = Environment::new(None);
    Environment::define(
        &env,
        "host-zero",
        RuntimeValue::HostFunction(Rc::new(
            caap_core_port::HostFunction::new(
                "host-zero",
                0,
                Some(0),
                Box::new(|_| Ok(RuntimeValue::Null)),
            )
            .unwrap(),
        )),
    );
    let graph = std::mem::take(&mut b.graph);
    let mut ev = Evaluator::new(graph);

    assert!(ev.eval(call_id, &env).is_err());
}

#[test]
fn test_host_service_registry_exports_explicit_runtime_function() {
    let mut registry = caap_core_port::HostServiceRegistry::new();
    registry
        .register_function(
            "math",
            "inc",
            caap_core_port::PhasePolicy::Runtime,
            caap_core_port::HostFunction::new(
                "math.inc",
                1,
                Some(1),
                Box::new(|args| {
                    let value = caap_core_port::require_int_strict(&args[0], "math.inc")?;
                    Ok(RuntimeValue::Int(value + 1))
                }),
            )
            .unwrap(),
        )
        .unwrap();

    assert_eq!(registry.library_names(), vec!["math"]);
    let exported = registry
        .export("math", "inc", caap_core_port::PhasePolicy::Runtime)
        .unwrap();
    assert!(matches!(exported, RuntimeValue::HostFunction(_)));
}

#[test]
fn test_host_service_registry_enforces_phase() {
    let mut registry = caap_core_port::HostServiceRegistry::new();
    registry
        .register_function(
            "compile",
            "emit",
            caap_core_port::PhasePolicy::CompileTime,
            caap_core_port::HostFunction::new(
                "compile.emit",
                0,
                Some(0),
                Box::new(|_| Ok(RuntimeValue::Null)),
            )
            .unwrap(),
        )
        .unwrap();

    assert!(registry
        .export("compile", "emit", caap_core_port::PhasePolicy::Runtime)
        .is_err());
    assert!(registry
        .export("compile", "emit", caap_core_port::PhasePolicy::CompileTime)
        .is_ok());
}

#[test]
fn test_host_service_registry_enforces_capability_policy() {
    let mut registry = caap_core_port::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry
        .allow_only_capabilities(["path.basename".to_string(), "time.*".to_string()])
        .unwrap();

    assert!(registry
        .export("path", "basename", caap_core_port::PhasePolicy::Runtime)
        .is_ok());
    assert!(registry
        .export("time", "unix-millis", caap_core_port::PhasePolicy::Runtime)
        .is_ok());
    assert!(registry
        .export("time", "now-unix-ns", caap_core_port::PhasePolicy::Runtime)
        .is_ok());

    let err = registry
        .export("fs", "write-text", caap_core_port::PhasePolicy::Runtime)
        .expect_err("fs.write-text should require an explicit capability");
    assert_eq!(err, "host capability denied: fs.write-text");
}

#[test]
fn test_host_service_registry_registers_explicit_system_libraries() {
    let mut registry = caap_core_port::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();

    assert_eq!(
        registry.library_names(),
        vec!["env", "format", "fs", "io", "net", "os", "path", "process", "time"]
    );

    let basename = registry
        .export("path", "basename", caap_core_port::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(basename) = basename else {
        panic!("expected host function");
    };
    assert_eq!(
        (basename.handler)(vec![RuntimeValue::Str("/tmp/demo.caap".into())]).unwrap(),
        RuntimeValue::Str("demo.caap".into())
    );

    let exists = registry
        .export("fs", "exists", caap_core_port::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(exists) = exists else {
        panic!("expected host function");
    };
    let manifest_path = format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"));
    assert_eq!(
        (exists.handler)(vec![RuntimeValue::Str(manifest_path.into())]).unwrap(),
        RuntimeValue::Bool(true)
    );

    assert!(registry
        .export(
            "time",
            "unix-millis",
            caap_core_port::PhasePolicy::CompileTime
        )
        .is_err());
    assert!(registry
        .export(
            "time",
            "now-unix-ns",
            caap_core_port::PhasePolicy::CompileTime
        )
        .is_err());
}

#[test]
fn test_host_service_registry_owns_python_shaped_export_metadata() {
    let mut registry = caap_core_port::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();

    let fs = registry.library("fs").unwrap().unwrap();
    let read_text = fs.export("read-text").unwrap().unwrap();
    assert_eq!(read_text.metadata.module.as_deref(), Some("sys.fs"));
    assert_eq!(read_text.metadata.public, "read-text");
    assert_eq!(read_text.metadata.policy, "fs-read-path");
    assert_eq!(read_text.metadata.effect, "impure");
    assert!(!read_text.metadata.pure);
    assert_eq!(read_text.metadata.kind, "function");
    assert_eq!(
        read_text.metadata.capability_kind.as_deref(),
        Some("sys.fs")
    );
    assert_eq!(read_text.metadata.signature.result, "string");
    assert_eq!(read_text.metadata.signature.params.len(), 1);
    assert_eq!(read_text.metadata.signature.params[0].name, "path");
    assert_eq!(read_text.metadata.signature.params[0].type_name, "string");
    assert_eq!(read_text.metadata.min_arity, 1);
    assert_eq!(read_text.metadata.max_arity, Some(1));
    assert!(!read_text.metadata.variadic);

    let format = registry.library("format").unwrap().unwrap();
    let format_export = format.export("format").unwrap().unwrap();
    assert_eq!(format_export.metadata.module.as_deref(), Some("sys.fmt"));
    assert_eq!(format_export.metadata.effect, "pure");
    assert!(format_export.metadata.pure);
    assert_eq!(format_export.metadata.capability_kind, None);
    assert_eq!(format_export.metadata.signature.params[0].name, "template");
    assert_eq!(format_export.metadata.signature.params[1].name, "values");
    assert_eq!(
        format_export.metadata.signature.params[1].type_name,
        "any[]"
    );
    assert_eq!(format_export.metadata.signature.result, "string");
    assert_eq!(format_export.metadata.max_arity, None);
    assert!(format_export.metadata.variadic);

    let path = registry.library("path").unwrap().unwrap();
    let basename = path.export("basename").unwrap().unwrap();
    assert_eq!(basename.metadata.module, None);
    assert_eq!(basename.metadata.capability_kind, None);
    assert_eq!(basename.metadata.policy, "none");

    let time = registry.library("time").unwrap().unwrap();
    let unix_millis = time.export("unix-millis").unwrap().unwrap();
    assert_eq!(unix_millis.metadata.module, None);
    assert_eq!(unix_millis.metadata.capability_kind, None);
}

#[test]
fn test_host_system_io_and_time_exports_match_python_native_surface() {
    let mut registry = caap_core_port::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();

    for export in [
        "print",
        "println",
        "write",
        "eprint",
        "eprintln",
        "flush-stdout",
        "flush-stderr",
        "read-line",
        "read-all",
    ] {
        assert!(
            registry
                .export("io", export, caap_core_port::PhasePolicy::Runtime)
                .is_ok(),
            "expected io.{export} export"
        );
    }

    let now = registry
        .export("time", "now-unix-ns", caap_core_port::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(now) = now else {
        panic!("expected time.now-unix-ns host function");
    };
    let RuntimeValue::Int(first) = (now.handler)(vec![]).unwrap() else {
        panic!("expected time.now-unix-ns int result");
    };
    let RuntimeValue::Int(second) = (now.handler)(vec![]).unwrap() else {
        panic!("expected time.now-unix-ns int result");
    };
    assert!(first > 0);
    assert!(second >= first);
}

#[test]
fn test_host_system_policy_enforces_python_native_sandbox_surface() {
    fn host_function(
        registry: &caap_core_port::HostServiceRegistry,
        library: &str,
        export: &str,
    ) -> Rc<caap_core_port::HostFunction> {
        let value = registry
            .export(library, export, caap_core_port::PhasePolicy::Runtime)
            .unwrap();
        let RuntimeValue::HostFunction(function) = value else {
            panic!("expected {library}.{export} host function");
        };
        function
    }

    fn spec(entries: Vec<(&str, RuntimeValue)>) -> RuntimeValue {
        let mut map = std::collections::HashMap::new();
        for (key, value) in entries {
            map.insert(MapKey::Str(key.into()), value);
        }
        RuntimeValue::Map(Rc::new(std::cell::RefCell::new(map)))
    }

    fn argv(items: &[&str]) -> RuntimeValue {
        RuntimeValue::List(Rc::new(std::cell::RefCell::new(
            items
                .iter()
                .map(|item| RuntimeValue::Str((*item).into()))
                .collect(),
        )))
    }

    let allowed_root = std::env::temp_dir().join(format!(
        "caap-rust-host-policy-allowed-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let denied_root = std::env::temp_dir().join(format!(
        "caap-rust-host-policy-denied-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&allowed_root).unwrap();
    std::fs::create_dir_all(&denied_root).unwrap();
    let allowed_file = allowed_root.join("allowed.txt");
    let denied_file = denied_root.join("denied.txt");
    std::fs::write(&allowed_file, "allowed").unwrap();
    std::fs::write(&denied_file, "denied").unwrap();

    let mut registry = caap_core_port::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    let mut policy = caap_core_port::HostSystemPolicy::allow_all();
    policy.fs = caap_core_port::HostFileSystemPolicy {
        read_roots: Some(vec![allowed_root.clone()]),
        write_roots: Some(vec![allowed_root.clone()]),
    };
    policy.io.allow_stdin_read = false;
    policy.process.allow_spawn = false;
    policy.net.allow_listen = false;
    policy.net.allow_connect = false;
    policy.os = caap_core_port::HostOsEnvironmentPolicy::allow_only([]).unwrap();
    registry.set_system_policy(policy);

    let read_text = host_function(&registry, "fs", "read-text");
    assert_eq!(
        (read_text.handler)(vec![RuntimeValue::Str(
            allowed_file.to_str().unwrap().into()
        )])
        .unwrap(),
        RuntimeValue::Str("allowed".into())
    );
    let err = (read_text.handler)(vec![RuntimeValue::Str(
        denied_file.to_str().unwrap().into(),
    )])
    .expect_err("read outside allowed roots must fail");
    assert!(format!("{err}").contains("outside allowed roots"));

    let write_text = host_function(&registry, "fs", "write-text");
    (write_text.handler)(vec![
        RuntimeValue::Str(allowed_root.join("written.txt").to_str().unwrap().into()),
        RuntimeValue::Str("ok".into()),
    ])
    .unwrap();
    let err = (write_text.handler)(vec![
        RuntimeValue::Str(denied_root.join("written.txt").to_str().unwrap().into()),
        RuntimeValue::Str("no".into()),
    ])
    .expect_err("write outside allowed roots must fail");
    assert!(format!("{err}").contains("outside allowed roots"));

    let process_run = host_function(&registry, "process", "run");
    let err = (process_run.handler)(vec![spec(vec![(
        "argv",
        argv(&["/bin/sh", "-c", "exit 0"]),
    )])])
    .expect_err("process spawn must be denied");
    assert!(format!("{err}").contains("process spawning is not allowed"));

    let net_listen = host_function(&registry, "net", "listen");
    let err = (net_listen.handler)(vec![spec(vec![
        ("host", RuntimeValue::Str("127.0.0.1".into())),
        ("port", RuntimeValue::Int(0)),
    ])])
    .expect_err("network listen must be denied");
    assert!(format!("{err}").contains("network listening is not allowed"));

    let io_read_line = host_function(&registry, "io", "read-line");
    let err = (io_read_line.handler)(vec![]).expect_err("stdin read must be denied");
    assert!(format!("{err}").contains("stdin reading is not allowed"));

    assert_eq!(
        (host_function(&registry, "os", "env-has").handler)(vec![RuntimeValue::Str("PATH".into())])
            .unwrap(),
        RuntimeValue::Bool(false)
    );
    assert_eq!(
        (host_function(&registry, "os", "env-get").handler)(vec![RuntimeValue::Str("PATH".into())])
            .unwrap(),
        RuntimeValue::Null
    );
    let RuntimeValue::List(env_keys) =
        (host_function(&registry, "os", "env-keys").handler)(vec![]).unwrap()
    else {
        panic!("expected os.env-keys list");
    };
    assert!(env_keys.borrow().is_empty());
    let RuntimeValue::Map(env_vars) =
        (host_function(&registry, "os", "env-vars").handler)(vec![]).unwrap()
    else {
        panic!("expected os.env-vars map");
    };
    assert!(env_vars.borrow().is_empty());

    let _ = std::fs::remove_dir_all(allowed_root);
    let _ = std::fs::remove_dir_all(denied_root);
}

#[test]
fn test_compiler_host_compile_time_system_libraries_use_sandbox_policy() {
    let mut host = caap_core_port::CompilerHost::new();
    host.register_default_compile_time_system_libraries()
        .unwrap();

    let io_read_line = host
        .compile_time_services()
        .export("io", "read-line", caap_core_port::PhasePolicy::CompileTime)
        .unwrap();
    let RuntimeValue::HostFunction(io_read_line) = io_read_line else {
        panic!("expected io.read-line host function");
    };
    let err = (io_read_line.handler)(vec![]).expect_err("compile-time stdin must be denied");
    assert!(format!("{err}").contains("stdin reading is not allowed"));

    let env_get = host
        .compile_time_services()
        .export("os", "env-get", caap_core_port::PhasePolicy::CompileTime)
        .unwrap();
    let RuntimeValue::HostFunction(env_get) = env_get else {
        panic!("expected os.env-get host function");
    };
    assert_eq!(
        (env_get.handler)(vec![RuntimeValue::Str("PATH".into())]).unwrap(),
        RuntimeValue::Null
    );
}

#[test]
fn test_host_service_registry_exports_library_to_environment_explicitly() {
    let mut registry = caap_core_port::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    let env = Environment::new(None);

    let bindings = registry
        .export_library_to_environment("path", caap_core_port::PhasePolicy::Runtime, &env)
        .unwrap();
    assert_eq!(bindings, vec!["path.basename", "path.dirname", "path.join"]);

    let exported = Environment::lookup(&env, "path.basename").unwrap();
    let RuntimeValue::HostFunction(exported) = exported else {
        panic!("expected path.basename host function");
    };
    assert_eq!(
        (exported.handler)(vec![RuntimeValue::Str("/tmp/demo.caap".into())]).unwrap(),
        RuntimeValue::Str("demo.caap".into())
    );
}

#[test]
fn test_host_service_builtins_export_compile_time_and_runtime_services() {
    let mut host = caap_core_port::CompilerHost::new();
    host.compile_time_services_mut()
        .register_function(
            "math",
            "inc",
            caap_core_port::PhasePolicy::CompileTime,
            caap_core_port::HostFunction::new(
                "math.inc",
                1,
                Some(1),
                Box::new(|args| {
                    let value = caap_core_port::require_int_strict(&args[0], "math.inc")?;
                    Ok(RuntimeValue::Int(value + 1))
                }),
            )
            .unwrap(),
        )
        .unwrap();
    host.register_default_runtime_system_libraries().unwrap();
    let mut compiler = host.new_session();
    let graph = parse(
        "(bind inc (host-service-export \"math\" \"inc\" \"compile_time\")
          (bind runtime-basename
            (host-runtime-service-export
              (host-service-capability \"host_services\")
              \"path\"
              \"basename\")
            (list-of
              (inc 41)
              (runtime-basename \"/tmp/demo.caap\"))))",
    )
    .unwrap();
    let unit = Unit::from_graph("host-service-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(42));
    assert_eq!(items[1], RuntimeValue::Str("demo.caap".into()));
}

#[test]
fn test_host_service_builtins_project_libraries_catalog_and_capability_exports() {
    let mut host = caap_core_port::CompilerHost::new();
    host.register_default_runtime_system_libraries().unwrap();
    let mut compiler = host.new_session();
    let graph = parse(
        "(bind libraries (host-service-libraries \"runtime\")
          (bind catalog (host-service-library-catalog \"os\" \"runtime\")
            (bind guarded-current-exe
              (host-service-capability-export
                (host-service-capability \"sys.path\")
                \"os\"
                \"current-exe\"
                \"runtime\")
              (list-of
                (contains libraries \"os\")
                (get (get catalog 2) \"module\")
                (get (get catalog 2) \"capability_kind\")
                (value-is-string
                  (guarded-current-exe
                    (host-service-capability \"sys.path\")))))))",
    )
    .unwrap();
    let unit = Unit::from_graph("host-service-catalog-builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core_port::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Str("sys.path".into()));
    assert_eq!(items[2], RuntimeValue::Str("sys.path".into()));
    assert_eq!(items[3], RuntimeValue::Bool(true));
}

#[test]
fn test_host_system_libraries_support_net_parsing_without_network_io() {
    let mut registry = caap_core_port::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();

    let is_ip = registry
        .export("net", "is-ip", caap_core_port::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(is_ip) = is_ip else {
        panic!("expected net.is-ip host function");
    };
    assert_eq!(
        (is_ip.handler)(vec![RuntimeValue::Str("127.0.0.1".into())]).unwrap(),
        RuntimeValue::Bool(true)
    );
    assert_eq!(
        (is_ip.handler)(vec![RuntimeValue::Str("localhost".into())]).unwrap(),
        RuntimeValue::Bool(false)
    );

    let host_port = registry
        .export("net", "host-port", caap_core_port::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(host_port) = host_port else {
        panic!("expected net.host-port host function");
    };
    assert_eq!(
        (host_port.handler)(vec![
            RuntimeValue::Str("::1".into()),
            RuntimeValue::Int(8080),
        ])
        .unwrap(),
        RuntimeValue::Str("[::1]:8080".into())
    );
}

#[test]
fn test_host_system_libraries_support_process_and_fs_write_text() {
    let mut registry = caap_core_port::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();

    let process_id = registry
        .export("process", "id", caap_core_port::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(process_id) = process_id else {
        panic!("expected process.id host function");
    };
    assert!(matches!((process_id.handler)(vec![]).unwrap(), RuntimeValue::Int(id) if id > 0));

    let path = std::env::temp_dir().join(format!(
        "caap-rust-fs-write-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let write_text = registry
        .export("fs", "write-text", caap_core_port::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(write_text) = write_text else {
        panic!("expected fs.write-text host function");
    };
    (write_text.handler)(vec![
        RuntimeValue::Str(path.to_str().unwrap().into()),
        RuntimeValue::Str("hello".into()),
    ])
    .unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    let _ = std::fs::remove_file(path);
}

#[test]
fn test_host_system_process_lifecycle_exports_match_python_native_surface() {
    fn host_call(
        registry: &caap_core_port::HostServiceRegistry,
        export: &str,
        args: Vec<RuntimeValue>,
    ) -> RuntimeValue {
        let value = registry
            .export("process", export, caap_core_port::PhasePolicy::Runtime)
            .unwrap();
        let RuntimeValue::HostFunction(function) = value else {
            panic!("expected process.{export} host function");
        };
        (function.handler)(args).unwrap()
    }

    fn spec(entries: Vec<(&str, RuntimeValue)>) -> RuntimeValue {
        let mut map = std::collections::HashMap::new();
        for (key, value) in entries {
            map.insert(MapKey::Str(key.into()), value);
        }
        RuntimeValue::Map(Rc::new(std::cell::RefCell::new(map)))
    }

    fn argv(items: &[&str]) -> RuntimeValue {
        RuntimeValue::List(Rc::new(std::cell::RefCell::new(
            items
                .iter()
                .map(|item| RuntimeValue::Str((*item).into()))
                .collect(),
        )))
    }

    let mut registry = caap_core_port::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();

    let run = host_call(
        &registry,
        "run",
        vec![spec(vec![(
            "argv",
            argv(&["/bin/sh", "-c", "printf out; printf err >&2; exit 4"]),
        )])],
    );
    let RuntimeValue::Map(run) = run else {
        panic!("expected process.run map");
    };
    let run = run.borrow();
    assert_eq!(
        run.get(&MapKey::Str("status".into())),
        Some(&RuntimeValue::Int(4))
    );
    assert_eq!(
        run.get(&MapKey::Str("success".into())),
        Some(&RuntimeValue::Bool(false))
    );
    assert_eq!(
        run.get(&MapKey::Str("stdout".into())),
        Some(&RuntimeValue::Str("out".into()))
    );
    assert_eq!(
        run.get(&MapKey::Str("stderr".into())),
        Some(&RuntimeValue::Str("err".into()))
    );
    drop(run);

    let spawned = host_call(
        &registry,
        "spawn",
        vec![spec(vec![
            ("argv", argv(&["/bin/sh", "-c", "cat"])),
            ("capture_stdout", RuntimeValue::Bool(true)),
            ("capture_stderr", RuntimeValue::Bool(true)),
        ])],
    );
    let RuntimeValue::Int(handle) = spawned else {
        panic!("expected process handle");
    };
    host_call(
        &registry,
        "write-stdin",
        vec![RuntimeValue::Int(handle), RuntimeValue::Str("hello".into())],
    );
    host_call(&registry, "close-stdin", vec![RuntimeValue::Int(handle)]);
    assert_eq!(
        host_call(&registry, "read-stdout", vec![RuntimeValue::Int(handle)]),
        RuntimeValue::Str("hello".into())
    );
    let waited = host_call(&registry, "wait", vec![RuntimeValue::Int(handle)]);
    let RuntimeValue::Map(waited) = waited else {
        panic!("expected process.wait map");
    };
    assert_eq!(
        waited.borrow().get(&MapKey::Str("success".into())),
        Some(&RuntimeValue::Bool(true))
    );
}

#[test]
fn test_host_system_net_socket_exports_match_python_native_surface() {
    fn host_call(
        registry: &caap_core_port::HostServiceRegistry,
        export: &str,
        args: Vec<RuntimeValue>,
    ) -> RuntimeValue {
        let value = registry
            .export("net", export, caap_core_port::PhasePolicy::Runtime)
            .unwrap();
        let RuntimeValue::HostFunction(function) = value else {
            panic!("expected net.{export} host function");
        };
        (function.handler)(args).unwrap()
    }

    fn spec(entries: Vec<(&str, RuntimeValue)>) -> RuntimeValue {
        let mut map = std::collections::HashMap::new();
        for (key, value) in entries {
            map.insert(MapKey::Str(key.into()), value);
        }
        RuntimeValue::Map(Rc::new(std::cell::RefCell::new(map)))
    }

    fn handles(items: &[i64]) -> RuntimeValue {
        RuntimeValue::List(Rc::new(std::cell::RefCell::new(
            items.iter().map(|item| RuntimeValue::Int(*item)).collect(),
        )))
    }

    let port = {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    };
    let mut registry = caap_core_port::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();

    let listener = host_call(
        &registry,
        "listen",
        vec![spec(vec![
            ("host", RuntimeValue::Str("127.0.0.1".into())),
            ("port", RuntimeValue::Int(port as i64)),
            ("backlog", RuntimeValue::Int(16)),
            ("reuse_addr", RuntimeValue::Bool(true)),
        ])],
    );
    let RuntimeValue::Int(listener_handle) = listener else {
        panic!("expected listener handle");
    };

    let client = host_call(
        &registry,
        "connect",
        vec![spec(vec![
            ("host", RuntimeValue::Str("127.0.0.1".into())),
            ("port", RuntimeValue::Int(port as i64)),
        ])],
    );
    let RuntimeValue::Int(client_handle) = client else {
        panic!("expected client socket handle");
    };

    let server = host_call(
        &registry,
        "accept",
        vec![RuntimeValue::Int(listener_handle)],
    );
    let RuntimeValue::Int(server_handle) = server else {
        panic!("expected server socket handle");
    };

    host_call(
        &registry,
        "write",
        vec![
            RuntimeValue::Int(client_handle),
            RuntimeValue::Str("ping".into()),
        ],
    );
    assert_eq!(
        host_call(
            &registry,
            "read",
            vec![RuntimeValue::Int(server_handle), RuntimeValue::Int(4)]
        ),
        RuntimeValue::Str("ping".into())
    );

    host_call(
        &registry,
        "write",
        vec![
            RuntimeValue::Int(server_handle),
            RuntimeValue::Str("pong".into()),
        ],
    );
    let RuntimeValue::List(events) = host_call(
        &registry,
        "poll",
        vec![handles(&[client_handle]), RuntimeValue::Int(1000)],
    ) else {
        panic!("expected net.poll list");
    };
    let events = events.borrow();
    assert!(!events.is_empty());
    let RuntimeValue::Map(event) = &events[0] else {
        panic!("expected net.poll event map");
    };
    assert_eq!(
        event.borrow().get(&MapKey::Str("handle".into())),
        Some(&RuntimeValue::Int(client_handle))
    );
    assert_eq!(
        event.borrow().get(&MapKey::Str("kind".into())),
        Some(&RuntimeValue::Str("socket".into()))
    );
    assert_eq!(
        event.borrow().get(&MapKey::Str("readable".into())),
        Some(&RuntimeValue::Bool(true))
    );
    drop(events);

    assert_eq!(
        host_call(
            &registry,
            "read",
            vec![RuntimeValue::Int(client_handle), RuntimeValue::Int(4)]
        ),
        RuntimeValue::Str("pong".into())
    );
    host_call(&registry, "close", vec![RuntimeValue::Int(client_handle)]);
    host_call(&registry, "close", vec![RuntimeValue::Int(server_handle)]);
    host_call(&registry, "close", vec![RuntimeValue::Int(listener_handle)]);
}

#[test]
fn test_host_system_fs_path_exports_match_python_native_surface() {
    fn host_call(
        registry: &caap_core_port::HostServiceRegistry,
        export: &str,
        args: Vec<RuntimeValue>,
    ) -> RuntimeValue {
        let value = registry
            .export("fs", export, caap_core_port::PhasePolicy::Runtime)
            .unwrap();
        let RuntimeValue::HostFunction(function) = value else {
            panic!("expected fs.{export} host function");
        };
        (function.handler)(args).unwrap()
    }

    fn path_value(path: &std::path::Path) -> RuntimeValue {
        RuntimeValue::Str(path.to_str().unwrap().into())
    }

    let mut registry = caap_core_port::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    let root = std::env::temp_dir().join(format!(
        "caap-rust-fs-path-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let nested = root.join("nested");
    let source = nested.join("source.txt");
    let copied = root.join("copied.txt");
    let renamed = root.join("renamed.txt");

    host_call(&registry, "create-dir-all", vec![path_value(&nested)]);
    host_call(
        &registry,
        "write-text",
        vec![path_value(&source), RuntimeValue::Str("hello".into())],
    );
    host_call(
        &registry,
        "append-text",
        vec![path_value(&source), RuntimeValue::Str(" world".into())],
    );
    assert_eq!(
        host_call(&registry, "read-text", vec![path_value(&source)]),
        RuntimeValue::Str("hello world".into())
    );
    assert_eq!(
        host_call(&registry, "is-file", vec![path_value(&source)]),
        RuntimeValue::Bool(true)
    );
    assert_eq!(
        host_call(&registry, "is-dir", vec![path_value(&nested)]),
        RuntimeValue::Bool(true)
    );

    let RuntimeValue::Map(metadata) = host_call(&registry, "metadata", vec![path_value(&source)])
    else {
        panic!("expected fs.metadata map");
    };
    let metadata = metadata.borrow();
    assert_eq!(
        metadata.get(&MapKey::Str("kind".into())),
        Some(&RuntimeValue::Str("file".into()))
    );
    assert_eq!(
        metadata.get(&MapKey::Str("size".into())),
        Some(&RuntimeValue::Int(11))
    );
    drop(metadata);

    let RuntimeValue::List(entries) = host_call(&registry, "list-dir", vec![path_value(&root)])
    else {
        panic!("expected fs.list-dir list");
    };
    let entries = entries.borrow();
    let RuntimeValue::Map(first_entry) = &entries[0] else {
        panic!("expected fs.list-dir entry map");
    };
    assert_eq!(
        first_entry.borrow().get(&MapKey::Str("name".into())),
        Some(&RuntimeValue::Str("nested".into()))
    );
    drop(entries);

    assert_eq!(
        host_call(&registry, "canonicalize", vec![path_value(&source)]),
        RuntimeValue::Str(
            std::fs::canonicalize(&source)
                .unwrap()
                .to_str()
                .unwrap()
                .into()
        )
    );

    host_call(
        &registry,
        "copy-file",
        vec![path_value(&source), path_value(&copied)],
    );
    assert_eq!(std::fs::read_to_string(&copied).unwrap(), "hello world");
    host_call(
        &registry,
        "rename",
        vec![path_value(&copied), path_value(&renamed)],
    );
    assert!(renamed.exists());
    host_call(&registry, "remove-file", vec![path_value(&renamed)]);
    assert!(!renamed.exists());
    host_call(&registry, "remove-dir-all", vec![path_value(&root)]);
    assert!(!root.exists());
}

#[test]
fn test_host_system_fs_handle_exports_match_python_native_surface() {
    fn host_call(
        registry: &caap_core_port::HostServiceRegistry,
        export: &str,
        args: Vec<RuntimeValue>,
    ) -> RuntimeValue {
        let value = registry
            .export("fs", export, caap_core_port::PhasePolicy::Runtime)
            .unwrap();
        let RuntimeValue::HostFunction(function) = value else {
            panic!("expected fs.{export} host function");
        };
        (function.handler)(args).unwrap()
    }

    fn path_value(path: &std::path::Path) -> RuntimeValue {
        RuntimeValue::Str(path.to_str().unwrap().into())
    }

    fn spec(entries: Vec<(&str, RuntimeValue)>) -> RuntimeValue {
        let mut map = std::collections::HashMap::new();
        for (key, value) in entries {
            map.insert(MapKey::Str(key.into()), value);
        }
        RuntimeValue::Map(Rc::new(std::cell::RefCell::new(map)))
    }

    let mut registry = caap_core_port::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    let root = std::env::temp_dir().join(format!(
        "caap-rust-fs-handle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let file_path = root.join("handle.txt");
    std::fs::create_dir_all(&root).unwrap();

    let handle = host_call(
        &registry,
        "open-file",
        vec![spec(vec![
            ("path", path_value(&file_path)),
            ("write", RuntimeValue::Bool(true)),
            ("read", RuntimeValue::Bool(true)),
            ("create", RuntimeValue::Bool(true)),
            ("truncate", RuntimeValue::Bool(true)),
        ])],
    );
    let RuntimeValue::Int(handle_id) = handle else {
        panic!("expected file handle id");
    };
    host_call(
        &registry,
        "file-write",
        vec![
            RuntimeValue::Int(handle_id),
            RuntimeValue::Str("first\nsecond".into()),
        ],
    );
    host_call(&registry, "file-flush", vec![RuntimeValue::Int(handle_id)]);
    assert_eq!(
        host_call(
            &registry,
            "file-seek",
            vec![RuntimeValue::Int(handle_id), RuntimeValue::Int(0)]
        ),
        RuntimeValue::Int(0)
    );
    assert_eq!(
        host_call(
            &registry,
            "file-read-line",
            vec![RuntimeValue::Int(handle_id)]
        ),
        RuntimeValue::Str("first\n".into())
    );
    assert_eq!(
        host_call(
            &registry,
            "file-read-all-text",
            vec![RuntimeValue::Int(handle_id)]
        ),
        RuntimeValue::Str("second".into())
    );
    let RuntimeValue::Map(metadata) = host_call(
        &registry,
        "file-metadata",
        vec![RuntimeValue::Int(handle_id)],
    ) else {
        panic!("expected file metadata map");
    };
    assert_eq!(
        metadata.borrow().get(&MapKey::Str("size".into())),
        Some(&RuntimeValue::Int(12))
    );
    host_call(&registry, "close-file", vec![RuntimeValue::Int(handle_id)]);

    let dir_handle = host_call(&registry, "open-dir", vec![path_value(&root)]);
    let RuntimeValue::Int(dir_handle_id) = dir_handle else {
        panic!("expected dir handle id");
    };
    let RuntimeValue::List(entries) = host_call(
        &registry,
        "dir-list",
        vec![RuntimeValue::Int(dir_handle_id)],
    ) else {
        panic!("expected dir-list result");
    };
    let RuntimeValue::Map(entry) = &entries.borrow()[0] else {
        panic!("expected dir entry map");
    };
    assert_eq!(
        entry.borrow().get(&MapKey::Str("name".into())),
        Some(&RuntimeValue::Str("handle.txt".into()))
    );
    host_call(
        &registry,
        "close-dir",
        vec![RuntimeValue::Int(dir_handle_id)],
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_host_system_libraries_are_not_ambient_globals() {
    let mut registry = caap_core_port::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    let graph = parse("(path.basename \"/tmp/demo.caap\")").unwrap();
    let mut ev = Evaluator::new(graph);

    assert!(ev.run().is_err());
}

#[test]
fn test_apply() {
    // (apply (lambda (x y) (int-add x y)) (list-of 3 4)) → 7
    let mut b = GraphBuilder::new();
    let params_callee = b.name("__params__");
    let px = b.name("x");
    let py = b.name("y");
    let params = b.call(params_callee, vec![px, py]);
    let add_fn = b.name("int-add");
    let rx = b.name("x");
    let ry = b.name("y");
    let body = b.call(add_fn, vec![rx, ry]);
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, body]);
    let list_fn = b.name("list-of");
    let three = b.literal(lit_int(3));
    let four = b.literal(lit_int(4));
    let lst = b.call(list_fn, vec![three, four]);
    let apply_fn = b.name("apply");
    let call_id = b.call(apply_fn, vec![lam, lst]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(7));
}

#[test]
fn test_apply_accepts_tuple_rest_arg() {
    let mut b = GraphBuilder::new();

    let params_callee = b.name("__params__");
    let px = b.name("x");
    let py = b.name("y");
    let params = b.call(params_callee, vec![px, py]);
    let add_fn = b.name("int-add");
    let rx = b.name("x");
    let ry = b.name("y");
    let body = b.call(add_fn, vec![rx, ry]);
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, body]);

    let tuple = b.literal(IrLiteralData::Tuple(vec![
        IrLiteralData::Int(2),
        IrLiteralData::Int(3),
    ]));
    let apply_fn = b.name("apply");
    let call_id = b.call(apply_fn, vec![lam, tuple]);

    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(5));
}

#[test]
fn test_value_lt() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("value-lt");
    let a = b.literal(lit_int(3));
    let c = b.literal(lit_int(7));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_le_equal() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("le");
    let a = b.literal(lit_int(5));
    let c = b.literal(lit_int(5));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_gensym_unique() {
    let mut b = GraphBuilder::new();
    let fn1 = b.name("gensym");
    let call1 = b.call(fn1, vec![]);
    let fn2 = b.name("gensym");
    let call2 = b.call(fn2, vec![]);
    let graph = std::mem::take(&mut b.graph);
    let env = Environment::new(None);
    let mut ev = Evaluator::new(graph);
    let v1 = ev.eval(call1, &env).unwrap();
    let v2 = ev.eval(call2, &env).unwrap();
    assert_ne!(v1, v2);
}

// ── sequence-sort-by / group-by / zip / unique-by ─────────────────────────────

#[test]
fn test_sequence_sort_by() {
    // (sequence-sort-by (list-of 3 1 2) (lambda (x) x)) → [1, 2, 3]
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list-of");
    let three = b.literal(lit_int(3));
    let one = b.literal(lit_int(1));
    let two = b.literal(lit_int(2));
    let list = b.call(list_fn, vec![three, one, two]);
    let params_callee = b.name("__params__");
    let param_x = b.name("x");
    let params = b.call(params_callee, vec![param_x]);
    let identity = b.name("x");
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, identity]);
    let sort_fn = b.name("sequence-sort-by");
    let call_id = b.call(sort_fn, vec![list, lam]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow[0], RuntimeValue::Int(1));
            assert_eq!(borrow[1], RuntimeValue::Int(2));
            assert_eq!(borrow[2], RuntimeValue::Int(3));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_sequence_zip() {
    // (sequence-zip (list-of 1 2 3) (list-of 4 5 6)) → [[1,4],[2,5],[3,6]]
    let mut b = GraphBuilder::new();
    let list_fn1 = b.name("list-of");
    let a1 = b.literal(lit_int(1));
    let b1 = b.literal(lit_int(2));
    let c1 = b.literal(lit_int(3));
    let list1 = b.call(list_fn1, vec![a1, b1, c1]);
    let list_fn2 = b.name("list-of");
    let a2 = b.literal(lit_int(4));
    let b2 = b.literal(lit_int(5));
    let c2 = b.literal(lit_int(6));
    let list2 = b.call(list_fn2, vec![a2, b2, c2]);
    let zip_fn = b.name("sequence-zip");
    let call_id = b.call(zip_fn, vec![list1, list2]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(outer) => {
            let borrow = outer.borrow();
            assert_eq!(borrow.len(), 3);
            match &borrow[0] {
                RuntimeValue::List(inner) => {
                    let ib = inner.borrow();
                    assert_eq!(ib[0], RuntimeValue::Int(1));
                    assert_eq!(ib[1], RuntimeValue::Int(4));
                }
                other => panic!("expected inner list, got {other}"),
            }
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_sequence_unique_by() {
    // (sequence-unique-by (list-of 1 2 1 3 2) identity) → [1, 2, 3]
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list-of");
    let vals: Vec<u32> = [1, 2, 1, 3, 2]
        .iter()
        .map(|&i| b.literal(lit_int(i)))
        .collect();
    let list = b.call(list_fn, vals);
    let params_callee = b.name("__params__");
    let param_x = b.name("x");
    let params = b.call(params_callee, vec![param_x]);
    let identity = b.name("x");
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, identity]);
    let uniq_fn = b.name("sequence-unique-by");
    let call_id = b.call(uniq_fn, vec![list, lam]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => assert_eq!(l.borrow().len(), 3),
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_map_of_entries() {
    // (map-of-entries (list-of (list-of "a" 1) (list-of "b" 2))) → {a:1, b:2}
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list-of");
    let inner1_fn = b.name("list-of");
    let k1 = b.literal(lit_str("a"));
    let v1 = b.literal(lit_int(1));
    let pair1 = b.call(inner1_fn, vec![k1, v1]);
    let inner2_fn = b.name("list-of");
    let k2 = b.literal(lit_str("b"));
    let v2 = b.literal(lit_int(2));
    let pair2 = b.call(inner2_fn, vec![k2, v2]);
    let entries = b.call(list_fn, vec![pair1, pair2]);
    let moe_fn = b.name("map-of-entries");
    let call_id = b.call(moe_fn, vec![entries]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::Map(m) => {
            use caap_core_port::MapKey;
            let borrow = m.borrow();
            assert_eq!(borrow[&MapKey::Str("a".into())], RuntimeValue::Int(1));
            assert_eq!(borrow[&MapKey::Str("b".into())], RuntimeValue::Int(2));
        }
        other => panic!("expected map, got {other}"),
    }
}
