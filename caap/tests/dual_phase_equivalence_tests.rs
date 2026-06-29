//! THE dual-phase law (kernel must-have #1): a pure expression evaluates to
//! the same outcome under `PhasePolicy::CompileTime` (the substrate the
//! partial-evaluation fold runs under) and `PhasePolicy::Runtime`. "Same
//! outcome" is strict: equal values on success, and an error in one phase
//! must be an error in the other (same message). A third law bounds the fold
//! substrate itself: a budgeted compile-time evaluation may fail with a budget
//! error, but when it succeeds it must produce exactly the runtime value —
//! never a different one.
//!
//! Expressions are generated over the pure runtime surface (int arithmetic
//! incl. overflow/zero edges, comparisons incl. type errors, bool logic,
//! strings, if/bind/lambda) with deliberately mixed types, so the error paths
//! are exercised as hard as the value paths. Floats are excluded from v1: NaN
//! breaks naive result equality and deserves its own bit-exact harness.

use caap_core::frontend::parse;
use caap_core::semantic::PhasePolicy;
use caap_core::{Evaluator, RuntimeValue};
use proptest::prelude::*;

/// Outcome of one evaluation, comparable across phases.
#[derive(Debug, Clone, PartialEq)]
enum Outcome {
    Value(RuntimeValue),
    Error(String),
}

fn eval_in_phase(source: &str, phase: PhasePolicy) -> Outcome {
    let graph = parse(source).expect("generated source must parse");
    match Evaluator::with_phase(graph, phase).run() {
        Ok(value) => Outcome::Value(value),
        Err(error) => Outcome::Error(comparable_error(&error.to_string())),
    }
}

/// The error proper, without the runtime-frames trailer: frames legitimately
/// name the executing phase (`phase=CompileTime` vs `phase=Runtime`), which is
/// diagnostic presentation, not semantics. The law compares the error itself.
fn comparable_error(message: &str) -> String {
    message
        .split("\nRuntime frames:")
        .next()
        .unwrap_or(message)
        .to_string()
}

fn eval_budgeted_compile_time(source: &str, budget: usize) -> Outcome {
    let graph = parse(source).expect("generated source must parse");
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    let forms: Vec<_> = ev.graph().top_level_form_ids().to_vec();
    let env = ev.make_env();
    let result = ev.with_eval_step_budget(budget, |ev| {
        let mut last = Ok(RuntimeValue::Null);
        for id in &forms {
            last = ev.eval(*id, &env);
            if last.is_err() {
                break;
            }
        }
        last
    });
    match result {
        Ok(value) => Outcome::Value(value),
        Err(signal) => Outcome::Error(format!("{signal:?}")),
    }
}

// ── expression generator ────────────────────────────────────────────────────

/// Leaf literals: small ints, boundary ints, bools, strings, null — mixed on
/// purpose so generated calls hit both value and type-error paths.
fn leaf() -> impl Strategy<Value = String> {
    prop_oneof![
        (-7i64..=7).prop_map(|n| n.to_string()),
        Just("9223372036854775807".to_string()),
        Just("-9223372036854775808".to_string()),
        Just("1000000007".to_string()),
        Just("true".to_string()),
        Just("false".to_string()),
        Just("null".to_string()),
        Just("\"a\"".to_string()),
        Just("\"bb\"".to_string()),
        Just("\"\"".to_string()),
    ]
}

fn expr() -> impl Strategy<Value = String> {
    leaf().prop_recursive(4, 64, 3, |inner| {
        let bin = prop_oneof![
            Just("int_add"),
            Just("int_sub"),
            Just("int_mul"),
            Just("int_div"),
            Just("int_rem"),
            Just("int_mod"),
            Just("int_and"),
            Just("int_or"),
            Just("int_xor"),
            Just("int_shl"),
            Just("int_shr"),
            Just("eq"),
            Just("ne"),
            Just("lt"),
            Just("gt"),
            Just("le"),
            Just("ge"),
            Just("string_concat_many"),
        ];
        let una = prop_oneof![Just("int_abs"), Just("int_not"), Just("not")];
        prop_oneof![
            // (op a b)
            (bin, inner.clone(), inner.clone()).prop_map(|(op, a, b)| format!("({op} {a} {b})")),
            // (op a)
            (una, inner.clone()).prop_map(|(op, a)| format!("({op} {a})")),
            // (if c t e)
            (inner.clone(), inner.clone(), inner.clone())
                .prop_map(|(c, t, e)| format!("(if {c} {t} {e})")),
            // (bind x v body-using-x)
            (inner.clone(), inner.clone())
                .prop_map(|(v, b)| format!("(bind x {v} (if (eq x x) {b} x))")),
            // ((lambda (y) y) v) — closure round-trip
            inner.clone().prop_map(|v| format!("((lambda (y) y) {v})")),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 512,
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    /// LAW 1+2: the two phases agree exactly — equal values on success, and
    /// errors (with equal messages) on failure.
    #[test]
    fn compile_time_and_runtime_phases_agree(source in expr()) {
        let compile_time = eval_in_phase(&source, PhasePolicy::CompileTime);
        let runtime = eval_in_phase(&source, PhasePolicy::Runtime);
        prop_assert_eq!(
            compile_time, runtime,
            "phase divergence on: {}", source
        );
    }

    /// LAW 3: the budgeted fold substrate never invents a wrong value — when a
    /// budgeted compile-time evaluation succeeds, it equals the runtime result.
    #[test]
    fn budgeted_fold_substrate_matches_runtime_when_it_succeeds(source in expr()) {
        if let Outcome::Value(folded) = eval_budgeted_compile_time(&source, 5_000) {
            match eval_in_phase(&source, PhasePolicy::Runtime) {
                Outcome::Value(run) => prop_assert_eq!(
                    folded, run, "budgeted fold diverged on: {}", source
                ),
                Outcome::Error(error) => prop_assert!(
                    false,
                    "budgeted fold produced {:?} but runtime errors with {} on: {}",
                    folded, error, source
                ),
            }
        }
        // Budget errors / declines impose no constraint: the call simply
        // stays for runtime. That is the fold contract.
    }
}

// ── pinned regressions: the edges that motivated this law ──────────────────

#[test]
fn pinned_edges_agree_across_phases() {
    for source in [
        "(int_add 9223372036854775807 1)", // overflow → error in BOTH phases
        "(int_div 1 0)",                   // division by zero
        "(int_div -9223372036854775808 -1)", // MIN / -1 overflow
        "(int_rem -9223372036854775808 -1)", // defined as 0
        "(int_abs -9223372036854775808)",  // abs(MIN) overflow
        "(lt 1 \"a\")",                    // comparison type error
        "(lt true false)",                 // bools are unordered
        "(eq 1 \"1\")",                    // cross-type equality is false, not error
        "(int_shl 1 63)",                  // boundary shift
        "(int_shl 1 64)",                  // out-of-range shift → error
    ] {
        let compile_time = eval_in_phase(source, PhasePolicy::CompileTime);
        let runtime = eval_in_phase(source, PhasePolicy::Runtime);
        assert_eq!(compile_time, runtime, "phase divergence on: {source}");
    }
}
