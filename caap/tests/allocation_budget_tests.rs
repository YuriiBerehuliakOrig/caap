//! Allocation budget — the memory sibling of the evaluation step budget. The
//! step budget bounds CPU; an `O(1)`-step builtin (`string_repeat`, `list_of`,
//! …) can still allocate `O(limit)` memory, so a hostile loop chaining them
//! would exhaust host memory — aborting the process UNCATCHABLY — before the
//! step budget tripped. The allocation budget bounds that. It is opt-in and
//! scoped exactly like the step budget; `effect_scope` (the kernel's
//! untrusted-code boundary) installs a default so the documented sandbox is
//! actually memory-safe.

use caap_core::{frontend::parse, Evaluator, PhasePolicy, RuntimeValue};

fn run_unbudgeted(src: &str) -> Result<RuntimeValue, String> {
    let graph = parse(src).unwrap();
    Evaluator::with_phase(graph, PhasePolicy::Runtime)
        .run()
        .map_err(|e| e.to_string())
}

fn run_with_alloc_budget(src: &str, budget: usize) -> Result<RuntimeValue, String> {
    let graph = parse(src).unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::Runtime);
    let forms: Vec<_> = ev.graph().top_level_form_ids().to_vec();
    let env = ev.make_env();
    ev.with_eval_alloc_budget(budget, |ev| {
        let mut last = Ok(RuntimeValue::Null);
        for id in &forms {
            last = ev.eval(*id, &env);
            if last.is_err() {
                break;
            }
        }
        last
    })
    .map_err(|e| e.to_string())
}

#[test]
fn an_ample_allocation_budget_evaluates_normally() {
    // 1000 elements, well under the budget.
    let value = run_with_alloc_budget("(size (sequence_range 0 1000))", 1_000_000).unwrap();
    assert_eq!(value, RuntimeValue::Int(1000));
}

#[test]
fn an_exhausted_allocation_budget_fails_cleanly() {
    // A single (sequence_range 0 50000) needs 50_000 units; a 1000-unit budget
    // cannot cover it. Clean structured error, not an OOM abort.
    let err = run_with_alloc_budget("(size (sequence_range 0 50000))", 1_000)
        .expect_err("a tiny budget cannot allocate 50k elements");
    assert!(
        err.contains("allocation budget exhausted"),
        "expected an allocation-budget error, got: {err}"
    );
}

#[test]
fn the_budget_bounds_a_cumulative_loop_of_bounded_collections() {
    // The real hole: each collection is under the per-collection limit, but the
    // loop allocates unboundedly in total. The cumulative budget catches it.
    let src = "(bind go (lambda (self n acc)
                 (if (eq n 0) acc
                   (self self (int_sub n 1)
                     (int_add acc (size (string_repeat \"x\" 100000))))))
               (go go 10000 0))";
    let err = run_with_alloc_budget(src, 1_000_000)
        .expect_err("10000 * 100000 chars far exceeds a 1M budget");
    assert!(
        err.contains("allocation budget exhausted"),
        "expected clean failure, got: {err}"
    );
}

#[test]
fn the_budget_is_restored_after_the_scope() {
    // An exhausted scoped budget must not poison later unbudgeted evaluation on
    // the same evaluator.
    let graph = parse("(size (sequence_range 0 50000))").unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::Runtime);
    let forms: Vec<_> = ev.graph().top_level_form_ids().to_vec();
    let env = ev.make_env();
    let _ = ev.with_eval_alloc_budget(10, |ev| ev.eval(forms[0], &env));
    let value = ev
        .eval(forms[0], &env)
        .expect("unbudgeted evaluation succeeds");
    assert_eq!(value, RuntimeValue::Int(50000));
}

#[test]
fn trusted_top_level_execution_is_unbounded() {
    // No budget is installed for a plain program run — the user's own code is
    // trusted and may allocate freely (here ~5M cumulative across the loop).
    let src = "(bind go (lambda (self n acc)
                 (if (eq n 0) acc
                   (self self (int_sub n 1)
                     (int_add acc (size (sequence_range 0 500000))))))
               (go go 10 0))";
    assert_eq!(run_unbudgeted(src).unwrap(), RuntimeValue::Int(5_000_000));
}

// ── effect_scope: the documented untrusted-code boundary is memory-safe ────

fn run_compile_time(src: &str) -> Result<RuntimeValue, String> {
    let graph = parse(src).unwrap();
    Evaluator::with_phase(graph, PhasePolicy::CompileTime)
        .run()
        .map_err(|e| e.to_string())
}

#[test]
fn effect_scope_installs_a_default_allocation_budget() {
    // A hostile loop inside a pure scope fails cleanly instead of OOM-aborting.
    let src = "(bind go (lambda (self n acc)
                 (if (eq n 0) acc
                   (self self (int_sub n 1)
                     (int_add acc (size (string_repeat \"x\" 1000000))))))
               (effect_scope (list_of)
                 (go go 100000 0)))";
    let err = run_compile_time(src).expect_err("hostile loop must hit the default budget");
    assert!(
        err.contains("allocation budget exhausted"),
        "expected the effect_scope default budget to trip, got: {err}"
    );
}

#[test]
fn effect_scope_still_runs_legitimate_work() {
    let src = "(effect_scope (list_of)
                 (size (sequence_map (sequence_range 0 1000) (lambda (x) (int_mul x 2)))))";
    assert_eq!(run_compile_time(src).unwrap(), RuntimeValue::Int(1000));
}

#[test]
fn nested_effect_scopes_cannot_reset_the_budget() {
    // Re-entering effect_scope must NOT hand the body a fresh budget — otherwise
    // untrusted code could nest scopes to dodge the bound. Inner scopes inherit
    // the outer, already-depleted budget.
    let src = "(bind go (lambda (self n acc)
                 (if (eq n 0) acc
                   (effect_scope (list_of)
                     (self self (int_sub n 1)
                       (int_add acc (size (string_repeat \"x\" 1000000)))))))
               (effect_scope (list_of)
                 (go go 100000 0)))";
    let err = run_compile_time(src).expect_err("nesting must not reset the budget");
    assert!(
        err.contains("allocation budget exhausted"),
        "nested scopes must share the depleting budget, got: {err}"
    );
}

#[test]
fn the_allocation_budget_is_fatal_and_pierces_try() {
    // Like the step budget: a hostile loop must not be able to trap its own
    // resource bound with `try` and keep allocating.
    let src = "(bind go (lambda (self n acc)
                 (if (eq n 0) acc
                   (self self (int_sub n 1)
                     (int_add acc (size (string_repeat \"x\" 1000000))))))
               (effect_scope (list_of)
                 (try (go go 100000 0) (catch e \"trapped\")))";
    let err = run_compile_time(&format!("{src})")).expect_err("budget must pierce try");
    assert!(
        err.contains("allocation budget exhausted"),
        "the allocation budget must be fatal, got: {err}"
    );
}
