/// Core evaluator — mirrors Python's `Evaluator._eval()` dispatch loop.
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use crate::graph::GraphBuilder;
use crate::graph::IRGraph;
use crate::ir::{CallNode, IrLiteralData, Node, NodeId};
use crate::values::{
    eval_err, runtime_value_from_literal, BuiltinInfo, BuiltinMetadata, EnvRef, Environment,
    EvalResult, EvalSignal, RuntimeCallFrame, RuntimeValue,
};

pub struct Evaluator {
    graph: Rc<IRGraph>,
    builtins: Rc<HashMap<String, Rc<BuiltinInfo>>>,
    top_level_names: Vec<String>,
}

// Builtin handlers intentionally receive `&mut Evaluator`. The Rust port keeps
// evaluation single-threaded and phase-ordered, and builtins may need graph
// access, environment-sensitive lazy evaluation, or callback invocation through
// the current evaluator. Pure eager callbacks should use `BuiltinInfo::eager_handler`
// to avoid borrowing the evaluator on hot callback paths.

impl Evaluator {
    pub fn new(graph: IRGraph) -> Self {
        Self::with_top_level_names(graph, Vec::new())
    }

    pub fn with_top_level_names(graph: IRGraph, top_level_names: Vec<String>) -> Self {
        let mut ev = Self {
            graph: Rc::new(graph),
            builtins: Rc::new(HashMap::new()),
            top_level_names,
        };
        crate::builtins::register_all(&mut ev);
        ev
    }

    fn with_registered_builtins(
        graph: Rc<IRGraph>,
        top_level_names: Vec<String>,
        builtins: Rc<HashMap<String, Rc<BuiltinInfo>>>,
    ) -> Self {
        Self {
            graph,
            builtins,
            top_level_names,
        }
    }

    /// Register a builtin by name.
    pub fn register_builtin(&mut self, info: BuiltinInfo) {
        Rc::make_mut(&mut self.builtins).insert(info.name.clone(), Rc::new(info));
    }

    pub fn builtin_info(&self, name: &str) -> Option<&BuiltinInfo> {
        self.builtins.get(name).map(Rc::as_ref)
    }

    pub fn builtin_metadata(&self, name: &str) -> Option<BuiltinMetadata> {
        self.builtin_info(name).map(BuiltinInfo::metadata)
    }

    pub fn graph(&self) -> &IRGraph {
        self.graph.as_ref()
    }

    pub fn graph_handle(&self) -> Rc<IRGraph> {
        Rc::clone(&self.graph)
    }

    pub fn graph_mut(&mut self) -> &mut IRGraph {
        Rc::make_mut(&mut self.graph)
    }

    /// Evaluate a single node in the given environment.
    pub fn eval(&mut self, node_id: NodeId, env: &EnvRef) -> EvalResult {
        let node = self
            .graph
            .node(node_id)
            .ok_or_else(|| eval_err(format!("unknown node id: {node_id}")))?
            .clone();
        trace_eval_node(&self.graph, node_id, &node);

        let result = match &node {
            Node::Literal(lit) => Ok(runtime_value_from_literal(&lit.value)),
            Node::Name(name_node) => {
                match Environment::lookup(env, name_node.identifier.as_ref()) {
                    Ok(value) => Ok(value),
                    Err(error) => self
                        .builtins
                        .get(name_node.identifier.as_ref())
                        .cloned()
                        .map(RuntimeValue::Builtin)
                        .ok_or(EvalSignal::Error(error)),
                }
            }
            Node::Call(call_node) => {
                let call_node = call_node.clone();
                self.eval_call(&call_node, env)
            }
        };
        result.map_err(|signal| self.annotate_signal(signal, node_id, &node))
    }

    /// Evaluate a sequence of nodes, returning the last value (or Null).
    pub fn eval_sequence(&mut self, ids: &[NodeId], env: &EnvRef) -> EvalResult {
        let mut result = RuntimeValue::Null;
        for &id in ids {
            result = self.eval(id, env)?;
        }
        Ok(result)
    }

