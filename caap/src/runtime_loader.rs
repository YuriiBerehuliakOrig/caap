//! Bridge between caap-core and caap-sys-runtime.
//!
//! Provides:
//! - `RuntimeValue` ↔ `SysValue` conversion
//! - `load_plugin_directory` — dlopen-based loader for third-party runtime plugins

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use caap_sys_runtime::ffi::{
    caap_runtime_capability_effect_catalog_hash, caap_runtime_required_symbols_hash,
    free_sys_value_contents, from_sys_value, to_sys_value, CaapRuntimeAbiDescriptor, CaapSysValue,
    CAAP_RUNTIME_ABI_VERSION, CAAP_RUNTIME_CATALOG_SCHEMA_VERSION, CAAP_RUNTIME_REQUIRED_SYMBOLS,
    CAAP_RUNTIME_VALUE_ENCODING_VERSION,
};
use caap_sys_runtime::ffi_value::SysValue;

use crate::host::{
    HostExportMetadata, HostExportParameter, HostExportSignature, HostServiceRegistry,
};
use crate::semantic::PhasePolicy;
use crate::values::{
    eval_err, ordered_runtime_map_entries, EvalSignal, HostFunction, MapKey, RuntimeValue,
};
use crate::CaapResult;

type StringFreeFn = unsafe extern "C" fn(*mut c_char);
type InvokeDirectFn = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *const CaapSysValue,
    usize,
    *mut CaapSysValue,
    *mut *mut c_char,
) -> c_int;
type ValueFreeFn = unsafe extern "C" fn(*mut CaapSysValue);
const MAX_PLUGIN_CATALOG_ENTRIES: usize = 4096;

// ── Value conversions ─────────────────────────────────────────────────────────

pub fn runtime_to_sys(v: &RuntimeValue) -> Result<SysValue, EvalSignal> {
    Ok(match v {
        RuntimeValue::Null => SysValue::Null,
        RuntimeValue::Bool(b) => SysValue::Bool(*b),
        RuntimeValue::Int(n) => SysValue::Int(*n),
        RuntimeValue::Float(f) => SysValue::Float(*f),
        RuntimeValue::Str(s) => SysValue::Str(s.to_string()),
        RuntimeValue::Bytes(b) => SysValue::Bytes(b.to_vec()),
        RuntimeValue::List(list) => {
            let items = list
                .borrow()
                .iter()
                .map(runtime_to_sys)
                .collect::<Result<Vec<_>, _>>()?;
            SysValue::List(items)
        }
        RuntimeValue::Map(map) => {
            let borrow = map.borrow();
            let pairs = ordered_runtime_map_entries(&borrow)
                .into_iter()
                .map(|(k, v)| match k {
                    MapKey::Str(s) => Ok((s.to_string(), runtime_to_sys(v)?)),
                    _ => Err(eval_err(format!(
                        "CAAP SYS boundary only supports string map keys, got {k}"
                    ))),
                })
                .collect::<Result<_, _>>()?;
            SysValue::Map(pairs)
        }
        RuntimeValue::Tuple(items) => {
            let converted = items
                .iter()
                .map(runtime_to_sys)
                .collect::<Result<Vec<_>, _>>()?;
            SysValue::List(converted)
        }
        RuntimeValue::Closure(_)
        | RuntimeValue::Macro(_)
        | RuntimeValue::Builtin(_)
        | RuntimeValue::HostFunction(_)
        | RuntimeValue::HostObject(_)
        | RuntimeValue::Ref(_)
        | RuntimeValue::UninitializedTopLevel => {
            return Err(eval_err(format!(
                "CAAP SYS boundary cannot serialize {} values",
                runtime_value_kind(v)
            )));
        }
    })
}

fn runtime_value_kind(value: &RuntimeValue) -> &'static str {
    match value {
        RuntimeValue::Null => "null",
        RuntimeValue::Bool(_) => "bool",
        RuntimeValue::Int(_) => "int",
        RuntimeValue::Float(_) => "float",
        RuntimeValue::Str(_) => "string",
        RuntimeValue::Bytes(_) => "bytes",
        RuntimeValue::Tuple(_) => "tuple",
        RuntimeValue::Closure(_) => "closure",
        RuntimeValue::Macro(_) => "macro",
        RuntimeValue::Builtin(_) => "builtin",
        RuntimeValue::HostFunction(_) => "host_function",
        RuntimeValue::HostObject(_) => "host_object",
        RuntimeValue::List(_) => "list",
        RuntimeValue::Map(_) => "map",
        RuntimeValue::Ref(_) => "ref",
        RuntimeValue::UninitializedTopLevel => "uninitialized_top_level",
    }
}

pub fn sys_to_runtime(v: SysValue) -> RuntimeValue {
    match v {
        SysValue::Null => RuntimeValue::Null,
        SysValue::Bool(b) => RuntimeValue::Bool(b),
        SysValue::Int(n) => RuntimeValue::Int(n),
        SysValue::Float(n) => RuntimeValue::Float(n),
        SysValue::Str(s) => RuntimeValue::Str(s.into()),
        SysValue::Bytes(b) => RuntimeValue::Bytes(b.into()),
        SysValue::List(items) => RuntimeValue::List(Rc::new(RefCell::new(
            items.into_iter().map(sys_to_runtime).collect(),
        ))),
        SysValue::Map(m) => RuntimeValue::Map(Rc::new(RefCell::new(
            m.into_iter()
                .map(|(k, v)| (MapKey::Str(k.into()), sys_to_runtime(v)))
                .collect(),
        ))),
    }
}

