use crate::ir::{IrLiteralData, NodeId};
use crate::semantic::{ControlPolicy, EffectPolicy, EvalPolicy, PhasePolicy, ScopePolicy};
use crate::source::SourceSpan;
/// Runtime value types mirroring Python's `values.py`.
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

// ── MapKey ────────────────────────────────────────────────────────────────────

/// Hashable subset of RuntimeValue that can serve as a map key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MapKey {
    Null,
    Bool(bool),
    Int(i64),
    Str(Rc<str>),
}

impl fmt::Display for MapKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MapKey::Null => write!(f, "null"),
            MapKey::Bool(b) => write!(f, "{b}"),
            MapKey::Int(i) => write!(f, "{i}"),
            MapKey::Str(s) => write!(f, "{s}"),
        }
    }
}

impl TryFrom<&RuntimeValue> for MapKey {
    type Error = EvalSignal;
    fn try_from(v: &RuntimeValue) -> Result<Self, EvalSignal> {
        match v {
            RuntimeValue::Null => Ok(MapKey::Null),
            RuntimeValue::Bool(b) => Ok(MapKey::Bool(*b)),
            RuntimeValue::Int(i) => Ok(MapKey::Int(*i)),
            RuntimeValue::Str(s) => Ok(MapKey::Str(Rc::clone(s))),
            other => Err(eval_err(format!(
                "value {other} is not hashable (cannot be used as map key)"
            ))),
        }
    }
}

impl From<MapKey> for RuntimeValue {
    fn from(k: MapKey) -> Self {
        match k {
            MapKey::Null => RuntimeValue::Null,
            MapKey::Bool(b) => RuntimeValue::Bool(b),
            MapKey::Int(i) => RuntimeValue::Int(i),
            MapKey::Str(s) => RuntimeValue::Str(s),
        }
    }
}

// ── RuntimeValue ─────────────────────────────────────────────────────────────

pub type RtList = Rc<RefCell<Vec<RuntimeValue>>>;
pub type RtMap = Rc<RefCell<HashMap<MapKey, RuntimeValue>>>;

/// Host objects are single-threaded by design: runtime values use `Rc`/`RefCell`
/// so evaluation stays cheap and deterministic inside one evaluator thread.
pub trait HostObject: fmt::Debug {
    fn type_name(&self) -> &'static str;
    fn as_any(&self) -> &dyn Any;
}

/// Runtime value for the single-threaded evaluator. This type is intentionally
/// `!Send` and `!Sync`; host integrations should cross thread boundaries via
/// explicit serialized data or host-service APIs, not by sharing live values.
#[derive(Clone, Debug)]
pub enum RuntimeValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    Tuple(Rc<[RuntimeValue]>),
    Closure(Rc<ClosureValue>),
    Builtin(Rc<BuiltinInfo>),
    HostFunction(Rc<HostFunction>),
    HostObject(Rc<dyn HostObject>),
    List(RtList),
    Map(RtMap),
    UninitializedTopLevel,
}

impl PartialEq for RuntimeValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (RuntimeValue::Null, RuntimeValue::Null) => true,
            (RuntimeValue::Bool(a), RuntimeValue::Bool(b)) => a == b,
            (RuntimeValue::Int(a), RuntimeValue::Int(b)) => a == b,
            (RuntimeValue::Float(a), RuntimeValue::Float(b)) => a == b,
            (RuntimeValue::Str(a), RuntimeValue::Str(b)) => a == b,
            (RuntimeValue::Tuple(a), RuntimeValue::Tuple(b)) => a == b,
            (RuntimeValue::Closure(a), RuntimeValue::Closure(b)) => Rc::ptr_eq(a, b),
            (RuntimeValue::Builtin(a), RuntimeValue::Builtin(b)) => Rc::ptr_eq(a, b),
            (RuntimeValue::HostFunction(a), RuntimeValue::HostFunction(b)) => Rc::ptr_eq(a, b),
            (RuntimeValue::HostObject(a), RuntimeValue::HostObject(b)) => Rc::ptr_eq(a, b),
            (RuntimeValue::List(a), RuntimeValue::List(b)) => Rc::ptr_eq(a, b),
            (RuntimeValue::Map(a), RuntimeValue::Map(b)) => Rc::ptr_eq(a, b),
            (RuntimeValue::UninitializedTopLevel, RuntimeValue::UninitializedTopLevel) => true,
            _ => false,
        }
    }
}

