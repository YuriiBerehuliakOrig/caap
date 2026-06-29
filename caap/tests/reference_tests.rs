//! First-class mutable references (`RuntimeValue::Ref`): `ref` / `deref` /
//! `set_ref`. References are shared mutable cells — aliasing, mutate-in-place,
//! identity equality — so the language can hold a reference to an object and
//! mutate through it instead of copying.

use caap_core::frontend::eval_source;
use caap_core::RuntimeValue;

fn eval(src: &str) -> RuntimeValue {
    eval_source(src).expect("eval")
}

#[test]
fn deref_reads_the_boxed_value() {
    assert_eq!(eval("(deref (ref 5))"), RuntimeValue::Int(5));
}

#[test]
fn set_ref_mutates_in_place() {
    assert_eq!(
        eval("(bind r (ref 0))\n(set_ref r 42)\n(deref r)"),
        RuntimeValue::Int(42)
    );
}

/// Two bindings to the same cell alias: a write through one is seen through the
/// other (true reference semantics, no copy).
#[test]
fn references_alias_the_same_cell() {
    assert_eq!(
        eval("(bind r (ref 1))\n(bind s r)\n(set_ref s 99)\n(deref r)"),
        RuntimeValue::Int(99)
    );
}

/// A closure capturing a reference shares mutable state across calls.
#[test]
fn closure_captures_a_shared_mutable_cell() {
    let src = "(bind c (ref 0))\n\
               (bind inc (lambda () (set_ref c (int_add (deref c) 1))))\n\
               (inc)\n(inc)\n(inc)\n(deref c)";
    assert_eq!(eval(src), RuntimeValue::Int(3));
}

/// Reference equality is cell identity, not contents.
#[test]
fn reference_equality_is_cell_identity() {
    assert_eq!(eval("(bind r (ref 1))\n(eq r r)"), RuntimeValue::Bool(true));
    assert_eq!(eval("(eq (ref 1) (ref 1))"), RuntimeValue::Bool(false));
}

/// A reference can box any value, including a map — mutating the boxed map is
/// visible through every alias without copying the map.
#[test]
fn reference_to_a_map_shares_mutation() {
    let src = "(bind r (ref (map_of \"n\" 1)))\n\
               (set (deref r) \"n\" 2)\n\
               (get (deref r) \"n\" 0)";
    assert_eq!(eval(src), RuntimeValue::Int(2));
}

#[test]
fn deref_of_non_reference_is_an_error() {
    assert!(eval_source("(deref 5)").is_err());
}

#[test]
fn value_type_of_a_reference_is_ref() {
    assert_eq!(
        eval("(value_type (ref 0))"),
        RuntimeValue::Str("ref".into())
    );
}
