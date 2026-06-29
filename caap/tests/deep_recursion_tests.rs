//! The evaluator grows the native stack on demand (stacker::maybe_grow in
//! Evaluator::eval), so recursion depth is bounded by the max_eval_depth work
//! policy — not by thread stack size. This drives recursion far past what the
//! old fixed-stack setup allowed (depth cap was 10_000 precisely because ~8 MiB
//! default stacks overflowed near there; RUST_MIN_STACK=33MiB is gone from the
//! repo). Run on a plain thread with no stack tuning.

use caap_core::frontend::eval_source;
use caap_core::RuntimeValue;

/// 50_000 self-recursive frames — needs well over 50 MiB of native stack
/// without on-demand growth, i.e. impossible on a default 8 MiB thread.
#[test]
fn recursion_50k_deep_succeeds_on_default_stack() {
    // NON-tail recursion (the +1 keeps a live continuation per frame): a tail
    // self-call would be trampolined to constant depth and test nothing here.
    let src = "(bind count_up\n\
                 (lambda (n) (if (eq n 0) 0 (int_add 1 (count_up (int_sub n 1)))))\n\
                 (count_up 50000))";
    assert_eq!(
        eval_source(src).expect("deep recursion"),
        RuntimeValue::Int(50000)
    );
}

/// Past the depth budget the evaluator must fail with a clean error — the
/// policy guard, not a stack abort.
#[test]
fn runaway_recursion_fails_cleanly_at_the_depth_budget() {
    // Non-tail on purpose: a runaway TAIL loop is depth-constant by design
    // (bounded by the step budget where one is active, like `while`).
    let src = "(bind spin (lambda (n) (int_add 1 (spin (int_add n 1)))) (spin 0))";
    let err = eval_source(src).expect_err("must hit the depth budget");
    assert!(
        format!("{err}").contains("depth"),
        "expected a depth-budget error, got: {err}"
    );
}
