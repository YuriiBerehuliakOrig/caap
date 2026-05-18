/// CTFE surface construction builtins, ported from
/// `caap/builtins/compiler/surface.py`.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::eval::{eval_args, Evaluator};
use crate::frontend::{parse_forms, reparse_surface_rule, ParsedForm};
use crate::source::SourceSpan;
use crate::values::{eval_err, BuiltinInfo, EvalSignal, MapKey, RuntimeValue};

pub fn register(ev: &mut Evaluator) {
    ev.register_builtin(BuiltinInfo {
        name: "ctfe-surface-binding-get".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let key = require_string(&args[1], "ctfe-surface-binding-get expects a string key")?;
            let result = surface_binding_get(&args[0], &key)?;
            Ok(match result {
                Some(RuntimeValue::Null) | None => {
                    args.get(2).cloned().unwrap_or(RuntimeValue::Null)
                }
                Some(value) => value,
            })
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-surface-binding-group-collect".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-surface-unwrap".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(surface_unwrap(&args[0]))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-surface-form-symbol".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let name = require_string(&args[0], "ctfe-surface-form-symbol expects a string name")?;
            let span = require_span_map(&args[1], "ctfe-surface-form-symbol expects a span map")?;
            Ok(form_symbol(name, span))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-surface-form-integer".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let value = require_integer_payload(&args[0])?;
            let span = require_span_map(&args[1], "ctfe-surface-form-integer expects a span map")?;
            Ok(form_integer(value, span))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-surface-form-null".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let span = require_span_map(&args[0], "ctfe-surface-form-null expects a span map")?;
            Ok(form_null(span))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-surface-form-list".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let children = require_form_sequence(
                &args[0],
                "ctfe-surface-form-list expects a sequence of surface forms",
            )?;
            let span = require_span_map(&args[1], "ctfe-surface-form-list expects a span map")?;
            let delimiter = optional_delimiter(args.get(2))?;
            Ok(form_list(children, span, &delimiter))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-surface-form-list-prepend".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
            Ok(form_list(children, span, &delimiter))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-surface-parse-form".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-grammar-reparse-text".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let rule_name = require_string(
                &args[1],
                "ctfe-compiler-grammar-reparse-text expects a string rule name",
            )?;
            let source = require_string(
                &args[2],
                "ctfe-compiler-grammar-reparse-text expects a string source",
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
        }),
    });
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

fn parsed_form_to_value(form: &ParsedForm) -> RuntimeValue {
    match form {
        ParsedForm::List { items, span } => form_list(
            items.iter().map(parsed_form_to_value).collect(),
            span.clone(),
            "paren",
        ),
        ParsedForm::Symbol { text, span } => form_symbol(text.clone(), span.clone()),
        ParsedForm::String { value, raw, span } => form_atom(
            "string",
            string(value),
            span.clone(),
            raw.clone(),
            "string",
            Vec::new(),
            None,
            "paren",
        ),
        ParsedForm::Integer { value, raw, span } => form_atom(
            "integer",
            RuntimeValue::Int(*value),
            span.clone(),
            raw.clone(),
            "integer",
            Vec::new(),
            None,
            "paren",
        ),
        ParsedForm::Boolean { value, span } => form_atom(
            "boolean",
            RuntimeValue::Bool(*value),
            span.clone(),
            value.to_string(),
            "boolean",
            Vec::new(),
            None,
            "paren",
        ),
        ParsedForm::Null { span } => form_null(span.clone()),
    }
}

pub(crate) fn form_symbol(name: String, span: SourceSpan) -> RuntimeValue {
    form_atom(
        "symbol",
        string(name.as_str()),
        span,
        name,
        "symbol",
        Vec::new(),
        None,
        "paren",
    )
}

pub(crate) fn form_integer(value: i64, span: SourceSpan) -> RuntimeValue {
    form_atom(
        "integer",
        RuntimeValue::Int(value),
        span,
        value.to_string(),
        "integer",
        Vec::new(),
        None,
        "paren",
    )
}

pub(crate) fn form_null(span: SourceSpan) -> RuntimeValue {
    form_atom(
        "null",
        RuntimeValue::Null,
        span,
        "null".to_string(),
        "null",
        Vec::new(),
        None,
        "paren",
    )
}

pub(crate) fn form_list(
    children: Vec<RuntimeValue>,
    span: SourceSpan,
    delimiter: &str,
) -> RuntimeValue {
    let head = children.first().and_then(surface_form_symbol_value);
    form_atom(
        "list",
        RuntimeValue::Null,
        span,
        String::new(),
        "list",
        children,
        head,
        delimiter,
    )
}

pub(crate) fn form_atom(
    kind: &str,
    value: RuntimeValue,
    span: SourceSpan,
    raw_text: String,
    rule: &str,
    items: Vec<RuntimeValue>,
    head: Option<String>,
    delimiter: &str,
) -> RuntimeValue {
    map([
        ("kind", string(kind)),
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
        ("rule", string(rule)),
        ("delimiter", string(delimiter)),
    ])
}

fn require_surface_form(value: &RuntimeValue, message: &str) -> Result<(), EvalSignal> {
    let RuntimeValue::Map(fields) = value else {
        return Err(eval_err(message));
    };
    match fields.borrow().get(&str_key("kind")) {
        Some(RuntimeValue::Str(kind))
            if matches!(
                kind.as_ref(),
                "list" | "symbol" | "integer" | "string" | "boolean" | "null"
            ) =>
        {
            Ok(())
        }
        _ => Err(eval_err(message)),
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
        require_surface_form(item, &format!("{message}: item {index} is {item}"))?;
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

fn optional_delimiter(value: Option<&RuntimeValue>) -> Result<String, EvalSignal> {
    let delimiter = match value {
        None => "paren".to_string(),
        Some(value) => require_string(value, "surface-form-list expects a delimiter string")?,
    };
    match delimiter.as_str() {
        "paren" | "bracket" | "brace" => Ok(delimiter),
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
    SourceSpan::new(start, end, start_line, start_col, end_line, end_col).map_err(eval_err)
}

fn required_usize(
    fields: &HashMap<MapKey, RuntimeValue>,
    key: &str,
    message: &str,
) -> Result<usize, EvalSignal> {
    match fields.get(&str_key(key)) {
        Some(RuntimeValue::Int(value)) if *value >= 0 => Ok(*value as usize),
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
    ])
}

fn require_string(value: &RuntimeValue, message: &str) -> Result<String, EvalSignal> {
    match value {
        RuntimeValue::Str(value) => Ok(value.to_string()),
        _ => Err(eval_err(message)),
    }
}

fn tuple(items: Vec<RuntimeValue>) -> RuntimeValue {
    RuntimeValue::Tuple(items.into())
}

fn map<const N: usize>(entries: [(&str, RuntimeValue); N]) -> RuntimeValue {
    RuntimeValue::Map(Rc::new(RefCell::new(
        entries
            .into_iter()
            .map(|(key, value)| (str_key(key), value))
            .collect(),
    )))
}

fn string(value: impl AsRef<str>) -> RuntimeValue {
    RuntimeValue::Str(value.as_ref().into())
}

fn optional_string(value: Option<&str>) -> RuntimeValue {
    value.map(string).unwrap_or(RuntimeValue::Null)
}

fn str_key(key: &str) -> MapKey {
    MapKey::Str(key.into())
}
