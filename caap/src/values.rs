use crate::error::{CaapError, CaapResult};
use crate::ir::{IrLiteralData, NodeId};
use crate::semantic::{
    BuiltinEffectTag, ControlPolicy, EffectPolicy, EvalPolicy, FoldPolicy, PhasePolicy, ScopePolicy,
};
use crate::source::SourceSpan;
use indexmap::IndexMap;
/// Runtime value types.
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::rc::{Rc, Weak};

thread_local! {
    static TRACKED_ENVIRONMENTS: RefCell<Vec<Weak<RefCell<Environment>>>> = const { RefCell::new(Vec::new()) };
    static EVALUATOR_NESTING: Cell<usize> = const { Cell::new(0) };
}

// ── MapKey ────────────────────────────────────────────────────────────────────

/// Hashable subset of RuntimeValue that can serve as a map key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
pub type RtMap = Rc<RefCell<IndexMap<MapKey, RuntimeValue>>>;
/// A first-class mutable reference cell ("box") to any runtime value. Created by
/// `ref`, read by `deref`, written by `set_ref`. Two refs to the same cell alias
/// (a write through one is seen through the other); equality is cell identity.
pub type RtRef = Rc<RefCell<RuntimeValue>>;

/// The map's deterministic iteration order — INSERTION order (the backing
/// store is an IndexMap). `map_keys`/`map_values` and every walk follow
/// construction order, so e.g. struct fields keep their declared order
/// without sorting. (Until 2026-06 this sorted by key; insertion order is
/// strictly more informative and equally deterministic.)
pub fn ordered_runtime_map_entries(
    map: &IndexMap<MapKey, RuntimeValue>,
) -> Vec<(&MapKey, &RuntimeValue)> {
    map.iter().collect()
}

/// Host objects are single-threaded by design: runtime values use `Rc`/`RefCell`
/// so evaluation stays cheap and deterministic inside one evaluator thread.
pub trait HostObject: fmt::Debug {
    fn type_name(&self) -> &'static str;
    fn as_any(&self) -> &dyn Any;

    /// Named child values for inspection in a debugger's variables panel
    /// (e.g. the compiler bridge exposes its registered values). Default: none.
    fn debug_children(&self) -> Vec<(String, RuntimeValue)> {
        Vec::new()
    }

    /// Cheap check (no value cloning) of whether [`HostObject::debug_children`]
    /// would return anything — used to decide expandability.
    fn has_debug_children(&self) -> bool {
        false
    }
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
    /// Immutable binary blob. Mirrors `Str` (shared, immutable) but holds
    /// arbitrary bytes rather than UTF-8 text.
    Bytes(Rc<[u8]>),
    Tuple(Rc<[RuntimeValue]>),
    Closure(Rc<ClosureValue>),
    Macro(Rc<MacroValue>),
    Builtin(Rc<BuiltinInfo>),
    HostFunction(Rc<HostFunction>),
    HostObject(Rc<dyn HostObject>),
    List(RtList),
    Map(RtMap),
    /// Mutable reference cell ([`RtRef`]). Shared, aliasing, mutate-in-place.
    Ref(RtRef),
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
            (RuntimeValue::Bytes(a), RuntimeValue::Bytes(b)) => a == b,
            (RuntimeValue::Tuple(a), RuntimeValue::Tuple(b)) => a == b,
            (RuntimeValue::Closure(a), RuntimeValue::Closure(b)) => Rc::ptr_eq(a, b),
            (RuntimeValue::Macro(a), RuntimeValue::Macro(b)) => Rc::ptr_eq(a, b),
            (RuntimeValue::Builtin(a), RuntimeValue::Builtin(b)) => Rc::ptr_eq(a, b),
            (RuntimeValue::HostFunction(a), RuntimeValue::HostFunction(b)) => Rc::ptr_eq(a, b),
            (RuntimeValue::HostObject(a), RuntimeValue::HostObject(b)) => Rc::ptr_eq(a, b),
            (RuntimeValue::List(a), RuntimeValue::List(b)) => Rc::ptr_eq(a, b),
            (RuntimeValue::Map(a), RuntimeValue::Map(b)) => Rc::ptr_eq(a, b),
            // Reference equality is cell identity (same cell), not contents.
            (RuntimeValue::Ref(a), RuntimeValue::Ref(b)) => Rc::ptr_eq(a, b),
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
            RuntimeValue::Bytes(b) => write!(f, "#bytes({})", b.len()),
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
            RuntimeValue::Macro(_) => write!(f, "<macro>"),
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
                for (i, (k, v)) in ordered_runtime_map_entries(&b).into_iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            // Opaque on purpose: printing the inner value would recurse on cyclic
            // refs and make output address/identity dependent (breaks determinism).
            RuntimeValue::Ref(_) => write!(f, "<ref>"),
            RuntimeValue::UninitializedTopLevel => write!(f, "<uninitialized-top-level>"),
        }
    }
}

