/// String builtins — port of the string-* functions in `caap/builtins/lang/data.py`.
use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;

use crate::eval::{eval_args, Evaluator};
use crate::values::{eval_err, require_int_strict, require_str, MapKey, RuntimeValue};

fn to_str(v: &RuntimeValue, ctx: &str) -> Result<String, crate::values::EvalSignal> {
    require_str(v, ctx).map(|s| s.to_string())
}

fn require_char_start_index(
    value: &RuntimeValue,
    context: &str,
) -> Result<usize, crate::values::EvalSignal> {
    let index = require_int_strict(value, context)?;
    if index < 0 {
        return Err(eval_err(format!(
            "{context} start index must be non-negative"
        )));
    }
    usize::try_from(index).map_err(|_| eval_err(format!("{context} start index is too large")))
}

fn require_non_negative_usize(
    value: &RuntimeValue,
    context: &str,
    name: &str,
) -> Result<usize, crate::values::EvalSignal> {
    let value = require_int_strict(value, context)?;
    if value < 0 {
        return Err(eval_err(format!("{context} expects a non-negative {name}")));
    }
    usize::try_from(value).map_err(|_| eval_err(format!("{context} {name} is too large")))
}

use super::args::{ensure_string_char_limit, normalize_slice_index, optional_slice_end};

fn checked_string(
    context: &str,
    text: String,
    ev: &Evaluator,
) -> Result<RuntimeValue, crate::values::EvalSignal> {
    ensure_string_char_limit(context, text.chars().count(), ev)?;
    Ok(RuntimeValue::Str(text.into()))
}

fn ensure_list_len(
    context: &str,
    len: usize,
    ev: &Evaluator,
) -> Result<(), crate::values::EvalSignal> {
    let limit = ev.runtime_collection_limit();
    if len > limit {
        return Err(eval_err(format!(
            "{context}: list size limit {limit} exceeded"
        )));
    }
    ev.charge_allocation(len)
}

fn byte_offset_for_char_index(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(byte_index, _)| byte_index)
        .unwrap_or(text.len())
}

fn find_from_char_index(haystack: &str, needle: &str, start: usize) -> Option<usize> {
    let byte_start = byte_offset_for_char_index(haystack, start);
    let search = &haystack[byte_start..];
    let found = search.find(needle)?;
    Some(start + search[..found].chars().count())
}

