//! Variable / scope inspection for [`DebugController`]: allocating
//! `variablesReference` handles and expanding them into [`VarSnapshot`]s for the
//! DAP `variables`/`scopes`/`completions` requests. Split out of `mod.rs` so the
//! controller core keeps only the stepping/pause/breakpoint logic.

use caap_core::values::{EnvRef, RuntimeValue};

use crate::protocol::VarSnapshot;

use super::{
    captured_bindings, collect_scope_bindings, expand_children, indexed_count, is_expandable,
    named_count, render_value, value_kind, DebugController, Handle,
};

impl DebugController {
    /// Allocate (or reuse the current stop's) reference for a frame's locals.
    pub(super) fn scope_reference(&mut self, frame_id: i64) -> i64 {
        self.handles.push(Handle::Locals(frame_id as usize));
        self.handles.len() as i64
    }

    /// Allocate a reference for an expandable value, or 0 for a leaf.
    pub(super) fn child_reference(&mut self, value: &RuntimeValue) -> i64 {
        if is_expandable(value) {
            self.handles.push(Handle::Value(value.clone()));
            self.handles.len() as i64
        } else {
            0
        }
    }

    fn env_reference(&mut self, env: EnvRef) -> i64 {
        self.handles.push(Handle::Env(env));
        self.handles.len() as i64
    }

    pub(super) fn snapshots_for(&mut self, pairs: Vec<(String, RuntimeValue)>) -> Vec<VarSnapshot> {
        pairs
            .into_iter()
            .map(|(name, value)| VarSnapshot {
                value: render_value(&value),
                kind: value_kind(&value).to_string(),
                variables_reference: self.child_reference(&value),
                indexed_variables: indexed_count(&value),
                named_variables: named_count(&value),
                name,
            })
            .collect()
    }

    /// Expand a variablesReference into its members/fields, paging indexed
    /// children with `start`/`count` (`count == 0` means all).
    pub(super) fn variables(
        &mut self,
        reference: i64,
        start: usize,
        count: usize,
    ) -> Vec<VarSnapshot> {
        let take = |len: usize| {
            if count == 0 {
                len
            } else {
                (start + count).min(len)
            }
        };
        let handle = self.handles.get((reference - 1) as usize).cloned();
        match handle {
            Some(Handle::Locals(frame)) => {
                let Some(env) = self.frames.get(frame).and_then(|f| f.env.clone()) else {
                    return Vec::new();
                };
                let pairs = collect_scope_bindings(&env);
                self.snapshots_for(pairs)
            }
            Some(Handle::Env(env)) => {
                let pairs = captured_bindings(&env);
                self.snapshots_for(pairs)
            }
            // Closures/macros expand into their parameters + captured scope.
            Some(Handle::Value(RuntimeValue::Closure(cl))) => {
                self.callable_fields(&cl.params, &cl.env)
            }
            Some(Handle::Value(RuntimeValue::Macro(m))) => self.callable_fields(&m.params, &m.env),
            // Host objects (e.g. the `compiler` bridge) expose named children
            // for inspection (registered values, …).
            Some(Handle::Value(RuntimeValue::HostObject(object))) => {
                let mut pairs = object.debug_children();
                pairs.sort_by(|a, b| a.0.cmp(&b.0));
                pairs.truncate(500);
                self.snapshots_for(pairs)
            }
            // Lists/tuples are paged at the source so huge collections don't
            // materialize every element per request.
            Some(Handle::Value(RuntimeValue::List(items))) => {
                let items = items.borrow();
                let end = take(items.len());
                let pairs = (start..end)
                    .map(|i| (format!("[{i}]"), items[i].clone()))
                    .collect();
                self.snapshots_for(pairs)
            }
            Some(Handle::Value(RuntimeValue::Tuple(items))) => {
                let end = take(items.len());
                let pairs = (start..end)
                    .map(|i| (format!("[{i}]"), items[i].clone()))
                    .collect();
                self.snapshots_for(pairs)
            }
            Some(Handle::Value(value)) => {
                let pairs = expand_children(&value);
                let end = take(pairs.len());
                let start = start.min(end);
                self.snapshots_for(pairs[start..end].to_vec())
            }
            None => Vec::new(),
        }
    }

    /// Fields for an expanded closure/macro: its parameter list, arity, and a
    /// drillable `captured` scope (the variables it closed over).
    fn callable_fields(&mut self, params: &[String], env: &EnvRef) -> Vec<VarSnapshot> {
        let captured = captured_bindings(env);
        let captured_ref = if captured.is_empty() {
            0
        } else {
            self.env_reference(env.clone())
        };
        vec![
            VarSnapshot {
                name: "params".to_string(),
                value: format!("({})", params.join(" ")),
                kind: "params".to_string(),
                variables_reference: 0,
                indexed_variables: 0,
                named_variables: 0,
            },
            VarSnapshot {
                name: "arity".to_string(),
                value: params.len().to_string(),
                kind: "int".to_string(),
                variables_reference: 0,
                indexed_variables: 0,
                named_variables: 0,
            },
            VarSnapshot {
                name: "captured".to_string(),
                value: format!("{} variable(s)", captured.len()),
                kind: "scope".to_string(),
                variables_reference: captured_ref,
                indexed_variables: 0,
                named_variables: 0,
            },
        ]
    }

    /// Identifier completions for the debug console: the binding names visible in
    /// the frame's scope chain.
    pub(super) fn completions(&self, frame_id: i64) -> Vec<String> {
        let Some(env) = self.frame_env(frame_id) else {
            return Vec::new();
        };
        collect_scope_bindings(&env)
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }
}
