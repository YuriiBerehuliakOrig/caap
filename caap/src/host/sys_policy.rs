//! Host policy applied to sys-runtime calls.
//!
//! Every `library.export` the interpreter exposes is dispatched to
//! `caap_sys_runtime` for its behaviour. This module is the single place where
//! caap's host policy is enforced *before* that dispatch: capability granularity
//! is gated at bind time (see `HostServiceRegistry::export`), while the
//! call-time concerns live here — filesystem sandboxing (which also rewrites the
//! path argument to its normalized, policy-checked form), network/process/stdin
//! permission flags, and the OS environment allowlist.
//!
//! No sys operation is reimplemented here; `authorize` only inspects/rewrites
//! arguments or short-circuits, and `filter_result` post-filters the environment
//! listing exports against the allowlist.

use std::cell::RefCell;
use std::rc::Rc;

use caap_sys_runtime::ffi_value::{SysArgs, SysValue};

use crate::values::{eval_err, EvalSignal};

use super::fn_fs::authorize_fs_path;
use super::fn_io::require_stdin_allowed;
use super::fn_misc::{
    env_name_allowed, require_net_connect_allowed, require_net_listen_allowed,
    require_process_spawn_allowed,
};
use super::HostSystemPolicy;

/// The outcome of authorizing a call against host policy.
pub(super) enum Authorization {
    /// Arguments are authorized (and possibly rewritten); proceed to dispatch.
    Proceed,
    /// Policy hides this result; return the given value without dispatching.
    Short(SysValue),
}

type PolicyRef = Rc<RefCell<HostSystemPolicy>>;

/// Enforce call-time host policy for `library.export`, mutating `args` in place
/// (e.g. rewriting filesystem paths to their normalized, sandbox-checked form).
pub(super) fn authorize(
    policy: &PolicyRef,
    library: &str,
    export: &str,
    args: &mut SysArgs,
) -> Result<Authorization, EvalSignal> {
    match library {
        "fs" => authorize_fs(policy, export, args),
        "net" => authorize_net(policy, export),
        "process" => authorize_process(policy, export),
        "io" => authorize_io(policy, export),
        "os" => authorize_os(policy, export, args),
        // path, time and any pure exports need no policy.
        _ => Ok(Authorization::Proceed),
    }
}

/// Post-filter a dispatch result against host policy. Only the OS environment
/// listing exports are filtered (to the configured allowlist); everything else
/// passes through unchanged.
pub(super) fn filter_result(
    policy: &PolicyRef,
    library: &str,
    export: &str,
    value: SysValue,
) -> SysValue {
    if library != "os" {
        return value;
    }
    match export {
        "env_keys" => match value {
            SysValue::List(keys) => SysValue::List(
                keys.into_iter()
                    .filter(|key| match key {
                        SysValue::Str(name) => env_name_allowed(policy, name),
                        _ => true,
                    })
                    .collect(),
            ),
            other => other,
        },
        "env_vars" => match value {
            SysValue::Map(vars) => SysValue::Map(
                vars.into_iter()
                    .filter(|(name, _)| env_name_allowed(policy, name))
                    .collect(),
            ),
            other => other,
        },
        _ => value,
    }
}

// ── Per-library authorization ───────────────────────────────────────────────

fn authorize_fs(
    policy: &PolicyRef,
    export: &str,
    args: &mut SysArgs,
) -> Result<Authorization, EvalSignal> {
    let context = format!("fs.{export}");
    match export {
        // Read a single path argument at index 0.
        "exists" | "read_text" | "is_file" | "is_dir" | "metadata" | "canonicalize"
        | "list_dir" | "open_dir" | "read_link" | "read_bytes" => {
            rewrite_fs_path(policy, args, 0, "read", &context)?;
        }
        // Write a single path argument at index 0.
        "write_text" | "append_text" | "create_dir" | "create_dir_all" | "remove_file"
        | "remove_dir" | "remove_dir_all" | "set_readonly" | "set_permissions" | "write_bytes"
        | "append_bytes" => {
            rewrite_fs_path(policy, args, 0, "write", &context)?;
        }
        // Source is read, destination is written.
        "rename" | "copy_file" | "hard_link" => {
            rewrite_fs_path(policy, args, 0, "read", &context)?;
            rewrite_fs_path(policy, args, 1, "write", &context)?;
        }
        // Only the link itself (index 1) is written; the target (index 0) is the
        // link's textual contents, not a path this call reads or writes.
        "symlink" => {
            rewrite_fs_path(policy, args, 1, "write", &context)?;
        }
        // open-file carries its path inside the spec map; the access verb
        // depends on the open flags.
        "open_file" => authorize_fs_open_file(policy, args, &context)?,
        // Handle-based operations (file-*, close-*, dir-list) touch no path.
        _ => {}
    }
    Ok(Authorization::Proceed)
}

