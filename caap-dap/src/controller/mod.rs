//! The debug controller: a [`caap_core::debug::DebugHook`] that runs on the
//! evaluation thread, decides when to pause, and exchanges snapshots/commands
//! with the DAP loop over channels.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

use caap_core::debug::DebugHook;
use caap_core::error::CaapError;
use caap_core::eval::Evaluator;
use caap_core::frontend::parse;
use caap_core::graph::IRGraph;
use caap_core::ir::NodeId;
use caap_core::source::SourceSpan;
use caap_core::values::{EnvRef, Environment, RuntimeValue};

use crate::protocol::{
    BreakpointSpec, DapEvent, DebugCommand, EvalReply, FrameSnapshot, StopReason,
};

mod variables;

/// Sentinel panic payload used to unwind out of evaluation when the client
/// disconnects mid-pause. The worker catches exactly this.
pub struct AbortEval;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StepMode {
    Continue,
    StepIn,
    /// Stop on a new line once the stack is at most this deep (step-over).
    Next(usize),
    /// Stop on a new line once the stack is shallower than this (step-out).
    StepOut(usize),
}

/// One logical call frame. `env` is the live environment of the most recent
/// node evaluated while this frame was on top — used to populate the variables
/// panel and to evaluate watch/condition expressions. Not `Send`.
struct Frame {
    name: Option<String>,
    file: Option<String>,
    line: usize,
    col: usize,
    env: Option<EnvRef>,
}

/// A `variablesReference` target, allocated per stop and invalidated on resume.
#[derive(Clone)]
enum Handle {
    /// The locals of frame N (full lexical scope chain).
    Locals(usize),
    /// An environment's own bindings (e.g. a closure's captured scope).
    Env(EnvRef),
    /// A compound runtime value (list/map/tuple) or a callable (closure/macro)
    /// to expand into its members/fields.
    Value(RuntimeValue),
}

pub struct DebugController {
    to_dap: Sender<DapEvent>,
    from_dap: Receiver<DebugCommand>,
    /// When set, stepping/entry pauses only on nodes from this file (the user's
    /// program); breakpoints still fire in any file. Lets dependency/stdlib
    /// compilation run at full speed and keeps stepping in the user's code.
    /// `None` (bootstrap mode) steps anywhere.
    focus: Option<PathBuf>,
    mode: StepMode,
    frames: Vec<Frame>,
    breakpoints: HashMap<PathBuf, Vec<BreakpointSpec>>,
    hit_counts: HashMap<(PathBuf, usize), u64>,
    /// variablesReference table for the current stop (index+1 = reference).
    handles: Vec<Handle>,
    /// Line of the previously evaluated node, to detect "entered a new line".
    prev_line: Option<(String, usize)>,
    /// Whether we have paused at least once (first pause reports "entry").
    stopped_once: bool,
    /// Pause when CTFE evaluation errors.
    exception_break: bool,
    /// Whether we already paused for the in-flight error (avoid re-pausing as
    /// it propagates up the stack).
    error_paused: bool,
    /// The most recent error message (for DAP `exceptionInfo`).
    last_error: Option<String>,
    /// Function names to break on when entered.
    fn_breakpoints: HashSet<String>,
    /// Set on entering a function breakpoint's frame; pauses at its first node.
    break_at_next_node: bool,
    /// Expressions watched as data breakpoints: pause when their evaluated value
    /// changes (a bare variable name is the trivial expression).
    watch_exprs: HashSet<String>,
    /// Last-observed rendered value per watched expression, to detect changes.
    watch_values: HashMap<String, String>,
    /// Memoized parsed graphs for the expressions evaluated on the hot path
    /// (breakpoint conditions, watched data-breakpoint expressions, logpoint
    /// messages). These are re-evaluated on every node / every hit, so parsing
    /// them once and cloning the graph avoids re-lexing on each visit. Keyed by
    /// the expression text; the key set is bounded by the user's configuration.
    parse_cache: RefCell<HashMap<String, IRGraph>>,
    aborted: bool,
}

impl DebugController {
    pub fn new(
        to_dap: Sender<DapEvent>,
        from_dap: Receiver<DebugCommand>,
        stop_on_entry: bool,
        focus: Option<PathBuf>,
    ) -> Self {
        let focus = focus.map(|p| std::fs::canonicalize(&p).unwrap_or(p));
        Self {
            to_dap,
            from_dap,
            focus,
            mode: if stop_on_entry {
                StepMode::StepIn
            } else {
                StepMode::Continue
            },
            frames: vec![Frame {
                name: Some("<bootstrap>".to_string()),
                file: None,
                line: 0,
                col: 0,
                env: None,
            }],
            breakpoints: HashMap::new(),
            hit_counts: HashMap::new(),
            handles: Vec::new(),
            prev_line: None,
            stopped_once: false,
            exception_break: false,
            error_paused: false,
            last_error: None,
            fn_breakpoints: HashSet::new(),
            break_at_next_node: false,
            watch_exprs: HashSet::new(),
            watch_values: HashMap::new(),
            parse_cache: RefCell::new(HashMap::new()),
            aborted: false,
        }
    }

    fn set_breakpoints(&mut self, file: PathBuf, specs: Vec<BreakpointSpec>) {
        // Reset hit counts for this file's breakpoints when they are (re)set.
        self.hit_counts.retain(|(f, _), _| f != &file);
        self.breakpoints.insert(file, specs);
    }

    /// Apply commands that arrived while evaluation was running (not paused).
    fn drain_commands(&mut self) {
        while let Ok(cmd) = self.from_dap.try_recv() {
            match cmd {
                DebugCommand::SetBreakpoints { file, breakpoints } => {
                    self.set_breakpoints(file, breakpoints)
                }
                DebugCommand::SetExceptionBreak(on) => self.exception_break = on,
                DebugCommand::SetFunctionBreakpoints(names) => {
                    self.fn_breakpoints = names.into_iter().collect()
                }
                DebugCommand::SetDataBreakpoints(names) => self.set_data_breakpoints(names),
                DebugCommand::ExceptionInfo { reply } => {
                    let _ = reply.send(self.last_error.clone());
                }
                DebugCommand::Disconnect => self.aborted = true,
                // Inspection requests (Scopes/Variables/Evaluate/Completions/
                // SetVariable/DataBreakpointInfo) only make sense while paused,
                // where the `pause` loop answers them. A well-behaved client
                // never sends them while running; if one arrives, dropping it
                // here also drops its reply sender, so the requester observes a
                // closed channel and returns a default immediately (no hang).
                _ => {}
            }
        }
    }

