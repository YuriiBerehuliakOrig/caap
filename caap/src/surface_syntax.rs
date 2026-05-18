//! Surface grammar materialization from syntax-unit state.
//!
//! Syntax authoring writes neutral rule specs into [`UnitSyntaxState`].  This
//! module is the bridge that turns those specs back into an executable PEG
//! grammar without baking any CAAP-specific syntax extension into the core
//! parser.

use caap_peg_port::{
    Grammar, GrammarScalar, ParseValue, SemanticContext, SemanticRuntime, SpecCompiler,
};
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::rc::Rc;

use crate::builtins::surface::form_atom;
use crate::eval::{eval_args, Evaluator};
use crate::frontend::{ParsedForm, ParsedSource};
use crate::semantic::SemanticValue;
use crate::source::SourceSpan;
use crate::unit::UnitSyntaxState;
use crate::values::{BuiltinInfo, MapKey, RuntimeValue};

const BASE_RULE_NAMES: &[&str] = &[
    "forms", "form", "list", "string", "integer", "boolean", "null", "symbol",
];

pub fn compile_surface_grammar_from_syntax_state(
    syntax: &UnitSyntaxState,
) -> Result<Grammar, String> {
    let spec = surface_grammar_spec_from_syntax_state(syntax)?;
    SpecCompiler::new()
        .compile(&spec)
        .map_err(|error| format!("failed to compile surface syntax grammar: {error}"))
}

pub fn surface_grammar_spec_from_syntax_state(syntax: &UnitSyntaxState) -> Result<Value, String> {
    let mut rules = base_surface_rules();
    for (name, expr) in &syntax.grammar_rules {
        rules.insert(name.clone(), semantic_value_to_json(expr)?);
    }

    let mut rule_entries = Vec::with_capacity(rules.len());
    for (name, expr) in rules {
        let mut entry = vec![
            Value::String("rule".to_string()),
            Value::String(name.clone()),
            expr,
        ];
        if let Some(SemanticValue::Map(metadata)) = syntax.grammar_metadata.get(&name) {
            for (key, value) in metadata {
                entry.push(json!(["metadata", key, semantic_value_to_json(value)?]));
            }
        }
        rule_entries.push(Value::Array(entry));
    }

    let mut spec = vec![
        Value::String("grammar".to_string()),
        Value::String(syntax.language.clone()),
        Value::String("forms".to_string()),
        Value::Array(rule_entries),
    ];
    if !syntax.grammar_rules.is_empty() {
        spec.push(json!(["grammar_metadata", "trivia", "whitespace"]));
    }
    for (key, value) in &syntax.grammar_metadata {
        if BASE_RULE_NAMES.contains(&key.as_str()) || syntax.grammar_rules.contains_key(key) {
            continue;
        }
        spec.push(json!([
            "grammar_metadata",
            key,
            semantic_value_to_json(value)?
        ]));
    }

    Ok(Value::Array(spec))
}

pub fn semantic_value_to_json(value: &SemanticValue) -> Result<Value, String> {
    match value {
        SemanticValue::Null => Ok(Value::Null),
        SemanticValue::Bool(value) => Ok(Value::Bool(*value)),
        SemanticValue::Int(value) => Ok(Value::Number((*value).into())),
        SemanticValue::Float(value) => serde_json::Number::from_f64(*value)
            .map(Value::Number)
            .ok_or_else(|| format!("semantic float cannot be represented as JSON: {value}")),
        SemanticValue::Str(value) => Ok(Value::String(value.clone())),
        SemanticValue::Node(node) => Err(format!(
            "semantic node references are not valid grammar spec values: {node}"
        )),
        SemanticValue::List(items) => semantic_list_to_json(items),
        SemanticValue::Map(entries) => {
            let mut map = Map::new();
            for (key, value) in entries {
                map.insert(key.clone(), semantic_value_to_json(value)?);
            }
            Ok(Value::Object(map))
        }
    }
}

pub fn runtime_value_to_parse_value(value: &RuntimeValue) -> Result<ParseValue, String> {
    match value {
        RuntimeValue::Null => Ok(ParseValue::Node("__caap_rt_null".to_string(), Vec::new())),
        RuntimeValue::Bool(value) => Ok(ParseValue::Node(
            "__caap_rt_bool".to_string(),
            vec![ParseValue::Text(value.to_string())],
        )),
        RuntimeValue::Int(value) => Ok(ParseValue::Number(*value)),
        RuntimeValue::Float(value) => Ok(ParseValue::Node(
            "__caap_rt_float".to_string(),
            vec![ParseValue::Text(value.to_string())],
        )),
        RuntimeValue::Str(value) => Ok(ParseValue::Text(value.to_string())),
        RuntimeValue::Tuple(items) => items
            .iter()
            .map(runtime_value_to_parse_value)
            .collect::<Result<Vec<_>, _>>()
            .map(|items| ParseValue::Node("__caap_rt_tuple".to_string(), items)),
        RuntimeValue::List(items) => items
            .borrow()
            .iter()
            .map(runtime_value_to_parse_value)
            .collect::<Result<Vec<_>, _>>()
            .map(|items| ParseValue::Node("__caap_rt_list".to_string(), items)),
        RuntimeValue::Map(entries) => {
            let entries = entries.borrow();
            if let Some(value) = runtime_surface_form_map_to_parse_value(&entries)? {
                return Ok(value);
            }
            let mut pairs = Vec::with_capacity(entries.len());
            for (key, value) in entries.iter() {
                pairs.push(ParseValue::Node(
                    "__caap_rt_pair".to_string(),
                    vec![
                        runtime_value_to_parse_value(&RuntimeValue::from(key.clone()))?,
                        runtime_value_to_parse_value(value)?,
                    ],
                ));
            }
            Ok(ParseValue::Node("__caap_rt_map".to_string(), pairs))
        }
        RuntimeValue::Closure(_)
        | RuntimeValue::Builtin(_)
        | RuntimeValue::HostFunction(_)
        | RuntimeValue::HostObject(_)
        | RuntimeValue::UninitializedTopLevel => Err(format!(
            "runtime value {value} cannot be embedded in a PEG parse value"
        )),
    }
}

