//! The native (statically linked) C-ABI surface of caap-sys-runtime.
//!
//! `compile --target native-exe` links this crate as a `staticlib`, so these
//! symbols ARE the native runtime — there is no separate C runtime. Two tiers,
//! both calling the operation implementations **directly** (no
//! [`crate::catalog::dispatch`]): no catalog arity scan, no policy gate, no
//! library/export string match — just the owning module's `invoke` on a literal
//! export, against the shared thread-local session that the interpreter's handle
//! tables also flow through.
//!
//! 1. **Flat fast-path symbols** — the exact set the LLVM backend declares as
//!    typed externs (`sys.io`, the string builtins, `sys.time`, `sys.net`).
//!    Scalars are passed by value: string → [`CaapString`] == LLVM `{ ptr, i64 }`,
//!    i32 → i32, bool → i1. A `{ ptr, i64 }` aggregate is a 16-byte two-eightbyte
//!    INTEGER struct under the SysV/Win64 C ABIs, so `#[repr(C)] CaapString` by
//!    value is register-compatible with the LLVM representation.
//!
//! 2. **Per-export direct symbols** — one `caap_runtime_<library>_<export>` for
//!    every entry in the export catalog (io/path/time/rand/os/net/fs/process), so
//!    the *whole* sys interface is individually linkable, not just the flat hot
//!    path. These use the structured [`crate::ffi::CaapSysValue`] ABI (so map /
//!    list / bytes operations work uniformly) and call the module impl directly.
//!    The six `net` handle ops already covered flat (`listen`/`connect`/`accept`/
//!    `read`/`write`/`close`) are not re-emitted here — their flat symbol owns the
//!    name; every other net export (read_bytes/write_bytes/poll/…/udp_*) is here.
//!
//! ── Memory ownership ────────────────────────────────────────────────────────
//! A returned `CaapString` owns a heap buffer (via `cstring_lossy_nul` +
//! `CString::into_raw`) and is released with `caap_runtime_string_free`; a
//! returned `CaapSysValue` owns nested allocations released with
//! `caap_runtime_value_free`. Borrowed inputs are never freed by the callee.
//!
//! ── Security posture ────────────────────────────────────────────────────────
//! These entry points install no policy: a native executable has already cleared
//! the compile-time effect checks and runs with the host process's ambient
//! authority (native = trusted).

use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::{c_char, c_int};
use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::ffi::{
    cstring_lossy_nul, from_sys_value, to_sys_value, with_thread_runtime, CaapSysValue,
};
use crate::ffi_value::{SysArgs, SysError, SysResult, SysValue};

/// The string value ABI: a (ptr, len) pair matching LLVM `{ ptr, i64 }`. The
/// bytes are not required to be NUL-terminated; `len` is authoritative.
#[repr(C)]
pub struct CaapString {
    pub ptr: *const u8,
    pub len: i64,
}

impl CaapString {
    /// The empty string (null pointer, zero length).
    fn empty() -> Self {
        CaapString {
            ptr: std::ptr::null(),
            len: 0,
        }
    }

    /// Borrow the raw bytes for the duration of a call.
    ///
    /// # Safety
    /// `self.ptr` must point to `self.len` readable bytes (or be null with len 0).
    unsafe fn bytes(&self) -> &[u8] {
        if self.ptr.is_null() || self.len <= 0 {
            &[]
        } else {
            std::slice::from_raw_parts(self.ptr, self.len as usize)
        }
    }

    /// Borrow the bytes as UTF-8 (lossily — invalid sequences become U+FFFD).
    ///
    /// # Safety
    /// Same contract as [`CaapString::bytes`].
    unsafe fn as_str(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(self.bytes())
    }
}

/// Allocate an owned `CaapString` over a copy of `s`, freeable through the shared
/// `caap_runtime_string_free`.
fn owned_string(s: &str) -> CaapString {
    let cstring = cstring_lossy_nul(s);
    let len = cstring.as_bytes().len() as i64;
    let ptr = cstring.into_raw() as *const u8;
    CaapString { ptr, len }
}

