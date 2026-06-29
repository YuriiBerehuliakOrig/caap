//! Explicit host service registry substrate.
//!
//! The registry is inert by itself: it does not inject ambient capabilities into
//! evaluation. Callers must explicitly export functions into an environment or
//! pass them through a future compiler/session boundary.

mod fn_fs;
mod fn_io;
mod fn_misc;
mod registry;
mod sys_policy;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::rc::Rc;

use caap_sys_runtime::RuntimeState;

use crate::error::{CaapError, CaapResult};
use crate::semantic::{CapabilityName, PhasePolicy};
use crate::values::{HostFunction, RuntimeValue};

#[derive(Clone, Debug)]
pub struct HostServiceExport {
    pub library: String,
    pub name: String,
    pub phase: PhasePolicy,
    pub function: Rc<HostFunction>,
    pub metadata: HostExportMetadata,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostExportMetadata {
    pub module: Option<String>,
    pub policy: String,
    pub effect: String,
    pub kind: String,
    pub capability_kind: Option<String>,
    pub signature: HostExportSignature,
    pub min_arity: usize,
    pub max_arity: Option<usize>,
}

impl HostExportMetadata {
    pub fn is_pure(&self) -> bool {
        self.effect == "pure"
    }

    pub fn is_variadic(&self) -> bool {
        self.max_arity.is_none()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostExportSignature {
    pub params: Vec<HostExportParameter>,
    pub result: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostExportParameter {
    pub name: String,
    pub type_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct HostExportContract {
    pub(super) module: Option<&'static str>,
    pub(super) capability_kind: Option<&'static str>,
    pub(super) policy: &'static str,
    pub(super) effect: &'static str,
    pub(super) signature: HostExportSignature,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostCapabilityPolicy {
    AllowAll,
    AllowOnly(BTreeSet<CapabilityName>),
}

impl Default for HostCapabilityPolicy {
    /// Denies all host capabilities by default (empty allowlist).
    ///
    /// Use [`HostCapabilityPolicy::allow_all`] or
    /// [`HostCapabilityPolicy::allow_only`] to grant access. The high-level
    /// helpers [`CompilerHost::register_default_compile_time_system_libraries`]
    /// and [`CompilerHost::register_default_runtime_system_libraries`] set
    /// `allow_all` automatically alongside their system-policy setup.
    fn default() -> Self {
        Self::deny_all()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostSystemPolicy {
    pub fs: HostFileSystemPolicy,
    pub io: HostIoPolicy,
    pub process: HostProcessPolicy,
    pub net: HostNetworkPolicy,
    pub os: HostOsEnvironmentPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostFileSystemPolicy {
    pub read_roots: Option<Vec<PathBuf>>,
    pub write_roots: Option<Vec<PathBuf>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostIoPolicy {
    pub allow_stdin_read: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostProcessPolicy {
    pub allow_spawn: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostNetworkPolicy {
    pub allow_listen: bool,
    pub allow_connect: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostOsEnvironmentPolicy {
    pub allowlist: Option<BTreeSet<String>>,
}

#[derive(Clone, Debug)]
pub struct HostServiceLibrary {
    pub(super) name: String,
    pub(super) exports: BTreeMap<String, HostServiceExport>,
}

impl HostServiceExport {
    pub fn runtime_value(&self) -> RuntimeValue {
        RuntimeValue::HostFunction(Rc::clone(&self.function))
    }
}

impl Default for HostSystemPolicy {
    /// Denies all host capabilities by default.
    ///
    /// This is the safe-by-default starting point. Use [`HostSystemPolicy::allow_all`]
    /// or [`HostSystemPolicy::compile_time_sandbox`] to opt into the capabilities your
    /// program actually needs. Calling
    /// [`CompilerHost::register_default_runtime_system_libraries`] automatically
    /// upgrades the runtime registry to `allow_all`.
    fn default() -> Self {
        Self::deny_all()
    }
}

impl Default for HostFileSystemPolicy {
    /// Denies all filesystem access by default.
    ///
    /// Use [`HostFileSystemPolicy::allow_all`] or construct with explicit `read_roots`
    /// and `write_roots` to grant the access you need.
    fn default() -> Self {
        Self {
            read_roots: Some(Vec::new()),
            write_roots: Some(Vec::new()),
        }
    }
}

impl Default for HostIoPolicy {
    /// Denies stdin reads by default. Use [`HostIoPolicy::allow_all`] to opt in.
    fn default() -> Self {
        Self {
            allow_stdin_read: false,
        }
    }
}

impl Default for HostProcessPolicy {
    /// Denies subprocess spawning by default. Use [`HostProcessPolicy::allow_all`]
    /// to opt in.
    fn default() -> Self {
        Self { allow_spawn: false }
    }
}

impl Default for HostNetworkPolicy {
    /// Denies inbound and outbound network operations by default. Use
    /// [`HostNetworkPolicy::allow_all`] to opt in.
    fn default() -> Self {
        Self {
            allow_listen: false,
            allow_connect: false,
        }
    }
}

impl HostCapabilityPolicy {
    pub fn deny_all() -> Self {
        Self::AllowOnly(BTreeSet::new())
    }

    pub fn allow_all() -> Self {
        Self::AllowAll
    }

    pub fn allow_only(capabilities: impl IntoIterator<Item = String>) -> CaapResult<Self> {
        let mut allowed = BTreeSet::new();
        for capability in capabilities {
            let capability = CapabilityName::new(&capability).map_err(|error| {
                CaapError::host(format!("host capability name is invalid: {error}"))
            })?;
            allowed.insert(capability);
        }
        Ok(Self::AllowOnly(allowed))
    }

    /// Fine-grained capability check against a `required_capability`.
    ///
    /// A `None` requirement (pure operations) is always allowed. Otherwise the
    /// requirement must be covered by a granted capability under hierarchical
    /// matching. This is the gate the capability model enforces; see
    /// `docs/design-capability-enforcement.md`.
    pub fn allows_capability(&self, required: Option<&str>) -> bool {
        let Some(required) = required else {
            return true;
        };
        match self {
            Self::AllowAll => true,
            Self::AllowOnly(allowed) => {
                let Ok(requested) = CapabilityName::new(required) else {
                    return false;
                };
                allowed.iter().any(|grant| grant.covers(&requested))
            }
        }
    }
}

impl HostExportParameter {
    pub fn new(name: impl Into<String>, type_name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            type_name: type_name.into(),
        }
    }
}

impl HostSystemPolicy {
    /// Deny all host capabilities. Use this as a safe starting point and selectively
    /// enable only what the program requires.
    pub fn deny_all() -> Self {
        Self {
            fs: HostFileSystemPolicy {
                read_roots: Some(Vec::new()),
                write_roots: Some(Vec::new()),
            },
            io: HostIoPolicy {
                allow_stdin_read: false,
            },
            process: HostProcessPolicy { allow_spawn: false },
            net: HostNetworkPolicy {
                allow_listen: false,
                allow_connect: false,
            },
            os: HostOsEnvironmentPolicy {
                allowlist: Some(BTreeSet::new()),
            },
        }
    }

    pub fn allow_all() -> Self {
        Self {
            fs: HostFileSystemPolicy::allow_all(),
            io: HostIoPolicy::allow_all(),
            process: HostProcessPolicy::allow_all(),
            net: HostNetworkPolicy::allow_all(),
            os: HostOsEnvironmentPolicy::default(),
        }
    }

    pub fn compile_time_sandbox(read_roots: Option<Vec<PathBuf>>) -> Self {
        Self {
            fs: HostFileSystemPolicy {
                read_roots,
                write_roots: Some(Vec::new()),
            },
            io: HostIoPolicy {
                allow_stdin_read: false,
            },
            process: HostProcessPolicy { allow_spawn: false },
            net: HostNetworkPolicy {
                allow_listen: false,
                allow_connect: false,
            },
            os: HostOsEnvironmentPolicy {
                allowlist: Some(BTreeSet::new()),
            },
        }
    }
}

impl HostFileSystemPolicy {
    pub fn allow_all() -> Self {
        Self {
            read_roots: None,
            write_roots: None,
        }
    }

    pub fn read_only(read_roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            read_roots: Some(read_roots.into_iter().collect()),
            write_roots: Some(Vec::new()),
        }
    }
}

impl HostIoPolicy {
    pub fn allow_all() -> Self {
        Self {
            allow_stdin_read: true,
        }
    }
}

impl HostProcessPolicy {
    pub fn allow_all() -> Self {
        Self { allow_spawn: true }
    }
}

impl HostNetworkPolicy {
    pub fn allow_all() -> Self {
        Self {
            allow_listen: true,
            allow_connect: true,
        }
    }
}

impl HostOsEnvironmentPolicy {
    pub fn allow_only(names: impl IntoIterator<Item = String>) -> CaapResult<Self> {
        let mut allowlist = BTreeSet::new();
        for name in names {
            if name.is_empty() {
                return Err(CaapError::host(
                    "OS environment allowlist entries must be non-empty",
                ));
            }
            allowlist.insert(name);
        }
        Ok(Self {
            allowlist: Some(allowlist),
        })
    }
}

impl HostServiceLibrary {
    pub fn new(name: impl Into<String>) -> CaapResult<Self> {
        let name = name.into();
        if name.is_empty() {
            return Err(CaapError::host(
                "host service library name must be non-empty",
            ));
        }
        Ok(Self {
            name,
            exports: BTreeMap::new(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn export_names(&self) -> Vec<&str> {
        self.exports.keys().map(String::as_str).collect()
    }

    pub fn export(&self, name: &str) -> CaapResult<Option<&HostServiceExport>> {
        if name.is_empty() {
            return Err(CaapError::host(
                "host service export name must be non-empty",
            ));
        }
        Ok(self.exports.get(name))
    }

    pub fn register_function(
        &mut self,
        name: impl Into<String>,
        phase: PhasePolicy,
        function: HostFunction,
    ) -> CaapResult<()> {
        let name = name.into();
        let metadata = fn_misc::host_export_metadata(&self.name, &name, &function)?;
        self.register_function_with_metadata(name, phase, function, metadata)
    }

    pub fn register_function_with_metadata(
        &mut self,
        name: impl Into<String>,
        phase: PhasePolicy,
        function: HostFunction,
        metadata: HostExportMetadata,
    ) -> CaapResult<()> {
        let name = name.into();
        if name.is_empty() {
            return Err(CaapError::host(
                "host service export name must be non-empty",
            ));
        }
        validate_host_export_metadata(&self.name, &name, &function, &metadata)?;
        let entry = match self.exports.entry(name) {
            std::collections::btree_map::Entry::Occupied(e) => {
                return Err(CaapError::host(format!(
                    "host service export already registered: {}.{}",
                    self.name,
                    e.key()
                )));
            }
            std::collections::btree_map::Entry::Vacant(e) => e,
        };
        let function = function.with_phase_policy(phase);
        let export = HostServiceExport {
            library: self.name.clone(),
            name: entry.key().clone(),
            phase,
            metadata,
            function: Rc::new(function),
        };
        entry.insert(export);
        Ok(())
    }
}

fn validate_host_export_metadata(
    library: &str,
    name: &str,
    function: &HostFunction,
    metadata: &HostExportMetadata,
) -> CaapResult<()> {
    let export_name = format!("{library}.{name}");
    if metadata.kind != "function" {
        return Err(CaapError::host(format!(
            "host service metadata kind for {export_name} must be function"
        )));
    }
    if let Some(module) = metadata.module.as_deref() {
        if module.is_empty() {
            return Err(CaapError::host(format!(
                "host service metadata module for {export_name} must be non-empty"
            )));
        }
    }
    if metadata.policy.is_empty() {
        return Err(CaapError::host(format!(
            "host service metadata policy for {export_name} must be non-empty"
        )));
    }
    match metadata.effect.as_str() {
        "pure" | "impure" => {}
        _ => {
            return Err(CaapError::host(format!(
                "host service metadata effect for {export_name} must be pure or impure"
            )));
        }
    }
    if metadata.signature.result.is_empty() {
        return Err(CaapError::host(format!(
            "host service metadata result type for {export_name} must be non-empty"
        )));
    }
    for parameter in &metadata.signature.params {
        if parameter.name.is_empty() || parameter.type_name.is_empty() {
            return Err(CaapError::host(format!(
                "host service metadata parameters for {export_name} must have non-empty names and types"
            )));
        }
    }
    let parameter_count = metadata.signature.params.len();
    if parameter_count < function.min_arity {
        return Err(CaapError::host(format!(
            "host service metadata signature for {export_name} has fewer parameters than required arity"
        )));
    }
    if function
        .max_arity
        .is_some_and(|max_arity| parameter_count > max_arity)
    {
        return Err(CaapError::host(format!(
            "host service metadata signature for {export_name} has more parameters than maximum arity"
        )));
    }
    if let Some(capability) = metadata.capability_kind.as_deref() {
        CapabilityName::new(capability).map_err(|error| {
            CaapError::host(format!(
                "host service metadata capability for {export_name} is invalid: {error}"
            ))
        })?;
    }
    if metadata.min_arity != function.min_arity || metadata.max_arity != function.max_arity {
        return Err(CaapError::host(format!(
            "host service metadata arity for {export_name} does not match function arity"
        )));
    }
    Ok(())
}

#[derive(Clone)]
pub struct HostServiceRegistry {
    pub(super) libraries: BTreeMap<String, HostServiceLibrary>,
    pub(super) capability_policy: HostCapabilityPolicy,
    pub(super) system_policy: Rc<RefCell<HostSystemPolicy>>,
    /// Per-session sys-runtime handle state (open files, sockets, processes).
    /// Shared by `Rc` so cloning the registry shares one set of live handles,
    /// and dropped with the last registry clone — scoping resource lifetime to
    /// the session rather than to a process-global `thread_local`.
    pub(super) runtime_state: Rc<RefCell<RuntimeState>>,
}

impl Default for HostServiceRegistry {
    fn default() -> Self {
        Self {
            libraries: BTreeMap::new(),
            capability_policy: HostCapabilityPolicy::default(),
            system_policy: Rc::new(RefCell::new(HostSystemPolicy::default())),
            runtime_state: Rc::new(RefCell::new(RuntimeState::new())),
        }
    }
}

impl fmt::Debug for HostServiceRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `runtime_state` holds live OS handles and is intentionally omitted.
        f.debug_struct("HostServiceRegistry")
            .field("libraries", &self.libraries)
            .field("capability_policy", &self.capability_policy)
            .field("system_policy", &self.system_policy)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::values::HostFunction;

    #[test]
    fn capability_matcher_is_hierarchical_and_segment_aware() {
        let covers = |grant: &str, requested: &str| {
            CapabilityName::new(grant)
                .unwrap()
                .covers(&CapabilityName::new(requested).unwrap())
        };

        // Exact and subtree matches.
        assert!(covers("sys.fs.read", "sys.fs.read"));
        assert!(covers("sys.fs", "sys.fs.read"));
        assert!(covers("sys.fs", "sys.fs.write"));
        assert!(covers("sys", "sys.fs.read"));
        // A narrower grant does not cover a sibling or broader capability.
        assert!(!covers("sys.fs.read", "sys.fs.write"));
        assert!(!covers("sys.fs", "sys.net"));
        assert!(!covers("sys.fs.read", "sys.fs"));
        // Segment-aware: a prefix that is not a full segment does not match.
        assert!(!covers("sys.fs", "sys.fsx"));
    }

    #[test]
    fn allow_only_policy_honors_hierarchical_grants() {
        let policy = HostCapabilityPolicy::allow_only(["sys.fs".to_string()]).unwrap();
        assert!(policy.allows_capability(Some("sys.fs.read")));
        assert!(policy.allows_capability(Some("sys.fs.write")));
        assert!(!policy.allows_capability(Some("sys.net.connect")));
    }

    #[test]
    fn registry_enforces_fine_grained_fs_capabilities() {
        use crate::PhasePolicy::Runtime;
        let build = || {
            let mut registry = HostServiceRegistry::new();
            registry.register_default_system_libraries().unwrap();
            registry
        };

        // Read-only grant: read-text binds; write-text is denied, naming the
        // capability it would need.
        let mut registry = build();
        registry.set_capability_policy(
            HostCapabilityPolicy::allow_only(["sys.fs.read".to_string()]).unwrap(),
        );
        assert!(registry.export("fs", "read_text", Runtime).is_ok());
        let denied = format!(
            "{}",
            registry.export("fs", "write_text", Runtime).unwrap_err()
        );
        assert!(denied.contains("sys.fs.write"), "{denied}");

        // A domain grant covers both accesses (hierarchical matching).
        let mut registry = build();
        registry.set_capability_policy(
            HostCapabilityPolicy::allow_only(["sys.fs".to_string()]).unwrap(),
        );
        assert!(registry.export("fs", "read_text", Runtime).is_ok());
        assert!(registry.export("fs", "write_text", Runtime).is_ok());

        // Explicit allow_all remains the opt-in escape hatch for trusted hosts.
        let mut registry = build();
        registry.set_capability_policy(HostCapabilityPolicy::allow_all());
        assert!(registry.export("fs", "write_text", Runtime).is_ok());
    }

    #[test]
    fn allows_capability_gates_on_required_capability() {
        // Pure operations (no requirement) are always allowed.
        assert!(HostCapabilityPolicy::deny_all().allows_capability(None));
        assert!(HostCapabilityPolicy::allow_all().allows_capability(Some("sys.fs.write")));

        let read_only = HostCapabilityPolicy::allow_only(["sys.fs.read".to_string()]).unwrap();
        assert!(read_only.allows_capability(Some("sys.fs.read")));
        assert!(!read_only.allows_capability(Some("sys.fs.write")));
        assert!(!read_only.allows_capability(Some("sys.net")));
        assert!(read_only.allows_capability(None));

        // A domain grant covers both accesses under hierarchical matching.
        let fs = HostCapabilityPolicy::allow_only(["sys.fs".to_string()]).unwrap();
        assert!(fs.allows_capability(Some("sys.fs.read")));
        assert!(fs.allows_capability(Some("sys.fs.write")));
        assert!(!fs.allows_capability(Some("sys.net")));
    }

    #[test]
    fn host_system_component_defaults_are_deny_all() {
        assert!(!HostIoPolicy::default().allow_stdin_read);
        assert!(!HostProcessPolicy::default().allow_spawn);
        assert!(!HostNetworkPolicy::default().allow_listen);
        assert!(!HostNetworkPolicy::default().allow_connect);

        let allow_all = HostSystemPolicy::allow_all();
        assert!(allow_all.io.allow_stdin_read);
        assert!(allow_all.process.allow_spawn);
        assert!(allow_all.net.allow_listen);
        assert!(allow_all.net.allow_connect);
    }

    #[test]
    fn host_service_library_rejects_duplicate_exports() {
        let mut library = HostServiceLibrary::new("demo").unwrap();
        let metadata = HostExportMetadata {
            module: Some("test.host".to_string()),
            policy: "none".to_string(),
            effect: "impure".to_string(),
            kind: "function".to_string(),
            capability_kind: Some("test.host".to_string()),
            signature: HostExportSignature {
                params: vec![HostExportParameter::new("value", "any")],
                result: "any".to_string(),
            },
            min_arity: 1,
            max_arity: Some(1),
        };
        library
            .register_function_with_metadata(
                "id",
                PhasePolicy::Runtime,
                HostFunction::new("demo.id", 1, Some(1), Box::new(|args| Ok(args[0].clone())))
                    .unwrap(),
                metadata.clone(),
            )
            .unwrap();

        let error = library
            .register_function_with_metadata(
                "id",
                PhasePolicy::Runtime,
                HostFunction::new("demo.id", 1, Some(1), Box::new(|args| Ok(args[0].clone())))
                    .unwrap(),
                metadata,
            )
            .unwrap_err()
            .to_string();

        assert!(error.contains("host service export already registered: demo.id"));
    }
}
