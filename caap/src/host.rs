//! Explicit host service registry substrate.
//!
//! The registry is inert by itself: it does not inject ambient capabilities into
//! evaluation. Callers must explicitly export functions into an environment or
//! pass them through a future compiler/session boundary.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::fd::AsRawFd;

use crate::semantic::PhasePolicy;
use crate::values::{
    eval_err, require_int_strict, require_map, require_str, EnvRef, Environment, HostFunction,
    MapKey, RuntimeValue,
};

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
    pub public: String,
    pub policy: String,
    pub effect: String,
    pub pure: bool,
    pub kind: String,
    pub capability_kind: Option<String>,
    pub signature: HostExportSignature,
    pub min_arity: usize,
    pub max_arity: Option<usize>,
    pub variadic: bool,
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum HostCapabilityPolicy {
    #[default]
    AllowAll,
    AllowOnly(BTreeSet<String>),
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
    name: String,
    exports: BTreeMap<String, HostServiceExport>,
}

#[derive(Clone, Debug)]
pub struct HostServiceRegistry {
    libraries: BTreeMap<String, HostServiceLibrary>,
    capability_policy: HostCapabilityPolicy,
    system_policy: Rc<RefCell<HostSystemPolicy>>,
}

impl HostServiceExport {
    pub fn runtime_value(&self) -> RuntimeValue {
        RuntimeValue::HostFunction(Rc::clone(&self.function))
    }
}

impl Default for HostSystemPolicy {
    fn default() -> Self {
        Self::allow_all()
    }
}

impl Default for HostFileSystemPolicy {
    fn default() -> Self {
        Self::allow_all()
    }
}

impl Default for HostIoPolicy {
    fn default() -> Self {
        Self {
            allow_stdin_read: true,
        }
    }
}

impl Default for HostProcessPolicy {
    fn default() -> Self {
        Self { allow_spawn: true }
    }
}

impl Default for HostNetworkPolicy {
    fn default() -> Self {
        Self {
            allow_listen: true,
            allow_connect: true,
        }
    }
}

impl Default for HostServiceRegistry {
    fn default() -> Self {
        Self {
            libraries: BTreeMap::new(),
            capability_policy: HostCapabilityPolicy::default(),
            system_policy: Rc::new(RefCell::new(HostSystemPolicy::default())),
        }
    }
}

impl HostCapabilityPolicy {
    pub fn allow_all() -> Self {
        Self::AllowAll
    }

    pub fn allow_only(capabilities: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut allowed = BTreeSet::new();
        for capability in capabilities {
            if capability.is_empty() {
                return Err("host capability name must be non-empty".to_string());
            }
            allowed.insert(capability);
        }
        Ok(Self::AllowOnly(allowed))
    }

    pub fn allows(&self, library: &str, export: &str) -> bool {
        match self {
            Self::AllowAll => true,
            Self::AllowOnly(allowed) => {
                allowed.contains("*")
                    || allowed.contains(&format!("{library}.*"))
                    || allowed.contains(&format!("{library}.{export}"))
            }
        }
    }
}

impl HostExportParameter {
    fn new(name: impl Into<String>, type_name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            type_name: type_name.into(),
        }
    }
}

impl HostSystemPolicy {
    pub fn allow_all() -> Self {
        Self {
            fs: HostFileSystemPolicy::allow_all(),
            io: HostIoPolicy::default(),
            process: HostProcessPolicy::default(),
            net: HostNetworkPolicy::default(),
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

impl HostOsEnvironmentPolicy {
    pub fn allow_only(names: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut allowlist = BTreeSet::new();
        for name in names {
            if name.is_empty() {
                return Err("OS environment allowlist entries must be non-empty".to_string());
            }
            allowlist.insert(name);
        }
        Ok(Self {
            allowlist: Some(allowlist),
        })
    }
}

impl HostServiceLibrary {
    pub fn new(name: impl Into<String>) -> Result<Self, String> {
        let name = name.into();
        if name.is_empty() {
            return Err("host service library name must be non-empty".to_string());
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

    pub fn export(&self, name: &str) -> Result<Option<&HostServiceExport>, String> {
        if name.is_empty() {
            return Err("host service export name must be non-empty".to_string());
        }
        Ok(self.exports.get(name))
    }

    pub fn register_function(
        &mut self,
        name: impl Into<String>,
        phase: PhasePolicy,
        function: HostFunction,
    ) -> Result<(), String> {
        let name = name.into();
        if name.is_empty() {
            return Err("host service export name must be non-empty".to_string());
        }
        let export = HostServiceExport {
            library: self.name.clone(),
            name: name.clone(),
            phase,
            metadata: host_export_metadata(&self.name, &name, &function),
            function: Rc::new(function),
        };
        self.exports.insert(name, export);
        Ok(())
    }
}

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
    }

    pub fn set_system_policy(&mut self, policy: HostSystemPolicy) {
        *self.system_policy.borrow_mut() = policy;
    }

    pub fn allow_only_capabilities(
        &mut self,
        capabilities: impl IntoIterator<Item = String>,
    ) -> Result<(), String> {
        self.capability_policy = HostCapabilityPolicy::allow_only(capabilities)?;
        Ok(())
    }

    pub fn library(&self, name: &str) -> Result<Option<&HostServiceLibrary>, String> {
        if name.is_empty() {
            return Err("host service library name must be non-empty".to_string());
        }
        Ok(self.libraries.get(name))
    }

    pub fn register_function(
        &mut self,
        library: impl Into<String>,
        export: impl Into<String>,
        phase: PhasePolicy,
        function: HostFunction,
    ) -> Result<(), String> {
        let library = library.into();
        if library.is_empty() {
            return Err("host service library name must be non-empty".to_string());
        }
        let entry = match self.libraries.get_mut(&library) {
            Some(entry) => entry,
            None => {
                self.libraries
                    .insert(library.clone(), HostServiceLibrary::new(library.clone())?);
                self.libraries
                    .get_mut(&library)
                    .expect("inserted host service library must exist")
            }
        };
        entry.register_function(export, phase, function)
    }

    pub fn export(
        &self,
        library: &str,
        name: &str,
        phase: PhasePolicy,
    ) -> Result<RuntimeValue, String> {
        let library_entry = self
            .library(library)?
            .ok_or_else(|| format!("host service library does not exist: {library}"))?;
        let export = library_entry
            .export(name)?
            .ok_or_else(|| format!("host service export does not exist: {library}.{name}"))?;
        if export.phase != PhasePolicy::Dual && export.phase != phase {
            return Err(format!(
                "host service export {library}.{name} is not available in phase {}",
                phase.as_str()
            ));
        }
        if !self.capability_policy.allows(library, name) {
            return Err(format!("host capability denied: {library}.{name}"));
        }
        Ok(export.runtime_value())
    }

    pub fn export_library_to_environment(
        &self,
        library: &str,
        phase: PhasePolicy,
        env: &EnvRef,
    ) -> Result<Vec<String>, String> {
        let library_entry = self
            .library(library)?
            .ok_or_else(|| format!("host service library does not exist: {library}"))?;
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

    pub fn register_default_system_libraries(&mut self) -> Result<(), String> {
        self.register_format_library()?;
        self.register_path_library()?;
        self.register_io_library()?;
        self.register_env_library()?;
        self.register_net_library()?;
        self.register_os_library()?;
        self.register_process_library()?;
        self.register_time_library()?;
        self.register_fs_library()?;
        Ok(())
    }

    pub fn register_format_library(&mut self) -> Result<(), String> {
        self.register_function(
            "format",
            "format",
            PhasePolicy::Dual,
            HostFunction::new(
                "format.format",
                1,
                None,
                Box::new(|args| {
                    let template = require_str(&args[0], "format.format")?;
                    let mut rendered = template.to_string();
                    for value in args.iter().skip(1) {
                        rendered = rendered.replacen("{}", &value.to_string(), 1);
                    }
                    Ok(RuntimeValue::Str(rendered.into()))
                }),
            )?,
        )?;
        Ok(())
    }

    pub fn register_path_library(&mut self) -> Result<(), String> {
        self.register_function(
            "path",
            "join",
            PhasePolicy::Runtime,
            HostFunction::new(
                "path.join",
                1,
                None,
                Box::new(|args| {
                    let mut path = PathBuf::new();
                    for arg in &args {
                        path.push(require_str(arg, "path.join")?.as_ref());
                    }
                    Ok(RuntimeValue::Str(path_to_string(path)?.into()))
                }),
            )?,
        )?;
        self.register_function(
            "path",
            "basename",
            PhasePolicy::Runtime,
            HostFunction::new(
                "path.basename",
                1,
                Some(1),
                Box::new(|args| {
                    let path = require_str(&args[0], "path.basename")?;
                    let name = Path::new(path.as_ref())
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("");
                    Ok(RuntimeValue::Str(name.into()))
                }),
            )?,
        )?;
        self.register_function(
            "path",
            "dirname",
            PhasePolicy::Runtime,
            HostFunction::new(
                "path.dirname",
                1,
                Some(1),
                Box::new(|args| {
                    let path = require_str(&args[0], "path.dirname")?;
                    let parent = Path::new(path.as_ref())
                        .parent()
                        .map(path_to_string)
                        .transpose()?
                        .unwrap_or_default();
                    Ok(RuntimeValue::Str(parent.into()))
                }),
            )?,
        )?;
        Ok(())
    }

    pub fn register_env_library(&mut self) -> Result<(), String> {
        self.register_function(
            "env",
            "get",
            PhasePolicy::Runtime,
            HostFunction::new(
                "env.get",
                1,
                Some(1),
                Box::new(|args| {
                    let name = require_str(&args[0], "env.get")?;
                    match std::env::var(name.as_ref()) {
                        Ok(value) => Ok(RuntimeValue::Str(value.into())),
                        Err(std::env::VarError::NotPresent) => Ok(RuntimeValue::Null),
                        Err(error) => Err(eval_err(format!("env.get: {error}"))),
                    }
                }),
            )?,
        )?;
        Ok(())
    }

    pub fn register_io_library(&mut self) -> Result<(), String> {
        self.register_function(
            "io",
            "print",
            PhasePolicy::Dual,
            HostFunction::new("io.print", 1, Some(1), Box::new(io_print_stdout))?,
        )?;
        self.register_function(
            "io",
            "println",
            PhasePolicy::Dual,
            HostFunction::new("io.println", 1, Some(1), Box::new(io_println_stdout))?,
        )?;
        self.register_function(
            "io",
            "write",
            PhasePolicy::Dual,
            HostFunction::new("io.write", 1, Some(1), Box::new(io_write_stdout))?,
        )?;
        self.register_function(
            "io",
            "eprint",
            PhasePolicy::Dual,
            HostFunction::new("io.eprint", 1, Some(1), Box::new(io_print_stderr))?,
        )?;
        self.register_function(
            "io",
            "eprintln",
            PhasePolicy::Dual,
            HostFunction::new("io.eprintln", 1, Some(1), Box::new(io_println_stderr))?,
        )?;
        self.register_function(
            "io",
            "flush-stdout",
            PhasePolicy::Dual,
            HostFunction::new("io.flush-stdout", 0, Some(0), Box::new(io_flush_stdout))?,
        )?;
        self.register_function(
            "io",
            "flush-stderr",
            PhasePolicy::Dual,
            HostFunction::new("io.flush-stderr", 0, Some(0), Box::new(io_flush_stderr))?,
        )?;
        self.register_function(
            "io",
            "write-string",
            PhasePolicy::Dual,
            HostFunction::new("io.write-string", 1, Some(1), Box::new(io_write_stdout))?,
        )?;
        self.register_function(
            "io",
            "write-line",
            PhasePolicy::Dual,
            HostFunction::new("io.write-line", 1, Some(1), Box::new(io_println_stdout))?,
        )?;
        self.register_function(
            "io",
            "read-line",
            PhasePolicy::Dual,
            HostFunction::new("io.read-line", 0, Some(0), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| io_read_line(args, &policy))
            })?,
        )?;
        self.register_function(
            "io",
            "read-all",
            PhasePolicy::Dual,
            HostFunction::new("io.read-all", 0, Some(0), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| io_read_all(args, &policy))
            })?,
        )?;
        Ok(())
    }

