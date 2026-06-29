/// CTFE IR construction builtins.
use indexmap::IndexMap;
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use crate::eval::{eval_args, Evaluator};
use crate::ir::{ExprSpec, IrLiteralData};
use crate::source::SourceSpan;
use crate::values::{eval_err, EvalSignal, HostObject, MapKey, RuntimeValue};

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
    // Per-kind IR-node constructors. The node kind is the *builtin name* rather
    // than a string selector argument, so a misspelled kind is an unknown-builtin
    // error (not a runtime "unsupported IR kind") and each kind is classified
    // individually — mirroring the per-severity `ctfe_provider_diagnostics_*`
    // builtins. Each takes `(payload_map [metadata])`; the payload is the same map
    // the string-selector form took.
    ev.register_special(
        "ctfe_ir_name",
        1,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            make_ir_spec("name", &args[0], args.get(1))
        },
    );
    ev.register_special(
        "ctfe_ir_literal",
        1,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            make_ir_spec("literal", &args[0], args.get(1))
        },
    );
    ev.register_special(
        "ctfe_ir_call",
        1,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            make_ir_spec("call", &args[0], args.get(1))
        },
    );
    // ctfe-spec-span: the optional source location of a DETACHED spec — the
    // mirror of ctfe_unit_node_span for values that left the graph
    // (ctfe_node_to_spec preserves spans; built specs carry one only when the
    // metadata arg supplied source_span). Null when absent — never fabricated.
    ev.register_special(
        "ctfe_spec_span",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let spec = downcast_expr_spec(&args[0], "ctfe-spec-span")?;
            Ok(match spec.source_span() {
                Some(span) => crate::builtins::compiler_units_helpers_span_to_value(&span),
                None => RuntimeValue::Null,
            })
        },
    );

    // ctfe-spec-with-span: a copy of an IR spec with its ROOT span set from a
    // span map ({start, end, start_line, start_col, end_line, end_col[, path]}),
    // copied from a DONOR spec, or cleared with null. Synthesized nodes
    // (per-kind builders / data-built syntax) are span-less — this is the one
    // channel that gives them a location; child spans stay untouched.
    ev.register_special(
        "ctfe_spec_with_span",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let mut spec = downcast_expr_spec(&args[0], "ctfe-spec-with-span")?.spec();
            let span = match &args[1] {
                RuntimeValue::Null => None,
                RuntimeValue::Map(_) => Some(crate::builtins::surface::require_span_map(
                    &args[1],
                    "ctfe-spec-with-span expects a span map \
                     ({start, end, start_line, start_col, end_line, end_col[, path]})",
                )?),
                RuntimeValue::HostObject(_) => {
                    downcast_expr_spec(&args[1], "ctfe-spec-with-span donor")?.source_span()
                }
                _ => {
                    return Err(eval_err(
                        "ctfe-spec-with-span expects a span map, a donor spec, or null",
                    ))
                }
            };
            set_expr_spec_source_span(&mut spec, span);
            Ok(RuntimeValue::HostObject(Rc::new(ExprSpecBridgeValue::new(
                spec,
            ))))
        },
    );

    // ctfe-eval-node ir-node → value
    //   Evaluate a constructed IR node (from the per-kind builders) at compile
    //   time, in the current phase/environment, sharing the registered builtins.
    //   The metaprogramming closure: build IR, then run it.
    ev.register_special(
        "ctfe_eval_node",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let spec = downcast_expr_spec(&args[0], "ctfe-eval-node")?.spec();
            ev.eval_expr_spec(spec, env)
        },
    );
}

fn downcast_expr_spec<'a>(
    value: &'a RuntimeValue,
    context: &str,
) -> Result<&'a ExprSpecBridgeValue, EvalSignal> {
    let RuntimeValue::HostObject(obj) = value else {
        return Err(eval_err(format!(
            "{context}: expected an IR node (expr-spec)"
        )));
    };
    obj.as_any()
        .downcast_ref::<ExprSpecBridgeValue>()
        .ok_or_else(|| eval_err(format!("{context}: expected an IR node (expr-spec)")))
}

/// Build an `expr_spec` host value of `kind` from a payload map and optional
/// metadata. Shared by the per-kind builtins.
fn make_ir_spec(
    kind: &str,
    payload: &RuntimeValue,
    metadata: Option<&RuntimeValue>,
) -> Result<RuntimeValue, EvalSignal> {
    let payload = require_map(payload, "ctfe-ir builder expects a payload map")?;
    let span = metadata_source_span(metadata)?;
    let mut spec = instantiate_ir(kind, &payload.borrow())?;
    if span.is_some() {
        set_expr_spec_source_span(&mut spec, span);
    }
    Ok(RuntimeValue::HostObject(Rc::new(ExprSpecBridgeValue::new(
        spec,
    ))))
}

