use serde_json::{Map, Value};

use super::spec_compiler::SpecCompileError;

// ── Type introspection ─────────────────────────────────────────────────────

pub(super) fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ── Boolean access ─────────────────────────────────────────────────────────

pub(super) fn expect_bool(v: &Value, ctx: &str) -> Result<bool, SpecCompileError> {
    if let Some(b) = v.as_bool() {
        return Ok(b);
    }
    Err(SpecCompileError::TypeError {
        expected: "bool",
        actual: type_name(v).to_string(),
        ctx: ctx.to_string(),
    })
}

// ── String quoting ─────────────────────────────────────────────────────────

pub(super) fn quote_rule_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ── Array / string collection helpers ─────────────────────────────────────

pub(super) fn string_values(values: &[Value], ctx: &str) -> Result<Vec<String>, SpecCompileError> {
    values
        .iter()
        .map(|value| expect_str(value, ctx).map(str::to_string))
        .collect()
}

pub(super) fn string_array(values: &[String]) -> Value {
    Value::Array(
        values
            .iter()
            .map(|value| Value::String(value.clone()))
            .collect(),
    )
}

// ── Positional accessors ───────────────────────────────────────────────────

pub(super) fn expect_arr<'v>(v: &'v Value, ctx: &str) -> Result<&'v Vec<Value>, SpecCompileError> {
    v.as_array().ok_or_else(|| SpecCompileError::TypeError {
        expected: "array",
        actual: type_name(v).to_string(),
        ctx: ctx.to_string(),
    })
}

pub(super) fn expect_str<'v>(v: &'v Value, ctx: &str) -> Result<&'v str, SpecCompileError> {
    v.as_str().ok_or_else(|| SpecCompileError::TypeError {
        expected: "string",
        actual: type_name(v).to_string(),
        ctx: ctx.to_string(),
    })
}

pub(super) fn expect_str_at<'v>(
    arr: &'v [Value],
    idx: usize,
    ctx: &str,
) -> Result<&'v str, SpecCompileError> {
    let v = arr
        .get(idx)
        .ok_or_else(|| SpecCompileError::MissingField(ctx.to_string()))?;
    expect_str(v, ctx)
}

pub(super) fn expect_non_empty_str<'v>(
    value: &'v Value,
    ctx: &str,
) -> Result<&'v str, SpecCompileError> {
    let value = expect_str(value, ctx)?;
    require_non_empty_str(value, ctx)
}

pub(super) fn expect_non_empty_str_at<'v>(
    arr: &'v [Value],
    idx: usize,
    ctx: &str,
) -> Result<&'v str, SpecCompileError> {
    let value = expect_str_at(arr, idx, ctx)?;
    require_non_empty_str(value, ctx)
}

pub(super) fn expect_arr_at<'v>(
    arr: &'v [Value],
    idx: usize,
    ctx: &str,
) -> Result<&'v Vec<Value>, SpecCompileError> {
    let v = arr
        .get(idx)
        .ok_or_else(|| SpecCompileError::MissingField(ctx.to_string()))?;
    expect_arr(v, ctx)
}

pub(super) fn expect_val_at<'v>(
    arr: &'v [Value],
    idx: usize,
    ctx: &str,
) -> Result<&'v Value, SpecCompileError> {
    arr.get(idx)
        .ok_or_else(|| SpecCompileError::MissingField(ctx.to_string()))
}

// ── Optional field accessors ───────────────────────────────────────────────

pub(super) fn optional_bool_at(
    arr: &[Value],
    idx: usize,
    ctx: &str,
) -> Result<Option<bool>, SpecCompileError> {
    let Some(v) = arr.get(idx) else {
        return Ok(None);
    };
    v.as_bool()
        .map(Some)
        .ok_or_else(|| SpecCompileError::TypeError {
            expected: "bool",
            actual: type_name(v).to_string(),
            ctx: ctx.to_string(),
        })
}

pub(super) fn optional_str_at<'v>(
    arr: &'v [Value],
    idx: usize,
    ctx: &str,
) -> Result<Option<&'v str>, SpecCompileError> {
    let Some(v) = arr.get(idx) else {
        return Ok(None);
    };
    v.as_str()
        .map(Some)
        .ok_or_else(|| SpecCompileError::TypeError {
            expected: "string",
            actual: type_name(v).to_string(),
            ctx: ctx.to_string(),
        })
}

pub(super) fn optional_bool_field(
    obj: &Map<String, Value>,
    field: &str,
    ctx: &str,
) -> Result<Option<bool>, SpecCompileError> {
    let Some(v) = obj.get(field) else {
        return Ok(None);
    };
    v.as_bool()
        .map(Some)
        .ok_or_else(|| SpecCompileError::TypeError {
            expected: "bool",
            actual: type_name(v).to_string(),
            ctx: ctx.to_string(),
        })
}

pub(super) fn optional_str_field<'v>(
    obj: &'v Map<String, Value>,
    field: &str,
    ctx: &str,
) -> Result<Option<&'v str>, SpecCompileError> {
    let Some(v) = obj.get(field) else {
        return Ok(None);
    };
    v.as_str()
        .map(Some)
        .ok_or_else(|| SpecCompileError::TypeError {
            expected: "string",
            actual: type_name(v).to_string(),
            ctx: ctx.to_string(),
        })
}

pub(super) fn optional_non_empty_str_field<'v>(
    obj: &'v Map<String, Value>,
    field: &str,
    ctx: &str,
) -> Result<Option<&'v str>, SpecCompileError> {
    optional_str_field(obj, field, ctx)?
        .map(|value| require_non_empty_str(value, ctx))
        .transpose()
}

pub(super) fn require_non_empty_str<'v>(
    value: &'v str,
    ctx: &str,
) -> Result<&'v str, SpecCompileError> {
    if value.is_empty() {
        return Err(SpecCompileError::InvalidFormat(format!(
            "{ctx} must be a non-empty string"
        )));
    }
    Ok(value)
}
