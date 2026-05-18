/// CTFE IR construction builtins — Rust port of
/// `caap/builtins/graph/graph_ir/builders.py`.
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::eval::{eval_args, Evaluator};
use crate::ir::{ExprSpec, IrLiteralData};
use crate::source::SourceSpan;
use crate::values::{eval_err, BuiltinInfo, EvalSignal, HostObject, MapKey, RuntimeValue};

#[derive(Clone, Debug)]
pub struct ExprSpecBridgeValue {
    spec: RefCell<ExprSpec>,
}

impl ExprSpecBridgeValue {
    pub fn new(spec: ExprSpec) -> Self {
        Self {
            spec: RefCell::new(spec),
        }
    }

    pub fn spec(&self) -> ExprSpec {
        self.spec.borrow().clone()
    }

    pub fn clone_spec(&self) -> ExprSpec {
        self.spec()
    }

    pub fn source_span(&self) -> Option<SourceSpan> {
        self.spec.borrow().span().cloned()
    }

    pub fn set_source_span(&self, span: Option<SourceSpan>) {
        set_expr_spec_source_span(&mut self.spec.borrow_mut(), span);
    }
}

impl HostObject for ExprSpecBridgeValue {
    fn type_name(&self) -> &'static str {
        "expr_spec"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn register(ev: &mut Evaluator) {
    ev.register_builtin(BuiltinInfo {
        name: "ctfe-ir-instantiate".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let kind = require_named_string(&args[0], "ctfe-ir-instantiate expects a string kind")?;
            let payload = require_map(&args[1], "ctfe-ir-instantiate expects a payload map")?;
            let span = metadata_source_span(args.get(2))?;
            let mut spec = instantiate_ir(kind.as_str(), &payload.borrow())?;
            if span.is_some() {
                set_expr_spec_source_span(&mut spec, span);
            }
            Ok(RuntimeValue::HostObject(Rc::new(ExprSpecBridgeValue::new(
                spec,
            ))))
        }),
    });
}

fn instantiate_ir(
    kind: &str,
    payload: &HashMap<MapKey, RuntimeValue>,
) -> Result<ExprSpec, EvalSignal> {
    match kind {
        "name" => instantiate_name(payload),
        "literal" => instantiate_literal(payload),
        "call" => instantiate_call(payload),
        "lambda" => instantiate_lambda(payload),
        "bind" => instantiate_bind(payload),
        "do" => instantiate_do(payload),
        "if" => instantiate_if(payload),
        "block" => instantiate_block(payload),
        "leave" => instantiate_leave(payload),
        _ => Err(eval_err(format!(
            "ctfe-ir-instantiate does not support IR kind {kind:?}"
        ))),
    }
}

fn instantiate_name(payload: &HashMap<MapKey, RuntimeValue>) -> Result<ExprSpec, EvalSignal> {
    let identifier = require_named_string(
        payload_required(payload, "identifier")?,
        "ctfe-ir-instantiate Name payload expects a non-empty string identifier",
    )?;
    ExprSpec::name(identifier).map_err(eval_err)
}

fn instantiate_literal(payload: &HashMap<MapKey, RuntimeValue>) -> Result<ExprSpec, EvalSignal> {
    Ok(ExprSpec::literal(runtime_to_literal(payload_required(
        payload, "value",
    )?)?))
}

fn instantiate_call(payload: &HashMap<MapKey, RuntimeValue>) -> Result<ExprSpec, EvalSignal> {
    let callee = coerce_expr_like(
        payload_required(payload, "callee")?,
        "ctfe-ir-instantiate Call payload expects an expression spec callee",
    )?;
    let args = match payload_optional(payload, "args") {
        Some(value) => coerce_payload_expr_sequence(
            value,
            "ctfe-ir-instantiate Call payload expects expression specs in args",
        )?,
        None => Vec::new(),
    };
    Ok(ExprSpec::call(callee, args))
}

fn instantiate_lambda(payload: &HashMap<MapKey, RuntimeValue>) -> Result<ExprSpec, EvalSignal> {
    let params = string_sequence(
        payload_required(payload, "params")?,
        "ctfe-ir-instantiate lambda payload expects parameter names",
    )?;
    let body = coerce_expr_like(
        payload_required(payload, "body")?,
        "ctfe-ir-instantiate lambda payload expects an expression spec body",
    )?;
    call_language_form(
        "lambda",
        vec![
            ExprSpec::literal(IrLiteralData::Tuple(
                params.into_iter().map(IrLiteralData::Str).collect(),
            )),
            body,
        ],
    )
}