/// Run an operation, turning a panic in the implementation into an ordinary error
/// instead of unwinding across the `extern "C"` boundary (which would abort).
fn guard(op: impl FnOnce() -> SysResult) -> SysResult {
    catch_unwind(AssertUnwindSafe(op))
        .unwrap_or_else(|_| Err(SysError::other("native runtime: operation panicked")))
}

// ════════════════════════════════════════════════════════════════════════════
// Tier 1 — flat fast-path symbols (the LLVM-declared typed externs)
// ════════════════════════════════════════════════════════════════════════════

// ── sys.io.println ──────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn caap_runtime_print_i32_ln(value: i32) -> i32 {
    let _ = guard(|| crate::io::invoke("println", SysArgs(vec![SysValue::Int(value as i64)])));
    value
}

/// # Safety
/// `value` must be a valid `CaapString` (see [`CaapString`]).
#[no_mangle]
pub unsafe extern "C" fn caap_runtime_print_string_ln(value: CaapString) -> i32 {
    let text = value.as_str().into_owned();
    let _ = guard(|| crate::io::invoke("println", SysArgs(vec![SysValue::Str(text)])));
    0
}

// ── String builtins (pure: implemented directly, not sys ops) ───────────────

/// # Safety
/// `left` and `right` must be valid `CaapString`s.
#[no_mangle]
pub unsafe extern "C" fn caap_runtime_string_concat2(
    left: CaapString,
    right: CaapString,
) -> CaapString {
    let mut joined = left.as_str().into_owned();
    joined.push_str(&right.as_str());
    owned_string(&joined)
}

#[no_mangle]
pub extern "C" fn caap_runtime_i32_to_string(value: i32) -> CaapString {
    owned_string(&value.to_string())
}

#[no_mangle]
pub extern "C" fn caap_runtime_bool_to_string(value: bool) -> CaapString {
    owned_string(if value { "true" } else { "false" })
}

/// # Safety
/// `left` and `right` must be valid `CaapString`s.
#[no_mangle]
pub unsafe extern "C" fn caap_runtime_string_eq(left: CaapString, right: CaapString) -> bool {
    left.bytes() == right.bytes()
}

/// FNV-1a over the raw bytes, truncated to i32 — matches the interpreter's value
/// hash contract (equal strings hash equal) and is pointer-identity independent.
///
/// # Safety
/// `value` must be a valid `CaapString`.
#[no_mangle]
pub unsafe extern "C" fn caap_runtime_string_hash(value: CaapString) -> i32 {
    let mut hash: u32 = 0x811c_9dc5; // FNV offset basis
    for byte in value.bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193); // FNV prime
    }
    hash as i32
}

/// # Safety
/// `value` must be a valid `CaapString`.
#[no_mangle]
pub unsafe extern "C" fn caap_runtime_string_to_i32(value: CaapString) -> i32 {
    value.as_str().trim().parse::<i32>().unwrap_or(0)
}

// ── sys.io.read_line / sys.time.unix_millis ─────────────────────────────────

#[no_mangle]
pub extern "C" fn caap_runtime_read_line() -> CaapString {
    // io.read_line returns Null at EOF and a Str (with its trailing newline) for
    // a line; strip a single trailing CR/LF to match a line-oriented read.
    match guard(|| crate::io::invoke("read_line", SysArgs(Vec::new()))) {
        Ok(SysValue::Str(mut line)) => {
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            owned_string(&line)
        }
        _ => CaapString::empty(),
    }
}

#[no_mangle]
pub extern "C" fn caap_runtime_unix_millis() -> i32 {
    // The flat native int ABI is i32 (see the `external_entry` contract), which
    // cannot hold a real unix-millis value (~1.7e12). Return the low 31 bits:
    // always non-negative — so `unix_millis() % n` stays in range instead of
    // going negative on a sign-bit-wrapping `as i32` — and still changing every
    // millisecond, i.e. usable as a seed. It is NOT a wall-clock value; use the
    // structured `caap_runtime_time_unix_millis` (i64) for that.
    match guard(|| crate::time::invoke("unix_millis", SysArgs(Vec::new()))) {
        Ok(SysValue::Int(millis)) => (millis & 0x7fff_ffff) as i32,
        _ => 0,
    }
}

