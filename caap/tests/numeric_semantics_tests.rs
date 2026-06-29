//! Defined arithmetic & comparison semantics (remediation: correctness gaps).
//!
//! i64 arithmetic is CHECKED: overflow is a clean, catchable CAAP error —
//! never a build-profile-dependent panic (debug) or silent wrap (release).
//! Ordered comparison on incomparable types is a TYPE ERROR, not a silent
//! `false` (principle #11). Equality across types stays plain inequality, and
//! `sequence_sort_by`'s total order over heterogeneous keys is unaffected.

use caap_core::frontend::eval_source;
use caap_core::RuntimeValue;

fn ok(src: &str) -> RuntimeValue {
    eval_source(src).expect("eval")
}

fn err(src: &str) -> String {
    format!("{}", eval_source(src).expect_err("must fail"))
}

// ── overflow is an error, edges still work ──────────────────────────────

#[test]
fn int_add_overflow_is_a_clean_error() {
    let e = err("(int_add 9223372036854775807 1)");
    assert!(e.contains("integer overflow"), "{e}");
    assert_eq!(
        ok("(int_add 9223372036854775806 1)"),
        RuntimeValue::Int(i64::MAX)
    );
}

#[test]
fn int_sub_and_mul_overflow_are_errors() {
    assert!(err("(int_sub -9223372036854775808 1)").contains("integer overflow"));
    assert!(err("(int_mul 4611686018427387904 2)").contains("integer overflow"));
}

#[test]
fn int_div_min_by_minus_one_is_an_error_not_a_panic() {
    assert!(err("(int_div -9223372036854775808 -1)").contains("integer overflow"));
    assert_eq!(
        ok("(int_div -9223372036854775808 1)"),
        RuntimeValue::Int(i64::MIN)
    );
}

#[test]
fn int_rem_min_by_minus_one_is_zero() {
    // Mathematically MIN rem -1 = 0; Rust's bare `%` would panic.
    assert_eq!(
        ok("(int_rem -9223372036854775808 -1)"),
        RuntimeValue::Int(0)
    );
}

#[test]
fn int_abs_of_min_is_an_error() {
    assert!(err("(int_abs -9223372036854775808)").contains("integer overflow"));
}

#[test]
fn overflow_uses_the_runtime_error_channel_like_division_by_zero() {
    // Overflow is a runtime ERROR (the same channel as int_div-by-zero) —
    // and since 2026-06-12 that channel is CATCHABLE by `try` (see
    // try_error_tests.rs for the full contract incl. fatal budget piercing).
    assert!(err("(int_add 9223372036854775807 1)").contains("int_add"));
    assert!(err("(int_div 1 0)").contains("division by zero"));
    assert_eq!(
        ok("(try (int_add 9223372036854775807 1) (catch e \"caught\"))"),
        RuntimeValue::Str("caught".into())
    );
}

// ── ordered comparison: typed, no silent false ──────────────────────────

#[test]
fn comparing_incompatible_types_is_a_type_error() {
    let e = err("(lt 1 \"a\")");
    assert!(e.contains("cannot compare int with string"), "{e}");
    assert!(err("(lt true false)").contains("cannot compare bool with bool"));
    assert!(err("(ge null 1)").contains("cannot compare"));
}

#[test]
fn legal_comparisons_are_unchanged() {
    assert_eq!(ok("(lt 1 2)"), RuntimeValue::Bool(true));
    assert_eq!(ok("(lt \"a\" \"b\")"), RuntimeValue::Bool(true));
    assert_eq!(ok("(lt 1 2.5)"), RuntimeValue::Bool(true)); // numeric mix stays
    assert_eq!(ok("(ge 3 3)"), RuntimeValue::Bool(true));
}

#[test]
fn equality_across_types_stays_plain_inequality() {
    assert_eq!(ok("(eq 1 \"1\")"), RuntimeValue::Bool(false));
    assert_eq!(ok("(ne 1 \"1\")"), RuntimeValue::Bool(true));
}

#[test]
fn sort_total_order_over_heterogeneous_keys_is_unaffected() {
    // sequence_sort_by uses its own total order — must keep working.
    assert_eq!(
        ok("(size (sequence_sort_by (list_of 3 \"a\" 1) (lambda (x) x)))"),
        RuntimeValue::Int(3)
    );
}

// ── float bit views: exact codegen emission substrate ───────────────────

#[test]
fn float_to_bits_yields_exact_ieee754_patterns() {
    assert_eq!(
        ok("(float_to_bits 1.0)"),
        RuntimeValue::Int(0x3FF0000000000000)
    );
    assert_eq!(
        ok("(float_to_bits 2.0)"),
        RuntimeValue::Int(0x4000000000000000)
    );
    // -0.0: only the sign bit — as i64 that is i64::MIN
    assert_eq!(ok("(float_to_bits -0.0)"), RuntimeValue::Int(i64::MIN));
    assert_eq!(ok("(float_to_bits_f32 1.0)"), RuntimeValue::Int(0x3F800000));
    // f32 narrowing is part of the contract: 0.1f64 → nearest f32 bits
    assert_eq!(
        ok("(float_to_bits_f32 0.1)"),
        RuntimeValue::Int(i64::from(0.1f32.to_bits()))
    );
    assert!(err("(float_to_bits 1)").contains("expected float"));
}

#[test]
fn bits_to_float_inverts_float_to_bits_exactly() {
    // round-trip: bits_to_float(float_to_bits(x)) == x, bit-for-bit
    assert_eq!(
        ok("(bits_to_float (float_to_bits 12.5))"),
        RuntimeValue::Float(12.5)
    );
    // 0x3FF0000000000000 = 1.0
    assert_eq!(
        ok("(bits_to_float 4607182418800017408)"),
        RuntimeValue::Float(1.0)
    );
    // i64::MIN bits = -0.0: the exact bit pattern (incl. sign) must survive
    match ok("(bits_to_float -9223372036854775808)") {
        RuntimeValue::Float(f) => assert_eq!(f.to_bits(), (-0.0f64).to_bits()),
        other => panic!("expected float, got {other:?}"),
    }
    // f32 inverse: narrow round-trip matches the f32 value widened to f64
    assert_eq!(
        ok("(bits_to_float_f32 (float_to_bits_f32 0.1))"),
        RuntimeValue::Float(0.1f32 as f64)
    );
    // 0x7FF0000000000000 = +inf: reinterprets without panicking
    match ok("(bits_to_float 9218868437227405312)") {
        RuntimeValue::Float(f) => assert!(f.is_infinite() && f.is_sign_positive()),
        other => panic!("expected float, got {other:?}"),
    }
    assert!(err("(bits_to_float 1.0)").contains("expected int"));
}
