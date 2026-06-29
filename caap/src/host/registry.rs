use std::collections::btree_map::Entry;
use std::rc::Rc;

use caap_sys_runtime::catalog::{dispatch, export_catalog};
use caap_sys_runtime::ffi_value::SysArgs;

use crate::error::{CaapError, CaapResult};
use crate::runtime_loader::{runtime_to_sys, sys_to_runtime};
use crate::semantic::PhasePolicy;
use crate::values::{EnvRef, Environment, EvalSignal, EvaluationError, HostFunction, RuntimeValue};

use super::fn_misc::{known_host_export_contract, required_capability};
use super::sys_policy::{authorize, filter_result, Authorization};
use super::{
    HostCapabilityPolicy, HostExportMetadata, HostServiceExport, HostServiceLibrary,
    HostServiceRegistry, HostSystemPolicy,
};

impl HostServiceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn library_names(&self) -> Vec<&str> {
        self.libraries.keys().map(String::as_str).collect()
    }

    pub fn capability_policy(&self) -> &HostCapabilityPolicy {
        &self.capability_policy
    }

    pub fn system_policy(&self) -> HostSystemPolicy {
        self.system_policy.borrow().clone()
    }

    pub fn set_capability_policy(&mut self, policy: HostCapabilityPolicy) {
        self.capability_policy = policy;
        self.install_capability_gate();
    }

    pub fn set_system_policy(&mut self, policy: HostSystemPolicy) {
        *self.system_policy.borrow_mut() = policy;
    }

    pub fn allow_only_capabilities(
        &mut self,
        capabilities: impl IntoIterator<Item = String>,
    ) -> CaapResult<()> {
        self.capability_policy = HostCapabilityPolicy::allow_only(capabilities)?;
        self.install_capability_gate();
        Ok(())
    }

    /// Install (refresh) the capability backstop on the shared runtime state so
    /// the current capability policy is re-checked inside `dispatch` itself — a
    /// second line of defense behind bind-time gating. It catches a policy
    /// tightened after binding (already-bound exports stop being callable
    /// immediately) and any future dispatch path that does not route through
    /// `authorize`. Refreshed on every policy change so the gate never lags.
    fn install_capability_gate(&self) {
        self.runtime_state
            .borrow_mut()
            .set_policy(Some(Box::new(HostCapabilityGate {
                policy: self.capability_policy.clone(),
            })));
    }

    pub fn library(&self, name: &str) -> CaapResult<Option<&HostServiceLibrary>> {
        if name.is_empty() {
            return Err(CaapError::host(
                "host service library name must be non-empty",
            ));
        }
        Ok(self.libraries.get(name))
    }

    pub fn register_function(
        &mut self,
        library: impl Into<String>,
        export: impl Into<String>,
        phase: PhasePolicy,
        function: HostFunction,
    ) -> CaapResult<()> {
        let library = library.into();
        let export_str = export.into();
        tracing::debug!(library, export = export_str, "registering host function");
        if library.is_empty() {
            return Err(CaapError::host(
                "host service library name must be non-empty",
            ));
        }
        let entry = match self.libraries.entry(library.clone()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(HostServiceLibrary::new(library.clone())?),
        };
        entry.register_function(export_str, phase, function)
    }

    pub fn register_function_with_metadata(
        &mut self,
        library: impl Into<String>,
        export: impl Into<String>,
        phase: PhasePolicy,
        function: HostFunction,
        metadata: HostExportMetadata,
    ) -> CaapResult<()> {
        let library = library.into();
        if library.is_empty() {
            return Err(CaapError::host(
                "host service library name must be non-empty",
            ));
        }
        let entry = match self.libraries.entry(library.clone()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(HostServiceLibrary::new(library.clone())?),
        };
        entry.register_function_with_metadata(export, phase, function, metadata)
    }

    pub fn export(
        &self,
        library: &str,
        name: &str,
        phase: PhasePolicy,
    ) -> CaapResult<RuntimeValue> {
        let library_entry = self.library(library)?.ok_or_else(|| {
            CaapError::host(format!("host service library does not exist: {library}"))
        })?;
        let export = library_entry.export(name)?.ok_or_else(|| {
            CaapError::host(format!(
                "host service export does not exist: {library}.{name}"
            ))
        })?;
        if export.phase != PhasePolicy::Dual && export.phase != phase {
            return Err(CaapError::host(format!(
                "host service export {library}.{name} is not available in phase {}",
                phase.as_str()
            )));
        }
        // Enforce the fine-grained capability model from the registered export
        // contract. Built-in sys exports use the caap-sys read/write catalog;
        // custom/plugin exports use their explicit metadata capability.
        let required = required_export_capability(library, name, export)?;
        let allowed = self
            .capability_policy
            .allows_capability(required.as_deref());
        if !allowed {
            return Err(CaapError::host(match required {
                Some(capability) => {
                    format!("host capability denied: {library}.{name} (requires {capability})")
                }
                None => format!("host capability denied: {library}.{name}"),
            }));
        }
        Ok(export.runtime_value())
    }

    pub(crate) fn export_required_capability(
        &self,
        library: &str,
        name: &str,
    ) -> CaapResult<Option<String>> {
        let library_entry = self.library(library)?.ok_or_else(|| {
            CaapError::host(format!("host service library does not exist: {library}"))
        })?;
        let export = library_entry.export(name)?.ok_or_else(|| {
            CaapError::host(format!(
                "host service export does not exist: {library}.{name}"
            ))
        })?;
        required_export_capability(library, name, export)
    }

    pub fn export_library_to_environment(
        &self,
        library: &str,
        phase: PhasePolicy,
        env: &EnvRef,
    ) -> CaapResult<Vec<String>> {
        let library_entry = self.library(library)?.ok_or_else(|| {
            CaapError::host(format!("host service library does not exist: {library}"))
        })?;
        let mut bindings = Vec::new();
        let mut values = Vec::new();
        for name in library_entry.export_names() {
            let binding = format!("{library}.{name}");
            let value = self.export(library, name, phase)?;
            bindings.push(binding.clone());
            values.push((binding, value));
        }
        for (binding, value) in values {
            Environment::define(env, binding, value);
        }
        Ok(bindings)
    }

    pub fn register_default_system_libraries(&mut self) -> CaapResult<()> {
        // Every sys export is registered uniformly: the bound closure converts
        // arguments to the sys value model, applies caap host policy
        // (`sys_policy::authorize`), dispatches to caap-sys-runtime for the
        // actual behaviour, then post-filters the result. The operation
        // semantics are a single source of truth in caap-sys-runtime; this layer
        // contributes only policy enforcement and value conversion.
        for entry in export_catalog() {
            self.register_delegated_export(
                entry.library,
                entry.export,
                entry.min_arity as usize,
                entry.max_arity.map(|max| max as usize),
            )?;
        }
        Ok(())
    }

    fn register_delegated_export(
        &mut self,
        library: &'static str,
        export: &'static str,
        min_arity: usize,
        max_arity: Option<usize>,
    ) -> CaapResult<()> {
        let policy = Rc::clone(&self.system_policy);
        let state = Rc::clone(&self.runtime_state);
        let function = HostFunction::new(
            format!("{library}.{export}"),
            min_arity,
            max_arity,
            Box::new(move |args: Vec<RuntimeValue>| {
                let mut sys_args = SysArgs(
                    args.iter()
                        .map(runtime_to_sys)
                        .collect::<Result<Vec<_>, _>>()?,
                );
                match authorize(&policy, library, export, &mut sys_args)? {
                    Authorization::Short(value) => Ok(sys_to_runtime(value)),
                    Authorization::Proceed => {
                        let result = dispatch(&mut state.borrow_mut(), library, export, sys_args)
                            // Preserve the runtime error's classification so the
                            // diagnostic and any tooling can see the kind instead
                            // of a flattened message.
                            .map_err(|error| {
                                EvalSignal::Error(
                                    EvaluationError::new(error.message().to_string())
                                        .with_category(error.kind().as_str()),
                                )
                            })?;
                        let result = filter_result(&policy, library, export, result);
                        Ok(sys_to_runtime(result))
                    }
                }
            }),
        )?;
        self.register_function(library, export, sys_export_phase(library), function)
    }
}

