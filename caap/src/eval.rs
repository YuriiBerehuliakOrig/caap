/// Core evaluator dispatch loop.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use crate::bind_args::flat_bind_pairs_and_body;
use crate::builtins::ir_builders::{require_expr_spec, ExprSpecBridgeValue};
use crate::graph::IRGraph;
use crate::ir::{CallNode, ExprSpec, IrLiteralData, Node, NodeId};
use crate::semantic::{EffectPolicy, EffectSet, PhasePolicy};
use crate::values::{
    eval_err, runtime_value_from_literal, BuiltinHandler, BuiltinInfo, BuiltinMetadata, EnvRef,
    Environment, EvalResult, EvalSignal, LexicalAddress, RuntimeCallFrame, RuntimeValue,
};

type BindPairs = Vec<(String, NodeId)>;
type BindBodyIds = Vec<NodeId>;
type BindParts = (BindPairs, BindBodyIds);

/// Recursion-depth budget for runtime evaluation. Since the evaluator grows the
/// native stack on demand (see the `stacker::maybe_grow` wrap in [`Evaluator::eval`]),
/// this no longer protects against stack overflow — it is a *work/memory policy*
/// that turns runaway recursion into a clean error instead of slow OOM. Sized so
/// that legitimately deep recursion (parsers, folds over long lists written
/// recursively) fits with two orders of magnitude to spare.
const DEFAULT_MAX_EVAL_DEPTH: usize = 200_000;
const DEFAULT_RUNTIME_COLLECTION_LIMIT: usize = 1_000_000;

/// Stack headroom watermark for `stacker::maybe_grow`: when less than this
/// remains on the current segment, the continuation runs on a fresh segment of
/// `STACK_GROW_BY` bytes. Values mirror rustc's `ensure_sufficient_stack`.
const STACK_RED_ZONE: usize = 100 * 1024;
const STACK_GROW_BY: usize = 1024 * 1024;

/// Run `f`, growing the native stack first if the red zone is hit. Shared by
/// every deep-recursion site in the crate (eval dispatch, CST→ParsedForm
/// conversion, ParsedForm→IR lowering) so depth limits stay pure work policy.
pub(crate) fn grow_stack<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(STACK_RED_ZONE, STACK_GROW_BY, f)
}

/// Default cumulative evaluation-step budget for a single compile-time fold.
/// Generous enough for any reasonable partial-evaluation reduction, but finite
/// so a non-terminating compile-time computation fails the compile instead of
/// hanging it. See docs/design-partial-evaluation.md (phase 2).
pub const DEFAULT_CTFE_FOLD_STEP_BUDGET: usize = 5_000_000;

/// Maximum call-recursion depth for a single compile-time fold. The step budget
/// bounds *total* work, but a deeply recursive fold also grows the native call
/// stack one frame per level, which would abort the compiler before the step
/// budget trips. This cap is far below the runtime depth limit so a deep
/// recursion simply fails to fold (the call is left to run at runtime) instead
/// of overflowing the stack during compilation.
pub const DEFAULT_CTFE_FOLD_DEPTH_BUDGET: usize = 256;

/// Default cumulative *allocation* budget for a single compile-time fold,
/// counted in collection/string elements (list & map entries, string & bytes
/// chars). Mirrors the step budget: the step budget bounds CPU, but an
/// `O(1)`-step builtin can still allocate `O(limit)` memory (`string_repeat`,
/// `list_of`, …), and a hostile loop chaining these would exhaust host memory
/// — aborting the process uncatchably — before the step budget tripped. The
/// allocation budget bounds that amplification. Generous enough for any real
/// fold (which allocates little), finite so untrusted compile-time code fails
/// cleanly instead of OOM-killing the compiler. Tunable per scope.
pub const DEFAULT_CTFE_FOLD_ALLOC_BUDGET: usize = 64_000_000;

pub struct Evaluator {
    graph: Rc<IRGraph>,
    builtins: Rc<HashMap<String, Rc<BuiltinInfo>>>,
    top_level_names: Vec<String>,
    phase: PhasePolicy,
    effect_scope: Option<EffectSet>,
    eval_depth: Cell<usize>,
    max_eval_depth: usize,
    /// Remaining cumulative evaluation steps when a step budget is active
    /// (`None` = unbounded). Set only for the dynamic extent of a scoped
    /// partial-evaluation reduction via [`Evaluator::with_eval_step_budget`];
    /// bootstrap, providers, and runtime execution stay unbounded.
    eval_step_budget: Cell<Option<usize>>,
    /// Remaining cumulative allocation units (collection/string elements) when
    /// an allocation budget is active (`None` = unbounded). Set for the same
    /// dynamic extent as the step budget (the CTFE fold and any explicit
    /// sandbox); bootstrap and runtime execution stay unbounded. See
    /// [`DEFAULT_CTFE_FOLD_ALLOC_BUDGET`].
    ///
    /// Unlike the step budget, this is `Rc`-shared with sub-evaluators (macro
    /// expansion, `ctfe_eval_node`, cross-graph closure bodies) rather than
    /// copied forward, so the ceiling holds END-TO-END: a hostile fold cannot
    /// reset its own budget by crossing a graph boundary. A memory abort is
    /// uncatchable, so the bound must be hard, not per-evaluator.
    eval_alloc_budget: Rc<Cell<Option<usize>>>,
    runtime_collection_limit: usize,
    lexical_address_cache: HashMap<NodeId, LexicalAddress>,
    assignment_address_cache: HashMap<NodeId, LexicalAddress>,
    /// Tail-call machinery (0 = unarmed). `tail_armed` holds the identity
    /// token of the closure whose tail position is being evaluated RIGHT NOW;
    /// `eval_call` takes it on entry, so only the node literally in tail
    /// position sees it. `handler_tail` carries the taken token across a
    /// special-form handler invocation so the transparent kernel forms
    /// (if/do/bind/match) can re-arm it for THEIR tail subexpressions.
    tail_armed: Cell<usize>,
    handler_tail: Cell<usize>,
    /// Live closure-call stack for `ctfe_debug_frames` — STRICTLY diagnostics
    /// (REPL/tracing), never semantics. Pushed/popped in `eval_call`'s closure
    /// arm only: host-driven callbacks and sub-evaluator bodies don't appear,
    /// and a TCO trampoline shows one collapsed frame.
    diagnostic_frames: Vec<DiagnosticFrame>,
    /// Session cache for `ctfe_kernel_vocabulary` (it materializes several
    /// times per bootstrap otherwise). Shared with sub-evaluators (they share
    /// the registry); invalidated by `register_builtin` — the registry's only
    /// mutation channel. Callers receive a detached copy, so the cached value
    /// cannot be poisoned.
    vocabulary_cache: Rc<RefCell<Option<RuntimeValue>>>,
}