    fn set_data_breakpoints(&mut self, exprs: Vec<String>) {
        self.watch_exprs = exprs.into_iter().collect();
        // Re-baseline so the first observation after a (re)set never fires.
        self.watch_values.clear();
    }

    fn bp_spec(&self, file: &str, line: usize) -> Option<BreakpointSpec> {
        self.breakpoints
            .get(&PathBuf::from(file))
            .and_then(|specs| specs.iter().find(|s| s.line == line))
            .cloned()
    }

    fn frame_snapshots(&self) -> Vec<FrameSnapshot> {
        self.frames
            .iter()
            .enumerate()
            .rev()
            .map(|(id, f)| FrameSnapshot {
                id: id as i64,
                name: f.name.clone().unwrap_or_else(|| "<fn>".to_string()),
                file: f.file.clone(),
                line: f.line,
                col: f.col,
            })
            .collect()
    }

    fn frame_env(&self, frame_id: i64) -> Option<EnvRef> {
        self.frames
            .get(frame_id as usize)
            .and_then(|f| f.env.clone())
    }

    /// Evaluate `expr` against `env`, memoizing its parsed graph. For
    /// expressions re-evaluated on the hot path (breakpoint conditions, watched
    /// data-breakpoint expressions, logpoint messages) this skips re-lexing on
    /// every node visit; the graph is `Clone`, so each call gets a fresh copy.
    fn eval_cached(&self, env: &EnvRef, expr: &str) -> Result<RuntimeValue, String> {
        let graph = {
            let mut cache = self.parse_cache.borrow_mut();
            match cache.get(expr) {
                Some(graph) => graph.clone(),
                None => {
                    let graph = parse(expr).map_err(|e: CaapError| e.message().to_string())?;
                    cache.insert(expr.to_string(), graph.clone());
                    graph
                }
            }
        };
        eval_graph(graph, env)
    }

    /// Evaluate an expression in a paused frame's environment.
    fn evaluate(&mut self, frame_id: i64, expression: &str) -> EvalReply {
        let Some(env) = self.frame_env(frame_id) else {
            return EvalReply {
                result: "no frame".to_string(),
                variables_reference: 0,
                success: false,
            };
        };
        match evaluate_in_env(&env, expression) {
            Ok(value) => EvalReply {
                result: render_value(&value),
                variables_reference: self.child_reference(&value),
                success: true,
            },
            Err(message) => EvalReply {
                result: message,
                variables_reference: 0,
                success: false,
            },
        }
    }

    /// The environment a `variablesReference` resolves to, for assignment /
    /// watch lookups (a frame's locals scope or a captured-scope env).
    fn reference_env(&self, reference: i64) -> Option<EnvRef> {
        match self.handles.get((reference - 1) as usize) {
            Some(Handle::Locals(frame)) => self.frames.get(*frame).and_then(|f| f.env.clone()),
            Some(Handle::Env(env)) => Some(env.clone()),
            _ => None,
        }
    }

    /// Assign a new value (parsed + evaluated in the relevant scope) to an
    /// existing binding (scope variable) or to a list element / map entry of an
    /// expanded compound value.
    fn set_variable(&mut self, reference: i64, name: &str, value_expr: &str) -> EvalReply {
        let fail = |result: String| EvalReply {
            result,
            variables_reference: 0,
            success: false,
        };
        let handle = self.handles.get((reference - 1) as usize).cloned();

        // Scope variable: assign into its environment.
        if matches!(handle, Some(Handle::Locals(_)) | Some(Handle::Env(_))) {
            let Some(env) = self.reference_env(reference) else {
                return fail("variables reference has no environment".to_string());
            };
            let value = match evaluate_in_env(&env, value_expr) {
                Ok(v) => v,
                Err(message) => return fail(message),
            };
            return match Environment::assign(&env, name, value.clone()) {
                Ok(()) => self.assigned_reply(value),
                Err(err) => fail(err.message().to_string()),
            };
        }

        // Compound element: evaluate the new value in the innermost paused scope,
        // then mutate the list/map in place through its shared cell.
        let Some(env) = self.frames.last().and_then(|f| f.env.clone()) else {
            return fail("no scope to evaluate the value in".to_string());
        };
        let value = match evaluate_in_env(&env, value_expr) {
            Ok(v) => v,
            Err(message) => return fail(message),
        };
        match handle {
            Some(Handle::Value(RuntimeValue::List(items))) => {
                let Some(idx) = parse_index(name) else {
                    return fail(format!("not a list index: {name}"));
                };
                let mut borrow = items.borrow_mut();
                if idx >= borrow.len() {
                    return fail("list index out of range".to_string());
                }
                borrow[idx] = value.clone();
                drop(borrow);
                self.assigned_reply(value)
            }
            Some(Handle::Value(RuntimeValue::Map(entries))) => {
                let key = entries
                    .borrow()
                    .keys()
                    .find(|k| format!("{k}") == name)
                    .cloned();
                let Some(key) = key else {
                    return fail(format!("no such map key: {name}"));
                };
                entries.borrow_mut().insert(key, value.clone());
                self.assigned_reply(value)
            }
            _ => fail("cannot assign here".to_string()),
        }
    }

    /// Build the success reply for an assignment and re-baseline watches so the
    /// edit doesn't immediately re-trigger a data breakpoint that referenced it.
    fn assigned_reply(&mut self, value: RuntimeValue) -> EvalReply {
        self.watch_values.clear();
        EvalReply {
            variables_reference: self.child_reference(&value),
            result: render_value(&value),
            success: true,
        }
    }

