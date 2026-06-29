//! Tail-call optimization (TCO): a tail call whose callee evaluates to the
//! closure CURRENTLY EXECUTING reuses the frame via the trampoline in
//! `Evaluator::invoke_closure` — self-recursive tail loops run at constant
//! evaluation depth. Tail positions: the lambda body's last form, composed
//! through the transparent kernel forms `if` / `do` / `bind` / `match`.
//! Everything else (arguments, `try` bodies, custom callables) evaluates
//! non-tail, so semantics are unchanged there.

use caap_core::frontend::{eval_source, parse};
use caap_core::{Evaluator, PhasePolicy, RuntimeValue};

#[test]
fn self_param_tail_loop_runs_at_constant_depth() {
    // 400_000 iterations — well above the depth budget (the try test below hits it
    // execution can finish this.
    let src = "(bind loop (lambda (self n acc)\n\
                 (if (eq n 0) acc (self self (int_sub n 1) (int_add acc 1))))\n\
                 (loop loop 400000 0))";
    assert_eq!(
        eval_source(src).expect("tail loop"),
        RuntimeValue::Int(400000)
    );
}

#[test]
fn named_self_recursion_gets_tco() {
    // Identity is by closure VALUE, not syntax: a named binding resolving to
    // the executing closure is the same self-call.
    let src = "(bind countdown (lambda (n)\n\
                 (if (eq n 0) \"done\" (countdown (int_sub n 1))))\n\
                 (countdown 400000))";
    assert_eq!(
        eval_source(src).expect("named tail loop"),
        RuntimeValue::Str("done".into())
    );
}

#[test]
fn tail_positions_compose_through_do_bind_and_match() {
    let src = r#"(bind via_match (lambda (n)
                    (match n (0 "m") (_ (via_match (int_sub n 1)))))
                  (bind via_bind (lambda (n)
                    (if (eq n 0) "b" (bind step (int_sub n 1) (via_bind step))))
                    (bind via_do (lambda (n)
                      (if (eq n 0) "d" (do (int_add 1 1) (via_do (int_sub n 1)))))
                      (list_of (via_match 400000) (via_bind 400000) (via_do 400000)))))"#;
    let RuntimeValue::List(items) = eval_source(src).expect("composed tails") else {
        panic!("expected list")
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("m".into()));
    assert_eq!(items[1], RuntimeValue::Str("b".into()));
    assert_eq!(items[2], RuntimeValue::Str("d".into()));
}

#[test]
fn non_tail_self_calls_keep_their_semantics() {
    // The +1 consumes the recursive value — not a tail call; the result must
    // be the true sum, proving no frame was skipped.
    let src = "(bind f (lambda (n) (if (eq n 0) 0 (int_add 1 (f (int_sub n 1)))))\n\
                 (f 1000))";
    assert_eq!(eval_source(src).expect("non-tail"), RuntimeValue::Int(1000));
}

#[test]
fn non_tail_recursion_is_still_bounded_by_the_depth_budget() {
    let src = "(bind f (lambda (n) (int_add 1 (f (int_add n 1)))) (f 0))";
    let err = eval_source(src).expect_err("must hit the depth budget");
    assert!(format!("{err}").contains("depth"), "got: {err}");
}

#[test]
fn a_tail_call_inside_try_is_not_optimized() {
    // The `try` handler frame must survive until the body returns, so its body
    // is NOT a tail position: deep recursion through try hits the depth budget
    // (correctness over optimization), while shallow recursion works.
    let shallow = r#"(bind f (lambda (n)
                       (if (eq n 0) "ok" (try (f (int_sub n 1)) (catch e "caught"))))
                       (f 50))"#;
    assert_eq!(
        eval_source(shallow).expect("shallow try recursion"),
        RuntimeValue::Str("ok".into())
    );
    let deep = r#"(bind f (lambda (n)
                    (if (eq n 0) "ok" (try (f (int_sub n 1)) (catch e (throw e)))))
                    (f 300000))"#;
    let err = eval_source(deep).expect_err("try frames must stack");
    assert!(format!("{err}").contains("depth"), "got: {err}");
}

#[test]
fn an_infinite_tail_loop_is_bounded_by_the_step_budget() {
    // A runaway TAIL loop is depth-constant by design (like `while true`);
    // under a scoped step budget it must still die with the FATAL budget error
    // — and `try` must not trap it.
    let graph = parse(
        r#"(bind spin (lambda (n) (spin (int_add n 1)))
             (try (spin 0) (catch e "trapped")))"#,
    )
    .unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    let forms: Vec<_> = ev.graph().top_level_form_ids().to_vec();
    let env = ev.make_env();
    let result = ev.with_eval_step_budget(10_000, |ev| {
        let mut last = Ok(RuntimeValue::Null);
        for id in &forms {
            last = ev.eval(*id, &env);
            if last.is_err() {
                break;
            }
        }
        last
    });
    let err = result.expect_err("step budget must stop the loop");
    assert!(
        format!("{err:?}").contains("step budget exhausted"),
        "got: {err:?}"
    );
}

#[test]
fn mutual_recursion_is_unoptimized_but_correct() {
    // Only SELF-calls are trampolined; a↔b recursion keeps frames (and the
    // depth budget) but must compute the right answer.
    let src = r#"(bind ((even? (lambda (n) (if (eq n 0) true (odd? (int_sub n 1)))))
                        (odd?  (lambda (n) (if (eq n 0) false (even? (int_sub n 1))))))
                   (list_of (even? 10000) (odd? 10001)))"#;
    let RuntimeValue::List(items) = eval_source(src).expect("mutual recursion") else {
        panic!("expected list")
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Bool(true));
}
