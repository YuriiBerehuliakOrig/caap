/// Arithmetic and comparison builtins — port of `caap/builtins/lang/arithmetic.py`.
use super::args::require_int;
use crate::eval::{eval_args, Evaluator};
use crate::values::{eval_err, RuntimeValue};

fn require_float(v: &RuntimeValue, context: &str) -> Result<f64, crate::values::EvalSignal> {
    match v {
        RuntimeValue::Float(f) => Ok(*f),
        other => Err(eval_err(format!("{context}: expected float, got {other}"))),
    }
}

fn float_to_int(value: f64) -> Result<i64, crate::values::EvalSignal> {
    const I64_MIN_F64: f64 = -9_223_372_036_854_775_808.0;
    const I64_MAX_EXCLUSIVE_F64: f64 = 9_223_372_036_854_775_808.0;
    if !value.is_finite() {
        return Err(eval_err("float_to_int: value must be finite"));
    }
    if !(I64_MIN_F64..I64_MAX_EXCLUSIVE_F64).contains(&value) {
        return Err(eval_err("float_to_int: value is outside int range"));
    }
    Ok(value.trunc() as i64)
}

/// i64 arithmetic is **checked**: overflow is a clean, catchable CAAP error,
/// never a build-profile-dependent panic (debug) or silent wrap (release).
/// Matches the float side's precedent (`float_div` rejects division by zero).
fn checked(
    context: &str,
    l: i64,
    r: i64,
    op: &str,
    v: Option<i64>,
) -> Result<RuntimeValue, crate::values::EvalSignal> {
    v.map(RuntimeValue::Int)
        .ok_or_else(|| eval_err(format!("{context}: integer overflow ({l} {op} {r})")))
}

fn int_add_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    let (l, r) = (
        require_int(&args[0], "int_add")?,
        require_int(&args[1], "int_add")?,
    );
    checked("int_add", l, r, "+", l.checked_add(r))
}

fn int_sub_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    let (l, r) = (
        require_int(&args[0], "int_sub")?,
        require_int(&args[1], "int_sub")?,
    );
    checked("int_sub", l, r, "-", l.checked_sub(r))
}

fn int_mul_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    let (l, r) = (
        require_int(&args[0], "int_mul")?,
        require_int(&args[1], "int_mul")?,
    );
    checked("int_mul", l, r, "*", l.checked_mul(r))
}

fn int_abs_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    let v = require_int(&args[0], "int_abs")?;
    v.checked_abs()
        .map(RuntimeValue::Int)
        .ok_or_else(|| eval_err(format!("int_abs: integer overflow (abs({v}))")))
}

fn eq_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    if args.len() < 2 {
        return Ok(RuntimeValue::Bool(true));
    }
    let first = &args[0];
    Ok(RuntimeValue::Bool(args[1..].iter().all(|v| v == first)))
}

fn ne_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    if args.len() < 2 {
        return Ok(RuntimeValue::Bool(false));
    }
    let first = &args[0];
    Ok(RuntimeValue::Bool(args[1..].iter().any(|v| v != first)))
}

fn value_eq_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    if args.len() < 2 {
        return Ok(RuntimeValue::Bool(true));
    }
    let first = &args[0];
    Ok(RuntimeValue::Bool(
        args[1..]
            .iter()
            .all(|v| super::value_compare::deep_equal(first, v)),
    ))
}

fn value_compare_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    // Total structural order over any two values; -1/0/+1. Equal IFF value_eq.
    let ord = super::value_compare::deep_compare(&args[0], &args[1]);
    Ok(RuntimeValue::Int(match ord {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }))
}

fn value_hash_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    // Structural hash to an i64; value_eq-equal values hash equal.
    Ok(RuntimeValue::Int(super::value_compare::deep_hash(&args[0])))
}

fn lt_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    Ok(RuntimeValue::Bool(super::value_compare::all_lt(&args)?))
}

fn gt_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    Ok(RuntimeValue::Bool(super::value_compare::all_gt(&args)?))
}

fn le_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    Ok(RuntimeValue::Bool(super::value_compare::all_le(&args)?))
}

fn ge_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    Ok(RuntimeValue::Bool(super::value_compare::all_ge(&args)?))
}

fn not_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    Ok(RuntimeValue::Bool(!crate::values::is_truthy(&args[0])))
}