    /// Evaluate top-level forms with Python-compatible exported scoped bindings.
    pub fn eval_top_level_sequence(&mut self, ids: &[NodeId], env: &EnvRef) -> EvalResult {
        let mut result = RuntimeValue::Null;
        for &id in ids {
            result = self.eval_top_level_form(id, env)?;
        }
        Ok(result)
    }

    fn eval_call(&mut self, call_node: &CallNode, env: &EnvRef) -> EvalResult {
        // Fast path: if callee is a NameNode, look it up directly (skip re-evaluation).
        let callee_node = self
            .graph
            .node(call_node.callee)
            .ok_or_else(|| eval_err(format!("missing callee node: {}", call_node.callee)))?
            .clone();

        let callee = match &callee_node {
            Node::Name(n) => {
                // Check builtins first, then environment.
                let name = n.identifier.as_ref();
                if let Some(bi) = self.builtins.get(name).cloned() {
                    RuntimeValue::Builtin(bi)
                } else {
                    Environment::lookup(env, name).map_err(EvalSignal::Error)?
                }
            }
            _ => self.eval(call_node.callee, env)?,
        };

        match callee {
            RuntimeValue::Builtin(bi) => {
                let arity = call_node.args.len();
                if arity < bi.min_arity {
                    return Err(eval_err(format!(
                        "builtin {} expects at least {} args, got {}",
                        bi.name, bi.min_arity, arity
                    )));
                }
                if let Some(max) = bi.max_arity {
                    if arity > max {
                        return Err(eval_err(format!(
                            "builtin {} expects at most {} args, got {}",
                            bi.name, max, arity
                        )));
                    }
                }
                // bi is a local Rc<BuiltinInfo> — independent of self's borrow.
                (bi.handler)(self, call_node, env)
            }
            RuntimeValue::Closure(cl) => {
                let args_ids: Vec<NodeId> = call_node.args.clone();

                // Evaluate arguments in the *call* environment, not the closure's.
                let mut arg_values = Vec::with_capacity(args_ids.len());
                for &arg_id in &args_ids {
                    arg_values.push(self.eval(arg_id, env)?);
                }

                self.invoke_closure(&cl, arg_values)
            }
            RuntimeValue::HostFunction(host) => {
                let arity = call_node.args.len();
                if arity < host.min_arity {
                    return Err(eval_err(format!(
                        "host function {} expects at least {} args, got {}",
                        host.name, host.min_arity, arity
                    )));
                }
                if let Some(max) = host.max_arity {
                    if arity > max {
                        return Err(eval_err(format!(
                            "host function {} expects at most {} args, got {}",
                            host.name, max, arity
                        )));
                    }
                }
                let mut args = Vec::with_capacity(call_node.args.len());
                for &arg_id in &call_node.args {
                    args.push(self.eval(arg_id, env)?);
                }
                (host.handler)(args)
            }
            other => Err(eval_err(format!("value {other} is not callable"))),
        }
    }

    /// Make a fresh top-level environment with no parent.
    pub fn make_env(&self) -> EnvRef {
        let env = Environment::new(None);
        for name in &self.top_level_names {
            Environment::define_uninitialized(&env, name.clone());
        }
        env
    }

    /// Evaluate the top-level forms of the graph in a fresh environment.
    pub fn run(&mut self) -> EvalResult {
        let env = self.make_env();
        let forms: Vec<NodeId> = self.graph.top_level_form_ids().to_vec();
        self.eval_top_level_sequence(&forms, &env)
    }

    /// Invoke a callback (closure or builtin) with pre-evaluated argument values.
    ///
    /// Used by sequence builtins (sequence-each, sequence-map, etc.) and for-range.
    pub fn invoke_callback(
        &mut self,
        callback: &RuntimeValue,
        args: Vec<RuntimeValue>,
    ) -> EvalResult {
        match callback {
            RuntimeValue::Closure(cl) => self.invoke_closure(cl, args),
            RuntimeValue::HostFunction(host) => {
                if args.len() < host.min_arity {
                    return Err(eval_err(format!(
                        "host function {} expects at least {} args, got {}",
                        host.name,
                        host.min_arity,
                        args.len()
                    )));
                }
                if let Some(max) = host.max_arity {
                    if args.len() > max {
                        return Err(eval_err(format!(
                            "host function {} expects at most {} args, got {}",
                            host.name,
                            max,
                            args.len()
                        )));
                    }
                }
                (host.handler)(args)
            }
            RuntimeValue::Builtin(builtin) => invoke_builtin_with_values(Rc::clone(builtin), args),
            other => Err(eval_err(format!(
                "value {other} is not a callable callback"
            ))),
        }
    }