/// Phase availability for a sys library. All sys exports are dual-phase: each
/// phase has its own registry with its own [`HostSystemPolicy`], and that
/// policy — not a coarse per-library phase gate — decides what an evaluation
/// may do (the compile-time sandbox blocks stdin reads, scopes fs to the read
/// roots, and denies net/process outright). Build-time logging via `io.print*`
/// and platform queries via `os` are the headline compile-time uses.
fn sys_export_phase(_library: &str) -> PhasePolicy {
    PhasePolicy::Dual
}

fn required_export_capability(
    library: &str,
    name: &str,
    export: &HostServiceExport,
) -> CaapResult<Option<String>> {
    if known_host_export_contract(library, name).is_some() {
        required_capability(library, name)
    } else {
        required_metadata_capability(&export.metadata)
    }
}

fn required_metadata_capability(metadata: &HostExportMetadata) -> CaapResult<Option<String>> {
    if metadata.is_pure() {
        return Ok(None);
    }
    let Some(capability) = metadata.capability_kind.as_deref() else {
        return Ok(None);
    };
    crate::semantic::CapabilityName::new(capability).map_err(|error| {
        CaapError::host(format!(
            "host export metadata capability is invalid: {error}"
        ))
    })?;
    Ok(Some(capability.to_string()))
}

/// Capability backstop enforced inside `caap_sys_runtime::dispatch` via the
/// runtime's `SysPolicy` hook. Holds a snapshot of the host capability policy
/// (refreshed by [`HostServiceRegistry::install_capability_gate`]) and re-checks
/// every sys operation against it at call time, mirroring the bind-time gate in
/// `required_export_capability`.
struct HostCapabilityGate {
    policy: HostCapabilityPolicy,
}

impl caap_sys_runtime::SysPolicy for HostCapabilityGate {
    fn check(
        &self,
        request: caap_sys_runtime::PolicyRequest<'_>,
    ) -> caap_sys_runtime::PolicyDecision {
        // Known sys exports map to a hierarchical capability; plugin exports have
        // no contract here, so leave their enforcement to bind time (allow here).
        let required = required_capability(request.library, request.export)
            .ok()
            .flatten();
        if self.policy.allows_capability(required.as_deref()) {
            caap_sys_runtime::PolicyDecision::Allow
        } else {
            caap_sys_runtime::PolicyDecision::Deny(format!(
                "host capability denied: {}.{}{}",
                request.library,
                request.export,
                required
                    .map(|capability| format!(" (requires {capability})"))
                    .unwrap_or_default()
            ))
        }
    }
}
