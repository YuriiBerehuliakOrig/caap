use std::cell::RefCell;
use std::rc::Rc;

use crate::error::{CaapError, CaapResult};
use crate::values::{eval_err, HostFunction};

use super::{
    HostExportContract, HostExportMetadata, HostExportParameter, HostExportSignature,
    HostSystemPolicy,
};

// ---------------------------------------------------------------------------
// Metadata contract
// ---------------------------------------------------------------------------

pub(super) fn host_export_metadata(
    library: &str,
    export: &str,
    function: &HostFunction,
) -> CaapResult<HostExportMetadata> {
    let min_arity = function.min_arity;
    let max_arity = function.max_arity;
    let contract = host_export_contract(library, export)?;
    Ok(HostExportMetadata {
        module: contract.module.map(str::to_string),
        policy: contract.policy.to_string(),
        effect: contract.effect.to_string(),
        kind: "function".to_string(),
        capability_kind: contract.capability_kind.map(str::to_string),
        signature: contract.signature,
        min_arity,
        max_arity,
    })
}

/// The fine-grained capability a unit must hold to invoke `library.export`, or
/// `None` for pure operations that touch no external resource.
///
/// The capability authority comes from the host contract's `capability_kind`
/// (e.g. `sys.fs`); the read/write granularity comes from the sys-runtime
/// capability-effect catalog (`fs.read-text` → `sys.fs.read`,
/// `fs.write-text` → `sys.fs.write`). Pure operations (`path.*`, `net.is-ip`,
/// `os.platform`) require no grant. This is the single source of truth the
/// capability policy enforces; see `docs/design-capability-enforcement.md`.
pub(crate) fn required_capability(library: &str, export: &str) -> CaapResult<Option<String>> {
    let contract = host_export_contract(library, export)?;
    if contract.effect == "pure" {
        return Ok(None);
    }
    let Some(domain) = contract.capability_kind else {
        return Ok(None);
    };
    let (_domain, access) = caap_sys_runtime::catalog::capability_effect(library, export);
    let capability = match access {
        "read" => format!("{domain}.read"),
        "write" => format!("{domain}.write"),
        _ => domain.to_string(),
    };
    Ok(Some(capability))
}