// ── sys.net (flat) ──────────────────────────────────────────────────────────
//
// Handles are the abstract i32 IDs the net module hands out; integer entry
// points return -1 on error (net_read returns the empty string). listen/connect
// take a flat (host, port) — the LLVM call lowerer destructures the source
// `listen(map_of("host", h, "port", p))` into these — and rebuild the map the
// net module expects.

fn host_port_spec(host: String, port: i32) -> SysValue {
    SysValue::Map(HashMap::from([
        ("host".to_string(), SysValue::Str(host)),
        ("port".to_string(), SysValue::Int(port as i64)),
    ]))
}

fn net_handle_result(result: SysResult) -> i32 {
    match result {
        Ok(SysValue::Int(handle)) => handle as i32,
        _ => -1,
    }
}

/// Call a `net` operation directly against this thread's session.
fn net_call(export: &'static str, args: Vec<SysValue>) -> SysResult {
    guard(|| with_thread_runtime(|state| crate::net::invoke(&mut state.net, export, SysArgs(args))))
}

/// # Safety
/// `host` must be a valid `CaapString`.
#[no_mangle]
pub unsafe extern "C" fn caap_runtime_net_listen(host: CaapString, port: i32) -> i32 {
    let spec = host_port_spec(host.as_str().into_owned(), port);
    net_handle_result(net_call("listen", vec![spec]))
}

/// # Safety
/// `host` must be a valid `CaapString`.
#[no_mangle]
pub unsafe extern "C" fn caap_runtime_net_connect(host: CaapString, port: i32) -> i32 {
    let spec = host_port_spec(host.as_str().into_owned(), port);
    net_handle_result(net_call("connect", vec![spec]))
}

#[no_mangle]
pub extern "C" fn caap_runtime_net_accept(listener: i32, timeout_ms: i32) -> i32 {
    net_handle_result(net_call(
        "accept",
        vec![
            SysValue::Int(listener as i64),
            SysValue::Int(timeout_ms as i64),
        ],
    ))
}

#[no_mangle]
pub extern "C" fn caap_runtime_net_read(sock: i32, max_bytes: i32, timeout_ms: i32) -> CaapString {
    match net_call(
        "read",
        vec![
            SysValue::Int(sock as i64),
            SysValue::Int(max_bytes as i64),
            SysValue::Int(timeout_ms as i64),
        ],
    ) {
        Ok(SysValue::Str(text)) => owned_string(&text),
        _ => CaapString::empty(),
    }
}

/// # Safety
/// `data` must be a valid `CaapString`.
#[no_mangle]
pub unsafe extern "C" fn caap_runtime_net_write(sock: i32, data: CaapString) -> i32 {
    let bytes = data.as_str().into_owned();
    let written = bytes.len() as i32;
    match net_call(
        "write",
        vec![SysValue::Int(sock as i64), SysValue::Str(bytes)],
    ) {
        Ok(_) => written,
        Err(_) => -1,
    }
}