// ── dlopen plugin loader ──────────────────────────────────────────────────────

/// Load all `*.so` / `*.dylib` files from `dir` as runtime plugins.
///
/// Each plugin must export the Rust FFI ABI from caap-sys-runtime/src/ffi.rs:
///   `caap_runtime_abi_descriptor() → CaapRuntimeAbiDescriptor`
///   `caap_runtime_abi_version() → u32`
///   `caap_runtime_catalog_count() → usize`
///   `caap_runtime_catalog_entries() → *const CaapCatalogEntry`
///   `caap_runtime_invoke(library, export, args_json, out_json, error) → int`
///   `caap_runtime_string_free(s)`
///
/// Returns the number of exports registered. Because this is an explicit
/// integration boundary, missing directories and malformed plugin libraries are
/// reported instead of being silently skipped.
pub fn load_plugin_directory(registry: &mut HostServiceRegistry, dir: &Path) -> CaapResult<usize> {
    if !dir.is_dir() {
        return Err(crate::error::CaapError::host(format!(
            "runtime plugin dir {} does not exist or is not a directory",
            dir.display()
        )));
    }

    let mut total = 0usize;
    for path in runtime_plugin_candidates(dir)? {
        total += load_plugin(registry, &path)?;
    }

    Ok(total)
}

fn runtime_plugin_candidates(dir: &Path) -> CaapResult<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        crate::error::CaapError::host(format!("runtime plugin dir {}: {e}", dir.display()))
    })?;
    let mut candidates = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|e| {
            crate::error::CaapError::host(format!(
                "runtime plugin dir {} entry read failed: {e}",
                dir.display()
            ))
        })?;
        let path = entry.path();
        if !matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("so" | "dylib")
        ) {
            continue;
        }
        candidates.push(path);
    }
    candidates.sort_by(|left, right| left.as_os_str().cmp(right.as_os_str()));
    Ok(candidates)
}