fn authorize_fs_open_file(
    policy: &PolicyRef,
    args: &mut SysArgs,
    context: &str,
) -> Result<(), EvalSignal> {
    let spec = args
        .0
        .get_mut(0)
        .ok_or_else(|| eval_err(format!("{context}: missing spec")))?;
    let SysValue::Map(map) = spec else {
        return Err(eval_err(format!("{context}: spec must be a map")));
    };
    let raw = match map.get("path") {
        Some(SysValue::Str(path)) => path.clone(),
        Some(_) => return Err(eval_err(format!("{context}: spec.path must be a string"))),
        None => return Err(eval_err(format!("{context}: missing path"))),
    };
    let flag = |key: &str| matches!(map.get(key), Some(SysValue::Bool(true)));
    let (read, write, append) = (flag("read"), flag("write"), flag("append"));
    // Mirror the runtime's default: with no read/write/append flag, the file is
    // opened for reading.
    let read = read || (!write && !append);
    let writes = write || append || flag("truncate") || flag("create") || flag("create_new");

    let policy_borrow = policy.borrow();
    if read {
        authorize_fs_path(&policy_borrow, &raw, "read", context)?;
    }
    let normalized = if writes {
        authorize_fs_path(&policy_borrow, &raw, "write", context)?
    } else {
        authorize_fs_path(&policy_borrow, &raw, "read", context)?
    };
    drop(policy_borrow);
    map.insert("path".to_string(), SysValue::Str(normalized));
    Ok(())
}

fn rewrite_fs_path(
    policy: &PolicyRef,
    args: &mut SysArgs,
    idx: usize,
    verb: &str,
    context: &str,
) -> Result<(), EvalSignal> {
    let raw = match args.0.get(idx) {
        Some(SysValue::Str(path)) => path.clone(),
        Some(_) => return Err(eval_err(format!("{context}: path must be a string"))),
        None => return Err(eval_err(format!("{context}: missing path argument"))),
    };
    let normalized = authorize_fs_path(&policy.borrow(), &raw, verb, context)?;
    args.0[idx] = SysValue::Str(normalized);
    Ok(())
}

fn authorize_net(policy: &PolicyRef, export: &str) -> Result<Authorization, EvalSignal> {
    match export {
        // Binding (TCP listen or UDP bind) requires inbound permission.
        "listen" | "udp_bind" => require_net_listen_allowed(policy, &format!("net.{export}"))?,
        // Outbound traffic (connect, datagram send, DNS resolution) requires
        // outbound permission.
        "connect" | "udp_send_to" | "resolve" => {
            require_net_connect_allowed(policy, &format!("net.{export}"))?
        }
        // accept/read/write/close/poll/local-addr/peer-addr/shutdown and the pure
        // helpers operate on already-authorized handles or are side-effect-free.
        _ => {}
    }
    Ok(Authorization::Proceed)
}

fn authorize_process(policy: &PolicyRef, export: &str) -> Result<Authorization, EvalSignal> {
    match export {
        "run" => require_process_spawn_allowed(policy, "process.run")?,
        "spawn" => require_process_spawn_allowed(policy, "process.spawn")?,
        _ => {}
    }
    Ok(Authorization::Proceed)
}

fn authorize_io(policy: &PolicyRef, export: &str) -> Result<Authorization, EvalSignal> {
    match export {
        "read_line" => require_stdin_allowed(policy, "io.read_line")?,
        "read_all" => require_stdin_allowed(policy, "io.read_all")?,
        _ => {}
    }
    Ok(Authorization::Proceed)
}

fn authorize_os(
    policy: &PolicyRef,
    export: &str,
    args: &SysArgs,
) -> Result<Authorization, EvalSignal> {
    match export {
        // env-get / env-has on a disallowed name are hidden as if the variable
        // were absent, rather than erroring.
        "env_get" if !os_arg_name_allowed(policy, args) => {
            return Ok(Authorization::Short(SysValue::Null))
        }
        "env_has" if !os_arg_name_allowed(policy, args) => {
            return Ok(Authorization::Short(SysValue::Bool(false)))
        }
        // Mutating a disallowed variable is an explicit error (a write must not
        // silently no-op the way a hidden read returns absent).
        "set_env" | "remove_env" if !os_arg_name_allowed(policy, args) => {
            return Err(eval_err(format!(
                "os.{export}: environment variable is outside the allowed set"
            )))
        }
        // env-keys / env-vars are filtered after dispatch in `filter_result`.
        _ => {}
    }
    Ok(Authorization::Proceed)
}

fn os_arg_name_allowed(policy: &PolicyRef, args: &SysArgs) -> bool {
    match args.0.first() {
        Some(SysValue::Str(name)) => env_name_allowed(policy, name),
        // Non-string / missing names are left for dispatch to reject with the
        // canonical argument error.
        _ => true,
    }
}