impl fmt::Display for RuntimeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeValue::Null => write!(f, "null"),
            RuntimeValue::Bool(b) => write!(f, "{b}"),
            RuntimeValue::Int(i) => write!(f, "{i}"),
            RuntimeValue::Float(v) => write!(f, "{v}"),
            RuntimeValue::Str(s) => write!(f, "{s}"),
            RuntimeValue::Tuple(items) => {
                write!(f, "(")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, ")")
            }
            RuntimeValue::Closure(_) => write!(f, "<closure>"),
            RuntimeValue::Builtin(b) => write!(f, "<builtin:{}>", b.name),
            RuntimeValue::HostFunction(h) => write!(f, "<host-function:{}>", h.name),
            RuntimeValue::HostObject(object) => write!(f, "<host-object:{}>", object.type_name()),
            RuntimeValue::List(l) => {
                let b = l.borrow();
                write!(f, "[")?;
                for (i, v) in b.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            RuntimeValue::Map(m) => {
                let b = m.borrow();
                write!(f, "{{")?;
                for (i, (k, v)) in b.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            RuntimeValue::UninitializedTopLevel => write!(f, "<uninitialized-top-level>"),
        }
    }
}

pub fn is_truthy(v: &RuntimeValue) -> bool {
    match v {
        RuntimeValue::Null => false,
        RuntimeValue::Bool(b) => *b,
        RuntimeValue::Int(i) => *i != 0,
        RuntimeValue::Float(f) => *f != 0.0,
        RuntimeValue::Str(s) => !s.is_empty(),
        RuntimeValue::Tuple(items) => !items.is_empty(),
        RuntimeValue::Closure(_)
        | RuntimeValue::Builtin(_)
        | RuntimeValue::HostFunction(_)
        | RuntimeValue::HostObject(_) => true,
        RuntimeValue::List(l) => !l.borrow().is_empty(),
        RuntimeValue::Map(m) => !m.borrow().is_empty(),
        RuntimeValue::UninitializedTopLevel => false,
    }
}

/// Convert snapshot-safe IR literal data into runtime values recursively.
///
/// Python returns tuple/dict literal payloads directly at runtime.  Rust keeps
/// the same semantic shape with immutable Tuple values and mutable Map values
/// keyed by the IR literal's required string keys.
pub fn runtime_value_from_literal(value: &IrLiteralData) -> RuntimeValue {
    match value {
        IrLiteralData::Null => RuntimeValue::Null,
        IrLiteralData::Bool(b) => RuntimeValue::Bool(*b),
        IrLiteralData::Int(i) => RuntimeValue::Int(*i),
        IrLiteralData::Float(f) => RuntimeValue::Float(*f),
        IrLiteralData::Str(s) => RuntimeValue::Str(s.as_str().into()),
        IrLiteralData::Tuple(items) => RuntimeValue::Tuple(
            items
                .iter()
                .map(runtime_value_from_literal)
                .collect::<Vec<_>>()
                .into(),
        ),
        IrLiteralData::Dict(entries) => {
            let mut map = HashMap::new();
            for (key, item) in entries {
                map.insert(
                    MapKey::Str(key.as_str().into()),
                    runtime_value_from_literal(item),
                );
            }
            RuntimeValue::Map(Rc::new(RefCell::new(map)))
        }
    }
}

/// Require the value to be a string, returning an error with context `ctx` otherwise.
pub fn require_str<'a>(v: &'a RuntimeValue, ctx: &str) -> Result<&'a Rc<str>, EvalSignal> {
    match v {
        RuntimeValue::Str(s) => Ok(s),
        other => Err(eval_err(format!("{ctx}: expected string, got {other}"))),
    }
}

