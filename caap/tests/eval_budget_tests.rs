//! Unit tests for the compile-time evaluation step budget (partial evaluation,
//! phase 2). The budget bounds a scoped reduction so a non-terminating
//! compile-time computation fails with a structured error instead of hanging
//! the compiler. It is opt-in: plain evaluation stays unbounded.

use caap_core::{
    graph::GraphBuilder,
    ir::{IrLiteralData, NodeId},
    values::Environment,
    Evaluator, RuntimeValue,
};

/// Build `(int-add 1 2)` and return its root node id (several IR nodes: a call,
/// its callee name, and two literals — comfortably more than one eval step).
fn add_one_two(b: &mut GraphBuilder) -> NodeId {
    let callee = b.try_name("int_add").unwrap();
    let one = b.try_literal(IrLiteralData::Int(1)).unwrap();
    let two = b.try_literal(IrLiteralData::Int(2)).unwrap();
    b.try_call(callee, vec![one, two]).unwrap()
}

#[test]
fn ample_step_budget_evaluates_normally() {
    let mut b = GraphBuilder::new();
    let root = add_one_two(&mut b);
    b.graph.root_id = root;
    let env = Environment::new(None);
    let mut ev = Evaluator::new(std::mem::take(&mut b.graph));

    let value = ev
        .with_eval_step_budget(1_000, |ev| ev.eval(root, &env))
        .expect("an ample budget evaluates to completion");
    assert_eq!(value, RuntimeValue::Int(3));
}

#[test]
fn exhausted_step_budget_fails_cleanly() {
    let mut b = GraphBuilder::new();
    let root = add_one_two(&mut b);
    b.graph.root_id = root;
    let env = Environment::new(None);
    let mut ev = Evaluator::new(std::mem::take(&mut b.graph));

    // A budget of one step cannot cover the whole expression: evaluation fails
    // with a structured budget error rather than hanging or panicking.
    let error = ev
        .with_eval_step_budget(1, |ev| ev.eval(root, &env))
        .expect_err("a one_step budget cannot evaluate (int_add 1 2)");
    assert!(
        format!("{error}").contains("step budget exhausted"),
        "expected a step-budget error, got: {error}"
    );
}

#[test]
fn budget_is_restored_after_the_scope() {
    let mut b = GraphBuilder::new();
    let root = add_one_two(&mut b);
    b.graph.root_id = root;
    let env = Environment::new(None);
    let mut ev = Evaluator::new(std::mem::take(&mut b.graph));

    // A scoped budget that is exhausted must not poison later unbounded
    // evaluation on the same evaluator: the previous (unbounded) budget is
    // restored when the scope returns.
    let _ = ev.with_eval_step_budget(1, |ev| ev.eval(root, &env));
    let value = ev.eval(root, &env).expect("unbounded evaluation succeeds");
    assert_eq!(value, RuntimeValue::Int(3));
}