#[no_mangle]
pub extern "C" fn caap_runtime_net_close(handle: i32) -> i32 {
    match net_call("close", vec![SysValue::Int(handle as i64)]) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Tier 2 — per-export direct symbols over the structured CaapSysValue ABI
// ════════════════════════════════════════════════════════════════════════════

fn null_value() -> CaapSysValue {
    CaapSysValue {
        tag: 0,
        int_val: 0,
        float_val: 0.0,
        ptr_val: std::ptr::null_mut(),
        len_val: 0,
    }
}

/// # Safety
/// `err` must be null or a valid writable `*mut *mut c_char` slot.
unsafe fn set_error(err: *mut *mut c_char, message: &str) {
    if !err.is_null() {
        *err = cstring_lossy_nul(message).into_raw();
    }
}

/// Shared body for the per-export direct symbols: decode the `CaapSysValue`
/// arguments, run `op` directly (no `catalog::dispatch`), and encode the result.
/// Returns 1 on success (writing `*out`), 0 on error (writing `*err`).
///
/// # Safety
/// `args` must point to `count` initialised `CaapSysValue`s (or be null when
/// `count == 0`); `out`/`err` must be valid writable slots. On success `*out`
/// owns nested allocations (free with `caap_runtime_value_free`); on failure
/// `*err` is a runtime-allocated string (free with `caap_runtime_string_free`).
unsafe fn run_export(
    args: *const CaapSysValue,
    count: usize,
    out: *mut CaapSysValue,
    err: *mut *mut c_char,
    op: impl FnOnce(SysArgs) -> SysResult,
) -> c_int {
    if !out.is_null() {
        *out = null_value();
    }
    if err.is_null() {
        return 0;
    }
    *err = std::ptr::null_mut();
    if out.is_null() {
        set_error(err, "out pointer is null");
        return 0;
    }
    if args.is_null() && count > 0 {
        set_error(err, "args pointer is null for non-empty arguments");
        return 0;
    }

    let slice = if count == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(args, count)
    };
    let mut decoded = Vec::with_capacity(count);
    for value in slice {
        match to_sys_value(value) {
            Ok(value) => decoded.push(value),
            Err(error) => {
                set_error(err, &error);
                return 0;
            }
        }
    }

    match guard(|| op(SysArgs(decoded))) {
        Ok(value) => match from_sys_value(&value) {
            Ok(encoded) => {
                *out = encoded;
                1
            }
            Err(error) => {
                set_error(err, &error);
                0
            }
        },
        Err(error) => {
            set_error(err, &error.to_string());
            0
        }
    }
}

/// Define one `caap_runtime_<library>_<export>` direct symbol. The stateless arm
/// targets a module whose `invoke(export, args)` needs no session; the stateful
/// arm threads the named `RuntimeState` field (net/fs/proc).
macro_rules! native_export {
    ($symbol:ident, $module:ident, $export:literal) => {
        /// Direct per-export native entry point. See [`run_export`] for the ABI.
        ///
        /// # Safety
        /// Same contract as [`run_export`].
        #[no_mangle]
        pub unsafe extern "C" fn $symbol(
            args: *const CaapSysValue,
            count: usize,
            out: *mut CaapSysValue,
            err: *mut *mut c_char,
        ) -> c_int {
            run_export(args, count, out, err, move |a| {
                crate::$module::invoke($export, a)
            })
        }
    };
    ($symbol:ident, $module:ident, $field:ident, $export:literal) => {
        /// Direct per-export native entry point. See [`run_export`] for the ABI.
        ///
        /// # Safety
        /// Same contract as [`run_export`].
        #[no_mangle]
        pub unsafe extern "C" fn $symbol(
            args: *const CaapSysValue,
            count: usize,
            out: *mut CaapSysValue,
            err: *mut *mut c_char,
        ) -> c_int {
            run_export(args, count, out, err, move |a| {
                with_thread_runtime(move |state| {
                    crate::$module::invoke(&mut state.$field, $export, a)
                })
            })
        }
    };
}

// io
native_export!(caap_runtime_io_print, io, "print");
native_export!(caap_runtime_io_println, io, "println");
native_export!(caap_runtime_io_write, io, "write");
native_export!(caap_runtime_io_eprint, io, "eprint");
native_export!(caap_runtime_io_eprintln, io, "eprintln");
native_export!(caap_runtime_io_flush_stdout, io, "flush_stdout");
native_export!(caap_runtime_io_flush_stderr, io, "flush_stderr");
native_export!(caap_runtime_io_read_line, io, "read_line");
native_export!(caap_runtime_io_read_all, io, "read_all");
native_export!(caap_runtime_io_write_bytes, io, "write_bytes");