    fn invoke_closure(
        &mut self,
        closure: &crate::values::ClosureValue,
        args: Vec<RuntimeValue>,
    ) -> EvalResult {
        let child_env = Environment::new(Some(Rc::clone(&closure.env)));
        bind_closure_arguments(&child_env, &closure.params, args)?;
        self.eval_closure_body(closure, &child_env)
    }

    pub fn invoke_closure_with_initial_bindings(
        &mut self,
        closure: &crate::values::ClosureValue,
        args: Vec<RuntimeValue>,
        initial_bindings: &[(String, RuntimeValue)],
    ) -> EvalResult {
        if args.len() != closure.params.len() {
            return Err(eval_err(format!(
                "callback closure expects {} args, got {}",
                closure.params.len(),
                args.len()
            )));
        }
        let child_env = Environment::new(Some(Rc::clone(&closure.env)));
        for (name, value) in initial_bindings {
            Environment::define(&child_env, name.clone(), value.clone());
        }
        for (name, value) in closure.params.iter().zip(args) {
            Environment::define(&child_env, name.clone(), value);
        }
        self.eval_closure_body(closure, &child_env)
    }

    fn eval_closure_body(
        &mut self,
        closure: &crate::values::ClosureValue,
        child_env: &EnvRef,
    ) -> EvalResult {
        if Rc::ptr_eq(&self.graph, &closure.graph) {
            return self.eval_sequence(&closure.body_ids, child_env);
        }
        let mut evaluator = Evaluator::with_registered_builtins(
            Rc::clone(&closure.graph),
            Vec::new(),
            Rc::clone(&self.builtins),
        );
        evaluator.eval_sequence(&closure.body_ids, child_env)
    }

    fn annotate_signal(&self, signal: EvalSignal, node_id: NodeId, node: &Node) -> EvalSignal {
        match signal {
            EvalSignal::Error(mut error) => {
                error.push_frame(self.runtime_call_frame(node_id, node));
                EvalSignal::Error(error)
            }
            EvalSignal::Leave(signal) => EvalSignal::Leave(signal),
        }
    }

    fn runtime_call_frame(&self, node_id: NodeId, node: &Node) -> RuntimeCallFrame {
        RuntimeCallFrame {
            unit_id: None,
            node_id,
            phase: crate::semantic::PhasePolicy::Runtime,
            name: self.runtime_frame_name(node),
            span: self.graph.source_span(node_id).cloned(),
        }
    }

    fn runtime_frame_name(&self, node: &Node) -> Option<String> {
        match node {
            Node::Name(name) => Some(name.identifier.to_string()),
            Node::Call(call) => self.graph.node(call.callee).map(|callee| match callee {
                Node::Name(name) => name.identifier.to_string(),
                _ => "call".to_string(),
            }),
            Node::Literal(_) => None,
        }
    }

    fn eval_top_level_form(&mut self, node_id: NodeId, env: &EnvRef) -> EvalResult {
        let Some((bindings, body_ids)) = self.top_level_exported_bind_parts(node_id)? else {
            return self.eval(node_id, env);
        };
        for (name, value_id) in bindings {
            let value = self.eval(value_id, env)?;
            Environment::define(env, name, value);
        }
        self.eval_sequence(&body_ids, env)
    }