    /// Whether `name` is watchable — a binding under `reference`, or an
    /// expression evaluable in `frame_id`'s scope. Its dataId is the text itself.
    fn data_breakpoint_info(
        &self,
        reference: i64,
        frame_id: Option<i64>,
        name: &str,
    ) -> Option<String> {
        if let Some(env) = self.reference_env(reference) {
            if Environment::lookup(&env, name).is_ok() {
                return Some(name.to_string());
            }
        }
        if let Some(env) = frame_id.and_then(|f| self.frame_env(f)) {
            if evaluate_in_env(&env, name).is_ok() {
                return Some(name.to_string());
            }
        }
        None
    }

    /// Evaluate each watched expression in the current node's environment and
    /// compare to its last-seen value; returns `true` (and re-baselines) when one
    /// changed. Expressions not yet evaluable here are skipped.
    fn watch_changed(&mut self, env: &EnvRef) -> bool {
        if self.watch_exprs.is_empty() {
            return false;
        }
        let mut changed = false;
        let exprs: Vec<String> = self.watch_exprs.iter().cloned().collect();
        for expr in exprs {
            let Ok(value) = self.eval_cached(env, &expr) else {
                continue; // not evaluable in this scope yet
            };
            let rendered = render_value(&value);
            match self.watch_values.get(&expr) {
                Some(prev) if prev != &rendered => changed = true,
                _ => {}
            }
            self.watch_values.insert(expr, rendered);
        }
        changed
    }

    /// Block until the client issues a movement command (or disconnects),
    /// answering inspection requests in the meantime.
    fn pause(&mut self, reason: StopReason) {
        // References from the previous stop are now invalid.
        self.handles.clear();
        let Some(top) = self.frames.last() else {
            self.aborted = true;
            let _ = self.to_dap.send(DapEvent::Terminated {
                error: Some("debug controller has no frame to pause".to_string()),
            });
            return;
        };
        let (file, line, col) = (top.file.clone(), top.line, top.col);
        let frames = self.frame_snapshots();
        let _ = self.to_dap.send(DapEvent::Stopped {
            reason,
            file,
            line,
            col,
            frames,
        });
        loop {
            match self.from_dap.recv() {
                Ok(DebugCommand::Continue) => {
                    self.mode = StepMode::Continue;
                    return;
                }
                Ok(DebugCommand::StepIn) => {
                    self.mode = StepMode::StepIn;
                    return;
                }
                Ok(DebugCommand::Next) => {
                    self.mode = StepMode::Next(self.frames.len());
                    return;
                }
                Ok(DebugCommand::StepOut) => {
                    self.mode = StepMode::StepOut(self.frames.len());
                    return;
                }
                Ok(DebugCommand::SetBreakpoints { file, breakpoints }) => {
                    self.set_breakpoints(file, breakpoints);
                }
                Ok(DebugCommand::SetExceptionBreak(on)) => self.exception_break = on,
                Ok(DebugCommand::SetFunctionBreakpoints(names)) => {
                    self.fn_breakpoints = names.into_iter().collect();
                }
                Ok(DebugCommand::SetDataBreakpoints(names)) => self.set_data_breakpoints(names),
                Ok(DebugCommand::DataBreakpointInfo {
                    reference,
                    frame_id,
                    name,
                    reply,
                }) => {
                    let _ = reply.send(self.data_breakpoint_info(reference, frame_id, &name));
                }
                Ok(DebugCommand::SetVariable {
                    reference,
                    name,
                    value,
                    reply,
                }) => {
                    let result = self.set_variable(reference, &name, &value);
                    let _ = reply.send(result);
                }
                Ok(DebugCommand::ExceptionInfo { reply }) => {
                    let _ = reply.send(self.last_error.clone());
                }
                Ok(DebugCommand::Scopes { frame_id, reply }) => {
                    let r = self.scope_reference(frame_id);
                    let _ = reply.send(r);
                }
                Ok(DebugCommand::Variables {
                    reference,
                    start,
                    count,
                    reply,
                }) => {
                    let vars = self.variables(reference, start, count);
                    let _ = reply.send(vars);
                }
                Ok(DebugCommand::Evaluate {
                    expression,
                    frame_id,
                    reply,
                }) => {
                    let result = self.evaluate(frame_id, &expression);
                    let _ = reply.send(result);
                }
                Ok(DebugCommand::Completions { frame_id, reply }) => {
                    let _ = reply.send(self.completions(frame_id));
                }
                Ok(DebugCommand::Disconnect) | Err(_) => {
                    self.aborted = true;
                    return;
                }
            }
        }
    }

    /// Evaluate a breakpoint's condition / hit condition; emit logpoint output.
    /// Returns `true` when execution should pause here.
    fn breakpoint_should_stop(
        &mut self,
        spec: &BreakpointSpec,
        file: &str,
        line: usize,
        env: &EnvRef,
    ) -> bool {
        if let Some(message) = &spec.log_message {
            let text = self.interpolate_log(env, message);
            let _ = self.to_dap.send(DapEvent::Output {
                category: "console".to_string(),
                text: format!("{text}\n"),
            });
            return false; // logpoints never pause
        }
        let count = {
            let entry = self
                .hit_counts
                .entry((PathBuf::from(file), line))
                .or_insert(0);
            *entry += 1;
            *entry
        };
        if let Some(hit) = &spec.hit_condition {
            if !hit_condition_met(hit, count) {
                return false;
            }
        }
        if let Some(cond) = &spec.condition {
            return matches!(self.eval_cached(env, cond), Ok(v) if is_truthy(&v));
        }
        true
    }