// path
native_export!(caap_runtime_path_join, path, "join");
native_export!(caap_runtime_path_basename, path, "basename");
native_export!(caap_runtime_path_dirname, path, "dirname");
native_export!(caap_runtime_path_extension, path, "extension");
native_export!(caap_runtime_path_stem, path, "stem");
native_export!(caap_runtime_path_with_extension, path, "with_extension");
native_export!(caap_runtime_path_is_absolute, path, "is_absolute");
native_export!(caap_runtime_path_normalize, path, "normalize");
native_export!(caap_runtime_path_split, path, "split");
native_export!(caap_runtime_path_strip_prefix, path, "strip_prefix");

// time
native_export!(caap_runtime_time_now_unix_ns, time, "now_unix_ns");
native_export!(caap_runtime_time_unix_millis, time, "unix_millis");
native_export!(caap_runtime_time_unix_seconds, time, "unix_seconds");
native_export!(caap_runtime_time_monotonic_ns, time, "monotonic_ns");
native_export!(caap_runtime_time_sleep_ms, time, "sleep_ms");

// rand
native_export!(caap_runtime_rand_bytes, rand, "bytes");
native_export!(caap_runtime_rand_int, rand, "int");
native_export!(caap_runtime_rand_float, rand, "float");
native_export!(caap_runtime_rand_bool, rand, "bool");

// os
native_export!(caap_runtime_os_env_get, os, "env_get");
native_export!(caap_runtime_os_env_has, os, "env_has");
native_export!(caap_runtime_os_env_keys, os, "env_keys");
native_export!(caap_runtime_os_env_vars, os, "env_vars");
native_export!(caap_runtime_os_getcwd, os, "getcwd");
native_export!(caap_runtime_os_current_exe, os, "current_exe");
native_export!(caap_runtime_os_temp_dir, os, "temp_dir");
native_export!(caap_runtime_os_platform, os, "platform");
native_export!(caap_runtime_os_arch, os, "arch");
native_export!(caap_runtime_os_family, os, "family");
native_export!(
    caap_runtime_os_available_parallelism,
    os,
    "available_parallelism"
);
native_export!(caap_runtime_os_hostname, os, "hostname");
native_export!(caap_runtime_os_set_current_dir, os, "set_current_dir");
native_export!(caap_runtime_os_set_env, os, "set_env");
native_export!(caap_runtime_os_remove_env, os, "remove_env");

// net — every export except the six provided flat above (listen/connect/accept/
// read/write/close), whose flat symbol owns the `caap_runtime_net_<op>` name.
native_export!(caap_runtime_net_read_bytes, net, net, "read_bytes");
native_export!(caap_runtime_net_write_bytes, net, net, "write_bytes");
native_export!(caap_runtime_net_poll, net, net, "poll");
native_export!(caap_runtime_net_is_ip, net, net, "is_ip");
native_export!(caap_runtime_net_is_loopback, net, net, "is_loopback");
native_export!(caap_runtime_net_host_port, net, net, "host_port");
native_export!(caap_runtime_net_resolve, net, net, "resolve");
native_export!(caap_runtime_net_local_addr, net, net, "local_addr");
native_export!(caap_runtime_net_peer_addr, net, net, "peer_addr");
native_export!(caap_runtime_net_shutdown, net, net, "shutdown");
native_export!(caap_runtime_net_udp_bind, net, net, "udp_bind");
native_export!(caap_runtime_net_udp_send_to, net, net, "udp_send_to");
native_export!(caap_runtime_net_udp_recv_from, net, net, "udp_recv_from");