    fn top_level_exported_bind_parts(
        &self,
        node_id: NodeId,
    ) -> Result<Option<(Vec<(String, NodeId)>, Vec<NodeId>)>, EvalSignal> {
        let Some(Node::Call(call)) = self.graph.node(node_id) else {
            return Ok(None);
        };
        if !self.call_callee_is(call, "bind") || call.args.is_empty() {
            return Ok(None);
        }
        match self.graph.node(call.args[0]) {
            Some(Node::Name(name)) => {
                if call.args.len() < 2 {
                    return Ok(None);
                }
                Ok(Some((
                    vec![(name.identifier.to_string(), call.args[1])],
                    call.args[2..].to_vec(),
                )))
            }
            Some(Node::Literal(literal)) => match &literal.value {
                IrLiteralData::Str(_) => self.flat_literal_bind_parts(call).map(Some),
                IrLiteralData::Null => Ok(Some((Vec::new(), call.args[1..].to_vec()))),
                _ => Ok(None),
            },
            Some(Node::Call(bindings_node)) => {
                let mut bindings = Vec::new();
                for pair_id in self.call_item_ids(bindings_node) {
                    let pair = match self.graph.node(pair_id) {
                        Some(Node::Call(pair)) => pair,
                        _ => return Ok(None),
                    };
                    let pair_items = self.call_item_ids(pair);
                    if pair_items.len() != 2 {
                        return Ok(None);
                    }
                    let name = match self.graph.node(pair_items[0]) {
                        Some(Node::Name(name)) => name.identifier.to_string(),
                        _ => return Ok(None),
                    };
                    bindings.push((name, pair_items[1]));
                }
                Ok(Some((bindings, call.args[1..].to_vec())))
            }
            None => Err(eval_err("bind: missing bindings node")),
        }
    }

    fn flat_literal_bind_parts(
        &self,
        call: &CallNode,
    ) -> Result<(Vec<(String, NodeId)>, Vec<NodeId>), EvalSignal> {
        if call.args.len() < 3 || call.args.len().is_multiple_of(2) {
            return Err(eval_err(
                "bind expects flat canonical binding pairs and a body",
            ));
        }
        let mut bindings = Vec::new();
        for index in (0..call.args.len() - 1).step_by(2) {
            let name = match self.graph.node(call.args[index]) {
                Some(Node::Literal(literal)) => match &literal.value {
                    IrLiteralData::Str(name) if !name.is_empty() => name.clone(),
                    _ => return Ok((Vec::new(), Vec::new())),
                },
                _ => return Ok((Vec::new(), Vec::new())),
            };
            bindings.push((name, call.args[index + 1]));
        }
        Ok((
            bindings,
            vec![*call.args.last().expect("checked non-empty bind args")],
        ))
    }

    fn call_callee_is(&self, call: &CallNode, expected: &str) -> bool {
        matches!(
            self.graph.node(call.callee),
            Some(Node::Name(name)) if name.identifier.as_ref() == expected
        )
    }

    fn call_item_ids(&self, call: &CallNode) -> Vec<NodeId> {
        match self.graph.node(call.callee) {
            Some(Node::Name(name)) if is_synthetic_group_callee(&name.identifier) => {
                call.args.clone()
            }
            _ => {
                let mut ids = Vec::with_capacity(call.args.len() + 1);
                ids.push(call.callee);
                ids.extend_from_slice(&call.args);
                ids
            }
        }
    }
}

fn is_synthetic_group_callee(identifier: &str) -> bool {
    identifier.starts_with("__") && identifier.ends_with("__")
}

#[derive(Clone, Debug)]
struct EvalLiveTrace {
    interval: usize,
}

static EVAL_LIVE_TRACE: OnceLock<Option<EvalLiveTrace>> = OnceLock::new();
static EVAL_LIVE_TRACE_STEPS: AtomicUsize = AtomicUsize::new(0);

fn trace_eval_node(graph: &IRGraph, node_id: NodeId, node: &Node) {
    let Some(trace) = EVAL_LIVE_TRACE.get_or_init(eval_live_trace_from_env) else {
        return;
    };
    let step = EVAL_LIVE_TRACE_STEPS.fetch_add(1, Ordering::Relaxed) + 1;
    if step.is_multiple_of(trace.interval) {
        eprintln!(
            "[caap-trace] eval.step step={step} node={node_id} kind={}",
            eval_node_kind(graph, node)
        );
    }
}

