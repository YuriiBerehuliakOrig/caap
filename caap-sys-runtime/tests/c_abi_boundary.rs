//! Exercises the runtime through its *real* exported C ABI.
//!
//! The in-crate tests call the FFI functions in-process via the rlib, which does
//! not prove the symbols are actually exported from the cdylib with C linkage.
//! This test loads `libcaap_sys_runtime.{so,dylib}` with `dlopen`, resolves the
//! exports by name, and drives the JSON invoke surface and the policy hook —
//! catching, for example, a missing `#[no_mangle]`, an ABI-version mismatch, or
//! a policy gate that does not actually fire across the boundary.
//!
//! `cargo test` does not build the cdylib (only the rlib for the harness), so
//! the test builds it on demand and skips gracefully if that is impossible.

#![cfg(unix)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::process::Command;

// ── Resolved C-ABI signatures ───────────────────────────────────────────────

type AbiVersionFn = extern "C" fn() -> u32;
type CatalogCountFn = extern "C" fn() -> usize;
type InvokeFn = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *const c_char,
    *mut *mut c_char,
    *mut *mut c_char,
) -> c_int;
type StringFreeFn = unsafe extern "C" fn(*mut c_char);
type PolicyCallback =
    extern "C" fn(*const c_char, *const c_char, *const c_char, *const c_char) -> c_int;
type SetPolicyFn = unsafe extern "C" fn(Option<PolicyCallback>);

/// Test policy that denies every operation.
extern "C" fn deny_all(
    _library: *const c_char,
    _export: *const c_char,
    _capability: *const c_char,
    _effect: *const c_char,
) -> c_int {
    1
}

// ── dlopen handle ───────────────────────────────────────────────────────────

struct Library(*mut c_void);

impl Library {
    fn open(path: &std::path::Path) -> Library {
        let c_path = CString::new(path.to_str().expect("utf-8 path")).unwrap();
        // SAFETY: `c_path` is a valid null-terminated path to the cdylib.
        let handle = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW) };
        assert!(
            !handle.is_null(),
            "dlopen({}) failed: {}",
            path.display(),
            last_dl_error()
        );
        Library(handle)
    }

    /// Resolve a symbol and transmute it to `T` (a function-pointer type).
    ///
    /// # Safety
    /// `T` must be the correct function-pointer type for `name`.
    unsafe fn symbol<T: Copy>(&self, name: &str) -> T {
        assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
        let c_name = CString::new(name).unwrap();
        let ptr = libc::dlsym(self.0, c_name.as_ptr());
        assert!(!ptr.is_null(), "symbol {name:?} not found in cdylib");
        // Transmute the resolved address to the requested fn-pointer type.
        std::mem::transmute_copy::<*mut c_void, T>(&ptr)
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a handle returned by `dlopen` and not yet closed.
        unsafe {
            libc::dlclose(self.0);
        }
    }
}

fn last_dl_error() -> String {
    // SAFETY: dlerror returns a valid C string or null.
    unsafe {
        let err = libc::dlerror();
        if err.is_null() {
            "unknown error".to_string()
        } else {
            CStr::from_ptr(err).to_string_lossy().into_owned()
        }
    }
}

// ── cdylib discovery / on-demand build ──────────────────────────────────────

fn cdylib_path() -> Option<(PathBuf, bool)> {
    // The test binary lives at target/<profile>/deps/<exe>; the cdylib is one
    // directory up, at target/<profile>/lib<name>.<ext>.
    let exe = std::env::current_exe().ok()?;
    let profile_dir = exe.parent()?.parent()?.to_path_buf();
    let is_release = profile_dir.file_name()?.to_str()? == "release";
    let name = if cfg!(target_os = "macos") {
        "libcaap_sys_runtime.dylib"
    } else {
        "libcaap_sys_runtime.so"
    };
    Some((profile_dir.join(name), is_release))
}

