/// String builtins — port of the string-* functions in `caap/builtins/lang/data.py`.
use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;

use crate::eval::{eval_args, Evaluator};
use crate::values::{eval_err, require_int_strict, require_str, BuiltinInfo, MapKey, RuntimeValue};

fn to_str(v: &RuntimeValue, ctx: &str) -> Result<String, crate::values::EvalSignal> {
    require_str(v, ctx).map(|s| s.to_string())
}

pub fn register(ev: &mut Evaluator) {
    // ── string-concat-many ────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-concat-many".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 0,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let mut result = String::new();
            for v in &args {
                result.push_str(&to_str(v, "string-concat-many")?);
            }
            Ok(RuntimeValue::Str(result.into()))
        }),
    });

    // ── stable-hash ──────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "stable-hash".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let token = stable_hash_token(&args[0])?;
            let mut digest = [0u8; 16];
            let mut hasher = Blake2bVar::new(16)
                .map_err(|error| eval_err(format!("stable-hash failed: {error}")))?;
            hasher.update(token.as_bytes());
            hasher
                .finalize_variable(&mut digest)
                .map_err(|error| eval_err(format!("stable-hash failed: {error}")))?;
            Ok(RuntimeValue::Str(hex_lower(&digest).into()))
        }),
    });

    // ── string-slice ─────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-slice".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string-slice")?;
            let chars: Vec<char> = s.chars().collect();
            let start =
                normalize_slice_index(require_int_strict(&args[1], "string-slice")?, chars.len());
            let end = optional_slice_end(args.get(2), chars.len(), "string-slice")?;
            let sliced: String = if end <= start {
                String::new()
            } else {
                chars[start..end].iter().collect()
            };
            Ok(RuntimeValue::Str(sliced.into()))
        }),
    });

    // ── string-split ──────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-split".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string-split")?;
            let sep = to_str(&args[1], "string-split")?;
            if sep.is_empty() {
                return Err(eval_err("string-split expects a non-empty separator"));
            }
            let parts: Vec<RuntimeValue> = s
                .split(&sep as &str)
                .map(|p| RuntimeValue::Str(p.into()))
                .collect();
            Ok(RuntimeValue::List(std::rc::Rc::new(
                std::cell::RefCell::new(parts),
            )))
        }),
    });

    // ── string-find ───────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-find".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let haystack = to_str(&args[0], "string-find")?;
            let needle = to_str(&args[1], "string-find")?;
            if needle.is_empty() {
                return Err(eval_err("string-find expects a non-empty needle"));
            }
            let start = if args.len() == 3 {
                require_int_strict(&args[2], "string-find")? as usize
            } else {
                0
            };
            let search = if start < haystack.len() {
                &haystack[start..]
            } else {
                ""
            };
            match search.find(&needle as &str) {
                Some(idx) => Ok(RuntimeValue::Int((start + idx) as i64)),
                None => Ok(RuntimeValue::Null),
            }
        }),
    });

    // ── string-index-of ───────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-index-of".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let haystack = to_str(&args[0], "string-index-of")?;
            let needle = to_str(&args[1], "string-index-of")?;
            if needle.is_empty() {
                return Err(eval_err("string-index-of expects a non-empty needle"));
            }
            let start = if args.len() == 3 {
                require_int_strict(&args[2], "string-index-of")? as usize
            } else {
                0
            };
            let search = if start < haystack.len() {
                &haystack[start..]
            } else {
                ""
            };
            Ok(RuntimeValue::Int(match search.find(&needle as &str) {
                Some(idx) => (start + idx) as i64,
                None => -1,
            }))
        }),
    });

    // ── string-repeat ─────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-repeat".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string-repeat")?;
            let n = require_int_strict(&args[1], "string-repeat")?;
            if n < 0 {
                return Err(eval_err("string-repeat expects a non-negative count"));
            }
            Ok(RuntimeValue::Str(s.repeat(n as usize).into()))
        }),
    });

    // ── string-format ────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-format".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let template = to_str(&args[0], "string-format")?;
            format_template(&template, &args[1..]).map(|text| RuntimeValue::Str(text.into()))
        }),
    });

    // ── string-trim ───────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-trim".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Str(
                to_str(&args[0], "string-trim")?.trim().into(),
            ))
        }),
    });

    // ── string-starts-with ────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-starts-with".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string-starts-with")?;
            let prefix = to_str(&args[1], "string-starts-with")?;
            Ok(RuntimeValue::Bool(s.starts_with(&prefix as &str)))
        }),
    });

    // ── string-ends-with ──────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-ends-with".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string-ends-with")?;
            let suffix = to_str(&args[1], "string-ends-with")?;
            Ok(RuntimeValue::Bool(s.ends_with(&suffix as &str)))
        }),
    });

    // ── string-contains ───────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-contains".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string-contains")?;
            let sub = to_str(&args[1], "string-contains")?;
            Ok(RuntimeValue::Bool(s.contains(&sub as &str)))
        }),
    });

    // ── string-upcase ─────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-upcase".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Str(
                to_str(&args[0], "string-upcase")?.to_uppercase().into(),
            ))
        }),
    });

    // ── string-downcase ───────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-downcase".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Str(
                to_str(&args[0], "string-downcase")?.to_lowercase().into(),
            ))
        }),
    });

    // ── string-replace ────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-replace".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string-replace")?;
            let old = to_str(&args[1], "string-replace")?;
            let new = to_str(&args[2], "string-replace")?;
            Ok(RuntimeValue::Str(
                s.replace(&old as &str, &new as &str).into(),
            ))
        }),
    });

    // ── string-lines ─────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-lines".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string-lines")?;
            let lines: Vec<RuntimeValue> = s.lines().map(|l| RuntimeValue::Str(l.into())).collect();
            Ok(RuntimeValue::List(std::rc::Rc::new(
                std::cell::RefCell::new(lines),
            )))
        }),
    });

    // ── string-to-int ─────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-to-int".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string-to-int")?;
            s.trim()
                .parse::<i64>()
                .map(RuntimeValue::Int)
                .map_err(|_| eval_err("string-to-int expects a base-10 integer string"))
        }),
    });

    // ── int-to-string ─────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "int-to-string".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let i = match &args[0] {
                RuntimeValue::Int(i) => *i,
                other => {
                    return Err(eval_err(format!(
                        "int-to-string: expected int, got {other}"
                    )))
                }
            };
            Ok(RuntimeValue::Str(i.to_string().into()))
        }),
    });

    // ── string-byte-length ────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-byte-length".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string-byte-length")?;
            Ok(RuntimeValue::Int(s.len() as i64))
        }),
    });

    // ── string-byte-at ────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-byte-at".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string-byte-at")?;
            let index = require_int_strict(&args[1], "string-byte-at")?;
            if index < 0 {
                return Err(eval_err("string-byte-at index is out of range"));
            }
            let Some(byte) = s.as_bytes().get(index as usize) else {
                return Err(eval_err("string-byte-at index is out of range"));
            };
            Ok(RuntimeValue::Int(*byte as i64))
        }),
    });

    // ── string-last-segment ───────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-last-segment".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string-last-segment")?;
            let sep = to_str(&args[1], "string-last-segment")?;
            if sep.is_empty() {
                return Err(eval_err(
                    "string-last-segment expects a non-empty separator",
                ));
            }
            let parts: Vec<&str> = s.split(&sep as &str).filter(|p| !p.is_empty()).collect();
            let result = if parts.is_empty() {
                &s as &str
            } else {
                parts[parts.len() - 1]
            };
            Ok(RuntimeValue::Str(result.into()))
        }),
    });

    // ── string-pad-left ───────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-pad-left".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string-pad-left")?;
            let width = require_int_strict(&args[1], "string-pad-left")?;
            if width < 0 {
                return Err(eval_err("string-pad-left expects a non-negative width"));
            }
            let fill = if args.len() == 3 {
                to_str(&args[2], "string-pad-left")?
            } else {
                " ".to_string()
            };
            if fill.len() != 1 {
                return Err(eval_err(
                    "string-pad-left expects a one-character fill string",
                ));
            }
            let chars: Vec<char> = s.chars().collect();
            let pad = width as usize;
            if chars.len() >= pad {
                return Ok(RuntimeValue::Str(s.into()));
            }
            let padding: String = fill
                .chars()
                .next()
                .unwrap()
                .to_string()
                .repeat(pad - chars.len());
            Ok(RuntimeValue::Str(format!("{padding}{s}").into()))
        }),
    });

    // ── string-pad-right ──────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "string-pad-right".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string-pad-right")?;
            let width = require_int_strict(&args[1], "string-pad-right")?;
            if width < 0 {
                return Err(eval_err("string-pad-right expects a non-negative width"));
            }
            let fill = if args.len() == 3 {
                to_str(&args[2], "string-pad-right")?
            } else {
                " ".to_string()
            };
            if fill.len() != 1 {
                return Err(eval_err(
                    "string-pad-right expects a one-character fill string",
                ));
            }
            let chars: Vec<char> = s.chars().collect();
            let pad = width as usize;
            if chars.len() >= pad {
                return Ok(RuntimeValue::Str(s.into()));
            }
            let padding: String = fill
                .chars()
                .next()
                .unwrap()
                .to_string()
                .repeat(pad - chars.len());
            Ok(RuntimeValue::Str(format!("{s}{padding}").into()))
        }),
    });
}