fn instantiate_bind(payload: &HashMap<MapKey, RuntimeValue>) -> Result<ExprSpec, EvalSignal> {
    let bindings = sequence(
        payload_required(payload, "bindings")?,
        "ctfe-ir-instantiate bind payload expects binding maps",
    )?;
    let body = coerce_expr_like(
        payload_required(payload, "body")?,
        "ctfe-ir-instantiate bind payload expects an expression spec body",
    )?;
    let mut args = Vec::new();
    for binding in bindings {
        let binding = require_map(
            &binding,
            "ctfe-ir-instantiate bind payload expects binding maps",
        )?;
        let binding = binding.borrow();
        let name = require_named_string(
            payload_required(&binding, "name")?,
            "ctfe-ir-instantiate bind payload expects binding names",
        )?;
        let value = coerce_expr_like(
            payload_required(&binding, "value")?,
            "ctfe-ir-instantiate bind payload expects expression specs in binding values",
        )?;
        args.push(ExprSpec::literal(IrLiteralData::Str(name)));
        args.push(value);
    }
    args.push(body);
    call_language_form("bind", args)
}

fn instantiate_do(payload: &HashMap<MapKey, RuntimeValue>) -> Result<ExprSpec, EvalSignal> {
    let forms = match payload_optional(payload, "forms") {
        Some(value) => coerce_payload_expr_sequence(
            value,
            "ctfe-ir-instantiate do payload expects expression specs in forms",
        )?,
        None => Vec::new(),
    };
    call_language_form("do", forms)
}

fn instantiate_if(payload: &HashMap<MapKey, RuntimeValue>) -> Result<ExprSpec, EvalSignal> {
    let condition = coerce_expr_like(
        payload_required(payload, "condition")?,
        "ctfe-ir-instantiate if payload expects an expression spec condition",
    )?;
    let then_branch = optional_expr(
        payload_optional(payload, "then"),
        "ctfe-ir-instantiate if payload expects an expression spec then-branch",
    )?
    .unwrap_or_else(null_literal);
    let mut args = vec![condition, then_branch];
    if let Some(else_branch) = optional_expr(
        payload_optional(payload, "else"),
        "ctfe-ir-instantiate if payload expects an expression spec else-branch",
    )? {
        args.push(else_branch);
    }
    call_language_form("if", args)
}

fn instantiate_block(payload: &HashMap<MapKey, RuntimeValue>) -> Result<ExprSpec, EvalSignal> {
    let label = optional_string(
        payload_optional(payload, "label"),
        "ctfe-ir-instantiate block payload expects a string label or null",
    )?;
    let body = coerce_expr_like(
        payload_required(payload, "body")?,
        "ctfe-ir-instantiate block payload expects an expression spec body",
    )?;
    call_language_form(
        "block",
        vec![ExprSpec::literal(optional_string_literal(label)), body],
    )
}

fn instantiate_leave(payload: &HashMap<MapKey, RuntimeValue>) -> Result<ExprSpec, EvalSignal> {
    let target = optional_string(
        payload_optional(payload, "target"),
        "ctfe-ir-instantiate leave payload expects a string target or null",
    )?;
    let value = optional_expr(
        payload_optional(payload, "value"),
        "ctfe-ir-instantiate leave payload expects an expression spec value",
    )?
    .unwrap_or_else(null_literal);
    call_language_form(
        "leave",
        vec![ExprSpec::literal(optional_string_literal(target)), value],
    )
}

fn call_language_form(name: &str, args: Vec<ExprSpec>) -> Result<ExprSpec, EvalSignal> {
    Ok(ExprSpec::call(
        ExprSpec::name(name).map_err(eval_err)?,
        args,
    ))
}

fn null_literal() -> ExprSpec {
    ExprSpec::literal(IrLiteralData::Null)
}

fn optional_string_literal(value: Option<String>) -> IrLiteralData {
    value.map(IrLiteralData::Str).unwrap_or(IrLiteralData::Null)
}

fn coerce_expr_like(value: &RuntimeValue, message: &str) -> Result<ExprSpec, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    object
        .as_any()
        .downcast_ref::<ExprSpecBridgeValue>()
        .map(ExprSpecBridgeValue::clone_spec)
        .ok_or_else(|| eval_err(message))
}

fn optional_expr(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<ExprSpec>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(value) => coerce_expr_like(value, message).map(Some),
    }
}

fn coerce_payload_expr_sequence(
    value: &RuntimeValue,
    message: &str,
) -> Result<Vec<ExprSpec>, EvalSignal> {
    sequence(value, message)?
        .iter()
        .map(|item| coerce_expr_like(item, message))
        .collect()
}