    /// Interpolate `{expr}` segments in a logpoint message by evaluating each
    /// against `env` (parses are memoized via [`Self::eval_cached`]).
    fn interpolate_log(&self, env: &EnvRef, message: &str) -> String {
        let mut out = String::new();
        let mut chars = message.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '{' {
                let mut expr = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '}' {
                        closed = true;
                        break;
                    }
                    expr.push(c);
                }
                if closed {
                    match self.eval_cached(env, expr.trim()) {
                        Ok(v) => out.push_str(&render_value(&v)),
                        Err(e) => out.push_str(&format!("<{e}>")),
                    }
                } else {
                    out.push('{');
                    out.push_str(&expr);
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}

impl DebugHook for DebugController {
    fn on_node(
        &mut self,
        _node_id: NodeId,
        span: Option<&SourceSpan>,
        _graph: &IRGraph,
        env: &EnvRef,
    ) {
        self.drain_commands();
        if self.aborted {
            std::panic::panic_any(AbortEval);
        }
        // A node is about to evaluate, so any previously-reported error is past;
        // allow the next error to pause again.
        self.error_paused = false;

        let Some(span) = span else {
            return;
        };
        let file = span.path.clone();
        let line = span.start_line;
        let col = span.start_col;

        if let Some(top) = self.frames.last_mut() {
            top.file = file.clone();
            top.line = line;
            top.col = col;
            top.env = Some(env.clone());
        }

        // Data breakpoints: pause as soon as a watched variable's value changes,
        // anywhere it is visible (independent of line/focus).
        let data_stop = self.watch_changed(env);

        let entered_new_line = match (&self.prev_line, &file) {
            (Some((pf, pl)), Some(f)) => pf != f || *pl != line,
            (None, _) => true,
            (Some(_), None) => false,
        };

        let depth = self.frames.len();
        let mut bp_stop = false;
        if entered_new_line {
            if let Some(file) = &file {
                if let Some(spec) = self.bp_spec(file, line) {
                    bp_stop = self.breakpoint_should_stop(&spec, file, line, env);
                }
            }
        }
        // Stepping/entry only pauses on the focus file (the user's program);
        // dependency/stdlib compilation runs through without stepping.
        let in_focus = match (&self.focus, &file) {
            (Some(focus), Some(f)) => focus.as_path() == Path::new(f),
            (None, Some(_)) => true,
            _ => false,
        };
        let step_stop = entered_new_line
            && in_focus
            && match self.mode {
                StepMode::Continue => false,
                StepMode::StepIn => true,
                StepMode::Next(target) => depth <= target,
                StepMode::StepOut(target) => depth < target,
            };

        if let Some(f) = &file {
            self.prev_line = Some((f.clone(), line));
        }

        // Pause at the first node of a function whose name hit a function
        // breakpoint (armed in on_call_enter).
        let fn_stop = self.break_at_next_node;
        self.break_at_next_node = false;

        if bp_stop || step_stop || fn_stop || data_stop {
            let reason = if data_stop {
                StopReason::DataBreakpoint
            } else if fn_stop {
                StopReason::FunctionBreakpoint
            } else if bp_stop {
                StopReason::Breakpoint
            } else if !self.stopped_once {
                StopReason::Entry
            } else {
                StopReason::Step
            };
            self.stopped_once = true;
            self.pause(reason);
            if self.aborted {
                std::panic::panic_any(AbortEval);
            }
        }
    }

    fn on_call_enter(&mut self, name: Option<&str>) {
        // Arm a function breakpoint: pause at the entered function's first node.
        if let Some(name) = name {
            if self.fn_breakpoints.contains(name) {
                self.break_at_next_node = true;
            }
        }
        self.frames.push(Frame {
            name: name.map(str::to_string),
            file: None,
            line: 0,
            col: 0,
            env: None,
        });
    }

    fn on_call_exit(&mut self) {
        if self.frames.len() > 1 {
            self.frames.pop();
        }
    }

    fn on_error(
        &mut self,
        message: &str,
        _node_id: NodeId,
        span: Option<&SourceSpan>,
        _graph: &IRGraph,
        env: &EnvRef,
    ) {
        // Record the error for `exceptionInfo` regardless; pause only when the
        // user enabled exception breakpoints, and only at the innermost report.
        self.last_error = Some(message.to_string());
        if !self.exception_break || self.error_paused {
            return;
        }
        self.error_paused = true;
        // Anchor the stop at the failing node (and capture its env).
        if let Some(span) = span {
            if let Some(top) = self.frames.last_mut() {
                top.file = span.path.clone();
                top.line = span.start_line;
                top.col = span.start_col;
                top.env = Some(env.clone());
            }
        }
        self.pause(StopReason::Exception);
        if self.aborted {
            std::panic::panic_any(AbortEval);
        }
    }
}

/// Evaluate an already-parsed `graph` against `env` using a fresh evaluator. The
/// thread-local debug hook is re-entrancy-guarded ([`caap_core::debug::with_hook`]
/// uses `try_borrow_mut`), so this nested evaluation does not pause or step.
fn eval_graph(graph: IRGraph, env: &EnvRef) -> Result<RuntimeValue, String> {
    let mut ev = Evaluator::new(graph);
    let ids = ev.graph().top_level_form_ids().to_vec();
    if ids.is_empty() {
        return Ok(RuntimeValue::Null);
    }
    let mut last = RuntimeValue::Null;
    for id in ids {
        last = ev
            .eval(id, env)
            .map_err(|signal| CaapError::from(signal).message().to_string())?;
    }
    Ok(last)
}

/// Parse and evaluate `expr` against `env`. Used for one-shot evaluations
/// (REPL/hover/`setVariable`/watchability probes) where caching the parse is not
/// worthwhile; hot-path callers use [`DebugController::eval_cached`] instead.
fn evaluate_in_env(env: &EnvRef, expr: &str) -> Result<RuntimeValue, String> {
    let graph = parse(expr).map_err(|e: CaapError| e.message().to_string())?;
    eval_graph(graph, env)
}

/// Collect the visible bindings of a frame: the innermost scope plus its
/// enclosing lexical scopes (inner shadows outer), so outer/global bindings
/// like `compiler` are shown, not only the innermost locals. Uninitialized
/// top-level slots are skipped, and the total is capped to stay manageable.
fn collect_scope_bindings(env: &EnvRef) -> Vec<(String, RuntimeValue)> {
    const LIMIT: usize = 250;
    let mut seen = HashSet::new();
    let mut out: Vec<(String, RuntimeValue)> = Vec::new();
    let mut current = Some(env.clone());
    while let Some(scope) = current {
        for (name, value) in Environment::snapshot_bindings(&scope) {
            if matches!(value, RuntimeValue::UninitializedTopLevel) {
                continue;
            }
            if seen.insert(name.clone()) {
                out.push((name, value));
            }
        }
        if out.len() >= LIMIT {
            break;
        }
        current = Environment::parent_env(&scope);
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Parse a `[i]` child label (as produced for list/tuple elements) into its
/// index.
fn parse_index(name: &str) -> Option<usize> {
    name.strip_prefix('[')?.strip_suffix(']')?.parse().ok()
}

/// Number of indexed children (list/tuple length), for client-side paging.
fn indexed_count(value: &RuntimeValue) -> i64 {
    match value {
        RuntimeValue::List(items) => items.borrow().len() as i64,
        RuntimeValue::Tuple(items) => items.len() as i64,
        _ => 0,
    }
}

/// Number of named children (map size).
fn named_count(value: &RuntimeValue) -> i64 {
    match value {
        RuntimeValue::Map(entries) => entries.borrow().len() as i64,
        _ => 0,
    }
}

fn is_truthy(value: &RuntimeValue) -> bool {
    !matches!(value, RuntimeValue::Null | RuntimeValue::Bool(false))
}

fn is_expandable(value: &RuntimeValue) -> bool {
    match value {
        RuntimeValue::List(items) => !items.borrow().is_empty(),
        RuntimeValue::Tuple(items) => !items.is_empty(),
        RuntimeValue::Map(entries) => !entries.borrow().is_empty(),
        // Callables expand into params + captured scope.
        RuntimeValue::Closure(_) | RuntimeValue::Macro(_) => true,
        // Host objects may expose named children (e.g. the compiler bridge's
        // registered values).
        RuntimeValue::HostObject(object) => object.has_debug_children(),
        _ => false,
    }
}

/// A closure/macro's captured (closed-over) bindings: the environment's own
/// frame, minus uninitialized top-level slots, sorted and capped.
fn captured_bindings(env: &EnvRef) -> Vec<(String, RuntimeValue)> {
    const LIMIT: usize = 200;
    let mut out: Vec<(String, RuntimeValue)> = Environment::snapshot_bindings(env)
        .into_iter()
        .filter(|(_, v)| !matches!(v, RuntimeValue::UninitializedTopLevel))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.truncate(LIMIT);
    out
}

fn expand_children(value: &RuntimeValue) -> Vec<(String, RuntimeValue)> {
    match value {
        RuntimeValue::List(items) => items
            .borrow()
            .iter()
            .enumerate()
            .map(|(i, v)| (format!("[{i}]"), v.clone()))
            .collect(),
        RuntimeValue::Tuple(items) => items
            .iter()
            .enumerate()
            .map(|(i, v)| (format!("[{i}]"), v.clone()))
            .collect(),
        RuntimeValue::Map(entries) => {
            let mut pairs: Vec<(String, RuntimeValue)> = entries
                .borrow()
                .iter()
                .map(|(k, v)| (format!("{k}"), v.clone()))
                .collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            pairs
        }
        _ => Vec::new(),
    }
}

/// DAP hit-condition mini-language: `N`, `==N`, `>N`, `>=N`, `%N`.
fn hit_condition_met(spec: &str, count: u64) -> bool {
    let spec = spec.trim();
    let parse = |s: &str| s.trim().parse::<u64>().ok();
    if let Some(rest) = spec.strip_prefix(">=") {
        return parse(rest).is_some_and(|n| count >= n);
    }
    if let Some(rest) = spec.strip_prefix('>') {
        return parse(rest).is_some_and(|n| count > n);
    }
    if let Some(rest) = spec.strip_prefix("==") {
        return parse(rest).is_some_and(|n| count == n);
    }
    if let Some(rest) = spec.strip_prefix('%') {
        return parse(rest)
            .map(|n| n.max(1))
            .is_some_and(|n| count.is_multiple_of(n));
    }
    match parse(spec) {
        Some(n) => count == n,
        None => true,
    }
}

fn render_value(value: &RuntimeValue) -> String {
    // Compact summaries for callables/host objects (the kind is shown in the
    // type column; expanding a closure/macro reveals params + captured scope).
    match value {
        RuntimeValue::Closure(cl) => return format!("λ({})", cl.params.join(" ")),
        RuntimeValue::Macro(m) => return format!("μ({})", m.params.join(" ")),
        RuntimeValue::Builtin(b) => return format!("<builtin {}>", b.name),
        RuntimeValue::HostFunction(h) => return format!("<fn {}>", h.name),
        RuntimeValue::HostObject(object) => return format!("<{}>", object.type_name()),
        _ => {}
    }
    let s = value.to_string();
    let len = s.chars().count();
    if len > 200 {
        let truncated: String = s.chars().take(200).collect();
        format!("{truncated}… ({len} chars)")
    } else {
        s
    }
}

fn value_kind(value: &RuntimeValue) -> &'static str {
    match value {
        RuntimeValue::Null => "null",
        RuntimeValue::Bool(_) => "bool",
        RuntimeValue::Int(_) => "int",
        RuntimeValue::Float(_) => "float",
        RuntimeValue::Str(_) => "string",
        RuntimeValue::Bytes(_) => "bytes",
        RuntimeValue::Tuple(_) => "tuple",
        RuntimeValue::Closure(_) => "closure",
        RuntimeValue::Macro(_) => "macro",
        RuntimeValue::Builtin(_) => "builtin",
        RuntimeValue::HostFunction(_) => "host_fn",
        RuntimeValue::HostObject(_) => "host_object",
        RuntimeValue::List(_) => "list",
        RuntimeValue::Map(_) => "map",
        RuntimeValue::Ref(_) => "ref",
        RuntimeValue::UninitializedTopLevel => "uninitialized",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    use std::sync::mpsc;
    use std::time::Duration;

    use caap_core::frontend::parse_with_source_path;

    const VPATH: &str = "/virtual/test.caap";

    struct Session {
        events: mpsc::Receiver<DapEvent>,
        commands: mpsc::Sender<DebugCommand>,
    }

    impl Session {
        fn start(src: &'static str, stop_on_entry: bool) -> Self {
            let (event_tx, events) = mpsc::channel();
            let (commands, cmd_rx) = mpsc::channel();
            std::thread::spawn(move || {
                let controller = Rc::new(RefCell::new(DebugController::new(
                    event_tx.clone(),
                    cmd_rx,
                    stop_on_entry,
                    None,
                )));
                caap_core::debug::install_hook(controller);
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let graph = parse_with_source_path(src, VPATH).unwrap();
                    let mut ev = Evaluator::new(graph);
                    let _ = ev.run();
                }));
                caap_core::debug::clear_hook();
                let _ = event_tx.send(DapEvent::Terminated {
                    error: outcome.err().map(|_| "panic".to_string()),
                });
            });
            Session { events, commands }
        }

        fn next(&self) -> DapEvent {
            self.events
                .recv_timeout(Duration::from_secs(10))
                .expect("debug event within timeout")
        }

        fn send(&self, cmd: DebugCommand) {
            self.commands.send(cmd).expect("worker alive");
        }
    }

    fn bp(line: usize) -> BreakpointSpec {
        BreakpointSpec {
            line,
            condition: None,
            hit_condition: None,
            log_message: None,
        }
    }

    const PROGRAM: &str = "(do\n  (bind ((a 1)) a)\n  (bind ((b 2)) b)\n  (bind ((c 3)) c))";

    #[test]
    fn step_in_visits_successive_lines() {
        let s = Session::start(PROGRAM, true);
        let mut lines = Vec::new();
        loop {
            match s.next() {
                DapEvent::Stopped { line, .. } => {
                    lines.push(line);
                    assert!(lines.len() < 100, "runaway stepping: {lines:?}");
                    s.send(DebugCommand::StepIn);
                }
                DapEvent::Terminated { error } => {
                    assert!(error.is_none(), "eval errored: {error:?}");
                    break;
                }
                DapEvent::Output { .. } => {}
            }
        }
        for expected in [2usize, 3, 4] {
            assert!(
                lines.contains(&expected),
                "missing line {expected}: {lines:?}"
            );
        }
    }

    #[test]
    fn breakpoint_under_continue_stops_at_line() {
        let s = Session::start(PROGRAM, true);
        assert!(matches!(s.next(), DapEvent::Stopped { .. }));
        s.send(DebugCommand::SetBreakpoints {
            file: PathBuf::from(VPATH),
            breakpoints: vec![bp(3)],
        });
        s.send(DebugCommand::Continue);
        match s.next() {
            DapEvent::Stopped { line, reason, .. } => {
                assert_eq!(line, 3);
                assert_eq!(reason, StopReason::Breakpoint);
            }
            other => panic!("expected breakpoint stop, got {:?}", event_name(&other)),
        }
        s.send(DebugCommand::Continue);
        loop {
            if let DapEvent::Terminated { .. } = s.next() {
                break;
            }
        }
    }

    const PROGRAM_CALL: &str =
        "(do\n  (bind ((f (lambda (n) (int_add n 1))))\n    (do\n      (f 5)\n      (f 6))))";

    #[test]
    fn step_over_skips_closure_body() {
        let s = Session::start(PROGRAM_CALL, true);
        let mut stepped_over = false;
        let mut landed: Option<usize> = None;
        let mut steps = 0;
        loop {
            match s.next() {
                DapEvent::Stopped { line, .. } => {
                    if stepped_over {
                        landed = Some(line);
                        s.send(DebugCommand::Continue);
                    } else if line == 4 {
                        stepped_over = true;
                        s.send(DebugCommand::Next);
                    } else {
                        s.send(DebugCommand::StepIn);
                    }
                    steps += 1;
                    assert!(steps < 100, "runaway stepping");
                }
                DapEvent::Terminated { .. } => break,
                DapEvent::Output { .. } => {}
            }
        }
        assert!(stepped_over, "never reached the call on line 4");
        assert_eq!(landed, Some(5), "step-over should land on line 5");
    }

    #[test]
    fn conditional_breakpoint_stops_only_when_true() {
        // Iterate the body for n in 0..5 via a closure; break only when n == 3.
        let src =
            "(do\n  (bind ((f (lambda (n) (int_add n 0))))\n    (do (f 1) (f 2) (f 3) (f 4))))";
        let s = Session::start(src, true);
        assert!(matches!(s.next(), DapEvent::Stopped { .. })); // entry
        s.send(DebugCommand::SetBreakpoints {
            file: PathBuf::from(VPATH),
            breakpoints: vec![BreakpointSpec {
                line: 2,
                condition: Some("(eq n 3)".to_string()),
                hit_condition: None,
                log_message: None,
            }],
        });
        s.send(DebugCommand::Continue);
        // Should stop with n == 3 in scope.
        match s.next() {
            DapEvent::Stopped { reason, .. } => assert_eq!(reason, StopReason::Breakpoint),
            other => panic!("expected conditional stop, got {:?}", event_name(&other)),
        }
        // Evaluate `n` in the call frame (id 1; id 0 is the bootstrap root) to
        // confirm the condition held.
        let (tx, rx) = mpsc::channel();
        s.send(DebugCommand::Evaluate {
            expression: "n".to_string(),
            frame_id: 1,
            reply: tx,
        });
        let reply = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(reply.success, "evaluate failed: {}", reply.result);
        assert_eq!(reply.result, "3");
        s.send(DebugCommand::Continue);
        loop {
            if let DapEvent::Terminated { .. } = s.next() {
                break;
            }
        }
    }

    #[test]
    fn variables_expand_compound_values() {
        // `xs` is bound on line 1 but only in scope from the body (lines 2+).
        let src = "(bind ((xs (list_of 10 20 30)))\n  (do\n    xs\n    xs))";
        let s = Session::start(src, true);
        let mut found_list = false;
        for _ in 0..30 {
            match s.next() {
                DapEvent::Stopped { frames, .. } => {
                    let fid = frames.first().unwrap().id;
                    let (tx, rx) = mpsc::channel();
                    s.send(DebugCommand::Scopes {
                        frame_id: fid,
                        reply: tx,
                    });
                    let scope_ref = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    let (tx, rx) = mpsc::channel();
                    s.send(DebugCommand::Variables {
                        reference: scope_ref,
                        start: 0,
                        count: 0,
                        reply: tx,
                    });
                    let vars = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    if let Some(xs) = vars
                        .iter()
                        .find(|v| v.name == "xs" && v.variables_reference != 0)
                    {
                        let (tx, rx) = mpsc::channel();
                        s.send(DebugCommand::Variables {
                            reference: xs.variables_reference,
                            start: 0,
                            count: 0,
                            reply: tx,
                        });
                        let items = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        assert_eq!(items.len(), 3, "list has 3 items: {items:?}");
                        assert_eq!(items[0].value, "10");
                        found_list = true;
                        s.send(DebugCommand::Continue);
                        break;
                    }
                    s.send(DebugCommand::StepIn);
                }
                DapEvent::Terminated { .. } => break,
                DapEvent::Output { .. } => {}
            }
        }
        assert!(found_list, "never observed `xs` as an expandable list");
    }

    #[test]
    fn function_breakpoint_stops_on_call() {
        let src = "(do\n  (bind ((f (lambda (n) (int_add n 1)))) (f 5)))";
        let s = Session::start(src, true);
        assert!(matches!(s.next(), DapEvent::Stopped { .. })); // entry
        s.send(DebugCommand::SetFunctionBreakpoints(vec!["f".to_string()]));
        s.send(DebugCommand::Continue);
        match s.next() {
            DapEvent::Stopped { reason, .. } => {
                assert_eq!(reason, StopReason::FunctionBreakpoint)
            }
            other => panic!(
                "expected function-breakpoint stop, got {:?}",
                event_name(&other)
            ),
        }
        s.send(DebugCommand::Continue);
        loop {
            if let DapEvent::Terminated { .. } = s.next() {
                break;
            }
        }
    }

    #[test]
    fn exception_breakpoint_stops_on_error() {
        // `nope` is unbound → evaluation errors.
        let src = "(do\n  (int_add 1 nope))";
        let s = Session::start(src, true);
        assert!(matches!(s.next(), DapEvent::Stopped { .. })); // entry
        s.send(DebugCommand::SetExceptionBreak(true));
        s.send(DebugCommand::Continue);
        match s.next() {
            DapEvent::Stopped { reason, .. } => assert_eq!(reason, StopReason::Exception),
            other => panic!("expected exception stop, got {:?}", event_name(&other)),
        }
        // exceptionInfo carries the error message.
        let (tx, rx) = mpsc::channel();
        s.send(DebugCommand::ExceptionInfo { reply: tx });
        let info = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            info.is_some_and(|m| !m.is_empty()),
            "exception info has a message"
        );
        s.send(DebugCommand::Continue);
        loop {
            if let DapEvent::Terminated { .. } = s.next() {
                break;
            }
        }
    }