fn load_plugin(registry: &mut HostServiceRegistry, path: &Path) -> CaapResult<usize> {
    use libloading::{Library, Symbol};
    use std::ffi::CString;

    // Safety: only the ABI descriptor is called before descriptor validation succeeds.
    // The library is kept alive for the process lifetime via Box::leak.
    let lib = unsafe { Library::new(path) }
        .map_err(|e| crate::error::CaapError::host(format!("dlopen {}: {e}", path.display())))?;

    type AbiDescriptorFn = unsafe extern "C" fn() -> CaapRuntimeAbiDescriptor;
    type AbiVersionFn = unsafe extern "C" fn() -> u32;
    type CatalogCountFn = unsafe extern "C" fn() -> usize;
    type CatalogEntriesFn = unsafe extern "C" fn() -> *const PluginCatalogEntry;
    type InvokeFn = unsafe extern "C" fn(
        *const c_char,
        *const c_char,
        *const c_char,
        *mut *mut c_char,
        *mut *mut c_char,
    ) -> c_int;
    #[repr(C)]
    struct PluginCatalogEntry {
        library: *const c_char,
        export: *const c_char,
        min_arity: c_int,
        max_arity: c_int,
    }

    // SAFETY: symbol name matches the descriptor function type. This is the only plugin function
    // called before the loader validates ABI/value/catalog/effect metadata.
    let descriptor_fn: Symbol<AbiDescriptorFn> =
        unsafe { lib.get(b"caap_runtime_abi_descriptor\0") }
            .map_err(|e| plugin_required_symbol_error(path, "caap_runtime_abi_descriptor", e))?;
    // SAFETY: `descriptor_fn` is a valid no-argument C function pointer just obtained from the
    // loaded library. The returned repr(C) descriptor is validated before any other plugin call.
    let descriptor = unsafe { descriptor_fn() };
    validate_plugin_abi_descriptor(path, descriptor)?;

    // SAFETY: symbol name matches the `unsafe extern "C" fn() -> u32` type alias; descriptor
    // validation has already established the ABI contract for this plugin.
    let abi_version_fn: Symbol<AbiVersionFn> = unsafe { lib.get(b"caap_runtime_abi_version\0") }
        .map_err(|e| plugin_required_symbol_error(path, "caap_runtime_abi_version", e))?;
    // SAFETY: `abi_version_fn` is a valid function pointer just obtained from `lib.get`; the
    // function takes no arguments and returns a plain u32.
    let abi_version = unsafe { abi_version_fn() };
    if abi_version != descriptor.abi_version {
        return Err(crate::error::CaapError::host(format!(
            "runtime plugin {} ABI version symbol does not match descriptor: descriptor {}, symbol {}",
            path.display(),
            descriptor.abi_version,
            abi_version
        )));
    }

    // SAFETY: symbol name matches the `unsafe extern "C" fn() -> usize` type alias.
    let count_fn: Symbol<CatalogCountFn> = unsafe { lib.get(b"caap_runtime_catalog_count\0") }
        .map_err(|e| plugin_required_symbol_error(path, "caap_runtime_catalog_count", e))?;
    // SAFETY: symbol name matches the `unsafe extern "C" fn() -> *const PluginCatalogEntry` alias.
    let entries_fn: Symbol<CatalogEntriesFn> =
        unsafe { lib.get(b"caap_runtime_catalog_entries\0") }
            .map_err(|e| plugin_required_symbol_error(path, "caap_runtime_catalog_entries", e))?;
    let invoke_fn: InvokeFn = {
        // SAFETY: symbol name matches the `InvokeFn` type alias for the C dispatch function.
        let symbol: Symbol<InvokeFn> = unsafe { lib.get(b"caap_runtime_invoke\0") }
            .map_err(|e| plugin_required_symbol_error(path, "caap_runtime_invoke", e))?;
        *symbol
    };
    let free_fn: StringFreeFn = {
        // SAFETY: symbol name matches the `StringFreeFn` alias; we deref to copy the fn pointer
        // before the Symbol lifetime ends.
        let symbol: Symbol<StringFreeFn> = unsafe { lib.get(b"caap_runtime_string_free\0") }
            .map_err(|e| plugin_required_symbol_error(path, "caap_runtime_string_free", e))?;
        *symbol
    };

    let invoke_direct_fn: Option<InvokeDirectFn> =
        unsafe { lib.get(b"caap_runtime_invoke_direct\0") }
            .map(|symbol: Symbol<InvokeDirectFn>| *symbol)
            .ok();
    let value_free_fn: Option<ValueFreeFn> = unsafe { lib.get(b"caap_runtime_value_free\0") }
        .map(|symbol: Symbol<ValueFreeFn>| *symbol)
        .ok();

    // SAFETY: `count_fn` is a valid no-argument C function pointer obtained above from `lib.get`.
    let count = validate_plugin_catalog_count(unsafe { count_fn() }, path)?;
    if count == 0 {
        // Keep library alive even if nothing to register.
        std::mem::forget(lib);
        return Ok(0);
    }
    // SAFETY: `entries_fn` is a valid no-argument C function pointer; returns a pointer to a
    // static array of `count` `PluginCatalogEntry` structs owned by the plugin library.
    let entries_ptr = unsafe { entries_fn() };
    if entries_ptr.is_null() {
        return Err(crate::error::CaapError::host(format!(
            "plugin catalog {} returned null entries pointer for {count} entries",
            path.display()
        )));
    }

    // Collect catalog entries while the library is still loaded.
    let mut catalog: Vec<(String, String, usize, Option<usize>)> = Vec::with_capacity(count);
    for i in 0..count {
        // SAFETY: `entries_ptr` is non-null (checked above) and points to a C array of exactly
        // `count` entries; `i` is in `0..count` so the offset is within bounds.
        let entry = unsafe { &*entries_ptr.add(i) };
        if entry.library.is_null() {
            return Err(crate::error::CaapError::host(format!(
                "plugin catalog {} entry {i} has null library pointer",
                path.display()
            )));
        }
        if entry.export.is_null() {
            return Err(crate::error::CaapError::host(format!(
                "plugin catalog {} entry {i} has null export pointer",
                path.display()
            )));
        }
        let library = cstr_to_string(
            // SAFETY: `entry.library` is non-null (checked above) and points to a null-terminated
            // C string owned by the plugin library, which is still loaded.
            unsafe { CStr::from_ptr(entry.library) },
            "plugin catalog library",
        )
        .map_err(crate::error::CaapError::host)?;
        let export = cstr_to_string(
            // SAFETY: `entry.export` is non-null (checked above) and points to a null-terminated
            // C string owned by the plugin library, which is still loaded.
            unsafe { CStr::from_ptr(entry.export) },
            "plugin catalog export",
        )
        .map_err(crate::error::CaapError::host)?;
        let (min, max) = plugin_catalog_arity(&library, &export, entry.min_arity, entry.max_arity)?;
        catalog.push((library, export, min, max));
    }
    validate_plugin_catalog_exports(registry, &catalog, path)?;

    // Leak the library so captured C function pointers remain valid for the process lifetime.
    Box::leak(Box::new(lib));

    for (library, export, min_arity, max_arity) in catalog {
        let fn_name = format!("{library}.{export}");
        let lib_name = library.clone();
        let exp_name = export.clone();

        let function = HostFunction::new(
            fn_name,
            min_arity,
            max_arity,
            Box::new(move |args: Vec<RuntimeValue>| {
                if let (Some(invoke_direct), Some(val_free)) = (invoke_direct_fn, value_free_fn) {
                    let lib_c =
                        CString::new(lib_name.as_str()).map_err(|e| eval_err(e.to_string()))?;
                    let exp_c =
                        CString::new(exp_name.as_str()).map_err(|e| eval_err(e.to_string()))?;

                    let mut sys_args = Vec::with_capacity(args.len());
                    for arg in &args {
                        sys_args.push(runtime_to_sys(arg)?);
                    }

                    let mut raw_args = encode_direct_plugin_args(&sys_args)?;

                    let mut out_val = CaapSysValue {
                        tag: 0,
                        int_val: 0,
                        float_val: 0.0,
                        ptr_val: std::ptr::null_mut(),
                        len_val: 0,
                    };
                    let mut err_ptr: *mut c_char = std::ptr::null_mut();

                    let ok = unsafe {
                        invoke_direct(
                            lib_c.as_ptr(),
                            exp_c.as_ptr(),
                            raw_args.as_ptr(),
                            raw_args.len(),
                            &mut out_val,
                            &mut err_ptr,
                        )
                    };

                    for raw_arg in &mut raw_args {
                        unsafe { free_sys_value_contents(raw_arg) };
                    }

                    if ok != 1 {
                        unsafe { val_free(&mut out_val) };
                        let msg = unsafe {
                            plugin_error_message_from_ptr(err_ptr, free_fn, &lib_name, &exp_name)?
                        };
                        return Err(eval_err(msg));
                    }

                    // The ABI contract states `error` is null on success, but a
                    // misbehaving plugin may still write a string there.  Free
                    // it unconditionally to avoid leaking that allocation.
                    if !err_ptr.is_null() {
                        unsafe { free_fn(err_ptr) };
                    }

                    unsafe {
                        direct_plugin_result_to_runtime(
                            &mut out_val,
                            val_free,
                            &lib_name,
                            &exp_name,
                        )
                    }
                } else {
                    // Build JSON args array.
                    let sys_args: Vec<SysValue> = args
                        .iter()
                        .map(runtime_to_sys)
                        .collect::<Result<Vec<_>, _>>()?;
                    let args_json = sys_values_to_json_array(&sys_args).map_err(eval_err)?;

                    let lib_c =
                        CString::new(lib_name.as_str()).map_err(|e| eval_err(e.to_string()))?;
                    let exp_c =
                        CString::new(exp_name.as_str()).map_err(|e| eval_err(e.to_string()))?;
                    let args_c = CString::new(args_json).map_err(|e| eval_err(e.to_string()))?;

                    let mut out_ptr: *mut c_char = std::ptr::null_mut();
                    let mut err_ptr: *mut c_char = std::ptr::null_mut();

                    // Safety: the plugin library is leaked after load-time symbol validation, so
                    // these typed C function pointers remain valid for the process lifetime.
                    let ok = unsafe {
                        invoke_fn(
                            lib_c.as_ptr(),
                            exp_c.as_ptr(),
                            args_c.as_ptr(),
                            &mut out_ptr,
                            &mut err_ptr,
                        )
                    };

                    if ok != 1 {
                        // SAFETY: `err_ptr` is written by the plugin's `caap_runtime_invoke`; the
                        // function validates the pointer and frees it via `free_fn`.
                        let msg = unsafe {
                            plugin_error_message_from_ptr(err_ptr, free_fn, &lib_name, &exp_name)?
                        };
                        return Err(eval_err(msg));
                    }

                    // Contract: `error` is null on success.  Defensively free
                    // anything a misbehaving plugin may have stashed there.
                    if !err_ptr.is_null() {
                        unsafe { free_fn(err_ptr) };
                    }

                    // SAFETY: `out_ptr` is written by the plugin's `caap_runtime_invoke` on success;
                    // the function validates the pointer and frees it via `free_fn`.
                    let result_json = unsafe {
                        plugin_result_json_from_ptr(out_ptr, free_fn, &lib_name, &exp_name)?
                    };

                    parse_json_to_runtime(&result_json).map_err(|e| {
                        eval_err(format!("{lib_name}.{exp_name}: bad result JSON: {e}"))
                    })
                }
            }),
        )?;

        let metadata = plugin_export_metadata(&library, &export, min_arity, max_arity);
        registry.register_function_with_metadata(
            &library,
            &export,
            PhasePolicy::Runtime,
            function,
            metadata,
        )?;
    }

    Ok(count)
}

