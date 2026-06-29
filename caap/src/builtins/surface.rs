/// CTFE surface construction builtins, ported from
/// `caap/builtins/compiler/surface.py`.
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

use crate::eval::{eval_args, Evaluator};
use crate::frontend::{parse_forms, reparse_surface_rule, ParsedForm};
use crate::source::{SourceSpan, SourceSpanLocator};
use crate::values::{eval_err, EvalSignal, MapKey, RuntimeValue};

pub(crate) struct SurfaceAtomSpec {
    kind: String,
    value: RuntimeValue,
    span: SourceSpan,
    raw_text: String,
    rule: String,
    items: Vec<RuntimeValue>,
    head: Option<String>,
    // None = the form is not bracket-delimited (atoms; list rules whose match
    // starts with no real bracket literal) — surfaced as null, never a
    // fabricated "paren".
    delimiter: Option<String>,
}

impl SurfaceAtomSpec {
    pub(crate) fn new(kind: impl Into<String>, value: RuntimeValue, span: SourceSpan) -> Self {
        let kind = kind.into();
        Self {
            rule: kind.clone(),
            kind,
            value,
            span,
            raw_text: String::new(),
            items: Vec::new(),
            head: None,
            delimiter: None,
        }
    }

    pub(crate) fn raw_text(mut self, raw_text: impl Into<String>) -> Self {
        self.raw_text = raw_text.into();
        self
    }

    pub(crate) fn rule(mut self, rule: impl Into<String>) -> Self {
        self.rule = rule.into();
        self
    }

    pub(crate) fn items(mut self, items: Vec<RuntimeValue>) -> Self {
        self.items = items;
        self
    }

    pub(crate) fn head(mut self, head: Option<String>) -> Self {
        self.head = head;
        self
    }

    pub(crate) fn delimiter(mut self, delimiter: Option<String>) -> Self {
        self.delimiter = delimiter;
        self
    }
}