    #[test]
    fn set_variable_assigns_in_scope() {
        // `x` is in scope across the body; edit it during a pause and confirm.
        let src = "(bind ((x 1))\n  (do\n    x\n    x))";
        let s = Session::start(src, true);
        let mut done = false;
        for _ in 0..40 {
            match s.next() {
                DapEvent::Stopped { frames, .. } => {
                    let fid = frames.first().unwrap().id;
                    let (tx, rx) = mpsc::channel();
                    s.send(DebugCommand::Scopes {
                        frame_id: fid,
                        reply: tx,
                    });
                    let scope_ref = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    let (tx, rx) = mpsc::channel();
                    s.send(DebugCommand::Variables {
                        reference: scope_ref,
                        start: 0,
                        count: 0,
                        reply: tx,
                    });
                    let vars = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    if vars.iter().any(|v| v.name == "x") {
                        let (tx, rx) = mpsc::channel();
                        s.send(DebugCommand::SetVariable {
                            reference: scope_ref,
                            name: "x".to_string(),
                            value: "42".to_string(),
                            reply: tx,
                        });
                        let reply = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        assert!(reply.success, "set failed: {}", reply.result);
                        assert_eq!(reply.result, "42");
                        // The new value is observable via evaluate in the frame.
                        let (tx, rx) = mpsc::channel();
                        s.send(DebugCommand::Evaluate {
                            expression: "x".to_string(),
                            frame_id: fid,
                            reply: tx,
                        });
                        let ev = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        assert_eq!(ev.result, "42", "edited value should persist");
                        done = true;
                        s.send(DebugCommand::Continue);
                        break;
                    }
                    s.send(DebugCommand::StepIn);
                }
                DapEvent::Terminated { .. } => break,
                DapEvent::Output { .. } => {}
            }
        }
        assert!(done, "never observed `x` in a locals scope");
        loop {
            if let DapEvent::Terminated { .. } = s.next() {
                break;
            }
        }
    }