fn plugin_export_metadata(
    library: &str,
    export: &str,
    min_arity: usize,
    max_arity: Option<usize>,
) -> HostExportMetadata {
    let mut params = (0..min_arity)
        .map(|index| HostExportParameter::new(format!("arg{index}"), "sys_value"))
        .collect::<Vec<_>>();
    if max_arity.is_none() {
        params.push(HostExportParameter::new("rest", "list<sys-value>"));
    }
    HostExportMetadata {
        module: Some(format!("plugin.{library}")),
        policy: "plugin_runtime".to_string(),
        effect: "impure".to_string(),
        kind: "function".to_string(),
        capability_kind: Some(format!("plugin.{library}.{export}")),
        signature: HostExportSignature {
            params,
            result: "sys_value".to_string(),
        },
        min_arity,
        max_arity,
    }
}

unsafe fn direct_plugin_result_to_runtime(
    out_val: &mut CaapSysValue,
    val_free: ValueFreeFn,
    lib_name: &str,
    exp_name: &str,
) -> Result<RuntimeValue, EvalSignal> {
    let result = to_sys_value(out_val)
        .map(sys_to_runtime)
        .map_err(|e| eval_err(format!("{lib_name}.{exp_name}: bad direct result: {e}")));
    val_free(out_val);
    result
}