fn format_template(
    template: &str,
    args: &[RuntimeValue],
) -> Result<String, crate::values::EvalSignal> {
    let mut output = String::new();
    let mut next_auto_index = 0usize;
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    output.push('{');
                    continue;
                }
                let mut field = String::new();
                let mut closed = false;
                for field_ch in chars.by_ref() {
                    if field_ch == '}' {
                        closed = true;
                        break;
                    }
                    field.push(field_ch);
                }
                if !closed {
                    return Err(eval_err(
                        "string-format failed: expected '}' before end of string",
                    ));
                }
                let index = if field.is_empty() {
                    let index = next_auto_index;
                    next_auto_index += 1;
                    index
                } else if field.chars().all(|item| item.is_ascii_digit()) {
                    field
                        .parse::<usize>()
                        .map_err(|_| eval_err("string-format failed: invalid replacement field"))?
                } else {
                    return Err(eval_err(format!(
                        "string-format failed: unsupported replacement field {{{field}}}"
                    )));
                };
                let Some(value) = args.get(index) else {
                    return Err(eval_err(format!(
                        "string-format failed: replacement index {index} out of range"
                    )));
                };
                output.push_str(&value.to_string());
            }
            '}' => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                    output.push('}');
                } else {
                    return Err(eval_err(
                        "string-format failed: single '}' encountered in format string",
                    ));
                }
            }
            _ => output.push(ch),
        }
    }

    Ok(output)
}