/// Require the value to be a list.
pub fn require_list(v: &RuntimeValue, ctx: &str) -> Result<RtList, EvalSignal> {
    match v {
        RuntimeValue::List(l) => Ok(Rc::clone(l)),
        other => Err(eval_err(format!("{ctx}: expected list, got {other}"))),
    }
}

/// Require the value to be a map.
pub fn require_map(v: &RuntimeValue, ctx: &str) -> Result<RtMap, EvalSignal> {
    match v {
        RuntimeValue::Map(m) => Ok(Rc::clone(m)),
        other => Err(eval_err(format!("{ctx}: expected map, got {other}"))),
    }
}

/// Require the value to be an exact integer (not bool).
pub fn require_int_strict(v: &RuntimeValue, ctx: &str) -> Result<i64, EvalSignal> {
    match v {
        RuntimeValue::Int(i) => Ok(*i),
        RuntimeValue::Bool(_) => Err(eval_err(format!("{ctx}: expected integer, got boolean"))),
        other => Err(eval_err(format!("{ctx}: expected integer, got {other}"))),
    }
}

// ── Environment ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Environment {
    pub parent: Option<EnvRef>,
    pub values: HashMap<String, RuntimeValue>,
}

pub type EnvRef = Rc<RefCell<Environment>>;

impl Environment {
    pub fn new(parent: Option<EnvRef>) -> EnvRef {
        Rc::new(RefCell::new(Environment {
            parent,
            values: HashMap::new(),
        }))
    }

    pub fn define(env: &EnvRef, name: impl Into<String>, value: RuntimeValue) {
        env.borrow_mut().values.insert(name.into(), value);
    }

    pub fn define_uninitialized(env: &EnvRef, name: impl Into<String>) {
        env.borrow_mut()
            .values
            .insert(name.into(), RuntimeValue::UninitializedTopLevel);
    }

    pub fn lookup(env: &EnvRef, name: &str) -> Result<RuntimeValue, EvaluationError> {
        if let Some(value) = Self::lookup_exact(env, name)? {
            return Ok(value);
        }
        if name.contains('.') {
            if let Some(value) = Self::lookup_qualified(env, name)? {
                return Ok(value);
            }
        }
        Err(EvaluationError::new(format!("unknown name: {name}")))
    }

    fn lookup_exact(env: &EnvRef, name: &str) -> Result<Option<RuntimeValue>, EvaluationError> {
        let mut current: Option<EnvRef> = Some(Rc::clone(env));
        while let Some(e) = current {
            let borrow = e.borrow();
            if let Some(v) = borrow.values.get(name) {
                if matches!(v, RuntimeValue::UninitializedTopLevel) {
                    return Err(EvaluationError::new(format!(
                        "name {name:?} was accessed before initialization"
                    )));
                }
                return Ok(Some(v.clone()));
            }
            current = borrow.parent.clone();
        }
        Ok(None)
    }