pub fn parse_value_to_runtime_value(value: &ParseValue) -> Result<RuntimeValue, String> {
    match value {
        ParseValue::Nil => Ok(RuntimeValue::Null),
        ParseValue::Text(value) => Ok(RuntimeValue::Str(Rc::from(value.as_str()))),
        ParseValue::Number(value) => Ok(RuntimeValue::Int(*value)),
        ParseValue::Named(_, value) => parse_value_to_runtime_value(value),
        ParseValue::SpannedValue { value, .. } => parse_value_to_runtime_value(value),
        ParseValue::Node(kind, children) if kind == "__caap_rt_null" => Ok(RuntimeValue::Null),
        ParseValue::Node(kind, children) if kind == "__caap_rt_bool" => {
            let Some(ParseValue::Text(value)) = children.first() else {
                return Err("encoded runtime bool is missing payload".to_string());
            };
            match value.as_str() {
                "true" => Ok(RuntimeValue::Bool(true)),
                "false" => Ok(RuntimeValue::Bool(false)),
                _ => Err(format!("encoded runtime bool has invalid payload: {value}")),
            }
        }
        ParseValue::Node(kind, children) if kind == "__caap_rt_float" => {
            let Some(ParseValue::Text(value)) = children.first() else {
                return Err("encoded runtime float is missing payload".to_string());
            };
            value
                .parse::<f64>()
                .map(RuntimeValue::Float)
                .map_err(|error| format!("encoded runtime float is invalid: {error}"))
        }
        ParseValue::Node(kind, children) if kind == "__caap_rt_surface_form" => {
            parse_encoded_surface_form(children)
        }
        ParseValue::Node(kind, children) if kind == "__caap_rt_tuple" => children
            .iter()
            .map(parse_value_to_runtime_value)
            .collect::<Result<Vec<_>, _>>()
            .map(|items| RuntimeValue::Tuple(items.into())),
        ParseValue::Node(kind, children) if kind == "__caap_rt_list" => children
            .iter()
            .map(parse_value_to_runtime_value)
            .collect::<Result<Vec<_>, _>>()
            .map(|items| RuntimeValue::List(Rc::new(RefCell::new(items)))),
        ParseValue::Node(kind, children) if kind == "__caap_rt_map" => {
            let mut map = HashMap::new();
            for child in children {
                let ParseValue::Node(pair_kind, pair) = child else {
                    return Err("encoded runtime map contains a non-pair entry".to_string());
                };
                if pair_kind != "__caap_rt_pair" || pair.len() != 2 {
                    return Err("encoded runtime map contains malformed pair entry".to_string());
                }
                let key_value = parse_value_to_runtime_value(&pair[0])?;
                let key = MapKey::try_from(&key_value)
                    .map_err(|error| format!("encoded runtime map key is invalid: {error}"))?;
                map.insert(key, parse_value_to_runtime_value(&pair[1])?);
            }
            Ok(RuntimeValue::Map(Rc::new(RefCell::new(map))))
        }
        ParseValue::Node(kind, children)
            if matches!(
                kind.as_str(),
                "one_or_more" | "zero_or_more" | "sep_one_or_more"
            ) =>
        {
            children
                .iter()
                .map(parse_value_to_runtime_value)
                .collect::<Result<Vec<_>, _>>()
                .map(|items| RuntimeValue::List(Rc::new(RefCell::new(items))))
        }
        ParseValue::Node(_, children) if children.len() == 1 => {
            parse_value_to_runtime_value(&children[0])
        }
        ParseValue::Node(_, children) => children
            .iter()
            .map(parse_value_to_runtime_value)
            .collect::<Result<Vec<_>, _>>()
            .map(|items| RuntimeValue::List(Rc::new(RefCell::new(items)))),
    }
}