pub fn register(ev: &mut Evaluator) {
    ev.register_special(
        "ctfe_surface_binding_get",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let key = require_string(&args[1], "ctfe-surface-binding-get expects a string key")?;
            let result = surface_binding_get(&args[0], &key)?;
            Ok(match result {
                Some(RuntimeValue::Null) | None => {
                    args.get(2).cloned().unwrap_or(RuntimeValue::Null)
                }
                Some(value) => value,
            })
        },
    );

    ev.register_special(
        "ctfe_surface_binding_group_collect",
        1,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let item_key = match args.get(1) {
                None | Some(RuntimeValue::Null) => None,
                Some(value) => Some(require_string(
                    value,
                    "ctfe-surface-binding-group-collect expects item key to be a string",
                )?),
            };
            Ok(RuntimeValue::List(Rc::new(RefCell::new(
                surface_binding_group_collect(&args[0], item_key.as_deref())?,
            ))))
        },
    );

    ev.register_special(
        "ctfe_surface_unwrap",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(surface_unwrap(&args[0]))
        },
    );

    ev.register_special(
        "ctfe_surface_form_symbol",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let name = require_string(&args[0], "ctfe-surface-form-symbol expects a string name")?;
            let span = require_span_map(&args[1], "ctfe-surface-form-symbol expects a span map")?;
            Ok(form_symbol(name, span))
        },
    );

    ev.register_special(
        "ctfe_surface_form_integer",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let value = require_integer_payload(&args[0])?;
            let span = require_span_map(&args[1], "ctfe-surface-form-integer expects a span map")?;
            Ok(form_integer(value, span))
        },
    );

    // Float and boolean literal constructors: without these a lower hook could
    // only emit them via the parse_form detour (build text, re-parse it).
    ev.register_special(
        "ctfe_surface_form_float",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let value = match &args[0] {
                RuntimeValue::Float(f) => *f,
                RuntimeValue::Int(i) => *i as f64,
                other => {
                    return Err(eval_err(format!(
                        "ctfe-surface-form-float expects a float, got {other}"
                    )))
                }
            };
            let span = require_span_map(&args[1], "ctfe-surface-form-float expects a span map")?;
            Ok(form_float(value, span))
        },
    );
    ev.register_special(
        "ctfe_surface_form_bool",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let RuntimeValue::Bool(value) = &args[0] else {
                return Err(eval_err(format!(
                    "ctfe-surface-form-bool expects a bool, got {}",
                    &args[0]
                )));
            };
            let span = require_span_map(&args[1], "ctfe-surface-form-bool expects a span map")?;
            Ok(form_boolean(*value, span))
        },
    );
    ev.register_special(
        "ctfe_surface_form_string",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let value = require_string_payload(
                &args[0],
                "ctfe-surface-form-string expects a string value",
            )?;
            let span = require_span_map(&args[1], "ctfe-surface-form-string expects a span map")?;
            Ok(form_string(value, span))
        },
    );

    ev.register_special(
        "ctfe_surface_form_null",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let span = require_span_map(&args[0], "ctfe-surface-form-null expects a span map")?;
            Ok(form_null(span))
        },
    );

    ev.register_special(
        "ctfe_surface_form_list",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let children = require_form_sequence(
                &args[0],
                "ctfe-surface-form-list expects a sequence of surface forms",
            )?;
            let span = require_span_map(&args[1], "ctfe-surface-form-list expects a span map")?;
            let delimiter = optional_delimiter(args.get(2))?;
            Ok(form_list(children, span, delimiter))
        },
    );

    ev.register_special(
        "ctfe_surface_form_list_prepend",
        3,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            require_surface_form(
                &args[0],
                "ctfe-surface-form-list-prepend expects a surface form head",
            )?;
            let mut children = vec![args[0].clone()];
            children.extend(require_form_sequence(
                &args[1],
                "ctfe-surface-form-list-prepend expects a sequence tail",
            )?);
            let span = require_span_map(
                &args[2],
                "ctfe-surface-form-list-prepend expects a span map",
            )?;
            let delimiter = optional_delimiter(args.get(3))?;
            Ok(form_list(children, span, delimiter))
        },
    );

    ev.register_special(
        "ctfe_surface_parse_form",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let source =
                require_string(&args[0], "ctfe-surface-parse-form expects a string source")?;
            let parsed = parse_forms(&source).map_err(|error| {
                eval_err(format!(
                    "ctfe-surface-parse-form failed for {source:?}: {error}"
                ))
            })?;
            if parsed.forms.len() != 1 {
                return Err(eval_err("ctfe-surface-parse-form expects exactly one form"));
            }
            Ok(parsed_form_to_value(&parsed.forms[0]))
        },
    );

    // ctfe-surface-match form [kind-filter [head-filter]]
    // Returns the form map when the form matches all supplied filters, else null.
    //   1 arg  — succeeds for any valid surface form
    //   2 args — also checks form.kind == kind-filter
    //   3 args — also checks form.head == head-filter (meaningful for "list" forms)

    ev.register_special(
        "ctfe_surface_reparse_text",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let rule_name = require_string(
                &args[0],
                "ctfe-surface-reparse-text expects a string rule name",
            )?;
            let source = require_string(
                &args[1],
                "ctfe-surface-reparse-text expects a string source",
            )?;
            let parsed = reparse_surface_rule(&rule_name, &source).map_err(|error| {
                eval_err(format!("Reparse failed for rule {rule_name:?}: {error}"))
            })?;
            match parsed.forms.as_slice() {
                [form] => Ok(parsed_form_to_value(form)),
                forms => Ok(RuntimeValue::List(Rc::new(RefCell::new(
                    forms.iter().map(parsed_form_to_value).collect(),
                )))),
            }
        },
    );
}

fn surface_binding_get(
    value: &RuntimeValue,
    key: &str,
) -> Result<Option<RuntimeValue>, EvalSignal> {
    let RuntimeValue::Map(fields) = value else {
        return Err(eval_err(
            "surface binding lookup expects a mapping of named bindings",
        ));
    };
    Ok(fields.borrow().get(&str_key(key)).cloned())
}

