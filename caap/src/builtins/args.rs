//! Shared value constructors and argument-coercion helpers for the builtin
//! layer. One home for the small helpers that were previously copied across
//! builtin modules. Naming is deliberate and consistent:
//!
//! - `require_string` accepts any `Str` (including empty); `require_named_string`
//!   requires a non-empty `Str` (identifiers, names, kinds — the old reject-empty
//!   `require_string`/`require_named_string` copies collapse onto this).
//! - `optional_string_value` is the *encoder* (`Option<&str>` → `RuntimeValue`),
//!   distinct from the decoders that read an optional string out of a value.
//!
//! Note: `surface.rs` keeps its own interning `string`/`str_key` (a deliberate
//! hot-path choice), so it is not migrated onto the non-interning constructors
//! here.
use crate::values::{eval_err, require_int_strict, EvalSignal, MapKey, RuntimeValue};

// ── Value constructors ────────────────────────────────────────────────────────

/// A `RuntimeValue::Str` from any string-like value (non-interning).
pub(crate) fn string(value: impl AsRef<str>) -> RuntimeValue {
    RuntimeValue::Str(value.as_ref().into())
}

/// A `RuntimeValue::Tuple`.
pub(crate) fn tuple(items: Vec<RuntimeValue>) -> RuntimeValue {
    RuntimeValue::Tuple(items.into())
}

/// A string map key (non-interning).
pub(crate) fn str_key(key: &str) -> MapKey {
    MapKey::Str(key.into())
}

/// Encode an optional string: `Some(s)` → a string value, `None` → null.
pub(crate) fn optional_string_value(value: Option<&str>) -> RuntimeValue {
    value.map(string).unwrap_or(RuntimeValue::Null)
}

// ── Argument coercion ─────────────────────────────────────────────────────────

/// Require a `Str` (any, including empty); `message` is the error on mismatch.
pub(crate) fn require_string(value: &RuntimeValue, message: &str) -> Result<String, EvalSignal> {
    match value {
        RuntimeValue::Str(text) => Ok(text.to_string()),
        _ => Err(eval_err(message)),
    }
}

/// Require a non-empty `Str` (identifiers, names, kinds).
pub(crate) fn require_named_string(
    value: &RuntimeValue,
    message: &str,
) -> Result<String, EvalSignal> {
    let text = require_string(value, message)?;
    if text.is_empty() {
        return Err(eval_err(message));
    }
    Ok(text)
}

/// Require a `Bool`.
pub(crate) fn require_bool(value: &RuntimeValue, message: &str) -> Result<bool, EvalSignal> {
    match value {
        RuntimeValue::Bool(value) => Ok(*value),
        _ => Err(eval_err(message)),
    }
}

/// Require a non-negative integer as `usize`.
pub(crate) fn require_usize(value: &RuntimeValue, message: &str) -> Result<usize, EvalSignal> {
    match value {
        RuntimeValue::Int(value) if *value >= 0 => usize::try_from(*value)
            .map_err(|_| eval_err(format!("{message}: value exceeds usize range"))),
        RuntimeValue::Int(_) => Err(eval_err(format!("{message}: value must be non-negative"))),
        _ => Err(eval_err(message)),
    }
}

/// Require an `Int`.
pub(crate) fn require_int(value: &RuntimeValue, context: &str) -> Result<i64, EvalSignal> {
    match value {
        RuntimeValue::Int(value) => Ok(*value),
        other => Err(eval_err(format!("{context}: expected int, got {other}"))),
    }
}

// ── Slice helpers ─────────────────────────────────────────────────────────────

/// Resolve a possibly-negative slice index against `len`, clamped to `[0, len]`.
pub(crate) fn normalize_slice_index(index: i64, len: usize) -> usize {
    if index < 0 {
        let offset = usize::try_from(index.unsigned_abs()).unwrap_or(usize::MAX);
        len.saturating_sub(offset)
    } else {
        usize::try_from(index).unwrap_or(usize::MAX).min(len)
    }
}

/// Resolve an optional slice end (absent/null → `len`).
pub(crate) fn optional_slice_end(
    value: Option<&RuntimeValue>,
    len: usize,
    ctx: &str,
) -> Result<usize, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(len),
        Some(value) => Ok(normalize_slice_index(require_int_strict(value, ctx)?, len)),
    }
}

/// Error if `char_count` exceeds the configured string-size limit, then charge
/// the produced characters against the active allocation budget (a no-op
/// outside a sandbox). An O(1)-step builtin like `string_repeat` can still
/// allocate O(limit) chars, so a loop chaining them must be bounded too.
pub(crate) fn ensure_string_char_limit(
    context: &str,
    char_count: usize,
    ev: &crate::eval::Evaluator,
) -> Result<(), EvalSignal> {
    let limit = ev.runtime_collection_limit();
    if char_count > limit {
        return Err(eval_err(format!(
            "{context}: string size limit {limit} exceeded"
        )));
    }
    ev.charge_allocation(char_count)
}