    #[test]
    fn data_breakpoint_stops_when_value_changes() {
        // `x` starts at 1, then `set!` changes it to 2 — the watch should fire.
        let src = "(bind ((x 1))\n  (do\n    (set! x 2)\n    x))";
        let s = Session::start(src, true);
        assert!(matches!(s.next(), DapEvent::Stopped { .. })); // entry
        s.send(DebugCommand::SetDataBreakpoints(vec!["x".to_string()]));
        s.send(DebugCommand::Continue);
        match s.next() {
            DapEvent::Stopped { reason, .. } => assert_eq!(reason, StopReason::DataBreakpoint),
            other => panic!(
                "expected data-breakpoint stop, got {:?}",
                event_name(&other)
            ),
        }
        s.send(DebugCommand::Continue);
        loop {
            if let DapEvent::Terminated { .. } = s.next() {
                break;
            }
        }
    }

    #[test]
    fn data_breakpoint_on_expression_stops_on_change() {
        // Watch a compound expression, not a bare name: it changes when `x` does.
        let src = "(bind ((x 1))\n  (do\n    (set! x 2)\n    (int_add x 0)))";
        let s = Session::start(src, true);
        assert!(matches!(s.next(), DapEvent::Stopped { .. })); // entry
        s.send(DebugCommand::SetDataBreakpoints(vec![
            "(int_add x 1)".to_string()
        ]));
        s.send(DebugCommand::Continue);
        match s.next() {
            DapEvent::Stopped { reason, .. } => assert_eq!(reason, StopReason::DataBreakpoint),
            other => panic!(
                "expected data-breakpoint stop, got {:?}",
                event_name(&other)
            ),
        }
        s.send(DebugCommand::Continue);
        loop {
            if let DapEvent::Terminated { .. } = s.next() {
                break;
            }
        }
    }