fn surface_binding_group_collect(
    value: &RuntimeValue,
    item_key: Option<&str>,
) -> Result<Vec<RuntimeValue>, EvalSignal> {
    if matches!(value, RuntimeValue::Null) {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    if let Some(first) =
        surface_binding_get(value, "first")?.filter(|value| !matches!(value, RuntimeValue::Null))
    {
        items.push(first);
    }
    let Some(rest) = surface_binding_get(value, "rest")? else {
        return Ok(items);
    };
    if matches!(rest, RuntimeValue::Null) {
        return Ok(items);
    }
    let rest_items = sequence_items(
        &rest,
        "surface binding group collect expects rest to be a sequence",
    )?;
    for entry in rest_items {
        items.push(match item_key {
            Some(key) => surface_binding_get(&entry, key)?.unwrap_or(RuntimeValue::Null),
            None => entry,
        });
    }
    Ok(items)
}

fn surface_unwrap(value: &RuntimeValue) -> RuntimeValue {
    let RuntimeValue::Map(fields) = value else {
        return value.clone();
    };
    let fields = fields.borrow();
    let Some(RuntimeValue::Str(wrapper_kind)) = fields.get(&str_key("wrapper")) else {
        return value.clone();
    };
    if wrapper_kind.as_ref() != "spanned" && wrapper_kind.as_ref() != "named" {
        return value.clone();
    }
    fields
        .get(&str_key("value"))
        .cloned()
        .unwrap_or(RuntimeValue::Null)
}

fn require_string_payload(value: &RuntimeValue, message: &str) -> Result<String, EvalSignal> {
    surface_string_payload(value).ok_or_else(|| eval_err(message))
}

fn surface_string_payload(value: &RuntimeValue) -> Option<String> {
    match value {
        RuntimeValue::Str(value) => Some(value.to_string()),
        RuntimeValue::Map(fields) => {
            let fields = fields.borrow();
            if matches!(
                fields.get(&str_key("wrapper")),
                Some(RuntimeValue::Str(wrapper)) if wrapper.as_ref() == "spanned" || wrapper.as_ref() == "named"
            ) {
                return fields
                    .get(&str_key("value"))
                    .and_then(surface_string_payload);
            }
            if !matches!(
                fields.get(&str_key("kind")),
                Some(RuntimeValue::Str(kind)) if kind.as_ref() == "string"
            ) {
                return None;
            }
            match fields
                .get(&str_key("value"))
                .or_else(|| fields.get(&str_key("atom_value")))
            {
                Some(RuntimeValue::Str(value)) => Some(value.to_string()),
                _ => None,
            }
        }
        _ => None,
    }
}

pub(crate) fn parsed_form_to_value(form: &ParsedForm) -> RuntimeValue {
    match form {
        // ParsedForm comes from the kernel S-expression reader, where every
        // list really is parenthesised.
        ParsedForm::List { items, span } => form_list(
            items.iter().map(parsed_form_to_value).collect(),
            span.clone(),
            Some("paren".to_string()),
        ),
        ParsedForm::Symbol { text, span } => form_symbol(text.clone(), span.clone()),
        ParsedForm::String { value, raw, span } => form_atom(
            SurfaceAtomSpec::new("string", string(value), span.clone()).raw_text(raw.clone()),
        ),
        ParsedForm::Integer { value, raw, span } => form_atom(
            SurfaceAtomSpec::new("integer", RuntimeValue::Int(*value), span.clone())
                .raw_text(raw.clone()),
        ),
        ParsedForm::Float { value, raw, span } => form_atom(
            SurfaceAtomSpec::new("float", RuntimeValue::Float(*value), span.clone())
                .raw_text(raw.clone()),
        ),
        ParsedForm::Boolean { value, span } => form_atom(
            SurfaceAtomSpec::new("boolean", RuntimeValue::Bool(*value), span.clone())
                .raw_text(value.to_string()),
        ),
        ParsedForm::Null { span } => form_null(span.clone()),
    }
}

pub(crate) fn form_symbol(name: String, span: SourceSpan) -> RuntimeValue {
    form_atom(SurfaceAtomSpec::new("symbol", string(name.as_str()), span).raw_text(name))
}

pub(crate) fn form_integer(value: i64, span: SourceSpan) -> RuntimeValue {
    form_atom(
        SurfaceAtomSpec::new("integer", RuntimeValue::Int(value), span).raw_text(value.to_string()),
    )
}

pub(crate) fn form_string(value: String, span: SourceSpan) -> RuntimeValue {
    let raw = format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""));
    form_atom(SurfaceAtomSpec::new("string", string(value.as_str()), span).raw_text(raw))
}