fn stable_hash_token(value: &RuntimeValue) -> Result<String, crate::values::EvalSignal> {
    match value {
        RuntimeValue::Null => Ok("n:null".to_string()),
        RuntimeValue::Bool(value) => Ok(format!("b:{}", if *value { "1" } else { "0" })),
        RuntimeValue::Int(value) => Ok(format!("i:{value}")),
        RuntimeValue::Float(value) => Ok(format!("f:{}", stable_float_token(*value))),
        RuntimeValue::Str(value) => Ok(format!("s:{}:{value}", value.chars().count())),
        RuntimeValue::HostObject(object) if object.type_name() == "node" => {
            if let Some(node) = object
                .as_any()
                .downcast_ref::<crate::bridges::NodeBridgeValue>()
            {
                return Ok(format!("h:{}", node.node_id()));
            }
            Err(eval_err("stable-hash cannot hash opaque node host object"))
        }
        RuntimeValue::Tuple(items) => stable_sequence_token(items.iter()),
        RuntimeValue::List(items) => stable_sequence_token(items.borrow().iter()),
        RuntimeValue::Map(items) => {
            let mut entries = Vec::new();
            for (key, item) in items.borrow().iter() {
                entries.push((stable_map_key_token(key), stable_hash_token(item)?));
            }
            entries.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
            Ok(format!(
                "m:[{}]",
                entries
                    .into_iter()
                    .map(|(key, item)| format!("{key}->{item}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ))
        }
        other => Err(eval_err(format!(
            "stable-hash cannot hash value of type {}",
            stable_hash_type_name(other)
        ))),
    }
}

fn stable_sequence_token<'a>(
    items: impl Iterator<Item = &'a RuntimeValue>,
) -> Result<String, crate::values::EvalSignal> {
    Ok(format!(
        "l:[{}]",
        items
            .map(stable_hash_token)
            .collect::<Result<Vec<_>, _>>()?
            .join(",")
    ))
}

fn stable_map_key_token(key: &MapKey) -> String {
    match key {
        MapKey::Null => "n:null".to_string(),
        MapKey::Bool(value) => format!("b:{}", if *value { "1" } else { "0" }),
        MapKey::Int(value) => format!("i:{value}"),
        MapKey::Str(value) => format!("s:{}:{value}", value.chars().count()),
    }
}

fn stable_float_token(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-inf".to_string()
        } else {
            "inf".to_string()
        };
    }
    let bits = value.to_bits();
    let sign = if bits >> 63 == 1 { "-" } else { "" };
    let exponent_bits = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & 0x000f_ffff_ffff_ffff;
    if exponent_bits == 0 && fraction == 0 {
        return format!("{sign}0x0.0p+0");
    }
    if exponent_bits == 0 {
        let leading = 63 - fraction.leading_zeros() as i32;
        let exponent = leading - 1074;
        let mantissa = (fraction ^ (1u64 << leading)) << (52 - leading);
        return format!("{sign}0x1.{mantissa:013x}p{exponent:+}");
    }
    let exponent = exponent_bits - 1023;
    format!("{sign}0x1.{fraction:013x}p{exponent:+}")
}

fn stable_hash_type_name(value: &RuntimeValue) -> &'static str {
    match value {
        RuntimeValue::Closure(_) => "closure",
        RuntimeValue::Builtin(_) => "builtin",
        RuntimeValue::HostFunction(_) => "host-function",
        RuntimeValue::HostObject(_) => "host-object",
        RuntimeValue::UninitializedTopLevel => "uninitialized-top-level",
        RuntimeValue::Null
        | RuntimeValue::Bool(_)
        | RuntimeValue::Int(_)
        | RuntimeValue::Float(_)
        | RuntimeValue::Str(_)
        | RuntimeValue::Tuple(_)
        | RuntimeValue::List(_)
        | RuntimeValue::Map(_) => "value",
    }
}

fn optional_slice_end(
    value: Option<&RuntimeValue>,
    len: usize,
    ctx: &str,
) -> Result<usize, crate::values::EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(len),
        Some(value) => Ok(normalize_slice_index(require_int_strict(value, ctx)?, len)),
    }
}

fn normalize_slice_index(index: i64, len: usize) -> usize {
    if index < 0 {
        len.saturating_sub(index.unsigned_abs() as usize)
    } else {
        (index as usize).min(len)
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}