fn instantiate_ir(
    kind: &str,
    payload: &IndexMap<MapKey, RuntimeValue>,
) -> Result<ExprSpec, EvalSignal> {
    match kind {
        "name" => instantiate_name(payload),
        "literal" => instantiate_literal(payload),
        "call" => instantiate_call(payload),
        _ => Err(eval_err(format!(
            "ctfe-ir-instantiate does not support IR kind {kind:?}"
        ))),
    }
}

fn instantiate_name(payload: &IndexMap<MapKey, RuntimeValue>) -> Result<ExprSpec, EvalSignal> {
    let identifier = require_named_string(
        payload_required(payload, "identifier")?,
        "ctfe-ir Name payload expects a non-empty string identifier",
    )?;
    ExprSpec::name(identifier).map_err(eval_err)
}

fn instantiate_literal(payload: &IndexMap<MapKey, RuntimeValue>) -> Result<ExprSpec, EvalSignal> {
    Ok(ExprSpec::literal(runtime_to_literal(payload_required(
        payload, "value",
    )?)?))
}

fn instantiate_call(payload: &IndexMap<MapKey, RuntimeValue>) -> Result<ExprSpec, EvalSignal> {
    let callee = coerce_expr_like(
        payload_required(payload, "callee")?,
        "ctfe-ir Call payload expects an expression spec callee",
    )?;
    let args = match payload_optional(payload, "args") {
        Some(value) => coerce_payload_expr_sequence(
            value,
            "ctfe-ir Call payload expects expression specs in args",
        )?,
        None => Vec::new(),
    };
    Ok(ExprSpec::call(callee, args))
}

pub(crate) fn require_expr_spec(
    value: &RuntimeValue,
    message: &str,
) -> Result<ExprSpec, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    object
        .as_any()
        .downcast_ref::<ExprSpecBridgeValue>()
        .map(ExprSpecBridgeValue::clone_spec)
        .ok_or_else(|| eval_err(message))
}

fn coerce_expr_like(value: &RuntimeValue, message: &str) -> Result<ExprSpec, EvalSignal> {
    require_expr_spec(value, message)
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
                    "ctfe-ir metadata source_span expects SourceSpan",
                )
                .map(Some),
            }
        }
        Some(_) => Err(eval_err("ctfe-ir metadata expects a map")),
    }
}

pub(crate) fn set_expr_spec_source_span(spec: &mut ExprSpec, span: Option<SourceSpan>) {
    match spec {
        ExprSpec::Name(name) => name.span = span,
        ExprSpec::Literal(literal) => literal.span = span,
        ExprSpec::Call(call) => call.span = span,
    }
}

pub(crate) fn runtime_to_literal(value: &RuntimeValue) -> Result<IrLiteralData, EvalSignal> {
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
        RuntimeValue::Bytes(_)
        | RuntimeValue::Closure(_)
        | RuntimeValue::Macro(_)
        | RuntimeValue::Builtin(_)
        | RuntimeValue::HostFunction(_)
        | RuntimeValue::HostObject(_)
        | RuntimeValue::Ref(_)
        | RuntimeValue::UninitializedTopLevel => Err(eval_err(
            "ctfe-ir-instantiate literal payload value is not liftable into IR",
        )),
    }
}

fn payload_required<'a>(
    payload: &'a IndexMap<MapKey, RuntimeValue>,
    key: &str,
) -> Result<&'a RuntimeValue, EvalSignal> {
    payload.get(&str_key(key)).ok_or_else(|| {
        eval_err(format!(
            "ctfe-ir-instantiate payload for {key:?} is required"
        ))
    })
}

fn payload_optional<'a>(
    payload: &'a IndexMap<MapKey, RuntimeValue>,
    key: &str,
) -> Option<&'a RuntimeValue> {
    payload.get(&str_key(key))
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
) -> Result<Rc<RefCell<IndexMap<MapKey, RuntimeValue>>>, EvalSignal> {
    match value {
        RuntimeValue::Map(map) => Ok(Rc::clone(map)),
        _ => Err(eval_err(message)),
    }
}

use super::args::{require_named_string, str_key};
