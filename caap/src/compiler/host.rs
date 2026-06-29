use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Mutex;

use crate::diagnostics::Diagnostic;
use crate::error::{CaapError, CaapResult};
use crate::host::{HostCapabilityPolicy, HostServiceRegistry, HostSystemPolicy};

use super::session::HostSourceTemplateCache;

/// Callback invoked immediately when a diagnostic is pushed to the compiler.
/// Cloneable (shared Rc) so it can be transferred from `Compiler` to
/// `CompilerBridgeValue` without copying the closure.
type DiagnosticSinkCallback = Rc<dyn Fn(&Diagnostic)>;

#[derive(Clone, Default)]
pub struct DiagnosticSink(pub(super) Option<DiagnosticSinkCallback>);

impl DiagnosticSink {
    pub fn new(f: impl Fn(&Diagnostic) + 'static) -> Self {
        DiagnosticSink(Some(Rc::new(f)))
    }

    #[inline]
    pub(crate) fn emit(&self, diagnostic: &Diagnostic) {
        if let Some(f) = &self.0 {
            f(diagnostic);
        }
    }
}

impl fmt::Debug for DiagnosticSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DiagnosticSink")
            .field(&self.0.is_some())
            .finish()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompilerHostConfig {
    pub search_paths: Vec<String>,
    pub validation_debug: bool,
}

impl CompilerHostConfig {
    pub fn new(search_paths: impl IntoIterator<Item = String>) -> CaapResult<Self> {
        let mut search_paths: Vec<String> = search_paths.into_iter().collect();
        if search_paths.iter().any(String::is_empty) {
            return Err(CaapError::compiler(
                "compiler host search paths must be non-empty",
            ));
        }
        search_paths.sort();
        search_paths.dedup();
        Ok(Self {
            search_paths,
            validation_debug: false,
        })
    }

    pub fn with_validation_debug(mut self, validation_debug: bool) -> Self {
        self.validation_debug = validation_debug;
        self
    }
}

#[derive(Clone, Debug)]
pub struct CompilerHost {
    pub(super) config: CompilerHostConfig,
    pub(super) runtime_services: HostServiceRegistry,
    pub(super) compile_time_services: HostServiceRegistry,
    pub(super) source_template_cache: HostSourceTemplateCache,
    pub(super) host_version: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompilerNameService {
    pub(super) names: BTreeSet<String>,
    pub(super) version: u64,
}

#[derive(Clone, Debug)]
pub struct CompilerCatalog<'a> {
    pub(super) units: &'a BTreeMap<String, crate::unit::Unit>,
}

impl CompilerHost {
    pub fn new() -> Self {
        Self {
            config: CompilerHostConfig::default(),
            runtime_services: HostServiceRegistry::new(),
            compile_time_services: HostServiceRegistry::new(),
            source_template_cache: Rc::new(Mutex::new(BTreeMap::new())),
            host_version: 0,
        }
    }

    pub fn with_config(config: CompilerHostConfig) -> Self {
        Self {
            config,
            runtime_services: HostServiceRegistry::new(),
            compile_time_services: HostServiceRegistry::new(),
            source_template_cache: Rc::new(Mutex::new(BTreeMap::new())),
            host_version: 0,
        }
    }

    pub fn config(&self) -> &CompilerHostConfig {
        &self.config
    }

    pub fn runtime_services(&self) -> &HostServiceRegistry {
        &self.runtime_services
    }

    pub fn runtime_services_mut(&mut self) -> CaapResult<&mut HostServiceRegistry> {
        self.host_version = self.next_host_version()?;
        Ok(&mut self.runtime_services)
    }

    pub fn register_default_runtime_system_libraries(&mut self) -> CaapResult<()> {
        let host_version = self.next_host_version()?;
        self.runtime_services
            .set_system_policy(HostSystemPolicy::allow_all());
        self.runtime_services
            .set_capability_policy(HostCapabilityPolicy::allow_all());
        self.runtime_services.register_default_system_libraries()?;
        self.host_version = host_version;
        Ok(())
    }

    pub fn compile_time_services(&self) -> &HostServiceRegistry {
        &self.compile_time_services
    }

    pub fn compile_time_services_mut(&mut self) -> CaapResult<&mut HostServiceRegistry> {
        self.host_version = self.next_host_version()?;
        Ok(&mut self.compile_time_services)
    }

    pub fn register_default_compile_time_system_libraries(&mut self) -> CaapResult<()> {
        self.register_default_compile_time_system_libraries_with_read_roots(Vec::new())
    }

    pub fn register_default_compile_time_system_libraries_with_read_roots(
        &mut self,
        read_roots: impl IntoIterator<Item = PathBuf>,
    ) -> CaapResult<()> {
        let host_version = self.next_host_version()?;
        self.compile_time_services
            .set_system_policy(HostSystemPolicy::compile_time_sandbox(Some(
                read_roots.into_iter().collect(),
            )));
        self.compile_time_services
            .set_capability_policy(HostCapabilityPolicy::allow_all());
        self.compile_time_services
            .register_default_system_libraries()?;
        self.host_version = host_version;
        Ok(())
    }

