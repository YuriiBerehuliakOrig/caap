/// Integration tests for concrete CAAP SYS host library exports.
///
/// These scenarios validate fs/path/net/process/io/time behavior separately from
/// registry and builtin projection tests.
use caap_core::{frontend::parse, Evaluator, MapKey, RuntimeValue};
use std::rc::Rc;

#[test]
fn test_host_system_io_and_time_exports_match_system_surface() {
    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());

    for export in [
        "print",
        "println",
        "write",
        "eprint",
        "eprintln",
        "flush_stdout",
        "flush_stderr",
        "read_line",
        "read_all",
    ] {
        assert!(
            registry
                .export("io", export, caap_core::PhasePolicy::Runtime)
                .is_ok(),
            "expected io.{export} export"
        );
    }

    // Sys exports are dual-phase: what a compile-time evaluation may actually
    // DO is decided by the per-phase HostSystemPolicy (the compile-time sandbox
    // blocks stdin reads etc.), not by a coarse per-library phase gate.
    for export in [
        "print",
        "println",
        "write",
        "eprint",
        "eprintln",
        "flush_stdout",
        "flush_stderr",
        "read_line",
        "read_all",
    ] {
        assert!(
            registry
                .export("io", export, caap_core::PhasePolicy::CompileTime)
                .is_ok(),
            "expected io.{export} to be available at compile time (policy gates behaviour)"
        );
    }

    let now = registry
        .export("time", "now_unix_ns", caap_core::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(now) = now else {
        panic!("expected time.now-unix-ns host function");
    };
    let RuntimeValue::Int(first) = (now.handler)(vec![]).unwrap() else {
        panic!("expected time.now-unix-ns int result");
    };
    let RuntimeValue::Int(second) = (now.handler)(vec![]).unwrap() else {
        panic!("expected time.now-unix-ns int result");
    };
    assert!(first > 0);
    assert!(second >= first);
}

