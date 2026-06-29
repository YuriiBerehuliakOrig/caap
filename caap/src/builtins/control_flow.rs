/// Control-flow builtins — port of `caap/builtins/lang/control_flow.py`.
///
/// Covers: if, or, and, do, lambda, bind, block, leave, while, not.
use std::rc::Rc;

use crate::bind_args::flat_bind_pairs_and_body;
use crate::eval::Evaluator;
use crate::ir::{IrLiteralData, Node, NodeId};
use crate::values::{
    eval_err, is_truthy, ClosureValue, Environment, EvalSignal, LeaveSignal, MacroValue, MapKey,
    RuntimeValue,
};

type BindPairs = Vec<(String, NodeId)>;
type BindBodyIds = Vec<NodeId>;
type BindParts = (BindPairs, BindBodyIds);

pub fn register(ev: &mut Evaluator) {
    // ── if ───────────────────────────────────────────────────────────────────
    ev.register_special(
        "if",
        2,
        Some(3),
        crate::values::BuiltinMetadata::special_form()
            .with_eval_policy(crate::semantic::EvalPolicy::LazyIf)
            .with_control_policy(crate::semantic::ControlPolicy::ConditionalBranch),
        |ev, call, env| {
            let cond = ev.eval(call.args[0], env)?;
            // Branches inherit the `if`'s own tail position (TCO).
            if is_truthy(&cond) {
                ev.eval_in_tail_position(call.args[1], env)
            } else if call.args.len() == 3 {
                ev.eval_in_tail_position(call.args[2], env)
            } else {
                Ok(RuntimeValue::Null)
            }
        },
    );

    // ── or ───────────────────────────────────────────────────────────────────
    ev.register_special(
        "or",
        0,
        None,
        crate::values::BuiltinMetadata::special_form()
            .with_eval_policy(crate::semantic::EvalPolicy::LazyIf)
            .with_control_policy(crate::semantic::ControlPolicy::ConditionalBranch),
        |ev, call, env| {
            let mut result = RuntimeValue::Null;
            for &arg_id in call.args.iter() {
                result = ev.eval(arg_id, env)?;
                if is_truthy(&result) {
                    return Ok(result);
                }
            }
            Ok(result)
        },
    );

    // ── and ──────────────────────────────────────────────────────────────────
    ev.register_special(
        "and",
        0,
        None,
        crate::values::BuiltinMetadata::special_form()
            .with_eval_policy(crate::semantic::EvalPolicy::LazyIf)
            .with_control_policy(crate::semantic::ControlPolicy::ConditionalBranch),
        |ev, call, env| {
            let mut result = RuntimeValue::Null;
            for &arg_id in call.args.iter() {
                result = ev.eval(arg_id, env)?;
                if !is_truthy(&result) {
                    return Ok(result);
                }
            }
            Ok(result)
        },
    );

    // ── do ───────────────────────────────────────────────────────────────────
    ev.register_special(
        "do",
        0,
        None,
        crate::values::BuiltinMetadata::special_form()
            .with_eval_policy(crate::semantic::EvalPolicy::Sequential),
        // The last form inherits the `do`'s own tail position (TCO).
        |ev, call, env| ev.eval_sequence_in_tail_position(&call.args, env),
    );

    // ── lambda ───────────────────────────────────────────────────────────────
    // Syntax: (lambda (param ...) body ...)
    // The first arg must resolve to a "params list" node — we support two
    // representations:
    //   • A CallNode whose args are NameNodes (e.g. `(x y z)`)
    //   • A LiteralNode with a Tuple/Null (parameter-less lambda)
    ev.register_special(
        "lambda",
        2,
        None,
        crate::values::BuiltinMetadata::special_form()
            .with_scope_policy(crate::semantic::ScopePolicy::LexicalBinding),
        |ev, call, env| {
            let params = extract_param_names(ev, call.args[0])?;
            let body_ids: Vec<NodeId> = call.args[1..].to_vec();
            let closure = Rc::new(ClosureValue {
                params,
                body_ids,
                env: Rc::clone(env),
                graph: ev.graph_handle(),
            });
            Ok(RuntimeValue::Closure(closure))
        },
    );

    // ── macro ────────────────────────────────────────────────────────────────
    // Syntax: (macro (param ...) body ...)
    //
    // Macro arguments are quoted into syntax values. The macro body must return
    // syntax, which is expanded and evaluated in the caller's environment.
    ev.register_special(
        "macro",
        2,
        None,
        crate::values::BuiltinMetadata::special_form()
            .with_scope_policy(crate::semantic::ScopePolicy::LexicalBinding),
        |ev, call, env| {
            let params = extract_param_names(ev, call.args[0])?;
            let body_ids: Vec<NodeId> = call.args[1..].to_vec();
            let mac = Rc::new(MacroValue {
                params,
                body_ids,
                env: Rc::clone(env),
                graph: ev.graph_handle(),
            });
            Ok(RuntimeValue::Macro(mac))
        },
    );

    // ── bind ─────────────────────────────────────────────────────────────────
    // Two forms:
    //   Flat:  (bind name val [body ...])  — defines name=val in current env
    //   Multi: (bind ((name val) ...) body ...) — letrec-style in child env
    ev.register_special(
        "bind",
        2,
        None,
        crate::values::BuiltinMetadata::special_form()
            .with_scope_policy(crate::semantic::ScopePolicy::LexicalBinding),
        |ev, call, env| {
            let bindings_id = call.args[0];
            let body_ids: Vec<NodeId> = call.args[1..].to_vec();

            let bindings_node = ev
                .graph()
                .node(bindings_id)
                .ok_or_else(|| eval_err("bind: missing bindings node"))?
                .clone();

            match bindings_node {
                Node::Name(name_node) => {
                    // Flat form: (bind name val [body ...])
                    // val is body_ids[0], rest are actual body forms.
                    if body_ids.is_empty() {
                        return Err(eval_err("bind: flat form requires at least a value"));
                    }
                    let value = ev.eval(body_ids[0], env)?;
                    Environment::define(env, name_node.identifier.to_string(), value);
                    if body_ids.len() > 1 {
                        // The bind body's last form inherits the bind's own
                        // tail position (TCO).
                        ev.eval_sequence_in_tail_position(&body_ids[1..], env)
                    } else {
                        Ok(RuntimeValue::Null)
                    }
                }
                Node::Literal(lit) => match &lit.value {
                    IrLiteralData::Str(_) => {
                        let (pairs, body_ids) = extract_flat_literal_bindings(ev, call)?;
                        evaluate_bind_pairs(ev, env, pairs, &body_ids)
                    }
                    IrLiteralData::Null => ev.eval_sequence_in_tail_position(&body_ids, env),
                    _ => Err(eval_err(
                        "bind: literal binding head must be a string name or null",
                    )),
                },
                Node::Call(_) => {
                    // Multi-binding form with bindings CallNode.
                    let pairs = extract_bindings(ev, bindings_id)?;
                    evaluate_bind_pairs(ev, env, pairs, &body_ids)
                }
            }
        },
    );

    // ── assign-lexical ───────────────────────────────────────────────────────
    // Internal special form emitted by the frontend for (set! name expr).
    // args[0] is the unevaluated target NameNode; args[1] is the new value.
    ev.register_special(
        "assign_lexical",
        2,
        Some(2),
        crate::values::BuiltinMetadata::runtime_mutation().internal(),
        |ev, call, env| {
            let target_id = call.args[0];
            let name = match ev.graph().node(target_id) {
                Some(Node::Name(name_node)) => name_node.identifier.to_string(),
                Some(_) => {
                    return Err(eval_err(
                        "assign_lexical: first arg must be an unevaluated name target",
                    ))
                }
                None => return Err(eval_err("assign_lexical: missing target node")),
            };
            let value = ev.eval(call.args[1], env)?;
            ev.assign_lexical_name(target_id, env, &name, value)
                .map_err(EvalSignal::Error)?;
            Ok(RuntimeValue::Null)
        },
    );

    // ── block ────────────────────────────────────────────────────────────────
    // Syntax: (block label body ...) or just (block body ...)
    // Catches LeaveSignal whose target matches this call_node.id.
    ev.register_special(
        "block",
        1,
        None,
        crate::values::BuiltinMetadata::special_form()
            .with_control_policy(crate::semantic::ControlPolicy::StructuredExit),
        |ev, call, env| {
            let block_id = call.id;
            // The optional label is consumed by the frontend lowering (leave
            // carries the resolved block id), so at eval time every arg IS a
            // body expression — nothing transitional about it.
            let body_ids: Vec<NodeId> = call.args.to_vec();
            match ev.eval_sequence(&body_ids, env) {
                Ok(v) => Ok(v),
                Err(EvalSignal::Leave(signal)) if signal.target_block_id == block_id => {
                    Ok(signal.value)
                }
                Err(other) => Err(other),
            }
        },
    );

    // ── leave ────────────────────────────────────────────────────────────────
    // Syntax: (leave target-block-id [value])
    // target-block-id is a literal integer NodeId of the target block CallNode.
    ev.register_special(
        "leave",
        1,
        Some(2),
        crate::values::BuiltinMetadata::special_form()
            .with_control_policy(crate::semantic::ControlPolicy::StructuredExit),
        |ev, call, env| {
            let target_block_id = resolve_leave_target(ev, call.args[0])?;
            let value = if call.args.len() == 2 {
                ev.eval(call.args[1], env)?
            } else {
                RuntimeValue::Null
            };
            Err(EvalSignal::Leave(LeaveSignal {
                target_block_id,
                value,
            }))
        },
    );

    // ── while ────────────────────────────────────────────────────────────────
    ev.register_special(
        "while",
        2,
        Some(2),
        crate::values::BuiltinMetadata::special_form()
            .with_eval_policy(crate::semantic::EvalPolicy::LazyIf)
            .with_control_policy(crate::semantic::ControlPolicy::ConditionalBranch),
        |ev, call, env| {
            while is_truthy(&ev.eval(call.args[0], env)?) {
                ev.eval(call.args[1], env)?;
            }
            Ok(RuntimeValue::Null)
        },
    );

    // ── match ─────────────────────────────────────────────────────────────────
    // Syntax: (match scrutinee (pattern body...) ...)
    // Patterns:
    //   _  /  else              – wildcard
    //   name                   – bind the value to `name` in body scope
    //   null / true / false    – literal equality
    //   integer / float / str  – literal equality
    //   (null? n) (bool? n) (int? n) (float? n) (str? n) (list? n) (map? n)
    //                          – type guard; binds `n` if given
    ev.register_special(
        "match",
        1,
        None,
        crate::values::BuiltinMetadata::special_form()
            .with_eval_policy(crate::semantic::EvalPolicy::LazyIf)
            .with_control_policy(crate::semantic::ControlPolicy::ConditionalBranch),
        |ev, call, env| {
            let scrutinee = ev.eval(call.args[0], env)?;
            // Collect clause info without holding a borrow on `ev` across the eval calls.
            let clauses: Vec<(NodeId, Vec<NodeId>)> = call.args[1..]
                .iter()
                .map(|&id| extract_match_clause(ev, id))
                .collect::<Result<_, _>>()?;
            for (pattern_id, body_ids) in clauses {
                let bindings = match_pattern(ev, pattern_id, &scrutinee)?;
                if let Some(bindings) = bindings {
                    let clause_env = Environment::new(Some(Rc::clone(env)));
                    for (name, val) in bindings {
                        Environment::define(&clause_env, name, val);
                    }
                    // Arm bodies inherit the match's own tail position (TCO).
                    return ev.eval_sequence_in_tail_position(&body_ids, &clause_env);
                }
            }
            Ok(RuntimeValue::Null)
        },
    );

    // ── throw ─────────────────────────────────────────────────────────────────
    // Syntax: (throw expr)
    ev.register_special(
        "throw",
        1,
        Some(1),
        crate::values::BuiltinMetadata::special_form()
            .with_control_policy(crate::semantic::ControlPolicy::StructuredExit),
        |ev, call, env| {
            let val = ev.eval(call.args[0], env)?;
            Err(EvalSignal::Exception(val))
        },
    );

    // ── try ───────────────────────────────────────────────────────────────────
    // Syntax: (try body-expr (catch err-name handler-expr...))
    // Catches `throw`n values AND (non-fatal) evaluation errors: the handler
    // receives the thrown value as-is, or — for an error — a map
    // {"message": str, "category": str|null}. FATAL errors (step/depth budget
    // exhaustion) pierce `try` by design: catching them would void the
    // resource guarantee. `leave` passes through (it is control flow).
    ev.register_special(
        "try",
        1,
        Some(2),
        crate::values::BuiltinMetadata::special_form()
            .with_eval_policy(crate::semantic::EvalPolicy::LazyIf)
            .with_control_policy(crate::semantic::ControlPolicy::ConditionalBranch),
        |ev, call, env| {
            // Collect catch clause info before any eval so we don't hold a borrow.
            let catch_info: Option<(String, Vec<NodeId>)> = if call.args.len() == 2 {
                Some(extract_catch_clause(ev, call.args[1])?)
            } else {
                None
            };
            let caught = match ev.eval(call.args[0], env) {
                Ok(v) => return Ok(v),
                Err(EvalSignal::Exception(exc_val)) => exc_val,
                Err(EvalSignal::Error(error)) if !error.is_fatal() => {
                    let mut entries = indexmap::IndexMap::new();
                    entries.insert(
                        MapKey::Str("message".into()),
                        RuntimeValue::Str(error.message().into()),
                    );
                    entries.insert(
                        MapKey::Str("category".into()),
                        error
                            .category()
                            .map(|c| RuntimeValue::Str(c.into()))
                            .unwrap_or(RuntimeValue::Null),
                    );
                    RuntimeValue::Map(Rc::new(std::cell::RefCell::new(entries)))
                }
                Err(other) => return Err(other),
            };
            if let Some((err_name, handler_ids)) = catch_info {
                let catch_env = Environment::new(Some(Rc::clone(env)));
                Environment::define(&catch_env, err_name, caught);
                ev.eval_sequence(&handler_ids, &catch_env)
            } else {
                Ok(RuntimeValue::Null)
            }
        },
    );
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Extract parameter name strings from the params list node.
/// The params node can be:
///   • A CallNode with NameNode args  →  (x y z)
///   • A NameNode with identifier "params-list" or anything: treat as 0-arity
///   • A LiteralNode(Null)           →  no parameters
fn extract_param_names(ev: &Evaluator, params_id: NodeId) -> Result<Vec<String>, EvalSignal> {
    let node = ev
        .graph()
        .node(params_id)
        .ok_or_else(|| eval_err(format!("missing params node: {params_id}")))?
        .clone();

    match node {
        Node::Call(call) => {
            let mut names = Vec::new();
            for arg_id in call_item_ids(ev, &call) {
                let arg = ev
                    .graph()
                    .node(arg_id)
                    .ok_or_else(|| eval_err("missing param name node"))?;
                match arg {
                    Node::Name(n) => names.push(n.identifier.to_string()),
                    _ => return Err(eval_err("lambda params must be names")),
                }
            }
            Ok(names)
        }
        Node::Literal(lit) => match &lit.value {
            IrLiteralData::Null => Ok(vec![]),
            IrLiteralData::Tuple(items) => items
                .iter()
                .map(|item| match item {
                    IrLiteralData::Str(name) if !name.is_empty() => Ok(name.clone()),
                    _ => Err(eval_err(
                        "lambda params tuple must contain non-empty string names",
                    )),
                })
                .collect(),
            _ => Err(eval_err(
                "lambda params node must be a call or null literal",
            )),
        },
        Node::Name(_) => Ok(vec![]),
    }
}

/// Extract (name, value_id) pairs from a bind-bindings node.
/// Expected shape: CallNode[ CallNode[NameNode, value_expr] ... ]
fn extract_bindings(
    ev: &Evaluator,
    bindings_id: NodeId,
) -> Result<Vec<(String, NodeId)>, EvalSignal> {
    let node = ev
        .graph()
        .node(bindings_id)
        .ok_or_else(|| eval_err("missing bindings node"))?
        .clone();

    let pairs_node = match node {
        Node::Call(c) => c,
        _ => return Err(eval_err("bind bindings must be a call node")),
    };

    let mut result = Vec::new();
    for pair_id in call_item_ids(ev, &pairs_node) {
        let pair = ev
            .graph()
            .node(pair_id)
            .ok_or_else(|| eval_err("missing binding pair node"))?
            .clone();
        let pair_call = match pair {
            Node::Call(c) => c,
            _ => return Err(eval_err("each binding must be a call node (name value)")),
        };
        let pair_items = call_item_ids(ev, &pair_call);
        if pair_items.len() != 2 {
            return Err(eval_err("each binding must have exactly 2 elements"));
        }
        let name_node = ev
            .graph()
            .node(pair_items[0])
            .ok_or_else(|| eval_err("missing binding name node"))?;
        let name = match name_node {
            Node::Name(n) => n.identifier.to_string(),
            _ => return Err(eval_err("binding name must be a NameNode")),
        };
        result.push((name, pair_items[1]));
    }
    Ok(result)
}

fn extract_flat_literal_bindings(
    ev: &Evaluator,
    call: &crate::ir::CallNode,
) -> Result<BindParts, EvalSignal> {
    let Some((pair_ids, body_id)) = flat_bind_pairs_and_body(&call.args) else {
        return Err(eval_err(
            "bind expects flat canonical binding pairs and a body",
        ));
    };
    let mut pairs = Vec::new();
    for pair in pair_ids.chunks_exact(2) {
        let name = match ev
            .graph()
            .node(pair[0])
            .ok_or_else(|| eval_err("missing bind name node"))?
        {
            Node::Literal(lit) => match &lit.value {
                IrLiteralData::Str(name) if !name.is_empty() => name.clone(),
                _ => return Err(eval_err("bind canonical names must be non-empty strings")),
            },
            _ => return Err(eval_err("bind canonical names must be string literals")),
        };
        pairs.push((name, pair[1]));
    }
    Ok((pairs, vec![body_id]))
}

fn evaluate_bind_pairs(
    ev: &mut Evaluator,
    env: &crate::values::EnvRef,
    pairs: Vec<(String, NodeId)>,
    body_ids: &[NodeId],
) -> Result<RuntimeValue, EvalSignal> {
    let child_env = Environment::new(Some(Rc::clone(env)));
    for (name, _) in &pairs {
        Environment::define(&child_env, name.clone(), RuntimeValue::Null);
    }
    for (name, value_id) in pairs {
        let value = ev.eval(value_id, &child_env)?;
        Environment::define(&child_env, name, value);
    }
    // The bind body's last form inherits the bind's own tail position (TCO).
    ev.eval_sequence_in_tail_position(body_ids, &child_env)
}

fn call_item_ids(ev: &Evaluator, call: &crate::ir::CallNode) -> Vec<NodeId> {
    match ev.graph().node(call.callee) {
        Some(Node::Name(name)) if is_synthetic_group_callee(&name.identifier) => call.args.to_vec(),
        _ => {
            let mut ids = Vec::with_capacity(call.args.len() + 1);
            ids.push(call.callee);
            ids.extend_from_slice(&call.args);
            ids
        }
    }
}

fn is_synthetic_group_callee(identifier: &str) -> bool {
    identifier.starts_with("__") && identifier.ends_with("__")
}

/// Resolve the target block ID for a `leave` call.
/// Accepts: LiteralNode(Int) encoding the NodeId.
fn resolve_leave_target(ev: &Evaluator, target_id: NodeId) -> Result<NodeId, EvalSignal> {
    let node = ev
        .graph()
        .node(target_id)
        .ok_or_else(|| eval_err("missing leave target node"))?;
    match node {
        Node::Literal(lit) => match &lit.value {
            IrLiteralData::Int(i) => Ok(*i as NodeId),
            _ => Err(eval_err("leave target must be an integer NodeId")),
        },
        _ => Err(eval_err("leave target must be a literal integer node")),
    }
}

/// Extract (pattern_id, body_ids) from a match clause node.
fn extract_match_clause(
    ev: &Evaluator,
    clause_id: NodeId,
) -> Result<(NodeId, Vec<NodeId>), EvalSignal> {
    let clause = ev
        .graph()
        .node(clause_id)
        .ok_or_else(|| eval_err("match: missing clause node"))?
        .clone();
    let clause_call = match clause {
        Node::Call(c) => c,
        _ => {
            return Err(eval_err(
                "match: each clause must be a call node (pattern body...)",
            ))
        }
    };
    if is_synthetic_group_callee(match ev.graph().node(clause_call.callee) {
        Some(Node::Name(n)) => n.identifier.as_ref(),
        _ => "",
    }) {
        // flat grouping: args = [pattern_id, body_id...]
        if clause_call.args.is_empty() {
            return Err(eval_err("match: clause must have at least a pattern"));
        }
        Ok((clause_call.args[0], clause_call.args[1..].to_vec()))
    } else {
        // normal: callee = pattern, args = body expressions
        Ok((clause_call.callee, clause_call.args.to_vec()))
    }
}

/// Try to match `value` against the pattern at `pattern_id`.
/// Returns `Some(bindings)` on match or `None` on mismatch.
fn match_pattern(
    ev: &Evaluator,
    pattern_id: NodeId,
    value: &RuntimeValue,
) -> Result<Option<Vec<(String, RuntimeValue)>>, EvalSignal> {
    use crate::ir::IrLiteralData;
    let pattern = ev
        .graph()
        .node(pattern_id)
        .ok_or_else(|| eval_err("match: missing pattern node"))?
        .clone();
    match pattern {
        Node::Name(n) => {
            let ident = n.identifier.as_ref();
            if ident == "_" || ident == "else" {
                Ok(Some(vec![]))
            } else {
                Ok(Some(vec![(ident.to_string(), value.clone())]))
            }
        }
        Node::Literal(lit) => {
            let matched = match &lit.value {
                IrLiteralData::Null => matches!(value, RuntimeValue::Null),
                IrLiteralData::Bool(b) => matches!(value, RuntimeValue::Bool(v) if v == b),
                IrLiteralData::Int(i) => matches!(value, RuntimeValue::Int(v) if v == i),
                IrLiteralData::Float(f) => {
                    // Exact IEEE equality is the intended pattern-match semantics,
                    // consistent with the Int/Str arms. An absolute epsilon tolerance
                    // would be too tight at large magnitudes and too loose near zero.
                    #[allow(clippy::float_cmp)]
                    {
                        matches!(value, RuntimeValue::Float(v) if *v == *f)
                    }
                }
                IrLiteralData::Str(s) => matches!(value, RuntimeValue::Str(v) if v.as_ref() == s),
                _ => return Err(eval_err("match: unsupported literal pattern type")),
            };
            Ok(if matched { Some(vec![]) } else { None })
        }
        Node::Call(call) => {
            // Type-guard pattern: (null? n), (bool? n), (int? n), (float? n),
            //                     (str? n), (list? n), (map? n)
            let callee = ev
                .graph()
                .node(call.callee)
                .ok_or_else(|| eval_err("match: missing type-guard callee"))?
                .clone();
            let pred = match &callee {
                Node::Name(n) => n.identifier.to_string(),
                _ => return Err(eval_err("match: type-guard callee must be a name")),
            };
            let type_match = match pred.as_str() {
                "null?" => matches!(value, RuntimeValue::Null),
                "bool?" => matches!(value, RuntimeValue::Bool(_)),
                "int?" => matches!(value, RuntimeValue::Int(_)),
                "float?" => matches!(value, RuntimeValue::Float(_)),
                "str?" => matches!(value, RuntimeValue::Str(_)),
                "list?" => matches!(value, RuntimeValue::List(_)),
                "map?" => matches!(value, RuntimeValue::Map(_)),
                _ => return Err(eval_err(format!("match: unknown type predicate: {pred}"))),
            };
            if !type_match {
                return Ok(None);
            }
            // Optional binding: first arg is a NameNode
            let bindings = if !call.args.is_empty() {
                let name_node = ev
                    .graph()
                    .node(call.args[0])
                    .ok_or_else(|| eval_err("match: missing binding name"))?
                    .clone();
                match name_node {
                    Node::Name(n) => vec![(n.identifier.to_string(), value.clone())],
                    _ => return Err(eval_err("match: type-guard binding must be a name")),
                }
            } else {
                vec![]
            };
            Ok(Some(bindings))
        }
    }
}

/// Extract (err_name, handler_body_ids) from a `(catch err-name body...)` clause node.
fn extract_catch_clause(
    ev: &Evaluator,
    clause_id: NodeId,
) -> Result<(String, Vec<NodeId>), EvalSignal> {
    let clause = ev
        .graph()
        .node(clause_id)
        .ok_or_else(|| eval_err("try: missing catch clause node"))?
        .clone();
    let clause_call = match clause {
        Node::Call(c) => c,
        _ => return Err(eval_err("try: catch clause must be a call node")),
    };
    // Determine items: if callee is synthetic group, items = args; else items = [callee] ++ args
    let items = call_item_ids(ev, &clause_call);
    // items[0] must be NameNode("catch"), items[1] is err binding name, items[2..] are handler body
    if items.len() < 2 {
        return Err(eval_err(
            "try: catch clause must have at least (catch err_name ...)",
        ));
    }
    let head = ev
        .graph()
        .node(items[0])
        .ok_or_else(|| eval_err("try: missing catch head node"))?
        .clone();
    match &head {
        Node::Name(n) if n.identifier.as_ref() == "catch" => {}
        _ => {
            return Err(eval_err(
                "try: second argument must be a (catch ...) clause",
            ))
        }
    }
    let err_name_node = ev
        .graph()
        .node(items[1])
        .ok_or_else(|| eval_err("try: missing catch binding name node"))?
        .clone();
    let err_name = match err_name_node {
        Node::Name(n) => n.identifier.to_string(),
        _ => return Err(eval_err("try: catch binding must be a name")),
    };
    let handler_ids = items[2..].to_vec();
    Ok((err_name, handler_ids))
}
