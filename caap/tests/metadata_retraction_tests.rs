//! Fact/annotation retraction (`ctfe_meta_fact_delete` / `ctfe_meta_annotation_delete`)
//! and the new surface literal constructors (`ctfe_surface_form_bool` / `_float`).
//! Retraction is a tombstone from the current version onward: history stays for
//! older-version queries, and a later re-set becomes visible again.

use caap_core::{frontend::parse, RuntimeValue, Unit};

mod common;

fn run(case_name: &str, body: &str) -> RuntimeValue {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-retract-{}-{}.caap",
        case_name,
        std::process::id()
    ));
    std::fs::write(&path, "(int_add 1 2)\n").unwrap();
    let source = format!(
        "(bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
          (bind node (get (ctfe_unit_top_level_forms unit) 0)
            {body}))",
        path.display()
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph(case_name, graph).unwrap();
    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();
    let _ = std::fs::remove_file(&path);
    value
}

#[test]
fn fact_delete_retracts_and_reset_revives() {
    let value = run(
        "fact",
        r#"(do
            (ctfe_meta_fact_set_by_key node "demo.fact" "v1")
            (bind first (ctfe_meta_fact_has_by_key node "demo.fact")
            (bind deleted (ctfe_meta_fact_delete node "demo.fact")
            (bind gone (ctfe_meta_fact_has_by_key node "demo.fact")
            (bind missing_delete (ctfe_meta_fact_delete node "demo.fact")
            (do
              (ctfe_meta_fact_set_by_key node "demo.fact" "v2")
              (list_of first deleted gone missing_delete
                       (ctfe_meta_fact_get_by_key node "demo.fact"))))))))"#,
    );
    let RuntimeValue::List(items) = value else {
        panic!("expected list, got {value:?}")
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true), "set visible");
    assert_eq!(items[1], RuntimeValue::Bool(true), "delete reports change");
    assert_eq!(
        items[2],
        RuntimeValue::Bool(false),
        "fact gone after delete"
    );
    assert_eq!(
        items[3],
        RuntimeValue::Bool(false),
        "second delete is a no-op"
    );
    assert_eq!(items[4], RuntimeValue::Str("v2".into()), "re-set revives");
}

#[test]
fn annotation_delete_mirrors_fact_delete() {
    let value = run(
        "ann",
        r#"(do
            (ctfe_meta_annotation_set node "demo.ann" 7)
            (bind deleted (ctfe_meta_annotation_delete node "demo.ann")
              (list_of deleted (ctfe_meta_annotation_get node "demo.ann" "absent"))))"#,
    );
    let RuntimeValue::List(items) = value else {
        panic!("expected list, got {value:?}")
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Str("absent".into()));
}

#[test]
fn surface_bool_and_float_constructors_build_typed_atoms() {
    let value = run(
        "forms",
        r#"(bind span (ctfe_unit_node_span unit node)
            (list_of
              (get (ctfe_surface_unwrap (ctfe_surface_form_bool true span)) "value")
              (get (ctfe_surface_unwrap (ctfe_surface_form_float 2.5 span)) "value")
              (get (ctfe_surface_unwrap (ctfe_surface_form_bool false span)) "kind")
              (get (ctfe_surface_unwrap (ctfe_surface_form_float 1.0 span)) "kind")))"#,
    );
    let RuntimeValue::List(items) = value else {
        panic!("expected list, got {value:?}")
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Float(2.5));
    assert_eq!(items[2], RuntimeValue::Str("boolean".into()));
    assert_eq!(items[3], RuntimeValue::Str("float".into()));
}
