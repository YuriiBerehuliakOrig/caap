//! `ctfe_kernel_vocabulary` session cache: repeated calls reuse the built
//! vocabulary (it used to materialize several times per bootstrap), callers
//! always get a detached copy (mutation cannot poison the cache), and
//! registering a builtin invalidates it.

use caap_core::values::BuiltinMetadata;
use caap_core::{frontend::parse, Evaluator, PhasePolicy, RuntimeValue};

#[test]
fn repeated_calls_agree_structurally_but_are_detached() {
    let graph = parse(
        r#"(bind a (ctfe_kernel_vocabulary)
             (bind b (ctfe_kernel_vocabulary)
               (do
                 (append (get (get a "eq") "params") "EVIL")
                 (bind c (ctfe_kernel_vocabulary)
                   (list_of
                     (eq a b)
                     (value_eq b c)
                     (size (get (get a "eq") "params"))
                     (size (get (get c "eq") "params")))))))"#,
    )
    .unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    let RuntimeValue::List(items) = ev.run().expect("vocabulary calls") else {
        panic!("expected list");
    };
    let items = items.borrow();
    assert_eq!(
        items[0],
        RuntimeValue::Bool(false),
        "each call returns a fresh handle"
    );
    assert_eq!(
        items[1],
        RuntimeValue::Bool(true),
        "structurally identical across calls"
    );
    assert_eq!(items[2], RuntimeValue::Int(2), "caller's copy mutated");
    assert_eq!(
        items[3],
        RuntimeValue::Int(1),
        "the cache was NOT poisoned by the caller's mutation"
    );
}

#[test]
fn registering_a_builtin_invalidates_the_cache() {
    let graph = parse("(contains (ctfe_kernel_vocabulary) \"c6_probe_builtin\")").unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    assert_eq!(
        ev.run().expect("first vocabulary"),
        RuntimeValue::Bool(false),
        "probe builtin not registered yet"
    );
    ev.register_eager(
        "c6_probe_builtin",
        0,
        Some(0),
        BuiltinMetadata::eager_runtime(),
        |_| Ok(RuntimeValue::Null),
    );
    assert_eq!(
        ev.run().expect("second vocabulary"),
        RuntimeValue::Bool(true),
        "registration must invalidate the session cache"
    );
}

#[test]
fn cached_vocabulary_round_trips_key_content() {
    // Sanity on the detached copy: a known entry keeps its shape.
    let graph = parse(
        r#"(bind v (ctfe_kernel_vocabulary)
             (bind v2 (ctfe_kernel_vocabulary)
               (get (get v2 "string_chars") "kind")))"#,
    )
    .unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    let value = ev.run().expect("cached entry");
    assert_eq!(value, RuntimeValue::Str("builtin".into()));
}