    #[test]
    fn set_variable_edits_list_element() {
        let src = "(bind ((xs (list_of 10 20 30)))\n  (do\n    xs\n    xs))";
        let s = Session::start(src, true);
        let mut done = false;
        for _ in 0..40 {
            match s.next() {
                DapEvent::Stopped { frames, .. } => {
                    let fid = frames.first().unwrap().id;
                    let (tx, rx) = mpsc::channel();
                    s.send(DebugCommand::Scopes {
                        frame_id: fid,
                        reply: tx,
                    });
                    let scope_ref = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    let (tx, rx) = mpsc::channel();
                    s.send(DebugCommand::Variables {
                        reference: scope_ref,
                        start: 0,
                        count: 0,
                        reply: tx,
                    });
                    let vars = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    if let Some(xs) = vars
                        .iter()
                        .find(|v| v.name == "xs" && v.variables_reference != 0)
                    {
                        // Edit element [0] in place.
                        let (tx, rx) = mpsc::channel();
                        s.send(DebugCommand::SetVariable {
                            reference: xs.variables_reference,
                            name: "[0]".to_string(),
                            value: "99".to_string(),
                            reply: tx,
                        });
                        let reply = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        assert!(reply.success, "set failed: {}", reply.result);
                        assert_eq!(reply.result, "99");
                        let (tx, rx) = mpsc::channel();
                        s.send(DebugCommand::Variables {
                            reference: xs.variables_reference,
                            start: 0,
                            count: 0,
                            reply: tx,
                        });
                        let items = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        assert_eq!(items[0].value, "99", "element should be edited: {items:?}");
                        done = true;
                        s.send(DebugCommand::Continue);
                        break;
                    }
                    s.send(DebugCommand::StepIn);
                }
                DapEvent::Terminated { .. } => break,
                DapEvent::Output { .. } => {}
            }
        }
        assert!(done, "never observed `xs` as an expandable list");
        loop {
            if let DapEvent::Terminated { .. } = s.next() {
                break;
            }
        }
    }