/// Canonical type tag for any value — the single source of truth shared by the
/// `value_type` builtin and type-error messages ("callable" covers every
/// function flavour).
pub fn canonical_type_tag(value: &RuntimeValue) -> &'static str {
    match value {
        RuntimeValue::Null => "null",
        RuntimeValue::Bool(_) => "bool",
        RuntimeValue::Int(_) => "int",
        RuntimeValue::Float(_) => "float",
        RuntimeValue::Str(_) => "string",
        RuntimeValue::Bytes(_) => "bytes",
        RuntimeValue::List(_) => "list",
        RuntimeValue::Tuple(_) => "tuple",
        RuntimeValue::Map(_) => "map",
        RuntimeValue::Closure(_)
        | RuntimeValue::Macro(_)
        | RuntimeValue::Builtin(_)
        | RuntimeValue::HostFunction(_) => "callable",
        RuntimeValue::HostObject(_) => "object",
        RuntimeValue::Ref(_) => "ref",
        RuntimeValue::UninitializedTopLevel => "null",
    }
}

pub fn is_truthy(v: &RuntimeValue) -> bool {
    match v {
        RuntimeValue::Null => false,
        RuntimeValue::Bool(b) => *b,
        RuntimeValue::Int(i) => *i != 0,
        RuntimeValue::Float(f) => *f != 0.0,
        RuntimeValue::Str(s) => !s.is_empty(),
        RuntimeValue::Bytes(b) => !b.is_empty(),
        RuntimeValue::Tuple(items) => !items.is_empty(),
        RuntimeValue::Closure(_)
        | RuntimeValue::Macro(_)
        | RuntimeValue::Builtin(_)
        | RuntimeValue::HostFunction(_)
        | RuntimeValue::HostObject(_) => true,
        RuntimeValue::List(l) => !l.borrow().is_empty(),
        RuntimeValue::Map(m) => !m.borrow().is_empty(),
        // A reference is always a live cell (truthy); deref to test its contents.
        RuntimeValue::Ref(_) => true,
        RuntimeValue::UninitializedTopLevel => false,
    }
}

/// Convert snapshot-safe IR literal data into runtime values recursively.
///
/// Runtime lowering preserves tuple/dict literal payloads with immutable Tuple values and mutable Map values
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
            let mut map = IndexMap::new();
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

