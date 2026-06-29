//! Sub-struct types that compose the `Compiler` session state.
//!
//! Keeping these isolated makes the ownership model clear:
//!   - `BootstrapState` — append-only bootstrap logs + ephemeral call stacks
//!   - `ProviderDispatch` — query provider registry and active execution context
//!   - `CompileCache` — artifact, CTFE, and source-template caches
//!   - `DiagnosticAccumulator` — diagnostics, event log, and live sink
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use crate::artifacts::{ArtifactCache, ArtifactKey, SourceTemplateCache};
use crate::diagnostics::{CompilerEventLog, Diagnostic};
use crate::error::{CaapError, CaapResult};
use crate::semantic::CapabilityName;

use super::super::bootstrap::{BootstrapCapabilityGraph, BootstrapImageStore, BootstrapTraceEvent};
use super::super::host::DiagnosticSink;
use super::super::query_provider::{
    ProviderCacheEntry, QueryProviderContext, QueryProviderRegistry,
};

/// Bootstrap execution state — tracks capabilities, images, and in-progress depth.
///
/// `executions`, `trace`, and `execution_memo` are wrapped in `Rc<RefCell<>>` so
/// that `CompilerBridgeValue::from_compiler` can clone them O(1) (pointer copy)
/// instead of deep-copying growing vecs/sets on every provider invocation.  All
/// three are pure append-only logs — mutations from a rolled-back transaction are
/// intentionally preserved (traces and memos are not undone on failure).
#[derive(Clone, Debug)]
pub struct BootstrapState {
    pub capabilities: BootstrapCapabilityGraph,
    pub images: BootstrapImageStore,
    pub executions: Rc<RefCell<Vec<String>>>,
    pub trace: Rc<RefCell<Vec<BootstrapTraceEvent>>>,
    pub execution_memo: Rc<RefCell<BTreeSet<String>>>,
    pub active_depth: usize,
    pub path_stack: Vec<String>,
    pub unit_stack: Vec<String>,
    pub capability_stack: Vec<Vec<CapabilityName>>,
}

impl BootstrapState {
    pub fn new() -> Self {
        Self {
            capabilities: BootstrapCapabilityGraph::new(),
            images: BootstrapImageStore::new(),
            executions: Rc::new(RefCell::new(Vec::new())),
            trace: Rc::new(RefCell::new(Vec::new())),
            execution_memo: Rc::new(RefCell::new(BTreeSet::new())),
            active_depth: 0,
            path_stack: Vec::new(),
            unit_stack: Vec::new(),
            capability_stack: Vec::new(),
        }
    }

    pub fn enter_execution(&mut self) -> CaapResult<usize> {
        let depth = self.active_depth;
        self.active_depth = self
            .active_depth
            .checked_add(1)
            .ok_or_else(|| CaapError::compiler("bootstrap active depth overflow"))?;
        Ok(depth)
    }

    pub fn leave_execution(&mut self) -> CaapResult<()> {
        self.active_depth = self
            .active_depth
            .checked_sub(1)
            .ok_or_else(|| CaapError::compiler("bootstrap active depth underflow"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_state_rejects_depth_overflow_without_mutating() {
        let mut state = BootstrapState::new();
        state.active_depth = usize::MAX;

        let error = state.enter_execution().unwrap_err().to_string();

        assert!(error.contains("bootstrap active depth overflow"));
        assert_eq!(state.active_depth, usize::MAX);
    }

    #[test]
    fn bootstrap_state_rejects_depth_underflow_without_mutating() {
        let mut state = BootstrapState::new();

        let error = state.leave_execution().unwrap_err().to_string();

        assert!(error.contains("bootstrap active depth underflow"));
        assert_eq!(state.active_depth, 0);
    }
}

/// Provider dispatch state — registry, active context, restart signals.
#[derive(Clone, Debug)]
pub struct ProviderDispatch {
    pub registry: Rc<QueryProviderRegistry>,
    pub active_context: Option<QueryProviderContext>,
    pub dynamic_requires: BTreeMap<String, Vec<String>>,
    pub pending_restart: Option<String>,
}

impl ProviderDispatch {
    pub fn new() -> Self {
        Self {
            registry: Rc::new(QueryProviderRegistry::new()),
            active_context: None,
            dynamic_requires: BTreeMap::new(),
            pending_restart: None,
        }
    }
}

/// Compilation caches — artifact cache, CTFE cache, source templates.
#[derive(Clone, Debug)]
pub struct CompileCache {
    pub artifact_cache: Rc<ArtifactCache>,
    pub ctfe_cache: BTreeMap<ArtifactKey, ProviderCacheEntry>,
    pub source_templates: Rc<SourceTemplateCache>,
}

impl CompileCache {
    pub fn new() -> Self {
        Self {
            artifact_cache: Rc::new(ArtifactCache::new()),
            ctfe_cache: BTreeMap::new(),
            source_templates: Rc::new(SourceTemplateCache::new()),
        }
    }
}

/// Diagnostic accumulator — collects diagnostics, events, and the live sink.
#[derive(Clone, Debug)]
pub struct DiagnosticAccumulator {
    pub diagnostics: Vec<Diagnostic>,
    pub events: CompilerEventLog,
    pub sink: DiagnosticSink,
}

impl DiagnosticAccumulator {
    pub fn new() -> Self {
        Self {
            diagnostics: Vec::new(),
            events: CompilerEventLog::new(),
            sink: DiagnosticSink::default(),
        }
    }
}