// fs
native_export!(caap_runtime_fs_exists, fs, fs, "exists");
native_export!(caap_runtime_fs_read_text, fs, fs, "read_text");
native_export!(caap_runtime_fs_write_text, fs, fs, "write_text");
native_export!(caap_runtime_fs_append_text, fs, fs, "append_text");
native_export!(caap_runtime_fs_is_file, fs, fs, "is_file");
native_export!(caap_runtime_fs_is_dir, fs, fs, "is_dir");
native_export!(caap_runtime_fs_metadata, fs, fs, "metadata");
native_export!(caap_runtime_fs_canonicalize, fs, fs, "canonicalize");
native_export!(caap_runtime_fs_list_dir, fs, fs, "list_dir");
native_export!(caap_runtime_fs_create_dir, fs, fs, "create_dir");
native_export!(caap_runtime_fs_create_dir_all, fs, fs, "create_dir_all");
native_export!(caap_runtime_fs_remove_file, fs, fs, "remove_file");
native_export!(caap_runtime_fs_remove_dir, fs, fs, "remove_dir");
native_export!(caap_runtime_fs_remove_dir_all, fs, fs, "remove_dir_all");
native_export!(caap_runtime_fs_rename, fs, fs, "rename");
native_export!(caap_runtime_fs_copy_file, fs, fs, "copy_file");
native_export!(caap_runtime_fs_read_link, fs, fs, "read_link");
native_export!(caap_runtime_fs_hard_link, fs, fs, "hard_link");
native_export!(caap_runtime_fs_symlink, fs, fs, "symlink");
native_export!(caap_runtime_fs_set_readonly, fs, fs, "set_readonly");
native_export!(caap_runtime_fs_set_permissions, fs, fs, "set_permissions");
native_export!(caap_runtime_fs_read_bytes, fs, fs, "read_bytes");
native_export!(caap_runtime_fs_write_bytes, fs, fs, "write_bytes");
native_export!(caap_runtime_fs_append_bytes, fs, fs, "append_bytes");
native_export!(caap_runtime_fs_file_read_bytes, fs, fs, "file_read_bytes");
native_export!(caap_runtime_fs_file_write_bytes, fs, fs, "file_write_bytes");
native_export!(caap_runtime_fs_open_file, fs, fs, "open_file");
native_export!(caap_runtime_fs_close_file, fs, fs, "close_file");
native_export!(
    caap_runtime_fs_file_read_all_text,
    fs,
    fs,
    "file_read_all_text"
);
native_export!(caap_runtime_fs_file_read_line, fs, fs, "file_read_line");
native_export!(caap_runtime_fs_file_write, fs, fs, "file_write");
native_export!(caap_runtime_fs_file_flush, fs, fs, "file_flush");
native_export!(caap_runtime_fs_file_seek, fs, fs, "file_seek");
native_export!(caap_runtime_fs_file_metadata, fs, fs, "file_metadata");
native_export!(caap_runtime_fs_open_dir, fs, fs, "open_dir");
native_export!(caap_runtime_fs_close_dir, fs, fs, "close_dir");
native_export!(caap_runtime_fs_dir_list, fs, fs, "dir_list");

// process (the catalog library is "process"; the module is `proc`)
native_export!(caap_runtime_process_id, proc, proc, "id");
native_export!(caap_runtime_process_args, proc, proc, "args");
native_export!(caap_runtime_process_run, proc, proc, "run");
native_export!(caap_runtime_process_spawn, proc, proc, "spawn");
native_export!(caap_runtime_process_wait, proc, proc, "wait");
native_export!(caap_runtime_process_wait_result, proc, proc, "wait_result");
native_export!(caap_runtime_process_kill, proc, proc, "kill");
native_export!(caap_runtime_process_write_stdin, proc, proc, "write_stdin");
native_export!(caap_runtime_process_close_stdin, proc, proc, "close_stdin");
native_export!(caap_runtime_process_read_stdout, proc, proc, "read_stdout");
native_export!(caap_runtime_process_read_stderr, proc, proc, "read_stderr");

// Note: returned strings are freed through the `caap_runtime_string_free` symbol
// exported by `crate::ffi` (the same `CString::into_raw` allocation
// `owned_string` produces); returned `CaapSysValue`s through `caap_runtime_value_free`.

