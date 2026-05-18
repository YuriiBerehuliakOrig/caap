/// Control-flow builtins — port of `caap/builtins/lang/control_flow.py`.
///
/// Covers: if, or, and, do, lambda, bind, block, leave, while, not.
use std::rc::Rc;

use crate::eval::Evaluator;
use crate::ir::{IrLiteralData, Node, NodeId};
use crate::values::{
    eval_err, is_truthy, BuiltinInfo, ClosureValue, Environment, EvalSignal, LeaveSignal,
    RuntimeValue,
};

pub fn register(ev: &mut Evaluator) {
    // ── if ───────────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "if".to_string(),
        metadata: crate::values::BuiltinMetadata::special_form()
            .with_eval_policy(crate::semantic::EvalPolicy::LazyIf)
            .with_control_policy(crate::semantic::ControlPolicy::ConditionalBranch),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let cond = ev.eval(call.args[0], env)?;
            if is_truthy(&cond) {
                ev.eval(call.args[1], env)
            } else if call.args.len() == 3 {
                ev.eval(call.args[2], env)
            } else {
                Ok(RuntimeValue::Null)
            }
        }),
    });

    // ── or ───────────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "or".to_string(),
        metadata: crate::values::BuiltinMetadata::special_form()
            .with_eval_policy(crate::semantic::EvalPolicy::LazyIf)
            .with_control_policy(crate::semantic::ControlPolicy::ConditionalBranch),
        min_arity: 0,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let mut result = RuntimeValue::Null;
            for &arg_id in &call.args {
                result = ev.eval(arg_id, env)?;
                if is_truthy(&result) {
                    return Ok(result);
                }
            }
            Ok(result)
        }),
    });

    // ── and ──────────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "and".to_string(),
        metadata: crate::values::BuiltinMetadata::special_form()
            .with_eval_policy(crate::semantic::EvalPolicy::LazyIf)
            .with_control_policy(crate::semantic::ControlPolicy::ConditionalBranch),
        min_arity: 0,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let mut result = RuntimeValue::Null;
            for &arg_id in &call.args {
                result = ev.eval(arg_id, env)?;
                if !is_truthy(&result) {
                    return Ok(result);
                }
            }
            Ok(result)
        }),
    });

    // ── do ───────────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "do".to_string(),
        metadata: crate::values::BuiltinMetadata::special_form()
            .with_eval_policy(crate::semantic::EvalPolicy::Sequential),
        min_arity: 0,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let mut result = RuntimeValue::Null;
            for &arg_id in &call.args {
                result = ev.eval(arg_id, env)?;
            }
            Ok(result)
        }),
    });

    // ── lambda ───────────────────────────────────────────────────────────────
    // Syntax: (lambda (param ...) body ...)
    // The first arg must resolve to a "params list" node — we support two
    // representations:
    //   • A CallNode whose args are NameNodes (e.g. `(x y z)`)
    //   • A LiteralNode with a Tuple/Null (parameter-less lambda)
    ev.register_builtin(BuiltinInfo {
        name: "lambda".to_string(),
        metadata: crate::values::BuiltinMetadata::special_form()
            .with_scope_policy(crate::semantic::ScopePolicy::LexicalBinding),
        min_arity: 2,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let params = extract_param_names(ev, call.args[0])?;
            let body_ids: Vec<NodeId> = call.args[1..].to_vec();
            let closure = Rc::new(ClosureValue {
                params,
                body_ids,
                env: Rc::clone(env),
                graph: ev.graph_handle(),
            });
            Ok(RuntimeValue::Closure(closure))
        }),
    });

    // ── bind ─────────────────────────────────────────────────────────────────
    // Two forms:
    //   Flat:  (bind name val [body ...])  — defines name=val in current env
    //   Multi: (bind ((name val) ...) body ...) — letrec-style in child env
    ev.register_builtin(BuiltinInfo {
        name: "bind".to_string(),
        metadata: crate::values::BuiltinMetadata::special_form()
            .with_scope_policy(crate::semantic::ScopePolicy::LexicalBinding),
        min_arity: 2,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
                        ev.eval_sequence(&body_ids[1..], env)
                    } else {
                        Ok(RuntimeValue::Null)
                    }
                }
                Node::Literal(lit) => match &lit.value {
                    IrLiteralData::Str(_) => {
                        let (pairs, body_ids) = extract_flat_literal_bindings(ev, call)?;
                        evaluate_bind_pairs(ev, env, pairs, &body_ids)
                    }
                    IrLiteralData::Null => ev.eval_sequence(&body_ids, env),
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
        }),
    });

    // ── set-var ───────────────────────────────────────────────────────────────
    // Internal special form emitted by the frontend for (set name expr).
    // args[0] evaluates to a string (the variable name); args[1] is the new value.
    ev.register_builtin(BuiltinInfo {
        name: "set-var".to_string(),
        metadata: crate::values::BuiltinMetadata::runtime_mutation(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let name_val = ev.eval(call.args[0], env)?;
            let name = match &name_val {
                RuntimeValue::Str(s) => s.to_string(),
                _ => return Err(eval_err("set-var: first arg must be a string name")),
            };
            let value = ev.eval(call.args[1], env)?;
            Environment::assign(env, &name, value).map_err(EvalSignal::Error)?;
            Ok(RuntimeValue::Null)
        }),
    });

    // ── block ────────────────────────────────────────────────────────────────
    // Syntax: (block label body ...) or just (block body ...)
    // Catches LeaveSignal whose target matches this call_node.id.
    ev.register_builtin(BuiltinInfo {
        name: "block".to_string(),
        metadata: crate::values::BuiltinMetadata::special_form()
            .with_control_policy(crate::semantic::ControlPolicy::StructuredExit),
        min_arity: 1,
        max_arity: None,
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let block_id = call.id;
            // Determine whether first arg is a label string (NameNode/LiteralNode with str).
            // For now: treat all args as body expressions.
            let body_ids: Vec<NodeId> = call.args.clone();
            match ev.eval_sequence(&body_ids, env) {
                Ok(v) => Ok(v),
                Err(EvalSignal::Leave(signal)) if signal.target_block_id == block_id => {
                    Ok(signal.value)
                }
                Err(other) => Err(other),
            }
        }),
    });

    // ── leave ────────────────────────────────────────────────────────────────
    // Syntax: (leave target-block-id [value])
    // target-block-id is a literal integer NodeId of the target block CallNode.
    ev.register_builtin(BuiltinInfo {
        name: "leave".to_string(),
        metadata: crate::values::BuiltinMetadata::special_form()
            .with_control_policy(crate::semantic::ControlPolicy::StructuredExit),
        min_arity: 1,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    // ── while ────────────────────────────────────────────────────────────────
    ev.register_builtin(BuiltinInfo {
        name: "while".to_string(),
        metadata: crate::values::BuiltinMetadata::special_form()
            .with_eval_policy(crate::semantic::EvalPolicy::LazyIf)
            .with_control_policy(crate::semantic::ControlPolicy::ConditionalBranch),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            while is_truthy(&ev.eval(call.args[0], env)?) {
                ev.eval(call.args[1], env)?;
            }
            Ok(RuntimeValue::Null)
        }),
    });
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
) -> Result<(Vec<(String, NodeId)>, Vec<NodeId>), EvalSignal> {
    if call.args.len() < 3 || call.args.len().is_multiple_of(2) {
        return Err(eval_err(
            "bind expects flat canonical binding pairs and a body",
        ));
    }
    let mut pairs = Vec::new();
    for index in (0..call.args.len() - 1).step_by(2) {
        let name = match ev
            .graph()
            .node(call.args[index])
            .ok_or_else(|| eval_err("missing bind name node"))?
        {
            Node::Literal(lit) => match &lit.value {
                IrLiteralData::Str(name) if !name.is_empty() => name.clone(),
                _ => return Err(eval_err("bind canonical names must be non-empty strings")),
            },
            _ => return Err(eval_err("bind canonical names must be string literals")),
        };
        pairs.push((name, call.args[index + 1]));
    }
    Ok((
        pairs,
        vec![*call.args.last().expect("checked non-empty args")],
    ))
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
    ev.eval_sequence(body_ids, &child_env)
}

fn call_item_ids(ev: &Evaluator, call: &crate::ir::CallNode) -> Vec<NodeId> {
    match ev.graph().node(call.callee) {
        Some(Node::Name(name)) if is_synthetic_group_callee(&name.identifier) => call.args.clone(),
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
