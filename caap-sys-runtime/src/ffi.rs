//! C ABI exports for dlopen-based plugin loading and LLVM static linking.
//!
//! The kernel calls:
//!   caap_runtime_abi_descriptor() → CaapRuntimeAbiDescriptor
//!   caap_runtime_abi_version() → u32
//!   caap_runtime_catalog_count() → usize
//!   caap_runtime_catalog_entries() → *const CaapCatalogEntry
//!   caap_runtime_invoke(library, export, args_json, out_json, error) → bool
//!
//! It may optionally call:
//!   caap_runtime_set_policy(callback) — install a per-thread gate that vets
//!   every invoke against the operation's capability/effect before dispatch.
//!
//! JSON is used as the serialisation layer for the C boundary because the
//! value graph is recursive and cannot be expressed in a flat C struct.

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::ptr;

use crate::catalog::{PolicyDecision, PolicyRequest, RuntimeState, SysPolicy};

thread_local! {
    /// The single runtime handle state serving C-ABI consumers (LLVM-compiled
    /// binaries and dlopen plugins). The interpreter does not use this — it owns
    /// its own `RuntimeState` per session — so handle lifetime here is bounded by
    /// the calling thread, which is the correct scope for a standalone process.
    static RUNTIME_STATE: RefCell<RuntimeState> = RefCell::new(RuntimeState::new());
}

/// Run `f` against this thread's C-ABI [`RuntimeState`]. Shared with
/// [`crate::ffi_native`], whose per-function LLVM-extern symbols dispatch against
/// the same thread-local session so a native binary's `sys.net` handles persist
/// across `listen`/`accept`/`read`/`close` calls.
pub(crate) fn with_thread_runtime<R>(f: impl FnOnce(&mut RuntimeState) -> R) -> R {
    RUNTIME_STATE.with(|state| f(&mut state.borrow_mut()))
}

// ── Catalog ABI ───────────────────────────────────────────────────────────────

#[repr(C)]
pub struct CaapCatalogEntry {
    pub library: *const c_char,
    pub export: *const c_char,
    pub min_arity: c_int,
    pub max_arity: c_int, // -1 = variadic
}

// Safety: pointers point to 'static CStrings held in CATALOG_STRINGS.
unsafe impl Send for CaapCatalogEntry {}
unsafe impl Sync for CaapCatalogEntry {}

// Static catalog — built once at startup.
use std::sync::OnceLock;