fn metadata_source_span(value: Option<&RuntimeValue>) -> Result<Option<SourceSpan>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(RuntimeValue::Map(map)) => {
            let map = map.borrow();
            match map.get(&str_key("source_span")) {
                None | Some(RuntimeValue::Null) => Ok(None),
                Some(value) => crate::builtins::surface::require_span_map(
                    value,
                    "ctfe-ir-instantiate metadata source_span expects SourceSpan",
                )
                .map(Some),
            }
        }
        Some(_) => Err(eval_err("ctfe-ir-instantiate metadata expects a map")),
    }
}

pub(crate) fn set_expr_spec_source_span(spec: &mut ExprSpec, span: Option<SourceSpan>) {
    match spec {
        ExprSpec::Name(name) => name.span = span,
        ExprSpec::Literal(literal) => literal.span = span,
        ExprSpec::Call(call) => call.span = span,
    }
}

fn runtime_to_literal(value: &RuntimeValue) -> Result<IrLiteralData, EvalSignal> {
    match value {
        RuntimeValue::Null => Ok(IrLiteralData::Null),
        RuntimeValue::Bool(value) => Ok(IrLiteralData::Bool(*value)),
        RuntimeValue::Int(value) => Ok(IrLiteralData::Int(*value)),
        RuntimeValue::Float(value) => Ok(IrLiteralData::Float(*value)),
        RuntimeValue::Str(value) => Ok(IrLiteralData::Str(value.to_string())),
        RuntimeValue::Tuple(items) => items
            .iter()
            .map(runtime_to_literal)
            .collect::<Result<Vec<_>, _>>()
            .map(IrLiteralData::Tuple),
        RuntimeValue::List(items) => items
            .borrow()
            .iter()
            .map(runtime_to_literal)
            .collect::<Result<Vec<_>, _>>()
            .map(IrLiteralData::Tuple),
        RuntimeValue::Map(map) => {
            let entries = map
                .borrow()
                .iter()
                .map(|(key, value)| {
                    let MapKey::Str(key) = key else {
                        return Err(eval_err(
                            "ctfe-ir-instantiate literal map keys must be strings",
                        ));
                    };
                    Ok((key.to_string(), runtime_to_literal(value)?))
                })
                .collect::<Result<Vec<_>, _>>()?;
            IrLiteralData::dict(entries).map_err(eval_err)
        }
        RuntimeValue::Closure(_)
        | RuntimeValue::Builtin(_)
        | RuntimeValue::HostFunction(_)
        | RuntimeValue::HostObject(_)
        | RuntimeValue::UninitializedTopLevel => Err(eval_err(
            "ctfe-ir-instantiate literal payload value is not liftable into IR",
        )),
    }
}

fn payload_required<'a>(
    payload: &'a HashMap<MapKey, RuntimeValue>,
    key: &str,
) -> Result<&'a RuntimeValue, EvalSignal> {
    payload.get(&str_key(key)).ok_or_else(|| {
        eval_err(format!(
            "ctfe-ir-instantiate payload for {key:?} is required"
        ))
    })
}

fn payload_optional<'a>(
    payload: &'a HashMap<MapKey, RuntimeValue>,
    key: &str,
) -> Option<&'a RuntimeValue> {
    payload.get(&str_key(key))
}

fn optional_string(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<String>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(value) => require_named_string(value, message).map(Some),
    }
}

fn string_sequence(value: &RuntimeValue, message: &str) -> Result<Vec<String>, EvalSignal> {
    sequence(value, message)?
        .iter()
        .map(|item| require_named_string(item, message))
        .collect()
}

fn sequence(value: &RuntimeValue, message: &str) -> Result<Vec<RuntimeValue>, EvalSignal> {
    match value {
        RuntimeValue::Tuple(items) => Ok(items.iter().cloned().collect()),
        RuntimeValue::List(items) => Ok(items.borrow().iter().cloned().collect()),
        _ => Err(eval_err(message)),
    }
}

fn require_map(
    value: &RuntimeValue,
    message: &str,
) -> Result<Rc<RefCell<HashMap<MapKey, RuntimeValue>>>, EvalSignal> {
    match value {
        RuntimeValue::Map(map) => Ok(Rc::clone(map)),
        _ => Err(eval_err(message)),
    }
}

fn require_named_string(value: &RuntimeValue, message: &str) -> Result<String, EvalSignal> {
    let RuntimeValue::Str(text) = value else {
        return Err(eval_err(message));
    };
    if text.is_empty() {
        return Err(eval_err(message));
    }
    Ok(text.to_string())
}

fn str_key(key: &str) -> MapKey {
    MapKey::Str(key.into())
}
