/// Runtime syntax construction and inspection builtins.
///
/// These are the public syntax primitives used by runtime macros. They expose
/// the same detached `ExprSpec` representation as CTFE IR builders without
/// making macro authors depend on compile-time-only APIs.
use std::cell::RefCell;
use std::rc::Rc;

use crate::builtins::ir_builders::{require_expr_spec, runtime_to_literal, ExprSpecBridgeValue};
use crate::eval::Evaluator;
use crate::ir::ExprSpec;
use crate::values::{eval_err, runtime_value_from_literal, EvalSignal, RuntimeValue};

pub fn register(ev: &mut Evaluator) {
    // The optional trailing ORIGIN argument (a syntax/spec value) donates its
    // source span to the constructed node, so expander-generated trees keep
    // pointing at the user form they came from instead of carrying null spans.
    ev.register_eager(
        "syntax_name",
        1,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime(),
        |args| {
            let identifier = require_string(&args[0], "syntax-name expects a non-empty string")?;
            let span = origin_span(args.get(1), "syntax-name")?;
            Ok(syntax_value(
                ExprSpec::name_with_span(identifier, span).map_err(eval_err)?,
            ))
        },
    );

    ev.register_eager(
        "syntax_literal",
        1,
        Some(2),
        crate::values::BuiltinMetadata::eager_runtime(),
        |args| {
            let span = origin_span(args.get(1), "syntax-literal")?;
            Ok(syntax_value(ExprSpec::literal_with_span(
                runtime_to_literal(&args[0])?,
                span,
            )))
        },
    );

    // A constructed call inherits its span from the first spanned child
    // (callee, then args) unless an explicit origin overrides it — generated
    // trees stay located after expansion.
    ev.register_eager(
        "syntax_call",
        2,
        Some(3),
        crate::values::BuiltinMetadata::eager_runtime(),
        |args| {
            let callee = require_expr_spec(&args[0], "syntax-call expects syntax callee")?;
            let call_args =
                syntax_sequence(&args[1], "syntax-call expects a list/tuple of syntax args")?;
            let span = match origin_span(args.get(2), "syntax-call")? {
                Some(span) => Some(span),
                None => callee
                    .span()
                    .cloned()
                    .or_else(|| call_args.iter().find_map(|arg| arg.span().cloned())),
            };
            Ok(syntax_value(ExprSpec::call_with_span(
                callee, call_args, span,
            )))
        },
    );

    ev.register_eager(
        "syntax_kind",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime(),
        |args| {
            let spec = require_expr_spec(&args[0], "syntax-kind expects syntax")?;
            let kind = match spec {
                ExprSpec::Name(_) => "name",
                ExprSpec::Literal(_) => "literal",
                ExprSpec::Call(_) => "call",
            };
            Ok(RuntimeValue::Str(kind.into()))
        },
    );

    ev.register_eager(
        "syntax_name_identifier",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime(),
        |args| match require_expr_spec(&args[0], "syntax-name-identifier expects syntax")? {
            ExprSpec::Name(name) => Ok(RuntimeValue::Str(name.identifier.into())),
            _ => Err(eval_err("syntax-name-identifier expects name syntax")),
        },
    );

    ev.register_eager(
        "syntax_literal_value",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime(),
        |args| match require_expr_spec(&args[0], "syntax-literal-value expects syntax")? {
            ExprSpec::Literal(literal) => Ok(runtime_value_from_literal(&literal.value)),
            _ => Err(eval_err("syntax-literal-value expects literal syntax")),
        },
    );

    ev.register_eager(
        "syntax_call_callee",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime(),
        |args| match require_expr_spec(&args[0], "syntax-call-callee expects syntax")? {
            ExprSpec::Call(call) => Ok(syntax_value(*call.callee)),
            _ => Err(eval_err("syntax-call-callee expects call syntax")),
        },
    );

    ev.register_eager(
        "syntax_call_args",
        1,
        Some(1),
        crate::values::BuiltinMetadata::eager_runtime(),
        |args| match require_expr_spec(&args[0], "syntax-call-args expects syntax")? {
            ExprSpec::Call(call) => Ok(RuntimeValue::List(Rc::new(RefCell::new(
                call.args.into_iter().map(syntax_value).collect(),
            )))),
            _ => Err(eval_err("syntax-call-args expects call syntax")),
        },
    );

    // Source-text projections of the kernel frontend, for tool programs
    // (tools/ast_json.caap, tools/canonicalize.caap): parse CAAP source text
    // into the span-carrying JSON AST, or into its canonical rendering. Both
    // are pure text -> text and double as a parse check (they fail on
    // malformed source with the parser's diagnostic).
    ev.register_eager(
        "ctfe_source_ast_json",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |args| {
            let source = require_string(&args[0], "ctfe-source-ast-json expects source text")?;
            crate::frontend::ast_json(&source)
                .map(|json| super::args::string(&json))
                .map_err(EvalSignal::from)
        },
    );

    ev.register_eager(
        "ctfe_source_canonicalize",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |args| {
            let source = require_string(&args[0], "ctfe-source-canonicalize expects source text")?;
            crate::frontend::canonicalize_source(&source)
                .map(|text| super::args::string(&text))
                .map_err(EvalSignal::from)
        },
    );
}

/// The span donated by an optional ORIGIN argument: a syntax/spec value whose
/// own span is inherited. Null/absent → no span (never fabricated).
fn origin_span(
    value: Option<&RuntimeValue>,
    context: &str,
) -> Result<Option<crate::source::SourceSpan>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(value) => Ok(require_expr_spec(
            value,
            &format!("{context} origin must be a syntax value or null"),
        )?
        .span()
        .cloned()),
    }
}

fn syntax_value(spec: ExprSpec) -> RuntimeValue {
    RuntimeValue::HostObject(Rc::new(ExprSpecBridgeValue::new(spec)))
}

fn syntax_sequence(value: &RuntimeValue, message: &str) -> Result<Vec<ExprSpec>, EvalSignal> {
    match value {
        RuntimeValue::Tuple(items) => items
            .iter()
            .map(|item| require_expr_spec(item, message))
            .collect(),
        RuntimeValue::List(items) => items
            .borrow()
            .iter()
            .map(|item| require_expr_spec(item, message))
            .collect(),
        _ => Err(eval_err(message)),
    }
}

// Non-empty string coercion: canonical `args::require_named_string`, kept under
// the historical local name to avoid touching call sites.
use super::args::require_named_string as require_string;