struct DiagnosticFrame {
    name: Option<Rc<str>>,
    node_id: NodeId,
    graph: Rc<IRGraph>,
}

// Special builtin handlers intentionally receive `&mut Evaluator`. Evaluation
// stays single-threaded and phase-ordered, and special builtins may need graph
// access, lazy evaluation, or callback invocation through the current evaluator.
// Eager builtins use the `Vec<RuntimeValue>` dispatch variant.

impl Evaluator {
    pub fn new(graph: IRGraph) -> Self {
        Self::with_top_level_names(graph, Vec::new())
    }

    pub fn with_top_level_names(graph: IRGraph, top_level_names: Vec<String>) -> Self {
        Self::with_top_level_names_and_phase(graph, top_level_names, PhasePolicy::Runtime)
    }

    pub fn with_phase(graph: IRGraph, phase: PhasePolicy) -> Self {
        Self::with_top_level_names_and_phase(graph, Vec::new(), phase)
    }

    pub fn with_top_level_names_and_phase(
        graph: IRGraph,
        top_level_names: Vec<String>,
        phase: PhasePolicy,
    ) -> Self {
        crate::values::increment_evaluator_nesting();
        let mut ev = Self {
            graph: Rc::new(graph),
            builtins: Rc::new(HashMap::new()),
            top_level_names,
            phase,
            effect_scope: None,
            eval_depth: Cell::new(0),
            max_eval_depth: DEFAULT_MAX_EVAL_DEPTH,
            eval_step_budget: Cell::new(None),
            eval_alloc_budget: Rc::new(Cell::new(None)),
            runtime_collection_limit: DEFAULT_RUNTIME_COLLECTION_LIMIT,
            lexical_address_cache: HashMap::new(),
            assignment_address_cache: HashMap::new(),
            tail_armed: Cell::new(0),
            handler_tail: Cell::new(0),
            diagnostic_frames: Vec::new(),
            vocabulary_cache: Rc::new(RefCell::new(None)),
        };
        crate::builtins::register_all(&mut ev);
        ev
    }

    fn with_registered_builtins(
        graph: Rc<IRGraph>,
        top_level_names: Vec<String>,
        builtins: Rc<HashMap<String, Rc<BuiltinInfo>>>,
        phase: PhasePolicy,
        effect_scope: Option<EffectSet>,
        max_eval_depth: usize,
        runtime_collection_limit: usize,
    ) -> Self {
        crate::values::increment_evaluator_nesting();
        Self {
            graph,
            builtins,
            top_level_names,
            phase,
            effect_scope,
            eval_depth: Cell::new(0),
            max_eval_depth,
            eval_step_budget: Cell::new(None),
            eval_alloc_budget: Rc::new(Cell::new(None)),
            runtime_collection_limit,
            lexical_address_cache: HashMap::new(),
            assignment_address_cache: HashMap::new(),
            tail_armed: Cell::new(0),
            handler_tail: Cell::new(0),
            diagnostic_frames: Vec::new(),
            vocabulary_cache: Rc::new(RefCell::new(None)),
        }
    }

    /// Register a builtin by name.
    pub fn register_builtin(&mut self, info: BuiltinInfo) {
        // The registry's only mutation channel — drop the vocabulary cache.
        self.vocabulary_cache.borrow_mut().take();
        Rc::make_mut(&mut self.builtins).insert(info.name.clone(), Rc::new(info));
    }

    /// Register an **eager** builtin: arguments are evaluated before the handler
    /// runs. Collapses the `BuiltinInfo { … }` literal for the common case.
    /// `metadata` stays an explicit parameter so effect/phase classification
    /// remains a deliberate choice per call site, not a hidden default.
    pub fn register_eager(
        &mut self,
        name: impl Into<String>,
        min_arity: usize,
        max_arity: Option<usize>,
        metadata: BuiltinMetadata,
        handler: impl Fn(Vec<RuntimeValue>) -> EvalResult + 'static,
    ) {
        self.register_builtin(BuiltinInfo {
            name: name.into(),
            metadata,
            min_arity,
            max_arity,
            handler: BuiltinHandler::Eager(Box::new(handler)),
        });
    }

