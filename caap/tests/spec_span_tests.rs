//! Spans on detached specs (`ctfe_spec_span`) and origin-span propagation in
//! the `syntax_*` constructors: expander-generated trees keep pointing at the
//! user forms they came from; absent spans stay null — never fabricated.

use caap_core::{frontend::parse, RuntimeValue, Unit};

mod common;

fn run(case: &str, body: &str) -> RuntimeValue {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!("caap-span-{case}-{}.caap", std::process::id()));
    std::fs::write(&path, "\n\n(int_add 1 2)\n").unwrap(); // форма на 3-му рядку
    let source = format!(
        "(bind unit (ctfe_compiler_load_surface_file_template compiler {:?})
          (bind node (get (ctfe_unit_top_level_forms unit) 0)
            {body}))",
        path.display()
    );
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph(case, graph).unwrap();
    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();
    let _ = std::fs::remove_file(&path);
    value
}

fn as_list(value: RuntimeValue) -> Vec<RuntimeValue> {
    let RuntimeValue::List(items) = value else {
        panic!("expected list, got {value:?}")
    };
    let items = items.borrow();
    items.clone()
}

#[test]
fn node_to_spec_preserves_the_span_and_spec_span_reads_it() {
    let items = as_list(run(
        "roundtrip",
        r#"(bind spec (ctfe_node_to_spec node)
            (bind span (ctfe_spec_span spec)
              (list_of (value_type span) (get span "start_line" 0))))"#,
    ));
    assert_eq!(items[0], RuntimeValue::Str("map".into()));
    assert_eq!(items[1], RuntimeValue::Int(3), "form sits on line 3");
}

#[test]
fn built_specs_without_metadata_have_null_span() {
    let items = as_list(run(
        "null",
        r#"(list_of
            (value_type (ctfe_spec_span (ctfe_ir_name (map_of "identifier" "x"))))
            (value_type (ctfe_spec_span (syntax_name "y"))))"#,
    ));
    assert_eq!(items[0], RuntimeValue::Str("null".into()));
    assert_eq!(items[1], RuntimeValue::Str("null".into()));
}

#[test]
fn syntax_constructors_inherit_origin_spans() {
    let items = as_list(run(
        "origin",
        r#"(bind origin (ctfe_node_to_spec node)
            (bind named (syntax_name "generated" origin)
            (bind lit (syntax_literal 7 origin)
            (bind call (syntax_call (syntax_name "f") (list_of named))
              (list_of
                (get (ctfe_spec_span named) "start_line" 0)
                (get (ctfe_spec_span lit) "start_line" 0)
                ;; call успадковує з першого spanned-нащадка (named arg)
                (get (ctfe_spec_span call) "start_line" 0))))))"#,
    ));
    assert_eq!(items[0], RuntimeValue::Int(3));
    assert_eq!(items[1], RuntimeValue::Int(3));
    assert_eq!(items[2], RuntimeValue::Int(3));
}