fn runtime_surface_form_map_to_parse_value(
    fields: &HashMap<MapKey, RuntimeValue>,
) -> Result<Option<ParseValue>, String> {
    let Some(RuntimeValue::Str(kind)) = fields.get(&MapKey::Str(Rc::from("kind"))) else {
        return Ok(None);
    };
    if !matches!(
        kind.as_ref(),
        "list" | "symbol" | "string" | "integer" | "boolean" | "null"
    ) {
        return Ok(None);
    }
    let span = required_span_field(fields)?;
    let value = fields
        .get(&MapKey::Str(Rc::from("value")))
        .cloned()
        .unwrap_or(RuntimeValue::Null);
    let raw_text = optional_string_field(fields, "raw_text").unwrap_or_default();
    let rule = optional_string_field(fields, "rule").unwrap_or_else(|| kind.to_string());
    let delimiter = optional_string_field(fields, "delimiter").unwrap_or_else(|| "paren".into());
    let items = fields
        .get(&MapKey::Str(Rc::from("items")))
        .cloned()
        .unwrap_or_else(|| RuntimeValue::Tuple(Vec::new().into()));
    Ok(Some(ParseValue::Node(
        "__caap_rt_surface_form".to_string(),
        vec![
            ParseValue::Text(kind.to_string()),
            runtime_value_to_parse_value(&value)?,
            ParseValue::Text(raw_text),
            ParseValue::Text(rule),
            ParseValue::Text(delimiter),
            ParseValue::Number(span.start as i64),
            ParseValue::Number(span.end as i64),
            ParseValue::Number(span.start_line as i64),
            ParseValue::Number(span.start_col as i64),
            ParseValue::Number(span.end_line as i64),
            ParseValue::Number(span.end_col as i64),
            runtime_value_to_parse_value(&items)?,
        ],
    )))
}

fn parse_encoded_surface_form(children: &[ParseValue]) -> Result<RuntimeValue, String> {
    if children.len() != 12 {
        return Err("encoded surface form has malformed arity".to_string());
    }
    let kind = encoded_text_child(children, 0, "kind")?;
    let value = parse_value_to_runtime_value(&children[1])?;
    let raw_text = encoded_text_child(children, 2, "raw_text")?.to_string();
    let rule = encoded_text_child(children, 3, "rule")?.to_string();
    let delimiter = encoded_text_child(children, 4, "delimiter")?.to_string();
    let span = SourceSpan::new(
        encoded_usize_child(children, 5, "start")?,
        encoded_usize_child(children, 6, "end")?,
        encoded_usize_child(children, 7, "start_line")?,
        encoded_usize_child(children, 8, "start_col")?,
        encoded_usize_child(children, 9, "end_line")?,
        encoded_usize_child(children, 10, "end_col")?,
    )?;
    let items = encoded_sequence_items(&parse_value_to_runtime_value(&children[11])?)?;
    Ok(form_atom(
        kind, value, span, raw_text, &rule, items, None, &delimiter,
    ))
}

fn encoded_text_child<'a>(
    children: &'a [ParseValue],
    index: usize,
    label: &str,
) -> Result<&'a str, String> {
    match children.get(index) {
        Some(ParseValue::Text(value)) => Ok(value.as_str()),
        _ => Err(format!("encoded surface form missing text child {label:?}")),
    }
}

fn encoded_usize_child(
    children: &[ParseValue],
    index: usize,
    label: &str,
) -> Result<usize, String> {
    match children.get(index) {
        Some(ParseValue::Number(value)) if *value >= 0 => Ok(*value as usize),
        _ => Err(format!(
            "encoded surface form missing non-negative integer child {label:?}"
        )),
    }
}

fn encoded_sequence_items(value: &RuntimeValue) -> Result<Vec<RuntimeValue>, String> {
    match value {
        RuntimeValue::Null => Ok(Vec::new()),
        RuntimeValue::Tuple(items) => Ok(items.iter().cloned().collect()),
        RuntimeValue::List(items) => Ok(items.borrow().clone()),
        item => Ok(vec![item.clone()]),
    }
}

pub fn named_parse_bindings_to_runtime_map(
    value: &ParseValue,
) -> Result<HashMap<MapKey, RuntimeValue>, String> {
    let mut fields = HashMap::new();
    for (name, bound) in value.named_bindings() {
        fields.insert(
            MapKey::Str(Rc::from(name.as_str())),
            parse_value_to_runtime_value(bound)?,
        );
    }
    Ok(fields)
}

pub fn parse_value_to_parsed_source(value: &ParseValue) -> Result<ParsedSource, String> {
    let runtime = parse_value_to_runtime_value(value)?;
    match runtime {
        RuntimeValue::List(items) => Ok(ParsedSource {
            forms: items
                .borrow()
                .iter()
                .map(runtime_surface_form_to_parsed_form)
                .collect::<Result<Vec<_>, _>>()?,
        }),
        RuntimeValue::Tuple(items) => Ok(ParsedSource {
            forms: items
                .iter()
                .map(runtime_surface_form_to_parsed_form)
                .collect::<Result<Vec<_>, _>>()?,
        }),
        form => Ok(ParsedSource {
            forms: vec![runtime_surface_form_to_parsed_form(&form)?],
        }),
    }
}