pub(crate) fn form_float(value: f64, span: SourceSpan) -> RuntimeValue {
    form_atom(
        SurfaceAtomSpec::new("float", RuntimeValue::Float(value), span).raw_text(value.to_string()),
    )
}

pub(crate) fn form_boolean(value: bool, span: SourceSpan) -> RuntimeValue {
    form_atom(
        SurfaceAtomSpec::new("boolean", RuntimeValue::Bool(value), span)
            .raw_text(value.to_string()),
    )
}

pub(crate) fn form_null(span: SourceSpan) -> RuntimeValue {
    form_atom(SurfaceAtomSpec::new("null", RuntimeValue::Null, span).raw_text("null"))
}

pub(crate) fn form_list(
    children: Vec<RuntimeValue>,
    span: SourceSpan,
    delimiter: Option<String>,
) -> RuntimeValue {
    let head = children.first().and_then(surface_form_symbol_value);
    form_atom(
        SurfaceAtomSpec::new("list", RuntimeValue::Null, span)
            .items(children)
            .head(head)
            .delimiter(delimiter),
    )
}

pub(crate) fn form_atom(spec: SurfaceAtomSpec) -> RuntimeValue {
    let SurfaceAtomSpec {
        kind,
        value,
        span,
        raw_text,
        rule,
        items,
        head,
        delimiter,
    } = spec;
    map([
        ("kind", string(kind.as_str())),
        ("value", value.clone()),
        ("atom_value", value.clone()),
        ("items", tuple(items.clone())),
        ("children", tuple(items.clone())),
        (
            "args",
            tuple(if kind == "list" && !items.is_empty() {
                items[1..].to_vec()
            } else {
                Vec::new()
            }),
        ),
        ("head", optional_string(head.as_deref())),
        ("tag", optional_string(head.as_deref())),
        ("span", span_to_value(&span)),
        ("grammar_version", RuntimeValue::Int(0)),
        ("raw_text", string(raw_text)),
        ("rule", string(rule.as_str())),
        ("delimiter", optional_string(delimiter.as_deref())),
    ])
}

fn is_surface_form(value: &RuntimeValue) -> bool {
    let RuntimeValue::Map(fields) = value else {
        return false;
    };
    matches!(
        fields.borrow().get(&str_key("kind")),
        Some(RuntimeValue::Str(kind))
            if matches!(
                kind.as_ref(),
                "list" | "symbol" | "integer" | "string" | "boolean" | "null"
            )
    )
}

fn require_surface_form(value: &RuntimeValue, message: &str) -> Result<(), EvalSignal> {
    if is_surface_form(value) {
        Ok(())
    } else {
        Err(eval_err(message))
    }
}

fn surface_form_symbol_value(value: &RuntimeValue) -> Option<String> {
    let RuntimeValue::Map(fields) = value else {
        return None;
    };
    let fields = fields.borrow();
    let Some(RuntimeValue::Str(kind)) = fields.get(&str_key("kind")) else {
        return None;
    };
    if kind.as_ref() != "symbol" {
        return None;
    }
    match fields.get(&str_key("value")) {
        Some(RuntimeValue::Str(name)) => Some(name.to_string()),
        _ => None,
    }
}

fn require_form_sequence(
    value: &RuntimeValue,
    message: &str,
) -> Result<Vec<RuntimeValue>, EvalSignal> {
    if matches!(value, RuntimeValue::Null) {
        return Ok(Vec::new());
    }
    let items = sequence_items(value, message)?;
    for (index, item) in items.iter().enumerate() {
        // NB: do not `Display`-format `item` into the message — surface-form
        // values can share substructure (a DAG), so formatting an arbitrary
        // item on this hot success path is O(exponential) in the sharing depth
        // and turns a large lowered program into an effectively unbounded
        // string build. Only the cheap structural predicate runs per item; the
        // failure message names the index, never the value.
        if !is_surface_form(item) {
            return Err(eval_err(format!(
                "{message}: item {index} is not a surface form"
            )));
        }
    }
    Ok(items)
}