fn encode_direct_plugin_args(args: &[SysValue]) -> Result<Vec<CaapSysValue>, EvalSignal> {
    let mut raw_args = Vec::with_capacity(args.len());
    for arg in args {
        match unsafe { from_sys_value(arg) } {
            Ok(raw) => raw_args.push(raw),
            Err(error) => {
                for raw_arg in &mut raw_args {
                    unsafe { free_sys_value_contents(raw_arg) };
                }
                return Err(eval_err(error));
            }
        }
    }
    Ok(raw_args)
}

fn validate_plugin_catalog_count(count: usize, path: &Path) -> CaapResult<usize> {
    if count > MAX_PLUGIN_CATALOG_ENTRIES {
        return Err(crate::error::CaapError::host(format!(
            "plugin catalog {} declares {count} entries, maximum supported is {MAX_PLUGIN_CATALOG_ENTRIES}",
            path.display()
        )));
    }
    Ok(count)
}

fn validate_plugin_abi_descriptor(
    path: &Path,
    descriptor: CaapRuntimeAbiDescriptor,
) -> CaapResult<()> {
    let expected = expected_plugin_abi_descriptor();
    if descriptor.abi_version != expected.abi_version {
        return Err(plugin_abi_mismatch(
            path,
            "ABI version",
            expected.abi_version,
            descriptor.abi_version,
        ));
    }
    if descriptor.value_encoding_version != expected.value_encoding_version {
        return Err(plugin_abi_mismatch(
            path,
            "value encoding version",
            expected.value_encoding_version,
            descriptor.value_encoding_version,
        ));
    }
    if descriptor.catalog_schema_version != expected.catalog_schema_version {
        return Err(plugin_abi_mismatch(
            path,
            "catalog schema version",
            expected.catalog_schema_version,
            descriptor.catalog_schema_version,
        ));
    }
    if descriptor.required_symbol_count != expected.required_symbol_count {
        return Err(plugin_abi_mismatch(
            path,
            "required symbol count",
            expected.required_symbol_count,
            descriptor.required_symbol_count,
        ));
    }
    if descriptor.required_symbols_hash != expected.required_symbols_hash {
        return Err(plugin_abi_mismatch(
            path,
            "required symbols hash",
            expected.required_symbols_hash,
            descriptor.required_symbols_hash,
        ));
    }
    if descriptor.capability_effect_catalog_hash != expected.capability_effect_catalog_hash {
        return Err(plugin_abi_mismatch(
            path,
            "capability/effect catalog hash",
            expected.capability_effect_catalog_hash,
            descriptor.capability_effect_catalog_hash,
        ));
    }
    Ok(())
}

fn expected_plugin_abi_descriptor() -> CaapRuntimeAbiDescriptor {
    CaapRuntimeAbiDescriptor {
        abi_version: CAAP_RUNTIME_ABI_VERSION,
        value_encoding_version: CAAP_RUNTIME_VALUE_ENCODING_VERSION,
        catalog_schema_version: CAAP_RUNTIME_CATALOG_SCHEMA_VERSION,
        required_symbol_count: CAAP_RUNTIME_REQUIRED_SYMBOLS.len() as u32,
        required_symbols_hash: caap_runtime_required_symbols_hash(),
        capability_effect_catalog_hash: caap_runtime_capability_effect_catalog_hash(),
    }
}

fn plugin_abi_mismatch<T: std::fmt::Display>(
    path: &Path,
    field: &str,
    expected: T,
    actual: T,
) -> crate::error::CaapError {
    crate::error::CaapError::host(format!(
        "runtime plugin {} {field} mismatch: expected {expected}, got {actual}",
        path.display()
    ))
}

fn plugin_required_symbol_error(
    path: &Path,
    symbol: &str,
    error: impl std::fmt::Display,
) -> crate::error::CaapError {
    crate::error::CaapError::host(format!(
        "runtime plugin {} missing required symbol {symbol}: {error}",
        path.display()
    ))
}

