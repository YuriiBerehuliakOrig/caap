/// Metadata for one exported function in the runtime.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub library: &'static str,
    pub export: &'static str,
    pub min_arity: u32,
    pub max_arity: Option<u32>,
}

impl CatalogEntry {
    const fn new(
        library: &'static str,
        export: &'static str,
        min_arity: u32,
        max_arity: Option<u32>,
    ) -> Self {
        Self {
            library,
            export,
            min_arity,
            max_arity,
        }
    }
}

const EXPORT_CATALOG: &[CatalogEntry] = &[
    // io
    CatalogEntry::new("io", "print", 1, Some(1)),
    CatalogEntry::new("io", "println", 1, Some(1)),
    CatalogEntry::new("io", "write", 1, Some(1)),
    CatalogEntry::new("io", "eprint", 1, Some(1)),
    CatalogEntry::new("io", "eprintln", 1, Some(1)),
    CatalogEntry::new("io", "flush_stdout", 0, Some(0)),
    CatalogEntry::new("io", "flush_stderr", 0, Some(0)),
    CatalogEntry::new("io", "read_line", 0, Some(0)),
    CatalogEntry::new("io", "read_all", 0, Some(0)),
    CatalogEntry::new("io", "write_bytes", 1, Some(1)),
    // path
    CatalogEntry::new("path", "join", 1, None),
    CatalogEntry::new("path", "basename", 1, Some(1)),
    CatalogEntry::new("path", "dirname", 1, Some(1)),
    CatalogEntry::new("path", "extension", 1, Some(1)),
    CatalogEntry::new("path", "stem", 1, Some(1)),
    CatalogEntry::new("path", "with_extension", 2, Some(2)),
    CatalogEntry::new("path", "is_absolute", 1, Some(1)),
    CatalogEntry::new("path", "normalize", 1, Some(1)),
    CatalogEntry::new("path", "split", 1, Some(1)),
    CatalogEntry::new("path", "strip_prefix", 2, Some(2)),
    // time
    CatalogEntry::new("time", "now_unix_ns", 0, Some(0)),
    CatalogEntry::new("time", "unix_millis", 0, Some(0)),
    CatalogEntry::new("time", "unix_seconds", 0, Some(0)),
    CatalogEntry::new("time", "monotonic_ns", 0, Some(0)),
    CatalogEntry::new("time", "sleep_ms", 1, Some(1)),
    // rand
    CatalogEntry::new("rand", "bytes", 1, Some(1)),
    CatalogEntry::new("rand", "int", 2, Some(2)),
    CatalogEntry::new("rand", "float", 0, Some(0)),
    CatalogEntry::new("rand", "bool", 0, Some(0)),
    // os
    CatalogEntry::new("os", "env_get", 1, Some(1)),
    CatalogEntry::new("os", "env_has", 1, Some(1)),
    CatalogEntry::new("os", "env_keys", 0, Some(0)),
    CatalogEntry::new("os", "env_vars", 0, Some(0)),
    CatalogEntry::new("os", "getcwd", 0, Some(0)),
    CatalogEntry::new("os", "current_exe", 0, Some(0)),
    CatalogEntry::new("os", "temp_dir", 0, Some(0)),
    CatalogEntry::new("os", "platform", 0, Some(0)),
    CatalogEntry::new("os", "arch", 0, Some(0)),
    CatalogEntry::new("os", "family", 0, Some(0)),
    CatalogEntry::new("os", "available_parallelism", 0, Some(0)),
    CatalogEntry::new("os", "hostname", 0, Some(0)),
    CatalogEntry::new("os", "set_current_dir", 1, Some(1)),
    CatalogEntry::new("os", "set_env", 2, Some(2)),
    CatalogEntry::new("os", "remove_env", 1, Some(1)),
    // net
    CatalogEntry::new("net", "listen", 1, Some(1)),
    CatalogEntry::new("net", "accept", 1, Some(2)),
    CatalogEntry::new("net", "connect", 1, Some(1)),
    CatalogEntry::new("net", "read", 2, Some(3)),
    CatalogEntry::new("net", "write", 2, Some(3)),
    CatalogEntry::new("net", "read_bytes", 2, Some(3)),
    CatalogEntry::new("net", "write_bytes", 2, Some(3)),
    CatalogEntry::new("net", "close", 1, Some(1)),
    CatalogEntry::new("net", "poll", 2, Some(2)),
    CatalogEntry::new("net", "is_ip", 1, Some(1)),
    CatalogEntry::new("net", "is_loopback", 1, Some(1)),
    CatalogEntry::new("net", "host_port", 2, Some(2)),
    CatalogEntry::new("net", "resolve", 1, Some(1)),
    CatalogEntry::new("net", "local_addr", 1, Some(1)),
    CatalogEntry::new("net", "peer_addr", 1, Some(1)),
    CatalogEntry::new("net", "shutdown", 2, Some(2)),
    CatalogEntry::new("net", "udp_bind", 1, Some(1)),
    CatalogEntry::new("net", "udp_send_to", 3, Some(3)),
    CatalogEntry::new("net", "udp_recv_from", 2, Some(3)),
    // fs
    CatalogEntry::new("fs", "exists", 1, Some(1)),
    CatalogEntry::new("fs", "read_text", 1, Some(1)),
    CatalogEntry::new("fs", "write_text", 2, Some(2)),
    CatalogEntry::new("fs", "append_text", 2, Some(2)),
    CatalogEntry::new("fs", "is_file", 1, Some(1)),
    CatalogEntry::new("fs", "is_dir", 1, Some(1)),
    CatalogEntry::new("fs", "metadata", 1, Some(1)),
    CatalogEntry::new("fs", "canonicalize", 1, Some(1)),
    CatalogEntry::new("fs", "list_dir", 1, Some(1)),
    CatalogEntry::new("fs", "create_dir", 1, Some(1)),
    CatalogEntry::new("fs", "create_dir_all", 1, Some(1)),
    CatalogEntry::new("fs", "remove_file", 1, Some(1)),
    CatalogEntry::new("fs", "remove_dir", 1, Some(1)),
    CatalogEntry::new("fs", "remove_dir_all", 1, Some(1)),
    CatalogEntry::new("fs", "rename", 2, Some(2)),
    CatalogEntry::new("fs", "copy_file", 2, Some(2)),
    CatalogEntry::new("fs", "read_link", 1, Some(1)),
    CatalogEntry::new("fs", "hard_link", 2, Some(2)),
    CatalogEntry::new("fs", "symlink", 2, Some(2)),
    CatalogEntry::new("fs", "set_readonly", 2, Some(2)),
    CatalogEntry::new("fs", "set_permissions", 2, Some(2)),
    CatalogEntry::new("fs", "read_bytes", 1, Some(1)),
    CatalogEntry::new("fs", "write_bytes", 2, Some(2)),
    CatalogEntry::new("fs", "append_bytes", 2, Some(2)),
    CatalogEntry::new("fs", "file_read_bytes", 2, Some(2)),
    CatalogEntry::new("fs", "file_write_bytes", 2, Some(2)),
    CatalogEntry::new("fs", "open_file", 1, Some(1)),
    CatalogEntry::new("fs", "close_file", 1, Some(1)),
    CatalogEntry::new("fs", "file_read_all_text", 1, Some(1)),
    CatalogEntry::new("fs", "file_read_line", 1, Some(1)),
    CatalogEntry::new("fs", "file_write", 2, Some(2)),
    CatalogEntry::new("fs", "file_flush", 1, Some(1)),
    CatalogEntry::new("fs", "file_seek", 2, Some(3)),
    CatalogEntry::new("fs", "file_metadata", 1, Some(1)),
    CatalogEntry::new("fs", "open_dir", 1, Some(1)),
    CatalogEntry::new("fs", "close_dir", 1, Some(1)),
    CatalogEntry::new("fs", "dir_list", 1, Some(1)),
    // process
    CatalogEntry::new("process", "id", 0, Some(0)),
    CatalogEntry::new("process", "args", 0, Some(0)),
    CatalogEntry::new("process", "run", 1, Some(1)),
    CatalogEntry::new("process", "spawn", 1, Some(1)),
    CatalogEntry::new("process", "wait", 1, Some(1)),
    CatalogEntry::new("process", "wait_result", 1, Some(1)),
    CatalogEntry::new("process", "kill", 1, Some(1)),
    CatalogEntry::new("process", "write_stdin", 2, Some(2)),
    CatalogEntry::new("process", "close_stdin", 1, Some(1)),
    CatalogEntry::new("process", "read_stdout", 1, Some(1)),
    CatalogEntry::new("process", "read_stderr", 1, Some(1)),
];