#[test]
fn test_host_system_policy_enforces_native_sandbox_surface() {
    fn host_function(
        registry: &caap_core::HostServiceRegistry,
        library: &str,
        export: &str,
    ) -> Rc<caap_core::HostFunction> {
        let value = registry
            .export(library, export, caap_core::PhasePolicy::Runtime)
            .unwrap();
        let RuntimeValue::HostFunction(function) = value else {
            panic!("expected {library}.{export} host function");
        };
        function
    }

    fn spec(entries: Vec<(&str, RuntimeValue)>) -> RuntimeValue {
        let mut map = indexmap::IndexMap::new();
        for (key, value) in entries {
            map.insert(MapKey::Str(key.into()), value);
        }
        RuntimeValue::Map(Rc::new(std::cell::RefCell::new(map)))
    }

    fn argv(items: &[&str]) -> RuntimeValue {
        RuntimeValue::List(Rc::new(std::cell::RefCell::new(
            items
                .iter()
                .map(|item| RuntimeValue::Str((*item).into()))
                .collect(),
        )))
    }

    let allowed_root = std::env::temp_dir().join(format!(
        "caap-host-policy-allowed-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let denied_root = std::env::temp_dir().join(format!(
        "caap-host-policy-denied-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&allowed_root).unwrap();
    std::fs::create_dir_all(&denied_root).unwrap();
    let allowed_file = allowed_root.join("allowed.txt");
    let denied_file = denied_root.join("denied.txt");
    std::fs::write(&allowed_file, "allowed").unwrap();
    std::fs::write(&denied_file, "denied").unwrap();

    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());
    let mut policy = caap_core::HostSystemPolicy::allow_all();
    policy.fs = caap_core::HostFileSystemPolicy {
        read_roots: Some(vec![allowed_root.clone()]),
        write_roots: Some(vec![allowed_root.clone()]),
    };
    policy.io.allow_stdin_read = false;
    policy.process.allow_spawn = false;
    policy.net.allow_listen = false;
    policy.net.allow_connect = false;
    policy.os = caap_core::HostOsEnvironmentPolicy::allow_only([]).unwrap();
    registry.set_system_policy(policy);

    let read_text = host_function(&registry, "fs", "read_text");
    assert_eq!(
        (read_text.handler)(vec![RuntimeValue::Str(
            allowed_file.to_str().unwrap().into()
        )])
        .unwrap(),
        RuntimeValue::Str("allowed".into())
    );
    let err = (read_text.handler)(vec![RuntimeValue::Str(
        denied_file.to_str().unwrap().into(),
    )])
    .expect_err("read outside allowed roots must fail");
    assert!(format!("{err}").contains("outside allowed roots"));

    let write_text = host_function(&registry, "fs", "write_text");
    (write_text.handler)(vec![
        RuntimeValue::Str(allowed_root.join("written.txt").to_str().unwrap().into()),
        RuntimeValue::Str("ok".into()),
    ])
    .unwrap();
    let err = (write_text.handler)(vec![
        RuntimeValue::Str(denied_root.join("written.txt").to_str().unwrap().into()),
        RuntimeValue::Str("no".into()),
    ])
    .expect_err("write outside allowed roots must fail");
    assert!(format!("{err}").contains("outside allowed roots"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let external_root = std::env::temp_dir().join(format!(
            "caap-host-policy-external-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&external_root).unwrap();
        let external_file = external_root.join("secret.txt");
        std::fs::write(&external_file, "secret").unwrap();
        let read_escape = allowed_root.join("read_escape.txt");
        symlink(&external_file, &read_escape).unwrap();

        let err = (read_text.handler)(vec![RuntimeValue::Str(
            read_escape.to_str().unwrap().into(),
        )])
        .expect_err("read through symlink outside allowed roots must fail");
        assert!(format!("{err}").contains("outside allowed roots"));

        let write_escape_dir = allowed_root.join("write_escape");
        symlink(&external_root, &write_escape_dir).unwrap();
        let err = (write_text.handler)(vec![
            RuntimeValue::Str(
                write_escape_dir
                    .join("written_through_link.txt")
                    .to_str()
                    .unwrap()
                    .into(),
            ),
            RuntimeValue::Str("no".into()),
        ])
        .expect_err("write through symlink outside allowed roots must fail");
        assert!(format!("{err}").contains("outside allowed roots"));
    }

    let process_run = host_function(&registry, "process", "run");
    let err = (process_run.handler)(vec![spec(vec![(
        "argv",
        argv(&["/bin/sh", "-c", "exit 0"]),
    )])])
    .expect_err("process spawn must be denied");
    assert!(format!("{err}").contains("process spawning is not allowed"));

    let net_listen = host_function(&registry, "net", "listen");
    let err = (net_listen.handler)(vec![spec(vec![
        ("host", RuntimeValue::Str("127.0.0.1".into())),
        ("port", RuntimeValue::Int(0)),
    ])])
    .expect_err("network listen must be denied");
    assert!(format!("{err}").contains("network listening is not allowed"));

    let io_read_line = host_function(&registry, "io", "read_line");
    let err = (io_read_line.handler)(vec![]).expect_err("stdin read must be denied");
    assert!(format!("{err}").contains("stdin reading is not allowed"));

    assert_eq!(
        (host_function(&registry, "os", "env_has").handler)(vec![RuntimeValue::Str("PATH".into())])
            .unwrap(),
        RuntimeValue::Bool(false)
    );
    assert_eq!(
        (host_function(&registry, "os", "env_get").handler)(vec![RuntimeValue::Str("PATH".into())])
            .unwrap(),
        RuntimeValue::Null
    );
    let RuntimeValue::List(env_keys) =
        (host_function(&registry, "os", "env_keys").handler)(vec![]).unwrap()
    else {
        panic!("expected os.env-keys list");
    };
    assert!(env_keys.borrow().is_empty());
    let RuntimeValue::Map(env_vars) =
        (host_function(&registry, "os", "env_vars").handler)(vec![]).unwrap()
    else {
        panic!("expected os.env-vars map");
    };
    assert!(env_vars.borrow().is_empty());

    let _ = std::fs::remove_dir_all(allowed_root);
    let _ = std::fs::remove_dir_all(denied_root);
}

#[test]
fn test_host_system_libraries_support_net_parsing_without_network_io() {
    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());

    let is_ip = registry
        .export("net", "is_ip", caap_core::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(is_ip) = is_ip else {
        panic!("expected net.is-ip host function");
    };
    assert_eq!(
        (is_ip.handler)(vec![RuntimeValue::Str("127.0.0.1".into())]).unwrap(),
        RuntimeValue::Bool(true)
    );
    assert_eq!(
        (is_ip.handler)(vec![RuntimeValue::Str("localhost".into())]).unwrap(),
        RuntimeValue::Bool(false)
    );

    let host_port = registry
        .export("net", "host_port", caap_core::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(host_port) = host_port else {
        panic!("expected net.host-port host function");
    };
    assert_eq!(
        (host_port.handler)(vec![
            RuntimeValue::Str("::1".into()),
            RuntimeValue::Int(8080),
        ])
        .unwrap(),
        RuntimeValue::Str("[::1]:8080".into())
    );
}

#[test]
fn test_host_system_libraries_support_process_and_fs_write_text() {
    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());

    let process_id = registry
        .export("process", "id", caap_core::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(process_id) = process_id else {
        panic!("expected process.id host function");
    };
    assert!(matches!((process_id.handler)(vec![]).unwrap(), RuntimeValue::Int(id) if id > 0));

    let path = std::env::temp_dir().join(format!(
        "caap-fs-write-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let write_text = registry
        .export("fs", "write_text", caap_core::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(write_text) = write_text else {
        panic!("expected fs.write_text host function");
    };
    (write_text.handler)(vec![
        RuntimeValue::Str(path.to_str().unwrap().into()),
        RuntimeValue::Str("hello".into()),
    ])
    .unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    let _ = std::fs::remove_file(path);
}

#[test]
fn test_host_system_process_lifecycle_exports_match_system_surface() {
    fn host_call(
        registry: &caap_core::HostServiceRegistry,
        export: &str,
        args: Vec<RuntimeValue>,
    ) -> RuntimeValue {
        let value = registry
            .export("process", export, caap_core::PhasePolicy::Runtime)
            .unwrap();
        let RuntimeValue::HostFunction(function) = value else {
            panic!("expected process.{export} host function");
        };
        (function.handler)(args).unwrap()
    }

    fn spec(entries: Vec<(&str, RuntimeValue)>) -> RuntimeValue {
        let mut map = indexmap::IndexMap::new();
        for (key, value) in entries {
            map.insert(MapKey::Str(key.into()), value);
        }
        RuntimeValue::Map(Rc::new(std::cell::RefCell::new(map)))
    }

    fn argv(items: &[&str]) -> RuntimeValue {
        RuntimeValue::List(Rc::new(std::cell::RefCell::new(
            items
                .iter()
                .map(|item| RuntimeValue::Str((*item).into()))
                .collect(),
        )))
    }

    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());

    let run = host_call(
        &registry,
        "run",
        vec![spec(vec![(
            "argv",
            argv(&["/bin/sh", "-c", "printf out; printf err >&2; exit 4"]),
        )])],
    );
    let RuntimeValue::Map(run) = run else {
        panic!("expected process.run map");
    };
    let run = run.borrow();
    assert_eq!(
        run.get(&MapKey::Str("status".into())),
        Some(&RuntimeValue::Int(4))
    );
    assert_eq!(
        run.get(&MapKey::Str("success".into())),
        Some(&RuntimeValue::Bool(false))
    );
    assert_eq!(
        run.get(&MapKey::Str("stdout".into())),
        Some(&RuntimeValue::Str("out".into()))
    );
    assert_eq!(
        run.get(&MapKey::Str("stderr".into())),
        Some(&RuntimeValue::Str("err".into()))
    );
    drop(run);

    let spawned = host_call(
        &registry,
        "spawn",
        vec![spec(vec![
            ("argv", argv(&["/bin/sh", "-c", "cat"])),
            ("capture_stdout", RuntimeValue::Bool(true)),
            ("capture_stderr", RuntimeValue::Bool(true)),
        ])],
    );
    let RuntimeValue::Int(handle) = spawned else {
        panic!("expected process handle");
    };
    host_call(
        &registry,
        "write_stdin",
        vec![RuntimeValue::Int(handle), RuntimeValue::Str("hello".into())],
    );
    host_call(&registry, "close_stdin", vec![RuntimeValue::Int(handle)]);
    assert_eq!(
        host_call(&registry, "read_stdout", vec![RuntimeValue::Int(handle)]),
        RuntimeValue::Str("hello".into())
    );
    let waited = host_call(&registry, "wait", vec![RuntimeValue::Int(handle)]);
    let RuntimeValue::Map(waited) = waited else {
        panic!("expected process.wait map");
    };
    assert_eq!(
        waited.borrow().get(&MapKey::Str("success".into())),
        Some(&RuntimeValue::Bool(true))
    );
}

#[cfg(unix)]
#[test]
fn test_host_system_process_io_enforces_spawn_timeout_deadline() {
    fn host_result(
        registry: &caap_core::HostServiceRegistry,
        export: &str,
        args: Vec<RuntimeValue>,
    ) -> Result<RuntimeValue, caap_core::EvalSignal> {
        let value = registry
            .export("process", export, caap_core::PhasePolicy::Runtime)
            .unwrap();
        let RuntimeValue::HostFunction(function) = value else {
            panic!("expected process.{export} host function");
        };
        (function.handler)(args)
    }

    fn spec(entries: Vec<(&str, RuntimeValue)>) -> RuntimeValue {
        let mut map = indexmap::IndexMap::new();
        for (key, value) in entries {
            map.insert(MapKey::Str(key.into()), value);
        }
        RuntimeValue::Map(Rc::new(std::cell::RefCell::new(map)))
    }

    fn argv(items: &[&str]) -> RuntimeValue {
        RuntimeValue::List(Rc::new(std::cell::RefCell::new(
            items
                .iter()
                .map(|item| RuntimeValue::Str((*item).into()))
                .collect(),
        )))
    }

    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());

    let spawned = host_result(
        &registry,
        "spawn",
        vec![spec(vec![
            // `exec` so the shell REPLACES itself with sleep (same PID): the
            // timeout's kill()+wait() then reaps the actual child instead of
            // orphaning a `sleep` grandchild (which nextest flags as a leak).
            ("argv", argv(&["/bin/sh", "-c", "exec sleep 1"])),
            ("capture_stdout", RuntimeValue::Bool(true)),
            ("timeout_ms", RuntimeValue::Int(1)),
        ])],
    )
    .unwrap();
    let RuntimeValue::Int(handle) = spawned else {
        panic!("expected process handle");
    };
    let error = host_result(&registry, "read_stdout", vec![RuntimeValue::Int(handle)])
        .expect_err("read_stdout should enforce elapsed process timeout");

    assert!(error
        .to_string()
        .contains("process.read_stdout: process timed out"));
    let error = host_result(&registry, "kill", vec![RuntimeValue::Int(handle)])
        .expect_err("timed out read should remove the process handle");
    assert!(error.to_string().contains("process.kill: unknown handle"));
}

#[test]
fn test_host_system_net_socket_exports_match_system_surface() {
    fn host_call(
        registry: &caap_core::HostServiceRegistry,
        export: &str,
        args: Vec<RuntimeValue>,
    ) -> RuntimeValue {
        let value = registry
            .export("net", export, caap_core::PhasePolicy::Runtime)
            .unwrap();
        let RuntimeValue::HostFunction(function) = value else {
            panic!("expected net.{export} host function");
        };
        (function.handler)(args).unwrap()
    }

    fn spec(entries: Vec<(&str, RuntimeValue)>) -> RuntimeValue {
        let mut map = indexmap::IndexMap::new();
        for (key, value) in entries {
            map.insert(MapKey::Str(key.into()), value);
        }
        RuntimeValue::Map(Rc::new(std::cell::RefCell::new(map)))
    }

    fn handles(items: &[i64]) -> RuntimeValue {
        RuntimeValue::List(Rc::new(std::cell::RefCell::new(
            items.iter().map(|item| RuntimeValue::Int(*item)).collect(),
        )))
    }

    let port = {
        let listener = match std::net::TcpListener::bind(("127.0.0.1", 0)) {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!(
                    "skipping net socket surface test: loopback bind is not permitted in this environment"
                );
                return;
            }
            Err(error) => panic!("failed to bind loopback test listener: {error}"),
        };
        listener.local_addr().unwrap().port()
    };
    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());

    let listener = host_call(
        &registry,
        "listen",
        vec![spec(vec![
            ("host", RuntimeValue::Str("127.0.0.1".into())),
            ("port", RuntimeValue::Int(port as i64)),
            ("backlog", RuntimeValue::Int(16)),
            ("reuse_addr", RuntimeValue::Bool(true)),
        ])],
    );
    let RuntimeValue::Int(listener_handle) = listener else {
        panic!("expected listener handle");
    };

    let client = host_call(
        &registry,
        "connect",
        vec![spec(vec![
            ("host", RuntimeValue::Str("127.0.0.1".into())),
            ("port", RuntimeValue::Int(port as i64)),
        ])],
    );
    let RuntimeValue::Int(client_handle) = client else {
        panic!("expected client socket handle");
    };

    let server = host_call(
        &registry,
        "accept",
        vec![RuntimeValue::Int(listener_handle)],
    );
    let RuntimeValue::Int(server_handle) = server else {
        panic!("expected server socket handle");
    };

    host_call(
        &registry,
        "write",
        vec![
            RuntimeValue::Int(client_handle),
            RuntimeValue::Str("ping".into()),
        ],
    );
    assert_eq!(
        host_call(
            &registry,
            "read",
            vec![RuntimeValue::Int(server_handle), RuntimeValue::Int(4)]
        ),
        RuntimeValue::Str("ping".into())
    );

    host_call(
        &registry,
        "write",
        vec![
            RuntimeValue::Int(server_handle),
            RuntimeValue::Str("pong".into()),
        ],
    );
    let RuntimeValue::List(events) = host_call(
        &registry,
        "poll",
        vec![handles(&[client_handle]), RuntimeValue::Int(1000)],
    ) else {
        panic!("expected net.poll list");
    };
    let events = events.borrow();
    assert!(!events.is_empty());
    let RuntimeValue::Map(event) = &events[0] else {
        panic!("expected net.poll event map");
    };
    assert_eq!(
        event.borrow().get(&MapKey::Str("handle".into())),
        Some(&RuntimeValue::Int(client_handle))
    );
    assert_eq!(
        event.borrow().get(&MapKey::Str("kind".into())),
        Some(&RuntimeValue::Str("socket".into()))
    );
    assert_eq!(
        event.borrow().get(&MapKey::Str("readable".into())),
        Some(&RuntimeValue::Bool(true))
    );
    drop(events);

    assert_eq!(
        host_call(
            &registry,
            "read",
            vec![RuntimeValue::Int(client_handle), RuntimeValue::Int(4)]
        ),
        RuntimeValue::Str("pong".into())
    );
    host_call(&registry, "close", vec![RuntimeValue::Int(client_handle)]);
    host_call(&registry, "close", vec![RuntimeValue::Int(server_handle)]);
    host_call(&registry, "close", vec![RuntimeValue::Int(listener_handle)]);
}

#[test]
fn test_host_system_fs_path_exports_match_system_surface() {
    fn host_call(
        registry: &caap_core::HostServiceRegistry,
        export: &str,
        args: Vec<RuntimeValue>,
    ) -> RuntimeValue {
        let value = registry
            .export("fs", export, caap_core::PhasePolicy::Runtime)
            .unwrap();
        let RuntimeValue::HostFunction(function) = value else {
            panic!("expected fs.{export} host function");
        };
        (function.handler)(args).unwrap()
    }

    fn path_value(path: &std::path::Path) -> RuntimeValue {
        RuntimeValue::Str(path.to_str().unwrap().into())
    }

    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());
    let root = std::env::temp_dir().join(format!(
        "caap-fs-path-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let nested = root.join("nested");
    let source = nested.join("source.txt");
    let copied = root.join("copied.txt");
    let renamed = root.join("renamed.txt");

    host_call(&registry, "create_dir_all", vec![path_value(&nested)]);
    host_call(
        &registry,
        "write_text",
        vec![path_value(&source), RuntimeValue::Str("hello".into())],
    );
    host_call(
        &registry,
        "append_text",
        vec![path_value(&source), RuntimeValue::Str(" world".into())],
    );
    assert_eq!(
        host_call(&registry, "read_text", vec![path_value(&source)]),
        RuntimeValue::Str("hello world".into())
    );
    assert_eq!(
        host_call(&registry, "is_file", vec![path_value(&source)]),
        RuntimeValue::Bool(true)
    );
    assert_eq!(
        host_call(&registry, "is_dir", vec![path_value(&nested)]),
        RuntimeValue::Bool(true)
    );

    let RuntimeValue::Map(metadata) = host_call(&registry, "metadata", vec![path_value(&source)])
    else {
        panic!("expected fs.metadata map");
    };
    let metadata = metadata.borrow();
    assert_eq!(
        metadata.get(&MapKey::Str("kind".into())),
        Some(&RuntimeValue::Str("file".into()))
    );
    assert_eq!(
        metadata.get(&MapKey::Str("is_file".into())),
        Some(&RuntimeValue::Bool(true))
    );
    assert_eq!(
        metadata.get(&MapKey::Str("is_dir".into())),
        Some(&RuntimeValue::Bool(false))
    );
    assert_eq!(
        metadata.get(&MapKey::Str("is_symlink".into())),
        Some(&RuntimeValue::Bool(false))
    );
    assert_eq!(
        metadata.get(&MapKey::Str("size".into())),
        Some(&RuntimeValue::Int(11))
    );
    drop(metadata);

    let RuntimeValue::List(entries) = host_call(&registry, "list_dir", vec![path_value(&root)])
    else {
        panic!("expected fs.list-dir list");
    };
    let entries = entries.borrow();
    let RuntimeValue::Map(first_entry) = &entries[0] else {
        panic!("expected fs.list-dir entry map");
    };
    assert_eq!(
        first_entry.borrow().get(&MapKey::Str("name".into())),
        Some(&RuntimeValue::Str("nested".into()))
    );
    drop(entries);

    assert_eq!(
        host_call(&registry, "canonicalize", vec![path_value(&source)]),
        RuntimeValue::Str(
            std::fs::canonicalize(&source)
                .unwrap()
                .to_str()
                .unwrap()
                .into()
        )
    );

    host_call(
        &registry,
        "copy_file",
        vec![path_value(&source), path_value(&copied)],
    );
    assert_eq!(std::fs::read_to_string(&copied).unwrap(), "hello world");
    host_call(
        &registry,
        "rename",
        vec![path_value(&copied), path_value(&renamed)],
    );
    assert!(renamed.exists());
    host_call(&registry, "remove_file", vec![path_value(&renamed)]);
    assert!(!renamed.exists());
    host_call(&registry, "remove_dir_all", vec![path_value(&root)]);
    assert!(!root.exists());
}

#[cfg(unix)]
#[test]
fn test_host_system_fs_list_dir_rejects_non_utf8_entry_names() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());
    let root = std::env::temp_dir().join(format!(
        "caap-host-fs-non-utf8-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let invalid_name = std::path::PathBuf::from(OsString::from_vec(b"bad-\xFF".to_vec()));
    std::fs::write(root.join(invalid_name), b"x").unwrap();

    let value = registry
        .export("fs", "list_dir", caap_core::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(function) = value else {
        panic!("expected fs.list-dir host function");
    };
    let error = (function.handler)(vec![RuntimeValue::Str(root.to_str().unwrap().into())])
        .expect_err("fs.list-dir should reject non-UTF-8 entry names")
        .to_string();

    std::fs::remove_dir_all(root).unwrap();
    assert!(error.contains("path component is not valid UTF-8"));
}

#[test]
fn test_host_system_fs_handle_exports_match_system_surface() {
    fn host_call(
        registry: &caap_core::HostServiceRegistry,
        export: &str,
        args: Vec<RuntimeValue>,
    ) -> RuntimeValue {
        let value = registry
            .export("fs", export, caap_core::PhasePolicy::Runtime)
            .unwrap();
        let RuntimeValue::HostFunction(function) = value else {
            panic!("expected fs.{export} host function");
        };
        (function.handler)(args).unwrap()
    }

    fn path_value(path: &std::path::Path) -> RuntimeValue {
        RuntimeValue::Str(path.to_str().unwrap().into())
    }

    fn spec(entries: Vec<(&str, RuntimeValue)>) -> RuntimeValue {
        let mut map = indexmap::IndexMap::new();
        for (key, value) in entries {
            map.insert(MapKey::Str(key.into()), value);
        }
        RuntimeValue::Map(Rc::new(std::cell::RefCell::new(map)))
    }

    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());
    let root = std::env::temp_dir().join(format!(
        "caap-fs-handle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let file_path = root.join("handle.txt");
    std::fs::create_dir_all(&root).unwrap();

    let handle = host_call(
        &registry,
        "open_file",
        vec![spec(vec![
            ("path", path_value(&file_path)),
            ("write", RuntimeValue::Bool(true)),
            ("read", RuntimeValue::Bool(true)),
            ("create", RuntimeValue::Bool(true)),
            ("truncate", RuntimeValue::Bool(true)),
        ])],
    );
    let RuntimeValue::Int(handle_id) = handle else {
        panic!("expected file handle id");
    };
    host_call(
        &registry,
        "file_write",
        vec![
            RuntimeValue::Int(handle_id),
            RuntimeValue::Str("first\nsecond".into()),
        ],
    );
    host_call(&registry, "file_flush", vec![RuntimeValue::Int(handle_id)]);
    assert_eq!(
        host_call(
            &registry,
            "file_seek",
            vec![RuntimeValue::Int(handle_id), RuntimeValue::Int(0)]
        ),
        RuntimeValue::Int(0)
    );
    assert_eq!(
        host_call(
            &registry,
            "file_read_line",
            vec![RuntimeValue::Int(handle_id)]
        ),
        RuntimeValue::Str("first\n".into())
    );
    assert_eq!(
        host_call(
            &registry,
            "file_read_all_text",
            vec![RuntimeValue::Int(handle_id)]
        ),
        RuntimeValue::Str("second".into())
    );
    let RuntimeValue::Map(metadata) = host_call(
        &registry,
        "file_metadata",
        vec![RuntimeValue::Int(handle_id)],
    ) else {
        panic!("expected file metadata map");
    };
    assert_eq!(
        metadata.borrow().get(&MapKey::Str("size".into())),
        Some(&RuntimeValue::Int(12))
    );
    host_call(&registry, "close_file", vec![RuntimeValue::Int(handle_id)]);

    let dir_handle = host_call(&registry, "open_dir", vec![path_value(&root)]);
    let RuntimeValue::Int(dir_handle_id) = dir_handle else {
        panic!("expected dir handle id");
    };
    let RuntimeValue::List(entries) = host_call(
        &registry,
        "dir_list",
        vec![RuntimeValue::Int(dir_handle_id)],
    ) else {
        panic!("expected dir-list result");
    };
    let RuntimeValue::Map(entry) = &entries.borrow()[0] else {
        panic!("expected dir entry map");
    };
    assert_eq!(
        entry.borrow().get(&MapKey::Str("name".into())),
        Some(&RuntimeValue::Str("handle.txt".into()))
    );
    host_call(
        &registry,
        "close_dir",
        vec![RuntimeValue::Int(dir_handle_id)],
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_host_system_libraries_are_not_ambient_globals() {
    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());
    let graph = parse("(path.basename \"/tmp/demo.caap\")").unwrap();
    let mut ev = Evaluator::new(graph);

    assert!(ev.run().is_err());
}
