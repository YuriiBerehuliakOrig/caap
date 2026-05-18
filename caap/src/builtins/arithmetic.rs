/// Arithmetic and comparison builtins — port of `caap/builtins/lang/arithmetic.py`.
use crate::eval::{eval_args, Evaluator};
use crate::values::{eval_err, BuiltinInfo, RuntimeValue};

fn require_int(v: &RuntimeValue, context: &str) -> Result<i64, crate::values::EvalSignal> {
    match v {
        RuntimeValue::Int(i) => Ok(*i),
        other => Err(eval_err(format!("{context}: expected int, got {other}"))),
    }
}

fn require_float(v: &RuntimeValue, context: &str) -> Result<f64, crate::values::EvalSignal> {
    match v {
        RuntimeValue::Float(f) => Ok(*f),
        other => Err(eval_err(format!("{context}: expected float, got {other}"))),
    }
}

fn int_add_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    Ok(RuntimeValue::Int(
        require_int(&args[0], "int-add")? + require_int(&args[1], "int-add")?,
    ))
}

fn int_sub_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    Ok(RuntimeValue::Int(
        require_int(&args[0], "int-sub")? - require_int(&args[1], "int-sub")?,
    ))
}

fn int_mul_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    Ok(RuntimeValue::Int(
        require_int(&args[0], "int-mul")? * require_int(&args[1], "int-mul")?,
    ))
}

fn int_abs_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    Ok(RuntimeValue::Int(require_int(&args[0], "int-abs")?.abs()))
}

fn eq_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    if args.len() < 2 {
        return Ok(RuntimeValue::Bool(true));
    }
    let first = &args[0];
    Ok(RuntimeValue::Bool(args[1..].iter().all(|v| v == first)))
}

fn lt_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    if args.len() < 2 {
        return Ok(RuntimeValue::Bool(true));
    }
    Ok(RuntimeValue::Bool(
        args.windows(2).all(|w| cmp_lt(&w[0], &w[1])),
    ))
}

fn gt_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    if args.len() < 2 {
        return Ok(RuntimeValue::Bool(true));
    }
    Ok(RuntimeValue::Bool(
        args.windows(2).all(|w| cmp_lt(&w[1], &w[0])),
    ))
}

fn not_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    Ok(RuntimeValue::Bool(!crate::values::is_truthy(&args[0])))
}

pub fn register(ev: &mut Evaluator) {
    ev.register_builtin(BuiltinInfo {
        name: "int-add".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: Some(Box::new(int_add_eager)),
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            int_add_eager(args)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "int-sub".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: Some(Box::new(int_sub_eager)),
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            int_sub_eager(args)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "int-mul".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: Some(Box::new(int_mul_eager)),
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            int_mul_eager(args)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "int-div".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let l = require_int(&args[0], "int-div")?;
            let r = require_int(&args[1], "int-div")?;
            if r == 0 {
                return Err(eval_err("int-div: division by zero"));
            }
            Ok(RuntimeValue::Int(l.div_euclid(r)))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "int-rem".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let l = require_int(&args[0], "int-rem")?;
            let r = require_int(&args[1], "int-rem")?;
            if r == 0 {
                return Err(eval_err("int-rem: division by zero"));
            }
            Ok(RuntimeValue::Int(l % r))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "int-mod".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let l = require_int(&args[0], "int-mod")?;
            let r = require_int(&args[1], "int-mod")?;
            if r <= 0 {
                return Err(eval_err("int-mod: modulus must be positive"));
            }
            Ok(RuntimeValue::Int(l.rem_euclid(r)))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "int-abs".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: Some(Box::new(int_abs_eager)),
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            int_abs_eager(args)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "int-min".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let l = require_int(&args[0], "int-min")?;
            let r = require_int(&args[1], "int-min")?;
            Ok(RuntimeValue::Int(l.min(r)))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "int-max".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let l = require_int(&args[0], "int-max")?;
            let r = require_int(&args[1], "int-max")?;
            Ok(RuntimeValue::Int(l.max(r)))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "int-clamp".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let v = require_int(&args[0], "int-clamp")?;
            let lo = require_int(&args[1], "int-clamp")?;
            let hi = require_int(&args[2], "int-clamp")?;
            if lo > hi {
                return Err(eval_err("int-clamp: lower bound exceeds upper bound"));
            }
            Ok(RuntimeValue::Int(v.clamp(lo, hi)))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "int-and".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Int(
                require_int(&args[0], "int-and")? & require_int(&args[1], "int-and")?,
            ))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "int-xor".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Int(
                require_int(&args[0], "int-xor")? ^ require_int(&args[1], "int-xor")?,
            ))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "int-shr".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let l = require_int(&args[0], "int-shr")?;
            let r = require_int(&args[1], "int-shr")?;
            if r < 0 {
                return Err(eval_err("int-shr: shift amount must be non-negative"));
            }
            Ok(RuntimeValue::Int(l >> r))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "int-to-float".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Float(
                require_int(&args[0], "int-to-float")? as f64
            ))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "float-to-int".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Int(
                require_float(&args[0], "float-to-int")? as i64
            ))
        }),
    });

    // Comparison builtins (variadic, return bool)
    ev.register_builtin(BuiltinInfo {
        name: "eq".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 0,
        max_arity: None,
        eager_handler: Some(Box::new(eq_eager)),
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            eq_eager(args)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "lt".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 0,
        max_arity: None,
        eager_handler: Some(Box::new(lt_eager)),
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            lt_eager(args)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "gt".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 0,
        max_arity: None,
        eager_handler: Some(Box::new(gt_eager)),
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            gt_eager(args)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "not".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: Some(Box::new(not_eager)),
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            not_eager(args)
        }),
    });
}

fn cmp_lt(a: &RuntimeValue, b: &RuntimeValue) -> bool {
    match (a, b) {
        (RuntimeValue::Int(x), RuntimeValue::Int(y)) => x < y,
        (RuntimeValue::Float(x), RuntimeValue::Float(y)) => x < y,
        (RuntimeValue::Str(x), RuntimeValue::Str(y)) => x.as_ref() < y.as_ref(),
        _ => false,
    }
}