fn validate_plugin_catalog_exports(
    registry: &HostServiceRegistry,
    catalog: &[(String, String, usize, Option<usize>)],
    path: &Path,
) -> CaapResult<()> {
    let mut seen = BTreeSet::new();
    for (library, export, _, _) in catalog {
        let key = format!("{library}.{export}");
        if !seen.insert(key.clone()) {
            return Err(crate::error::CaapError::host(format!(
                "plugin catalog {} declares duplicate export {key}",
                path.display()
            )));
        }
        if let Some(entry) = registry.library(library)? {
            if entry.export(export)?.is_some() {
                return Err(crate::error::CaapError::host(format!(
                    "plugin catalog {} conflicts with existing host export {key}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn plugin_catalog_arity(
    library: &str,
    export: &str,
    min_arity: std::os::raw::c_int,
    max_arity: std::os::raw::c_int,
) -> CaapResult<(usize, Option<usize>)> {
    if min_arity < 0 {
        return Err(crate::error::CaapError::host(format!(
            "plugin catalog {library}.{export}: min_arity must be non-negative"
        )));
    }
    if max_arity < -1 {
        return Err(crate::error::CaapError::host(format!(
            "plugin catalog {library}.{export}: max_arity must be -1 or non-negative"
        )));
    }
    if max_arity >= 0 && max_arity < min_arity {
        return Err(crate::error::CaapError::host(format!(
            "plugin catalog {library}.{export}: max_arity must be >= min_arity"
        )));
    }
    let min = min_arity as usize;
    let max = if max_arity == -1 {
        None
    } else {
        Some(max_arity as usize)
    };
    Ok((min, max))
}

fn cstr_to_string(value: &CStr, context: &str) -> Result<String, String> {
    value
        .to_str()
        .map(|s| s.to_string())
        .map_err(|e| format!("{context}: C string is not valid UTF-8: {e}"))
}

unsafe fn plugin_error_message_from_ptr(
    err_ptr: *mut c_char,
    free_fn: StringFreeFn,
    library: &str,
    export: &str,
) -> Result<String, EvalSignal> {
    if err_ptr.is_null() {
        return Err(eval_err(format!(
            "{library}.{export}: plugin returned failure without error message"
        )));
    }

    let result = cstr_to_string(
        CStr::from_ptr(err_ptr),
        &format!("{library}.{export} plugin error"),
    )
    .map_err(eval_err);
    free_fn(err_ptr);
    result
}

unsafe fn plugin_result_json_from_ptr(
    out_ptr: *mut c_char,
    free_fn: StringFreeFn,
    library: &str,
    export: &str,
) -> Result<String, EvalSignal> {
    if out_ptr.is_null() {
        return Err(eval_err(format!(
            "{library}.{export}: plugin returned success without result JSON"
        )));
    }

    let result = cstr_to_string(
        CStr::from_ptr(out_ptr),
        &format!("{library}.{export} plugin result"),
    )
    .map_err(eval_err);
    free_fn(out_ptr);
    result
}

// ── JSON helpers for plugin ABI ───────────────────────────────────────────────

fn sys_values_to_json_array(vals: &[SysValue]) -> Result<String, String> {
    let values = vals
        .iter()
        .map(sys_value_to_json_value)
        .collect::<Result<Vec<_>, _>>()?;
    serde_json::to_string(&serde_json::Value::Array(values))
        .map_err(|e| format!("failed to encode CAAP SYS plugin JSON args: {e}"))
}

fn parse_json_to_runtime(json: &str) -> Result<RuntimeValue, String> {
    let value: serde_json::Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("plugin JSON parse error: {e}"))?;
    Ok(sys_to_runtime(json_value_to_sys(value)?))
}

fn sys_value_to_json_value(value: &SysValue) -> Result<serde_json::Value, String> {
    match value {
        SysValue::Null => Ok(serde_json::Value::Null),
        SysValue::Bool(value) => Ok(serde_json::Value::Bool(*value)),
        SysValue::Int(value) => Ok(serde_json::Value::Number((*value).into())),
        SysValue::Float(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| format!("cannot encode non-finite float: {value}")),
        SysValue::Str(value) => Ok(serde_json::Value::String(value.clone())),
        SysValue::Bytes(bytes) => {
            let mut map = serde_json::Map::new();
            map.insert(
                caap_sys_runtime::ffi::BYTES_JSON_TAG.to_string(),
                serde_json::Value::String(caap_sys_runtime::ffi::base64_encode(bytes)),
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
            if entries.len() == 1 {
                if let Some(serde_json::Value::String(encoded)) =
                    entries.get(caap_sys_runtime::ffi::BYTES_JSON_TAG)
                {
                    let bytes = caap_sys_runtime::ffi::base64_decode(encoded)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DIRECT_VALUE_FREE_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn sys_bridge_preserves_float_values() {
        let value = RuntimeValue::Float(3.5);
        assert_eq!(runtime_to_sys(&value).unwrap(), SysValue::Float(3.5));
        assert_eq!(sys_to_runtime(SysValue::Float(3.5)), value);
    }

    #[test]
    fn sys_bridge_rejects_values_without_sys_representation() {
        let error = runtime_to_sys(&RuntimeValue::UninitializedTopLevel)
            .unwrap_err()
            .to_string();
        assert!(error.contains("cannot serialize uninitialized_top_level values"));
    }

    #[test]
    fn sys_bridge_rejects_non_string_map_keys() {
        let map = RuntimeValue::Map(Rc::new(RefCell::new(indexmap::IndexMap::from([(
            MapKey::Int(1),
            RuntimeValue::Bool(true),
        )]))));
        let error = runtime_to_sys(&map).unwrap_err().to_string();
        assert!(error.contains("only supports string map keys"));
    }

    #[test]
    fn plugin_loader_rejects_missing_directory() {
        let mut registry = HostServiceRegistry::new();
        let missing_dir = std::env::temp_dir().join(format!(
            "caap-missing-plugin-dir-{}-{}",
            std::process::id(),
            line!()
        ));

        let error = load_plugin_directory(&mut registry, &missing_dir)
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not exist or is not a directory"));
    }

    #[test]
    fn plugin_candidates_are_filtered_and_sorted() {
        let root = std::env::temp_dir().join(format!(
            "caap-plugin-candidates-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let z = root.join("zeta.so");
        let a = root.join("alpha.dylib");
        let ignored = root.join("note.txt");
        let no_extension = root.join("plugin");
        let nested_extension = root.join("backup.so.bak");
        std::fs::write(&z, b"").unwrap();
        std::fs::write(&a, b"").unwrap();
        std::fs::write(&ignored, b"").unwrap();
        std::fs::write(&no_extension, b"").unwrap();
        std::fs::write(&nested_extension, b"").unwrap();

        let candidates = runtime_plugin_candidates(&root).unwrap();
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(candidates, vec![a, z]);
    }

    #[test]
    fn plugin_c_string_boundary_rejects_invalid_utf8() {
        let value = c"bad-\xFF";
        let error = cstr_to_string(value, "plugin result").unwrap_err();
        assert!(error.contains("plugin result: C string is not valid UTF-8"));
    }

    #[test]
    fn plugin_result_json_rejects_success_without_output_pointer() {
        // SAFETY: intentionally passing null_mut to test null-pointer rejection logic.
        let error = unsafe {
            plugin_result_json_from_ptr(std::ptr::null_mut(), test_string_free, "plugin", "export")
        }
        .unwrap_err()
        .to_string();

        assert!(error.contains("plugin.export: plugin returned success without result JSON"));
    }

    #[test]
    fn plugin_error_message_rejects_failure_without_error_pointer() {
        // SAFETY: intentionally passing null_mut to test null-pointer rejection logic.
        let error = unsafe {
            plugin_error_message_from_ptr(
                std::ptr::null_mut(),
                test_string_free,
                "plugin",
                "export",
            )
        }
        .unwrap_err()
        .to_string();

        assert!(error.contains("plugin.export: plugin returned failure without error message"));
    }

    #[test]
    fn plugin_error_message_decodes_and_frees_error_pointer() {
        let out = std::ffi::CString::new("failed").unwrap().into_raw();
        // SAFETY: `out` is a heap-allocated CString raw pointer; ownership is transferred to the
        // function which calls `test_string_free` to free it.
        let result = unsafe {
            plugin_error_message_from_ptr(out, test_string_free, "plugin", "export").unwrap()
        };

        assert_eq!(result, "failed");
    }

    #[test]
    fn plugin_result_json_decodes_and_frees_result_pointer() {
        let out = std::ffi::CString::new("null").unwrap().into_raw();
        // SAFETY: `out` is a heap-allocated CString raw pointer; ownership is transferred to the
        // function which calls `test_string_free` to free it.
        let result = unsafe {
            plugin_result_json_from_ptr(out, test_string_free, "plugin", "export").unwrap()
        };

        assert_eq!(result, "null");
    }

    #[test]
    fn plugin_direct_result_is_freed_when_decoding_fails() {
        DIRECT_VALUE_FREE_CALLS.store(0, Ordering::SeqCst);
        let mut bytes = vec![0xff].into_boxed_slice();
        let ptr = bytes.as_mut_ptr();
        std::mem::forget(bytes);
        let mut out = CaapSysValue {
            tag: 4,
            int_val: 0,
            float_val: 0.0,
            ptr_val: ptr,
            len_val: 1,
        };

        // SAFETY: `out` is a valid direct-ABI string buffer with invalid UTF-8 payload. The test
        // free function owns and releases the nested allocation.
        let error =
            unsafe { direct_plugin_result_to_runtime(&mut out, test_value_free, "plugin", "bad") }
                .unwrap_err()
                .to_string();

        assert!(error.contains("plugin.bad: bad direct result"));
        assert!(error.contains("invalid UTF-8 string in CaapSysValue"));
        assert_eq!(DIRECT_VALUE_FREE_CALLS.load(Ordering::SeqCst), 1);
        assert!(out.ptr_val.is_null());
        assert_eq!(out.len_val, 0);
    }

    #[test]
    fn direct_plugin_arg_encoding_rejects_invalid_later_arg_after_cleanup() {
        let bad_key = "bad\0key".to_string();
        let args = [
            SysValue::Str("allocated_before_error".to_string()),
            SysValue::Map(std::collections::HashMap::from([(
                bad_key,
                SysValue::Int(1),
            )])),
        ];

        let error = encode_direct_plugin_args(&args).unwrap_err().to_string();

        assert!(error.contains("map key contains interior NUL byte"));
    }

    unsafe extern "C" fn test_string_free(value: *mut c_char) {
        if !value.is_null() {
            drop(std::ffi::CString::from_raw(value));
        }
    }

    unsafe extern "C" fn test_value_free(value: *mut CaapSysValue) {
        DIRECT_VALUE_FREE_CALLS.fetch_add(1, Ordering::SeqCst);
        if !value.is_null() {
            free_sys_value_contents(&mut *value);
        }
    }

    #[test]
    fn plugin_catalog_arity_rejects_malformed_ranges() {
        assert_eq!(
            plugin_catalog_arity("math", "sum", 1, -1).unwrap(),
            (1, None)
        );
        assert_eq!(
            plugin_catalog_arity("math", "id", 1, 1).unwrap(),
            (1, Some(1))
        );

        let error = plugin_catalog_arity("math", "bad", -1, 1)
            .unwrap_err()
            .to_string();
        assert!(error.contains("min_arity must be non-negative"));

        let error = plugin_catalog_arity("math", "bad", 1, -2)
            .unwrap_err()
            .to_string();
        assert!(error.contains("max_arity must be -1 or non-negative"));

        let error = plugin_catalog_arity("math", "bad", 2, 1)
            .unwrap_err()
            .to_string();
        assert!(error.contains("max_arity must be >= min_arity"));
    }

    #[test]
    fn plugin_catalog_count_is_bounded_before_allocation() {
        let path = std::path::Path::new("/tmp/malformed-plugin.so");
        assert_eq!(validate_plugin_catalog_count(0, path).unwrap(), 0);
        assert_eq!(
            validate_plugin_catalog_count(MAX_PLUGIN_CATALOG_ENTRIES, path).unwrap(),
            MAX_PLUGIN_CATALOG_ENTRIES
        );

        let error = validate_plugin_catalog_count(MAX_PLUGIN_CATALOG_ENTRIES + 1, path)
            .unwrap_err()
            .to_string();

        assert!(error.contains("maximum supported"));
    }

    #[test]
    fn plugin_abi_descriptor_accepts_expected_descriptor() {
        let path = std::path::Path::new("/tmp/plugin.so");

        validate_plugin_abi_descriptor(path, expected_plugin_abi_descriptor()).unwrap();
    }

    #[test]
    fn plugin_abi_descriptor_rejects_wrong_abi_version() {
        let path = std::path::Path::new("/tmp/plugin.so");
        let mut descriptor = expected_plugin_abi_descriptor();
        descriptor.abi_version += 1;

        let error = validate_plugin_abi_descriptor(path, descriptor)
            .unwrap_err()
            .to_string();

        assert!(error.contains("ABI version mismatch"));
        assert!(error.contains("expected 1, got 2"));
    }

    #[test]
    fn plugin_abi_descriptor_rejects_wrong_catalog_hash() {
        let path = std::path::Path::new("/tmp/plugin.so");
        let mut descriptor = expected_plugin_abi_descriptor();
        descriptor.capability_effect_catalog_hash ^= 0x01;

        let error = validate_plugin_abi_descriptor(path, descriptor)
            .unwrap_err()
            .to_string();

        assert!(error.contains("capability/effect catalog hash mismatch"));
    }

    #[test]
    fn plugin_required_symbol_error_names_missing_symbol() {
        let path = std::path::Path::new("/tmp/plugin.so");
        let error = plugin_required_symbol_error(path, "caap_runtime_abi_descriptor", "not found")
            .to_string();

        assert!(error.contains("missing required symbol caap_runtime_abi_descriptor"));
    }

    #[test]
    fn plugin_catalog_preflight_rejects_duplicate_exports() {
        let registry = HostServiceRegistry::new();
        let path = std::path::Path::new("/tmp/duplicate-plugin.so");
        let catalog = vec![
            ("plugin".to_string(), "run".to_string(), 0, Some(0)),
            ("plugin".to_string(), "run".to_string(), 1, Some(1)),
        ];

        let error = validate_plugin_catalog_exports(&registry, &catalog, path)
            .unwrap_err()
            .to_string();

        assert!(error.contains("declares duplicate export plugin.run"));
    }

    #[test]
    fn plugin_catalog_preflight_rejects_existing_host_export_conflicts() {
        let mut registry = HostServiceRegistry::new();
        registry.register_default_system_libraries().unwrap();
        let path = std::path::Path::new("/tmp/conflicting-plugin.so");
        let catalog = vec![("path".to_string(), "join".to_string(), 2, None)];

        let error = validate_plugin_catalog_exports(&registry, &catalog, path)
            .unwrap_err()
            .to_string();

        assert!(error.contains("conflicts with existing host export path.join"));
    }

    #[test]
    fn plugin_export_metadata_is_explicit_and_independent_of_builtin_contracts() {
        let metadata = plugin_export_metadata("demo", "run", 2, None);

        assert_eq!(metadata.module, Some("plugin.demo".to_string()));
        assert_eq!(metadata.policy, "plugin_runtime");
        assert_eq!(
            metadata.capability_kind,
            Some("plugin.demo.run".to_string())
        );
        assert_eq!(metadata.min_arity, 2);
        assert_eq!(metadata.max_arity, None);
        assert!(metadata.is_variadic());
        assert_eq!(metadata.signature.params.len(), 3);
        assert_eq!(metadata.signature.result, "sys_value");
    }

    #[test]
    fn plugin_json_bridge_preserves_float_values() {
        assert_eq!(
            sys_values_to_json_array(&[SysValue::Float(3.5)]).unwrap(),
            "[3.5]"
        );
        assert_eq!(
            parse_json_to_runtime("3.5").unwrap(),
            RuntimeValue::Float(3.5)
        );
    }

    #[test]
    fn plugin_json_bridge_rejects_malformed_json() {
        let error = parse_json_to_runtime("[1,]").unwrap_err();
        assert!(error.contains("plugin JSON parse error"));
    }

    #[test]
    fn plugin_json_bridge_rejects_unsigned_values_outside_sys_int_range() {
        let error = parse_json_to_runtime("9223372036854775808").unwrap_err();
        assert!(error.contains("outside CAAP SYS range"));
    }
}