pub(super) fn known_host_export_contract(
    library: &str,
    export: &str,
) -> Option<HostExportContract> {
    let params = |items: &[(&str, &str)]| {
        items
            .iter()
            .map(|(name, type_name)| HostExportParameter::new(*name, *type_name))
            .collect::<Vec<_>>()
    };
    let signature = |params: Vec<HostExportParameter>, result: &str| HostExportSignature {
        params,
        result: result.to_string(),
    };
    let contract = |module: Option<&'static str>,
                    capability_kind: Option<&'static str>,
                    policy: &'static str,
                    effect: &'static str,
                    params: Vec<HostExportParameter>,
                    result: &'static str| {
        HostExportContract {
            module,
            capability_kind,
            policy,
            effect,
            signature: signature(params, result),
        }
    };

    let contract = match (library, export) {
        ("path", "join") => contract(
            Some("sys.path"),
            None,
            "none",
            "pure",
            params(&[("parts", "string[]")]),
            "string",
        ),
        ("path", "basename" | "dirname" | "stem") => contract(
            Some("sys.path"),
            None,
            "none",
            "pure",
            params(&[("path", "string")]),
            "string",
        ),
        ("path", "extension") => contract(
            Some("sys.path"),
            None,
            "none",
            "pure",
            params(&[("path", "string")]),
            "string|null",
        ),
        ("path", "with_extension") => contract(
            Some("sys.path"),
            None,
            "none",
            "pure",
            params(&[("path", "string"), ("extension", "string")]),
            "string",
        ),
        ("path", "is_absolute") => contract(
            Some("sys.path"),
            None,
            "none",
            "pure",
            params(&[("path", "string")]),
            "bool",
        ),
        ("path", "normalize") => contract(
            Some("sys.path"),
            None,
            "none",
            "pure",
            params(&[("path", "string")]),
            "string",
        ),
        ("path", "split") => contract(
            Some("sys.path"),
            None,
            "none",
            "pure",
            params(&[("path", "string")]),
            "list<string>",
        ),
        ("path", "strip_prefix") => contract(
            Some("sys.path"),
            None,
            "none",
            "pure",
            params(&[("path", "string"), ("prefix", "string")]),
            "string|null",
        ),
        ("fs", "read_text" | "canonicalize") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "fs_read_path",
            "impure",
            params(&[("path", "string")]),
            "string",
        ),
        ("fs", "exists" | "is_file" | "is_dir") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "fs_read_path",
            "impure",
            params(&[("path", "string")]),
            "bool",
        ),
        ("fs", "metadata") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "fs_read_path",
            "impure",
            params(&[("path", "string")]),
            "map",
        ),
        ("fs", "list_dir") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "fs_read_path",
            "impure",
            params(&[("path", "string")]),
            "list<map>",
        ),
        ("fs", "open_dir") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "fs_read_path",
            "impure",
            params(&[("path", "string")]),
            "dir_handle",
        ),
        ("fs", "dir_list") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "none",
            "impure",
            params(&[("handle", "dir_handle")]),
            "list<map>",
        ),
        ("fs", "write_text" | "append_text") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "fs_write_path",
            "impure",
            params(&[("path", "string"), ("text", "string")]),
            "null",
        ),
        (
            "fs",
            "create_dir" | "create_dir_all" | "remove_file" | "remove_dir" | "remove_dir_all",
        ) => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "fs_write_path",
            "impure",
            params(&[("path", "string")]),
            "null",
        ),
        ("fs", "rename" | "copy_file" | "hard_link") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "fs_read_write_paths",
            "impure",
            params(&[("source", "string"), ("target", "string")]),
            "null",
        ),
        ("fs", "read_link") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "fs_read_path",
            "impure",
            params(&[("path", "string")]),
            "string",
        ),
        ("fs", "read_bytes") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "fs_read_path",
            "impure",
            params(&[("path", "string")]),
            "bytes",
        ),
        ("fs", "write_bytes" | "append_bytes") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "fs_write_path",
            "impure",
            params(&[("path", "string"), ("bytes", "bytes")]),
            "null",
        ),
        ("fs", "file_read_bytes") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "none",
            "impure",
            params(&[("handle", "file_handle"), ("max_bytes", "int")]),
            "bytes",
        ),
        ("fs", "file_write_bytes") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "none",
            "impure",
            params(&[("handle", "file_handle"), ("bytes", "bytes")]),
            "null",
        ),
        ("fs", "set_readonly") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "fs_write_path",
            "impure",
            params(&[("path", "string"), ("readonly", "bool")]),
            "null",
        ),
        ("fs", "symlink") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "fs_write_path",
            "impure",
            params(&[("target", "string"), ("link", "string")]),
            "null",
        ),
        ("fs", "set_permissions") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "fs_write_path",
            "impure",
            params(&[("path", "string"), ("mode", "int")]),
            "null",
        ),
        ("fs", "open_file") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "fs_open_file",
            "impure",
            params(&[("spec", "map")]),
            "file_handle",
        ),
        ("fs", "close_file") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "none",
            "impure",
            params(&[("handle", "file_handle")]),
            "null",
        ),
        ("fs", "file_read_all_text") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "none",
            "impure",
            params(&[("handle", "file_handle")]),
            "string",
        ),
        ("fs", "file_read_line") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "none",
            "impure",
            params(&[("handle", "file_handle")]),
            "string|null",
        ),
        ("fs", "file_write") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "none",
            "impure",
            params(&[("handle", "file_handle"), ("text", "string")]),
            "null",
        ),
        ("fs", "file_flush") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "none",
            "impure",
            params(&[("handle", "file_handle")]),
            "null",
        ),
        ("fs", "file_seek") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "none",
            "impure",
            params(&[
                ("handle", "file_handle"),
                ("offset", "int"),
                ("whence", "string"),
            ]),
            "int",
        ),
        ("fs", "file_metadata") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "none",
            "impure",
            params(&[("handle", "file_handle")]),
            "map",
        ),
        ("fs", "close_dir") => contract(
            Some("sys.fs"),
            Some("sys.fs"),
            "none",
            "impure",
            params(&[("handle", "dir_handle")]),
            "null",
        ),
        ("io", "print" | "println" | "eprint" | "eprintln") => contract(
            Some("sys.io"),
            Some("sys.io"),
            "none",
            "impure",
            params(&[("value", "any")]),
            "null",
        ),
        ("io", "write") => contract(
            Some("sys.io"),
            Some("sys.io"),
            "none",
            "impure",
            params(&[("value", "string")]),
            "null",
        ),
        ("io", "flush_stdout" | "flush_stderr") => contract(
            Some("sys.io"),
            Some("sys.io"),
            "none",
            "impure",
            Vec::new(),
            "null",
        ),
        ("io", "read_line") => contract(
            Some("sys.io"),
            Some("sys.io"),
            "io_stdin",
            "impure",
            Vec::new(),
            "string|null",
        ),
        ("io", "read_all") => contract(
            Some("sys.io"),
            Some("sys.io"),
            "io_stdin",
            "impure",
            Vec::new(),
            "string",
        ),
        ("io", "write_bytes") => contract(
            Some("sys.io"),
            Some("sys.io"),
            "none",
            "impure",
            params(&[("bytes", "bytes")]),
            "null",
        ),
        ("os", "env_get") => contract(
            Some("sys.env"),
            Some("sys.env"),
            "env_get",
            "impure",
            params(&[("name", "string")]),
            "string|null",
        ),
        ("os", "env_has") => contract(
            Some("sys.env"),
            Some("sys.env"),
            "env_has",
            "impure",
            params(&[("name", "string")]),
            "bool",
        ),
        ("os", "env_keys") => contract(
            Some("sys.env"),
            Some("sys.env"),
            "env_keys",
            "impure",
            Vec::new(),
            "list<string>",
        ),
        ("os", "env_vars") => contract(
            Some("sys.env"),
            Some("sys.env"),
            "env_vars",
            "impure",
            Vec::new(),
            "map<string,string>",
        ),
        ("os", "getcwd" | "current_exe" | "temp_dir") => contract(
            Some("sys.path"),
            Some("sys.path"),
            "none",
            "impure",
            Vec::new(),
            "string",
        ),
        ("os", "platform" | "arch" | "family") => contract(
            Some("sys.os"),
            Some("sys.os"),
            "none",
            "pure",
            Vec::new(),
            "string",
        ),
        ("os", "available_parallelism") => contract(
            Some("sys.os"),
            Some("sys.os"),
            "none",
            "impure",
            Vec::new(),
            "int",
        ),
        ("os", "hostname") => contract(
            Some("sys.os"),
            Some("sys.os"),
            "none",
            "impure",
            Vec::new(),
            "string",
        ),
        ("os", "set_current_dir") => contract(
            Some("sys.path"),
            Some("sys.path"),
            "env_set",
            "impure",
            params(&[("path", "string")]),
            "null",
        ),
        ("os", "set_env") => contract(
            Some("sys.env"),
            Some("sys.env"),
            "env_set",
            "impure",
            params(&[("name", "string"), ("value", "string")]),
            "null",
        ),
        ("os", "remove_env") => contract(
            Some("sys.env"),
            Some("sys.env"),
            "env_set",
            "impure",
            params(&[("name", "string")]),
            "null",
        ),
        ("net", "is_ip" | "is_loopback") => contract(
            Some("sys.net"),
            None,
            "none",
            "pure",
            params(&[("address", "string")]),
            "bool",
        ),
        ("net", "host_port") => contract(
            Some("sys.net"),
            None,
            "none",
            "pure",
            params(&[("host", "string"), ("port", "int")]),
            "string",
        ),
        ("net", "listen") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "net_listen",
            "impure",
            params(&[("spec", "map")]),
            "listener_handle",
        ),
        ("net", "connect") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "net_connect",
            "impure",
            params(&[("spec", "map")]),
            "socket_handle",
        ),
        ("net", "accept") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "none",
            "impure",
            params(&[("listener", "listener_handle")]),
            "socket_handle",
        ),
        ("net", "read") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "none",
            "impure",
            params(&[("socket", "socket_handle"), ("max_bytes", "int")]),
            "string",
        ),
        ("net", "write") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "none",
            "impure",
            params(&[("socket", "socket_handle"), ("text", "string")]),
            "null",
        ),
        ("net", "read_bytes") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "none",
            "impure",
            params(&[("socket", "socket_handle"), ("max_bytes", "int")]),
            "bytes",
        ),
        ("net", "write_bytes") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "none",
            "impure",
            params(&[("socket", "socket_handle"), ("bytes", "bytes")]),
            "null",
        ),
        ("net", "close") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "none",
            "impure",
            params(&[("handle", "socket-handle|listener-handle")]),
            "null",
        ),
        ("net", "poll") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "none",
            "impure",
            params(&[("handles", "list<int>"), ("timeout_ms", "int")]),
            "list<map>",
        ),
        ("net", "resolve") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "net_connect",
            "impure",
            params(&[("address", "string")]),
            "list<string>",
        ),
        ("net", "local_addr") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "none",
            "impure",
            params(&[("handle", "listener-handle|socket-handle|udp-handle")]),
            "string",
        ),
        ("net", "peer_addr") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "none",
            "impure",
            params(&[("handle", "socket-handle|udp-handle")]),
            "string",
        ),
        ("net", "shutdown") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "none",
            "impure",
            params(&[("socket", "socket_handle"), ("how", "string")]),
            "null",
        ),
        ("net", "udp_bind") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "net_listen",
            "impure",
            params(&[("spec", "map")]),
            "udp_handle",
        ),
        ("net", "udp_send_to") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "net_connect",
            "impure",
            params(&[
                ("handle", "udp_handle"),
                ("data", "string"),
                ("address", "string"),
            ]),
            "int",
        ),
        ("net", "udp_recv_from") => contract(
            Some("sys.net"),
            Some("sys.net"),
            "none",
            "impure",
            params(&[("handle", "udp_handle"), ("max_bytes", "int")]),
            "map",
        ),
        ("process", "id") => contract(
            Some("sys.proc"),
            Some("sys.proc"),
            "none",
            "impure",
            Vec::new(),
            "int",
        ),
        ("process", "args") => contract(
            Some("sys.proc"),
            Some("sys.proc"),
            "none",
            "impure",
            Vec::new(),
            "list<string>",
        ),
        ("process", "run") => contract(
            Some("sys.proc"),
            Some("sys.proc"),
            "proc_spawn",
            "impure",
            params(&[("spec", "map")]),
            "map",
        ),
        ("process", "spawn") => contract(
            Some("sys.proc"),
            Some("sys.proc"),
            "proc_spawn",
            "impure",
            params(&[("spec", "map")]),
            "process_handle",
        ),
        ("process", "wait") => contract(
            Some("sys.proc"),
            Some("sys.proc"),
            "none",
            "impure",
            params(&[("handle", "process_handle")]),
            "map",
        ),
        ("process", "wait_result") => contract(
            Some("sys.proc"),
            Some("sys.proc"),
            "none",
            "impure",
            params(&[("handle", "process_handle")]),
            "map|null",
        ),
        ("process", "kill" | "close_stdin") => contract(
            Some("sys.proc"),
            Some("sys.proc"),
            "none",
            "impure",
            params(&[("handle", "process_handle")]),
            "null",
        ),
        ("process", "write_stdin") => contract(
            Some("sys.proc"),
            Some("sys.proc"),
            "none",
            "impure",
            params(&[("handle", "process_handle"), ("text", "string")]),
            "null",
        ),
        ("process", "read_stdout" | "read_stderr") => contract(
            Some("sys.proc"),
            Some("sys.proc"),
            "none",
            "impure",
            params(&[("handle", "process_handle")]),
            "string",
        ),
        ("time", "now_unix_ns" | "unix_millis" | "unix_seconds" | "monotonic_ns") => contract(
            Some("sys.time"),
            Some("sys.time"),
            "none",
            "impure",
            Vec::new(),
            "int",
        ),
        ("time", "sleep_ms") => contract(
            Some("sys.time"),
            Some("sys.time"),
            "none",
            "impure",
            params(&[("ms", "int")]),
            "null",
        ),
        ("rand", "bytes") => contract(
            Some("sys.rand"),
            Some("sys.rand"),
            "none",
            "impure",
            params(&[("count", "int")]),
            "bytes",
        ),
        ("rand", "int") => contract(
            Some("sys.rand"),
            Some("sys.rand"),
            "none",
            "impure",
            params(&[("lo", "int"), ("hi", "int")]),
            "int",
        ),
        ("rand", "float") => contract(
            Some("sys.rand"),
            Some("sys.rand"),
            "none",
            "impure",
            Vec::new(),
            "float",
        ),
        ("rand", "bool") => contract(
            Some("sys.rand"),
            Some("sys.rand"),
            "none",
            "impure",
            Vec::new(),
            "bool",
        ),
        _ => {
            return None;
        }
    };
    Some(contract)
}

