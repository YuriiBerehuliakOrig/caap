/// Integration tests for compiler bootstrap execution, bootstrap images, and cross-unit links.
use caap_core::{frontend::parse, RuntimeValue, Unit};
use std::collections::BTreeMap;
use std::rc::Rc;

#[test]
fn test_bootstrap_execute_text_is_explicit_and_records_trace() {
    let host = caap_core::CompilerHost::new();
    let mut compiler = host.new_session();

    assert!(!compiler.has_bootstrap_executions());
    let value = compiler
        .bootstrap()
        .execute_text("(int_add 1 2)", "stdlib.bootstrap")
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(3));
    assert!(compiler.has_bootstrap_executions());
    assert_eq!(
        compiler.bootstrap_executions(),
        &["<inline:stdlib.bootstrap>".to_string()]
    );
    assert!(compiler.catalog().contains_unit("stdlib.bootstrap"));
    assert_eq!(compiler.bootstrap_trace().len(), 1);
    assert_eq!(compiler.bootstrap_trace()[0].action, "bootstrap.raw");
    assert!(compiler.bootstrap_trace()[0].succeeded);
    let event = compiler.events().by_kind("bootstrap.execute").unwrap()[0];
    assert_eq!(event.target.as_deref(), Some("stdlib.bootstrap"));
    assert!(event
        .metadata
        .contains(&("succeeded".to_string(), "true".to_string())));
    assert!(event
        .metadata
        .iter()
        .any(|(key, value)| key == "elapsed_ms" && value.parse::<f64>().is_ok()));
}

#[test]
fn test_bootstrap_file_memo_tracks_content_fingerprint() {
    let host = caap_core::CompilerHost::new();
    let compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-bootstrap-memo-{}-{}.caap",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let bridge =
        Rc::new(caap_core::compiler::CompilerBridgeValue::from_session_state(compiler.clone()));
    let compiler_value = RuntimeValue::HostObject(bridge.clone());

    std::fs::write(
        &path,
        "(ctfe_compiler_register_value compiler \"memo.value.v1\" 1)",
    )
    .unwrap();
    bridge
        .execute_bootstrap_file_with_capabilities(
            &path,
            Vec::<String>::new(),
            compiler_value.clone(),
        )
        .unwrap();
    assert_eq!(
        bridge.lookup_registered_value("memo.value.v1").unwrap(),
        Some(RuntimeValue::Int(1))
    );

    std::fs::write(
        &path,
        "(ctfe_compiler_register_value compiler \"memo.value.v2\" 2)",
    )
    .unwrap();
    bridge
        .execute_bootstrap_file_with_capabilities(&path, Vec::<String>::new(), compiler_value)
        .unwrap();
    std::fs::remove_file(&path).ok();

    assert_eq!(
        bridge.lookup_registered_value("memo.value.v2").unwrap(),
        Some(RuntimeValue::Int(2))
    );
}

#[test]
fn test_bootstrap_execute_text_reports_parse_failure_without_implicit_stage_registration() {
    let host = caap_core::CompilerHost::new();
    let mut compiler = host.new_session();

    let err = compiler
        .bootstrap()
        .execute_text("(", "broken.bootstrap")
        .expect_err("invalid bootstrap source should fail");

    assert!(!format!("{err:?}").is_empty());
    assert!(compiler.registered_stages().is_empty());
    assert_eq!(compiler.bootstrap_trace().len(), 1);
    assert!(!compiler.bootstrap_trace()[0].succeeded);
}

#[test]
fn test_bootstrap_execute_virtual_file_uses_explicit_vfs_source() {
    let host = caap_core::CompilerHost::new();
    let mut compiler = host.new_session();
    let mut vfs = caap_core::BootstrapVirtualFileSystem::new();
    vfs.insert("/stdlib/bootstrap.caap", "(int_add 5 6)")
        .unwrap();

    let value = compiler
        .bootstrap()
        .execute_virtual_file(&vfs, "stdlib/bootstrap.caap", "stdlib.vfs")
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(11));
    assert_eq!(
        compiler.bootstrap_executions(),
        &["<vfs:stdlib/bootstrap.caap>".to_string()]
    );
    assert!(compiler.catalog().contains_unit("stdlib.vfs"));
    assert_eq!(compiler.bootstrap_trace().len(), 1);
    assert_eq!(compiler.bootstrap_trace()[0].action, "bootstrap.vfs");
    assert_eq!(
        compiler.bootstrap_trace()[0].target,
        "vfs:stdlib/bootstrap.caap"
    );
    assert!(compiler.bootstrap_trace()[0].succeeded);
    assert!(vfs.contains("stdlib/bootstrap.caap"));
    assert_eq!(vfs.paths(), vec!["stdlib/bootstrap.caap"]);
}