/// Version of the C-ABI symbol/dispatch contract; bumped on a breaking ABI change.
pub const CAAP_RUNTIME_ABI_VERSION: u32 = 1;
/// Version of the serialized value encoding crossing the C-ABI boundary.
/// v1: `Null`/`Bool`/`Int`/`Float`/`Str`/`List`/`Map`. v2 adds `Bytes`
/// (direct-ABI tag 7; JSON `{"$caap_b64": "..."}`). A host must reject a value
/// stream whose version it does not understand.
pub const CAAP_RUNTIME_VALUE_ENCODING_VERSION: u32 = 2;
/// Version of the exported catalog's schema (entry layout / field meanings).
pub const CAAP_RUNTIME_CATALOG_SCHEMA_VERSION: u32 = 1;
pub const CAAP_RUNTIME_REQUIRED_SYMBOLS: &[&str] = &[
    "caap_runtime_abi_descriptor",
    "caap_runtime_abi_version",
    "caap_runtime_catalog_count",
    "caap_runtime_catalog_entries",
    "caap_runtime_invoke",
    "caap_runtime_string_free",
    "caap_runtime_invoke_direct",
    "caap_runtime_value_free",
];
/// The runtime's capability/effect declaration, one entry per exported
/// operation. Derived from the export catalog (see
/// [`crate::catalog::capability_effect_catalog`]) and surfaced through the ABI
/// descriptor's `capability_effect_catalog_hash`.
pub fn caap_runtime_capability_effect_catalog() -> Vec<String> {
    crate::catalog::capability_effect_catalog()
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaapRuntimeAbiDescriptor {
    pub abi_version: u32,
    pub value_encoding_version: u32,
    pub catalog_schema_version: u32,
    pub required_symbol_count: u32,
    pub required_symbols_hash: u64,
    pub capability_effect_catalog_hash: u64,
}

pub fn caap_runtime_required_symbols_hash() -> u64 {
    stable_abi_hash(
        "caap-runtime-required-symbols:v1",
        CAAP_RUNTIME_REQUIRED_SYMBOLS,
    )
}

pub fn caap_runtime_capability_effect_catalog_hash() -> u64 {
    let entries = caap_runtime_capability_effect_catalog();
    let refs: Vec<&str> = entries.iter().map(String::as_str).collect();
    stable_abi_hash("caap-runtime-capability-effect-catalog:v1", &refs)
}

fn caap_runtime_abi_descriptor_value() -> CaapRuntimeAbiDescriptor {
    CaapRuntimeAbiDescriptor {
        abi_version: CAAP_RUNTIME_ABI_VERSION,
        value_encoding_version: CAAP_RUNTIME_VALUE_ENCODING_VERSION,
        catalog_schema_version: CAAP_RUNTIME_CATALOG_SCHEMA_VERSION,
        required_symbol_count: CAAP_RUNTIME_REQUIRED_SYMBOLS.len() as u32,
        required_symbols_hash: caap_runtime_required_symbols_hash(),
        capability_effect_catalog_hash: caap_runtime_capability_effect_catalog_hash(),
    }
}

fn stable_abi_hash(domain: &str, entries: &[&str]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    for byte in domain.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    for entry in entries {
        for byte in entry.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

static CATALOG_C: OnceLock<Vec<CaapCatalogEntry>> = OnceLock::new();
static CATALOG_STRINGS: OnceLock<Vec<(CString, CString)>> = OnceLock::new();

fn build_catalog_strings() -> Vec<(CString, CString)> {
    crate::catalog::export_catalog()
        .iter()
        .map(|e| (cstring_lossy_nul(e.library), cstring_lossy_nul(e.export)))
        .collect()
}

fn ensure_catalog() {
    // Build owned CString pairs first, then build the C entry table from
    // their pointers.  Both vecs must live for the process lifetime.
    CATALOG_STRINGS.get_or_init(build_catalog_strings);
    CATALOG_C.get_or_init(|| {
        let strings = CATALOG_STRINGS
            .get()
            .expect("CATALOG_STRINGS initialized above");
        strings
            .iter()
            .zip(crate::catalog::export_catalog().iter())
            .map(|((lib_cs, exp_cs), entry)| CaapCatalogEntry {
                library: lib_cs.as_ptr(),
                export: exp_cs.as_ptr(),
                min_arity: entry.min_arity as c_int,
                max_arity: entry.max_arity.map(|a| a as c_int).unwrap_or(-1),
            })
            .collect()
    });
}

#[no_mangle]
pub extern "C" fn caap_runtime_abi_descriptor() -> CaapRuntimeAbiDescriptor {
    caap_runtime_abi_descriptor_value()
}

#[no_mangle]
pub extern "C" fn caap_runtime_abi_version() -> u32 {
    CAAP_RUNTIME_ABI_VERSION
}

#[no_mangle]
pub extern "C" fn caap_runtime_catalog_count() -> usize {
    ensure_catalog();
    CATALOG_C.get().map(|v| v.len()).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn caap_runtime_catalog_entries() -> *const CaapCatalogEntry {
    ensure_catalog();
    CATALOG_C.get().map(|v| v.as_ptr()).unwrap_or(ptr::null())
}

// ── Invoke ABI ────────────────────────────────────────────────────────────────

/// Invoke a runtime function.
///
/// `args_json` — JSON array of argument values (null/bool/number/string/array/object)
/// `out_json`  — on success, *out_json is set to a malloc-ed JSON string; caller frees with caap_runtime_string_free
/// `error`     — on failure, *error is set to a malloc-ed error string; caller frees with caap_runtime_string_free
///
/// Returns 1 on success, 0 on error.
///
/// # Safety
///
/// `library`, `export`, and `args_json` must be valid null-terminated C strings when non-null.
/// `out_json` and `error` must be valid writable pointer slots when non-null. On success or
/// failure, any non-null string written into either slot is allocated by this runtime and must be
/// released exactly once with `caap_runtime_string_free`.
#[no_mangle]
pub unsafe extern "C" fn caap_runtime_invoke(
    library: *const c_char,
    export: *const c_char,
    args_json: *const c_char,
    out_json: *mut *mut c_char,
    error: *mut *mut c_char,
) -> c_int {
    if !out_json.is_null() {
        *out_json = ptr::null_mut();
    }
    if error.is_null() {
        return 0;
    }
    *error = ptr::null_mut();
    if out_json.is_null() {
        write_error(error, "out_json pointer is null");
        return 0;
    }

    if library.is_null() {
        write_error(error, "library name pointer is null");
        return 0;
    }
    if export.is_null() {
        write_error(error, "export name pointer is null");
        return 0;
    }
    if args_json.is_null() {
        write_error(error, "args_json pointer is null");
        return 0;
    }
    let lib = match CStr::from_ptr(library).to_str() {
        Ok(s) => s,
        Err(_) => {
            write_error(error, "library name is not valid UTF-8");
            return 0;
        }
    };
    let exp = match CStr::from_ptr(export).to_str() {
        Ok(s) => s,
        Err(_) => {
            write_error(error, "export name is not valid UTF-8");
            return 0;
        }
    };
    let json_str = match CStr::from_ptr(args_json).to_str() {
        Ok(s) => s,
        Err(_) => {
            write_error(error, "args_json is not valid UTF-8");
            return 0;
        }
    };

    let args_sv = match json_to_args(json_str) {
        Ok(v) => v,
        Err(e) => {
            write_error(error, &e);
            return 0;
        }
    };

    // Catch panics from dispatched code so they never unwind across the
    // `extern "C"` boundary (Rust now aborts on such unwinding; the catch turns
    // panics into a regular error path instead, preserving the C caller's
    // process and surfacing the panic message in `error`).
    let dispatch_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        RUNTIME_STATE
            .with(|state| crate::catalog::dispatch(&mut state.borrow_mut(), lib, exp, args_sv))
    }));
    match dispatch_result {
        Ok(Ok(value)) => {
            let json = match sys_value_to_json(&value) {
                Ok(json) => json,
                Err(e) => {
                    write_error(error, &e);
                    return 0;
                }
            };
            write_string(out_json, &json);
            1
        }
        Ok(Err(e)) => {
            write_error(error, &e);
            0
        }
        Err(payload) => {
            let msg = panic_payload_message(&*payload);
            write_error(error, &format!("runtime panic: {msg}"));
            0
        }
    }
}

/// Free a string returned by caap_runtime_invoke.
///
/// # Safety
///
/// `s` must be null or a pointer previously returned by this runtime through
/// `caap_runtime_invoke` and not already freed.
#[no_mangle]
pub unsafe extern "C" fn caap_runtime_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

// ── Policy ABI ────────────────────────────────────────────────────────────────

/// C callback for the opt-in policy gate. Receives `library`, `export`,
/// `capability`, and `effect` as null-terminated strings and returns 0 to allow
/// the operation or any non-zero value to deny it.
pub type CaapPolicyFn = extern "C" fn(
    library: *const c_char,
    export: *const c_char,
    capability: *const c_char,
    effect: *const c_char,
) -> c_int;

/// [`SysPolicy`] backed by a C function pointer, installed via
/// [`caap_runtime_set_policy`].
struct CAbiPolicy {
    callback: CaapPolicyFn,
}

impl SysPolicy for CAbiPolicy {
    fn check(&self, request: PolicyRequest<'_>) -> PolicyDecision {
        // `library`/`export` arrived through `CStr` (no interior NUL) and
        // `capability`/`effect` are 'static classifier strings, so these
        // conversions only fail on a malformed request — fail closed if so.
        let (Ok(library), Ok(export), Ok(capability), Ok(effect)) = (
            CString::new(request.library),
            CString::new(request.export),
            CString::new(request.capability),
            CString::new(request.effect),
        ) else {
            return PolicyDecision::Deny("policy request contained interior NUL".to_string());
        };
        // SAFETY: `callback` is a valid function pointer for the calling thread
        // per the contract of `caap_runtime_set_policy`; all four pointers are
        // valid null-terminated strings for the duration of the call.
        let verdict = (self.callback)(
            library.as_ptr(),
            export.as_ptr(),
            capability.as_ptr(),
            effect.as_ptr(),
        );
        if verdict == 0 {
            PolicyDecision::Allow
        } else {
            PolicyDecision::Deny("denied by host policy".to_string())
        }
    }
}

/// Install (or clear) the opt-in policy gate for C-ABI consumers on the current
/// thread.
///
/// Pass a non-null `callback` to gate every subsequent `caap_runtime_invoke` and
/// `caap_runtime_invoke_direct` call: the runtime invokes it with the operation's
/// `library`, `export`, and its `capability`/`effect` classification, and rejects
/// the operation before dispatch when the callback returns non-zero. Pass null to
/// clear the gate (the default — everything is allowed).
///
/// The gate is per-thread, matching the thread-local runtime state. This is an
/// *optional* export (not in [`CAAP_RUNTIME_REQUIRED_SYMBOLS`]); detect it with
/// `dlsym` before use.
///
/// # Safety
///
/// `callback`, when non-null, must remain a valid function pointer for as long as
/// it stays installed on this thread (until replaced or cleared).
//
// The parameter is written as an inline `Option<extern "C" fn(...)>` rather than
// `Option<CaapPolicyFn>` so cbindgen emits a plain nullable function pointer in
// the C header instead of an opaque `Option_CaapPolicyFn` struct.
#[no_mangle]
pub unsafe extern "C" fn caap_runtime_set_policy(
    callback: Option<
        extern "C" fn(
            library: *const c_char,
            export: *const c_char,
            capability: *const c_char,
            effect: *const c_char,
        ) -> c_int,
    >,
) {
    let policy: Option<Box<dyn SysPolicy>> =
        callback.map(|callback| Box::new(CAbiPolicy { callback }) as Box<dyn SysPolicy>);
    RUNTIME_STATE.with(|state| state.borrow_mut().set_policy(policy));
}

// ── JSON helpers ──────────────────────────────────────────────────────────────

use crate::ffi_value::{SysArgs, SysValue};

fn json_to_args(json: &str) -> Result<SysArgs, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Err("args_json must be a JSON array".into());
    }
    let value: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| format!("args_json JSON parse error: {e}"))?;
    let serde_json::Value::Array(items) = value else {
        return Err("args_json must be a JSON array".into());
    };
    items
        .into_iter()
        .map(json_value_to_sys)
        .collect::<Result<Vec<_>, _>>()
        .map(SysArgs)
}