/// Require the value to be a reference cell.
pub fn require_ref(v: &RuntimeValue, ctx: &str) -> Result<RtRef, EvalSignal> {
    match v {
        RuntimeValue::Ref(r) => Ok(Rc::clone(r)),
        other => Err(eval_err(format!("{ctx}: expected ref, got {other}"))),
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
    parent: Option<EnvRef>,
    bindings: HashMap<String, usize>,
    slots: Vec<RuntimeValue>,
}

pub type EnvRef = Rc<RefCell<Environment>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LexicalAddress {
    pub depth: usize,
    pub slot: usize,
}

impl Environment {
    pub fn new(parent: Option<EnvRef>) -> EnvRef {
        let env = Rc::new(RefCell::new(Environment {
            parent,
            bindings: HashMap::new(),
            slots: Vec::new(),
        }));
        TRACKED_ENVIRONMENTS.with(|tracked| {
            tracked.borrow_mut().push(Rc::downgrade(&env));
        });
        env
    }

    pub fn define(env: &EnvRef, name: impl Into<String>, value: RuntimeValue) {
        env.borrow_mut().define_local(name.into(), value);
    }

    pub fn define_uninitialized(env: &EnvRef, name: impl Into<String>) {
        env.borrow_mut()
            .define_local(name.into(), RuntimeValue::UninitializedTopLevel);
    }

    /// Drop every binding in this scope. Used to break a deliberate `letrec`
    /// reference cycle (closures whose `env` is the very scope that holds them)
    /// once it is no longer needed, so the scope and its closures can be freed
    /// instead of leaking.
    pub fn clear(env: &EnvRef) {
        let mut env = env.borrow_mut();
        env.bindings.clear();
        env.slots.clear();
    }

    pub fn lookup(env: &EnvRef, name: &str) -> Result<RuntimeValue, EvaluationError> {
        Self::try_lookup(env, name)?
            .ok_or_else(|| EvaluationError::new(format!("unknown name: {name}")))
    }

    pub fn try_lookup(env: &EnvRef, name: &str) -> Result<Option<RuntimeValue>, EvaluationError> {
        if let Some((_, value)) = Self::resolve_exact(env, name)? {
            return Ok(Some(value));
        }
        if name.contains('.') {
            if let Some(value) = Self::lookup_qualified(env, name)? {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    pub fn resolve_exact(
        env: &EnvRef,
        name: &str,
    ) -> Result<Option<(LexicalAddress, RuntimeValue)>, EvaluationError> {
        let mut current: Option<EnvRef> = Some(Rc::clone(env));
        let mut depth = 0;
        while let Some(e) = current {
            let borrow = e.borrow();
            if let Some(&slot) = borrow.bindings.get(name) {
                let value = checked_slot_value(&borrow, name, slot)?;
                return Ok(Some((LexicalAddress { depth, slot }, value)));
            }
            current = borrow.parent.clone();
            depth += 1;
        }
        Ok(None)
    }

    pub fn lookup_address(
        env: &EnvRef,
        name: &str,
        address: LexicalAddress,
    ) -> Result<Option<RuntimeValue>, EvaluationError> {
        let mut current = Rc::clone(env);
        for _ in 0..address.depth {
            let borrow = current.borrow();
            if borrow.bindings.contains_key(name) {
                return Ok(None);
            }
            let Some(parent) = borrow.parent.clone() else {
                return Ok(None);
            };
            drop(borrow);
            current = parent;
        }

        let borrow = current.borrow();
        if borrow.bindings.get(name).copied() != Some(address.slot) {
            return Ok(None);
        }
        checked_slot_value(&borrow, name, address.slot).map(Some)
    }

    pub fn snapshot_bindings(env: &EnvRef) -> Vec<(String, RuntimeValue)> {
        let borrow = env.borrow();
        borrow
            .bindings
            .iter()
            .filter_map(|(name, &slot)| borrow.slots.get(slot).map(|v| (name.clone(), v.clone())))
            .collect()
    }

    /// The enclosing (parent) environment, if any. Lets a debugger walk the
    /// lexical scope chain to present outer-scope variables.
    pub fn parent_env(env: &EnvRef) -> Option<EnvRef> {
        env.borrow().parent.clone()
    }

    fn lookup_qualified(env: &EnvRef, name: &str) -> Result<Option<RuntimeValue>, EvaluationError> {
        let dot_indices: Vec<_> = name
            .match_indices('.')
            .rev()
            .filter_map(|(dot_index, _)| {
                (dot_index > 0 && dot_index + 1 < name.len()).then_some(dot_index)
            })
            .collect();
        if dot_indices.is_empty() {
            return Ok(None);
        }

        let mut current: Option<EnvRef> = Some(Rc::clone(env));
        while let Some(e) = current {
            let borrow = e.borrow();
            let mut frame_matched_prefix = false;
            for dot_index in dot_indices.iter().copied() {
                let prefix = &name[..dot_index];
                if let Some(&value_slot) = borrow.bindings.get(prefix) {
                    frame_matched_prefix = true;
                    let value = checked_slot_value(&borrow, prefix, value_slot)?;
                    if let Some(value) =
                        Self::resolve_qualified_suffix(value, &name[dot_index + 1..])
                    {
                        return Ok(Some(value));
                    }
                }
            }
            if frame_matched_prefix {
                return Ok(None);
            }
            current = borrow.parent.clone();
        }
        Ok(None)
    }

    fn resolve_qualified_suffix(mut value: RuntimeValue, suffix: &str) -> Option<RuntimeValue> {
        let mut saw_segment = false;
        for segment in suffix.split('.').filter(|segment| !segment.is_empty()) {
            saw_segment = true;
            let RuntimeValue::Map(map) = &value else {
                return None;
            };
            let next = map.borrow().get(&MapKey::Str(segment.into())).cloned()?;
            value = next;
        }
        saw_segment.then_some(value)
    }

    /// Mutate an existing binding somewhere in the environment chain.
    pub fn assign(env: &EnvRef, name: &str, value: RuntimeValue) -> Result<(), EvaluationError> {
        Self::assign_resolved(env, name, value).map(|_| ())
    }

    pub fn assign_resolved(
        env: &EnvRef,
        name: &str,
        value: RuntimeValue,
    ) -> Result<LexicalAddress, EvaluationError> {
        let mut current: Option<EnvRef> = Some(Rc::clone(env));
        let mut depth = 0;
        while let Some(e) = current {
            let mut borrow = e.borrow_mut();
            if let Some(&slot) = borrow.bindings.get(name) {
                if slot >= borrow.slots.len() {
                    return Err(EvaluationError::new(format!(
                        "binding slot for {name} is out of range"
                    )));
                }
                borrow.slots[slot] = value;
                return Ok(LexicalAddress { depth, slot });
            }
            current = borrow.parent.clone();
            depth += 1;
        }
        Err(EvaluationError::new(format!("undefined variable: {name}")))
    }

    pub fn try_assign_address(
        env: &EnvRef,
        name: &str,
        address: LexicalAddress,
        value: RuntimeValue,
    ) -> Result<Option<RuntimeValue>, EvaluationError> {
        let mut current = Rc::clone(env);
        for _ in 0..address.depth {
            let borrow = current.borrow();
            if borrow.bindings.contains_key(name) {
                return Ok(Some(value));
            }
            let Some(parent) = borrow.parent.clone() else {
                return Ok(Some(value));
            };
            drop(borrow);
            current = parent;
        }

        let mut borrow = current.borrow_mut();
        if borrow.bindings.get(name).copied() != Some(address.slot) {
            return Ok(Some(value));
        }
        if address.slot >= borrow.slots.len() {
            return Err(EvaluationError::new(format!(
                "binding slot for {name} is out of range"
            )));
        }
        borrow.slots[address.slot] = value;
        Ok(None)
    }

    fn define_local(&mut self, name: String, value: RuntimeValue) {
        // Bounds-check via get_mut: if `bindings` somehow has a stale slot
        // index (an invariant violation no current code path produces, but
        // cheap to guard against), fall through to re-allocate the slot
        // instead of panicking on out-of-bounds index.  Consistent with
        // try_assign / try_assign_address, which both bounds-check explicitly.
        if let Some(&slot) = self.bindings.get(&name) {
            if let Some(slot_ref) = self.slots.get_mut(slot) {
                *slot_ref = value;
                return;
            }
        }
        let slot = self.slots.len();
        self.bindings.insert(name, slot);
        self.slots.push(value);
    }
}

fn checked_slot_value(
    env: &Environment,
    name: &str,
    slot: usize,
) -> Result<RuntimeValue, EvaluationError> {
    let Some(value) = env.slots.get(slot) else {
        return Err(EvaluationError::new(format!(
            "environment slot for {name:?} is missing"
        )));
    };
    if matches!(value, RuntimeValue::UninitializedTopLevel) {
        return Err(EvaluationError::new(format!(
            "name {name:?} was accessed before initialization"
        )));
    }
    Ok(value.clone())
}

pub fn increment_evaluator_nesting() {
    EVALUATOR_NESTING.with(|nesting| {
        nesting.set(nesting.get() + 1);
    });
}

pub fn decrement_evaluator_nesting() {
    EVALUATOR_NESTING.with(|nesting| {
        let val = nesting.get().saturating_sub(1);
        nesting.set(val);
        if val == 0 {
            clear_tracked_environments();
        }
    });
}

enum GraphNode {
    Env(EnvRef),
    Closure(Rc<ClosureValue>),
    Macro(Rc<MacroValue>),
    List(RtList),
    Map(RtMap),
    Tuple(Rc<[RuntimeValue]>),
}

impl GraphNode {
    fn address(&self) -> usize {
        match self {
            GraphNode::Env(e) => Rc::as_ptr(e) as usize,
            GraphNode::Closure(c) => Rc::as_ptr(c) as usize,
            GraphNode::Macro(m) => Rc::as_ptr(m) as usize,
            GraphNode::List(l) => Rc::as_ptr(l) as usize,
            GraphNode::Map(m) => Rc::as_ptr(m) as usize,
            GraphNode::Tuple(t) => Rc::as_ptr(t) as *const () as usize,
        }
    }

    fn strong_count(&self) -> usize {
        match self {
            GraphNode::Env(e) => Rc::strong_count(e),
            GraphNode::Closure(c) => Rc::strong_count(c),
            GraphNode::Macro(m) => Rc::strong_count(m),
            GraphNode::List(l) => Rc::strong_count(l),
            GraphNode::Map(m) => Rc::strong_count(m),
            GraphNode::Tuple(t) => Rc::strong_count(t),
        }
    }
}

fn collect_graph_nodes(val: &RuntimeValue, add_ref: &mut dyn FnMut(GraphNode)) {
    match val {
        RuntimeValue::Closure(c) => add_ref(GraphNode::Closure(Rc::clone(c))),
        RuntimeValue::Macro(m) => add_ref(GraphNode::Macro(Rc::clone(m))),
        RuntimeValue::List(l) => add_ref(GraphNode::List(Rc::clone(l))),
        RuntimeValue::Map(m) => add_ref(GraphNode::Map(Rc::clone(m))),
        RuntimeValue::Tuple(t) => add_ref(GraphNode::Tuple(Rc::clone(t))),
        _ => {}
    }
}

/// Build a single reachability graph starting from ALL `starts` simultaneously.
/// Each node in the returned map holds one Rc clone (the `GraphNode` itself).
/// Edges in the `Vec<usize>` are raw addresses — no additional Rc clones.
fn build_global_reachability_graph(starts: &[EnvRef]) -> HashMap<usize, (GraphNode, Vec<usize>)> {
    let mut graph: HashMap<usize, (GraphNode, Vec<usize>)> = HashMap::new();
    let mut queue: Vec<GraphNode> = starts
        .iter()
        .map(|e| GraphNode::Env(Rc::clone(e)))
        .collect();

    while let Some(node) = queue.pop() {
        let addr = node.address();
        if graph.contains_key(&addr) {
            continue;
        }

        let mut refs = Vec::new();
        let mut add_ref = |child: GraphNode| {
            refs.push(child.address());
            queue.push(child);
        };

        match &node {
            GraphNode::Env(env) => {
                if let Ok(borrow) = env.try_borrow() {
                    for val in &borrow.slots {
                        collect_graph_nodes(val, &mut add_ref);
                    }
                }
            }
            GraphNode::Closure(c) => {
                add_ref(GraphNode::Env(Rc::clone(&c.env)));
            }
            GraphNode::Macro(m) => {
                add_ref(GraphNode::Env(Rc::clone(&m.env)));
            }
            GraphNode::List(l) => {
                if let Ok(borrow) = l.try_borrow() {
                    for val in borrow.iter() {
                        collect_graph_nodes(val, &mut add_ref);
                    }
                }
            }
            GraphNode::Map(m) => {
                if let Ok(borrow) = m.try_borrow() {
                    for val in borrow.values() {
                        collect_graph_nodes(val, &mut add_ref);
                    }
                }
            }
            GraphNode::Tuple(t) => {
                for val in t.iter() {
                    collect_graph_nodes(val, &mut add_ref);
                }
            }
        }

        graph.insert(addr, (node, refs));
    }

    graph
}

pub fn clear_tracked_environments() {
    TRACKED_ENVIRONMENTS.with(|tracked| {
        let mut list = tracked.borrow_mut();

        // The outer loop runs at most a small number of times (bounded by the
        // depth of nested Rc cycles).  In the common case it exits after one
        // iteration because the global graph identifies ALL cyclic garbage in a
        // single O(n + m) pass — no per-environment BFS needed.
        loop {
            // Collect alive non-empty environments as candidates.
            let candidates: Vec<EnvRef> = list
                .iter()
                .filter_map(|w| w.upgrade())
                .filter(|e| {
                    let b = e.borrow();
                    !b.slots.is_empty() || b.parent.is_some()
                })
                .collect();

            if candidates.is_empty() {
                break;
            }

            let candidate_addrs: HashSet<usize> =
                candidates.iter().map(|e| Rc::as_ptr(e) as usize).collect();

            // Build ONE graph from all candidates at once — O(n + m) total
            // instead of the previous O(n(n + m)) from calling
            // build_reachability_graph once per candidate.
            let graph = build_global_reachability_graph(&candidates);

            // Count incoming edges (graph-internal strong refs to each node).
            let mut incoming: HashMap<usize, usize> = graph.keys().map(|&a| (a, 0)).collect();
            for (_, refs) in graph.values() {
                for &r in refs {
                    *incoming.entry(r).or_insert(0) += 1;
                }
            }

            // For each node X:
            //   external_refs(X) = strong_count(X)
            //                    - 1  (X's own GraphNode holds one Rc)
            //                    - incoming[X]  (refs from other graph nodes)
            //                    - 1  (candidates Vec holds one Rc, if X is a candidate)
            //
            // Nodes with external_refs > 0 are live roots — something outside
            // the graph holds a strong reference to them.  Everything reachable
            // from a live root is also live.
            let mut live: HashSet<usize> = HashSet::with_capacity(graph.len());
            let mut queue: VecDeque<usize> = VecDeque::new();
            for (&addr, (node, _)) in &graph {
                let cand_ref = usize::from(candidate_addrs.contains(&addr));
                let inc = incoming.get(&addr).copied().unwrap_or(0);
                let external = node.strong_count().saturating_sub(1 + inc + cand_ref);
                if external > 0 && live.insert(addr) {
                    queue.push_back(addr);
                }
            }
            while let Some(addr) = queue.pop_front() {
                if let Some((_, refs)) = graph.get(&addr) {
                    for &r in refs {
                        if live.insert(r) {
                            queue.push_back(r);
                        }
                    }
                }
            }

            // Clear candidates that are not reachable from any live root.
            let mut cleared = 0usize;
            for env in &candidates {
                let addr = Rc::as_ptr(env) as usize;
                if !live.contains(&addr) {
                    let mut b = env.borrow_mut();
                    b.bindings.clear();
                    b.slots.clear();
                    b.parent = None;
                    cleared += 1;
                }
            }

            if cleared == 0 {
                break;
            }
        }

        list.retain(|weak| weak.strong_count() > 0);
    });
}

// ── ClosureValue ─────────────────────────────────────────────────────────────

// `env` holds a strong Rc to the captured lexical environment so that closures
// keep their scope alive when they escape the block that created them.
// A closure stored back into its own captured env (letrec-style `bind`) creates
// an `env → Closure → env` Rc cycle.  The cycle is collected by
// `clear_tracked_environments` when the evaluator nesting level drops to zero.
// A `Weak` reference would break the cycle at define-time but cannot be used
// safely without escape analysis: ALL closures defined in the current scope
// capture that env, not only self-referential ones, so weakening at define-time
// would break any escaped non-recursive closure.
#[derive(Debug)]
pub struct ClosureValue {
    pub params: Vec<String>,
    pub body_ids: Vec<NodeId>,
    pub env: EnvRef,
    pub graph: Rc<crate::graph::IRGraph>,
}

/// Runtime macro value.
///
/// A macro is a lexical closure over syntax, not values: call arguments are
/// quoted into detached `ExprSpec` host objects before binding. The macro body
/// must return syntax, which the evaluator expands in the caller's environment.
#[derive(Debug)]
pub struct MacroValue {
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
    pub phase_policy: PhasePolicy,
    pub effect_policy: EffectPolicy,
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
    ) -> CaapResult<Self> {
        let name = name.into();
        if name.is_empty() {
            return Err(CaapError::host("host function name must be non-empty"));
        }
        if max_arity.is_some_and(|max| max < min_arity) {
            return Err(CaapError::host(
                "host function max arity must be >= min arity",
            ));
        }
        Ok(Self {
            name,
            min_arity,
            max_arity,
            phase_policy: PhasePolicy::Runtime,
            effect_policy: EffectPolicy::builtin(BuiltinEffectTag::Impure),
            handler,
        })
    }

    pub fn with_phase_policy(mut self, phase_policy: PhasePolicy) -> Self {
        self.phase_policy = phase_policy;
        self
    }

    pub fn with_effect_policy(mut self, effect_policy: EffectPolicy) -> Self {
        self.effect_policy = effect_policy;
        self
    }

    pub fn try_with_effect(mut self, tag: impl Into<String>) -> CaapResult<Self> {
        self.effect_policy = EffectPolicy::single(tag)?;
        Ok(self)
    }
}

impl fmt::Debug for HostFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HostFunction({})", self.name)
    }
}

// ── BuiltinInfo ──────────────────────────────────────────────────────────────

/// A registered builtin callable.
pub type EagerBuiltinHandler = dyn Fn(Vec<RuntimeValue>) -> EvalResult;
pub type SpecialBuiltinHandler =
    dyn Fn(&mut crate::eval::Evaluator, &crate::ir::CallNode, &EnvRef) -> EvalResult;

pub enum BuiltinHandler {
    Eager(Box<EagerBuiltinHandler>),
    Special(Box<SpecialBuiltinHandler>),
}

pub struct BuiltinInfo {
    pub name: String,
    pub metadata: BuiltinMetadata,
    pub min_arity: usize,
    pub max_arity: Option<usize>,
    pub handler: BuiltinHandler,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuiltinVisibility {
    Public,
    Internal,
}

/// A builtin's declared value signature, surfaced through
/// `ctfe_kernel_vocabulary` for in-language checkers. Types use the stdlib
/// notation strings (`int`/`float`/`string`/`bool`/`list`/`map`/`any`, …);
/// a `*`-prefixed final param means "all remaining arguments of this type".
/// Undeclared builtins surface the polymorphic default `["*any"] -> "any"`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuiltinSignature {
    pub params: &'static [&'static str],
    pub result: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltinMetadata {
    pub eval_policy: EvalPolicy,
    pub control_policy: ControlPolicy,
    pub scope_policy: ScopePolicy,
    pub phase_policy: PhasePolicy,
    pub effect_policy: EffectPolicy,
    pub fold_policy: FoldPolicy,
    pub visibility: BuiltinVisibility,
    /// Declared value signature; `None` surfaces as the polymorphic default.
    pub signature: Option<BuiltinSignature>,
}

impl BuiltinMetadata {
    /// Whether arguments should be pre-evaluated before calling the builtin.
    /// True for `Eager` policy only; derived from `eval_policy`.
    pub fn eager_args(&self) -> bool {
        matches!(self.eval_policy, EvalPolicy::Eager)
    }

    pub fn is_public(&self) -> bool {
        matches!(self.visibility, BuiltinVisibility::Public)
    }

    pub fn eager_runtime() -> Self {
        Self {
            eval_policy: EvalPolicy::Eager,
            control_policy: ControlPolicy::Plain,
            scope_policy: ScopePolicy::None,
            phase_policy: PhasePolicy::Dual,
            effect_policy: EffectPolicy::pure(),
            // Pure eager value builtins fold once their inputs are static.
            fold_policy: FoldPolicy::RuntimePure,
            signature: None,
            visibility: BuiltinVisibility::Public,
        }
    }

    pub fn runtime_only() -> Self {
        // Runtime-only builtins are not callable at compile time, so they are
        // never partial-evaluation fold candidates regardless of purity.
        Self::eager_runtime()
            .with_phase_policy(PhasePolicy::Runtime)
            .with_fold_policy(FoldPolicy::Never)
    }

    pub fn special_form() -> Self {
        Self {
            eval_policy: EvalPolicy::SpecialForm,
            // Structured special forms need dedicated partial-evaluation
            // handling, not value lifting; classify them as non-foldable.
            fold_policy: FoldPolicy::Never,
            ..Self::eager_runtime()
        }
    }

    pub fn compile_time_pure() -> Self {
        Self::eager_runtime()
            .with_phase_policy(PhasePolicy::CompileTime)
            .with_fold_policy(FoldPolicy::Always)
    }

    pub fn compile_time_impure() -> Self {
        Self::compile_time_pure()
            .with_builtin_effect(BuiltinEffectTag::Impure)
            .with_fold_policy(FoldPolicy::Never)
    }

    pub fn compile_time_write_ir() -> Self {
        Self::compile_time_pure()
            .with_builtin_effect(BuiltinEffectTag::WriteIr)
            .with_fold_policy(FoldPolicy::Never)
    }

    pub fn compile_time_effects<const N: usize>(tags: [BuiltinEffectTag; N]) -> Self {
        let mut metadata = Self::compile_time_pure().with_fold_policy(FoldPolicy::Never);
        metadata.effect_policy = EffectPolicy::builtins(tags);
        metadata
    }

    pub fn compile_time_emit_diagnostics() -> Self {
        Self::compile_time_pure()
            .with_builtin_effect(BuiltinEffectTag::EmitDiagnostics)
            .with_fold_policy(FoldPolicy::Never)
    }

    pub fn compile_time_read_files() -> Self {
        Self::compile_time_pure()
            .with_builtin_effect(BuiltinEffectTag::ReadFiles)
            .with_fold_policy(FoldPolicy::Never)
    }

    pub fn compile_time_compiler_registry() -> Self {
        Self::compile_time_pure()
            .with_builtin_effect(BuiltinEffectTag::CompilerRegistry)
            .with_fold_policy(FoldPolicy::Never)
    }

    pub fn compile_time_special_impure() -> Self {
        Self::eager_runtime()
            .with_eval_policy(EvalPolicy::SpecialForm)
            .with_phase_policy(PhasePolicy::CompileTime)
            .with_builtin_effect(BuiltinEffectTag::Impure)
            .with_fold_policy(FoldPolicy::Never)
    }

    pub fn compile_time_special_effects<const N: usize>(tags: [BuiltinEffectTag; N]) -> Self {
        let mut metadata = Self::eager_runtime()
            .with_eval_policy(EvalPolicy::SpecialForm)
            .with_phase_policy(PhasePolicy::CompileTime)
            .with_fold_policy(FoldPolicy::Never);
        metadata.effect_policy = EffectPolicy::builtins(tags);
        metadata
    }

    pub fn runtime_mutation() -> Self {
        Self::eager_runtime()
            .with_builtin_effect(BuiltinEffectTag::Mutation)
            .with_fold_policy(FoldPolicy::Never)
    }

    pub fn runtime_sequential() -> Self {
        Self::eager_runtime()
            .with_eval_policy(EvalPolicy::Sequential)
            .with_fold_policy(FoldPolicy::Never)
    }

    /// Declare the value signature surfaced by `ctfe_kernel_vocabulary`.
    pub fn with_signature(mut self, params: &'static [&'static str], result: &'static str) -> Self {
        self.signature = Some(BuiltinSignature { params, result });
        self
    }

    pub fn with_eval_policy(mut self, eval_policy: EvalPolicy) -> Self {
        self.eval_policy = eval_policy;
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

    pub fn with_fold_policy(mut self, fold_policy: FoldPolicy) -> Self {
        self.fold_policy = fold_policy;
        self
    }

    pub fn with_builtin_effect(mut self, tag: BuiltinEffectTag) -> Self {
        self.effect_policy = EffectPolicy::builtin(tag);
        self
    }

    pub fn with_visibility(mut self, visibility: BuiltinVisibility) -> Self {
        self.visibility = visibility;
        self
    }

    pub fn internal(self) -> Self {
        self.with_visibility(BuiltinVisibility::Internal)
    }

    pub fn try_with_effect(mut self, tag: impl Into<String>) -> CaapResult<Self> {
        self.effect_policy = EffectPolicy::single(tag)?;
        Ok(self)
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
    /// Stable machine-readable category, when the error has one (e.g. a sys
    /// failure carries its `SysErrorKind` as `"not_found"`/`"permission_denied"`).
    /// Lets tooling and diagnostics react to the error class without parsing the
    /// message. `None` for ordinary evaluation errors.
    category: Option<String>,
    /// FATAL errors pierce `try`: they terminate the whole evaluation extent
    /// because catching them would void a resource guarantee (step/depth
    /// budgets — a hostile fold must not trap its own budget exhaustion).
    fatal: bool,
    frames: Vec<RuntimeCallFrame>,
}

impl EvaluationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            category: None,
            fatal: false,
            frames: Vec::new(),
        }
    }

    /// Attach a machine-readable category (see [`EvaluationError::category`]).
    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Mark this error FATAL: it pierces `try` (see the field doc).
    pub fn into_fatal(mut self) -> Self {
        self.fatal = true;
        self
    }

    pub fn is_fatal(&self) -> bool {
        self.fatal
    }

    pub fn category(&self) -> Option<&str> {
        self.category.as_deref()
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
        match &self.category {
            Some(category) => write!(f, "EvaluationError[{category}]: {}", self.message)?,
            None => write!(f, "EvaluationError: {}", self.message)?,
        }
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

/// Non-local exit from a `leave` form.
#[derive(Debug)]
pub struct LeaveSignal {
    pub target_block_id: NodeId,
    pub value: RuntimeValue,
}

/// Tail self-call: emitted by `eval_call` when a closure calls ITSELF in tail
/// position. Caught only by that closure's own trampoline in
/// `Evaluator::invoke_closure`, which rebinds the arguments and loops instead
/// of recursing — self-recursive tail loops run at constant evaluation depth.
#[derive(Debug)]
pub struct TailCallSignal {
    /// Identity of the tail-called closure: its allocation address. Valid for
    /// comparison because the trampoline holds the closure borrowed for the
    /// whole extent the signal can travel.
    pub token: usize,
    pub args: Vec<RuntimeValue>,
}

#[derive(Debug)]
pub enum EvalSignal {
    Leave(LeaveSignal),
    Error(EvaluationError),
    /// User-thrown exception via `(throw val)`. Caught by `(try ... (catch e ...))`.
    Exception(RuntimeValue),
    /// Internal trampoline signal — never observable from CAAP code: it is
    /// emitted only in statically re-armed tail positions (if/do/bind/match
    /// tails), so the only frames it crosses are those transparent forms.
    TailCall(TailCallSignal),
}

impl fmt::Display for EvalSignal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalSignal::Leave(l) => write!(f, "LeaveSignal(target={})", l.target_block_id),
            EvalSignal::Error(e) => write!(f, "{e}"),
            EvalSignal::Exception(v) => write!(f, "Exception({v})"),
            EvalSignal::TailCall(_) => write!(f, "internal tail-call signal escaped its closure"),
        }
    }
}

impl std::error::Error for EvalSignal {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EvalSignal::Leave(_) => None,
            EvalSignal::Error(error) => Some(error),
            EvalSignal::Exception(_) => None,
            EvalSignal::TailCall(_) => None,
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
