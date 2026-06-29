//! Compile-time (CTFE) debug hook.
//!
//! The evaluator ([`crate::eval::Evaluator`]) is a single-threaded tree-walking
//! interpreter. To support step-through debugging of the stdlib bootstrap we
//! expose a thread-local [`DebugHook`] that the evaluator calls before each node
//! and on logical call enter/exit. The hook implementation (the debug adapter,
//! in the `caap-dap` crate) decides whether to pause; because evaluation and the
//! hook run on the same thread, the hook can borrow the live [`IRGraph`] and
//! [`EnvRef`] to snapshot the current position, call stack, and variables.
//!
//! When no debugger is attached, the only added cost on the evaluation hot path
//! is a single thread-local boolean load per node ([`hook_active`]).

use std::cell::RefCell;
use std::rc::Rc;

use crate::graph::IRGraph;
use crate::ir::NodeId;
use crate::source::SourceSpan;
use crate::values::EnvRef;

/// Callbacks the evaluator invokes during compile-time evaluation.
///
/// All methods run on the evaluation thread and may block (that is how a
/// breakpoint or a step pause is implemented: the method does not return until
/// the user issues the next command).
pub trait DebugHook {
    /// Called immediately before node `node_id` is evaluated. `span` is the
    /// node's source location (absent for synthetic nodes). `graph` and `env`
    /// are the live evaluation state for snapshotting.
    fn on_node(
        &mut self,
        node_id: NodeId,
        span: Option<&SourceSpan>,
        graph: &IRGraph,
        env: &EnvRef,
    );

    /// Called when a user-function (closure) call frame is entered. `name` is
    /// the callee name derived at the call site, when known. The frame's current
    /// line is tracked from the subsequent [`DebugHook::on_node`] calls.
    fn on_call_enter(&mut self, name: Option<&str>);

    /// Called when the matching call frame is left (success or error).
    fn on_call_exit(&mut self);

    /// Called when evaluating `node_id` produced an error. Invoked at each
    /// frame as the error propagates; an implementation that pauses should
    /// pause only on the first (innermost) report. `span` is the failing node's
    /// location. Default: no-op.
    fn on_error(
        &mut self,
        _message: &str,
        _node_id: NodeId,
        _span: Option<&SourceSpan>,
        _graph: &IRGraph,
        _env: &EnvRef,
    ) {
    }
}

thread_local! {
    static ACTIVE_HOOK: RefCell<Option<Rc<RefCell<dyn DebugHook>>>> = const { RefCell::new(None) };
}

/// Install a debug hook on the current thread. Subsequent evaluation on this
/// thread will route through it until [`clear_hook`].
pub fn install_hook(hook: Rc<RefCell<dyn DebugHook>>) {
    ACTIVE_HOOK.with(|h| *h.borrow_mut() = Some(hook));
}

/// Remove any installed hook on the current thread.
pub fn clear_hook() {
    ACTIVE_HOOK.with(|h| *h.borrow_mut() = None);
}

/// Cheap gate the evaluator uses to skip all hook work when no debugger is
/// attached. A single thread-local boolean load.
#[inline]
pub fn hook_active() -> bool {
    ACTIVE_HOOK.with(|h| h.borrow().is_some())
}

/// Run `f` with the installed hook, if any.
///
/// Uses `try_borrow_mut`, so if the hook is **already** borrowed — i.e. the
/// evaluator re-enters while the debugger is paused inside the hook (for
/// example evaluating a watch expression, which runs a nested evaluation) — the
/// call is a no-op (`None`). That nested evaluation therefore proceeds without
/// pausing or stepping again, and without a `RefCell` double-borrow panic.
#[inline]
pub fn with_hook<R>(f: impl FnOnce(&mut dyn DebugHook) -> R) -> Option<R> {
    ACTIVE_HOOK.with(|h| {
        // Clone the Rc out of the thread-local so the thread-local's own borrow
        // is released before we borrow the hook itself.
        let hook = h.borrow().clone();
        hook.and_then(|rc| rc.try_borrow_mut().ok().map(|mut guard| f(&mut *guard)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::Evaluator;
    use crate::frontend::parse;
    use crate::values::RuntimeValue;

    #[derive(Default)]
    struct Recorder {
        nodes: usize,
        depth: i32,
        max_depth: i32,
        enters: usize,
        exits: usize,
    }

    impl DebugHook for Recorder {
        fn on_node(
            &mut self,
            _node_id: NodeId,
            _span: Option<&SourceSpan>,
            _graph: &IRGraph,
            _env: &EnvRef,
        ) {
            self.nodes += 1;
        }
        fn on_call_enter(&mut self, _name: Option<&str>) {
            self.enters += 1;
            self.depth += 1;
            self.max_depth = self.max_depth.max(self.depth);
        }
        fn on_call_exit(&mut self) {
            self.exits += 1;
            self.depth -= 1;
        }
    }

    #[test]
    fn hook_records_nodes_and_balanced_call_frames() {
        let rec = Rc::new(RefCell::new(Recorder::default()));
        install_hook(rec.clone());
        let graph = parse("(bind ((f (lambda (x) (int_add x 1)))) (f 5))").unwrap();
        let mut ev = Evaluator::new(graph);
        let result = ev.run().unwrap();
        clear_hook();

        assert_eq!(result, RuntimeValue::Int(6));
        let r = rec.borrow();
        assert!(r.nodes > 0, "on_node should fire for evaluated nodes");
        assert_eq!(r.depth, 0, "call frames must be balanced");
        assert_eq!(r.enters, r.exits, "every enter has a matching exit");
        assert!(r.max_depth >= 1, "must have entered the `f` closure frame");
    }

    #[test]
    fn no_hook_is_inert() {
        clear_hook();
        assert!(!hook_active());
        let graph = parse("(int_add 2 3)").unwrap();
        let mut ev = Evaluator::new(graph);
        assert_eq!(ev.run().unwrap(), RuntimeValue::Int(5));
    }
}