fn json_value_to_sys(value: serde_json::Value) -> Result<SysValue, String> {
    match value {
        serde_json::Value::Null => Ok(SysValue::Null),
        serde_json::Value::Bool(value) => Ok(SysValue::Bool(value)),
        serde_json::Value::Number(value) => {
            if value.is_i64() {
                let value = value
                    .as_i64()
                    .ok_or_else(|| format!("JSON number is outside CAAP SYS range: {value}"))?;
                Ok(SysValue::Int(value))
            } else if value.is_f64() {
                let value = value
                    .as_f64()
                    .ok_or_else(|| format!("JSON number is outside CAAP SYS range: {value}"))?;
                Ok(SysValue::Float(value))
            } else {
                Err(format!("JSON number is outside CAAP SYS range: {value}"))
            }
        }
        serde_json::Value::String(value) => Ok(SysValue::Str(value)),
        serde_json::Value::Array(items) => items
            .into_iter()
            .map(json_value_to_sys)
            .collect::<Result<Vec<_>, _>>()
            .map(SysValue::List),
        serde_json::Value::Object(entries) => {
            // A single-key `{"$caap_b64": "..."}` object decodes back to Bytes.
            if entries.len() == 1 {
                if let Some(serde_json::Value::String(encoded)) = entries.get(BYTES_JSON_TAG) {
                    let bytes = base64_decode(encoded)
                        .ok_or_else(|| "invalid base64 in bytes value".to_string())?;
                    return Ok(SysValue::Bytes(bytes));
                }
            }
            entries
                .into_iter()
                .map(|(key, value)| json_value_to_sys(value).map(|value| (key, value)))
                .collect::<Result<std::collections::HashMap<_, _>, _>>()
                .map(SysValue::Map)
        }
    }
}

/// JSON object key that tags a base64-encoded [`SysValue::Bytes`] across the
/// JSON ABI (the direct ABI uses tag 7 instead). Shared with the caap-side
/// plugin-result decoder so both ends agree on the representation.
pub const BYTES_JSON_TAG: &str = "$caap_b64";

pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().filter(|&&c| c == b'=').count();
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            let v = if c == b'=' { 0 } else { val(c)? };
            n |= v << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

fn sys_value_to_json(v: &SysValue) -> Result<String, String> {
    serde_json::to_string(&sys_value_to_json_value(v)?)
        .map_err(|e| format!("failed to encode CAAP SYS JSON: {e}"))
}

fn sys_value_to_json_value(v: &SysValue) -> Result<serde_json::Value, String> {
    match v {
        SysValue::Null => Ok(serde_json::Value::Null),
        SysValue::Bool(value) => Ok(serde_json::Value::Bool(*value)),
        SysValue::Int(value) => Ok(serde_json::Value::Number((*value).into())),
        SysValue::Float(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| format!("cannot encode non-finite float: {value}")),
        SysValue::Str(value) => Ok(serde_json::Value::String(value.clone())),
        SysValue::Bytes(bytes) => {
            // JSON has no binary type; encode as a tagged base64 object so it
            // round-trips unambiguously (and never collides with a real map).
            let mut map = serde_json::Map::new();
            map.insert(
                BYTES_JSON_TAG.to_string(),
                serde_json::Value::String(base64_encode(bytes)),
            );
            Ok(serde_json::Value::Object(map))
        }
        SysValue::List(items) => items
            .iter()
            .map(sys_value_to_json_value)
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        SysValue::Map(entries) => {
            let mut map = serde_json::Map::new();
            for (key, value) in entries {
                map.insert(key.clone(), sys_value_to_json_value(value)?);
            }
            Ok(serde_json::Value::Object(map))
        }
    }
}

// ── C string allocation helpers ───────────────────────────────────────────────

unsafe fn write_string(out: *mut *mut c_char, s: &str) {
    if !out.is_null() {
        *out = cstring_lossy_nul(s).into_raw();
    }
}