pub fn register(ev: &mut Evaluator) {
    // ── string-concat-many ────────────────────────────────────────────────────
    ev.register_special(
        "string_concat_many",
        0,
        None,
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["*string"], "string"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let mut result = String::new();
            for v in &args {
                result.push_str(&to_str(v, "string_concat_many")?);
            }
            checked_string("string_concat_many", result, ev)
        },
    );

    // ── stable-hash ──────────────────────────────────────────────────────────
    ev.register_special(
        "stable_hash",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["any"], "string"),
        |ev, call, env| {
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
        },
    );

    // ── string-slice ─────────────────────────────────────────────────────────
    ev.register_special(
        "string_slice",
        2,
        Some(3),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["string", "int", "int"], "string"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string_slice")?;
            let chars: Vec<char> = s.chars().collect();
            let start =
                normalize_slice_index(require_int_strict(&args[1], "string_slice")?, chars.len());
            let end = optional_slice_end(args.get(2), chars.len(), "string_slice")?;
            let sliced: String = if end <= start {
                String::new()
            } else {
                chars[start..end].iter().collect()
            };
            Ok(RuntimeValue::Str(sliced.into()))
        },
    );

    // ── string-chars ──────────────────────────────────────────────────────────
    // One-call O(n) character iteration: the per-index `string_slice` loop the
    // stdlib used before is O(n²) (each slice re-collects the chars).
    ev.register_special(
        "string_chars",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["string"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string_chars")?;
            let chars: Vec<RuntimeValue> = s
                .chars()
                .map(|c| RuntimeValue::Str(String::from(c).into()))
                .collect();
            ensure_list_len("string_chars", chars.len(), ev)?;
            Ok(RuntimeValue::List(std::rc::Rc::new(
                std::cell::RefCell::new(chars),
            )))
        },
    );

    // ── string-split ──────────────────────────────────────────────────────────
    ev.register_special(
        "string_split",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["string", "string"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string_split")?;
            let sep = to_str(&args[1], "string_split")?;
            if sep.is_empty() {
                return Err(eval_err("string-split expects a non-empty separator"));
            }
            let parts: Vec<RuntimeValue> = s
                .split(&sep as &str)
                .map(|p| RuntimeValue::Str(p.into()))
                .collect();
            ensure_list_len("string_split", parts.len(), ev)?;
            Ok(RuntimeValue::List(std::rc::Rc::new(
                std::cell::RefCell::new(parts),
            )))
        },
    );

    // ── string-find ───────────────────────────────────────────────────────────
    ev.register_special(
        "string_find",
        2,
        Some(3),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["string", "string", "int"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let haystack = to_str(&args[0], "string_find")?;
            let needle = to_str(&args[1], "string_find")?;
            if needle.is_empty() {
                return Err(eval_err("string-find expects a non-empty needle"));
            }
            let start = if args.len() == 3 {
                require_char_start_index(&args[2], "string_find")?
            } else {
                0
            };
            match find_from_char_index(&haystack, &needle, start) {
                Some(idx) => Ok(RuntimeValue::Int(idx as i64)),
                None => Ok(RuntimeValue::Null),
            }
        },
    );

    // ── string-repeat ─────────────────────────────────────────────────────────
    ev.register_special(
        "string_repeat",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["string", "int"], "string"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string_repeat")?;
            let n = require_non_negative_usize(&args[1], "string_repeat", "count")?;
            let char_count = s
                .chars()
                .count()
                .checked_mul(n)
                .ok_or_else(|| eval_err("string_repeat: string size overflow"))?;
            ensure_string_char_limit("string_repeat", char_count, ev)?;
            Ok(RuntimeValue::Str(s.repeat(n).into()))
        },
    );

    // ── string-format ────────────────────────────────────────────────────────

    // ── string-trim ───────────────────────────────────────────────────────────
    ev.register_special(
        "string_trim",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["string"], "string"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let text = to_str(&args[0], "string_trim")?.trim().to_string();
            checked_string("string_trim", text, ev)
        },
    );

    // ── string-starts-with ────────────────────────────────────────────────────
    ev.register_special(
        "string_starts_with",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["string", "string"], "bool"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string_starts_with")?;
            let prefix = to_str(&args[1], "string_starts_with")?;
            Ok(RuntimeValue::Bool(s.starts_with(&prefix as &str)))
        },
    );

    // ── string-ends-with ──────────────────────────────────────────────────────
    ev.register_special(
        "string_ends_with",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["string", "string"], "bool"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string_ends_with")?;
            let suffix = to_str(&args[1], "string_ends_with")?;
            Ok(RuntimeValue::Bool(s.ends_with(&suffix as &str)))
        },
    );

    // ── string-contains ───────────────────────────────────────────────────────
    ev.register_special(
        "string_contains",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["string", "string"], "bool"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string_contains")?;
            let sub = to_str(&args[1], "string_contains")?;
            Ok(RuntimeValue::Bool(s.contains(&sub as &str)))
        },
    );

    // ── string-upcase ─────────────────────────────────────────────────────────
    ev.register_special(
        "string_upcase",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["string"], "string"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let text = to_str(&args[0], "string_upcase")?.to_uppercase();
            checked_string("string_upcase", text, ev)
        },
    );

    // ── string-downcase ───────────────────────────────────────────────────────
    ev.register_special(
        "string_downcase",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["string"], "string"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let text = to_str(&args[0], "string_downcase")?.to_lowercase();
            checked_string("string_downcase", text, ev)
        },
    );

    // ── string-replace ────────────────────────────────────────────────────────
    ev.register_special(
        "string_replace",
        3,
        Some(3),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["string", "string", "string"], "string"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string_replace")?;
            let old = to_str(&args[1], "string_replace")?;
            let new = to_str(&args[2], "string_replace")?;
            let text = s.replace(&old as &str, &new as &str);
            checked_string("string_replace", text, ev)
        },
    );

    // ── string-lines ─────────────────────────────────────────────────────────
    ev.register_special(
        "string_lines",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["string"], "list"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string_lines")?;
            let lines: Vec<RuntimeValue> = s.lines().map(|l| RuntimeValue::Str(l.into())).collect();
            ensure_list_len("string_lines", lines.len(), ev)?;
            Ok(RuntimeValue::List(std::rc::Rc::new(
                std::cell::RefCell::new(lines),
            )))
        },
    );

    // ── string-to-int ─────────────────────────────────────────────────────────
    ev.register_special(
        "string_to_int",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["string"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string_to_int")?;
            s.trim()
                .parse::<i64>()
                .map(RuntimeValue::Int)
                .map_err(|_| eval_err("string-to-int expects a base-10 integer string"))
        },
    );

    // ── string-to-float ───────────────────────────────────────────────────────
    ev.register_special(
        "string_to_float",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["string"], "float"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string_to_float")?;
            s.trim()
                .parse::<f64>()
                .map(RuntimeValue::Float)
                .map_err(|_| eval_err("string-to-float expects a decimal float string"))
        },
    );

    // ── int-to-string ─────────────────────────────────────────────────────────
    ev.register_special(
        "int_to_string",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["int"], "string"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let i = match &args[0] {
                RuntimeValue::Int(i) => *i,
                other => {
                    return Err(eval_err(format!(
                        "int_to_string: expected int, got {other}"
                    )))
                }
            };
            Ok(RuntimeValue::Str(i.to_string().into()))
        },
    );

    // ── string-byte-length ────────────────────────────────────────────────────
    ev.register_special(
        "string_byte_length",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime().with_signature(&["string"], "int"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string_byte_length")?;
            Ok(RuntimeValue::Int(s.len() as i64))
        },
    );

    // ── string-byte-at ────────────────────────────────────────────────────────

    // ── string-last-segment ───────────────────────────────────────────────────
    ev.register_special(
        "string_last_segment",
        2,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime()
            .with_signature(&["string", "string"], "string"),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let s = to_str(&args[0], "string_last_segment")?;
            let sep = to_str(&args[1], "string_last_segment")?;
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
            checked_string("string_last_segment", result.to_string(), ev)
        },
    );
}

