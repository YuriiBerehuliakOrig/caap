//! Internal control messages between the DAP loop (main thread) and the
//! evaluation worker / debug controller (eval thread). Only `Send` data crosses
//! these channels — no `Rc`/`RuntimeValue`/`EnvRef` ever leaves the eval thread.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

/// Why evaluation paused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopReason {
    Entry,
    Breakpoint,
    Step,
    Exception,
    FunctionBreakpoint,
    DataBreakpoint,
}

impl StopReason {
    pub fn as_dap(self) -> &'static str {
        match self {
            StopReason::Entry => "entry",
            StopReason::Breakpoint => "breakpoint",
            StopReason::Step => "step",
            StopReason::Exception => "exception",
            StopReason::FunctionBreakpoint => "function breakpoint",
            StopReason::DataBreakpoint => "data breakpoint",
        }
    }
}

/// One call-stack frame, rendered for the DAP `stackTrace` response.
#[derive(Clone, Debug)]
pub struct FrameSnapshot {
    /// Stable id (index from the stack bottom; 0 = root unit frame).
    pub id: i64,
    pub name: String,
    pub file: Option<String>,
    /// 1-based.
    pub line: usize,
    /// 1-based.
    pub col: usize,
}

/// One variable binding, rendered for the DAP `variables` response.
#[derive(Clone, Debug)]
pub struct VarSnapshot {
    pub name: String,
    pub value: String,
    /// RuntimeValue variant name, used as a type/icon hint.
    pub kind: String,
    /// Nonzero when the value is expandable (list/map/tuple); the client can
    /// request its children with another `variables` call using this reference.
    pub variables_reference: i64,
    /// Number of indexed children (list/tuple length) — lets the client page
    /// large collections via `start`/`count`. 0 when not indexable.
    pub indexed_variables: i64,
    /// Number of named children (map size). 0 when not applicable.
    pub named_variables: i64,
}

/// Result of an `evaluate` (watch / repl / hover) request.
#[derive(Clone, Debug)]
pub struct EvalReply {
    pub result: String,
    pub variables_reference: i64,
    pub success: bool,
}

/// A breakpoint as configured by the client, including optional condition,
/// hit condition, and log message (logpoint).
#[derive(Clone, Debug)]
pub struct BreakpointSpec {
    pub line: usize,
    pub condition: Option<String>,
    pub hit_condition: Option<String>,
    pub log_message: Option<String>,
}

/// Events the controller pushes to the DAP loop.
pub enum DapEvent {
    Stopped {
        reason: StopReason,
        file: Option<String>,
        line: usize,
        col: usize,
        frames: Vec<FrameSnapshot>,
    },
    Output {
        category: String,
        text: String,
    },
    Terminated {
        error: Option<String>,
    },
}

/// Commands the DAP loop sends to the controller (it blocks on these while paused).
pub enum DebugCommand {
    Continue,
    StepIn,
    Next,
    StepOut,
    /// Replace the breakpoints for one (canonical) source file.
    SetBreakpoints {
        file: PathBuf,
        breakpoints: Vec<BreakpointSpec>,
    },
    /// Pause when CTFE evaluation raises an error.
    SetExceptionBreak(bool),
    /// Pause when a function with one of these names is called.
    SetFunctionBreakpoints(Vec<String>),
    /// Fetch the last error's message (DAP `exceptionInfo`).
    ExceptionInfo {
        reply: Sender<Option<String>>,
    },
    /// Allocate and return the variables reference for a frame's locals scope.
    Scopes {
        frame_id: i64,
        reply: Sender<i64>,
    },
    /// Expand a previously-handed-out variables reference into its members.
    /// `start`/`count` page indexed children (`count == 0` means all).
    Variables {
        reference: i64,
        start: usize,
        count: usize,
        reply: Sender<Vec<VarSnapshot>>,
    },
    /// Evaluate an expression in the context of a paused frame.
    Evaluate {
        expression: String,
        frame_id: i64,
        reply: Sender<EvalReply>,
    },
    /// Identifier completions visible in a paused frame's scope (debug console).
    Completions {
        frame_id: i64,
        reply: Sender<Vec<String>>,
    },
    /// Assign a new value (an expression) to an existing binding reachable from
    /// a `variablesReference` (a Locals scope or a captured env).
    SetVariable {
        reference: i64,
        name: String,
        value: String,
        reply: Sender<EvalReply>,
    },
    /// Whether `name` (a variable under `reference`, or an expression in
    /// `frame_id`'s scope) can be watched; replies with its dataId (the name or
    /// expression text) or `None`.
    DataBreakpointInfo {
        reference: i64,
        frame_id: Option<i64>,
        name: String,
        reply: Sender<Option<String>>,
    },
    /// Replace the set of watched expressions (data breakpoints): pause when any
    /// of their evaluated values changes. A bare variable name is the trivial
    /// expression.
    SetDataBreakpoints(Vec<String>),
    Disconnect,
}