pub fn runtime_surface_form_to_parsed_form(value: &RuntimeValue) -> Result<ParsedForm, String> {
    let RuntimeValue::Map(fields) = value else {
        return Err(format!("surface parse result is not a form map: {value}"));
    };
    let fields = fields.borrow();
    let kind = required_str_field(&fields, "kind")?;
    let span = required_span_field(&fields)?;
    match kind {
        "list" => {
            let items = optional_sequence_field(&fields, "items")?
                .into_iter()
                .map(|item| runtime_surface_form_to_parsed_form(&item))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ParsedForm::List { items, span })
        }
        "symbol" => Ok(ParsedForm::Symbol {
            text: required_string_like_field(&fields, "value")?,
            span,
        }),
        "string" => {
            let value = required_string_like_field(&fields, "value")?;
            let raw = optional_string_field(&fields, "raw_text").unwrap_or_else(|| {
                serde_json::to_string(&value).unwrap_or_else(|_| format!("\"{value}\""))
            });
            Ok(ParsedForm::String { value, raw, span })
        }
        "integer" => {
            let value = required_int_field(&fields, "value")?;
            let raw =
                optional_string_field(&fields, "raw_text").unwrap_or_else(|| value.to_string());
            Ok(ParsedForm::Integer { value, raw, span })
        }
        "boolean" => Ok(ParsedForm::Boolean {
            value: required_bool_field(&fields, "value")?,
            span,
        }),
        "null" => Ok(ParsedForm::Null { span }),
        other => Err(format!("unknown surface form kind: {other}")),
    }
}

fn required_str_field<'a>(
    fields: &'a HashMap<MapKey, RuntimeValue>,
    key: &str,
) -> Result<&'a str, String> {
    match fields.get(&MapKey::Str(Rc::from(key))) {
        Some(RuntimeValue::Str(value)) => Ok(value.as_ref()),
        _ => Err(format!("surface form missing string field {key:?}")),
    }
}