fn sequence_items(value: &RuntimeValue, message: &str) -> Result<Vec<RuntimeValue>, EvalSignal> {
    match value {
        RuntimeValue::Tuple(items) => Ok(items.iter().cloned().collect()),
        RuntimeValue::List(items) => Ok(items.borrow().iter().cloned().collect()),
        _ => Err(eval_err(message)),
    }
}

fn require_integer_payload(value: &RuntimeValue) -> Result<i64, EvalSignal> {
    match value {
        RuntimeValue::Int(value) => Ok(*value),
        RuntimeValue::Str(text) => text.parse().map_err(|error| {
            eval_err(format!(
                "surface-form-atom integer expects text or integer payload: {error}"
            ))
        }),
        _ => Err(eval_err(
            "surface-form-atom integer expects text or integer payload",
        )),
    }
}

fn optional_delimiter(value: Option<&RuntimeValue>) -> Result<Option<String>, EvalSignal> {
    let delimiter = match value {
        None | Some(RuntimeValue::Null) => return Ok(None),
        Some(value) => require_string(value, "surface-form-list expects a delimiter string")?,
    };
    match delimiter.as_str() {
        "paren" | "bracket" | "brace" => Ok(Some(delimiter)),
        _ => Err(eval_err(format!(
            "unsupported list delimiter {delimiter:?}"
        ))),
    }
}

pub(crate) fn require_span_map(
    value: &RuntimeValue,
    message: &str,
) -> Result<SourceSpan, EvalSignal> {
    let RuntimeValue::Map(fields) = value else {
        return Err(eval_err(message));
    };
    let fields = fields.borrow();
    let start = required_usize(&fields, "start", message)?;
    let end = required_usize(&fields, "end", message)?;
    let start_line = required_usize(&fields, "start_line", message)?;
    let start_col = required_usize(&fields, "start_col", message)?;
    let end_line = required_usize(&fields, "end_line", message)?;
    let end_col = required_usize(&fields, "end_col", message)?;
    let locator = optional_span_path(&fields).map_err(eval_err)?;
    SourceSpan::with_locator(
        locator, start, end, start_line, start_col, end_line, end_col,
    )
    .map_err(eval_err)
}

/// Parse an optional `path` field into a `SourceSpanLocator`. Absent or null →
/// `None` (no locator).
fn optional_span_path(
    fields: &IndexMap<MapKey, RuntimeValue>,
) -> Result<Option<SourceSpanLocator>, crate::error::CaapError> {
    match fields.get(&str_key("path")) {
        Some(RuntimeValue::Str(path)) => {
            SourceSpanLocator::new(None, Some(path.to_string())).map(Some)
        }
        _ => Ok(None),
    }
}

fn required_usize(
    fields: &IndexMap<MapKey, RuntimeValue>,
    key: &str,
    message: &str,
) -> Result<usize, EvalSignal> {
    match fields.get(&str_key(key)) {
        Some(RuntimeValue::Int(value)) if *value >= 0 => usize::try_from(*value)
            .map_err(|_| eval_err(format!("{message}: integer {key:?} is too large"))),
        _ => Err(eval_err(format!(
            "{message}: missing non-negative integer {key:?}"
        ))),
    }
}

pub(crate) fn span_to_value(span: &SourceSpan) -> RuntimeValue {
    map([
        ("start", RuntimeValue::Int(span.start as i64)),
        ("end", RuntimeValue::Int(span.end as i64)),
        ("start_line", RuntimeValue::Int(span.start_line as i64)),
        ("start_col", RuntimeValue::Int(span.start_col as i64)),
        ("end_line", RuntimeValue::Int(span.end_line as i64)),
        ("end_col", RuntimeValue::Int(span.end_col as i64)),
        // Carry the source path so spans survive the surface-form round-trip
        // (grammar `lower` lambdas → IR), which source-level tooling (the
        // debugger) needs to map nodes to files. Null when absent.
        (
            "path",
            span.path.as_ref().map(string).unwrap_or(RuntimeValue::Null),
        ),
    ])
}