unsafe fn write_error(out: *mut *mut c_char, msg: &str) {
    write_string(out, msg);
}

/// Best-effort extraction of a panic payload's message.  The standard library
/// boxes the panic argument; the two common types are `&'static str` (from
/// `panic!("literal")`) and `String` (from `panic!("{x}")`).  Falls back to a
/// generic label so the C caller always gets *some* diagnostic.
fn panic_payload_message(payload: &(dyn std::any::Any + Send + 'static)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "<unknown panic payload>".to_string()
}

pub(crate) fn cstring_lossy_nul(s: &str) -> CString {
    let mut bytes = Vec::with_capacity(s.len());
    for byte in s.bytes() {
        if byte == 0 {
            bytes.extend_from_slice(b"\\0");
        } else {
            bytes.push(byte);
        }
    }
    // SAFETY: every interior NUL byte is replaced with the two-byte sequence `\0`;
    // `from_vec_unchecked` appends the trailing C terminator.
    unsafe { CString::from_vec_unchecked(bytes) }
}

// ── Direct / Serialization-Free Invoke ABI ───────────────────────────────────

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CaapSysValue {
    pub tag: u32,
    pub int_val: i64,
    pub float_val: f64,
    pub ptr_val: *mut u8,
    pub len_val: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CaapSysMapEntry {
    pub key: *mut c_char,
    pub value: CaapSysValue,
}

/// Decode a direct-ABI value into the internal CAAP SYS value representation.
///
/// # Safety
///
/// `val` must be a valid direct-ABI value produced by CAAP SYS or by a plugin
/// that follows the CAAP runtime ABI. For string, list, and map tags, `ptr_val`
/// must point to `len_val` initialized elements of the corresponding ABI type
/// for the duration of this call. Map keys must be valid null-terminated C
/// strings.
pub unsafe fn to_sys_value(val: &CaapSysValue) -> Result<SysValue, String> {
    match val.tag {
        0 => Ok(SysValue::Null),
        1 => Ok(SysValue::Bool(val.int_val != 0)),
        2 => Ok(SysValue::Int(val.int_val)),
        3 => Ok(SysValue::Float(val.float_val)),
        4 => {
            if val.ptr_val.is_null() {
                return if val.len_val == 0 {
                    Ok(SysValue::Str(String::new()))
                } else {
                    Err("null pointer for non-empty string value".to_string())
                };
            }
            let bytes = std::slice::from_raw_parts(val.ptr_val, val.len_val);
            let s = std::str::from_utf8(bytes)
                .map_err(|e| format!("invalid UTF-8 string in CaapSysValue: {e}"))?;
            Ok(SysValue::Str(s.to_string()))
        }
        7 => {
            if val.ptr_val.is_null() {
                return if val.len_val == 0 {
                    Ok(SysValue::Bytes(Vec::new()))
                } else {
                    Err("null pointer for non-empty bytes value".to_string())
                };
            }
            // Raw bytes — no UTF-8 validation, unlike the string tag.
            let bytes = std::slice::from_raw_parts(val.ptr_val, val.len_val);
            Ok(SysValue::Bytes(bytes.to_vec()))
        }
        5 => {
            if val.ptr_val.is_null() && val.len_val > 0 {
                return Err("null pointer for non-empty list value".to_string());
            }
            let slice = if val.len_val == 0 {
                &[]
            } else {
                // SAFETY: the caller guarantees `ptr_val` points to a `[CaapSysValue; len_val]`
                // allocated by `from_sys_value`. The alignment requirement is enforced here so
                // that misaligned pointers from buggy callers produce a clear error instead of UB.
                let align = std::mem::align_of::<CaapSysValue>();
                if !(val.ptr_val as usize).is_multiple_of(align) {
                    return Err(format!(
                        "misaligned CaapSysValue list pointer: expected {align}-byte alignment"
                    ));
                }
                std::slice::from_raw_parts(val.ptr_val as *const CaapSysValue, val.len_val)
            };
            let mut list = Vec::with_capacity(val.len_val);
            for item in slice {
                list.push(to_sys_value(item)?);
            }
            Ok(SysValue::List(list))
        }
        6 => {
            if val.ptr_val.is_null() && val.len_val > 0 {
                return Err("null pointer for non-empty map value".to_string());
            }
            let slice = if val.len_val == 0 {
                &[]
            } else {
                // SAFETY: same contract as the list branch above.
                let align = std::mem::align_of::<CaapSysMapEntry>();
                if !(val.ptr_val as usize).is_multiple_of(align) {
                    return Err(format!(
                        "misaligned CaapSysMapEntry pointer: expected {align}-byte alignment"
                    ));
                }
                std::slice::from_raw_parts(val.ptr_val as *const CaapSysMapEntry, val.len_val)
            };
            let mut map = std::collections::HashMap::with_capacity(val.len_val);
            for entry in slice {
                if entry.key.is_null() {
                    return Err("null key pointer in map entry".to_string());
                }
                let key = CStr::from_ptr(entry.key)
                    .to_str()
                    .map_err(|e| format!("invalid UTF-8 key in map entry: {e}"))?
                    .to_string();
                map.insert(key, to_sys_value(&entry.value)?);
            }
            Ok(SysValue::Map(map))
        }
        other => Err(format!("unknown CaapSysValue tag: {other}")),
    }
}

/// Encode an internal CAAP SYS value as a direct-ABI value allocated for the C boundary.
///
/// The returned value must be released with `free_sys_value_contents` or
/// `caap_runtime_value_free` exactly once.
///
/// # Safety
///
/// The returned pointers are owned by the caller according to the CAAP runtime
/// ABI. The caller must not mutate nested pointers except to pass them back to
/// `free_sys_value_contents` or `caap_runtime_value_free`.
pub unsafe fn from_sys_value(val: &SysValue) -> Result<CaapSysValue, String> {
    match val {
        SysValue::Null => Ok(CaapSysValue {
            tag: 0,
            int_val: 0,
            float_val: 0.0,
            ptr_val: std::ptr::null_mut(),
            len_val: 0,
        }),
        SysValue::Bool(b) => Ok(CaapSysValue {
            tag: 1,
            int_val: if *b { 1 } else { 0 },
            float_val: 0.0,
            ptr_val: std::ptr::null_mut(),
            len_val: 0,
        }),
        SysValue::Int(n) => Ok(CaapSysValue {
            tag: 2,
            int_val: *n,
            float_val: 0.0,
            ptr_val: std::ptr::null_mut(),
            len_val: 0,
        }),
        SysValue::Float(f) => Ok(CaapSysValue {
            tag: 3,
            int_val: 0,
            float_val: *f,
            ptr_val: std::ptr::null_mut(),
            len_val: 0,
        }),
        SysValue::Str(s) => {
            let bytes = s.as_bytes();
            let len = bytes.len();
            let ptr = if len > 0 {
                let mut boxed = bytes.to_vec().into_boxed_slice();
                let ptr = boxed.as_mut_ptr();
                std::mem::forget(boxed);
                ptr
            } else {
                std::ptr::null_mut()
            };
            Ok(CaapSysValue {
                tag: 4,
                int_val: 0,
                float_val: 0.0,
                ptr_val: ptr,
                len_val: len,
            })
        }
        SysValue::Bytes(bytes) => {
            let len = bytes.len();
            let ptr = if len > 0 {
                let mut boxed = bytes.clone().into_boxed_slice();
                let ptr = boxed.as_mut_ptr();
                std::mem::forget(boxed);
                ptr
            } else {
                std::ptr::null_mut()
            };
            Ok(CaapSysValue {
                tag: 7,
                int_val: 0,
                float_val: 0.0,
                ptr_val: ptr,
                len_val: len,
            })
        }
        SysValue::List(items) => {
            let len = items.len();
            let ptr = if len > 0 {
                let mut raw_items = Vec::with_capacity(len);
                for item in items {
                    match from_sys_value(item) {
                        Ok(raw) => raw_items.push(raw),
                        Err(error) => {
                            for raw in &mut raw_items {
                                free_sys_value_contents(raw);
                            }
                            return Err(error);
                        }
                    }
                }
                let boxed = raw_items.into_boxed_slice();
                Box::into_raw(boxed) as *mut u8
            } else {
                std::ptr::null_mut()
            };
            Ok(CaapSysValue {
                tag: 5,
                int_val: 0,
                float_val: 0.0,
                ptr_val: ptr,
                len_val: len,
            })
        }
        SysValue::Map(entries) => {
            let len = entries.len();
            let ptr = if len > 0 {
                let mut raw_entries = Vec::with_capacity(len);
                for (k, v) in entries {
                    let key_cstr = CString::new(k.as_str())
                        .map_err(|_| format!("map key contains interior NUL byte: {k:?}"))?;
                    let key_ptr = key_cstr.into_raw();
                    match from_sys_value(v) {
                        Ok(value) => raw_entries.push(CaapSysMapEntry {
                            key: key_ptr,
                            value,
                        }),
                        Err(error) => {
                            drop(CString::from_raw(key_ptr));
                            for entry in &mut raw_entries {
                                if !entry.key.is_null() {
                                    drop(CString::from_raw(entry.key));
                                    entry.key = std::ptr::null_mut();
                                }
                                free_sys_value_contents(&mut entry.value);
                            }
                            return Err(error);
                        }
                    }
                }
                let boxed = raw_entries.into_boxed_slice();
                Box::into_raw(boxed) as *mut u8
            } else {
                std::ptr::null_mut()
            };
            Ok(CaapSysValue {
                tag: 6,
                int_val: 0,
                float_val: 0.0,
                ptr_val: ptr,
                len_val: len,
            })
        }
    }
}

/// Free nested allocations held by a direct-ABI return value.
///
/// # Safety
///
/// `val` must be null or a pointer to a `CaapSysValue` whose nested pointers
/// were allocated by this runtime's direct ABI and have not already been freed.
#[no_mangle]
pub unsafe extern "C" fn caap_runtime_value_free(val: *mut CaapSysValue) {
    if !val.is_null() {
        free_sys_value_contents(&mut *val);
    }
}

/// Free nested allocations held by a direct-ABI value in place.
///
/// # Safety
///
/// `val` must be a CAAP direct-ABI value whose nested pointers, if present,
/// were allocated by `from_sys_value` and have not already been freed. The
/// outer `CaapSysValue` storage remains owned by the caller.
pub unsafe fn free_sys_value_contents(val: &mut CaapSysValue) {
    match val.tag {
        // String (4) and Bytes (7) are both a forgotten `Box<[u8]>`.
        4 | 7 if !val.ptr_val.is_null() => {
            // Bytes were produced by `Vec<u8>::into_boxed_slice()` in
            // `from_sys_value()`.  Reconstruct the Box<[u8]> and let its Drop
            // free the buffer through the same allocator used to allocate it.
            // (Previously this called `std::alloc::dealloc` directly with a
            // freshly-built Layout — correct on the default global allocator
            // but inconsistent with the List/Map paths and brittle under a
            // custom #[global_allocator] that adds layout side-tables.)
            let slice = std::slice::from_raw_parts_mut(val.ptr_val, val.len_val);
            let boxed = Box::from_raw(slice);
            drop(boxed);
            val.ptr_val = std::ptr::null_mut();
            val.len_val = 0;
        }
        5 if !val.ptr_val.is_null() => {
            let slice =
                std::slice::from_raw_parts_mut(val.ptr_val as *mut CaapSysValue, val.len_val);
            for item in slice.iter_mut() {
                free_sys_value_contents(item);
            }
            let boxed = Box::from_raw(slice);
            drop(boxed);
            val.ptr_val = std::ptr::null_mut();
            val.len_val = 0;
        }
        6 if !val.ptr_val.is_null() => {
            let slice =
                std::slice::from_raw_parts_mut(val.ptr_val as *mut CaapSysMapEntry, val.len_val);
            for entry in slice.iter_mut() {
                if !entry.key.is_null() {
                    drop(CString::from_raw(entry.key));
                    entry.key = std::ptr::null_mut();
                }
                free_sys_value_contents(&mut entry.value);
            }
            let boxed = Box::from_raw(slice);
            drop(boxed);
            val.ptr_val = std::ptr::null_mut();
            val.len_val = 0;
        }
        _ => {}
    }
}

/// Invoke a runtime function through the serialization-free direct ABI.
///
/// # Safety
///
/// `library` and `export` must be valid null-terminated C strings. `args` must
/// be null only when `args_count == 0`; otherwise it must point to `args_count`
/// initialized `CaapSysValue` elements valid for this call. `out_val` and
/// `error` must be valid writable pointer slots. On success, `out_val` owns
/// nested allocations that the caller must release with `caap_runtime_value_free`.
/// On failure, `*error` is allocated by this runtime and must be released with
/// `caap_runtime_string_free`.
#[no_mangle]
pub unsafe extern "C" fn caap_runtime_invoke_direct(
    library: *const c_char,
    export: *const c_char,
    args: *const CaapSysValue,
    args_count: usize,
    out_val: *mut CaapSysValue,
    error: *mut *mut c_char,
) -> c_int {
    if !out_val.is_null() {
        *out_val = CaapSysValue {
            tag: 0,
            int_val: 0,
            float_val: 0.0,
            ptr_val: std::ptr::null_mut(),
            len_val: 0,
        };
    }
    if error.is_null() {
        return 0;
    }
    *error = std::ptr::null_mut();

    if out_val.is_null() {
        write_error(error, "out_val pointer is null");
        return 0;
    }
    if library.is_null() {
        write_error(error, "library name pointer is null");
        return 0;
    }
    if export.is_null() {
        write_error(error, "export name pointer is null");
        return 0;
    }
    if args.is_null() && args_count > 0 {
        write_error(error, "args pointer is null for non-empty arguments");
        return 0;
    }

    let lib = match CStr::from_ptr(library).to_str() {
        Ok(s) => s,
        Err(_) => {
            write_error(error, "library name is not valid UTF-8");
            return 0;
        }
    };
    let exp = match CStr::from_ptr(export).to_str() {
        Ok(s) => s,
        Err(_) => {
            write_error(error, "export name is not valid UTF-8");
            return 0;
        }
    };

    let slice = if args_count == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(args, args_count)
    };
    let mut sys_args = Vec::with_capacity(args_count);
    for item in slice {
        match to_sys_value(item) {
            Ok(v) => sys_args.push(v),
            Err(e) => {
                write_error(error, &e);
                return 0;
            }
        }
    }

    // Catch panics from dispatched code so they never unwind across the
    // `extern "C"` boundary.  See caap_runtime_invoke for rationale.
    let dispatch_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        RUNTIME_STATE.with(|state| {
            crate::catalog::dispatch(&mut state.borrow_mut(), lib, exp, SysArgs(sys_args))
        })
    }));
    match dispatch_result {
        Ok(Ok(value)) => match from_sys_value(&value) {
            Ok(value) => {
                *out_val = value;
                1
            }
            Err(error_message) => {
                write_error(error, &error_message);
                0
            }
        },
        Ok(Err(e)) => {
            write_error(error, &e);
            0
        }
        Err(payload) => {
            let msg = panic_payload_message(&*payload);
            write_error(error, &format!("runtime panic: {msg}"));
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_payload_message_decodes_static_str_and_string() {
        let static_payload: Box<dyn std::any::Any + Send + 'static> = Box::new("boom");
        assert_eq!(panic_payload_message(&*static_payload), "boom");

        let owned_payload: Box<dyn std::any::Any + Send + 'static> = Box::new("oops".to_string());
        assert_eq!(panic_payload_message(&*owned_payload), "oops");

        let other_payload: Box<dyn std::any::Any + Send + 'static> = Box::new(42_u32);
        assert_eq!(
            panic_payload_message(&*other_payload),
            "<unknown panic payload>"
        );
    }

    #[test]
    fn runtime_abi_version_is_explicit() {
        assert_eq!(caap_runtime_abi_version(), CAAP_RUNTIME_ABI_VERSION);
        assert_eq!(CAAP_RUNTIME_ABI_VERSION, 1);
    }

    #[test]
    fn runtime_abi_descriptor_is_explicit() {
        let descriptor = caap_runtime_abi_descriptor();

        assert_eq!(descriptor.abi_version, CAAP_RUNTIME_ABI_VERSION);
        assert_eq!(
            descriptor.value_encoding_version,
            CAAP_RUNTIME_VALUE_ENCODING_VERSION
        );
        assert_eq!(
            descriptor.catalog_schema_version,
            CAAP_RUNTIME_CATALOG_SCHEMA_VERSION
        );
        assert_eq!(
            descriptor.required_symbol_count,
            CAAP_RUNTIME_REQUIRED_SYMBOLS.len() as u32
        );
        assert_eq!(
            descriptor.required_symbols_hash,
            caap_runtime_required_symbols_hash()
        );
        assert_eq!(
            descriptor.capability_effect_catalog_hash,
            caap_runtime_capability_effect_catalog_hash()
        );
    }

    #[test]
    fn json_boundary_preserves_float_values() {
        assert_eq!(json_to_args("[3.5]").unwrap().0, vec![SysValue::Float(3.5)]);
        assert_eq!(sys_value_to_json(&SysValue::Float(3.5)).unwrap(), "3.5");
    }

    #[test]
    fn json_args_reject_empty_or_non_array_input() {
        assert_error_contains(json_to_args(""), "JSON array");
        assert_error_contains(json_to_args("{}"), "JSON array");
    }

    #[test]
    fn json_args_reject_malformed_json() {
        assert_error_contains(json_to_args("[1,]"), "JSON parse error");
    }

    #[test]
    fn json_args_reject_unsigned_values_outside_caap_sys_int_range() {
        assert_error_contains(
            json_to_args("[9223372036854775808]"),
            "outside CAAP SYS range",
        );
    }

    #[test]
    fn invoke_rejects_null_pointers_before_cstr_decode() {
        // SAFETY: intentionally passing null library pointer to test null-rejection logic.
        unsafe {
            let export = CString::new("unix_millis").unwrap();
            let args = CString::new("[]").unwrap();
            let mut out_json: *mut c_char = ptr::null_mut();
            let mut error: *mut c_char = ptr::null_mut();
            let result = caap_runtime_invoke(
                ptr::null(),
                export.as_ptr(),
                args.as_ptr(),
                &mut out_json,
                &mut error,
            );
            assert_eq!(result, 0);
            assert!(out_json.is_null());
            assert!(!error.is_null());
            let message = CStr::from_ptr(error).to_str().unwrap().to_string();
            caap_runtime_string_free(error);
            assert!(message.contains("library name pointer is null"));
        }
    }

    #[test]
    fn invoke_rejects_null_output_slot_before_dispatch() {
        // SAFETY: intentionally passing null out-slot pointer to test null-rejection logic.
        unsafe {
            let library = CString::new("time").unwrap();
            let export = CString::new("unix_millis").unwrap();
            let args = CString::new("[]").unwrap();
            let mut error: *mut c_char = ptr::null_mut();

            let result = caap_runtime_invoke(
                library.as_ptr(),
                export.as_ptr(),
                args.as_ptr(),
                ptr::null_mut(),
                &mut error,
            );

            assert_eq!(result, 0);
            assert!(!error.is_null());
            let message = CStr::from_ptr(error).to_str().unwrap().to_string();
            caap_runtime_string_free(error);
            assert!(message.contains("out_json pointer is null"));
        }
    }

    #[test]
    fn invoke_rejects_null_error_slot_and_clears_output() {
        // SAFETY: intentionally passing null error-slot pointer to test null-rejection logic.
        unsafe {
            let library = CString::new("time").unwrap();
            let export = CString::new("unix_millis").unwrap();
            let args = CString::new("[]").unwrap();
            let stale_out = CString::new("stale output").unwrap().into_raw();
            let mut out_json = stale_out;

            let result = caap_runtime_invoke(
                library.as_ptr(),
                export.as_ptr(),
                args.as_ptr(),
                &mut out_json,
                ptr::null_mut(),
            );

            assert_eq!(result, 0);
            assert!(out_json.is_null());
            caap_runtime_string_free(stale_out);
        }
    }

    #[test]
    fn invoke_clears_stale_output_pointer_on_error() {
        // SAFETY: all pointers are valid CString raw pointers; stale_out is freed explicitly.
        unsafe {
            let library = CString::new("time").unwrap();
            let export = CString::new("unix_millis").unwrap();
            let args = CString::new("{}").unwrap();
            let stale_out = CString::new("stale output").unwrap().into_raw();
            let mut out_json = stale_out;
            let mut error: *mut c_char = ptr::null_mut();

            let result = caap_runtime_invoke(
                library.as_ptr(),
                export.as_ptr(),
                args.as_ptr(),
                &mut out_json,
                &mut error,
            );

            assert_eq!(result, 0);
            assert!(out_json.is_null());
            assert!(!error.is_null());
            let message = CStr::from_ptr(error).to_str().unwrap().to_string();
            caap_runtime_string_free(stale_out);
            caap_runtime_string_free(error);
            assert!(message.contains("args_json must be a JSON array"));
        }
    }

    #[test]
    fn invoke_clears_stale_error_pointer_on_success() {
        // SAFETY: all pointers are valid CString raw pointers; stale_error is freed explicitly.
        unsafe {
            let library = CString::new("time").unwrap();
            let export = CString::new("unix_millis").unwrap();
            let args = CString::new("[]").unwrap();
            let mut out_json: *mut c_char = ptr::null_mut();
            let stale_error = CString::new("stale error").unwrap().into_raw();
            let mut error = stale_error;

            let result = caap_runtime_invoke(
                library.as_ptr(),
                export.as_ptr(),
                args.as_ptr(),
                &mut out_json,
                &mut error,
            );

            assert_eq!(result, 1);
            assert!(!out_json.is_null());
            assert!(error.is_null());
            let payload = CStr::from_ptr(out_json).to_str().unwrap().to_string();
            caap_runtime_string_free(out_json);
            caap_runtime_string_free(stale_error);
            assert!(payload.parse::<i64>().is_ok(), "got {payload:?}");
        }
    }

    #[test]
    fn write_string_preserves_error_context_when_message_contains_nul() {
        // SAFETY: `out` is initialized to null and will be written to by `write_string`; freed
        // explicitly via `caap_runtime_string_free`.
        unsafe {
            let mut out: *mut c_char = ptr::null_mut();
            write_string(&mut out, "bad\0message");
            assert!(!out.is_null());
            let message = CStr::from_ptr(out).to_str().unwrap().to_string();
            caap_runtime_string_free(out);
            assert_eq!(message, "bad\\0message");
        }
    }

    #[test]
    fn invoke_direct_handles_string_args_and_return_value() {
        unsafe {
            let library = CString::new("path").unwrap();
            let export = CString::new("join").unwrap();

            let mut arg1 = from_sys_value(&SysValue::Str("a".to_string())).unwrap();
            let mut arg2 = from_sys_value(&SysValue::Str("b".to_string())).unwrap();
            let args = [arg1, arg2];

            let mut out_val = CaapSysValue {
                tag: 0,
                int_val: 0,
                float_val: 0.0,
                ptr_val: std::ptr::null_mut(),
                len_val: 0,
            };
            let mut error: *mut c_char = std::ptr::null_mut();

            let ok = caap_runtime_invoke_direct(
                library.as_ptr(),
                export.as_ptr(),
                args.as_ptr(),
                args.len(),
                &mut out_val,
                &mut error,
            );

            assert_eq!(ok, 1);
            assert!(error.is_null());

            let sys_res = to_sys_value(&out_val).unwrap();
            assert_eq!(sys_res, SysValue::Str("a/b".to_string()));

            caap_runtime_value_free(&mut out_val);
            free_sys_value_contents(&mut arg1);
            free_sys_value_contents(&mut arg2);
        }
    }

    #[test]
    fn direct_abi_roundtrips_empty_string() {
        unsafe {
            let mut value = from_sys_value(&SysValue::Str(String::new())).unwrap();

            assert_eq!(value.tag, 4);
            assert!(value.ptr_val.is_null());
            assert_eq!(value.len_val, 0);
            assert_eq!(to_sys_value(&value).unwrap(), SysValue::Str(String::new()));

            free_sys_value_contents(&mut value);
        }
    }

    #[test]
    fn direct_abi_decodes_empty_collections_from_null_pointer() {
        unsafe {
            let list = CaapSysValue {
                tag: 5,
                int_val: 0,
                float_val: 0.0,
                ptr_val: std::ptr::null_mut(),
                len_val: 0,
            };
            let map = CaapSysValue {
                tag: 6,
                int_val: 0,
                float_val: 0.0,
                ptr_val: std::ptr::null_mut(),
                len_val: 0,
            };

            assert_eq!(to_sys_value(&list).unwrap(), SysValue::List(Vec::new()));
            assert_eq!(
                to_sys_value(&map).unwrap(),
                SysValue::Map(std::collections::HashMap::new())
            );
        }
    }

    #[test]
    fn invoke_direct_accepts_null_empty_args_pointer() {
        unsafe {
            let library = CString::new("time").unwrap();
            let export = CString::new("unix_millis").unwrap();
            let mut out_val = CaapSysValue {
                tag: 0,
                int_val: 0,
                float_val: 0.0,
                ptr_val: std::ptr::null_mut(),
                len_val: 0,
            };
            let mut error: *mut c_char = std::ptr::null_mut();

            let ok = caap_runtime_invoke_direct(
                library.as_ptr(),
                export.as_ptr(),
                std::ptr::null(),
                0,
                &mut out_val,
                &mut error,
            );

            assert_eq!(ok, 1);
            assert!(error.is_null());
            assert!(matches!(to_sys_value(&out_val).unwrap(), SysValue::Int(_)));
            caap_runtime_value_free(&mut out_val);
        }
    }

    #[test]
    fn base64_round_trips_arbitrary_bytes() {
        for case in [
            &b""[..],
            &b"f"[..],
            &b"fo"[..],
            &b"foo"[..],
            &b"foob"[..],
            &[0u8, 255, 1, 254, 127, 128][..],
        ] {
            let encoded = base64_encode(case);
            assert_eq!(
                base64_decode(&encoded).as_deref(),
                Some(case),
                "case {case:?}"
            );
        }
    }

    #[test]
    fn json_boundary_round_trips_bytes_via_base64_tag() {
        let json = sys_value_to_json(&SysValue::Bytes(vec![0, 1, 2, 255])).unwrap();
        assert!(json.contains("$caap_b64"), "got {json}");
        let decoded = json_to_args(&format!("[{json}]")).unwrap();
        assert_eq!(decoded.0, vec![SysValue::Bytes(vec![0, 1, 2, 255])]);
    }

    #[test]
    fn direct_abi_round_trips_bytes_tag_7() {
        unsafe {
            let mut value = from_sys_value(&SysValue::Bytes(vec![10, 0, 200])).unwrap();
            assert_eq!(value.tag, 7);
            assert_eq!(
                to_sys_value(&value).unwrap(),
                SysValue::Bytes(vec![10, 0, 200])
            );
            free_sys_value_contents(&mut value);

            // Empty bytes use a null pointer like empty strings.
            let mut empty = from_sys_value(&SysValue::Bytes(Vec::new())).unwrap();
            assert!(empty.ptr_val.is_null());
            assert_eq!(to_sys_value(&empty).unwrap(), SysValue::Bytes(Vec::new()));
            free_sys_value_contents(&mut empty);
        }
    }

    /// Test policy callback: deny any operation classified with a `write` effect.
    extern "C" fn deny_write_effect_policy(
        _library: *const c_char,
        _export: *const c_char,
        _capability: *const c_char,
        effect: *const c_char,
    ) -> c_int {
        // SAFETY: `effect` is a valid null-terminated classifier string supplied
        // by the runtime.
        let effect = unsafe { CStr::from_ptr(effect) }.to_str().unwrap_or("");
        if effect == "write" {
            1
        } else {
            0
        }
    }

    #[test]
    fn set_policy_gates_c_abi_invoke_and_can_be_cleared() {
        // SAFETY: all pointers are valid for the calls below; the policy is cleared
        // before any assertion so it never leaks into another test on this thread.
        unsafe {
            let library = CString::new("fs").unwrap();
            let export = CString::new("remove_file").unwrap();
            // A path that does not exist: with the policy active the call is
            // rejected before dispatch, so the filesystem is never touched.
            let mut path_arg =
                from_sys_value(&SysValue::Str("/caap-policy-test-nonexistent".to_string()))
                    .unwrap();
            let args = [path_arg];

            let null_val = || CaapSysValue {
                tag: 0,
                int_val: 0,
                float_val: 0.0,
                ptr_val: std::ptr::null_mut(),
                len_val: 0,
            };
            let take_error = |error: *mut c_char| -> String {
                if error.is_null() {
                    return String::new();
                }
                let message = CStr::from_ptr(error).to_str().unwrap().to_string();
                caap_runtime_string_free(error);
                message
            };

            // With the deny-writes policy installed, fs.remove_file (effect write)
            // is rejected before dispatch.
            caap_runtime_set_policy(Some(deny_write_effect_policy));
            let mut out_denied = null_val();
            let mut err_denied: *mut c_char = std::ptr::null_mut();
            let rc_denied = caap_runtime_invoke_direct(
                library.as_ptr(),
                export.as_ptr(),
                args.as_ptr(),
                args.len(),
                &mut out_denied,
                &mut err_denied,
            );
            let msg_denied = take_error(err_denied);

            // After clearing the policy, the same call reaches dispatch (and fails
            // at the OS layer instead — proving the gate, not the OS, blocked it).
            caap_runtime_set_policy(None);
            let mut out_cleared = null_val();
            let mut err_cleared: *mut c_char = std::ptr::null_mut();
            let rc_cleared = caap_runtime_invoke_direct(
                library.as_ptr(),
                export.as_ptr(),
                args.as_ptr(),
                args.len(),
                &mut out_cleared,
                &mut err_cleared,
            );
            let msg_cleared = take_error(err_cleared);

            free_sys_value_contents(&mut path_arg);

            assert_eq!(rc_denied, 0);
            assert!(
                msg_denied.contains("fs.remove_file: denied by policy"),
                "got {msg_denied:?}"
            );

            assert_eq!(rc_cleared, 0);
            assert!(
                !msg_cleared.contains("denied by policy"),
                "expected an OS-layer error after clearing the policy, got {msg_cleared:?}"
            );
        }
    }

    fn assert_error_contains(result: Result<SysArgs, String>, expected: &str) {
        match result {
            Ok(_) => panic!("expected error containing {expected:?}"),
            Err(error) => assert!(
                error.contains(expected),
                "expected error containing {expected:?}, got {error:?}"
            ),
        }
    }
}