fn optional_string_field(fields: &HashMap<MapKey, RuntimeValue>, key: &str) -> Option<String> {
    match fields.get(&MapKey::Str(Rc::from(key))) {
        Some(RuntimeValue::Str(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn required_string_like_field(
    fields: &HashMap<MapKey, RuntimeValue>,
    key: &str,
) -> Result<String, String> {
    match fields.get(&MapKey::Str(Rc::from(key))) {
        Some(RuntimeValue::Str(value)) => Ok(value.to_string()),
        Some(value) => Ok(value.to_string()),
        None => Err(format!("surface form missing field {key:?}")),
    }
}

fn required_int_field(fields: &HashMap<MapKey, RuntimeValue>, key: &str) -> Result<i64, String> {
    match fields.get(&MapKey::Str(Rc::from(key))) {
        Some(RuntimeValue::Int(value)) => Ok(*value),
        Some(RuntimeValue::Str(value)) => value
            .parse::<i64>()
            .map_err(|error| format!("surface integer field {key:?} is invalid: {error}")),
        _ => Err(format!("surface form missing integer field {key:?}")),
    }
}

fn required_bool_field(fields: &HashMap<MapKey, RuntimeValue>, key: &str) -> Result<bool, String> {
    match fields.get(&MapKey::Str(Rc::from(key))) {
        Some(RuntimeValue::Bool(value)) => Ok(*value),
        Some(RuntimeValue::Str(value)) => match value.as_ref() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(format!("surface bool field {key:?} is invalid: {value}")),
        },
        _ => Err(format!("surface form missing bool field {key:?}")),
    }
}

fn optional_sequence_field(
    fields: &HashMap<MapKey, RuntimeValue>,
    key: &str,
) -> Result<Vec<RuntimeValue>, String> {
    match fields.get(&MapKey::Str(Rc::from(key))) {
        None | Some(RuntimeValue::Null) => Ok(Vec::new()),
        Some(RuntimeValue::Tuple(items)) => Ok(items.iter().cloned().collect()),
        Some(RuntimeValue::List(items)) => Ok(items.borrow().clone()),
        Some(item) => Ok(vec![item.clone()]),
    }
}

fn required_span_field(fields: &HashMap<MapKey, RuntimeValue>) -> Result<SourceSpan, String> {
    let RuntimeValue::Map(span) = fields
        .get(&MapKey::Str(Rc::from("span")))
        .ok_or_else(|| "surface form missing span field".to_string())?
    else {
        return Err("surface form span field must be a map".to_string());
    };
    let span = span.borrow();
    let start = required_usize_field(&span, "start")?;
    let end = required_usize_field(&span, "end")?;
    let start_line = required_usize_field(&span, "start_line")?;
    let start_col = required_usize_field(&span, "start_col")?;
    let end_line = required_usize_field(&span, "end_line")?;
    let end_col = required_usize_field(&span, "end_col")?;
    SourceSpan::new(start, end, start_line, start_col, end_line, end_col)
}

fn required_usize_field(
    fields: &HashMap<MapKey, RuntimeValue>,
    key: &str,
) -> Result<usize, String> {
    match fields.get(&MapKey::Str(Rc::from(key))) {
        Some(RuntimeValue::Int(value)) if *value >= 0 => Ok(*value as usize),
        _ => Err(format!(
            "surface span missing non-negative integer field {key:?}"
        )),
    }
}

pub struct SurfaceBuiltinSemanticRuntime<'a> {
    source: &'a str,
    source_path: Option<String>,
    hooks: HashMap<String, RuntimeValue>,
    evaluator: RefCell<Evaluator>,
    error: RefCell<Option<String>>,
    hook_calls: Cell<usize>,
}

impl<'a> SurfaceBuiltinSemanticRuntime<'a> {
    pub fn new(source: &'a str, source_path: Option<String>) -> Self {
        let mut evaluator = Evaluator::new(crate::graph::IRGraph::new());
        register_surface_hook_overrides(&mut evaluator);
        Self {
            source,
            source_path,
            hooks: HashMap::new(),
            evaluator: RefCell::new(evaluator),
            error: RefCell::new(None),
            hook_calls: Cell::new(0),
        }
    }

    pub fn with_hooks(mut self, hooks: HashMap<String, RuntimeValue>) -> Self {
        self.hooks = hooks;
        self
    }

    pub fn error(&self) -> Option<String> {
        self.error.borrow().clone()
    }

    fn set_error(&self, error: impl Into<String>) {
        let mut slot = self.error.borrow_mut();
        if slot.is_none() {
            *slot = Some(error.into());
        }
    }

    fn invoke_builtin_surface_hook(
        &self,
        name: &str,
        value: ParseValue,
        context: &SemanticContext<'_>,
    ) -> Result<ParseValue, String> {
        let runtime_value = if value.named_bindings().is_empty() {
            parse_value_to_runtime_value(&value)?
        } else {
            RuntimeValue::Map(Rc::new(RefCell::new(named_parse_bindings_to_runtime_map(
                &value,
            )?)))
        };
        let span = self
            .span_from_context(context)
            .ok_or_else(|| format!("surface semantic action {name:?} requires a local span"))?;
        let matched = if context.matched_text.is_empty() {
            runtime_value.to_string()
        } else {
            context.matched_text.to_string()
        };
        let result = match name {
            "surface.symbol" => {
                crate::builtins::surface::form_symbol(runtime_value.to_string(), span)
            }
            "surface.integer" => {
                let value = match runtime_value {
                    RuntimeValue::Int(value) => value,
                    RuntimeValue::Str(text) => text
                        .parse::<i64>()
                        .map_err(|error| format!("surface.integer parse failed: {error}"))?,
                    other => other
                        .to_string()
                        .parse::<i64>()
                        .map_err(|error| format!("surface.integer parse failed: {error}"))?,
                };
                crate::builtins::surface::form_integer(value, span)
            }
            "surface.string" => {
                let raw = match runtime_value {
                    RuntimeValue::Str(text) => text.to_string(),
                    other => other.to_string(),
                };
                let decoded = serde_json::from_str::<String>(&raw)
                    .map_err(|error| format!("surface.string decode failed: {error}"))?;
                crate::builtins::surface::form_atom(
                    "string",
                    RuntimeValue::Str(Rc::from(decoded.as_str())),
                    span,
                    raw,
                    "string",
                    Vec::new(),
                    None,
                    "paren",
                )
            }
            "surface.keyword-string" => {
                let keyword = context
                    .args
                    .first()
                    .map(grammar_scalar_to_string)
                    .unwrap_or(matched);
                crate::builtins::surface::form_atom(
                    "string",
                    RuntimeValue::Str(Rc::from(keyword.as_str())),
                    span,
                    keyword,
                    "keyword-string",
                    Vec::new(),
                    None,
                    "paren",
                )
            }
            "surface.boolean" => crate::builtins::surface::form_atom(
                "boolean",
                RuntimeValue::Bool(matched == "true"),
                span,
                matched,
                "boolean",
                Vec::new(),
                None,
                "paren",
            ),
            "surface.null" => crate::builtins::surface::form_null(span),
            "surface.list" => {
                let delimiter = context
                    .args
                    .first()
                    .map(grammar_scalar_to_string)
                    .unwrap_or_else(|| "paren".to_string());
                let items = surface_named_items(&runtime_value, "items")?;
                crate::builtins::surface::form_list(items, span, &delimiter)
            }
            _ => {
                let Some(hook) = self.hooks.get(name) else {
                    self.set_error(format!("unknown surface semantic action {name:?}"));
                    return Ok(value);
                };
                return self.invoke_custom_hook(name, hook, runtime_value, span);
            }
        };
        runtime_value_to_parse_value(&result)
    }

    fn invoke_custom_hook(
        &self,
        name: &str,
        hook: &RuntimeValue,
        value: RuntimeValue,
        span: SourceSpan,
    ) -> Result<ParseValue, String> {
        self.trace_hook_call(name, &value, &span);
        let value_summary = runtime_value_summary(&value, 0);
        let result = self
            .evaluator
            .borrow_mut()
            .invoke_callback(
                hook,
                vec![value, crate::builtins::surface::span_to_value(&span)],
            )
            .map_err(|error| {
                let previous = self
                    .error()
                    .map(|error| format!("; previous semantic error: {error}"))
                    .unwrap_or_default();
                format!(
                    "surface semantic hook {name:?} failed for {value_summary}: {error}{previous}"
                )
            })?;
        runtime_value_to_parse_value(&result)
    }

    fn trace_hook_call(&self, name: &str, value: &RuntimeValue, span: &SourceSpan) {
        if !should_trace_surface_hooks() {
            return;
        }
        let call = self.hook_calls.get().saturating_add(1);
        self.hook_calls.set(call);
        if call <= 32 || call.is_multiple_of(100) || call >= 4000 {
            eprintln!(
                "[caap-trace] surface-hook.call count={call} hook={name} span={}..{}@{}:{} value={}",
                span.start,
                span.end,
                span.start_line,
                span.start_col,
                runtime_value_summary(value, 0)
            );
        }
    }

    fn span_from_context(&self, context: &SemanticContext<'_>) -> Option<SourceSpan> {
        let (start, end) = context.span?;
        let (start_line, start_col) = line_col_for_offset(self.source, start);
        let (end_line, end_col) = line_col_for_offset(self.source, end);
        SourceSpan::with_locator(
            None,
            start,
            end,
            self.source_path.clone(),
            start_line,
            start_col,
            end_line,
            end_col,
        )
        .ok()
    }
}

fn runtime_value_summary(value: &RuntimeValue, depth: usize) -> String {
    if depth >= 3 {
        return "...".to_string();
    }
    match value {
        RuntimeValue::Null => "null".to_string(),
        RuntimeValue::Bool(value) => format!("bool({value})"),
        RuntimeValue::Int(value) => format!("int({value})"),
        RuntimeValue::Float(value) => format!("float({value})"),
        RuntimeValue::Str(value) => {
            let text = value.as_ref();
            let preview: String = text.chars().take(24).collect();
            if text.chars().count() > 24 {
                format!("str({preview}...)")
            } else {
                format!("str({preview})")
            }
        }
        RuntimeValue::Tuple(items) => format!("tuple(len={})", items.len()),
        RuntimeValue::List(items) => format!("list(len={})", items.borrow().len()),
        RuntimeValue::Map(fields) => {
            let fields = fields.borrow();
            let mut keys = fields
                .keys()
                .take(8)
                .map(|key| key.to_string())
                .collect::<Vec<_>>();
            keys.sort();
            let detail = if let Some(item) = fields.get(&MapKey::Str(Rc::from("item"))) {
                format!(", item={}", runtime_value_summary(item, depth + 1))
            } else if let Some(params) = fields.get(&MapKey::Str(Rc::from("params"))) {
                format!(", params={}", runtime_value_summary(params, depth + 1))
            } else {
                String::new()
            };
            format!(
                "map(len={}, keys=[{}]{detail})",
                fields.len(),
                keys.join(", ")
            )
        }
        RuntimeValue::Closure(_) => "closure".to_string(),
        RuntimeValue::Builtin(value) => format!("builtin({})", value.name),
        RuntimeValue::HostFunction(value) => format!("host-function({})", value.name),
        RuntimeValue::HostObject(value) => format!("host-object({})", value.type_name()),
        RuntimeValue::UninitializedTopLevel => "uninitialized".to_string(),
    }
}

fn register_surface_hook_overrides(evaluator: &mut Evaluator) {
    evaluator.register_builtin(BuiltinInfo {
        name: "value-is-map".to_string(),
        metadata: crate::values::BuiltinMetadata::eager_runtime(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            Ok(RuntimeValue::Bool(match &args[0] {
                RuntimeValue::Map(fields) => !is_surface_form_map(&fields.borrow()),
                _ => false,
            }))
        }),
    });
}

fn is_surface_form_map(fields: &HashMap<MapKey, RuntimeValue>) -> bool {
    let Some(RuntimeValue::Str(kind)) = fields.get(&MapKey::Str("kind".into())) else {
        return false;
    };
    matches!(
        kind.as_ref(),
        "list" | "symbol" | "integer" | "string" | "boolean" | "null"
    )
}

fn should_trace_surface_hooks() -> bool {
    if std::env::var_os("CAAP_RUST_LIVE_TRACE").is_none() {
        return false;
    }
    let Some(filter) = std::env::var("CAAP_RUST_LIVE_TRACE_FILTER")
        .ok()
        .filter(|filter| !filter.trim().is_empty())
    else {
        return true;
    };
    filter
        .split(',')
        .map(str::trim)
        .any(|needle| matches!(needle, "surface-hook" | "surface" | "hook"))
}

impl SemanticRuntime for SurfaceBuiltinSemanticRuntime<'_> {
    fn invoke_action(
        &self,
        _name: &str,
        value: ParseValue,
        _span: Option<(usize, usize)>,
        _named: &HashMap<String, ParseValue>,
    ) -> ParseValue {
        value
    }

    fn invoke_predicate(
        &self,
        _name: &str,
        _value: &ParseValue,
        _span: Option<(usize, usize)>,
        _named: &HashMap<String, ParseValue>,
    ) -> bool {
        true
    }

    fn invoke_action_with_context(
        &self,
        name: &str,
        value: ParseValue,
        context: &SemanticContext<'_>,
    ) -> ParseValue {
        match self.invoke_builtin_surface_hook(name, value.clone(), context) {
            Ok(value) => value,
            Err(error) => {
                self.set_error(error);
                value
            }
        }
    }
}

