/// Integration tests for the CAAP core evaluator.
///
/// Each test builds a small IR graph by hand and runs it through the evaluator.
use caap_core::{
    compiler::QueryArtifactSource,
    frontend::parse,
    graph::GraphBuilder,
    ir::{IrLiteralData, NodeId},
    values::Environment,
    Evaluator, MapKey, RuntimeValue, SourceSpan, Unit,
};
use std::cell::RefCell;
use std::rc::Rc;

// ── helpers ───────────────────────────────────────────────────────────────────

fn lit_int(v: i64) -> IrLiteralData {
    IrLiteralData::Int(v)
}

fn runtime_map(entries: impl IntoIterator<Item = (&'static str, RuntimeValue)>) -> RuntimeValue {
    RuntimeValue::Map(Rc::new(RefCell::new(
        entries
            .into_iter()
            .map(|(key, value)| (MapKey::Str(key.into()), value))
            .collect(),
    )))
}

trait TestGraphBuilderExt {
    fn name(&mut self, identifier: impl Into<String>) -> NodeId;
    fn internal_name(&mut self, identifier: impl Into<String>) -> NodeId;
    fn literal(&mut self, value: IrLiteralData) -> NodeId;
    fn call(&mut self, callee: NodeId, args: Vec<NodeId>) -> NodeId;
}

impl TestGraphBuilderExt for GraphBuilder {
    fn name(&mut self, identifier: impl Into<String>) -> NodeId {
        self.try_name(identifier)
            .expect("test graph name must be valid")
    }

    fn internal_name(&mut self, identifier: impl Into<String>) -> NodeId {
        self.try_internal_name(identifier)
            .expect("test graph internal name must be valid")
    }

    fn literal(&mut self, value: IrLiteralData) -> NodeId {
        self.try_literal(value)
            .expect("test graph literal must be valid")
    }

    fn call(&mut self, callee: NodeId, args: Vec<NodeId>) -> NodeId {
        self.try_call(callee, args)
            .expect("test graph call must reference existing nodes")
    }
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
fn test_runtime_map_display_uses_insertion_order() {
    let value = RuntimeValue::Map(Rc::new(RefCell::new(indexmap::IndexMap::from([
        (MapKey::Str("z".into()), RuntimeValue::Int(5)),
        (MapKey::Null, RuntimeValue::Int(1)),
        (MapKey::Int(2), RuntimeValue::Int(3)),
        (MapKey::Bool(false), RuntimeValue::Int(2)),
        (MapKey::Str("a".into()), RuntimeValue::Int(4)),
    ]))));

    // Canonical map order is INSERTION order (IndexMap backing) — display
    // mirrors construction, deterministic without sorting.
    assert_eq!(value.to_string(), "{z: 5, null: 1, 2: 3, false: 2, a: 4}");
}

#[test]
fn test_environment_try_lookup_distinguishes_missing_from_lookup_errors() {
    let env = Environment::new(None);
    Environment::define_uninitialized(&env, "pending");

    assert_eq!(Environment::try_lookup(&env, "missing").unwrap(), None);
    let error = Environment::try_lookup(&env, "pending")
        .expect_err("uninitialized binding should remain a lookup error");
    assert!(error
        .to_string()
        .contains("was accessed before initialization"));
}

#[test]
fn test_environment_resolves_stable_lexical_addresses() {
    let parent = Environment::new(None);
    Environment::define(&parent, "outer", RuntimeValue::Int(1));
    let child = Environment::new(Some(Rc::clone(&parent)));

    let (address, value) = Environment::resolve_exact(&child, "outer")
        .unwrap()
        .expect("outer binding should resolve through parent");
    assert_eq!(address.depth, 1);
    assert_eq!(address.slot, 0);
    assert_eq!(value, RuntimeValue::Int(1));
    assert_eq!(
        Environment::lookup_address(&child, "outer", address).unwrap(),
        Some(RuntimeValue::Int(1))
    );

    Environment::assign(&parent, "outer", RuntimeValue::Int(2)).unwrap();
    assert_eq!(
        Environment::lookup_address(&child, "outer", address).unwrap(),
        Some(RuntimeValue::Int(2))
    );
    assert_eq!(
        Environment::try_assign_address(&child, "outer", address, RuntimeValue::Int(3)).unwrap(),
        None
    );
    assert_eq!(
        Environment::lookup_address(&child, "outer", address).unwrap(),
        Some(RuntimeValue::Int(3))
    );
}

#[test]
fn test_environment_lexical_address_detects_later_shadowing() {
    let parent = Environment::new(None);
    Environment::define(&parent, "name", RuntimeValue::Int(1));
    let child = Environment::new(Some(Rc::clone(&parent)));
    let (outer_address, _) = Environment::resolve_exact(&child, "name").unwrap().unwrap();

    Environment::define(&child, "name", RuntimeValue::Int(2));

    assert_eq!(
        Environment::lookup_address(&child, "name", outer_address).unwrap(),
        None
    );
    assert_eq!(
        Environment::try_assign_address(&child, "name", outer_address, RuntimeValue::Int(3))
            .unwrap(),
        Some(RuntimeValue::Int(3))
    );
    assert_eq!(
        Environment::lookup(&parent, "name").unwrap(),
        RuntimeValue::Int(1)
    );
    let (inner_address, value) = Environment::resolve_exact(&child, "name").unwrap().unwrap();
    assert_eq!(inner_address.depth, 0);
    assert_eq!(inner_address.slot, 0);
    assert_eq!(value, RuntimeValue::Int(2));
}

#[test]
fn test_environment_qualified_lookup_respects_lexical_shadowing() {
    let parent = Environment::new(None);
    Environment::define(
        &parent,
        "pkg.mod",
        runtime_map([("value", RuntimeValue::Int(1))]),
    );
    let child = Environment::new(Some(Rc::clone(&parent)));
    Environment::define(
        &child,
        "pkg",
        runtime_map([("mod", runtime_map([("value", RuntimeValue::Int(2))]))]),
    );

    assert_eq!(
        Environment::try_lookup(&child, "pkg.mod.value").unwrap(),
        Some(RuntimeValue::Int(2))
    );
}

#[test]
fn test_environment_qualified_lookup_does_not_fall_through_shadowing_prefix() {
    let parent = Environment::new(None);
    Environment::define(
        &parent,
        "pkg.mod",
        runtime_map([("value", RuntimeValue::Int(1))]),
    );
    let child = Environment::new(Some(Rc::clone(&parent)));
    Environment::define(&child, "pkg", runtime_map([]));

    assert_eq!(
        Environment::try_lookup(&child, "pkg.mod.value").unwrap(),
        None
    );
}

#[test]
fn test_ir_name_constructor_rejects_empty_identifier() {
    assert!(caap_core::NameNode::new(0, "").is_err());
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
fn test_graph_builder_try_call_rejects_reused_child_position() {
    let mut b = GraphBuilder::new();
    let callee = b.name("id");
    let arg = b.literal(lit_int(1));

    assert!(b.try_call(callee, vec![arg, arg]).is_err());
    assert!(b.try_call(callee, vec![callee]).is_err());
    assert_eq!(b.graph.parent(callee), Some(None));
    assert_eq!(b.graph.parent(arg), Some(None));
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
    assert!(caap_core::ExprSpec::name("").is_err());
}

#[test]
fn test_graph_builder_lowers_detached_expr_spec_with_spans() {
    let call_span = SourceSpan::new(0, 7, 1, 1, 1, 8).unwrap();
    let arg_span = SourceSpan::new(5, 6, 1, 6, 1, 7).unwrap();
    let spec = caap_core::ExprSpec::call_with_span(
        caap_core::ExprSpec::name("id").unwrap(),
        vec![caap_core::ExprSpec::literal_with_span(
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
        caap_core::Node::Call(call) => {
            assert_eq!(call.callee, 1);
            assert_eq!(call.args, vec![2].into());
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
    let restored = caap_core::IRGraph::from_template(template).expect("restore failed");

    assert_eq!(restored.root_id, call);
    assert_eq!(restored.top_level_form_ids(), &[call]);
    assert!(restored.source_span(call).is_some());
    assert_eq!(restored.node_count(), 3);
}

#[test]
fn test_ir_graph_template_roundtrips_internal_nodes() {
    let mut b = GraphBuilder::new();
    let callee = b.internal_name("assign_lexical");
    let name = b.name("x");
    let value = b.literal(lit_int(1));
    let call = b.call(callee, vec![name, value]);
    b.graph.add_top_level_form(call).unwrap();

    let template = b.graph.to_template();
    assert_eq!(template.internal_nodes, vec![callee]);
    let restored = caap_core::IRGraph::from_template(template).unwrap();

    assert!(restored.is_internal_node(callee));
    assert!(!restored.is_internal_node(call));
}

#[test]
fn test_internal_markers_only_annotate_name_nodes() {
    let mut b = GraphBuilder::new();
    let literal = b.literal(lit_int(1));

    let error = b.graph.mark_internal_node(literal).unwrap_err().to_string();

    assert!(error.contains("internal marker can only annotate name nodes"));
}

#[test]
fn test_ir_graph_template_rejects_internal_marker_on_non_name_node() {
    let mut b = GraphBuilder::new();
    let callee = b.name("id");
    let arg = b.literal(lit_int(42));
    let call = b.call(callee, vec![arg]);
    b.graph.add_top_level_form(call).unwrap();

    let mut template = b.graph.to_template();
    template.internal_nodes = vec![arg];
    let error = template.validate().unwrap_err().to_string();

    assert!(error.contains("internal marker can only annotate name nodes"));
}

#[test]
fn test_ir_graph_allocate_id_rejects_overflow_without_advancing() {
    let template = caap_core::IRGraphTemplate {
        root_id: 0,
        nodes: vec![],
        parents: vec![],
        source_spans: vec![],
        internal_nodes: vec![],
        top_level_forms: vec![],
        next_id: u32::MAX,
    };
    let mut graph = caap_core::IRGraph::from_template(template).unwrap();

    let error = graph.allocate_id().unwrap_err().to_string();

    assert!(error.contains("IRGraph node id overflow"));
    assert_eq!(graph.to_template().next_id, u32::MAX);
    assert_eq!(graph.node_count(), 0);
}

#[test]
fn test_ir_graph_set_node_rejects_next_id_overflow_without_mutating() {
    let mut graph = caap_core::IRGraph::new();
    let node = caap_core::Node::Name(caap_core::NameNode::new(u32::MAX, "overflow").unwrap());

    let error = graph.set_node(node, None).unwrap_err().to_string();

    assert!(error.contains("IRGraph node id overflow"));
    assert!(!graph.contains(u32::MAX));
    assert_eq!(graph.to_template().next_id, 0);
}

#[test]
fn test_ir_graph_template_rejects_missing_call_child() {
    let template = caap_core::IRGraphTemplate {
        root_id: 0,
        nodes: vec![caap_core::Node::Call(caap_core::CallNode::new(
            0,
            99,
            vec![],
        ))],
        parents: vec![(0, None)],
        source_spans: vec![],
        internal_nodes: vec![],
        top_level_forms: vec![0],
        next_id: 1,
    };

    assert!(template.validate().is_err());
}

#[test]
fn test_ir_graph_template_rejects_reused_call_child_position() {
    let template = caap_core::IRGraphTemplate {
        root_id: 0,
        nodes: vec![
            caap_core::Node::Call(caap_core::CallNode::new(0, 1, vec![1])),
            caap_core::Node::Name(caap_core::NameNode::new(1, "id").unwrap()),
        ],
        parents: vec![(0, None), (1, Some(0))],
        source_spans: vec![],
        internal_nodes: vec![],
        top_level_forms: vec![0],
        next_id: 2,
    };

    assert!(template.validate().is_err());
    assert!(caap_core::IRGraph::from_template(template).is_err());
}

#[test]
fn test_ir_graph_template_rejects_unlisted_parentless_nodes() {
    let template = caap_core::IRGraphTemplate {
        root_id: 0,
        nodes: vec![
            caap_core::Node::Literal(caap_core::LiteralNode::new(0, lit_int(1))),
            caap_core::Node::Literal(caap_core::LiteralNode::new(1, lit_int(2))),
        ],
        parents: vec![(0, None), (1, None)],
        source_spans: vec![],
        internal_nodes: vec![],
        top_level_forms: vec![0],
        next_id: 2,
    };

    assert!(template.validate().is_err());
}

#[test]
fn test_ir_graph_template_rejects_parent_that_does_not_reference_child() {
    let template = caap_core::IRGraphTemplate {
        root_id: 0,
        nodes: vec![
            caap_core::Node::Literal(caap_core::LiteralNode::new(0, lit_int(1))),
            caap_core::Node::Literal(caap_core::LiteralNode::new(1, lit_int(2))),
        ],
        parents: vec![(0, None), (1, Some(0))],
        source_spans: vec![],
        internal_nodes: vec![],
        top_level_forms: vec![0],
        next_id: 2,
    };

    assert!(template.validate().is_err());
}

#[test]
fn test_ir_graph_template_rejects_child_parent_mismatch() {
    let template = caap_core::IRGraphTemplate {
        root_id: 0,
        nodes: vec![
            caap_core::Node::Call(caap_core::CallNode::new(0, 1, vec![])),
            caap_core::Node::Literal(caap_core::LiteralNode::new(1, lit_int(1))),
        ],
        parents: vec![(0, None), (1, None)],
        source_spans: vec![],
        internal_nodes: vec![],
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
            caap_core::Node::Literal(caap_core::LiteralNode::new(old, lit_int(2)))
        )
        .is_ok());
    assert!(b
        .graph
        .replace_node(
            old,
            caap_core::Node::Literal(caap_core::LiteralNode::new(old + 1, lit_int(3)))
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
        caap_core::Node::Call(call) => assert_eq!(call.args, vec![replacement].into()),
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
    unit.semantics_mut()
        .unwrap()
        .define_symbol(
            caap_core::SymbolEntry::new(
                "x",
                caap_core::SymbolKind::TopLevel,
                caap_core::PhasePolicy::Runtime,
                Some(x),
            )
            .unwrap(),
        )
        .unwrap();

    let err = unit
        .evaluate()
        .expect_err("expected uninitialized top-level error");
    match err {
        caap_core::EvalSignal::Error(error) => {
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

    let two = unit
        .append_ir_top_level_with_spec(&caap_core::ExprSpec::literal(lit_int(2)))
        .unwrap();
    assert_eq!(unit.top_level_form_ids().len(), 2);
    assert!(unit.top_level_form_ids().contains(&two));

    unit.restore_snapshot(snapshot).unwrap();
    assert_eq!(unit.unit_id(), "test.snapshot");
    assert_eq!(unit.top_level_form_ids(), &[one]);
    assert_eq!(unit.ir().node_count(), 1);
}

#[test]
fn test_unit_snapshot_restore_reverts_semantic_and_metadata_state() {
    let mut unit = Unit::empty("test.semantic_snapshot").expect("unit construction failed");
    unit.set_attribute("mode", caap_core::SemanticValue::Str("initial".to_string()))
        .unwrap();
    unit.add_link_binding(caap_core::LinkBinding::new("stdlib.core", "id", "id").unwrap())
        .unwrap();
    unit.semantics_mut()
        .unwrap()
        .define_symbol(
            caap_core::SymbolEntry::new(
                "id",
                caap_core::SymbolKind::Builtin,
                caap_core::PhasePolicy::Runtime,
                None,
            )
            .unwrap(),
        )
        .unwrap();
    let snapshot = unit.snapshot();

    unit.set_attribute("mode", caap_core::SemanticValue::Str("changed".to_string()))
        .unwrap();
    unit.add_link_binding(caap_core::LinkBinding::new("stdlib.math", "add", "add").unwrap())
        .unwrap();
    unit.restore_snapshot(snapshot).unwrap();

    assert_eq!(
        unit.attributes().get("mode"),
        Some(&caap_core::SemanticValue::Str("initial".to_string()))
    );
    assert_eq!(unit.link_bindings().len(), 1);
    assert!(unit.semantics().lookup_symbol("id").unwrap().is_some());
}

#[test]
fn test_unit_syntax_state_and_lifecycle_events_roundtrip() {
    let mut unit = Unit::empty("test.syntax_state").expect("unit construction failed");
    unit.set_syntax_state(
        caap_core::UnitSyntaxState::new("caap")
            .unwrap()
            .with_source("test.caap", "sha256:test")
            .unwrap(),
    )
    .unwrap();
    unit.set_attribute("kind", caap_core::SemanticValue::Str("surface".to_string()))
        .unwrap();
    unit.add_link_binding(
        caap_core::LinkBinding::with_syntax("stdlib.syntax", "quote", "quote", true).unwrap(),
    )
    .unwrap();

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
        vec!["syntax_state", "attribute", "link_binding"]
    );

    let snapshot = unit.snapshot();
    unit.set_syntax_state(caap_core::UnitSyntaxState::new("ir").unwrap())
        .unwrap();
    unit.restore_snapshot(snapshot).unwrap();
    assert_eq!(unit.syntax_state().language, "caap");

    let restored = Unit::from_template(unit.to_template()).unwrap();
    assert_eq!(restored.syntax_state().language, "caap");
    assert_eq!(restored.lifecycle_events().len(), 3);
    assert!(restored.link_bindings()[0].syntax);
}

#[test]
fn test_unit_assembly_pipeline_runs_hooks_and_records_lifecycle() {
    let mut unit = Unit::empty("test.assembly").expect("unit construction failed");
    let mut pipeline = caap_core::UnitAssemblyPipeline::new();
    pipeline
        .register_hook("syntax", |unit| {
            unit.set_syntax_state(caap_core::UnitSyntaxState::new("caap")?)
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .unwrap();
    pipeline
        .register_hook("metadata", |unit| {
            Ok(unit.set_attribute("assembled", caap_core::SemanticValue::Bool(true))?)
        })
        .unwrap();

    assert_eq!(pipeline.hook_names(), vec!["syntax", "metadata"]);
    pipeline.apply(&mut unit).unwrap();

    assert_eq!(unit.syntax_state().language, "caap");
    assert_eq!(
        unit.attributes().get("assembled"),
        Some(&caap_core::SemanticValue::Bool(true))
    );
    assert_eq!(
        unit.lifecycle_events()
            .iter()
            .filter(|event| event.kind == "assembly_hook")
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
    let mut unit = Unit::empty("test.assembly_error").expect("unit construction failed");
    let mut pipeline = caap_core::UnitAssemblyPipeline::new();
    pipeline
        .register_hook("bad", |_unit| Err("assembly failed".to_string()))
        .unwrap();

    let err = pipeline
        .apply(&mut unit)
        .expect_err("failing hook should stop assembly");

    assert_eq!(err.domain(), "unit");
    assert_eq!(
        err.message(),
        "unit assembly hook bad failed: assembly failed"
    );
    assert_eq!(
        unit.lifecycle_events()
            .last()
            .map(|event| event.kind.as_str()),
        Some("assembly_hook_error")
    );
}

#[test]
fn test_unit_transaction_commit_and_rollback_are_explicit() {
    let mut unit = Unit::empty("test.transaction").expect("unit construction failed");
    unit.set_attribute("state", caap_core::SemanticValue::Str("base".to_string()))
        .unwrap();

    let rollback_tx = unit.begin_transaction();
    unit.set_attribute(
        "state",
        caap_core::SemanticValue::Str("changed".to_string()),
    )
    .unwrap();
    unit.rollback_transaction(rollback_tx).unwrap();
    assert_eq!(
        unit.attributes().get("state"),
        Some(&caap_core::SemanticValue::Str("base".to_string()))
    );

    let commit_tx = unit.begin_transaction();
    unit.set_attribute(
        "state",
        caap_core::SemanticValue::Str("committed".to_string()),
    )
    .unwrap();
    let version = unit.commit_transaction(commit_tx).unwrap();
    assert_eq!(unit.version(), version);
    assert_eq!(
        unit.lifecycle_events()
            .last()
            .map(|event| event.kind.as_str()),
        Some("transaction")
    );
    assert_eq!(
        unit.attributes().get("state"),
        Some(&caap_core::SemanticValue::Str("committed".to_string()))
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
    unit.add_link_binding(caap_core::LinkBinding::new("stdlib.core", "id", "id").unwrap())
        .unwrap();
    unit.set_attribute("kind", caap_core::SemanticValue::Str("test".to_string()))
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
    let valid = Unit::empty("test.template_validation")
        .unwrap()
        .to_template();
    let template = caap_core::UnitTemplate {
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
    let source = "(int_add 1\n 2)";
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
        caap_core::Node::Call(call) => call,
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
fn test_frontend_preserves_leading_trivia_in_source_spans() {
    let source = "\n\n(int_add 1 2)";
    let graph = parse(source).expect("parse failed");
    let top_id = graph.top_level_form_ids()[0];
    let top_span = graph
        .source_span(top_id)
        .expect("missing top-level source span");

    assert_eq!(top_span.start, 2);
    assert_eq!(top_span.start_line, 3);
    assert_eq!(top_span.start_col, 1);
}

#[test]
fn test_frontend_preserves_leading_block_comment_in_source_spans() {
    let source = "/* comment */\n(int_add 1 2)";
    let graph = parse(source).expect("parse failed");
    let top_id = graph.top_level_form_ids()[0];
    let top_span = graph
        .source_span(top_id)
        .expect("missing top-level source span");

    assert_eq!(top_span.start, "/* comment */\n".len());
    assert_eq!(top_span.start_line, 2);
    assert_eq!(top_span.start_col, 1);
}

#[test]
fn test_frontend_attaches_source_path_to_file_spans() {
    let source = "(int_add 1 2)";
    let path = "/tmp/source-path-demo.caap";
    let graph = caap_core::parse_with_source_path(source, path).expect("parse failed");
    let top_id = graph.top_level_form_ids()[0];
    let top_span = graph
        .source_span(top_id)
        .expect("missing top-level source span");

    assert_eq!(top_span.path.as_deref(), Some(path));
}

#[test]
fn test_semantic_registry_assigns_stable_ids_and_forks() {
    let mut registry = caap_core::SemanticRegistry::new();
    registry
        .define(caap_core::SemanticEntry::new("add", caap_core::EntrySource::Builtin).unwrap())
        .unwrap();

    let entry = registry.lookup("add").unwrap().unwrap();
    assert_eq!(
        entry.stable_id.as_ref().unwrap().as_str(),
        "semantic:builtin:add"
    );

    let mut child = registry.fork();
    assert!(child.lookup("add").unwrap().is_some());
    child
        .define(caap_core::SemanticEntry::new("local", caap_core::EntrySource::Local).unwrap())
        .unwrap();
    assert!(registry.lookup("local").unwrap().is_none());
    assert!(child.lookup("local").unwrap().is_some());
}

#[test]
fn test_unified_semantic_graph_tracks_symbols_facts_and_snapshots() {
    let mut graph = caap_core::UnifiedSemanticGraph::new();
    let symbol = caap_core::SymbolEntry::new(
        "x",
        caap_core::SymbolKind::TopLevel,
        caap_core::PhasePolicy::Runtime,
        Some(1),
    )
    .unwrap();
    assert!(graph.define_symbol(symbol).unwrap());
    assert_eq!(graph.lookup_symbol("x").unwrap().unwrap().node_id, Some(1));

    let subject = caap_core::node_subject_id(1);
    assert!(graph
        .set_fact(
            subject.clone(),
            "type",
            caap_core::SemanticValue::Str("int".to_string()),
        )
        .unwrap());
    assert_eq!(
        graph.get_fact(&subject, "type").unwrap(),
        Some(&caap_core::SemanticValue::Str("int".to_string()))
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
    let mut graph = caap_core::UnifiedSemanticGraph::new();
    let subject = caap_core::node_subject_id(7);
    graph
        .set_fact(
            subject.clone(),
            "type",
            caap_core::SemanticValue::Str("int".to_string()),
        )
        .unwrap();
    let base_version = graph.version();

    let rollback_tx = graph.begin_transaction();
    graph
        .define_symbol(
            caap_core::SymbolEntry::new(
                "temp",
                caap_core::SymbolKind::Local,
                caap_core::PhasePolicy::Runtime,
                Some(7),
            )
            .unwrap(),
        )
        .unwrap();
    graph
        .set_fact(
            subject.clone(),
            "type",
            caap_core::SemanticValue::Str("string".to_string()),
        )
        .unwrap();
    graph.rollback_transaction(rollback_tx).unwrap();

    assert!(graph.lookup_symbol("temp").unwrap().is_none());
    assert_eq!(
        graph.get_fact(&subject, "type").unwrap(),
        Some(&caap_core::SemanticValue::Str("int".to_string()))
    );
    assert!(graph.version() > base_version);

    let commit_tx = graph.begin_transaction();
    graph
        .define_symbol(
            caap_core::SymbolEntry::new(
                "kept",
                caap_core::SymbolKind::Local,
                caap_core::PhasePolicy::Runtime,
                Some(8),
            )
            .unwrap(),
        )
        .unwrap();
    let committed_version = graph.commit_transaction(commit_tx).unwrap();
    assert_eq!(graph.version(), committed_version);
    assert!(graph.lookup_symbol("kept").unwrap().is_some());
}

#[test]
fn test_unified_semantic_graph_without_facts_rejects_fact_access() {
    let mut graph = caap_core::UnifiedSemanticGraph::without_facts();
    assert!(graph
        .set_fact(
            caap_core::node_subject_id(1),
            "type",
            caap_core::SemanticValue::Str("int".to_string()),
        )
        .is_err());
}

#[test]
fn test_builtin_metadata_classifies_special_forms_and_effects() {
    let ev = Evaluator::new(caap_core::IRGraph::new());

    let if_meta = ev.builtin_metadata("if").expect("missing if builtin");
    assert_eq!(if_meta.eval_policy, caap_core::EvalPolicy::LazyIf);
    assert_eq!(
        if_meta.control_policy,
        caap_core::ControlPolicy::ConditionalBranch
    );
    assert!(!if_meta.eager_args());

    let bind_meta = ev.builtin_metadata("bind").expect("missing bind builtin");
    assert_eq!(
        bind_meta.scope_policy,
        caap_core::ScopePolicy::LexicalBinding
    );
    assert_eq!(bind_meta.visibility, caap_core::BuiltinVisibility::Public);

    assert!(ev.builtin_metadata("set_var").is_none());
    let set_var_meta = ev
        .builtin_metadata("assign_lexical")
        .expect("missing assign-lexical builtin");
    assert_eq!(
        set_var_meta.visibility,
        caap_core::BuiltinVisibility::Internal
    );
    assert!(!ev.builtin_names().contains(&"assign_lexical"));

    let append_meta = ev
        .builtin_metadata("append")
        .expect("missing append builtin");
    assert!(append_meta.effect_policy.allows("mutation"));

    let add_meta = ev
        .builtin_metadata("int_add")
        .expect("missing int-add builtin");
    assert_eq!(add_meta.eval_policy, caap_core::EvalPolicy::Eager);
    assert!(add_meta.effect_policy.is_pure());

    let instantiate_meta = ev
        .builtin_metadata("ctfe_ir_name")
        .expect("missing ctfe-ir-name builtin");
    assert_eq!(
        instantiate_meta.phase_policy,
        caap_core::PhasePolicy::CompileTime
    );
    assert!(instantiate_meta.effect_policy.is_pure());

    let node_meta = ev
        .builtin_metadata("ctfe_node_call_semantics")
        .expect("missing ctfe-node-call-semantics builtin");
    assert_eq!(node_meta.phase_policy, caap_core::PhasePolicy::CompileTime);
    assert!(node_meta.effect_policy.is_pure());

    let annotation_set_meta = ev
        .builtin_metadata("ctfe_meta_annotation_set")
        .expect("missing ctfe-meta-annotation-set builtin");
    assert_eq!(
        annotation_set_meta.phase_policy,
        caap_core::PhasePolicy::CompileTime
    );
    assert!(annotation_set_meta.effect_policy.allows("impure"));

    let unit_template_meta = ev
        .builtin_metadata("ctfe_unit_to_template")
        .expect("missing ctfe-unit-to-template builtin");
    assert_eq!(
        unit_template_meta.phase_policy,
        caap_core::PhasePolicy::CompileTime
    );
    assert!(unit_template_meta.effect_policy.allows("impure"));

    let list_dir_meta = ev
        .builtin_metadata("ctfe_compiler_list_dir")
        .expect("missing ctfe-compiler-list-dir builtin");
    assert_eq!(
        list_dir_meta.phase_policy,
        caap_core::PhasePolicy::CompileTime
    );
    assert!(list_dir_meta.effect_policy.allows("read_files"));

    let load_surface_meta = ev
        .builtin_metadata("ctfe_compiler_load_surface_file_template")
        .expect("missing ctfe-compiler-load-surface-file-template builtin");
    assert!(load_surface_meta.effect_policy.allows("read_files"));
}

// ── literal evaluation ────────────────────────────────────────────────────────
