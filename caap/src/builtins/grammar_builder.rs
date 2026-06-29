/// CTFE builtins for programmatic grammar construction without string parsing.
///
/// Exposed primitives:
///   ctfe-peg-builder                 [start]            → builder-obj
///   ctfe-peg-builder-rule            b name expr        → builder-obj
///   ctfe-peg-builder-parametric-rule b name params expr → builder-obj
///   ctfe-peg-builder-import          b alias grammar    → builder-obj
///   ctfe-peg-builder-build           b                  → grammar-obj
///
///   ctfe-peg-* constructors expose the public `PegExpr` builder set:
///   terminals, structural combinators, layout markers, semantic hooks
///   (`@action`/`@?pred` via the Parse Effects Protocol driver), parametric
///   calls, token refs, and cross-grammar refs.
use std::{any::Any, rc::Rc};

use caap_peg::{builder, Grammar, GrammarBuilder, PegExpr};

use crate::{
    eval::{eval_args, Evaluator},
    values::{eval_err, EvalSignal, RuntimeValue},
};

use super::{
    args::{require_bool, require_string},
    grammar::{grammar_from_runtime_value, grammar_host_obj},
};

// ── PegExprValue ──────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct PegExprValue {
    pub expr: PegExpr,
}

impl crate::values::HostObject for PegExprValue {
    fn type_name(&self) -> &'static str {
        "peg_expr"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ── GrammarBuilderValue ───────────────────────────────────────────────────────

/// Immutable accumulator — each `ctfe-peg-builder-rule` returns a NEW value.
/// Each rule carries its parameter names (empty for ordinary rules).
#[derive(Debug)]
pub struct GrammarBuilderValue {
    start_rule: Option<String>,
    rules: Vec<(String, Vec<String>, PegExpr)>,
    imports: Vec<(String, Grammar)>,
}

impl crate::values::HostObject for GrammarBuilderValue {
    fn type_name(&self) -> &'static str {
        "peg_builder"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl GrammarBuilderValue {
    fn build(&self) -> caap_peg::Grammar {
        let mut gb = GrammarBuilder::new();
        if let Some(s) = &self.start_rule {
            gb = gb.start(s.as_str());
        }
        for (name, params, expr) in &self.rules {
            gb = if params.is_empty() {
                gb.rule(name.as_str(), expr.clone())
            } else {
                gb.parametric(name.as_str(), params.clone(), expr.clone())
            };
        }
        for (alias, grammar) in &self.imports {
            gb = gb.import(alias.as_str(), grammar.clone());
        }
        gb.build()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn downcast_peg_expr<'a>(
    value: &'a RuntimeValue,
    context: &str,
) -> Result<&'a PegExprValue, EvalSignal> {
    super::grammar::downcast_host_object(value, context, "peg-expr")
}

fn peg_expr_obj(expr: PegExpr) -> RuntimeValue {
    RuntimeValue::HostObject(Rc::new(PegExprValue { expr }))
}

fn downcast_builder<'a>(
    value: &'a RuntimeValue,
    context: &str,
) -> Result<&'a GrammarBuilderValue, EvalSignal> {
    super::grammar::downcast_host_object(value, context, "peg-builder")
}

fn builder_obj(b: GrammarBuilderValue) -> RuntimeValue {
    RuntimeValue::HostObject(Rc::new(b))
}

fn extract_expr_list(value: &RuntimeValue, context: &str) -> Result<Vec<PegExpr>, EvalSignal> {
    let RuntimeValue::List(lst) = value else {
        return Err(eval_err(format!("{context}: expected list of peg-expr")));
    };
    let items = lst.borrow().clone();
    let mut exprs = Vec::with_capacity(items.len());
    for item in &items {
        exprs.push(downcast_peg_expr(item, context)?.expr.clone());
    }
    Ok(exprs)
}

fn extract_string_list(value: &RuntimeValue, context: &str) -> Result<Vec<String>, EvalSignal> {
    let RuntimeValue::List(lst) = value else {
        return Err(eval_err(format!("{context}: expected list of strings")));
    };
    let items = lst.borrow().clone();
    let mut names = Vec::with_capacity(items.len());
    for item in &items {
        names.push(require_string(item, context)?);
    }
    Ok(names)
}