    /// Register a **special-form** builtin: the handler receives `(ev, call,
    /// env)` and controls argument evaluation itself (lazy forms, CTFE, control
    /// flow). Same boilerplate collapse as [`Self::register_eager`].
    pub fn register_special(
        &mut self,
        name: impl Into<String>,
        min_arity: usize,
        max_arity: Option<usize>,
        metadata: BuiltinMetadata,
        handler: impl Fn(&mut Evaluator, &CallNode, &EnvRef) -> EvalResult + 'static,
    ) {
        self.register_builtin(BuiltinInfo {
            name: name.into(),
            metadata,
            min_arity,
            max_arity,
            handler: BuiltinHandler::Special(Box::new(handler)),
        });
    }

    pub fn builtin_info(&self, name: &str) -> Option<&BuiltinInfo> {
        self.builtins.get(name).map(Rc::as_ref)
    }

    pub fn builtin_names(&self) -> Vec<&str> {
        let mut names: Vec<_> = self
            .builtins
            .iter()
            .filter_map(|(name, builtin)| builtin.metadata.is_public().then_some(name.as_str()))
            .collect();
        names.sort_unstable();
        names
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

    pub(crate) fn assign_lexical_name(
        &mut self,
        node_id: NodeId,
        env: &EnvRef,
        name: &str,
        value: RuntimeValue,
    ) -> Result<(), crate::values::EvaluationError> {
        let mut value = value;
        if let Some(address) = self.assignment_address_cache.get(&node_id).copied() {
            match Environment::try_assign_address(env, name, address, value)? {
                None => return Ok(()),
                Some(returned) => {
                    self.assignment_address_cache.remove(&node_id);
                    value = returned;
                }
            }
        }

        let address = Environment::assign_resolved(env, name, value)?;
        self.assignment_address_cache.insert(node_id, address);
        Ok(())
    }

    pub fn phase(&self) -> PhasePolicy {
        self.phase
    }

    pub(crate) fn with_effect_scope<F>(&mut self, requested: EffectSet, f: F) -> EvalResult
    where
        F: FnOnce(&mut Evaluator) -> EvalResult,
    {
        if let Some(current) = &self.effect_scope {
            if !requested.is_subset_of(current) {
                return Err(eval_err(format!(
                    "effect-scope cannot grant effects outside the active scope: requested [{}], active [{}]",
                    requested.to_strings().join(", "),
                    current.to_strings().join(", ")
                )));
            }
        }
        let previous = self.effect_scope.replace(requested);
        let result = f(self);
        self.effect_scope = previous;
        result
    }

    pub fn set_max_eval_depth(&mut self, max_eval_depth: usize) {
        self.max_eval_depth = max_eval_depth;
    }

    pub fn max_eval_depth(&self) -> usize {
        self.max_eval_depth
    }

    pub fn runtime_collection_limit(&self) -> usize {
        self.runtime_collection_limit
    }

    pub fn set_runtime_collection_limit(&mut self, runtime_collection_limit: usize) {
        self.runtime_collection_limit = runtime_collection_limit;
    }

    /// Evaluate `f` under a cumulative evaluation-step budget. Every node
    /// evaluated while the budget is active consumes one step; exhausting it
    /// fails with a structured error rather than letting a non-terminating
    /// compile-time computation hang the compiler. The previous budget (if any)
    /// is restored afterwards, so nested reductions are each bounded on their
    /// own. This is the kernel substrate the partial-evaluation fold runs under.
    pub fn with_eval_step_budget<F>(&mut self, budget: usize, f: F) -> EvalResult
    where
        F: FnOnce(&mut Evaluator) -> EvalResult,
    {
        let previous = self.eval_step_budget.replace(Some(budget));
        let result = f(self);
        self.eval_step_budget.set(previous);
        result
    }

    /// Evaluate `f` under a cumulative allocation budget (collection/string
    /// elements). The sibling of [`Self::with_eval_step_budget`]: it bounds
    /// memory growth where the step budget bounds CPU, so untrusted
    /// compile-time code cannot OOM-abort the host. The previous budget is
    /// restored afterwards, so nested sandboxes each bound on their own.
    pub fn with_eval_alloc_budget<F>(&mut self, budget: usize, f: F) -> EvalResult
    where
        F: FnOnce(&mut Evaluator) -> EvalResult,
    {
        let previous = self.eval_alloc_budget.replace(Some(budget));
        let result = f(self);
        self.eval_alloc_budget.set(previous);
        result
    }

    /// Run `f` under the default allocation budget *only if none is already
    /// active*, restoring the previous state afterwards. `effect_scope` uses
    /// this so the kernel's documented untrusted-code boundary is memory-safe:
    /// a hostile `(append …)` / `(string_repeat …)` loop inside a pure scope
    /// fails cleanly instead of OOM-aborting the host. The "only if unbounded"
    /// guard is essential — otherwise untrusted code could nest scopes to reset
    /// its own budget. An already-active (possibly depleted) budget is kept.
    pub fn with_default_alloc_budget_if_unbounded<F>(&mut self, f: F) -> EvalResult
    where
        F: FnOnce(&mut Evaluator) -> EvalResult,
    {
        if self.eval_alloc_budget.get().is_some() {
            return f(self);
        }
        self.with_eval_alloc_budget(DEFAULT_CTFE_FOLD_ALLOC_BUDGET, f)
    }

    /// Charge `units` (collection/string elements) against the active
    /// allocation budget, if one is set. Call this at every builtin that
    /// allocates more than `O(1)` memory per invocation — the same chokepoints
    /// that enforce [`Self::runtime_collection_limit`]. Exhaustion is FATAL: it
    /// pierces `try` exactly like the step budget, so a hostile fold cannot
    /// trap its own bound and keep allocating.
    pub fn charge_allocation(&self, units: usize) -> Result<(), EvalSignal> {
        if let Some(remaining) = self.eval_alloc_budget.get() {
            let Some(left) = remaining.checked_sub(units) else {
                return Err(EvalSignal::Error(
                    crate::values::EvaluationError::new(
                        "compile_time allocation budget exhausted (possible memory_exhaustion attack)",
                    )
                    .into_fatal(),
                ));
            };
            self.eval_alloc_budget.set(Some(left));
        }
        Ok(())
    }

    fn enter_eval(&self) -> Result<(), EvalSignal> {
        // A non-terminating compile-time fold may never grow the call depth
        // (e.g. a constant-depth loop), so the step budget — not the depth
        // guard — is what bounds it. Checked before the depth bump so an
        // exhausted budget returns without leaving `eval_depth` incremented.
        if let Some(remaining) = self.eval_step_budget.get() {
            if remaining == 0 {
                // FATAL: a budget exhaustion must terminate the whole budgeted
                // extent — if `try` could catch it, a hostile fold would trap
                // its own bound and keep running.
                return Err(EvalSignal::Error(
                    crate::values::EvaluationError::new(
                        "compile_time evaluation step budget exhausted (possible non_terminating fold)",
                    )
                    .into_fatal(),
                ));
            }
            self.eval_step_budget.set(Some(remaining - 1));
        }
        let depth = self.eval_depth.get();
        if depth >= self.max_eval_depth {
            return Err(EvalSignal::Error(
                crate::values::EvaluationError::new(format!(
                    "maximum evaluation depth {} exceeded",
                    self.max_eval_depth
                ))
                .into_fatal(),
            ));
        }
        self.eval_depth.set(depth + 1);
        Ok(())
    }

    fn exit_eval(&self) {
        self.eval_depth.set(self.eval_depth.get().saturating_sub(1));
    }

    /// Evaluate a single node in the given environment.
    pub fn eval(&mut self, node_id: NodeId, env: &EnvRef) -> EvalResult {
        self.enter_eval()?;

        // Extract minimal per-variant data within a short borrow, then release.
        // Literal: RuntimeValue computed in-place — no IrLiteralData clone.
        // Name: only the Rc<str> refcount is bumped — no heap alloc.
        // Call: CallNode cloned (Vec<NodeId> alloc — unavoidable with the
        //       current SpecialBuiltinHandler ABI that takes &CallNode).
        enum Dispatch {
            Literal(RuntimeValue),
            Name(Rc<str>),
            Call(CallNode),
        }

        let dispatch = match self.graph.node(node_id) {
            None => {
                self.exit_eval();
                return Err(eval_err(format!("unknown node id: {node_id}")));
            }
            Some(node) => {
                trace_eval_node(&self.graph, node_id, node);
                match node {
                    Node::Literal(lit) => Dispatch::Literal(runtime_value_from_literal(&lit.value)),
                    Node::Name(n) => Dispatch::Name(Rc::clone(&n.identifier)),
                    Node::Call(c) => Dispatch::Call(c.clone()),
                }
            }
        };
        // Compile-time debug hook: pause/step before evaluating this node.
        // `hook_active()` is a thread-local bool load; zero work when no
        // debugger is attached.
        if crate::debug::hook_active() {
            let span = self.graph.source_span(node_id);
            crate::debug::with_hook(|h| h.on_node(node_id, span, &self.graph, env));
        }
        // self.graph borrow fully released — Dispatch contains no lifetimes.

        let result = match dispatch {
            Dispatch::Literal(val) => Ok(val),
            Dispatch::Name(identifier) => self.eval_name(node_id, identifier.as_ref(), env),
            // Calls are the only recursive arm: grow the native stack on demand
            // so depth is bounded by `max_eval_depth` (a work policy), not by
            // the thread's stack size — no RUST_MIN_STACK tuning required.
            Dispatch::Call(call_node) => grow_stack(|| self.eval_call(&call_node, env)),
        };
        self.exit_eval();
        // Notify the debugger of errors (for exception breakpoints). Fires at
        // each frame as the error propagates; the hook pauses only on the first.
        if crate::debug::hook_active() {
            if let Err(EvalSignal::Error(ref error)) = result {
                let message = error.message().to_string();
                let span = self.graph.source_span(node_id);
                crate::debug::with_hook(|h| h.on_error(&message, node_id, span, &self.graph, env));
            }
        }
        result.map_err(|signal| self.annotate_signal(signal, node_id))
    }

    /// Evaluate a sequence of nodes, returning the last value (or Null).
    pub fn eval_sequence(&mut self, ids: &[NodeId], env: &EnvRef) -> EvalResult {
        let mut result = RuntimeValue::Null;
        for &id in ids {
            result = self.eval(id, env)?;
        }
        Ok(result)
    }

    // ── Tail-position evaluation ───────────────────────────────────────────
    //
    // A node evaluated "in tail position" carries a closure-identity token;
    // when that node turns out to be a call of the SAME closure, `eval_call`
    // unwinds to the closure's trampoline instead of recursing. The token is
    // re-armed only by the transparent kernel forms (if / do / bind / match)
    // for their own tail subexpressions, so the signal can never cross a frame
    // that would observe the skipped value (arguments, `try` bodies, custom
    // callables all evaluate unarmed).

    /// Evaluate `node_id` in the tail position of the CURRENT special-form
    /// call. For kernel control-form handlers (if/do/bind/match): their tail
    /// subexpression inherits the tail status of the form itself.
    pub fn eval_in_tail_position(&mut self, node_id: NodeId, env: &EnvRef) -> EvalResult {
        let token = self.handler_tail.get();
        self.eval_with_tail_token(node_id, env, token)
    }

    /// Sequence variant of [`Self::eval_in_tail_position`]: all but the last
    /// node evaluate normally, the last inherits the form's tail status.
    pub fn eval_sequence_in_tail_position(&mut self, ids: &[NodeId], env: &EnvRef) -> EvalResult {
        let token = self.handler_tail.get();
        self.eval_sequence_with_tail_token(ids, env, token)
    }

    fn eval_with_tail_token(&mut self, node_id: NodeId, env: &EnvRef, token: usize) -> EvalResult {
        self.tail_armed.set(token);
        let result = self.eval(node_id, env);
        self.tail_armed.set(0);
        result
    }

    fn eval_sequence_with_tail_token(
        &mut self,
        ids: &[NodeId],
        env: &EnvRef,
        token: usize,
    ) -> EvalResult {
        match ids.split_last() {
            None => Ok(RuntimeValue::Null),
            Some((&last, init)) => {
                for &id in init {
                    self.eval(id, env)?;
                }
                self.eval_with_tail_token(last, env, token)
            }
        }
    }

    fn eval_name(&mut self, node_id: NodeId, name: &str, env: &EnvRef) -> EvalResult {
        match self.eval_name_without_builtin(node_id, name, env) {
            Ok(value) => Ok(value),
            Err(EvalSignal::Error(error)) => self
                .builtins
                .get(name)
                .cloned()
                .filter(|builtin| builtin.metadata.is_public())
                .map(RuntimeValue::Builtin)
                .ok_or(EvalSignal::Error(error)),
            Err(signal) => Err(signal),
        }
    }

    fn eval_name_without_builtin(
        &mut self,
        node_id: NodeId,
        name: &str,
        env: &EnvRef,
    ) -> EvalResult {
        if let Some(address) = self.lexical_address_cache.get(&node_id).copied() {
            match Environment::lookup_address(env, name, address).map_err(EvalSignal::Error)? {
                Some(value) => return Ok(value),
                None => {
                    self.lexical_address_cache.remove(&node_id);
                }
            }
        }

        if let Some((address, value)) =
            Environment::resolve_exact(env, name).map_err(EvalSignal::Error)?
        {
            self.lexical_address_cache.insert(node_id, address);
            return Ok(value);
        }

        if name.contains('.') {
            if let Some(value) = Environment::try_lookup(env, name).map_err(EvalSignal::Error)? {
                return Ok(value);
            }
        }

        Err(EvalSignal::Error(crate::values::EvaluationError::new(
            format!("unknown name: {name}"),
        )))
    }

    /// Evaluate top-level forms with exported scoped bindings.
    pub fn eval_top_level_sequence(&mut self, ids: &[NodeId], env: &EnvRef) -> EvalResult {
        let mut result = RuntimeValue::Null;
        for &id in ids {
            result = self.eval_top_level_form(id, env)?;
        }
        Ok(result)
    }

    fn eval_call(&mut self, call_node: &CallNode, env: &EnvRef) -> EvalResult {
        // Consume the tail-position token: ONLY this call (the node literally
        // in tail position) may act on it — callee/argument evaluation below
        // runs unarmed.
        let tail_token = self.tail_armed.replace(0);
        // Fast path: if callee is a NameNode look it up without cloning the full Node.
        // Only the Rc<str> identifier is cloned (cheap refcount bump).
        let callee_id = call_node.callee;
        let name_opt: Option<Rc<str>> = match self.graph.node(callee_id) {
            None => return Err(eval_err(format!("missing callee node: {callee_id}"))),
            Some(Node::Name(n)) => Some(Rc::clone(&n.identifier)),
            Some(_) => None,
        }; // borrow of self.graph released here

        // Name used to label this call's frame in the debugger call stack.
        let frame_name = name_opt.clone();

        let callee = if let Some(name) = name_opt {
            if self.graph.is_internal_node(callee_id) {
                self.builtins
                    .get(name.as_ref())
                    .cloned()
                    .map(RuntimeValue::Builtin)
                    .ok_or_else(|| eval_err(format!("missing internal builtin: {name}")))?
            } else {
                match self.eval_name_without_builtin(callee_id, name.as_ref(), env) {
                    Ok(value) => value,
                    Err(EvalSignal::Error(error)) => match self.builtins.get(name.as_ref()) {
                        Some(builtin) if builtin.metadata.is_public() => {
                            RuntimeValue::Builtin(Rc::clone(builtin))
                        }
                        Some(builtin) => {
                            return Err(eval_err(format!(
                                "internal builtin {} is not directly callable",
                                builtin.name
                            )));
                        }
                        None => return Err(EvalSignal::Error(error)),
                    },
                    Err(signal) => return Err(signal),
                }
            }
        } else {
            self.eval(callee_id, env)?
        };

        match callee {
            RuntimeValue::Builtin(bi) => {
                self.require_phase("builtin", &bi.name, bi.metadata.phase_policy)?;
                self.require_effect_policy("builtin", &bi.name, &bi.metadata.effect_policy)?;
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
                // Expose this call's tail token to the handler for the
                // duration: the transparent kernel forms (if/do/bind/match)
                // re-arm it for their tail subexpressions; everything else
                // ignores it.
                let previous = self.handler_tail.replace(tail_token);
                let result = bi.handler.invoke(self, call_node, env);
                self.handler_tail.set(previous);
                result
            }
            RuntimeValue::Closure(cl) => {
                // Evaluate arguments in the *call* environment, not the closure's.
                let mut arg_values = Vec::with_capacity(call_node.args.len());
                for &arg_id in call_node.args.iter() {
                    arg_values.push(self.eval(arg_id, env)?);
                }
                // Tail self-call: this call sits in the tail position of the
                // very closure being invoked — unwind to its trampoline
                // instead of recursing (constant evaluation depth).
                if tail_token != 0 && tail_token == Rc::as_ptr(&cl) as usize {
                    return Err(EvalSignal::TailCall(crate::values::TailCallSignal {
                        token: tail_token,
                        args: arg_values,
                    }));
                }
                self.diagnostic_frames.push(DiagnosticFrame {
                    name: frame_name.clone(),
                    node_id: call_node.id,
                    graph: Rc::clone(&self.graph),
                });
                let result = self.invoke_closure(&cl, arg_values, frame_name.as_deref());
                self.diagnostic_frames.pop();
                result
            }
            RuntimeValue::Macro(mac) => self.invoke_macro(&mac, call_node, env),
            RuntimeValue::HostFunction(host) => {
                self.require_phase("host function", &host.name, host.phase_policy)?;
                self.require_effect_policy("host function", &host.name, &host.effect_policy)?;
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
                for &arg_id in call_node.args.iter() {
                    args.push(self.eval(arg_id, env)?);
                }
                (host.handler)(args)
            }
            other => Err(eval_err(format!("value {other} is not callable"))),
        }
    }

    /// Diagnostics snapshot of the live closure-call stack, outermost first:
    /// `(frame name if the callee was a named call, call-site span if known)`.
    /// Strictly diagnostics-class data for `ctfe_debug_frames` — names here
    /// must never feed semantics (alpha-renaming would become observable).
    pub fn diagnostic_frame_snapshot(
        &self,
    ) -> Vec<(Option<Rc<str>>, Option<crate::source::SourceSpan>)> {
        self.diagnostic_frames
            .iter()
            .map(|frame| {
                (
                    frame.name.clone(),
                    frame.graph.source_span(frame.node_id).cloned(),
                )
            })
            .collect()
    }

    /// Session vocabulary cache handle (see the field doc) — for the
    /// `ctfe_kernel_vocabulary` builtin only.
    pub(crate) fn vocabulary_cache(&self) -> &Rc<RefCell<Option<RuntimeValue>>> {
        &self.vocabulary_cache
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
            RuntimeValue::Closure(cl) => self.invoke_closure(cl, args, None),
            RuntimeValue::HostFunction(host) => {
                self.require_phase("host function", &host.name, host.phase_policy)?;
                self.require_effect_policy("host function", &host.name, &host.effect_policy)?;
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
            RuntimeValue::Builtin(builtin) => {
                self.invoke_builtin_with_values(Rc::clone(builtin), args)
            }
            other => Err(eval_err(format!(
                "value {other} is not a callable callback"
            ))),
        }
    }

    fn invoke_closure(
        &mut self,
        closure: &crate::values::ClosureValue,
        args: Vec<RuntimeValue>,
        frame_name: Option<&str>,
    ) -> EvalResult {
        // Trampoline: a tail SELF-call inside the body unwinds back here as a
        // TailCall signal carrying the new arguments; rebinding and looping
        // keeps self-recursive tail loops at constant evaluation depth. The
        // token is the closure's allocation address — unambiguous while
        // `closure` stays borrowed across the whole loop.
        let tail_token = closure as *const crate::values::ClosureValue as usize;
        let mut args = args;
        loop {
            let child_env = Environment::new(Some(Rc::clone(&closure.env)));
            bind_closure_arguments(&child_env, &closure.params, args)?;
            let result = if crate::debug::hook_active() {
                crate::debug::with_hook(|h| h.on_call_enter(frame_name));
                let result = self.eval_closure_body(closure, &child_env, tail_token);
                crate::debug::with_hook(|h| h.on_call_exit());
                result
            } else {
                self.eval_closure_body(closure, &child_env, tail_token)
            };
            match result {
                Err(EvalSignal::TailCall(tail)) if tail.token == tail_token => {
                    args = tail.args;
                }
                other => return other,
            }
        }
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
        if crate::debug::hook_active() {
            crate::debug::with_hook(|h| h.on_call_enter(None));
            let result = self.eval_closure_body(closure, &child_env, 0);
            crate::debug::with_hook(|h| h.on_call_exit());
            result
        } else {
            self.eval_closure_body(closure, &child_env, 0)
        }
    }

    /// `tail_token` ≠ 0 arms the body's last form as a tail position of the
    /// closure with that identity (see `invoke_closure`); 0 = no trampoline.
    fn eval_closure_body(
        &mut self,
        closure: &crate::values::ClosureValue,
        child_env: &EnvRef,
        tail_token: usize,
    ) -> EvalResult {
        self.eval_captured_body(&closure.graph, &closure.body_ids, child_env, tail_token)
    }

    fn invoke_macro(
        &mut self,
        mac: &crate::values::MacroValue,
        call_node: &CallNode,
        call_env: &EnvRef,
    ) -> EvalResult {
        let quoted_args = call_node
            .args
            .iter()
            .map(|&arg_id| self.quote_node(arg_id))
            .collect::<Result<Vec<_>, _>>()?;
        let child_env = Environment::new(Some(Rc::clone(&mac.env)));
        bind_closure_arguments(&child_env, &mac.params, quoted_args)?;
        let expanded = self.eval_captured_body(&mac.graph, &mac.body_ids, &child_env, 0)?;
        let spec = require_expr_spec(&expanded, "macro expansion must return syntax")?;
        self.eval_expansion_spec(spec, call_env)
    }

    fn eval_captured_body(
        &mut self,
        graph: &Rc<IRGraph>,
        body_ids: &[NodeId],
        child_env: &EnvRef,
        tail_token: usize,
    ) -> EvalResult {
        if Rc::ptr_eq(&self.graph, graph) {
            return self.eval_sequence_with_tail_token(body_ids, child_env, tail_token);
        }
        let mut evaluator = Evaluator::with_registered_builtins(
            Rc::clone(graph),
            Vec::new(),
            Rc::clone(&self.builtins),
            self.phase,
            self.effect_scope.clone(),
            self.max_eval_depth,
            self.runtime_collection_limit,
        );
        // Carry the remaining step budget into the sub-evaluator so a scoped
        // compile-time reduction (e.g. folding a call into a closure body that
        // lives in a different graph) stays bounded end-to-end.
        evaluator.eval_step_budget.set(self.eval_step_budget.get());
        // Share (not copy) the allocation budget so the ceiling holds across
        // the graph boundary — see the field doc.
        evaluator.eval_alloc_budget = Rc::clone(&self.eval_alloc_budget);
        evaluator.vocabulary_cache = Rc::clone(&self.vocabulary_cache);
        // The TailCall signal crosses the sub-evaluator boundary as a plain
        // Err value; the trampoline re-enters here with fresh arguments.
        evaluator.eval_sequence_with_tail_token(body_ids, child_env, tail_token)
    }

    fn quote_node(&self, node_id: NodeId) -> Result<RuntimeValue, EvalSignal> {
        let spec = self
            .graph
            .expr_spec_for_subtree(node_id)
            .map_err(|error| eval_err(error.to_string()))?;
        Ok(RuntimeValue::HostObject(Rc::new(ExprSpecBridgeValue::new(
            spec,
        ))))
    }

    /// Evaluate a freshly-constructed [`ExprSpec`] in the current phase, sharing
    /// this evaluator's builtins and the given environment. This is the host-side
    /// of the `ctfe-eval-node` metaprogramming primitive: build IR (e.g. via
    /// `ctfe-ir-instantiate`), then run it at compile time. Identical machinery
    /// to macro expansion.
    pub fn eval_expr_spec(&mut self, spec: ExprSpec, env: &EnvRef) -> EvalResult {
        self.eval_expansion_spec(spec, env)
    }

    fn eval_expansion_spec(&mut self, spec: ExprSpec, call_env: &EnvRef) -> EvalResult {
        let mut graph = IRGraph::new();
        let root_id = graph
            .insert_expr_spec(&spec)
            .map_err(|error| eval_err(error.to_string()))?;
        graph
            .add_top_level_form(root_id)
            .map_err(|error| eval_err(error.to_string()))?;
        let mut evaluator = Evaluator::with_registered_builtins(
            Rc::new(graph),
            Vec::new(),
            Rc::clone(&self.builtins),
            self.phase,
            self.effect_scope.clone(),
            self.max_eval_depth,
            self.runtime_collection_limit,
        );
        evaluator.eval_alloc_budget = Rc::clone(&self.eval_alloc_budget);
        evaluator.vocabulary_cache = Rc::clone(&self.vocabulary_cache);
        evaluator.eval(root_id, call_env)
    }

    fn annotate_signal(&self, signal: EvalSignal, node_id: NodeId) -> EvalSignal {
        match signal {
            EvalSignal::Error(mut error) => {
                error.push_frame(self.runtime_call_frame(node_id));
                EvalSignal::Error(error)
            }
            EvalSignal::Leave(signal) => EvalSignal::Leave(signal),
            EvalSignal::Exception(val) => EvalSignal::Exception(val),
            EvalSignal::TailCall(tail) => EvalSignal::TailCall(tail),
        }
    }

    // Node re-looked-up here — called only on the error path, never on success.
    fn runtime_call_frame(&self, node_id: NodeId) -> RuntimeCallFrame {
        RuntimeCallFrame {
            unit_id: None,
            node_id,
            phase: self.phase,
            name: self
                .graph
                .node(node_id)
                .and_then(|node| self.runtime_frame_name(node)),
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
    ) -> Result<Option<BindParts>, EvalSignal> {
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

    fn flat_literal_bind_parts(&self, call: &CallNode) -> Result<BindParts, EvalSignal> {
        let Some((pair_ids, body_id)) = flat_bind_pairs_and_body(&call.args) else {
            return Err(eval_err(
                "bind expects flat canonical binding pairs and a body",
            ));
        };
        let mut bindings = Vec::new();
        for pair in pair_ids.chunks_exact(2) {
            let name = match self.graph.node(pair[0]) {
                Some(Node::Literal(literal)) => match &literal.value {
                    IrLiteralData::Str(name) if !name.is_empty() => name.clone(),
                    _ => return Err(eval_err("bind canonical names must be non-empty strings")),
                },
                _ => return Err(eval_err("bind canonical names must be string literals")),
            };
            bindings.push((name, pair[1]));
        }
        Ok((bindings, vec![body_id]))
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
                call.args.to_vec()
            }
            _ => {
                let mut ids = Vec::with_capacity(call.args.len() + 1);
                ids.push(call.callee);
                ids.extend_from_slice(&call.args);
                ids
            }
        }
    }

    /// Invoke a builtin with pre-evaluated argument values, reusing the current
    /// builtins table instead of calling `register_all` again.
    fn invoke_builtin_with_values(
        &mut self,
        builtin: Rc<BuiltinInfo>,
        args: Vec<RuntimeValue>,
    ) -> EvalResult {
        self.require_phase("builtin", &builtin.name, builtin.metadata.phase_policy)?;
        self.require_effect_policy("builtin", &builtin.name, &builtin.metadata.effect_policy)?;
        if !builtin.metadata().eager_args() {
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
        builtin.handler.invoke_with_values(&builtin.name, args)
    }

    fn require_phase(
        &self,
        kind: &str,
        name: &str,
        phase_policy: PhasePolicy,
    ) -> Result<(), EvalSignal> {
        if phase_policy == PhasePolicy::Dual || phase_policy == self.phase {
            return Ok(());
        }
        Err(eval_err(format!(
            "{kind} {name} is not available in phase {}",
            self.phase.as_str()
        )))
    }

    fn require_effect_policy(
        &self,
        kind: &str,
        name: &str,
        effect_policy: &EffectPolicy,
    ) -> Result<(), EvalSignal> {
        if effect_policy.is_pure() {
            return Ok(());
        }
        let Some(scope) = &self.effect_scope else {
            return Ok(());
        };
        if effect_policy.is_subset_of(scope) {
            return Ok(());
        }
        Err(eval_err(format!(
            "{kind} {name} requires effect(s) [{}] outside active effect scope [{}]",
            effect_policy.tags().join(", "),
            scope.to_strings().join(", ")
        )))
    }
}

impl BuiltinHandler {
    fn invoke(&self, ev: &mut Evaluator, call: &CallNode, env: &EnvRef) -> EvalResult {
        match self {
            BuiltinHandler::Eager(handler) => handler(eval_args(ev, call, env)?),
            BuiltinHandler::Special(handler) => handler(ev, call, env),
        }
    }

    fn invoke_with_values(&self, name: &str, args: Vec<RuntimeValue>) -> EvalResult {
        match self {
            BuiltinHandler::Eager(handler) => handler(args),
            BuiltinHandler::Special(_) => Err(eval_err(format!(
                "builtin {name} does not support pre-evaluated callback dispatch"
            ))),
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
    std::env::var_os("CAAP_LIVE_TRACE")?;
    if let Ok(filter) = std::env::var("CAAP_LIVE_TRACE_FILTER") {
        let enabled = filter
            .split(',')
            .map(str::trim)
            .any(|needle| matches!(needle, "eval" | "evaluator"));
        if !enabled {
            return None;
        }
    }
    let interval = std::env::var("CAAP_EVAL_TRACE_INTERVAL")
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
    // The rest parameter is declared with a leading '&' (the rest marker) but is
    // bound under its bare name, so the body references it without the '&'
    // (e.g. `(lambda (a &rest) (f rest))` and `(lambda args (f args))`).
    let bare = rest_param[1..].to_string();
    let rest_list = RuntimeValue::List(Rc::new(std::cell::RefCell::new(
        args[required_count..].to_vec(),
    )));
    Environment::define(env, bare, rest_list);
    Ok(())
}

// ── Builtin helper ────────────────────────────────────────────────────────────

/// Evaluate all argument nodes eagerly, returning a Vec.
pub fn eval_args(
    ev: &mut Evaluator,
    call_node: &CallNode,
    env: &EnvRef,
) -> Result<Vec<RuntimeValue>, EvalSignal> {
    let mut args = Vec::with_capacity(call_node.args.len());
    for &id in call_node.args.iter() {
        args.push(ev.eval(id, env)?);
    }
    Ok(args)
}

impl Drop for Evaluator {
    fn drop(&mut self) {
        crate::values::decrement_evaluator_nesting();
    }
}