fn stable_hash_token(value: &RuntimeValue) -> Result<String, crate::values::EvalSignal> {
    match value {
        RuntimeValue::Null => Ok("n:null".to_string()),
        RuntimeValue::Bool(value) => Ok(format!("b:{}", if *value { "1" } else { "0" })),
        RuntimeValue::Int(value) => Ok(format!("i:{value}")),
        RuntimeValue::Float(value) => Ok(format!("f:{}", stable_float_token(*value))),
        RuntimeValue::Str(value) => Ok(format!("s:{}:{value}", value.chars().count())),
        RuntimeValue::Bytes(value) => {
            let mut token = format!("y:{}:", value.len());
            for byte in value.iter() {
                token.push_str(&format!("{byte:02x}"));
            }
            Ok(token)
        }
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
        RuntimeValue::Macro(_) => "macro",
        RuntimeValue::Builtin(_) => "builtin",
        RuntimeValue::HostFunction(_) => "host_function",
        RuntimeValue::HostObject(_) => "host_object",
        RuntimeValue::Ref(_) => "ref",
        RuntimeValue::UninitializedTopLevel => "uninitialized_top_level",
        RuntimeValue::Null
        | RuntimeValue::Bool(_)
        | RuntimeValue::Int(_)
        | RuntimeValue::Float(_)
        | RuntimeValue::Str(_)
        | RuntimeValue::Bytes(_)
        | RuntimeValue::Tuple(_)
        | RuntimeValue::List(_)
        | RuntimeValue::Map(_) => "value",
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}