/// `null` → `None`, a string → `Some(string)`; anything else is an error.
fn optional_string(value: &RuntimeValue, context: &str) -> Result<Option<String>, EvalSignal> {
    match value {
        RuntimeValue::Null => Ok(None),
        RuntimeValue::Str(s) => Ok(Some(s.to_string())),
        _ => Err(eval_err(format!("{context}: expected a string or null"))),
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(ev: &mut Evaluator) {
    // ctfe-peg-builder [start-rule] → builder-obj
    ev.register_special(
        "ctfe_peg_builder",
        0,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let start_rule = if args.is_empty() {
                None
            } else {
                Some(require_string(
                    &args[0],
                    "ctfe_peg_builder: start rule must be a string",
                )?)
            };
            Ok(builder_obj(GrammarBuilderValue {
                start_rule,
                rules: Vec::new(),
                imports: Vec::new(),
            }))
        },
    );

    // ctfe-peg-builder-rule builder name peg-expr → new builder-obj
    ev.register_special(
        "ctfe_peg_builder_rule",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bv = downcast_builder(&args[0], "ctfe_peg_builder_rule")?;
            let name = require_string(&args[1], "ctfe_peg_builder_rule: name must be a string")?;
            let expr = downcast_peg_expr(&args[2], "ctfe_peg_builder_rule")?
                .expr
                .clone();
            let mut rules = bv.rules.clone();
            rules.push((name, Vec::new(), expr));
            Ok(builder_obj(GrammarBuilderValue {
                start_rule: bv.start_rule.clone(),
                rules,
                imports: bv.imports.clone(),
            }))
        },
    );

    // ctfe-peg-builder-parametric-rule builder name params-list peg-expr → new builder-obj
    ev.register_special(
        "ctfe_peg_builder_parametric_rule",
        4,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bv = downcast_builder(&args[0], "ctfe_peg_builder_parametric_rule")?;
            let name = require_string(
                &args[1],
                "ctfe_peg_builder_parametric_rule: name must be a string",
            )?;
            let params = extract_string_list(
                &args[2],
                "ctfe_peg_builder_parametric_rule: params must be a list of strings",
            )?;
            let expr = downcast_peg_expr(&args[3], "ctfe_peg_builder_parametric_rule")?
                .expr
                .clone();
            let mut rules = bv.rules.clone();
            rules.push((name, params, expr));
            Ok(builder_obj(GrammarBuilderValue {
                start_rule: bv.start_rule.clone(),
                rules,
                imports: bv.imports.clone(),
            }))
        },
    );

    // ctfe-peg-builder-import builder alias grammar → new builder-obj
    ev.register_special(
        "ctfe_peg_builder_import",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bv = downcast_builder(&args[0], "ctfe_peg_builder_import")?;
            let alias =
                require_string(&args[1], "ctfe_peg_builder_import: alias must be a string")?;
            if alias.is_empty() {
                return Err(eval_err("ctfe_peg_builder_import: alias must be non-empty"));
            }
            let grammar = grammar_from_runtime_value(&args[2], "ctfe_peg_builder_import: grammar")?;
            let mut imports = bv.imports.clone();
            imports.push((alias, grammar));
            Ok(builder_obj(GrammarBuilderValue {
                start_rule: bv.start_rule.clone(),
                rules: bv.rules.clone(),
                imports,
            }))
        },
    );

    // ctfe-peg-builder-build builder → grammar-obj
    ev.register_special(
        "ctfe_peg_builder_build",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bv = downcast_builder(&args[0], "ctfe_peg_builder_build")?;
            Ok(grammar_host_obj(bv.build()))
        },
    );

    // ctfe-peg-lit text → peg-expr
    ev.register_special(
        "ctfe_peg_lit",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let text = require_string(&args[0], "ctfe_peg_lit: text must be a string")?;
            Ok(peg_expr_obj(builder::lit(text)))
        },
    );

    // ctfe-peg-ref name → peg-expr
    ev.register_special(
        "ctfe_peg_ref",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let name = require_string(&args[0], "ctfe_peg_ref: name must be a string")?;
            Ok(peg_expr_obj(builder::rule_ref(name)))
        },
    );

    // ctfe-peg-seq list-of-expr → peg-expr
    ev.register_special(
        "ctfe_peg_seq",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let exprs = extract_expr_list(&args[0], "ctfe_peg_seq")?;
            Ok(peg_expr_obj(builder::seq(exprs)))
        },
    );

    // ctfe-peg-choice list-of-expr → peg-expr
    ev.register_special(
        "ctfe_peg_choice",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let exprs = extract_expr_list(&args[0], "ctfe_peg_choice")?;
            Ok(peg_expr_obj(builder::choice(exprs)))
        },
    );

    // ctfe-peg-plus expr → peg-expr
    ev.register_special(
        "ctfe_peg_plus",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let expr = downcast_peg_expr(&args[0], "ctfe_peg_plus")?.expr.clone();
            Ok(peg_expr_obj(builder::plus(expr)))
        },
    );

    // ctfe-peg-star expr → peg-expr

    // ctfe-peg-opt expr → peg-expr

    // ctfe-peg-not expr → peg-expr

    // ctfe-peg-and expr → peg-expr

    // ctfe-peg-regex pattern → peg-expr  (eval error on bad pattern)
    ev.register_special(
        "ctfe_peg_regex",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let pattern = require_string(&args[0], "ctfe_peg_regex: pattern must be a string")?;
            let expr = builder::regex(pattern)
                .map_err(|e| eval_err(format!("ctfe_peg_regex: invalid pattern: {}", e.message)))?;
            Ok(peg_expr_obj(expr))
        },
    );

    // ctfe-peg-action name expr → peg-expr
    ev.register_special(
        "ctfe_peg_action",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let name = require_string(&args[0], "ctfe_peg_action: name must be a string")?;
            let expr = downcast_peg_expr(&args[1], "ctfe_peg_action")?.expr.clone();
            Ok(peg_expr_obj(builder::semantic_action(name, expr)))
        },
    );

    // ctfe-peg-predicate name → peg-expr

    // ── Zero-argument terminals ─────────────────────────────────────────────
    for (name, ctor) in [
        ("ctfe_peg_dot", builder::dot as fn() -> PegExpr),
        ("ctfe_peg_cut", builder::cut),
        ("ctfe_peg_newline", builder::newline),
        ("ctfe_peg_indent", builder::indent),
        ("ctfe_peg_dedent", builder::dedent),
    ] {
        ev.register_special(
            name.to_string(),
            0,
            Some(0),
            crate::values::BuiltinMetadata::compile_time_pure(),
            move |ev, call, env| {
                eval_args(ev, call, env)?;
                Ok(peg_expr_obj(ctor()))
            },
        );
    }

    // ── Single-string terminals ─────────────────────────────────────────────
    for (name, ctor) in [
        (
            "ctfe_peg_keyword",
            builder::keyword as fn(String) -> PegExpr,
        ),
        ("ctfe_peg_soft_keyword", builder::soft_keyword),
        ("ctfe_peg_param", builder::param),
    ] {
        ev.register_special(
            name.to_string(),
            1,
            Some(1),
            crate::values::BuiltinMetadata::compile_time_pure(),
            move |ev, call, env| {
                let args = eval_args(ev, call, env)?;
                let text = require_string(&args[0], name)?;
                Ok(peg_expr_obj(ctor(text)))
            },
        );
    }

    // ── Single sub-expression combinators ───────────────────────────────────
    for (name, ctor) in [
        ("ctfe_peg_eager", builder::eager as fn(PegExpr) -> PegExpr),
        ("ctfe_peg_no_trivia", builder::no_trivia),
    ] {
        ev.register_special(
            name.to_string(),
            1,
            Some(1),
            crate::values::BuiltinMetadata::compile_time_pure(),
            move |ev, call, env| {
                let args = eval_args(ev, call, env)?;
                let expr = downcast_peg_expr(&args[0], name)?.expr.clone();
                Ok(peg_expr_obj(ctor(expr)))
            },
        );
    }

    // ── name/label + sub-expression ─────────────────────────────────────────
    for (name, ctor) in [
        (
            "ctfe_peg_named",
            builder::named as fn(String, PegExpr) -> PegExpr,
        ),
        ("ctfe_peg_capture", builder::capture),
        ("ctfe_peg_expected", builder::expected),
        ("ctfe_peg_grammar_scope", builder::grammar_scope),
    ] {
        ev.register_special(
            name.to_string(),
            2,
            Some(2),
            crate::values::BuiltinMetadata::compile_time_pure(),
            move |ev, call, env| {
                let args = eval_args(ev, call, env)?;
                let label = require_string(&args[0], name)?;
                let expr = downcast_peg_expr(&args[1], name)?.expr.clone();
                Ok(peg_expr_obj(ctor(label, expr)))
            },
        );
    }

    // ── element + separator ─────────────────────────────────────────────────
    for (name, ctor) in [
        (
            "ctfe_peg_sep_plus",
            builder::sep_plus as fn(PegExpr, PegExpr) -> PegExpr,
        ),
        ("ctfe_peg_interspersed", builder::interspersed),
    ] {
        ev.register_special(
            name.to_string(),
            2,
            Some(2),
            crate::values::BuiltinMetadata::compile_time_pure(),
            move |ev, call, env| {
                let args = eval_args(ev, call, env)?;
                let element = downcast_peg_expr(&args[0], name)?.expr.clone();
                let separator = downcast_peg_expr(&args[1], name)?.expr.clone();
                Ok(peg_expr_obj(ctor(element, separator)))
            },
        );
    }

    // ctfe-peg-char-class body → peg-expr  (eval error on bad class)
    ev.register_special(
        "ctfe_peg_char_class",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let body = require_string(&args[0], "ctfe_peg_char_class: body must be a string")?;
            let expr = builder::char_class(body).map_err(|e| {
                eval_err(format!("ctfe_peg_char_class: invalid class: {}", e.message))
            })?;
            Ok(peg_expr_obj(expr))
        },
    );

    // ctfe-peg-imported-ref grammar rule → peg-expr
    ev.register_special(
        "ctfe_peg_imported_ref",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let grammar =
                require_string(&args[0], "ctfe_peg_imported_ref: grammar must be a string")?;
            let rule = require_string(&args[1], "ctfe_peg_imported_ref: rule must be a string")?;
            Ok(peg_expr_obj(builder::imported_ref(grammar, rule)))
        },
    );

    // ctfe-peg-call rule list-of-expr → peg-expr  (call a parametric rule)
    ev.register_special(
        "ctfe_peg_call",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let rule = require_string(&args[0], "ctfe_peg_call: rule must be a string")?;
            let call_args = extract_expr_list(&args[1], "ctfe_peg_call")?;
            Ok(peg_expr_obj(builder::call(rule, call_args)))
        },
    );

    // ctfe-peg-island start end include-delims? → peg-expr
    ev.register_special(
        "ctfe_peg_island",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let start = require_string(&args[0], "ctfe_peg_island: start must be a string")?;
            let end = require_string(&args[1], "ctfe_peg_island: end must be a string")?;
            let include = require_bool(&args[2], "ctfe_peg_island: include-delims must be a bool")?;
            Ok(peg_expr_obj(builder::island(start, end, include)))
        },
    );

    // ctfe-peg-raw-block start end delim-kind → peg-expr
    ev.register_special(
        "ctfe_peg_raw_block",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let start = require_string(&args[0], "ctfe_peg_raw_block: start must be a string")?;
            let end = require_string(&args[1], "ctfe_peg_raw_block: end must be a string")?;
            let kind = require_string(&args[2], "ctfe_peg_raw_block: delim-kind must be a string")?;
            Ok(peg_expr_obj(builder::raw_block(start, end, kind)))
        },
    );

    // ctfe-peg-token-ref [kind] [text] → peg-expr  (kind/text may be null)
    ev.register_special(
        "ctfe_peg_token_ref",
        0,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let kind = match args.first() {
                Some(value) => optional_string(value, "ctfe_peg_token_ref: kind")?,
                None => None,
            };
            let text = match args.get(1) {
                Some(value) => optional_string(value, "ctfe_peg_token_ref: text")?,
                None => None,
            };
            Ok(peg_expr_obj(builder::token_ref(kind, text)))
        },
    );
}