fn surface_named_items(value: &RuntimeValue, key: &str) -> Result<Vec<RuntimeValue>, String> {
    let RuntimeValue::Map(fields) = value else {
        return Ok(Vec::new());
    };
    let Some(items) = fields.borrow().get(&MapKey::Str(Rc::from(key))).cloned() else {
        return Ok(Vec::new());
    };
    match items {
        RuntimeValue::Null => Ok(Vec::new()),
        RuntimeValue::Tuple(items) => Ok(items.iter().cloned().collect()),
        RuntimeValue::List(items) => Ok(items.borrow().clone()),
        item => Ok(vec![item]),
    }
}

fn grammar_scalar_to_string(value: &GrammarScalar) -> String {
    match value {
        GrammarScalar::Str(value) => value.clone(),
        GrammarScalar::Int(value) => value.to_string(),
        GrammarScalar::Float(value) => value.to_string(),
        GrammarScalar::Bool(value) => value.to_string(),
        GrammarScalar::Null => "null".to_string(),
    }
}

fn line_col_for_offset(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (index, ch) in source.char_indices() {
        if index >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn semantic_list_to_json(items: &[SemanticValue]) -> Result<Value, String> {
    let Some(SemanticValue::Str(tag)) = items.first() else {
        return items
            .iter()
            .map(semantic_value_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array);
    };
    match tag.as_str() {
        "seq" | "choice" => {
            let children = items[1..]
                .iter()
                .map(semantic_value_to_json)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!([tag, children]))
        }
        _ => items
            .iter()
            .map(semantic_value_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
    }
}

fn base_surface_rules() -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("forms".to_string(), json!(["many", ["ref", "form"]])),
        (
            "form".to_string(),
            json!([
                "choice",
                [
                    ["ref", "list"],
                    ["ref", "string"],
                    ["ref", "integer"],
                    ["ref", "boolean"],
                    ["ref", "null"],
                    ["ref", "symbol"]
                ]
            ]),
        ),
        (
            "list".to_string(),
            json!([
                "seq",
                [
                    ["literal", "("],
                    ["named", "items", ["many", ["ref", "form"]]],
                    ["literal", ")"]
                ]
            ]),
        ),
        (
            "string".to_string(),
            json!(["regex", r#""(?:[^"\\]|\\.)*""#]),
        ),
        (
            "integer".to_string(),
            json!(["regex", r#"-?(?:0|[1-9][0-9]*)"#]),
        ),
        (
            "boolean".to_string(),
            json!(["choice", [["literal", "true"], ["literal", "false"]]]),
        ),
        ("null".to_string(), json!(["literal", "null"])),
        (
            "symbol".to_string(),
            json!([
                "regex",
                r#"[A-Za-z_+\-*\/<>=!?$%&:.][A-Za-z0-9_+\-*\/<>=!?$%&:.]*"#
            ]),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use caap_peg_port::{PEGParser, ParseValue, ParserConfig};

    use super::*;
    use crate::syntax_authoring::apply_authoring_grammar_source;

    #[test]
    fn compiles_base_surface_grammar_from_empty_syntax_state() {
        let syntax = UnitSyntaxState::new("caap").unwrap();
        let grammar = compile_surface_grammar_from_syntax_state(&syntax).unwrap();

        assert_eq!(grammar.start_rule, "forms");
        assert!(grammar.get_rule("form").is_some());
        assert!(grammar.get_rule("integer").is_some());
    }

    #[test]
    fn syntax_state_rules_override_base_rules_and_compile_many_literal_aliases() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(
            &mut syntax,
            r#"
add rule bracket = "[" items:integer* "]"
replace rule form = bracket | integer
"#,
        )
        .unwrap();

        let grammar = compile_surface_grammar_from_syntax_state(&syntax).unwrap();
        assert!(grammar.get_rule("bracket").is_some());

        let parser = PEGParser;
        let parsed = parser
            .parse(&grammar, "[1 2 3]", &ParserConfig::default())
            .unwrap();
        assert!(!matches!(parsed, ParseValue::Nil));
    }

    #[test]
    fn carries_rule_and_grammar_metadata() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(&mut syntax, r#"add rule demo = symbol -> surface.symbol"#)
            .unwrap();
        syntax
            .set_grammar_metadata(
                "semantic_hook_functions",
                SemanticValue::Map(vec![(
                    "surface.symbol".to_string(),
                    SemanticValue::Str("surface-symbol".to_string()),
                )]),
            )
            .unwrap();

        let grammar = compile_surface_grammar_from_syntax_state(&syntax).unwrap();
        assert!(grammar
            .metadata
            .get("demo")
            .and_then(|metadata| metadata.get("semantic_hooks"))
            .is_some());
        assert!(grammar
            .metadata
            .get("__grammar__")
            .and_then(|metadata| metadata.get("semantic_hook_functions"))
            .is_some());
    }

    #[test]
    fn runtime_parse_value_bridge_roundtrips_structured_values() {
        let value = RuntimeValue::Map(Rc::new(RefCell::new(HashMap::from([
            (
                MapKey::Str(Rc::from("name")),
                RuntimeValue::Str(Rc::from("demo")),
            ),
            (
                MapKey::Str(Rc::from("items")),
                RuntimeValue::List(Rc::new(RefCell::new(vec![
                    RuntimeValue::Int(1),
                    RuntimeValue::Bool(true),
                ]))),
            ),
        ]))));

        let encoded = runtime_value_to_parse_value(&value).unwrap();
        let decoded = parse_value_to_runtime_value(&encoded).unwrap();

        let RuntimeValue::Map(fields) = decoded else {
            panic!("expected decoded map");
        };
        assert_eq!(
            fields.borrow().get(&MapKey::Str(Rc::from("name"))),
            Some(&RuntimeValue::Str(Rc::from("demo")))
        );
        assert!(matches!(
            fields.borrow().get(&MapKey::Str(Rc::from("items"))),
            Some(RuntimeValue::List(_))
        ));
    }

    #[test]
    fn named_parse_bindings_project_to_runtime_map() {
        let value = ParseValue::Node(
            "pair".to_string(),
            vec![
                ParseValue::Named("left".to_string(), Box::new(ParseValue::Text("x".into()))),
                ParseValue::Named("right".to_string(), Box::new(ParseValue::Number(42))),
            ],
        );

        let fields = named_parse_bindings_to_runtime_map(&value).unwrap();
        assert_eq!(
            fields.get(&MapKey::Str(Rc::from("left"))),
            Some(&RuntimeValue::Str(Rc::from("x")))
        );
        assert_eq!(
            fields.get(&MapKey::Str(Rc::from("right"))),
            Some(&RuntimeValue::Int(42))
        );
    }

    #[test]
    fn builtin_surface_runtime_lowers_integer_transform() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(
            &mut syntax,
            r#"replace rule form = integer -> surface.integer"#,
        )
        .unwrap();
        let grammar = compile_surface_grammar_from_syntax_state(&syntax)
            .unwrap()
            .with_start_rule("form");
        let runtime = SurfaceBuiltinSemanticRuntime::new("42", None);

        let parsed = PEGParser
            .parse_with_semantic(&grammar, "42", &ParserConfig::default(), Some(&runtime))
            .unwrap();
        assert_eq!(runtime.error(), None);
        let RuntimeValue::Map(fields) = parse_value_to_runtime_value(&parsed).unwrap() else {
            panic!("expected surface form map");
        };
        assert_eq!(
            fields.borrow().get(&MapKey::Str(Rc::from("kind"))),
            Some(&RuntimeValue::Str(Rc::from("integer")))
        );
        assert_eq!(
            fields.borrow().get(&MapKey::Str(Rc::from("value"))),
            Some(&RuntimeValue::Int(42))
        );
    }

    #[test]
    fn builtin_surface_runtime_uses_named_list_items() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(
            &mut syntax,
            r#"replace rule form = list -> surface.list | integer -> surface.integer"#,
        )
        .unwrap();
        let grammar = compile_surface_grammar_from_syntax_state(&syntax)
            .unwrap()
            .with_start_rule("form");
        let runtime = SurfaceBuiltinSemanticRuntime::new("(1)", None);

        let parsed = PEGParser
            .parse_with_semantic(&grammar, "(1)", &ParserConfig::default(), Some(&runtime))
            .unwrap();
        assert_eq!(runtime.error(), None);
        let RuntimeValue::Map(fields) = parse_value_to_runtime_value(&parsed).unwrap() else {
            panic!("expected surface form map");
        };
        assert_eq!(
            fields.borrow().get(&MapKey::Str(Rc::from("kind"))),
            Some(&RuntimeValue::Str(Rc::from("list")))
        );
        assert!(matches!(
            fields.borrow().get(&MapKey::Str(Rc::from("items"))),
            Some(RuntimeValue::Tuple(items)) if items.len() == 1
        ));
    }

    #[test]
    fn parse_value_to_parsed_source_decodes_surface_forms() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(
            &mut syntax,
            r#"replace rule form = list -> surface.list | integer -> surface.integer"#,
        )
        .unwrap();
        let grammar = compile_surface_grammar_from_syntax_state(&syntax).unwrap();
        let runtime = SurfaceBuiltinSemanticRuntime::new("(1 2)", None);
        let parsed = PEGParser
            .parse_with_semantic(&grammar, "(1 2)", &ParserConfig::default(), Some(&runtime))
            .unwrap();

        let source = parse_value_to_parsed_source(&parsed).unwrap();
        assert_eq!(source.forms.len(), 1);
        let ParsedForm::List { items, .. } = &source.forms[0] else {
            panic!("expected list form");
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], ParsedForm::Integer { value: 1, .. }));
        assert!(matches!(&items[1], ParsedForm::Integer { value: 2, .. }));
    }

    #[test]
    fn parses_c_like_empty_map_value_declaration_prefix() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(
            &mut syntax,
            r#"
add rule c_ident = /[A-Za-z_][A-Za-z0-9_.-]*/
add rule c_expr = c_cond_expr
add rule c_cond_expr = c_or_expr
add rule c_or_expr = c_and_expr
add rule c_and_expr = c_eq_expr
add rule c_eq_expr = c_cmp_expr
add rule c_cmp_expr = c_add_expr
add rule c_add_expr = c_unary_expr
add rule c_unary_expr = c_primary_expr
add rule c_primary_expr = c_map_expr | c_ident_expr
add rule c_ident_expr = name:c_ident
add rule c_map_expr = "{" pairs:c_map_pair_list? "}"
add rule c_map_pair = key:c_ident ":" value:c_expr
add rule c_map_pair_tail = "," pair:c_map_pair
add rule c_map_pair_list = first:c_map_pair rest:c_map_pair_tail*
add rule c_value_form = "auto" name:c_ident "=" value:c_expr ";"
replace rule form = c_value_form
"#,
        )
        .unwrap();
        let grammar = compile_surface_grammar_from_syntax_state(&syntax).unwrap();
        PEGParser
            .parse(
                &grammar,
                "auto seen_diagnostics = {};",
                &ParserConfig::default().with_max_steps(4096),
            )
            .unwrap();
    }
}