pub fn export_catalog() -> &'static [CatalogEntry] {
    EXPORT_CATALOG
}

/// Classify an export by the capability domain it touches and the effect it has
/// within that domain. Capability is the resource a host must grant (e.g.
/// `filesystem`, `network`); effect is the kind of access (`read`, `write`,
/// `pure`, `network`, `process`, `console`). Together they let a host enforce
/// finer policy than a single all-or-nothing `host_services` grant.
pub fn capability_effect(library: &str, export: &str) -> (&'static str, &'static str) {
    match library {
        "io" => (
            "console",
            match export {
                "read_line" | "read_all" => "read",
                _ => "write",
            },
        ),
        "path" => ("path", "pure"),
        "time" => ("clock", "read"),
        // Reading the OS entropy pool; a host may gate it (e.g. to force
        // deterministic runs) independently of other capabilities.
        "rand" => ("entropy", "read"),
        "os" => (
            "environment",
            match export {
                "set_env" | "remove_env" | "set_current_dir" => "write",
                _ => "read",
            },
        ),
        "net" => (
            "network",
            match export {
                "is_ip" | "is_loopback" | "host_port" => "pure",
                _ => "network",
            },
        ),
        "fs" => (
            "filesystem",
            match export {
                "write_text" | "append_text" | "create_dir" | "create_dir_all" | "remove_file"
                | "remove_dir" | "remove_dir_all" | "rename" | "copy_file" | "file_write"
                | "file_flush" | "hard_link" | "symlink" | "set_readonly" | "set_permissions"
                | "write_bytes" | "append_bytes" | "file_write_bytes" => "write",
                _ => "read",
            },
        ),
        "process" => (
            "process",
            match export {
                "id" | "args" => "read",
                _ => "process",
            },
        ),
        _ => ("unknown", "unknown"),
    }
}