fn eval_live_trace_from_env() -> Option<EvalLiveTrace> {
    std::env::var_os("CAAP_RUST_LIVE_TRACE")?;
    if let Ok(filter) = std::env::var("CAAP_RUST_LIVE_TRACE_FILTER") {
        let enabled = filter
            .split(',')
            .map(str::trim)
            .any(|needle| matches!(needle, "eval" | "evaluator"));
        if !enabled {
            return None;
        }
    }
    let interval = std::env::var("CAAP_RUST_EVAL_TRACE_INTERVAL")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(100_000);
    Some(EvalLiveTrace { interval })
}

fn eval_node_kind(graph: &IRGraph, node: &Node) -> String {
    match node {
        Node::Literal(_) => "literal".to_string(),
        Node::Name(name) => format!("name:{}", name.identifier),
        Node::Call(call) => match graph.node(call.callee) {
            Some(Node::Name(name)) => format!("call:{}", name.identifier),
            _ => format!("call:{}", call.id),
        },
    }
}

fn bind_closure_arguments(
    env: &EnvRef,
    params: &[String],
    args: Vec<RuntimeValue>,
) -> Result<(), EvalSignal> {
    let Some(rest_param) = params.last().filter(|param| param.starts_with('&')) else {
        if args.len() != params.len() {
            return Err(eval_err(format!(
                "lambda expected {} args ({}) but got {}",
                params.len(),
                params.join(", "),
                args.len()
            )));
        }
        for (name, val) in params.iter().zip(args) {
            Environment::define(env, name.clone(), val);
        }
        return Ok(());
    };
    if rest_param == "&" {
        return Err(eval_err("rest parameter must include a name"));
    }
    let required_count = params.len() - 1;
    if args.len() < required_count {
        return Err(eval_err(format!(
            "lambda expected at least {required_count} args but got {}",
            args.len()
        )));
    }
    for (name, val) in params[..required_count]
        .iter()
        .zip(args[..required_count].iter().cloned())
    {
        Environment::define(env, name.clone(), val);
    }
    Environment::define(
        env,
        rest_param.clone(),
        RuntimeValue::List(Rc::new(std::cell::RefCell::new(
            args[required_count..].to_vec(),
        ))),
    );
    Ok(())
}

fn invoke_builtin_with_values(builtin: Rc<BuiltinInfo>, args: Vec<RuntimeValue>) -> EvalResult {
    if !builtin.metadata().eager_args {
        return Err(eval_err(format!(
            "builtin {} cannot be invoked as an eager callback",
            builtin.name
        )));
    }
    if args.len() < builtin.min_arity {
        return Err(eval_err(format!(
            "builtin {} expects at least {} args, got {}",
            builtin.name,
            builtin.min_arity,
            args.len()
        )));
    }
    if let Some(max) = builtin.max_arity {
        if args.len() > max {
            return Err(eval_err(format!(
                "builtin {} expects at most {} args, got {}",
                builtin.name,
                max,
                args.len()
            )));
        }
    }
    if let Some(handler) = &builtin.eager_handler {
        return handler(args);
    }

    let mut builder = GraphBuilder::new();
    let callee = builder.name(builtin.name.clone());
    let mut arg_ids = Vec::with_capacity(args.len());
    for index in 0..args.len() {
        arg_ids.push(builder.name(format!("__arg_{index}")));
    }
    let call_id = builder.call(callee, arg_ids);
    let graph = std::mem::take(&mut builder.graph);
    let call = match graph.node(call_id) {
        Some(Node::Call(call)) => call.clone(),
        _ => return Err(eval_err("failed to build builtin callback call")),
    };
    let mut evaluator = Evaluator::new(graph);
    let env = Environment::new(None);
    for (index, value) in args.into_iter().enumerate() {
        Environment::define(&env, format!("__arg_{index}"), value);
    }
    (builtin.handler)(&mut evaluator, &call, &env)
}

// ── Builtin helper ────────────────────────────────────────────────────────────

/// Evaluate all argument nodes eagerly, returning a Vec.
pub fn eval_args(
    ev: &mut Evaluator,
    call_node: &CallNode,
    env: &EnvRef,
) -> Result<Vec<RuntimeValue>, EvalSignal> {
    call_node.args.iter().map(|&id| ev.eval(id, env)).collect()
}
