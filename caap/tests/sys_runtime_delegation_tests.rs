//! Tests for the interpreter ↔ caap-sys-runtime delegation boundary.
//!
//! The interpreter no longer reimplements fs/net/process/io/os; it delegates to
//! caap-sys-runtime after applying host policy, holding its own per-session
//! `RuntimeState`. These tests pin down three properties of that design:
//!   1. delegated operations behave end-to-end (write → read → metadata, and the
//!      stateful open/seek/read/close handle cycle),
//!   2. host policy is enforced *before* dispatch (a denied write never reaches
//!      the runtime), and
//!   3. handle state is scoped to a registry, not a process-global thread_local.

use caap_core::{
    HostCapabilityPolicy, HostServiceRegistry, HostSystemPolicy, MapKey, PhasePolicy, RuntimeValue,
};
use std::rc::Rc;

fn open_registry() -> HostServiceRegistry {
    let mut registry = HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(HostSystemPolicy::allow_all());
    registry.set_capability_policy(HostCapabilityPolicy::allow_all());
    registry
}

fn call(
    registry: &HostServiceRegistry,
    library: &str,
    export: &str,
    args: Vec<RuntimeValue>,
) -> Result<RuntimeValue, caap_core::EvalSignal> {
    let value = registry
        .export(library, export, PhasePolicy::Runtime)
        .unwrap_or_else(|e| panic!("export {library}.{export}: {e}"));
    let RuntimeValue::HostFunction(function) = value else {
        panic!("expected {library}.{export} host function");
    };
    (function.handler)(args)
}

fn str_arg(value: &str) -> RuntimeValue {
    RuntimeValue::Str(value.into())
}

fn unique_temp_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "caap-delegation-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn delegated_fs_write_read_metadata_round_trip() {
    let registry = open_registry();
    let path = unique_temp_path("rw");
    let path_str = path.to_str().unwrap().to_string();

    call(
        &registry,
        "fs",
        "write_text",
        vec![str_arg(&path_str), str_arg("hello sys")],
    )
    .unwrap();

    let read = call(&registry, "fs", "read_text", vec![str_arg(&path_str)]).unwrap();
    assert_eq!(read, RuntimeValue::Str("hello sys".into()));

    let metadata = call(&registry, "fs", "metadata", vec![str_arg(&path_str)]).unwrap();
    let RuntimeValue::Map(metadata) = metadata else {
        panic!("expected metadata map");
    };
    let metadata = metadata.borrow();
    assert_eq!(
        metadata.get(&MapKey::Str("size".into())),
        Some(&RuntimeValue::Int(9))
    );
    assert_eq!(
        metadata.get(&MapKey::Str("is_file".into())),
        Some(&RuntimeValue::Bool(true))
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn delegated_fs_handle_cycle_open_write_seek_read_close() {
    let registry = open_registry();
    let path = unique_temp_path("handle");
    let path_str = path.to_str().unwrap().to_string();

    let spec = |entries: Vec<(&str, RuntimeValue)>| {
        let mut map = indexmap::IndexMap::new();
        for (key, value) in entries {
            map.insert(MapKey::Str(key.into()), value);
        }
        RuntimeValue::Map(Rc::new(std::cell::RefCell::new(map)))
    };

    let handle = call(
        &registry,
        "fs",
        "open_file",
        vec![spec(vec![
            ("path", str_arg(&path_str)),
            ("write", RuntimeValue::Bool(true)),
            ("read", RuntimeValue::Bool(true)),
            ("create", RuntimeValue::Bool(true)),
            ("truncate", RuntimeValue::Bool(true)),
        ])],
    )
    .unwrap();
    let RuntimeValue::Int(handle) = handle else {
        panic!("expected file handle");
    };
    let handle = RuntimeValue::Int(handle);

    call(
        &registry,
        "fs",
        "file_write",
        vec![handle.clone(), str_arg("0123456789")],
    )
    .unwrap();
    // Seek back to the start, then read the whole file.
    call(
        &registry,
        "fs",
        "file_seek",
        vec![handle.clone(), RuntimeValue::Int(0), str_arg("start")],
    )
    .unwrap();
    let text = call(&registry, "fs", "file_read_all_text", vec![handle.clone()]).unwrap();
    assert_eq!(text, RuntimeValue::Str("0123456789".into()));

    call(&registry, "fs", "close_file", vec![handle.clone()]).unwrap();
    // After close the handle is gone.
    let err =
        call(&registry, "fs", "close_file", vec![handle]).expect_err("closing twice should fail");
    assert!(err.to_string().contains("unknown"), "{err}");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn host_policy_denies_write_before_dispatch() {
    // Capability is allowed (so the export binds), but the system policy denies
    // all filesystem writes. The denial must happen in `authorize`, before the
    // runtime is touched: no file is created.
    let mut registry = HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_capability_policy(HostCapabilityPolicy::allow_all());
    registry.set_system_policy(HostSystemPolicy::deny_all());

    let path = unique_temp_path("denied");
    let path_str = path.to_str().unwrap().to_string();

    let err = call(
        &registry,
        "fs",
        "write_text",
        vec![str_arg(&path_str), str_arg("nope")],
    )
    .expect_err("write must be denied by host policy");
    assert!(
        err.to_string().contains("write access is not allowed"),
        "{err}"
    );
    assert!(!path.exists(), "denied write must not create the file");
}

#[test]
fn delegated_fs_bytes_round_trip_preserves_non_utf8() {
    let registry = open_registry();
    let path = unique_temp_path("bytes");
    let path_str = path.to_str().unwrap().to_string();
    let payload: Vec<u8> = vec![0, 159, 146, 150, 255];

    call(
        &registry,
        "fs",
        "write_bytes",
        vec![
            str_arg(&path_str),
            RuntimeValue::Bytes(payload.clone().into()),
        ],
    )
    .unwrap();

    let read = call(&registry, "fs", "read_bytes", vec![str_arg(&path_str)]).unwrap();
    match read {
        RuntimeValue::Bytes(bytes) => assert_eq!(bytes.as_ref(), payload.as_slice()),
        other => panic!("expected bytes, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn handle_state_is_scoped_per_registry_not_thread_global() {
    // Two independent registries each own their own RuntimeState. A file handle
    // opened in one must be invisible to the other — proving handle lifetime is
    // session-scoped rather than living in a process-global thread_local.
    let registry_a = open_registry();
    let registry_b = open_registry();

    let path = unique_temp_path("isolation");
    let path_str = path.to_str().unwrap().to_string();
    std::fs::write(&path, b"shared").unwrap();

    let mut map = indexmap::IndexMap::new();
    map.insert(MapKey::Str("path".into()), str_arg(&path_str));
    map.insert(MapKey::Str("read".into()), RuntimeValue::Bool(true));
    let spec = RuntimeValue::Map(Rc::new(std::cell::RefCell::new(map)));

    let handle = call(&registry_a, "fs", "open_file", vec![spec]).unwrap();
    let RuntimeValue::Int(_) = handle else {
        panic!("expected file handle");
    };

    // The same numeric handle is unknown in registry B.
    let err = call(
        &registry_b,
        "fs",
        "file_read_all_text",
        vec![handle.clone()],
    )
    .expect_err("handle from registry A must not exist in registry B");
    assert!(err.to_string().contains("unknown"), "{err}");

    // It is still valid in registry A.
    call(
        &registry_a,
        "fs",
        "file_read_all_text",
        vec![handle.clone()],
    )
    .unwrap();
    call(&registry_a, "fs", "close_file", vec![handle]).unwrap();

    let _ = std::fs::remove_file(&path);
}