/// The capability/effect declaration for every exported operation, one entry
/// per export as `"<library>.<export> <capability> <effect>"`. Derived from
/// [`export_catalog`] so it is always complete, and exposed through the ABI
/// descriptor's `capability_effect_catalog_hash` so a host can verify the
/// runtime's capability surface.
pub fn capability_effect_catalog() -> Vec<String> {
    export_catalog()
        .iter()
        .map(|entry| {
            let (capability, effect) = capability_effect(entry.library, entry.export);
            format!("{}.{} {capability} {effect}", entry.library, entry.export)
        })
        .collect()
}

/// A host's decision about whether one operation may run. Returned by a
/// [`SysPolicy`], which [`dispatch`] consults before the operation executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Let the operation proceed to its implementation.
    Allow,
    /// Reject the operation; the reason is surfaced in the dispatch error.
    Deny(String),
}

/// One operation presented to a [`SysPolicy`] for a decision. Carries the raw
/// `library`/`export` together with the [`capability_effect`] classification so
/// a policy can gate on the exact operation or on coarse capability/effect.
#[derive(Debug, Clone, Copy)]
pub struct PolicyRequest<'a> {
    pub library: &'a str,
    pub export: &'a str,
    pub capability: &'a str,
    pub effect: &'a str,
}