    pub fn register_net_library(&mut self) -> Result<(), String> {
        let net_handles = Rc::new(RefCell::new(NetHandleState::default()));
        self.register_function(
            "net",
            "is-ip",
            PhasePolicy::Runtime,
            HostFunction::new(
                "net.is-ip",
                1,
                Some(1),
                Box::new(|args| {
                    let address = require_str(&args[0], "net.is-ip")?;
                    Ok(RuntimeValue::Bool(address.parse::<IpAddr>().is_ok()))
                }),
            )?,
        )?;
        self.register_function(
            "net",
            "is-loopback",
            PhasePolicy::Runtime,
            HostFunction::new(
                "net.is-loopback",
                1,
                Some(1),
                Box::new(|args| {
                    let address = require_str(&args[0], "net.is-loopback")?;
                    let address = address
                        .parse::<IpAddr>()
                        .map_err(|error| eval_err(format!("net.is-loopback: {error}")))?;
                    Ok(RuntimeValue::Bool(address.is_loopback()))
                }),
            )?,
        )?;
        self.register_function(
            "net",
            "host-port",
            PhasePolicy::Runtime,
            HostFunction::new(
                "net.host-port",
                2,
                Some(2),
                Box::new(|args| {
                    let host = require_str(&args[0], "net.host-port")?;
                    let port = require_int_strict(&args[1], "net.host-port")?;
                    if !(0..=65535).contains(&port) {
                        return Err(eval_err("net.host-port: port must be in 0..=65535"));
                    }
                    if host.contains(':') && !host.starts_with('[') {
                        Ok(RuntimeValue::Str(format!("[{host}]:{port}").into()))
                    } else {
                        Ok(RuntimeValue::Str(format!("{host}:{port}").into()))
                    }
                }),
            )?,
        )?;
        self.register_function(
            "net",
            "listen",
            PhasePolicy::Runtime,
            HostFunction::new("net.listen", 1, Some(1), {
                let net_handles = Rc::clone(&net_handles);
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    require_net_listen_allowed(&policy, "net.listen")?;
                    let spec = net_listen_spec(&args[0])?;
                    let listener = tcp_listen(&spec)?;
                    let handle = net_handles.borrow_mut().allocate_listener_handle(listener);
                    Ok(RuntimeValue::Int(handle))
                })
            })?,
        )?;
        self.register_function(
            "net",
            "accept",
            PhasePolicy::Runtime,
            HostFunction::new("net.accept", 1, Some(1), {
                let net_handles = Rc::clone(&net_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "net.accept")?;
                    let mut handles = net_handles.borrow_mut();
                    let listener = handles.listener_handle_mut(handle, "net.accept")?;
                    let (stream, _) = listener
                        .accept()
                        .map_err(|error| eval_err(format!("net.accept: {error}")))?;
                    let handle = handles.allocate_socket_handle(stream);
                    Ok(RuntimeValue::Int(handle))
                })
            })?,
        )?;
        self.register_function(
            "net",
            "connect",
            PhasePolicy::Runtime,
            HostFunction::new("net.connect", 1, Some(1), {
                let net_handles = Rc::clone(&net_handles);
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    require_net_connect_allowed(&policy, "net.connect")?;
                    let spec = net_connect_spec(&args[0])?;
                    let stream = TcpStream::connect((spec.host.as_str(), spec.port))
                        .map_err(|error| eval_err(format!("net.connect: {error}")))?;
                    let handle = net_handles.borrow_mut().allocate_socket_handle(stream);
                    Ok(RuntimeValue::Int(handle))
                })
            })?,
        )?;
        self.register_function(
            "net",
            "read",
            PhasePolicy::Runtime,
            HostFunction::new("net.read", 2, Some(2), {
                let net_handles = Rc::clone(&net_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "net.read")?;
                    let max_bytes = require_int_strict(&args[1], "net.read")?;
                    if max_bytes <= 0 {
                        return Err(eval_err("net.read: max_bytes must be a positive int"));
                    }
                    let mut buffer = vec![0_u8; max_bytes as usize];
                    let mut handles = net_handles.borrow_mut();
                    let stream = handles.socket_handle_mut(handle, "net.read")?;
                    let read = stream
                        .read(&mut buffer)
                        .map_err(|error| eval_err(format!("net.read: {error}")))?;
                    buffer.truncate(read);
                    let text = String::from_utf8(buffer)
                        .map_err(|error| eval_err(format!("net.read: {error}")))?;
                    Ok(RuntimeValue::Str(text.into()))
                })
            })?,
        )?;
        self.register_function(
            "net",
            "write",
            PhasePolicy::Runtime,
            HostFunction::new("net.write", 2, Some(2), {
                let net_handles = Rc::clone(&net_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "net.write")?;
                    let text = require_str(&args[1], "net.write")?;
                    let mut handles = net_handles.borrow_mut();
                    let stream = handles.socket_handle_mut(handle, "net.write")?;
                    stream
                        .write_all(text.as_ref().as_bytes())
                        .map_err(|error| eval_err(format!("net.write: {error}")))?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "net",
            "close",
            PhasePolicy::Runtime,
            HostFunction::new("net.close", 1, Some(1), {
                let net_handles = Rc::clone(&net_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "net.close")?;
                    net_handles
                        .borrow_mut()
                        .remove_any_handle(handle, "net.close")?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "net",
            "poll",
            PhasePolicy::Runtime,
            HostFunction::new("net.poll", 2, Some(2), {
                let net_handles = Rc::clone(&net_handles);
                Box::new(move |args| {
                    let handles = runtime_handle_sequence(&args[0], "net.poll")?;
                    let timeout_ms = require_int_strict(&args[1], "net.poll")?;
                    if timeout_ms < 0 {
                        return Err(eval_err("net.poll: timeout_ms must be a non-negative int"));
                    }
                    net_poll(&net_handles.borrow(), &handles, timeout_ms)
                })
            })?,
        )?;
        Ok(())
    }

    pub fn register_os_library(&mut self) -> Result<(), String> {
        self.register_function(
            "os",
            "env-get",
            PhasePolicy::Dual,
            HostFunction::new("os.env-get", 1, Some(1), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let name = require_str(&args[0], "os.env-get")?;
                    if !env_name_allowed(&policy, name.as_ref()) {
                        return Ok(RuntimeValue::Null);
                    }
                    match std::env::var(name.as_ref()) {
                        Ok(value) => Ok(RuntimeValue::Str(value.into())),
                        Err(std::env::VarError::NotPresent) => Ok(RuntimeValue::Null),
                        Err(error) => Err(eval_err(format!("os.env-get: {error}"))),
                    }
                })
            })?,
        )?;
        self.register_function(
            "os",
            "env-has",
            PhasePolicy::Dual,
            HostFunction::new("os.env-has", 1, Some(1), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let name = require_str(&args[0], "os.env-has")?;
                    if !env_name_allowed(&policy, name.as_ref()) {
                        return Ok(RuntimeValue::Bool(false));
                    }
                    Ok(RuntimeValue::Bool(
                        std::env::var_os(name.as_ref()).is_some(),
                    ))
                })
            })?,
        )?;
        self.register_function(
            "os",
            "env-keys",
            PhasePolicy::Dual,
            HostFunction::new("os.env-keys", 0, Some(0), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |_| {
                    let mut keys = std::env::vars()
                        .map(|(key, _)| key)
                        .filter(|key| env_name_allowed(&policy, key))
                        .map(|key| RuntimeValue::Str(key.into()))
                        .collect::<Vec<_>>();
                    keys.sort_by_key(|left| left.to_string());
                    Ok(RuntimeValue::List(Rc::new(RefCell::new(keys))))
                })
            })?,
        )?;
        self.register_function(
            "os",
            "env-vars",
            PhasePolicy::Dual,
            HostFunction::new("os.env-vars", 0, Some(0), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |_| {
                    let mut map = BTreeMap::new();
                    for (key, value) in std::env::vars() {
                        if !env_name_allowed(&policy, &key) {
                            continue;
                        }
                        map.insert(key, value);
                    }
                    Ok(RuntimeValue::Map(Rc::new(RefCell::new(
                        map.into_iter()
                            .map(|(key, value)| {
                                (MapKey::Str(key.into()), RuntimeValue::Str(value.into()))
                            })
                            .collect(),
                    ))))
                })
            })?,
        )?;
        self.register_function(
            "os",
            "getcwd",
            PhasePolicy::Dual,
            HostFunction::new("os.getcwd", 0, Some(0), Box::new(os_current_dir))?,
        )?;
        self.register_function(
            "os",
            "current-exe",
            PhasePolicy::Dual,
            HostFunction::new(
                "os.current-exe",
                0,
                Some(0),
                Box::new(|_| {
                    let path = std::env::current_exe()
                        .map_err(|error| eval_err(format!("os.current-exe: {error}")))?;
                    Ok(RuntimeValue::Str(path_to_string(path)?.into()))
                }),
            )?,
        )?;
        self.register_function(
            "os",
            "temp-dir",
            PhasePolicy::Dual,
            HostFunction::new(
                "os.temp-dir",
                0,
                Some(0),
                Box::new(|_| {
                    Ok(RuntimeValue::Str(
                        path_to_string(std::env::temp_dir())?.into(),
                    ))
                }),
            )?,
        )?;
        self.register_function(
            "os",
            "platform",
            PhasePolicy::Dual,
            HostFunction::new(
                "os.platform",
                0,
                Some(0),
                Box::new(|_| Ok(RuntimeValue::Str(std::env::consts::OS.into()))),
            )?,
        )?;
        self.register_function(
            "os",
            "arch",
            PhasePolicy::Dual,
            HostFunction::new(
                "os.arch",
                0,
                Some(0),
                Box::new(|_| Ok(RuntimeValue::Str(std::env::consts::ARCH.into()))),
            )?,
        )?;
        self.register_function(
            "os",
            "current-dir",
            PhasePolicy::Dual,
            HostFunction::new("os.current-dir", 0, Some(0), Box::new(os_current_dir))?,
        )?;
        Ok(())
    }

    pub fn register_process_library(&mut self) -> Result<(), String> {
        let process_handles = Rc::new(RefCell::new(ProcessHandleState::default()));
        self.register_function(
            "process",
            "id",
            PhasePolicy::Runtime,
            HostFunction::new(
                "process.id",
                0,
                Some(0),
                Box::new(|_| Ok(RuntimeValue::Int(std::process::id() as i64))),
            )?,
        )?;
        self.register_function(
            "process",
            "args",
            PhasePolicy::Runtime,
            HostFunction::new(
                "process.args",
                0,
                Some(0),
                Box::new(|_| {
                    Ok(RuntimeValue::List(Rc::new(RefCell::new(
                        std::env::args()
                            .map(|arg| RuntimeValue::Str(arg.into()))
                            .collect(),
                    ))))
                }),
            )?,
        )?;
        self.register_function(
            "process",
            "run",
            PhasePolicy::Runtime,
            HostFunction::new("process.run", 1, Some(1), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    require_process_spawn_allowed(&policy, "process.run")?;
                    let spec = process_spec(&args[0], "process.run")?;
                    let output = process_command(&spec)
                        .output()
                        .map_err(|error| eval_err(format!("process.run: {error}")))?;
                    completed_process_value(
                        output.status,
                        String::from_utf8_lossy(&output.stdout).into_owned(),
                        String::from_utf8_lossy(&output.stderr).into_owned(),
                    )
                })
            })?,
        )?;
        self.register_function(
            "process",
            "spawn",
            PhasePolicy::Runtime,
            HostFunction::new("process.spawn", 1, Some(1), {
                let process_handles = Rc::clone(&process_handles);
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    require_process_spawn_allowed(&policy, "process.spawn")?;
                    let spec = process_spec(&args[0], "process.spawn")?;
                    let mut command = process_command(&spec);
                    command.stdin(if spec.inherit_stdin {
                        Stdio::inherit()
                    } else {
                        Stdio::piped()
                    });
                    command.stdout(if spec.inherit_stdout || !spec.capture_stdout {
                        Stdio::inherit()
                    } else {
                        Stdio::piped()
                    });
                    command.stderr(if spec.inherit_stderr || !spec.capture_stderr {
                        Stdio::inherit()
                    } else {
                        Stdio::piped()
                    });
                    let child = command
                        .spawn()
                        .map_err(|error| eval_err(format!("process.spawn: {error}")))?;
                    let handle = process_handles.borrow_mut().allocate_process_handle(child);
                    Ok(RuntimeValue::Int(handle))
                })
            })?,
        )?;
        self.register_function(
            "process",
            "wait",
            PhasePolicy::Runtime,
            HostFunction::new("process.wait", 1, Some(1), {
                let process_handles = Rc::clone(&process_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "process.wait")?;
                    let child = process_handles
                        .borrow_mut()
                        .remove_process_handle(handle, "process.wait")?;
                    process_wait_value(child, "process.wait")
                })
            })?,
        )?;
        self.register_function(
            "process",
            "wait-result",
            PhasePolicy::Runtime,
            HostFunction::new("process.wait-result", 1, Some(1), {
                let process_handles = Rc::clone(&process_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "process.wait-result")?;
                    let mut handles = process_handles.borrow_mut();
                    let Some(child) = handles.process_handles.get_mut(&handle) else {
                        return Err(eval_err(format!(
                            "process.wait-result: unknown process handle {handle}"
                        )));
                    };
                    match child
                        .try_wait()
                        .map_err(|error| eval_err(format!("process.wait-result: {error}")))?
                    {
                        None => Ok(RuntimeValue::Null),
                        Some(status) => {
                            let child = handles
                                .process_handles
                                .remove(&handle)
                                .expect("process handle observed by try_wait must still exist");
                            process_completed_child_value(child, status, "process.wait-result")
                        }
                    }
                })
            })?,
        )?;
        self.register_function(
            "process",
            "kill",
            PhasePolicy::Runtime,
            HostFunction::new("process.kill", 1, Some(1), {
                let process_handles = Rc::clone(&process_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "process.kill")?;
                    let mut handles = process_handles.borrow_mut();
                    let child = handles.process_handle_mut(handle, "process.kill")?;
                    child
                        .kill()
                        .map_err(|error| eval_err(format!("process.kill: {error}")))?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "process",
            "write-stdin",
            PhasePolicy::Runtime,
            HostFunction::new("process.write-stdin", 2, Some(2), {
                let process_handles = Rc::clone(&process_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "process.write-stdin")?;
                    let text = require_str(&args[1], "process.write-stdin")?;
                    let mut handles = process_handles.borrow_mut();
                    let child = handles.process_handle_mut(handle, "process.write-stdin")?;
                    let stdin = child.stdin.as_mut().ok_or_else(|| {
                        eval_err("process.write-stdin: stdin is not captured for this process")
                    })?;
                    stdin
                        .write_all(text.as_ref().as_bytes())
                        .map_err(|error| eval_err(format!("process.write-stdin: {error}")))?;
                    stdin
                        .flush()
                        .map_err(|error| eval_err(format!("process.write-stdin: {error}")))?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "process",
            "close-stdin",
            PhasePolicy::Runtime,
            HostFunction::new("process.close-stdin", 1, Some(1), {
                let process_handles = Rc::clone(&process_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "process.close-stdin")?;
                    let mut handles = process_handles.borrow_mut();
                    let child = handles.process_handle_mut(handle, "process.close-stdin")?;
                    child.stdin.take();
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "process",
            "read-stdout",
            PhasePolicy::Runtime,
            HostFunction::new("process.read-stdout", 1, Some(1), {
                let process_handles = Rc::clone(&process_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "process.read-stdout")?;
                    let mut handles = process_handles.borrow_mut();
                    let child = handles.process_handle_mut(handle, "process.read-stdout")?;
                    let stdout = child.stdout.as_mut().ok_or_else(|| {
                        eval_err("process.read-stdout: stdout is not captured for this process")
                    })?;
                    let mut text = String::new();
                    stdout
                        .read_to_string(&mut text)
                        .map_err(|error| eval_err(format!("process.read-stdout: {error}")))?;
                    Ok(RuntimeValue::Str(text.into()))
                })
            })?,
        )?;
        self.register_function(
            "process",
            "read-stderr",
            PhasePolicy::Runtime,
            HostFunction::new("process.read-stderr", 1, Some(1), {
                let process_handles = Rc::clone(&process_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "process.read-stderr")?;
                    let mut handles = process_handles.borrow_mut();
                    let child = handles.process_handle_mut(handle, "process.read-stderr")?;
                    let stderr = child.stderr.as_mut().ok_or_else(|| {
                        eval_err("process.read-stderr: stderr is not captured for this process")
                    })?;
                    let mut text = String::new();
                    stderr
                        .read_to_string(&mut text)
                        .map_err(|error| eval_err(format!("process.read-stderr: {error}")))?;
                    Ok(RuntimeValue::Str(text.into()))
                })
            })?,
        )?;
        Ok(())
    }

    pub fn register_time_library(&mut self) -> Result<(), String> {
        self.register_function(
            "time",
            "now-unix-ns",
            PhasePolicy::Runtime,
            HostFunction::new(
                "time.now-unix-ns",
                0,
                Some(0),
                Box::new(|_| {
                    let duration = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|error| eval_err(format!("time.now-unix-ns: {error}")))?;
                    Ok(RuntimeValue::Int(duration.as_nanos() as i64))
                }),
            )?,
        )?;
        self.register_function(
            "time",
            "unix-millis",
            PhasePolicy::Runtime,
            HostFunction::new(
                "time.unix-millis",
                0,
                Some(0),
                Box::new(|_| {
                    let duration = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|error| eval_err(format!("time.unix-millis: {error}")))?;
                    Ok(RuntimeValue::Int(duration.as_millis() as i64))
                }),
            )?,
        )?;
        Ok(())
    }

    pub fn register_fs_library(&mut self) -> Result<(), String> {
        let fs_handles = Rc::new(RefCell::new(FsHandleState::default()));
        self.register_function(
            "fs",
            "exists",
            PhasePolicy::Runtime,
            HostFunction::new("fs.exists", 1, Some(1), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let path = policy_checked_fs_path(&policy, &args[0], "read", "fs.exists")?;
                    Ok(RuntimeValue::Bool(path.exists()))
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "read-text",
            PhasePolicy::Runtime,
            HostFunction::new("fs.read-text", 1, Some(1), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let path = policy_checked_fs_path(&policy, &args[0], "read", "fs.read-text")?;
                    let text = std::fs::read_to_string(&path)
                        .map_err(|error| eval_err(format!("fs.read-text: {error}")))?;
                    Ok(RuntimeValue::Str(text.into()))
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "write-text",
            PhasePolicy::Runtime,
            HostFunction::new("fs.write-text", 2, Some(2), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let path = policy_checked_fs_path(&policy, &args[0], "write", "fs.write-text")?;
                    let text = require_str(&args[1], "fs.write-text")?;
                    std::fs::write(&path, text.as_ref())
                        .map_err(|error| eval_err(format!("fs.write-text: {error}")))?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "append-text",
            PhasePolicy::Runtime,
            HostFunction::new("fs.append-text", 2, Some(2), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let path =
                        policy_checked_fs_path(&policy, &args[0], "write", "fs.append-text")?;
                    let text = require_str(&args[1], "fs.append-text")?;
                    let mut file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .map_err(|error| eval_err(format!("fs.append-text: {error}")))?;
                    file.write_all(text.as_ref().as_bytes())
                        .map_err(|error| eval_err(format!("fs.append-text: {error}")))?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "is-file",
            PhasePolicy::Runtime,
            HostFunction::new("fs.is-file", 1, Some(1), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let path = policy_checked_fs_path(&policy, &args[0], "read", "fs.is-file")?;
                    Ok(RuntimeValue::Bool(path.is_file()))
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "is-dir",
            PhasePolicy::Runtime,
            HostFunction::new("fs.is-dir", 1, Some(1), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let path = policy_checked_fs_path(&policy, &args[0], "read", "fs.is-dir")?;
                    Ok(RuntimeValue::Bool(path.is_dir()))
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "metadata",
            PhasePolicy::Runtime,
            HostFunction::new("fs.metadata", 1, Some(1), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let path = policy_checked_fs_path(&policy, &args[0], "read", "fs.metadata")?;
                    fs_metadata_value(&path)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "canonicalize",
            PhasePolicy::Runtime,
            HostFunction::new("fs.canonicalize", 1, Some(1), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let path =
                        policy_checked_fs_path(&policy, &args[0], "read", "fs.canonicalize")?;
                    let path = std::fs::canonicalize(&path)
                        .map_err(|error| eval_err(format!("fs.canonicalize: {error}")))?;
                    Ok(RuntimeValue::Str(path_to_string(path)?.into()))
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "list-dir",
            PhasePolicy::Runtime,
            HostFunction::new("fs.list-dir", 1, Some(1), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let path = policy_checked_fs_path(&policy, &args[0], "read", "fs.list-dir")?;
                    fs_list_dir_value(&path)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "create-dir",
            PhasePolicy::Runtime,
            HostFunction::new("fs.create-dir", 1, Some(1), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let path = policy_checked_fs_path(&policy, &args[0], "write", "fs.create-dir")?;
                    std::fs::create_dir(&path)
                        .map_err(|error| eval_err(format!("fs.create-dir: {error}")))?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "create-dir-all",
            PhasePolicy::Runtime,
            HostFunction::new("fs.create-dir-all", 1, Some(1), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let path =
                        policy_checked_fs_path(&policy, &args[0], "write", "fs.create-dir-all")?;
                    std::fs::create_dir_all(&path)
                        .map_err(|error| eval_err(format!("fs.create-dir-all: {error}")))?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "remove-file",
            PhasePolicy::Runtime,
            HostFunction::new("fs.remove-file", 1, Some(1), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let path =
                        policy_checked_fs_path(&policy, &args[0], "write", "fs.remove-file")?;
                    std::fs::remove_file(&path)
                        .map_err(|error| eval_err(format!("fs.remove-file: {error}")))?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "remove-dir",
            PhasePolicy::Runtime,
            HostFunction::new("fs.remove-dir", 1, Some(1), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let path = policy_checked_fs_path(&policy, &args[0], "write", "fs.remove-dir")?;
                    std::fs::remove_dir(&path)
                        .map_err(|error| eval_err(format!("fs.remove-dir: {error}")))?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "remove-dir-all",
            PhasePolicy::Runtime,
            HostFunction::new("fs.remove-dir-all", 1, Some(1), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let path =
                        policy_checked_fs_path(&policy, &args[0], "write", "fs.remove-dir-all")?;
                    std::fs::remove_dir_all(&path)
                        .map_err(|error| eval_err(format!("fs.remove-dir-all: {error}")))?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "rename",
            PhasePolicy::Runtime,
            HostFunction::new("fs.rename", 2, Some(2), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let source = policy_checked_fs_path(&policy, &args[0], "read", "fs.rename")?;
                    let target = policy_checked_fs_path(&policy, &args[1], "write", "fs.rename")?;
                    std::fs::rename(&source, &target)
                        .map_err(|error| eval_err(format!("fs.rename: {error}")))?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "copy-file",
            PhasePolicy::Runtime,
            HostFunction::new("fs.copy-file", 2, Some(2), {
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let source = policy_checked_fs_path(&policy, &args[0], "read", "fs.copy-file")?;
                    let target =
                        policy_checked_fs_path(&policy, &args[1], "write", "fs.copy-file")?;
                    std::fs::copy(&source, &target)
                        .map_err(|error| eval_err(format!("fs.copy-file: {error}")))?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "open-file",
            PhasePolicy::Runtime,
            HostFunction::new("fs.open-file", 1, Some(1), {
                let fs_handles = Rc::clone(&fs_handles);
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| fs_open_file(&fs_handles, &policy, &args[0]))
            })?,
        )?;
        self.register_function(
            "fs",
            "close-file",
            PhasePolicy::Runtime,
            HostFunction::new("fs.close-file", 1, Some(1), {
                let fs_handles = Rc::clone(&fs_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "fs.close-file")?;
                    fs_handles
                        .borrow_mut()
                        .file_handles
                        .remove(&handle)
                        .ok_or_else(|| {
                            eval_err(format!("fs.close-file: unknown file handle {handle}"))
                        })?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "file-read-all-text",
            PhasePolicy::Runtime,
            HostFunction::new("fs.file-read-all-text", 1, Some(1), {
                let fs_handles = Rc::clone(&fs_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "fs.file-read-all-text")?;
                    let mut handles = fs_handles.borrow_mut();
                    let file = handles.file_handle_mut(handle, "fs.file-read-all-text")?;
                    let mut text = String::new();
                    file.file
                        .read_to_string(&mut text)
                        .map_err(|error| eval_err(format!("fs.file-read-all-text: {error}")))?;
                    Ok(RuntimeValue::Str(text.into()))
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "file-read-line",
            PhasePolicy::Runtime,
            HostFunction::new("fs.file-read-line", 1, Some(1), {
                let fs_handles = Rc::clone(&fs_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "fs.file-read-line")?;
                    let mut handles = fs_handles.borrow_mut();
                    let file = handles.file_handle_mut(handle, "fs.file-read-line")?;
                    fs_file_read_line(file)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "file-write",
            PhasePolicy::Runtime,
            HostFunction::new("fs.file-write", 2, Some(2), {
                let fs_handles = Rc::clone(&fs_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "fs.file-write")?;
                    let text = require_str(&args[1], "fs.file-write")?;
                    let mut handles = fs_handles.borrow_mut();
                    let file = handles.file_handle_mut(handle, "fs.file-write")?;
                    file.file
                        .write_all(text.as_ref().as_bytes())
                        .map_err(|error| eval_err(format!("fs.file-write: {error}")))?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "file-flush",
            PhasePolicy::Runtime,
            HostFunction::new("fs.file-flush", 1, Some(1), {
                let fs_handles = Rc::clone(&fs_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "fs.file-flush")?;
                    let mut handles = fs_handles.borrow_mut();
                    let file = handles.file_handle_mut(handle, "fs.file-flush")?;
                    file.file
                        .flush()
                        .map_err(|error| eval_err(format!("fs.file-flush: {error}")))?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "file-seek",
            PhasePolicy::Runtime,
            HostFunction::new("fs.file-seek", 2, Some(3), {
                let fs_handles = Rc::clone(&fs_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "fs.file-seek")?;
                    let offset = require_int_strict(&args[1], "fs.file-seek")?;
                    let whence = match args.get(2) {
                        None | Some(RuntimeValue::Null) => "start",
                        Some(value) => require_str(value, "fs.file-seek")?.as_ref(),
                    };
                    let seek_from = match whence {
                        "start" if offset >= 0 => SeekFrom::Start(offset as u64),
                        "current" => SeekFrom::Current(offset),
                        "end" => SeekFrom::End(offset),
                        "start" => {
                            return Err(eval_err(
                                "fs.file-seek: start offset must be non-negative",
                            ));
                        }
                        _ => {
                            return Err(eval_err(
                                "fs.file-seek: whence must be start, current, or end",
                            ));
                        }
                    };
                    let mut handles = fs_handles.borrow_mut();
                    let file = handles.file_handle_mut(handle, "fs.file-seek")?;
                    let position = file
                        .file
                        .seek(seek_from)
                        .map_err(|error| eval_err(format!("fs.file-seek: {error}")))?;
                    Ok(RuntimeValue::Int(position as i64))
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "file-metadata",
            PhasePolicy::Runtime,
            HostFunction::new("fs.file-metadata", 1, Some(1), {
                let fs_handles = Rc::clone(&fs_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "fs.file-metadata")?;
                    let handles = fs_handles.borrow();
                    let file = handles.file_handle(handle, "fs.file-metadata")?;
                    fs_metadata_value(&file.path)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "open-dir",
            PhasePolicy::Runtime,
            HostFunction::new("fs.open-dir", 1, Some(1), {
                let fs_handles = Rc::clone(&fs_handles);
                let policy = Rc::clone(&self.system_policy);
                Box::new(move |args| {
                    let path = policy_checked_fs_path(&policy, &args[0], "read", "fs.open-dir")?;
                    let handle = fs_handles.borrow_mut().allocate_dir_handle(path);
                    Ok(RuntimeValue::Int(handle))
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "close-dir",
            PhasePolicy::Runtime,
            HostFunction::new("fs.close-dir", 1, Some(1), {
                let fs_handles = Rc::clone(&fs_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "fs.close-dir")?;
                    fs_handles
                        .borrow_mut()
                        .dir_handles
                        .remove(&handle)
                        .ok_or_else(|| {
                            eval_err(format!("fs.close-dir: unknown dir handle {handle}"))
                        })?;
                    Ok(RuntimeValue::Null)
                })
            })?,
        )?;
        self.register_function(
            "fs",
            "dir-list",
            PhasePolicy::Runtime,
            HostFunction::new("fs.dir-list", 1, Some(1), {
                let fs_handles = Rc::clone(&fs_handles);
                Box::new(move |args| {
                    let handle = require_int_strict(&args[0], "fs.dir-list")?;
                    let handles = fs_handles.borrow();
                    let path = handles.dir_handle(handle, "fs.dir-list")?;
                    fs_list_dir_value(path)
                })
            })?,
        )?;
        Ok(())
    }
}

#[derive(Default)]
struct FsHandleState {
    next_handle: i64,
    file_handles: BTreeMap<i64, FsFileHandle>,
    dir_handles: BTreeMap<i64, PathBuf>,
}

struct FsFileHandle {
    path: PathBuf,
    file: std::fs::File,
}

#[derive(Default)]
struct ProcessHandleState {
    next_handle: i64,
    process_handles: BTreeMap<i64, Child>,
}

#[derive(Debug)]
struct ProcessSpec {
    argv: Vec<String>,
    cwd: Option<String>,
    env: BTreeMap<String, String>,
    capture_stdout: bool,
    capture_stderr: bool,
    inherit_stdin: bool,
    inherit_stdout: bool,
    inherit_stderr: bool,
}

#[derive(Default)]
struct NetHandleState {
    next_handle: i64,
    listener_handles: BTreeMap<i64, TcpListener>,
    socket_handles: BTreeMap<i64, TcpStream>,
}

#[derive(Debug)]
struct NetListenSpec {
    host: String,
    port: u16,
    backlog: i32,
    reuse_addr: bool,
}

#[derive(Debug)]
struct NetConnectSpec {
    host: String,
    port: u16,
}

impl FsHandleState {
    fn allocate_file_handle(&mut self, path: PathBuf, file: std::fs::File) -> i64 {
        let handle = self.allocate_handle_id();
        self.file_handles
            .insert(handle, FsFileHandle { path, file });
        handle
    }

    fn allocate_dir_handle(&mut self, path: PathBuf) -> i64 {
        let handle = self.allocate_handle_id();
        self.dir_handles.insert(handle, path);
        handle
    }

    fn allocate_handle_id(&mut self) -> i64 {
        self.next_handle += 1;
        self.next_handle
    }

    fn file_handle(
        &self,
        handle: i64,
        context: &str,
    ) -> Result<&FsFileHandle, crate::values::EvalSignal> {
        self.file_handles
            .get(&handle)
            .ok_or_else(|| eval_err(format!("{context}: unknown file handle {handle}")))
    }

    fn file_handle_mut(
        &mut self,
        handle: i64,
        context: &str,
    ) -> Result<&mut FsFileHandle, crate::values::EvalSignal> {
        self.file_handles
            .get_mut(&handle)
            .ok_or_else(|| eval_err(format!("{context}: unknown file handle {handle}")))
    }

    fn dir_handle(
        &self,
        handle: i64,
        context: &str,
    ) -> Result<&PathBuf, crate::values::EvalSignal> {
        self.dir_handles
            .get(&handle)
            .ok_or_else(|| eval_err(format!("{context}: unknown dir handle {handle}")))
    }
}

impl ProcessHandleState {
    fn allocate_process_handle(&mut self, child: Child) -> i64 {
        self.next_handle += 1;
        let handle = self.next_handle;
        self.process_handles.insert(handle, child);
        handle
    }

    fn process_handle_mut(
        &mut self,
        handle: i64,
        context: &str,
    ) -> Result<&mut Child, crate::values::EvalSignal> {
        self.process_handles
            .get_mut(&handle)
            .ok_or_else(|| eval_err(format!("{context}: unknown process handle {handle}")))
    }

    fn remove_process_handle(
        &mut self,
        handle: i64,
        context: &str,
    ) -> Result<Child, crate::values::EvalSignal> {
        self.process_handles
            .remove(&handle)
            .ok_or_else(|| eval_err(format!("{context}: unknown process handle {handle}")))
    }
}

impl NetHandleState {
    fn allocate_listener_handle(&mut self, listener: TcpListener) -> i64 {
        self.next_handle += 1;
        let handle = self.next_handle;
        self.listener_handles.insert(handle, listener);
        handle
    }

    fn allocate_socket_handle(&mut self, stream: TcpStream) -> i64 {
        self.next_handle += 1;
        let handle = self.next_handle;
        self.socket_handles.insert(handle, stream);
        handle
    }

    fn listener_handle_mut(
        &mut self,
        handle: i64,
        context: &str,
    ) -> Result<&mut TcpListener, crate::values::EvalSignal> {
        self.listener_handles
            .get_mut(&handle)
            .ok_or_else(|| eval_err(format!("{context}: unknown listener handle {handle}")))
    }

    fn socket_handle_mut(
        &mut self,
        handle: i64,
        context: &str,
    ) -> Result<&mut TcpStream, crate::values::EvalSignal> {
        self.socket_handles
            .get_mut(&handle)
            .ok_or_else(|| eval_err(format!("{context}: unknown socket handle {handle}")))
    }

    fn remove_any_handle(
        &mut self,
        handle: i64,
        context: &str,
    ) -> Result<(), crate::values::EvalSignal> {
        if self.listener_handles.remove(&handle).is_some()
            || self.socket_handles.remove(&handle).is_some()
        {
            return Ok(());
        }
        Err(eval_err(format!("{context}: unknown net handle {handle}")))
    }
}

fn os_current_dir(_args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    let cwd =
        std::env::current_dir().map_err(|error| eval_err(format!("os.current-dir: {error}")))?;
    Ok(RuntimeValue::Str(path_to_string(cwd)?.into()))
}

fn path_to_string(path: impl AsRef<Path>) -> Result<String, crate::values::EvalSignal> {
    path.as_ref()
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| eval_err("path is not valid UTF-8"))
}

fn host_export_metadata(
    library: &str,
    export: &str,
    function: &HostFunction,
) -> HostExportMetadata {
    let effect = host_export_effect(library, export).to_string();
    let min_arity = function.min_arity;
    let max_arity = function.max_arity;
    HostExportMetadata {
        module: host_export_module(library, export).map(str::to_string),
        public: export.to_string(),
        policy: host_export_policy(library, export).to_string(),
        effect: effect.clone(),
        pure: effect == "pure",
        kind: "function".to_string(),
        capability_kind: host_export_capability(library, export).map(str::to_string),
        signature: host_export_signature(library, export, min_arity, max_arity),
        min_arity,
        max_arity,
        variadic: max_arity.is_none(),
    }
}

fn host_export_module<'a>(library: &'a str, export: &str) -> Option<&'a str> {
    match (library, export) {
        ("format", "format") => Some("sys.fmt"),
        (
            "fs",
            "read-text" | "write-text" | "append-text" | "exists" | "is-file" | "is-dir"
            | "metadata" | "canonicalize" | "list-dir" | "create-dir" | "create-dir-all"
            | "remove-file" | "remove-dir" | "remove-dir-all" | "rename" | "copy-file"
            | "open-file" | "close-file" | "file-read-all-text" | "file-read-line" | "file-write"
            | "file-flush" | "file-seek" | "file-metadata" | "open-dir" | "close-dir" | "dir-list",
        ) => Some("sys.fs"),
        (
            "io",
            "print" | "println" | "write" | "eprint" | "eprintln" | "flush-stdout" | "flush-stderr"
            | "read-line" | "read-all",
        ) => Some("sys.io"),
        ("os", "env-get" | "env-has" | "env-keys" | "env-vars") => Some("sys.env"),
        ("os", "getcwd" | "current-exe" | "temp-dir") => Some("sys.path"),
        ("os", "platform" | "arch") => Some("sys.os"),
        (
            "process",
            "args" | "run" | "spawn" | "wait" | "wait-result" | "kill" | "write-stdin"
            | "close-stdin" | "read-stdout" | "read-stderr",
        ) => Some("sys.proc"),
        ("net", "listen" | "accept" | "connect" | "read" | "write" | "close" | "poll") => {
            Some("sys.net")
        }
        ("time", "now-unix-ns") => Some("sys.time"),
        _ => None,
    }
}

fn host_export_capability(library: &str, export: &str) -> Option<&'static str> {
    match (library, export) {
        ("format", _) => None,
        ("os", "env-get" | "env-has" | "env-keys" | "env-vars") => Some("sys.env"),
        ("os", "getcwd" | "current-exe" | "temp-dir") => Some("sys.path"),
        ("os", "platform" | "arch") => Some("sys.os"),
        ("path", _) => None,
        ("env", _) => None,
        ("io", "write-string" | "write-line") => None,
        ("net", "is-ip" | "is-loopback" | "host-port") => None,
        ("process", "id") => None,
        ("time", "unix-millis") => None,
        _ => host_library_capability(library),
    }
}

fn host_library_capability(library: &str) -> Option<&'static str> {
    match library {
        "env" => Some("sys.env"),
        "fs" => Some("sys.fs"),
        "io" => Some("sys.io"),
        "net" => Some("sys.net"),
        "os" => Some("sys.os"),
        "path" => Some("sys.path"),
        "process" => Some("sys.proc"),
        "time" => Some("sys.time"),
        _ => None,
    }
}

fn host_export_effect(library: &str, export: &str) -> &'static str {
    match (library, export) {
        ("format", _) => "pure",
        ("os", "platform" | "arch") => "pure",
        _ => "impure",
    }
}

fn host_export_policy(library: &str, export: &str) -> &'static str {
    match (library, export) {
        (
            "fs",
            "read-text" | "exists" | "is-file" | "is-dir" | "metadata" | "canonicalize"
            | "list-dir" | "open-dir",
        ) => "fs-read-path",
        (
            "fs",
            "write-text" | "append-text" | "create-dir" | "create-dir-all" | "remove-file"
            | "remove-dir" | "remove-dir-all",
        ) => "fs-write-path",
        ("fs", "rename" | "copy-file") => "fs-read-write-paths",
        ("fs", "open-file") => "fs-open-file",
        ("io", "read-line" | "read-all") => "io-stdin",
        ("net", "listen") => "net-listen",
        ("net", "connect") => "net-connect",
        ("os", "env-get") => "env-get",
        ("os", "env-has") => "env-has",
        ("os", "env-keys") => "env-keys",
        ("os", "env-vars") => "env-vars",
        ("process", "run" | "spawn") => "proc-spawn",
        _ => "none",
    }
}

fn host_export_signature(
    library: &str,
    export: &str,
    min_arity: usize,
    max_arity: Option<usize>,
) -> HostExportSignature {
    let (params, result) = host_export_known_signature(library, export)
        .unwrap_or_else(|| (generated_signature_params(min_arity, max_arity), "any"));
    HostExportSignature {
        params,
        result: result.to_string(),
    }
}

fn generated_signature_params(
    min_arity: usize,
    max_arity: Option<usize>,
) -> Vec<HostExportParameter> {
    let mut params = Vec::new();
    for index in 0..min_arity {
        params.push(HostExportParameter::new(format!("arg{index}"), "any"));
    }
    match max_arity {
        None => params.push(HostExportParameter::new("args", "any[]")),
        Some(max_arity) => {
            for index in min_arity..max_arity {
                params.push(HostExportParameter::new(format!("arg{index}"), "any"));
            }
        }
    }
    params
}

fn host_export_known_signature(
    library: &str,
    export: &str,
) -> Option<(Vec<HostExportParameter>, &'static str)> {
    let params = |items: &[(&str, &str)]| {
        items
            .iter()
            .map(|(name, type_name)| HostExportParameter::new(*name, *type_name))
            .collect::<Vec<_>>()
    };
    Some(match (library, export) {
        ("format", "format") => (
            params(&[("template", "string"), ("values", "any[]")]),
            "string",
        ),
        ("fs", "read-text" | "canonicalize") => (params(&[("path", "string")]), "string"),
        ("fs", "write-text" | "append-text") => {
            (params(&[("path", "string"), ("text", "string")]), "null")
        }
        ("fs", "exists" | "is-file" | "is-dir") => (params(&[("path", "string")]), "bool"),
        ("fs", "metadata") => (params(&[("path", "string")]), "map"),
        ("fs", "list-dir") => (params(&[("path", "string")]), "list<map>"),
        ("fs", "dir-list") => (params(&[("handle", "dir-handle")]), "list<map>"),
        (
            "fs",
            "create-dir" | "create-dir-all" | "remove-file" | "remove-dir" | "remove-dir-all",
        ) => (params(&[("path", "string")]), "null"),
        ("fs", "rename" | "copy-file") => (
            params(&[("source", "string"), ("target", "string")]),
            "null",
        ),
        ("fs", "open-file") => (params(&[("spec", "map")]), "file-handle"),
        ("fs", "close-file") => (params(&[("handle", "file-handle")]), "null"),
        ("fs", "file-read-all-text") => (params(&[("handle", "file-handle")]), "string"),
        ("fs", "file-read-line") => (params(&[("handle", "file-handle")]), "string|null"),
        ("fs", "file-write") => (
            params(&[("handle", "file-handle"), ("text", "string")]),
            "null",
        ),
        ("fs", "file-flush") => (params(&[("handle", "file-handle")]), "null"),
        ("fs", "file-seek") => (
            params(&[
                ("handle", "file-handle"),
                ("offset", "int"),
                ("whence", "string"),
            ]),
            "int",
        ),
        ("fs", "file-metadata") => (params(&[("handle", "file-handle")]), "map"),
        ("fs", "open-dir") => (params(&[("path", "string")]), "dir-handle"),
        ("fs", "close-dir") => (params(&[("handle", "dir-handle")]), "null"),
        ("io", "print" | "println" | "eprint" | "eprintln") => {
            (params(&[("value", "any")]), "null")
        }
        ("io", "write") => (params(&[("value", "string")]), "null"),
        ("io", "flush-stdout" | "flush-stderr") => (Vec::new(), "null"),
        ("io", "read-line") => (Vec::new(), "string|null"),
        ("io", "read-all") => (Vec::new(), "string"),
        ("os", "env-get") => (params(&[("name", "string")]), "string|null"),
        ("os", "env-has") => (params(&[("name", "string")]), "bool"),
        ("os", "env-keys") => (Vec::new(), "list<string>"),
        ("os", "env-vars") => (Vec::new(), "map<string,string>"),
        ("os", "getcwd" | "current-exe" | "temp-dir" | "platform" | "arch") => {
            (Vec::new(), "string")
        }
        ("process", "args") => (Vec::new(), "list<string>"),
        ("process", "run") => (params(&[("spec", "map")]), "map"),
        ("process", "wait") => (params(&[("handle", "process-handle")]), "map"),
        ("process", "spawn") => (params(&[("spec", "map")]), "process-handle"),
        ("process", "wait-result") => (params(&[("handle", "process-handle")]), "map|null"),
        ("process", "kill" | "close-stdin") => (params(&[("handle", "process-handle")]), "null"),
        ("process", "write-stdin") => (
            params(&[("handle", "process-handle"), ("text", "string")]),
            "null",
        ),
        ("process", "read-stdout" | "read-stderr") => {
            (params(&[("handle", "process-handle")]), "string")
        }
        ("net", "listen") => (params(&[("spec", "map")]), "listener-handle"),
        ("net", "connect") => (params(&[("spec", "map")]), "socket-handle"),
        ("net", "accept") => (params(&[("listener", "listener-handle")]), "socket-handle"),
        ("net", "read") => (
            params(&[("socket", "socket-handle"), ("max_bytes", "int")]),
            "string",
        ),
        ("net", "write") => (
            params(&[("socket", "socket-handle"), ("text", "string")]),
            "null",
        ),
        ("net", "close") => (
            params(&[("handle", "socket-handle|listener-handle")]),
            "null",
        ),
        ("net", "poll") => (
            params(&[("handles", "list<int>"), ("timeout_ms", "int")]),
            "list<map>",
        ),
        ("time", "now-unix-ns") => (Vec::new(), "int"),
        _ => return None,
    })
}

fn host_policy_path(path: &str, context: &str) -> Result<PathBuf, crate::values::EvalSignal> {
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| eval_err(format!("{context}: {error}")))?
            .join(path)
    };
    normalize_path_lexically(&absolute)
}

fn normalize_path_lexically(path: &Path) -> Result<PathBuf, crate::values::EvalSignal> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(eval_err("filesystem paths must be non-empty strings"));
    }
    Ok(normalized)
}

fn enforce_roots(
    path: &Path,
    roots: Option<&Vec<PathBuf>>,
    verb: &str,
    context: &str,
) -> Result<(), crate::values::EvalSignal> {
    let Some(roots) = roots else {
        return Ok(());
    };
    if roots.is_empty() {
        return Err(eval_err(format!(
            "compile-time {verb} access is not allowed for {}",
            path.display()
        )));
    }
    for root in roots {
        let root = normalize_path_lexically(root)?;
        if path.starts_with(&root) {
            return Ok(());
        }
    }
    let rendered_roots = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(eval_err(format!(
        "{context}: compile-time {verb} access to {} is outside allowed roots: {rendered_roots}",
        path.display()
    )))
}

fn policy_checked_fs_path(
    policy: &Rc<RefCell<HostSystemPolicy>>,
    value: &RuntimeValue,
    verb: &str,
    context: &str,
) -> Result<PathBuf, crate::values::EvalSignal> {
    let raw_path = require_str(value, context)?;
    if raw_path.is_empty() {
        return Err(eval_err("filesystem paths must be non-empty strings"));
    }
    let path = host_policy_path(raw_path.as_ref(), context)?;
    let policy = policy.borrow();
    let roots = match verb {
        "read" => policy.fs.read_roots.as_ref(),
        "write" => policy.fs.write_roots.as_ref(),
        _ => {
            return Err(eval_err(format!(
                "{context}: unsupported filesystem policy verb"
            )))
        }
    };
    enforce_roots(&path, roots, verb, context)?;
    Ok(path)
}

fn require_process_spawn_allowed(
    policy: &Rc<RefCell<HostSystemPolicy>>,
    context: &str,
) -> Result<(), crate::values::EvalSignal> {
    if !policy.borrow().process.allow_spawn {
        return Err(eval_err(format!(
            "{context}: compile-time process spawning is not allowed"
        )));
    }
    Ok(())
}

fn require_net_listen_allowed(
    policy: &Rc<RefCell<HostSystemPolicy>>,
    context: &str,
) -> Result<(), crate::values::EvalSignal> {
    if !policy.borrow().net.allow_listen {
        return Err(eval_err(format!(
            "{context}: compile-time network listening is not allowed"
        )));
    }
    Ok(())
}

fn require_net_connect_allowed(
    policy: &Rc<RefCell<HostSystemPolicy>>,
    context: &str,
) -> Result<(), crate::values::EvalSignal> {
    if !policy.borrow().net.allow_connect {
        return Err(eval_err(format!(
            "{context}: compile-time network connections are not allowed"
        )));
    }
    Ok(())
}

fn require_stdin_allowed(
    policy: &Rc<RefCell<HostSystemPolicy>>,
    context: &str,
) -> Result<(), crate::values::EvalSignal> {
    if !policy.borrow().io.allow_stdin_read {
        return Err(eval_err(format!(
            "{context}: compile-time stdin reading is not allowed"
        )));
    }
    Ok(())
}

fn env_name_allowed(policy: &Rc<RefCell<HostSystemPolicy>>, name: &str) -> bool {
    policy
        .borrow()
        .os
        .allowlist
        .as_ref()
        .map(|allowlist| allowlist.contains(name))
        .unwrap_or(true)
}

fn net_listen_spec(value: &RuntimeValue) -> Result<NetListenSpec, crate::values::EvalSignal> {
    let spec = require_map(value, "net.listen")?;
    let host =
        runtime_map_get(&spec, "host").ok_or_else(|| eval_err("net.listen: missing host"))?;
    let port =
        runtime_map_get(&spec, "port").ok_or_else(|| eval_err("net.listen: missing port"))?;
    let backlog = match runtime_map_get(&spec, "backlog") {
        Some(value) => require_int_strict(&value, "net.listen backlog")?,
        None => 16,
    };
    if backlog <= 0 {
        return Err(eval_err("net.listen: backlog must be a positive int"));
    }
    Ok(NetListenSpec {
        host: require_str(&host, "net.listen host")?.to_string(),
        port: net_port(&port, "net.listen")?,
        backlog: backlog as i32,
        reuse_addr: runtime_map_bool(&spec, "reuse_addr"),
    })
}

fn net_connect_spec(value: &RuntimeValue) -> Result<NetConnectSpec, crate::values::EvalSignal> {
    let spec = require_map(value, "net.connect")?;
    let host =
        runtime_map_get(&spec, "host").ok_or_else(|| eval_err("net.connect: missing host"))?;
    let port =
        runtime_map_get(&spec, "port").ok_or_else(|| eval_err("net.connect: missing port"))?;
    Ok(NetConnectSpec {
        host: require_str(&host, "net.connect host")?.to_string(),
        port: net_port(&port, "net.connect")?,
    })
}

fn net_port(value: &RuntimeValue, context: &str) -> Result<u16, crate::values::EvalSignal> {
    let port = require_int_strict(value, context)?;
    if !(0..=65535).contains(&port) {
        return Err(eval_err(format!(
            "{context}: port must be between 0 and 65535"
        )));
    }
    Ok(port as u16)
}

fn tcp_listen(spec: &NetListenSpec) -> Result<TcpListener, crate::values::EvalSignal> {
    #[cfg(unix)]
    {
        tcp_listen_unix(spec)
    }
    #[cfg(not(unix))]
    {
        let _ = (spec.backlog, spec.reuse_addr);
        TcpListener::bind((spec.host.as_str(), spec.port))
            .map_err(|error| eval_err(format!("net.listen: {error}")))
    }
}

#[cfg(unix)]
fn tcp_listen_unix(spec: &NetListenSpec) -> Result<TcpListener, crate::values::EvalSignal> {
    use std::os::fd::{FromRawFd, RawFd};

    let address = (spec.host.as_str(), spec.port)
        .to_socket_addrs()
        .map_err(|error| eval_err(format!("net.listen: {error}")))?
        .next()
        .ok_or_else(|| eval_err("net.listen: host resolved to no socket addresses"))?;
    let domain = if address.is_ipv4() {
        libc::AF_INET
    } else {
        libc::AF_INET6
    };
    let fd = unsafe { libc::socket(domain, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(eval_err(format!(
            "net.listen: {}",
            std::io::Error::last_os_error()
        )));
    }
    let result = bind_and_listen_fd(fd, address, spec);
    match result {
        Ok(()) => {
            let listener = unsafe { TcpListener::from_raw_fd(fd as RawFd) };
            Ok(listener)
        }
        Err(error) => {
            unsafe {
                libc::close(fd);
            }
            Err(error)
        }
    }
}

#[cfg(unix)]
fn bind_and_listen_fd(
    fd: libc::c_int,
    address: std::net::SocketAddr,
    spec: &NetListenSpec,
) -> Result<(), crate::values::EvalSignal> {
    if spec.reuse_addr {
        let opt: libc::c_int = 1;
        let rc = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_REUSEADDR,
                &opt as *const _ as *const libc::c_void,
                std::mem::size_of_val(&opt) as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(eval_err(format!(
                "net.listen: {}",
                std::io::Error::last_os_error()
            )));
        }
    }

    let (storage, len) = socket_addr_storage(address);
    let rc = unsafe {
        libc::bind(
            fd,
            &storage as *const _ as *const libc::sockaddr,
            len as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(eval_err(format!(
            "net.listen: {}",
            std::io::Error::last_os_error()
        )));
    }
    let rc = unsafe { libc::listen(fd, spec.backlog) };
    if rc < 0 {
        return Err(eval_err(format!(
            "net.listen: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn socket_addr_storage(address: std::net::SocketAddr) -> (libc::sockaddr_storage, usize) {
    unsafe {
        let mut storage: libc::sockaddr_storage = std::mem::zeroed();
        match address {
            std::net::SocketAddr::V4(address) => {
                let mut raw: libc::sockaddr_in = std::mem::zeroed();
                raw.sin_family = libc::AF_INET as libc::sa_family_t;
                raw.sin_port = address.port().to_be();
                raw.sin_addr = libc::in_addr {
                    s_addr: u32::from_ne_bytes(address.ip().octets()),
                };
                std::ptr::write(&mut storage as *mut _ as *mut libc::sockaddr_in, raw);
                (storage, std::mem::size_of::<libc::sockaddr_in>())
            }
            std::net::SocketAddr::V6(address) => {
                let mut raw: libc::sockaddr_in6 = std::mem::zeroed();
                raw.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                raw.sin6_port = address.port().to_be();
                raw.sin6_flowinfo = address.flowinfo();
                raw.sin6_scope_id = address.scope_id();
                raw.sin6_addr = libc::in6_addr {
                    s6_addr: address.ip().octets(),
                };
                std::ptr::write(&mut storage as *mut _ as *mut libc::sockaddr_in6, raw);
                (storage, std::mem::size_of::<libc::sockaddr_in6>())
            }
        }
    }
}

fn runtime_handle_sequence(
    value: &RuntimeValue,
    context: &str,
) -> Result<Vec<i64>, crate::values::EvalSignal> {
    let items: Vec<RuntimeValue> = match value {
        RuntimeValue::List(items) => items.borrow().clone(),
        RuntimeValue::Tuple(items) => items.iter().cloned().collect(),
        _ => {
            return Err(eval_err(format!(
                "{context}: handles must be a list or tuple"
            )))
        }
    };
    items
        .iter()
        .map(|item| require_int_strict(item, context))
        .collect()
}

fn net_poll(
    state: &NetHandleState,
    handles: &[i64],
    timeout_ms: i64,
) -> Result<RuntimeValue, crate::values::EvalSignal> {
    if handles.is_empty() {
        return Ok(RuntimeValue::List(Rc::new(RefCell::new(Vec::new()))));
    }
    #[cfg(unix)]
    {
        net_poll_unix(state, handles, timeout_ms)
    }
    #[cfg(not(unix))]
    {
        let _ = timeout_ms;
        net_poll_portable(state, handles)
    }
}

#[cfg(unix)]
fn net_poll_unix(
    state: &NetHandleState,
    handles: &[i64],
    timeout_ms: i64,
) -> Result<RuntimeValue, crate::values::EvalSignal> {
    let mut fds = Vec::new();
    let mut handle_kinds = Vec::new();
    for handle in handles {
        if let Some(listener) = state.listener_handles.get(handle) {
            fds.push(libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            handle_kinds.push((*handle, "listener"));
        } else if let Some(stream) = state.socket_handles.get(handle) {
            fds.push(libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN | libc::POLLOUT,
                revents: 0,
            });
            handle_kinds.push((*handle, "socket"));
        } else {
            return Err(eval_err(format!("net.poll: unknown net handle {handle}")));
        }
    }
    let rc = unsafe {
        libc::poll(
            fds.as_mut_ptr(),
            fds.len() as libc::nfds_t,
            timeout_ms as i32,
        )
    };
    if rc < 0 {
        return Err(eval_err(format!(
            "net.poll: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut events = Vec::new();
    for (index, fd) in fds.iter().enumerate() {
        if fd.revents == 0 {
            continue;
        }
        let (handle, kind) = handle_kinds[index];
        events.push(net_poll_event_value(
            handle,
            kind,
            fd.revents & libc::POLLIN != 0,
            fd.revents & libc::POLLOUT != 0,
            fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0,
        )?);
    }
    Ok(RuntimeValue::List(Rc::new(RefCell::new(events))))
}

#[cfg(not(unix))]
fn net_poll_portable(
    state: &NetHandleState,
    handles: &[i64],
) -> Result<RuntimeValue, crate::values::EvalSignal> {
    let mut events = Vec::new();
    for handle in handles {
        if state.listener_handles.contains_key(handle) {
            events.push(net_poll_event_value(
                *handle, "listener", false, false, false,
            )?);
        } else if state.socket_handles.contains_key(handle) {
            events.push(net_poll_event_value(*handle, "socket", false, true, false)?);
        } else {
            return Err(eval_err(format!("net.poll: unknown net handle {handle}")));
        }
    }
    Ok(RuntimeValue::List(Rc::new(RefCell::new(events))))
}

fn net_poll_event_value(
    handle: i64,
    kind: &str,
    readable: bool,
    writable: bool,
    error: bool,
) -> Result<RuntimeValue, crate::values::EvalSignal> {
    runtime_map([
        ("handle", RuntimeValue::Int(handle)),
        ("kind", RuntimeValue::Str(kind.into())),
        ("readable", RuntimeValue::Bool(readable)),
        ("writable", RuntimeValue::Bool(writable)),
        ("error", RuntimeValue::Bool(error)),
    ])
}

fn process_spec(
    value: &RuntimeValue,
    context: &str,
) -> Result<ProcessSpec, crate::values::EvalSignal> {
    let spec = require_map(value, context)?;
    let argv_value = runtime_map_get(&spec, "argv")
        .ok_or_else(|| eval_err(format!("{context}: missing argv")))?;
    let argv = process_argv(&argv_value, context)?;
    let cwd = match runtime_map_get(&spec, "cwd") {
        None | Some(RuntimeValue::Null) => None,
        Some(value) => Some(require_str(&value, context)?.to_string()),
    };
    let env = match runtime_map_get(&spec, "env") {
        None | Some(RuntimeValue::Null) => BTreeMap::new(),
        Some(RuntimeValue::Map(map)) => {
            let mut env = BTreeMap::new();
            for (key, value) in map.borrow().iter() {
                let MapKey::Str(key) = key else {
                    return Err(eval_err(format!("{context}: env keys must be strings")));
                };
                let value = require_str(value, context)?;
                env.insert(key.to_string(), value.to_string());
            }
            env
        }
        Some(_) => return Err(eval_err(format!("{context}: env must be a map"))),
    };
    Ok(ProcessSpec {
        argv,
        cwd,
        env,
        capture_stdout: runtime_map_bool(&spec, "capture_stdout"),
        capture_stderr: runtime_map_bool(&spec, "capture_stderr"),
        inherit_stdin: runtime_map_bool(&spec, "inherit_stdin"),
        inherit_stdout: runtime_map_bool(&spec, "inherit_stdout"),
        inherit_stderr: runtime_map_bool(&spec, "inherit_stderr"),
    })
}

fn process_argv(
    value: &RuntimeValue,
    context: &str,
) -> Result<Vec<String>, crate::values::EvalSignal> {
    let items: Vec<RuntimeValue> = match value {
        RuntimeValue::List(items) => items.borrow().clone(),
        RuntimeValue::Tuple(items) => items.iter().cloned().collect(),
        _ => {
            return Err(eval_err(format!(
                "{context}: argv must be a non-empty list or tuple of strings"
            )));
        }
    };
    if items.is_empty() {
        return Err(eval_err(format!(
            "{context}: argv must be a non-empty list or tuple of strings"
        )));
    }
    items
        .iter()
        .map(|item| require_str(item, context).map(|value| value.to_string()))
        .collect()
}

fn process_command(spec: &ProcessSpec) -> Command {
    let mut command = Command::new(&spec.argv[0]);
    command.args(spec.argv.iter().skip(1));
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    command.envs(spec.env.iter());
    command
}

fn process_wait_value(
    child: Child,
    context: &str,
) -> Result<RuntimeValue, crate::values::EvalSignal> {
    let output = child
        .wait_with_output()
        .map_err(|error| eval_err(format!("{context}: {error}")))?;
    completed_process_value(
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn process_completed_child_value(
    mut child: Child,
    status: ExitStatus,
    context: &str,
) -> Result<RuntimeValue, crate::values::EvalSignal> {
    let stdout = read_optional_pipe(child.stdout.as_mut(), context)?;
    let stderr = read_optional_pipe(child.stderr.as_mut(), context)?;
    completed_process_value(status, stdout, stderr)
}

fn read_optional_pipe(
    pipe: Option<&mut impl Read>,
    context: &str,
) -> Result<String, crate::values::EvalSignal> {
    let Some(pipe) = pipe else {
        return Ok(String::new());
    };
    let mut text = String::new();
    pipe.read_to_string(&mut text)
        .map_err(|error| eval_err(format!("{context}: {error}")))?;
    Ok(text)
}

fn completed_process_value(
    status: ExitStatus,
    stdout: String,
    stderr: String,
) -> Result<RuntimeValue, crate::values::EvalSignal> {
    runtime_map([
        (
            "status",
            RuntimeValue::Int(process_status_code(&status) as i64),
        ),
        ("success", RuntimeValue::Bool(status.success())),
        ("stdout", RuntimeValue::Str(stdout.into())),
        ("stderr", RuntimeValue::Str(stderr.into())),
        ("signal", process_signal(&status)),
    ])
}

fn process_status_code(status: &ExitStatus) -> i32 {
    status.code().unwrap_or_else(|| {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            -status.signal().unwrap_or(1)
        }
        #[cfg(not(unix))]
        {
            1
        }
    })
}

fn process_signal(status: &ExitStatus) -> RuntimeValue {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status
            .signal()
            .map(|signal| RuntimeValue::Int(signal as i64))
            .unwrap_or(RuntimeValue::Null)
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        RuntimeValue::Null
    }
}

fn fs_open_file(
    handles: &Rc<RefCell<FsHandleState>>,
    policy: &Rc<RefCell<HostSystemPolicy>>,
    spec: &RuntimeValue,
) -> Result<RuntimeValue, crate::values::EvalSignal> {
    let spec = require_map(spec, "fs.open-file")?;
    let path_value =
        runtime_map_get(&spec, "path").ok_or_else(|| eval_err("fs.open-file: missing path"))?;
    let path = require_str(&path_value, "fs.open-file path")?;
    let flags = FsOpenFileFlags {
        read: runtime_map_bool(&spec, "read"),
        write: runtime_map_bool(&spec, "write"),
        append: runtime_map_bool(&spec, "append"),
        create: runtime_map_bool(&spec, "create"),
        create_new: runtime_map_bool(&spec, "create_new"),
        truncate: runtime_map_bool(&spec, "truncate"),
    };
    let path = host_policy_path(path.as_ref(), "fs.open-file")?;
    if flags.read {
        enforce_roots(
            &path,
            policy.borrow().fs.read_roots.as_ref(),
            "read",
            "fs.open-file",
        )?;
    }
    if flags.write || flags.append || flags.truncate || flags.create || flags.create_new {
        enforce_roots(
            &path,
            policy.borrow().fs.write_roots.as_ref(),
            "write",
            "fs.open-file",
        )?;
    }
    let file = fs_open_options(flags)
        .open(&path)
        .map_err(|error| eval_err(format!("fs.open-file: {error}")))?;
    let handle = handles.borrow_mut().allocate_file_handle(path, file);
    Ok(RuntimeValue::Int(handle))
}

#[derive(Clone, Copy, Debug, Default)]
struct FsOpenFileFlags {
    read: bool,
    write: bool,
    append: bool,
    create: bool,
    create_new: bool,
    truncate: bool,
}

fn fs_open_options(flags: FsOpenFileFlags) -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    if flags.create_new {
        options.read(true).write(true).create_new(true);
    } else if flags.append {
        options.read(true).append(true).create(flags.create);
    } else if flags.create && !flags.write {
        options.read(true).append(true).create(true);
    } else if flags.write && flags.truncate {
        options
            .read(flags.read)
            .write(true)
            .create(true)
            .truncate(true);
    } else if flags.write {
        options.read(true).write(true);
    } else {
        options.read(true);
    }
    options
}

fn fs_file_read_line(handle: &mut FsFileHandle) -> Result<RuntimeValue, crate::values::EvalSignal> {
    let mut bytes = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        let read = handle
            .file
            .read(&mut byte)
            .map_err(|error| eval_err(format!("fs.file-read-line: {error}")))?;
        if read == 0 {
            break;
        }
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }
    if bytes.is_empty() {
        return Ok(RuntimeValue::Null);
    }
    let line = String::from_utf8(bytes)
        .map_err(|error| eval_err(format!("fs.file-read-line: {error}")))?;
    Ok(RuntimeValue::Str(line.into()))
}

fn fs_metadata_value(path: &Path) -> Result<RuntimeValue, crate::values::EvalSignal> {
    let metadata =
        std::fs::metadata(path).map_err(|error| eval_err(format!("fs.metadata: {error}")))?;
    runtime_map([
        ("path", RuntimeValue::Str(path_to_string(path)?.into())),
        (
            "kind",
            RuntimeValue::Str(if metadata.is_dir() { "dir" } else { "file" }.into()),
        ),
        ("exists", RuntimeValue::Bool(true)),
        ("size", RuntimeValue::Int(metadata.len() as i64)),
        (
            "readonly",
            RuntimeValue::Bool(metadata.permissions().readonly()),
        ),
        (
            "modified_unix_ns",
            system_time_unix_ns(metadata.modified().ok()),
        ),
        (
            "accessed_unix_ns",
            system_time_unix_ns(metadata.accessed().ok()),
        ),
        (
            "created_unix_ns",
            system_time_unix_ns(metadata.created().ok()),
        ),
    ])
}

fn fs_list_dir_value(path: &Path) -> Result<RuntimeValue, crate::values::EvalSignal> {
    let mut entries = Vec::new();
    for entry in
        std::fs::read_dir(path).map_err(|error| eval_err(format!("fs.list-dir: {error}")))?
    {
        let entry = entry.map_err(|error| eval_err(format!("fs.list-dir: {error}")))?;
        let entry_path = entry.path();
        let metadata = std::fs::symlink_metadata(&entry_path)
            .map_err(|error| eval_err(format!("fs.list-dir: {error}")))?;
        entries.push((
            entry.file_name().to_string_lossy().to_string(),
            runtime_map([
                (
                    "name",
                    RuntimeValue::Str(entry.file_name().to_string_lossy().to_string().into()),
                ),
                (
                    "path",
                    RuntimeValue::Str(path_to_string(&entry_path)?.into()),
                ),
                (
                    "kind",
                    RuntimeValue::Str(if metadata.is_dir() { "dir" } else { "file" }.into()),
                ),
                ("is_file", RuntimeValue::Bool(metadata.is_file())),
                ("is_dir", RuntimeValue::Bool(metadata.is_dir())),
                (
                    "is_symlink",
                    RuntimeValue::Bool(metadata.file_type().is_symlink()),
                ),
            ])?,
        ));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(RuntimeValue::List(Rc::new(RefCell::new(
        entries.into_iter().map(|(_, value)| value).collect(),
    ))))
}

fn runtime_map<const N: usize>(
    entries: [(&str, RuntimeValue); N],
) -> Result<RuntimeValue, crate::values::EvalSignal> {
    Ok(RuntimeValue::Map(Rc::new(RefCell::new(
        entries
            .into_iter()
            .map(|(key, value)| (MapKey::Str(key.into()), value))
            .collect(),
    ))))
}

fn runtime_map_get(map: &crate::values::RtMap, key: &str) -> Option<RuntimeValue> {
    map.borrow().get(&MapKey::Str(key.into())).cloned()
}

fn runtime_map_bool(map: &crate::values::RtMap, key: &str) -> bool {
    matches!(runtime_map_get(map, key), Some(RuntimeValue::Bool(true)))
}

fn system_time_unix_ns(time: Option<SystemTime>) -> RuntimeValue {
    time.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| RuntimeValue::Int(duration.as_nanos() as i64))
        .unwrap_or(RuntimeValue::Null)
}

fn io_print_stdout(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    print!("{}", args[0]);
    std::io::stdout()
        .flush()
        .map_err(|error| eval_err(format!("io.print: {error}")))?;
    Ok(RuntimeValue::Null)
}

fn io_println_stdout(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    println!("{}", args[0]);
    Ok(RuntimeValue::Null)
}

fn io_write_stdout(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    let value = require_str(&args[0], "io.write")?;
    print!("{value}");
    std::io::stdout()
        .flush()
        .map_err(|error| eval_err(format!("io.write: {error}")))?;
    Ok(RuntimeValue::Null)
}

fn io_print_stderr(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    eprint!("{}", args[0]);
    std::io::stderr()
        .flush()
        .map_err(|error| eval_err(format!("io.eprint: {error}")))?;
    Ok(RuntimeValue::Null)
}

fn io_println_stderr(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    eprintln!("{}", args[0]);
    Ok(RuntimeValue::Null)
}

fn io_flush_stdout(_args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    std::io::stdout()
        .flush()
        .map_err(|error| eval_err(format!("io.flush-stdout: {error}")))?;
    Ok(RuntimeValue::Null)
}

fn io_flush_stderr(_args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
    std::io::stderr()
        .flush()
        .map_err(|error| eval_err(format!("io.flush-stderr: {error}")))?;
    Ok(RuntimeValue::Null)
}

fn io_read_line(
    _args: Vec<RuntimeValue>,
    policy: &Rc<RefCell<HostSystemPolicy>>,
) -> Result<RuntimeValue, crate::values::EvalSignal> {
    require_stdin_allowed(policy, "io.read-line")?;
    let mut text = String::new();
    std::io::stdin()
        .read_line(&mut text)
        .map_err(|error| eval_err(format!("io.read-line: {error}")))?;
    Ok(RuntimeValue::Str(text.into()))
}

fn io_read_all(
    _args: Vec<RuntimeValue>,
    policy: &Rc<RefCell<HostSystemPolicy>>,
) -> Result<RuntimeValue, crate::values::EvalSignal> {
    require_stdin_allowed(policy, "io.read-all")?;
    let mut text = String::new();
    std::io::stdin()
        .lock()
        .read_to_string(&mut text)
        .map_err(|error| eval_err(format!("io.read-all: {error}")))?;
    Ok(RuntimeValue::Str(text.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_service_library_registers_export_metadata() {
        let mut library = HostServiceLibrary::new("math").unwrap();
        library
            .register_function(
                "id",
                PhasePolicy::Runtime,
                HostFunction::new("math.id", 1, Some(1), Box::new(|args| Ok(args[0].clone())))
                    .unwrap(),
            )
            .unwrap();

        assert_eq!(library.export_names(), vec!["id"]);
        let export = library.export("id").unwrap().unwrap();
        assert_eq!(export.library, "math");
        assert_eq!(export.metadata.min_arity, 1);
        assert_eq!(export.metadata.max_arity, Some(1));
    }

    #[test]
    fn host_service_registry_explicitly_exports_environment_bindings() {
        let mut registry = HostServiceRegistry::new();
        registry.register_path_library().unwrap();
        let env = Environment::new(None);

        let names = registry
            .export_library_to_environment("path", PhasePolicy::Runtime, &env)
            .unwrap();
        assert_eq!(names, vec!["path.basename", "path.dirname", "path.join"]);
        assert!(matches!(
            Environment::lookup(&env, "path.basename").unwrap(),
            RuntimeValue::HostFunction(_)
        ));
    }
}