    fn lookup_qualified(env: &EnvRef, name: &str) -> Result<Option<RuntimeValue>, EvaluationError> {
        for (dot_index, _) in name.match_indices('.').rev() {
            if dot_index == 0 || dot_index + 1 >= name.len() {
                continue;
            }
            let prefix = &name[..dot_index];
            let Some(mut value) = Self::lookup_exact(env, prefix)? else {
                continue;
            };
            let mut resolved = true;
            let mut saw_segment = false;
            for segment in name[dot_index + 1..]
                .split('.')
                .filter(|segment| !segment.is_empty())
            {
                saw_segment = true;
                let RuntimeValue::Map(map) = &value else {
                    resolved = false;
                    break;
                };
                let next = map.borrow().get(&MapKey::Str(segment.into())).cloned();
                let Some(next) = next else {
                    resolved = false;
                    break;
                };
                value = next;
            }
            if resolved && saw_segment {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    /// Mutate an existing binding somewhere in the environment chain.
    pub fn assign(env: &EnvRef, name: &str, value: RuntimeValue) -> Result<(), EvaluationError> {
        let mut current: Option<EnvRef> = Some(Rc::clone(env));
        while let Some(e) = current {
            if e.borrow().values.contains_key(name) {
                e.borrow_mut().values.insert(name.to_string(), value);
                return Ok(());
            }
            let parent = e.borrow().parent.clone();
            current = parent;
        }
        Err(EvaluationError::new(format!("undefined variable: {name}")))
    }
}

// ── ClosureValue ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ClosureValue {
    pub params: Vec<String>,
    pub body_ids: Vec<NodeId>,
    pub env: EnvRef,
    pub graph: Rc<crate::graph::IRGraph>,
}

// ── HostFunction ────────────────────────────────────────────────────────────

pub struct HostFunction {
    pub name: String,
    pub min_arity: usize,
    pub max_arity: Option<usize>,
    /// Single-threaded host callback. It is intentionally not `Send`/`Sync`
    /// because runtime values carry `Rc`/`RefCell` and evaluator execution is
    /// phase-ordered inside one interpreter thread.
    pub handler: Box<dyn Fn(Vec<RuntimeValue>) -> EvalResult>,
}

impl HostFunction {
    pub fn new(
        name: impl Into<String>,
        min_arity: usize,
        max_arity: Option<usize>,
        handler: Box<dyn Fn(Vec<RuntimeValue>) -> EvalResult>,
    ) -> Result<Self, String> {
        let name = name.into();
        if name.is_empty() {
            return Err("host function name must be non-empty".to_string());
        }
        if max_arity.is_some_and(|max| max < min_arity) {
            return Err("host function max arity must be >= min arity".to_string());
        }
        Ok(Self {
            name,
            min_arity,
            max_arity,
            handler,
        })
    }
}

impl fmt::Debug for HostFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HostFunction({})", self.name)
    }
}

// ── BuiltinInfo ──────────────────────────────────────────────────────────────

/// A registered builtin callable.
pub struct BuiltinInfo {
    pub name: String,
    pub metadata: BuiltinMetadata,
    pub min_arity: usize,
    pub max_arity: Option<usize>,
    /// Optional fast path for callbacks that already have evaluated argument values.
    pub eager_handler: Option<Box<dyn Fn(Vec<RuntimeValue>) -> EvalResult>>,
    /// Raw fast-dispatch path: receives the evaluator + call node + env directly.
    pub handler:
        Box<dyn Fn(&mut crate::eval::Evaluator, &crate::ir::CallNode, &EnvRef) -> EvalResult>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltinMetadata {
    pub eval_policy: EvalPolicy,
    pub control_policy: ControlPolicy,
    pub scope_policy: ScopePolicy,
    pub phase_policy: PhasePolicy,
    pub effect_policy: EffectPolicy,
    pub eager_args: bool,
}

impl BuiltinMetadata {
    pub fn eager_runtime() -> Self {
        Self {
            eval_policy: EvalPolicy::Eager,
            control_policy: ControlPolicy::Plain,
            scope_policy: ScopePolicy::None,
            phase_policy: PhasePolicy::Runtime,
            effect_policy: EffectPolicy::pure(),
            eager_args: true,
        }
    }

    pub fn special_form() -> Self {
        Self {
            eval_policy: EvalPolicy::SpecialForm,
            eager_args: false,
            ..Self::eager_runtime()
        }
    }

    pub fn compile_time_pure() -> Self {
        Self::eager_runtime().with_phase_policy(PhasePolicy::CompileTime)
    }

    pub fn compile_time_impure() -> Self {
        Self::compile_time_pure().with_effect("impure")
    }

    pub fn compile_time_write_ir() -> Self {
        Self::compile_time_pure().with_effect("write-ir")
    }

    pub fn compile_time_emit_diagnostics() -> Self {
        Self::compile_time_pure().with_effect("emit-diagnostics")
    }

    pub fn compile_time_compiler_registry() -> Self {
        Self::compile_time_pure().with_effect("compiler-registry")
    }