/// An opt-in gate consulted by [`dispatch`] before every operation.
///
/// The runtime publishes a capability/effect classification for every export
/// (see [`capability_effect`]) but enforces nothing by default: a bare
/// `RuntimeState` allows everything, and the caap interpreter applies its own
/// richer policy *before* calling `dispatch`. Attaching a `SysPolicy` lets a
/// consumer that calls `dispatch` directly enforce a decision at the runtime
/// boundary itself — most importantly the C-ABI path serving LLVM-compiled
/// binaries and dlopen plugins, which would otherwise bypass all policy.
pub trait SysPolicy {
    /// Decide whether `request` is permitted; consulted before every dispatched
    /// operation. Returning [`PolicyDecision::Deny`] aborts the call.
    fn check(&self, request: PolicyRequest<'_>) -> PolicyDecision;
}

/// All stateful handle tables for one runtime session, owned explicitly.
///
/// The FFI layer holds one `RuntimeState` in a `thread_local` to serve
/// LLVM-compiled binaries and dlopen plugins; the caap interpreter holds one
/// per `HostServiceRegistry` so file/socket/process handles are scoped to a
/// session and released when the registry is dropped. Both call [`dispatch`]
/// with their own state, so the operation semantics remain a single source of
/// truth regardless of who owns the handles.
#[derive(Default)]
pub struct RuntimeState {
    pub fs: crate::fs::FsState,
    pub net: crate::net::NetState,
    pub proc: crate::proc::ProcState,
    /// Optional gate consulted before every dispatched operation. `None` (the
    /// default) allows everything and adds no per-call cost; see [`SysPolicy`].
    pub policy: Option<Box<dyn SysPolicy>>,
}

impl RuntimeState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a session whose operations are gated by `policy`.
    pub fn with_policy(policy: Box<dyn SysPolicy>) -> Self {
        Self {
            policy: Some(policy),
            ..Self::default()
        }
    }

    /// Attach or clear (`None`) the policy gate consulted by [`dispatch`].
    pub fn set_policy(&mut self, policy: Option<Box<dyn SysPolicy>>) {
        self.policy = policy;
    }
}

/// Dispatch invoke by library + export name, threading the session's handle
/// state into the stateful libraries.
pub fn dispatch(
    state: &mut RuntimeState,
    library: &str,
    export: &str,
    args: crate::ffi_value::SysArgs,
) -> crate::ffi_value::SysResult {
    validate_arity(library, export, args.0.len())?;
    if let Some(policy) = state.policy.as_ref() {
        let (capability, effect) = capability_effect(library, export);
        if let PolicyDecision::Deny(reason) = policy.check(PolicyRequest {
            library,
            export,
            capability,
            effect,
        }) {
            return Err(crate::ffi_value::SysError::permission_denied(format!(
                "{library}.{export}: denied by policy: {reason}"
            )));
        }
    }
    match library {
        "io" => crate::io::invoke(export, args),
        "path" => crate::path::invoke(export, args),
        "time" => crate::time::invoke(export, args),
        "rand" => crate::rand::invoke(export, args),
        "os" => crate::os::invoke(export, args),
        "net" => crate::net::invoke(&mut state.net, export, args),
        "fs" => crate::fs::invoke(&mut state.fs, export, args),
        "process" => crate::proc::invoke(&mut state.proc, export, args),
        _ => Err(format!("caap-sys-runtime: unknown library '{library}'").into()),
    }
}