/// Every Tier-2 direct symbol's `(library, export)`, kept in lockstep with the
/// `native_export!` invocations above (each macro line has exactly one entry).
/// A test asserts this equals the export catalog minus the six flat-only net
/// ops, so adding a catalog export without its native symbol fails the build.
#[cfg(test)]
const NATIVE_DIRECT_EXPORTS: &[(&str, &str)] = &[
    ("io", "print"),
    ("io", "println"),
    ("io", "write"),
    ("io", "eprint"),
    ("io", "eprintln"),
    ("io", "flush_stdout"),
    ("io", "flush_stderr"),
    ("io", "read_line"),
    ("io", "read_all"),
    ("io", "write_bytes"),
    ("path", "join"),
    ("path", "basename"),
    ("path", "dirname"),
    ("path", "extension"),
    ("path", "stem"),
    ("path", "with_extension"),
    ("path", "is_absolute"),
    ("path", "normalize"),
    ("path", "split"),
    ("path", "strip_prefix"),
    ("time", "now_unix_ns"),
    ("time", "unix_millis"),
    ("time", "unix_seconds"),
    ("time", "monotonic_ns"),
    ("time", "sleep_ms"),
    ("rand", "bytes"),
    ("rand", "int"),
    ("rand", "float"),
    ("rand", "bool"),
    ("os", "env_get"),
    ("os", "env_has"),
    ("os", "env_keys"),
    ("os", "env_vars"),
    ("os", "getcwd"),
    ("os", "current_exe"),
    ("os", "temp_dir"),
    ("os", "platform"),
    ("os", "arch"),
    ("os", "family"),
    ("os", "available_parallelism"),
    ("os", "hostname"),
    ("os", "set_current_dir"),
    ("os", "set_env"),
    ("os", "remove_env"),
    ("net", "read_bytes"),
    ("net", "write_bytes"),
    ("net", "poll"),
    ("net", "is_ip"),
    ("net", "is_loopback"),
    ("net", "host_port"),
    ("net", "resolve"),
    ("net", "local_addr"),
    ("net", "peer_addr"),
    ("net", "shutdown"),
    ("net", "udp_bind"),
    ("net", "udp_send_to"),
    ("net", "udp_recv_from"),
    ("fs", "exists"),
    ("fs", "read_text"),
    ("fs", "write_text"),
    ("fs", "append_text"),
    ("fs", "is_file"),
    ("fs", "is_dir"),
    ("fs", "metadata"),
    ("fs", "canonicalize"),
    ("fs", "list_dir"),
    ("fs", "create_dir"),
    ("fs", "create_dir_all"),
    ("fs", "remove_file"),
    ("fs", "remove_dir"),
    ("fs", "remove_dir_all"),
    ("fs", "rename"),
    ("fs", "copy_file"),
    ("fs", "read_link"),
    ("fs", "hard_link"),
    ("fs", "symlink"),
    ("fs", "set_readonly"),
    ("fs", "set_permissions"),
    ("fs", "read_bytes"),
    ("fs", "write_bytes"),
    ("fs", "append_bytes"),
    ("fs", "file_read_bytes"),
    ("fs", "file_write_bytes"),
    ("fs", "open_file"),
    ("fs", "close_file"),
    ("fs", "file_read_all_text"),
    ("fs", "file_read_line"),
    ("fs", "file_write"),
    ("fs", "file_flush"),
    ("fs", "file_seek"),
    ("fs", "file_metadata"),
    ("fs", "open_dir"),
    ("fs", "close_dir"),
    ("fs", "dir_list"),
    ("process", "id"),
    ("process", "args"),
    ("process", "run"),
    ("process", "spawn"),
    ("process", "wait"),
    ("process", "wait_result"),
    ("process", "kill"),
    ("process", "write_stdin"),
    ("process", "close_stdin"),
    ("process", "read_stdout"),
    ("process", "read_stderr"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::caap_runtime_value_free;

    fn read(s: &CaapString) -> String {
        unsafe { s.as_str().into_owned() }
    }

    /// A non-owning view over an owned `CaapString` (shares its pointer) for
    /// passing into the by-value ABI without transferring ownership.
    fn borrow(s: &CaapString) -> CaapString {
        CaapString {
            ptr: s.ptr,
            len: s.len,
        }
    }

    /// Release an owned `CaapString` through the real free symbol, so the tests
    /// don't leak their heap buffers.
    fn free_str(s: CaapString) {
        if !s.ptr.is_null() {
            unsafe { crate::ffi::caap_runtime_string_free(s.ptr as *mut c_char) };
        }
    }

    #[test]
    fn string_builtins_round_trip_through_the_native_abi() {
        unsafe {
            let a = owned_string("foo");
            let b = owned_string("bar");
            let joined = caap_runtime_string_concat2(borrow(&a), borrow(&b));
            assert_eq!(read(&joined), "foobar");

            let foo2 = owned_string("foo");
            assert!(caap_runtime_string_eq(borrow(&a), borrow(&foo2)));
            assert!(!caap_runtime_string_eq(borrow(&a), borrow(&b)));

            let int_str = caap_runtime_i32_to_string(-42);
            assert_eq!(read(&int_str), "-42");
            let bool_str = caap_runtime_bool_to_string(true);
            assert_eq!(read(&bool_str), "true");

            let n = owned_string("  123 ");
            assert_eq!(caap_runtime_string_to_i32(borrow(&n)), 123);

            // Each `owned_string`/returned `CaapString` owns a distinct buffer; the
            // `borrow` copies share a pointer and are never freed separately.
            for owned in [a, b, joined, foo2, int_str, bool_str, n] {
                free_str(owned);
            }
        }
    }

    #[test]
    fn empty_string_decodes_from_null_pointer() {
        let empty = CaapString::empty();
        assert_eq!(read(&empty), "");
        unsafe {
            assert_eq!(caap_runtime_string_hash(empty), 0x811c_9dc5u32 as i32);
        }
    }

    #[test]
    fn unix_millis_seed_is_non_negative() {
        // The i32 flat clock masks to 31 bits, so it must never come back negative
        // (a sign-bit-wrapping `as i32` previously could, breaking `_ % n`).
        assert!(caap_runtime_unix_millis() >= 0);
    }

    #[test]
    fn tier2_direct_symbols_cover_the_catalog() {
        use crate::catalog::export_catalog;
        use std::collections::BTreeSet;
        // Served by the Tier-1 flat fast-path symbols; intentionally no Tier-2 symbol.
        const FLAT_NET: &[&str] = &["listen", "connect", "accept", "read", "write", "close"];

        let covered: BTreeSet<(&str, &str)> = NATIVE_DIRECT_EXPORTS.iter().copied().collect();
        let missing: Vec<String> = export_catalog()
            .iter()
            .filter(|entry| !(entry.library == "net" && FLAT_NET.contains(&entry.export)))
            .filter(|entry| !covered.contains(&(entry.library, entry.export)))
            .map(|entry| format!("{}.{}", entry.library, entry.export))
            .collect();
        assert!(
            missing.is_empty(),
            "catalog exports without a Tier-2 native symbol (add a native_export! + NATIVE_DIRECT_EXPORTS entry): {missing:?}"
        );

        let catalog: BTreeSet<(&str, &str)> = export_catalog()
            .iter()
            .map(|entry| (entry.library, entry.export))
            .collect();
        let stale: Vec<&(&str, &str)> = NATIVE_DIRECT_EXPORTS
            .iter()
            .filter(|pair| !catalog.contains(*pair))
            .collect();
        assert!(
            stale.is_empty(),
            "Tier-2 symbols with no catalog export: {stale:?}"
        );
    }

    #[test]
    fn per_export_direct_symbol_runs_without_dispatch() {
        // A representative tier-2 symbol over the structured ABI: path.join is
        // pure, so it exercises decode → direct module call → encode end to end.
        unsafe {
            let a = from_sys_value(&SysValue::Str("a".to_string())).unwrap();
            let b = from_sys_value(&SysValue::Str("b".to_string())).unwrap();
            let args = [a, b];
            let mut out = null_value();
            let mut err: *mut c_char = std::ptr::null_mut();

            let rc = caap_runtime_path_join(args.as_ptr(), args.len(), &mut out, &mut err);

            assert_eq!(rc, 1, "expected success");
            assert!(err.is_null());
            assert_eq!(
                to_sys_value(&out).unwrap(),
                SysValue::Str("a/b".to_string())
            );
            caap_runtime_value_free(&mut out);
        }
    }
}