#[test]
fn test_bootstrap_execute_virtual_file_reports_missing_source() {
    let host = caap_core::CompilerHost::new();
    let mut compiler = host.new_session();
    let vfs = caap_core::BootstrapVirtualFileSystem::new();

    let err = compiler
        .bootstrap()
        .execute_virtual_file(&vfs, "stdlib/missing.caap", "stdlib.missing")
        .expect_err("missing virtual bootstrap source should fail");

    assert!(format!("{err:?}").contains("virtual bootstrap file does not exist"));
    assert!(!compiler.has_bootstrap_executions());
    assert_eq!(compiler.bootstrap_trace().len(), 1);
    assert_eq!(compiler.bootstrap_trace()[0].action, "bootstrap.vfs");
    assert!(!compiler.bootstrap_trace()[0].succeeded);
}

#[test]
fn test_bootstrap_capability_graph_is_explicit_and_queryable() {
    let host = caap_core::CompilerHost::new();
    let mut compiler = host.new_session();

    assert_eq!(compiler.bootstrap_capabilities().version(), 0);
    assert!(!compiler
        .bootstrap_capabilities()
        .allows("stdlib.fs", "fs.read_text"));

    compiler
        .bootstrap()
        .grant_capabilities(
            "stdlib.fs",
            ["fs.read_text".to_string(), "path.basename".to_string()],
        )
        .unwrap();

    assert!(compiler
        .bootstrap()
        .require_capability("stdlib.fs", "fs.read_text")
        .is_ok());
    assert!(compiler
        .bootstrap_capabilities()
        .allows("stdlib.fs", "path.basename"));
    assert!(!compiler
        .bootstrap_capabilities()
        .allows("stdlib.fs", "process.args"));
    compiler
        .bootstrap()
        .grant_capabilities("stdlib.fs", ["sys.fs".to_string()])
        .unwrap();
    assert!(compiler
        .bootstrap_capabilities()
        .allows("stdlib.fs", "sys.fs.read"));
    assert!(compiler
        .bootstrap_capabilities()
        .allows("stdlib.fs", "sys.fs.write"));
    assert!(!compiler
        .bootstrap_capabilities()
        .allows("stdlib.fs", "sys.net.connect"));
    assert_eq!(
        compiler
            .bootstrap_capabilities()
            .capabilities_for("stdlib.fs"),
        vec![
            caap_core::CapabilityName::new("fs.read_text").unwrap(),
            caap_core::CapabilityName::new("path.basename").unwrap(),
            caap_core::CapabilityName::new("sys.fs").unwrap()
        ]
    );
    assert_eq!(
        compiler.bootstrap_capabilities().unit_ids(),
        vec!["stdlib.fs"]
    );

    compiler
        .bootstrap()
        .revoke_capability("stdlib.fs", "path.basename")
        .unwrap();
    assert!(!compiler
        .bootstrap_capabilities()
        .allows("stdlib.fs", "path.basename"));
    assert_eq!(
        compiler
            .bootstrap_capabilities()
            .capabilities_for("stdlib.fs"),
        vec![
            caap_core::CapabilityName::new("fs.read_text").unwrap(),
            caap_core::CapabilityName::new("sys.fs").unwrap()
        ]
    );
}

#[test]
fn test_bootstrap_execute_text_with_capabilities_records_grants() {
    let host = caap_core::CompilerHost::new();
    let mut compiler = host.new_session();

    let value = compiler
        .bootstrap()
        .execute_text_with_capabilities(
            "(int_add 2 8)",
            "stdlib.cap",
            ["fs.write_text".to_string(), "os.env_get".to_string()],
        )
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(10));
    assert!(compiler
        .bootstrap_capabilities()
        .allows("stdlib.cap", "fs.write_text"));
    assert!(compiler
        .bootstrap_capabilities()
        .allows("stdlib.cap", "os.env_get"));
    assert!(compiler.catalog().contains_unit("stdlib.cap"));
}

