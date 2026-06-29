//! `try` catches evaluation ERRORS, not only `throw`n values (must-have #2,
//! decided 2026-06-12). The handler receives the error as a map
//! {"message": str, "category": str|null}. FATAL errors — step/depth budget
//! exhaustion — pierce `try` by design: catching them would void the resource
//! guarantee that bounds hostile folds. Unlocks validate-before-eval removal
//! across the lib, recoverable parse_float, recoverable `const`.

use caap_core::frontend::eval_source;
use caap_core::semantic::PhasePolicy;
use caap_core::{frontend::parse, Evaluator, RuntimeValue};

fn ok(src: &str) -> RuntimeValue {
    eval_source(src).expect("eval")
}

#[test]
fn try_catches_arithmetic_and_type_errors() {
    assert_eq!(
        ok(r#"(try (int_div 1 0) (catch e "recovered"))"#),
        RuntimeValue::Str("recovered".into())
    );
    assert_eq!(
        ok(r#"(try (int_add 9223372036854775807 1) (catch e 42))"#),
        RuntimeValue::Int(42)
    );
    assert_eq!(
        ok(r#"(try (lt 1 "a") (catch e "type"))"#),
        RuntimeValue::Str("type".into())
    );
    assert_eq!(
        ok(r#"(try (string_to_float "not a number") (catch e -1.0))"#),
        RuntimeValue::Float(-1.0)
    );
}

#[test]
fn the_handler_receives_message_and_category() {
    assert_eq!(
        ok(
            r#"(try (int_div 1 0) (catch e (string_contains (get e "message" "") "division by zero")))"#
        ),
        RuntimeValue::Bool(true)
    );
    // ordinary evaluation errors carry no category
    assert_eq!(
        ok(r#"(try (int_div 1 0) (catch e (value_type (get e "category" "missing"))))"#),
        RuntimeValue::Str("null".into())
    );
}

#[test]
fn thrown_values_still_arrive_as_is() {
    assert_eq!(
        ok(r#"(try (throw 7) (catch e (int_add e 1)))"#),
        RuntimeValue::Int(8)
    );
    // handler-less try swallows both channels into null
    assert_eq!(ok(r#"(try (int_div 1 0))"#), RuntimeValue::Null);
    assert_eq!(ok(r#"(try (throw 7))"#), RuntimeValue::Null);
}

#[test]
fn unknown_name_errors_are_catchable() {
    assert_eq!(
        ok(r#"(try (no_such_function_anywhere 1) (catch e "caught"))"#),
        RuntimeValue::Str("caught".into())
    );
}

#[test]
fn budget_exhaustion_pierces_try() {
    // A hostile fold must not trap its own step budget: the error escapes the
    // try INSIDE the budgeted extent.
    let graph = parse(
        r#"(bind spin (lambda (n) (spin (int_add n 1)))
             (try (spin 0) (catch e "trapped")))"#,
    )
    .unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);
    let forms: Vec<_> = ev.graph().top_level_form_ids().to_vec();
    let env = ev.make_env();
    let result = ev.with_eval_step_budget(5_000, |ev| {
        let mut last = Ok(RuntimeValue::Null);
        for id in &forms {
            last = ev.eval(*id, &env);
            if last.is_err() {
                break;
            }
        }
        last
    });
    let err = result.expect_err("budget must escape the try");
    assert!(
        format!("{err:?}").contains("step budget exhausted"),
        "got: {err:?}"
    );
}

#[test]
fn depth_exhaustion_pierces_try() {
    // Non-tail recursion: a tail self-call would be trampolined to constant
    // depth and never reach the depth budget.
    let err = eval_source(
        r#"(bind spin (lambda (n) (int_add 1 (spin (int_add n 1))))
             (try (spin 0) (catch e "trapped")))"#,
    )
    .expect_err("depth budget must escape the try");
    assert!(format!("{err}").contains("depth"), "got: {err}");
}

#[test]
fn dual_phase_law_extends_to_caught_errors() {
    let src = r#"(try (int_div 1 0) (catch e (get e "message" "")))"#;
    let run = |phase| {
        let graph = parse(src).unwrap();
        Evaluator::with_phase(graph, phase).run().expect("caught")
    };
    assert_eq!(run(PhasePolicy::CompileTime), run(PhasePolicy::Runtime));
}