pub fn register(ev: &mut Evaluator) {
    ev.register_eager(
        "int_add",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int", "int"], "int"),
        int_add_eager,
    );

    ev.register_eager(
        "int_sub",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int", "int"], "int"),
        int_sub_eager,
    );

    ev.register_eager(
        "int_mul",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int", "int"], "int"),
        int_mul_eager,
    );

    ev.register_special(
        "int_div",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int", "int"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let l = require_int(&args[0], "int_div")?;
            let r = require_int(&args[1], "int_div")?;
            if r == 0 {
                return Err(eval_err("int_div: division by zero"));
            }
            // i64::MIN / -1 overflows (the true quotient is i64::MAX + 1).
            l.checked_div_euclid(r)
                .map(RuntimeValue::Int)
                .ok_or_else(|| eval_err(format!("int_div: integer overflow ({l} / {r})")))
        },
    );

    ev.register_special(
        "int_rem",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int", "int"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let l = require_int(&args[0], "int_rem")?;
            let r = require_int(&args[1], "int_rem")?;
            if r == 0 {
                return Err(eval_err("int_rem: division by zero"));
            }
            // i64::MIN % -1 is mathematically 0 but panics in Rust's `%`.
            Ok(RuntimeValue::Int(l.checked_rem(r).unwrap_or(0)))
        },
    );

    ev.register_special(
        "int_mod",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int", "int"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let l = require_int(&args[0], "int_mod")?;
            let r = require_int(&args[1], "int_mod")?;
            if r <= 0 {
                return Err(eval_err("int_mod: modulus must be positive"));
            }
            Ok(RuntimeValue::Int(l.rem_euclid(r)))
        },
    );

    ev.register_eager(
        "int_abs",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int"], "int"),
        int_abs_eager,
    );

    ev.register_special(
        "int_and",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int", "int"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Int(
                require_int(&args[0], "int_and")? & require_int(&args[1], "int_and")?,
            ))
        },
    );

    ev.register_special(
        "int_xor",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int", "int"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Int(
                require_int(&args[0], "int_xor")? ^ require_int(&args[1], "int_xor")?,
            ))
        },
    );

    ev.register_special(
        "int_shr",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int", "int"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let l = require_int(&args[0], "int_shr")?;
            let r = require_int(&args[1], "int_shr")?;
            if !(0..64).contains(&r) {
                return Err(eval_err("int_shr: shift amount must be in 0..63"));
            }
            Ok(RuntimeValue::Int(l >> r))
        },
    );

    ev.register_special(
        "int_or",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int", "int"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Int(
                require_int(&args[0], "int_or")? | require_int(&args[1], "int_or")?,
            ))
        },
    );

    ev.register_special(
        "int_not",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Int(!require_int(&args[0], "int_not")?))
        },
    );

    ev.register_special(
        "int_shl",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int", "int"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let l = require_int(&args[0], "int_shl")?;
            let r = require_int(&args[1], "int_shl")?;
            if !(0..64).contains(&r) {
                return Err(eval_err("int_shl: shift amount must be in 0..63"));
            }
            Ok(RuntimeValue::Int(l << r))
        },
    );

    ev.register_special(
        "int_to_float",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int"], "float"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Float(
                require_int(&args[0], "int_to_float")? as f64
            ))
        },
    );

    ev.register_special(
        "float_to_int",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["float"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Int(float_to_int(require_float(
                &args[0],
                "float_to_int",
            )?)?))
        },
    );

    ev.register_eager(
        "eq",
        0,
        None,
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["*any"], "bool"),
        eq_eager,
    );

    ev.register_eager(
        "ne",
        0,
        None,
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["*any"], "bool"),
        ne_eager,
    );

    // Structural twin of `eq`: lists/tuples element-wise, maps by key set,
    // scalars and identities exactly like `eq`. Cycle-safe (see
    // value_compare::deep_equal) — the stdlib `deep_eq` facade wraps this.
    ev.register_eager(
        "value_eq",
        0,
        None,
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["*any"], "bool"),
        value_eq_eager,
    );

    // Total structural order over ANY two values → -1/0/+1 (the order twin of
    // value_eq: returns 0 exactly when value_eq is true). See
    // value_compare::deep_compare. Keystone for sorted collections, structural
    // map keys, and cache fingerprints.
    ev.register_eager(
        "value_compare",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["any", "any"], "int"),
        value_compare_eager,
    );

    // Structural hash to an i64: value_eq-equal values hash equal. See
    // value_compare::deep_hash.
    ev.register_eager(
        "value_hash",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["any"], "int"),
        value_hash_eager,
    );

    ev.register_eager(
        "lt",
        0,
        None,
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["*any"], "bool"),
        lt_eager,
    );

    ev.register_eager(
        "gt",
        0,
        None,
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["*any"], "bool"),
        gt_eager,
    );

    ev.register_eager(
        "le",
        0,
        None,
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["*any"], "bool"),
        le_eager,
    );

    ev.register_eager(
        "ge",
        0,
        None,
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["*any"], "bool"),
        ge_eager,
    );

    ev.register_eager(
        "not",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["any"], "bool"),
        not_eager,
    );

    // ── Float arithmetic ─────────────────────────────────────────────────────
    macro_rules! float_binop {
        ($name:expr, $op:expr) => {
            ev.register_special(
                $name.to_string(),
                2,
                Some(2),
                crate::values::BuiltinMetadata::eager_runtime()
                    .with_signature(&["float", "float"], "float"),
                |ev, call, env| {
                    let args = eval_args(ev, call, env)?;
                    let l = require_float(&args[0], $name)?;
                    let r = require_float(&args[1], $name)?;
                    Ok(RuntimeValue::Float($op(l, r)))
                },
            );
        };
    }
    float_binop!("float_add", |l: f64, r: f64| l + r);
    float_binop!("float_sub", |l: f64, r: f64| l - r);
    float_binop!("float_mul", |l: f64, r: f64| l * r);

    ev.register_special(
        "float_div",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let l = require_float(&args[0], "float_div")?;
            let r = require_float(&args[1], "float_div")?;
            if r == 0.0 {
                return Err(eval_err("float_div: division by zero"));
            }
            Ok(RuntimeValue::Float(l / r))
        },
    );

    // ── Float math ───────────────────────────────────────────────────────────
    macro_rules! float_unary {
        ($name:expr, $method:ident) => {
            ev.register_special(
                $name.to_string(),
                1,
                Some(1),
                crate::values::BuiltinMetadata::eager_runtime().with_signature(&["float"], "float"),
                |ev, call, env| {
                    let args = eval_args(ev, call, env)?;
                    Ok(RuntimeValue::Float(
                        require_float(&args[0], $name)?.$method(),
                    ))
                },
            );
        };
    }
    // Bit-level views of floats, for exact codegen emission (LLVM 0xH<hex>
    // double / f32 constants instead of lossy decimal round-trips). Registered
    // eager_runtime like the rest of the float family: pure, so the dual-phase
    // law makes them foldable at compile time — strictly more capable than a
    // CTFE-only registration for the same passes.
    ev.register_special(
        "float_to_bits",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["float"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let value = require_float(&args[0], "float_to_bits")?;
            Ok(RuntimeValue::Int(value.to_bits() as i64))
        },
    );
    ev.register_special(
        "float_to_bits_f32",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["float"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let value = require_float(&args[0], "float_to_bits_f32")?;
            Ok(RuntimeValue::Int(i64::from((value as f32).to_bits())))
        },
    );
    // Inverse bit-casts of `float_to_bits`/`_f32`: reinterpret an integer bit
    // pattern as a float, exactly. Lets the integer-only eval byte runtime
    // round-trip floats (e.g. the storage codegen) so eval = native parity holds.
    ev.register_special(
        "bits_to_float",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int"], "float"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bits = require_int(&args[0], "bits_to_float")?;
            Ok(RuntimeValue::Float(f64::from_bits(bits as u64)))
        },
    );
    ev.register_special(
        "bits_to_float_f32",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int"], "float"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bits = require_int(&args[0], "bits_to_float_f32")?;
            Ok(RuntimeValue::Float(f32::from_bits(bits as u32) as f64))
        },
    );

    float_unary!("sqrt", sqrt);
    float_unary!("sin", sin);
    float_unary!("cos", cos);
    float_unary!("tan", tan);
    float_unary!("asin", asin);
    float_unary!("acos", acos);
    float_unary!("atan", atan);
    float_unary!("log", ln);
    float_unary!("log2", log2);
    float_unary!("log10", log10);
    float_unary!("exp", exp);
    float_unary!("floor", floor);
    float_unary!("ceil", ceil);
    float_unary!("round", round);
    float_unary!("float_abs", abs);

    ev.register_special(
        "pow",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["float", "float"], "float"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let base = require_float(&args[0], "pow")?;
            let exp = require_float(&args[1], "pow")?;
            Ok(RuntimeValue::Float(base.powf(exp)))
        },
    );

    ev.register_special(
        "atan2",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["float", "float"], "float"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let y = require_float(&args[0], "atan2")?;
            let x = require_float(&args[1], "atan2")?;
            Ok(RuntimeValue::Float(y.atan2(x)))
        },
    );

    float_binop!("float_min", |l: f64, r: f64| l.min(r));
    float_binop!("float_max", |l: f64, r: f64| l.max(r));

    ev.register_special(
        "float_nan?",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["float"], "bool"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Bool(
                require_float(&args[0], "float_nan?")?.is_nan(),
            ))
        },
    );

    ev.register_special(
        "float_inf?",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["float"], "bool"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Bool(
                require_float(&args[0], "float_inf?")?.is_infinite(),
            ))
        },
    );
}