#[test]
fn test_bootstrap_image_store_snapshots_units_and_capabilities() {
    let host = caap_core::CompilerHost::new();
    let mut compiler = host.new_session();

    compiler
        .bootstrap()
        .execute_text_with_capabilities("(int_add 1 1)", "stdlib.one", ["fs.read_text".to_string()])
        .unwrap();
    let image = compiler.store_bootstrap_image("base").unwrap();
    assert_eq!(image.name, "base");
    assert_eq!(image.unit_ids(), vec!["stdlib.one"]);
    assert!(image.capabilities.allows("stdlib.one", "fs.read_text"));
    assert_eq!(compiler.bootstrap_images().image_names(), vec!["base"]);

    compiler
        .register_unit(Unit::empty("scratch.extra").unwrap())
        .unwrap();
    compiler
        .bootstrap()
        .grant_capability("scratch.extra", "process.args")
        .unwrap();
    assert!(compiler.catalog().contains_unit("scratch.extra"));

    compiler.restore_bootstrap_image("base").unwrap();
    assert!(compiler.catalog().contains_unit("stdlib.one"));
    assert!(!compiler.catalog().contains_unit("scratch.extra"));
    assert!(compiler
        .bootstrap_capabilities()
        .allows("stdlib.one", "fs.read_text"));
    assert!(!compiler
        .bootstrap_capabilities()
        .allows("scratch.extra", "process.args"));
}

#[test]
fn test_bootstrap_image_store_orders_units_deterministically() {
    let host = caap_core::CompilerHost::new();
    let mut compiler = host.new_session();

    compiler
        .register_unit(Unit::empty("z.unit").unwrap())
        .unwrap();
    compiler
        .register_unit(Unit::empty("a.unit").unwrap())
        .unwrap();

    let image = compiler.store_bootstrap_image("ordered_units").unwrap();
    assert_eq!(image.unit_ids(), vec!["a.unit", "z.unit"]);
}

#[test]
fn test_bootstrap_image_file_roundtrips_through_compiler_store() {
    let host = caap_core::CompilerHost::new();
    let mut compiler = host.new_session();
    compiler
        .bootstrap()
        .execute_text_with_capabilities(
            "(int_add 2 3)",
            "stdlib.persisted",
            ["fs.read_text".to_string()],
        )
        .unwrap();
    compiler.store_bootstrap_image("base").unwrap();
    let path = std::env::temp_dir().join(format!(
        "caap-bootstrap-image-{}-{}.json",
        std::process::id(),
        line!()
    ));

    compiler.save_bootstrap_image_file("base", &path).unwrap();
    assert_eq!(
        compiler.events().by_kind("bootstrap.image.save").unwrap()[0]
            .target
            .as_deref(),
        Some("base")
    );

    let mut restored = host.new_session();
    let image_name = restored.load_bootstrap_image_file(&path).unwrap();
    std::fs::remove_file(&path).ok();

    assert_eq!(image_name, "base");
    assert_eq!(restored.bootstrap_images().image_names(), vec!["base"]);
    restored.restore_bootstrap_image("base").unwrap();
    assert!(restored.catalog().contains_unit("stdlib.persisted"));
    assert!(restored
        .bootstrap_capabilities()
        .allows("stdlib.persisted", "fs.read_text"));
    assert_eq!(
        restored.events().by_kind("bootstrap.image.load").unwrap()[0]
            .target
            .as_deref(),
        Some("base")
    );
}