fn ensure_cdylib() -> Option<PathBuf> {
    let (path, is_release) = cdylib_path()?;
    if path.exists() {
        return Some(path);
    }
    // `cargo test` builds only the rlib; build the cdylib explicitly. The outer
    // build has finished and released its lock by the time tests run, so this
    // nested invocation is safe.
    let mut cmd = Command::new(env!("CARGO"));
    cmd.args(["build", "-p", "caap-sys-runtime"]);
    if is_release {
        cmd.arg("--release");
    }
    let _ = cmd.status();
    path.exists().then_some(path)
}

// ── Invoke helper ───────────────────────────────────────────────────────────

struct InvokeOutcome {
    rc: c_int,
    out: Option<String>,
    error: Option<String>,
}

fn invoke(
    invoke_fn: InvokeFn,
    free_fn: StringFreeFn,
    library: &str,
    export: &str,
    args_json: &str,
) -> InvokeOutcome {
    let lib = CString::new(library).unwrap();
    let exp = CString::new(export).unwrap();
    let args = CString::new(args_json).unwrap();
    let mut out: *mut c_char = std::ptr::null_mut();
    let mut err: *mut c_char = std::ptr::null_mut();
    // SAFETY: all pointers are valid; out/err slots are read back and freed below.
    let rc = unsafe {
        invoke_fn(
            lib.as_ptr(),
            exp.as_ptr(),
            args.as_ptr(),
            &mut out,
            &mut err,
        )
    };
    // SAFETY: out/err are null or runtime-owned strings; take ownership and free.
    let take = |ptr: *mut c_char| -> Option<String> {
        if ptr.is_null() {
            return None;
        }
        let text = unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe { free_fn(ptr) };
        Some(text)
    };
    InvokeOutcome {
        rc,
        out: take(out),
        error: take(err),
    }
}

// ── The test ────────────────────────────────────────────────────────────────

#[test]
fn c_abi_boundary_invoke_and_policy_through_dlopen() {
    let Some(path) = ensure_cdylib() else {
        eprintln!("skipping: cdylib not present and could not be built");
        return;
    };
    let lib = Library::open(&path);

    // SAFETY: each symbol is resolved to its true exported signature.
    let abi_version: AbiVersionFn = unsafe { lib.symbol("caap_runtime_abi_version") };
    let catalog_count: CatalogCountFn = unsafe { lib.symbol("caap_runtime_catalog_count") };
    let invoke_fn: InvokeFn = unsafe { lib.symbol("caap_runtime_invoke") };
    let free_fn: StringFreeFn = unsafe { lib.symbol("caap_runtime_string_free") };
    let set_policy: SetPolicyFn = unsafe { lib.symbol("caap_runtime_set_policy") };

    // Descriptor surface is reachable and self-consistent.
    assert_eq!(abi_version(), 1, "unexpected ABI version across boundary");
    assert!(catalog_count() > 0, "catalog is empty across boundary");

    // A pure operation round-trips through the JSON invoke ABI.
    let joined = invoke(invoke_fn, free_fn, "path", "join", "[\"a\",\"b\"]");
    assert_eq!(joined.rc, 1, "path.join failed: {:?}", joined.error);
    assert_eq!(joined.out.as_deref(), Some("\"a/b\""));

    // With a deny-all policy installed, an invoke is rejected before dispatch —
    // proving the policy hook fires across the real boundary.
    unsafe { set_policy(Some(deny_all)) };
    let denied = invoke(invoke_fn, free_fn, "time", "unix_millis", "[]");
    assert_eq!(denied.rc, 0, "expected policy denial");
    let message = denied.error.unwrap_or_default();
    assert!(
        message.contains("denied by policy"),
        "expected policy denial, got {message:?}"
    );

    // Clearing the policy restores normal dispatch.
    unsafe { set_policy(None) };
    let allowed = invoke(invoke_fn, free_fn, "time", "unix_millis", "[]");
    assert_eq!(allowed.rc, 1, "expected success after clearing policy");
    let millis = allowed.out.unwrap_or_default();
    assert!(
        millis.parse::<i64>().is_ok(),
        "expected an integer timestamp, got {millis:?}"
    );
}