    #[test]
    fn variables_paginate_large_list() {
        let src = "(bind ((xs (list_of 0 1 2 3 4 5 6 7 8 9)))\n  (do\n    xs\n    xs))";
        let s = Session::start(src, true);
        let mut done = false;
        for _ in 0..40 {
            match s.next() {
                DapEvent::Stopped { frames, .. } => {
                    let fid = frames.first().unwrap().id;
                    let (tx, rx) = mpsc::channel();
                    s.send(DebugCommand::Scopes {
                        frame_id: fid,
                        reply: tx,
                    });
                    let scope_ref = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    let (tx, rx) = mpsc::channel();
                    s.send(DebugCommand::Variables {
                        reference: scope_ref,
                        start: 0,
                        count: 0,
                        reply: tx,
                    });
                    let vars = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    if let Some(xs) = vars
                        .iter()
                        .find(|v| v.name == "xs" && v.variables_reference != 0)
                    {
                        assert_eq!(xs.indexed_variables, 10, "parent reports element count");
                        // Page a window of the elements.
                        let (tx, rx) = mpsc::channel();
                        s.send(DebugCommand::Variables {
                            reference: xs.variables_reference,
                            start: 3,
                            count: 4,
                            reply: tx,
                        });
                        let page = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        let got: Vec<(&str, &str)> = page
                            .iter()
                            .map(|v| (v.name.as_str(), v.value.as_str()))
                            .collect();
                        assert_eq!(
                            got,
                            vec![("[3]", "3"), ("[4]", "4"), ("[5]", "5"), ("[6]", "6")],
                            "paged window with absolute indices"
                        );
                        done = true;
                        s.send(DebugCommand::Continue);
                        break;
                    }
                    s.send(DebugCommand::StepIn);
                }
                DapEvent::Terminated { .. } => break,
                DapEvent::Output { .. } => {}
            }
        }
        assert!(done, "never observed `xs` as an expandable list");
        loop {
            if let DapEvent::Terminated { .. } = s.next() {
                break;
            }
        }
    }

    #[test]
    fn completions_list_frame_bindings() {
        let src = "(bind ((foo 1) (bar 2))\n  (do foo bar))";
        let s = Session::start(src, true);
        let mut done = false;
        for _ in 0..40 {
            match s.next() {
                DapEvent::Stopped { frames, .. } => {
                    let fid = frames.first().unwrap().id;
                    let (tx, rx) = mpsc::channel();
                    s.send(DebugCommand::Completions {
                        frame_id: fid,
                        reply: tx,
                    });
                    let names = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    if names.iter().any(|n| n == "foo") {
                        assert!(names.iter().any(|n| n == "bar"), "both bindings: {names:?}");
                        done = true;
                        s.send(DebugCommand::Continue);
                        break;
                    }
                    s.send(DebugCommand::StepIn);
                }
                DapEvent::Terminated { .. } => break,
                DapEvent::Output { .. } => {}
            }
        }
        assert!(done, "completions never included `foo`");
        loop {
            if let DapEvent::Terminated { .. } = s.next() {
                break;
            }
        }
    }

    fn event_name(e: &DapEvent) -> &'static str {
        match e {
            DapEvent::Stopped { .. } => "stopped",
            DapEvent::Output { .. } => "output",
            DapEvent::Terminated { .. } => "terminated",
        }
    }
}