    pub fn compile_time_special_impure() -> Self {
        Self::eager_runtime()
            .with_eval_policy(EvalPolicy::SpecialForm)
            .with_phase_policy(PhasePolicy::CompileTime)
            .with_effect("impure")
    }

    pub fn runtime_mutation() -> Self {
        Self::eager_runtime().with_effect("mutation")
    }

    pub fn runtime_sequential() -> Self {
        Self::eager_runtime().with_eval_policy(EvalPolicy::Sequential)
    }

    pub fn with_eval_policy(mut self, eval_policy: EvalPolicy) -> Self {
        self.eval_policy = eval_policy;
        self.eager_args = matches!(eval_policy, EvalPolicy::Eager);
        self
    }

    pub fn with_control_policy(mut self, control_policy: ControlPolicy) -> Self {
        self.control_policy = control_policy;
        self
    }

    pub fn with_scope_policy(mut self, scope_policy: ScopePolicy) -> Self {
        self.scope_policy = scope_policy;
        self
    }

    pub fn with_phase_policy(mut self, phase_policy: PhasePolicy) -> Self {
        self.phase_policy = phase_policy;
        self
    }

    pub fn with_effect(mut self, tag: &str) -> Self {
        self.effect_policy = EffectPolicy::single(tag.to_string())
            .expect("builtin metadata effect tags are static and non-empty");
        self
    }
}

impl BuiltinInfo {
    pub fn metadata(&self) -> BuiltinMetadata {
        self.metadata.clone()
    }
}

impl fmt::Debug for BuiltinInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BuiltinInfo({})", self.name)
    }
}

// ── Error / signal types ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCallFrame {
    pub unit_id: Option<String>,
    pub node_id: NodeId,
    pub phase: PhasePolicy,
    pub name: Option<String>,
    pub span: Option<SourceSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluationError {
    message: String,
    frames: Vec<RuntimeCallFrame>,
}

impl EvaluationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            frames: Vec::new(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn frames(&self) -> &[RuntimeCallFrame] {
        &self.frames
    }

    pub fn push_frame(&mut self, frame: RuntimeCallFrame) {
        if self
            .frames
            .last()
            .is_some_and(|last| last.node_id == frame.node_id && last.unit_id == frame.unit_id)
        {
            return;
        }
        self.frames.push(frame);
    }
}

impl fmt::Display for EvaluationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EvaluationError: {}", self.message)?;
        if !self.frames.is_empty() {
            write!(f, "\nRuntime frames:")?;
            for frame in self.frames.iter().rev().take(12) {
                let name = frame.name.as_deref().unwrap_or("<anonymous>");
                if let Some(span) = &frame.span {
                    write!(
                        f,
                        "\n  {name} node={} phase={:?} span={}..{}",
                        frame.node_id, frame.phase, span.start, span.end
                    )?;
                } else {
                    write!(
                        f,
                        "\n  {name} node={} phase={:?}",
                        frame.node_id, frame.phase
                    )?;
                }
            }
        }
        Ok(())
    }
}

impl std::error::Error for EvaluationError {}

/// Non-local exit from a `leave` form — analogous to Python's `LeaveSignal` exception.
#[derive(Debug)]
pub struct LeaveSignal {
    pub target_block_id: NodeId,
    pub value: RuntimeValue,
}

#[derive(Debug)]
pub enum EvalSignal {
    Leave(LeaveSignal),
    Error(EvaluationError),
}

impl fmt::Display for EvalSignal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalSignal::Leave(l) => write!(f, "LeaveSignal(target={})", l.target_block_id),
            EvalSignal::Error(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for EvalSignal {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EvalSignal::Leave(_) => None,
            EvalSignal::Error(error) => Some(error),
        }
    }
}

impl From<EvaluationError> for EvalSignal {
    fn from(e: EvaluationError) -> Self {
        EvalSignal::Error(e)
    }
}

pub type EvalResult = Result<RuntimeValue, EvalSignal>;

pub fn eval_err(msg: impl Into<String>) -> EvalSignal {
    EvalSignal::Error(EvaluationError::new(msg))
}