#[test]
fn test_bootstrap_image_persists_compiler_fact_schema_and_base_semantic_entries() {
    let host = caap_core::CompilerHost::new();
    let mut compiler = host.new_session();
    let unit = Unit::from_graph(
        "bootstrap_image_compiler_state",
        parse(
            "(do
              (ctfe_compiler_fact_schema_type_bridge_register compiler \"demo_string\" \"string\")
              (ctfe_compiler_fact_schema_register compiler \"demo.fact\" \"demo_string\" false \"demo fact\")
              (ctfe_compiler_register_base_semantic_entries
                compiler
                (list_of
                  (map_of \"name\" \"int_add\" \"source\" \"builtin\" \"phase_policy\" \"dual\")
                  (map_of \"name\" \"if\" \"source\" \"builtin\" \"phase_policy\" \"dual\"))))",
        )
        .unwrap(),
    )
    .unwrap();
    compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let image = compiler.store_bootstrap_image("compiler_state").unwrap();
    assert_eq!(
        image
            .fact_schema
            .lookup("demo.fact")
            .unwrap()
            .unwrap()
            .bridge_name,
        "string"
    );
    let image_entry_names: Vec<_> = image
        .base_semantic_entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(image_entry_names, vec!["if", "int_add"]);

    let path = std::env::temp_dir().join(format!(
        "caap-bootstrap-image-compiler-state-{}-{}.json",
        std::process::id(),
        line!()
    ));
    compiler
        .save_bootstrap_image_file("compiler_state", &path)
        .unwrap();

    let mut restored = host.new_session();
    restored.load_bootstrap_image_file(&path).unwrap();
    std::fs::remove_file(&path).ok();
    restored.restore_bootstrap_image("compiler_state").unwrap();

    let restored_schema = restored
        .fact_schema()
        .lookup("demo.fact")
        .unwrap()
        .cloned()
        .expect("expected restored fact schema");
    assert_eq!(restored_schema.type_label, "demo_string");
    assert_eq!(restored_schema.bridge_name, "string");
    assert_eq!(restored_schema.description.as_deref(), Some("demo fact"));
    let restored_entry_names: Vec<_> = restored
        .base_semantic_entries()
        .into_iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(restored_entry_names, vec!["if", "int_add"]);
}

#[test]
fn test_bootstrap_image_file_defaults_missing_compiler_bridge_state() {
    let image_file = caap_core::BootstrapImageFile::from_json_str(
        r#"{
          "format_name": "caap_bootstrap_image",
          "format_version": 1,
          "image": {
            "name": "legacy",
            "units": [],
            "capabilities": {
              "grants": {},
              "version": 0
            },
            "session_version": 0
          }
        }"#,
    )
    .unwrap();

    assert_eq!(image_file.image.name, "legacy");
    assert!(image_file.image.fact_schema.schemas().is_empty());
    assert!(image_file.image.base_semantic_entries.is_empty());
}

#[test]
fn test_bootstrap_image_file_load_can_require_trusted_fingerprint() {
    let host = caap_core::CompilerHost::new();
    let mut compiler = host.new_session();
    compiler
        .bootstrap()
        .execute_text("(int_add 3 4)", "stdlib.trusted")
        .unwrap();
    compiler.store_bootstrap_image("trusted_base").unwrap();
    let path = std::env::temp_dir().join(format!(
        "caap-bootstrap-trusted-image-{}-{}.json",
        std::process::id(),
        line!()
    ));
    compiler
        .save_bootstrap_image_file("trusted_base", &path)
        .unwrap();

    let mut rejected = host.new_session();
    let err = rejected
        .load_trusted_bootstrap_image_file(&path, &caap_core::BootstrapImageTrustPolicy::new())
        .expect_err("empty trust policy should reject persisted bootstrap image");
    assert!(err.to_string().contains("not trusted"));

    let mut policy = caap_core::BootstrapImageTrustPolicy::new();
    let trusted_fingerprint = policy.trust_file(&path).unwrap();
    assert_eq!(
        policy.trusted_fingerprints(),
        vec![trusted_fingerprint.as_str()]
    );

    let mut restored = host.new_session();
    let image_name = restored
        .load_trusted_bootstrap_image_file(&path, &policy)
        .unwrap();
    std::fs::remove_file(&path).ok();

    assert_eq!(image_name, "trusted_base");
    assert_eq!(
        restored.bootstrap_images().image_names(),
        vec!["trusted_base"]
    );
    assert!(
        restored.events().by_kind("bootstrap.image.load").unwrap()[0]
            .metadata
            .contains(&("trusted".to_string(), "true".to_string()))
    );
}

#[test]
fn test_bootstrap_image_trust_policy_rotates_fingerprints() {
    let mut policy = caap_core::BootstrapImageTrustPolicy::new();
    policy
        .trust_fingerprint("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap();
    policy
        .trust_fingerprint("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        .unwrap();

    assert!(policy.is_trusted_fingerprint(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    ));
    assert!(policy
        .revoke_fingerprint("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap());
    assert!(!policy.is_trusted_fingerprint(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    ));
    assert!(!policy
        .revoke_fingerprint("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap());

    policy
        .replace_trusted_fingerprints([
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_string(),
        ])
        .unwrap();
    assert_eq!(
        policy.trusted_fingerprints(),
        vec!["cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"]
    );

    policy.clear();
    assert!(policy.trusted_fingerprints().is_empty());
}

#[test]
fn test_bootstrap_image_trust_policy_rejects_malformed_fingerprints() {
    let mut policy = caap_core::BootstrapImageTrustPolicy::new();

    let error = policy.trust_fingerprint(" abc ").unwrap_err().to_string();
    assert!(error.contains("must not contain whitespace"));

    let error = policy.revoke_fingerprint("").unwrap_err().to_string();
    assert!(error.contains("must be non-empty"));
}

#[test]
fn test_bootstrap_execute_file_records_resolved_path_trace() {
    let host = caap_core::CompilerHost::new();
    let mut compiler = host.new_session();
    let path = std::env::temp_dir().join(format!(
        "caap-bootstrap-{}-{}.caap",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, "(int_add 1 2)").unwrap();
    let resolved = std::fs::canonicalize(&path)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let value = compiler
        .bootstrap()
        .execute_file(&path, "file.bootstrap")
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(3));
    assert_eq!(
        compiler.bootstrap_executions(),
        std::slice::from_ref(&resolved)
    );
    assert_eq!(compiler.bootstrap_trace().len(), 1);
    assert_eq!(compiler.bootstrap_trace()[0].target, resolved);
    assert!(compiler.catalog().contains_unit("file.bootstrap"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_cross_unit_graph_resolves_projected_link_bindings() {
    let mut dependency = Unit::empty("dep").unwrap();
    dependency
        .semantics_mut()
        .unwrap()
        .define_symbol(
            caap_core::SymbolEntry::new(
                "public_value",
                caap_core::SymbolKind::TopLevel,
                caap_core::PhasePolicy::Runtime,
                None,
            )
            .unwrap(),
        )
        .unwrap();

    let mut main = Unit::empty("main").unwrap();
    main.add_link_binding(
        caap_core::LinkBinding::new("dep", "public_value", "local_value").unwrap(),
    )
    .unwrap();

    let mut units = BTreeMap::new();
    units.insert("main".to_string(), main);
    units.insert("dep".to_string(), dependency);
    let graph = caap_core::CrossUnitGraph::new(&units);

    let binding = graph
        .resolve_local("main", "local_value")
        .unwrap()
        .expect("binding should resolve");
    let symbol = graph
        .resolve_binding(binding)
        .unwrap()
        .expect("source symbol should resolve");

    assert_eq!(binding.source_unit, "dep");
    assert_eq!(symbol.name, "public_value");
}

#[test]
fn test_cross_unit_graph_missing_endpoint_is_degraded_none() {
    let mut main = Unit::empty("main").unwrap();
    main.add_link_binding(caap_core::LinkBinding::new("missing", "value", "local_value").unwrap())
        .unwrap();

    let mut units = BTreeMap::new();
    units.insert("main".to_string(), main);
    let graph = caap_core::CrossUnitGraph::new(&units);
    let binding = graph
        .resolve_local("main", "local_value")
        .unwrap()
        .expect("binding should exist");

    assert!(graph.resolve_binding(binding).unwrap().is_none());
}

#[test]
fn test_unit_link_state_validates_public_names() {
    let binding = caap_core::LinkBinding::new("dep", "value", "local").unwrap();
    let state =
        caap_core::UnitLinkState::new("main", [binding], ["z".to_string(), "a".to_string()])
            .unwrap();

    assert_eq!(state.public_names, vec!["a".to_string(), "z".to_string()]);
    assert!(caap_core::UnitLinkState::new("main", [], ["".to_string()]).is_err());
    assert!(caap_core::UnitLinkState::new("main", [], ["a".to_string(), "a".to_string()]).is_err());
}
