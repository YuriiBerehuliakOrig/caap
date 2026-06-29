//! `ctfe_kernel_vocabulary` carries every builtin's value signature
//! (params/result in stdlib type notation; `*`-prefixed final param =
//! "remaining args of this type"; undeclared builtins surface the polymorphic
//! `["*any"] -> "any"`). The lock asserts EVERY entry carries both keys, so
//! stdlib's checker can drop its manual tables.

use caap_core::{frontend::parse, RuntimeValue, Unit};

mod common;

fn vocab_probe(body: &str) -> RuntimeValue {
    let mut compiler = common::session();
    let source = format!("(bind v (ctfe_kernel_vocabulary) {body})");
    let graph = parse(&source).unwrap();
    let unit = Unit::from_graph("vocab", graph).unwrap();
    compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap()
}

#[test]
fn declared_signatures_surface_exactly() {
    let value = vocab_probe(
        r#"(list_of
            (get (get v "int_add" null) "params" null)
            (get (get v "int_add" null) "result" null)
            (get (get v "sqrt" null) "params" null)
            (get (get v "string_concat_many" null) "params" null)
            (get (get v "sequence_map" null) "result" null)
            ;; undeclared (host-object CTFE) → polymorphic default
            (get (get v "ctfe_node_kind" null) "params" null)
            (get (get v "ctfe_node_kind" null) "result" null))"#,
    );
    let RuntimeValue::List(items) = value else {
        panic!("expected list")
    };
    let items = items.borrow();
    assert_eq!(format!("{}", items[0]), "[int, int]");
    assert_eq!(items[1], RuntimeValue::Str("int".into()));
    assert_eq!(format!("{}", items[2]), "[float]");
    assert_eq!(format!("{}", items[3]), "[*string]");
    assert_eq!(items[4], RuntimeValue::Str("list".into()));
    assert_eq!(format!("{}", items[5]), "[*any]");
    assert_eq!(items[6], RuntimeValue::Str("any".into()));
}

#[test]
fn every_vocabulary_entry_carries_params_and_result() {
    let value = vocab_probe(
        r#"(bind bad (sequence_filter (map_keys v)
                       (lambda (k)
                         (bind e (get v k null)
                           (or (eq (get e "params" null) null)
                               (eq (get e "result" null) null)))))
            (size bad))"#,
    );
    assert_eq!(value, RuntimeValue::Int(0), "entries missing params/result");
}