// `require_string` (any string) and `tuple` are canonical; surface keeps its own
// `string`/`str_key`/`map` helpers. These build `Rc<str>` DIRECTLY rather than
// going through `intern_string`: they funnel arbitrary RUNTIME string values
// (string-literal payloads, symbol names, source paths) and lookup keys, which
// are unbounded and must not accumulate in the append-only intern pool.
// Interning stays reserved for the bounded set of IR identifiers
// (`NameNode::new`) and fixed parse-tree literal keys.
use super::args::{require_string, tuple};

fn map<const N: usize>(entries: [(&str, RuntimeValue); N]) -> RuntimeValue {
    RuntimeValue::Map(Rc::new(RefCell::new(
        entries
            .into_iter()
            .map(|(key, value)| (str_key(key), value))
            .collect(),
    )))
}

fn string(value: impl AsRef<str>) -> RuntimeValue {
    RuntimeValue::Str(Rc::from(value.as_ref()))
}

fn optional_string(value: Option<&str>) -> RuntimeValue {
    value.map(string).unwrap_or(RuntimeValue::Null)
}

fn str_key(key: &str) -> MapKey {
    MapKey::Str(Rc::from(key))
}
#[cfg(test)]
mod tests {
    use super::*;

    fn span() -> SourceSpan {
        SourceSpan::new(0, 1, 1, 1, 1, 2).unwrap()
    }

    // Surface runtime strings must NOT be interned: the intern pool is
    // append-only and would leak unboundedly for distinct CTFE/effect-scope
    // string values. Equal `string()` calls therefore produce content-equal
    // but pointer-DISTINCT `Rc`s (interning would have deduped them to one
    // shared allocation, i.e. `Rc::ptr_eq` would hold).
    #[test]
    fn string_does_not_intern_runtime_values() {
        let RuntimeValue::Str(a) = string("a-distinct-runtime-string") else {
            panic!("string() must produce a Str");
        };
        let RuntimeValue::Str(b) = string("a-distinct-runtime-string") else {
            panic!("string() must produce a Str");
        };

        assert_eq!(a.as_ref(), b.as_ref());
        assert!(
            !Rc::ptr_eq(&a, &b),
            "surface `string()` must not pool runtime strings via intern_string"
        );
    }

    // Map keys built by `str_key` (lookup keys / fixed literal keys) likewise
    // must bypass the intern pool.
    #[test]
    fn str_key_does_not_intern() {
        let MapKey::Str(a) = str_key("a-distinct-runtime-key") else {
            panic!("str_key() must produce a Str key");
        };
        let MapKey::Str(b) = str_key("a-distinct-runtime-key") else {
            panic!("str_key() must produce a Str key");
        };

        assert_eq!(a.as_ref(), b.as_ref());
        assert!(
            !Rc::ptr_eq(&a, &b),
            "surface `str_key()` must not pool keys via intern_string"
        );
    }

    #[test]
    fn surface_unwrap_only_unwraps_explicit_wrappers() {
        let plain = map([("value", string("payload"))]);

        assert!(matches!(surface_unwrap(&plain), RuntimeValue::Map(_)));

        let wrapped = map([("wrapper", string("named")), ("value", string("payload"))]);
        assert_eq!(surface_unwrap(&wrapped), string("payload"));
    }

    #[test]
    fn string_payload_accepts_surface_string_forms() {
        let form = form_string("payload".to_string(), span());

        assert_eq!(surface_string_payload(&form), Some("payload".to_string()));
    }

    #[test]
    fn string_payload_rejects_generic_value_maps() {
        let plain = map([("value", string("payload"))]);

        assert_eq!(surface_string_payload(&plain), None);
    }

    #[test]
    fn require_span_map_rejects_negative_offsets() {
        let value = map([
            ("start", RuntimeValue::Int(-1)),
            ("end", RuntimeValue::Int(1)),
            ("start_line", RuntimeValue::Int(1)),
            ("start_col", RuntimeValue::Int(1)),
            ("end_line", RuntimeValue::Int(1)),
            ("end_col", RuntimeValue::Int(2)),
        ]);

        let err = require_span_map(&value, "span required").unwrap_err();

        assert!(err
            .to_string()
            .contains("missing non-negative integer \"start\""));
    }
}