    /// Construct a host with both the default runtime and compile-time system
    /// libraries registered — the standard setup every front-end (CLI, DAP,
    /// LSP) needs before opening a bootstrap session. `read_roots` scopes the
    /// compile-time filesystem sandbox. Centralizes the three-call recipe that
    /// each front-end previously repeated.
    pub fn with_default_system_libraries(
        read_roots: impl IntoIterator<Item = PathBuf>,
    ) -> CaapResult<Self> {
        let mut host = Self::new();
        host.register_default_runtime_system_libraries()?;
        host.register_default_compile_time_system_libraries_with_read_roots(read_roots)?;
        Ok(host)
    }

    pub fn host_version(&self) -> u64 {
        self.host_version
    }

    fn next_host_version(&self) -> CaapResult<u64> {
        self.host_version
            .checked_add(1)
            .ok_or_else(|| CaapError::compiler("compiler host version overflow"))
    }

    pub fn new_session(&self) -> super::session::Compiler {
        super::session::Compiler::new(Rc::new(self.clone()))
    }
}

impl Default for CompilerHost {
    fn default() -> Self {
        Self::new()
    }
}

impl CompilerNameService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, name: impl Into<String>) -> CaapResult<()> {
        let name = name.into();
        if name.is_empty() {
            return Err(CaapError::compiler("compiler name must be non-empty"));
        }
        if !self.names.contains(&name) {
            let version = self.next_version()?;
            self.names.insert(name);
            self.version = version;
        }
        Ok(())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.names.iter().map(String::as_str).collect()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    fn next_version(&self) -> CaapResult<u64> {
        self.version
            .checked_add(1)
            .ok_or_else(|| CaapError::compiler("compiler name service version overflow"))
    }
}

impl<'a> CompilerCatalog<'a> {
    pub fn get_compiled_unit(&self, unit_id: &str) -> CaapResult<Option<&'a crate::unit::Unit>> {
        if unit_id.is_empty() {
            return Err(CaapError::compiler(
                "compiler catalog unit id lookup must be non-empty",
            ));
        }
        Ok(self.units.get(unit_id))
    }

    pub fn unit_ids(&self) -> Vec<&'a str> {
        self.units.keys().map(String::as_str).collect()
    }

    pub fn contains_unit(&self, unit_id: &str) -> bool {
        self.units.contains_key(unit_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_service_rejects_version_overflow_without_mutating() {
        let mut names = CompilerNameService {
            version: u64::MAX,
            ..CompilerNameService::new()
        };

        let error = names.register("overflow.name").unwrap_err().to_string();

        assert!(error.contains("compiler name service version overflow"));
        assert!(!names.contains("overflow.name"));
        assert_eq!(names.version(), u64::MAX);
    }

    #[test]
    fn duplicate_name_registration_is_version_neutral_at_max_version() {
        let mut names = CompilerNameService::new();
        names.register("existing.name").unwrap();
        names.version = u64::MAX;

        names.register("existing.name").unwrap();

        assert!(names.contains("existing.name"));
        assert_eq!(names.version(), u64::MAX);
    }

    #[test]
    fn host_default_runtime_registration_rejects_version_overflow_without_mutating() {
        let mut host = CompilerHost {
            host_version: u64::MAX,
            ..CompilerHost::new()
        };

        let error = host
            .register_default_runtime_system_libraries()
            .unwrap_err()
            .to_string();

        assert!(error.contains("compiler host version overflow"));
        assert_eq!(host.host_version(), u64::MAX);
        assert!(host.runtime_services().library_names().is_empty());
    }

    #[test]
    fn host_default_compile_time_registration_rejects_version_overflow_without_mutating() {
        let mut host = CompilerHost {
            host_version: u64::MAX,
            ..CompilerHost::new()
        };

        let error = host
            .register_default_compile_time_system_libraries()
            .unwrap_err()
            .to_string();

        assert!(error.contains("compiler host version overflow"));
        assert_eq!(host.host_version(), u64::MAX);
        assert!(host.compile_time_services().library_names().is_empty());
    }

    #[test]
    fn host_mutable_service_access_rejects_version_overflow() {
        let mut host = CompilerHost {
            host_version: u64::MAX,
            ..CompilerHost::new()
        };

        let runtime_error = host.runtime_services_mut().unwrap_err().to_string();
        assert!(runtime_error.contains("compiler host version overflow"));
        assert_eq!(host.host_version(), u64::MAX);

        let compile_time_error = host.compile_time_services_mut().unwrap_err().to_string();
        assert!(compile_time_error.contains("compiler host version overflow"));
        assert_eq!(host.host_version(), u64::MAX);
    }
}