fn validate_arity(
    library: &str,
    export: &str,
    actual: usize,
) -> Result<(), crate::ffi_value::SysError> {
    let Some(entry) = export_catalog()
        .iter()
        .find(|entry| entry.library == library && entry.export == export)
    else {
        return Ok(());
    };
    let min = entry.min_arity as usize;
    let max = entry.max_arity.map(|max| max as usize);
    if actual < min {
        return Err(crate::ffi_value::SysError::invalid_argument(format!(
            "{library}.{export}: expected at least {min} argument(s), got {actual}"
        )));
    }
    if let Some(max) = max {
        if actual > max {
            return Err(crate::ffi_value::SysError::invalid_argument(format!(
                "{library}.{export}: expected at most {max} argument(s), got {actual}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi_value::{SysArgs, SysValue};

    #[test]
    fn dispatch_rejects_missing_arguments_before_export_invoke() {
        let mut state = RuntimeState::new();
        let error = dispatch(&mut state, "io", "print", SysArgs(Vec::new())).unwrap_err();
        assert!(error.contains("io.print: expected at least 1 argument"));
    }

    #[test]
    fn dispatch_rejects_extra_arguments_before_export_invoke() {
        let mut state = RuntimeState::new();
        let error = dispatch(
            &mut state,
            "time",
            "unix_millis",
            SysArgs(vec![SysValue::Int(1)]),
        )
        .unwrap_err();
        assert!(error.contains("time.unix_millis: expected at most 0 argument"));
    }

    #[test]
    fn every_export_has_a_known_capability_and_effect() {
        for entry in export_catalog() {
            let (capability, effect) = capability_effect(entry.library, entry.export);
            assert_ne!(
                (capability, effect),
                ("unknown", "unknown"),
                "{}.{} is unclassified",
                entry.library,
                entry.export
            );
        }
    }

    /// A policy that denies any operation whose effect matches `0`.
    struct DenyEffect(&'static str);

    impl SysPolicy for DenyEffect {
        fn check(&self, request: PolicyRequest<'_>) -> PolicyDecision {
            if request.effect == self.0 {
                PolicyDecision::Deny(format!("effect '{}' is not permitted", self.0))
            } else {
                PolicyDecision::Allow
            }
        }
    }

    #[test]
    fn dispatch_classifies_errors_by_kind() {
        use crate::ffi_value::SysErrorKind;
        let mut state = RuntimeState::new();

        // Arity is rejected as an argument error.
        let error = dispatch(&mut state, "io", "print", SysArgs(Vec::new())).unwrap_err();
        assert_eq!(error.kind(), SysErrorKind::InvalidArgument);

        // A wrong-typed argument is also an argument error.
        let error = dispatch(
            &mut state,
            "fs",
            "read_text",
            SysArgs(vec![SysValue::Int(1)]),
        )
        .unwrap_err();
        assert_eq!(error.kind(), SysErrorKind::InvalidArgument);

        // A missing path surfaces the OS NotFound kind through `from_io`.
        let error = dispatch(
            &mut state,
            "fs",
            "read_text",
            SysArgs(vec![SysValue::Str(
                "/caap-does-not-exist-xyz/file".to_string(),
            )]),
        )
        .unwrap_err();
        assert_eq!(error.kind(), SysErrorKind::NotFound);

        // An unknown library stays unclassified.
        let error = dispatch(&mut state, "nope", "x", SysArgs(Vec::new())).unwrap_err();
        assert_eq!(error.kind(), SysErrorKind::Other);
    }

    #[test]
    fn dispatch_policy_denial_is_permission_denied() {
        use crate::ffi_value::SysErrorKind;
        let mut state = RuntimeState::with_policy(Box::new(DenyEffect("write")));
        let error = dispatch(
            &mut state,
            "io",
            "println",
            SysArgs(vec![SysValue::Str("x".to_string())]),
        )
        .unwrap_err();
        assert_eq!(error.kind(), SysErrorKind::PermissionDenied);
    }

    #[test]
    fn dispatch_rejects_operations_denied_by_policy_before_invoke() {
        let mut state = RuntimeState::with_policy(Box::new(DenyEffect("write")));
        // io.println classifies as console/write → denied before it touches stdout.
        let error = dispatch(
            &mut state,
            "io",
            "println",
            SysArgs(vec![SysValue::Str("should-not-print".to_string())]),
        )
        .unwrap_err();
        assert!(
            error.contains("io.println: denied by policy"),
            "got {error:?}"
        );
        assert!(error.contains("effect 'write'"), "got {error:?}");
    }

    #[test]
    fn dispatch_allows_operations_permitted_by_policy() {
        let mut state = RuntimeState::with_policy(Box::new(DenyEffect("write")));
        // path.join classifies as path/pure → permitted.
        let value = dispatch(
            &mut state,
            "path",
            "join",
            SysArgs(vec![
                SysValue::Str("a".to_string()),
                SysValue::Str("b".to_string()),
            ]),
        )
        .unwrap();
        assert_eq!(value, SysValue::Str("a/b".to_string()));
    }

    #[test]
    fn dispatch_without_policy_allows_everything() {
        let mut state = RuntimeState::new();
        assert!(state.policy.is_none());
        let value = dispatch(
            &mut state,
            "path",
            "join",
            SysArgs(vec![
                SysValue::Str("a".to_string()),
                SysValue::Str("b".to_string()),
            ]),
        )
        .unwrap();
        assert_eq!(value, SysValue::Str("a/b".to_string()));
    }

    #[test]
    fn capability_effect_catalog_covers_every_export() {
        let catalog = capability_effect_catalog();
        assert_eq!(catalog.len(), export_catalog().len());
        // Spot-check the read/write split that finer policy depends on.
        assert!(catalog.contains(&"fs.read_text filesystem read".to_string()));
        assert!(catalog.contains(&"fs.write_text filesystem write".to_string()));
        assert!(catalog.contains(&"net.connect network network".to_string()));
        assert!(catalog.contains(&"io.println console write".to_string()));
    }
}