pub(super) fn host_export_contract(library: &str, export: &str) -> CaapResult<HostExportContract> {
    known_host_export_contract(library, export).ok_or_else(|| {
        CaapError::host(format!(
            "host export {library}.{export} is missing an explicit metadata contract"
        ))
    })
}

// ---------------------------------------------------------------------------
// Policy guards
// ---------------------------------------------------------------------------

pub(super) fn require_process_spawn_allowed(
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

pub(super) fn require_net_listen_allowed(
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

pub(super) fn require_net_connect_allowed(
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

pub(super) fn env_name_allowed(policy: &Rc<RefCell<HostSystemPolicy>>, name: &str) -> bool {
    policy
        .borrow()
        .os
        .allowlist
        .as_ref()
        .map(|allowlist| allowlist.contains(name))
        .unwrap_or(true)
}

// ---------------------------------------------------------------------------
// Path / OsStr helpers
// ---------------------------------------------------------------------------

pub(super) fn path_to_string(
    path: impl AsRef<std::path::Path>,
) -> Result<String, crate::values::EvalSignal> {
    path.as_ref()
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| eval_err("path is not valid UTF-8"))
}

#[cfg(test)]
mod contract_completeness_tests {
    use super::{host_export_contract, required_capability};
    use crate::semantic::CapabilityName;
    use caap_sys_runtime::catalog::export_catalog;

    /// Every operation the sys runtime can dispatch must have an explicit
    /// caap-core host contract (capability/effect/signature). This guards
    /// against drift: adding an export to the runtime catalog without giving it
    /// a contract here is caught at build time instead of at first invocation.
    #[test]
    fn every_sys_runtime_export_has_a_host_contract() {
        let mut missing = Vec::new();
        for entry in export_catalog() {
            if host_export_contract(entry.library, entry.export).is_err() {
                missing.push(format!("{}.{}", entry.library, entry.export));
            }
        }
        assert!(
            missing.is_empty(),
            "sys runtime exports without a host contract: {missing:?}"
        );
    }

    #[test]
    fn required_capability_resolves_for_every_export() {
        for entry in export_catalog() {
            let capability = required_capability(entry.library, entry.export)
                .unwrap_or_else(|e| panic!("{}.{}: {e}", entry.library, entry.export));
            // Any required capability must be a valid, hierarchical name.
            if let Some(name) = capability {
                CapabilityName::new(&name).unwrap_or_else(|e| {
                    panic!(
                        "{}.{} -> invalid capability {name:?}: {e}",
                        entry.library, entry.export
                    )
                });
            }
        }
    }

    #[test]
    fn required_capability_splits_read_write_and_skips_pure() {
        let cap = |lib, exp| required_capability(lib, exp).unwrap();
        assert_eq!(cap("fs", "read_text"), Some("sys.fs.read".to_string()));
        assert_eq!(cap("fs", "write_text"), Some("sys.fs.write".to_string()));
        assert_eq!(cap("net", "connect"), Some("sys.net".to_string()));
        // Pure operations need no grant.
        assert_eq!(cap("path", "join"), None);
        assert_eq!(cap("net", "is_ip"), None);
        assert_eq!(cap("os", "platform"), None);
    }
}
