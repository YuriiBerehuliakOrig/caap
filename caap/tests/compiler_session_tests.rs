/// Integration tests for compiler session, registry, and CTFE compiler API behavior.
use caap_core::{frontend::parse, RuntimeValue, Unit};
use std::rc::Rc;

mod common;

#[test]
fn test_compiler_host_new_session_is_bare() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    assert!(compiler.units().is_empty());
    assert!(!compiler.has_bootstrap_executions());
    assert!(compiler.registered_stages().is_empty());

    let err = compiler
        .compile(&mut unit)
        .expect_err("compile should require bootstrap stages");
    assert_eq!(
        err.to_string(),
        "compiler error: no compiler stages registered"
    );
    assert_eq!(compiler.diagnostics().len(), 1);
    assert_eq!(
        compiler.diagnostics()[0].code.as_deref(),
        Some("CAAP-COMPILER-001")
    );
}

#[test]
fn test_compiler_host_registers_system_libraries_explicitly() {
    let mut host = caap_core::CompilerHost::new();
    assert!(host.runtime_services().library_names().is_empty());

    host.register_default_runtime_system_libraries().unwrap();
    assert!(host.host_version() > 0);
    assert_eq!(
        host.runtime_services().library_names(),
        vec!["fs", "io", "net", "os", "path", "process", "rand", "time"]
    );

    let compiler = host.new_session();
    assert!(compiler.registered_stages().is_empty());
    assert!(compiler
        .host()
        .runtime_services()
        .export("path", "basename", caap_core::PhasePolicy::Runtime)
        .is_ok());
}

#[test]
fn test_compiler_session_loads_surface_text_template_through_cache() {
    let mut compiler = common::session();

    let first = compiler
        .load_surface_text_template("(int_add 1 2)", "inline")
        .unwrap();
    let second = compiler
        .load_surface_text_template("(int_add 1 2)", "inline")
        .unwrap();

    assert_eq!(first.key, second.key);
    assert!(!first.cache_hit);
    assert!(second.cache_hit);
    assert_eq!(first.template.unit_id, "inline");
    assert_eq!(second.template.ir.top_level_forms.len(), 1);
    assert_eq!(
        compiler.source_templates().artifact_cache().stats().misses,
        1
    );
    assert_eq!(compiler.source_templates().artifact_cache().stats().hits, 1);
    assert!(!compiler.has_bootstrap_executions());
    let events = compiler.events().by_kind("source.template.load").unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].target.as_deref(), Some("inline"));
    assert!(events[0]
        .metadata
        .contains(&("origin".to_string(), "inline".to_string())));
    assert!(events[0]
        .metadata
        .contains(&("cache_hit".to_string(), "false".to_string())));
    assert!(events[1]
        .metadata
        .contains(&("cache_hit".to_string(), "true".to_string())));
    assert!(events[0]
        .metadata
        .iter()
        .any(|(key, _)| key == "elapsed_ms"));
}

#[test]
fn test_compiler_session_loads_surface_path_template_through_cache() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-source-template-{}-{}.caap",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::write(&path, "(int_add 1 2)").unwrap();

    let first = compiler
        .load_surface_path_template(&path, "path_unit")
        .unwrap();
    let second = compiler
        .load_surface_path_template(&path, "path_unit")
        .unwrap();

    assert_eq!(first.key, second.key);
    assert!(!first.cache_hit);
    assert!(second.cache_hit);
    assert_eq!(first.template.unit_id, "path_unit");
    assert_eq!(
        compiler.source_templates().artifact_cache().stats().misses,
        1
    );
    assert_eq!(compiler.source_templates().artifact_cache().stats().hits, 1);
    assert_eq!(
        compiler.events().by_kind("source.template.load").unwrap()[0]
            .target
            .as_deref(),
        Some("path_unit")
    );

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_compiler_bridge_load_surface_unit_template_uses_path_identity() {
    let host = caap_core::CompilerHost::new();
    let compiler = host.new_session();
    let bridge =
        Rc::new(caap_core::compiler::CompilerBridgeValue::from_session_state(compiler.clone()));
    let plain_path = std::env::temp_dir().join(format!(
        "caap-surface-unit-plain-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(&plain_path, "(int_add 1 2)").unwrap();

    let plain_unit = bridge.load_surface_unit_template(&plain_path).unwrap();
    let expected_plain_id = std::fs::canonicalize(&plain_path)
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert_eq!(
        plain_unit.clone_unit_snapshot().unit_id(),
        expected_plain_id
    );

    let named_unit = bridge
        .load_surface_unit_template_with_unit_id(&plain_path, Some("demo.explicit"))
        .unwrap();
    assert_eq!(named_unit.clone_unit_snapshot().unit_id(), "demo.explicit");

    let _ = std::fs::remove_file(plain_path);
}

#[test]
fn test_compiler_surface_path_template_records_syntax_source_and_span_paths() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-source-span-path-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(&path, "(int_add 1 2)").unwrap();

    let artifact = compiler
        .load_surface_path_template(&path, "demo.path.syntax")
        .unwrap();
    let expected_path = std::fs::canonicalize(&path)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let top_id = artifact.template.ir.top_level_forms[0];
    let span = artifact
        .template
        .ir
        .source_spans
        .iter()
        .find(|(node_id, _)| *node_id == top_id)
        .map(|(_, span)| span)
        .expect("missing top-level source span");

    assert_eq!(artifact.template.syntax_state.language, "caap");
    assert_eq!(
        artifact.template.syntax_state.source_path.as_deref(),
        Some(expected_path.as_str())
    );
    assert!(artifact.template.syntax_state.source_fingerprint.is_some());
    assert_eq!(span.path.as_deref(), Some(expected_path.as_str()));

    let _ = std::fs::remove_file(path);
}

#[test]
fn test_compiler_surface_path_template_treats_syntax_import_as_plain_source() {
    let mut compiler = common::session();
    let path = std::env::temp_dir().join(format!(
        "caap-syntax-import-body-{}-{}.caap",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &path,
        r#"
          (module "demo.syntax.body")
          (syntax_import "demo.syntax")
          (int_add 1 2)
        "#,
    )
    .unwrap();

    let artifact = compiler
        .load_surface_path_template(&path, "demo.syntax.body")
        .unwrap();

    assert_eq!(artifact.template.unit_id, "demo.syntax.body");
    let _ = std::fs::remove_file(path);
}

#[test]
fn test_compiler_register_stage_allows_compile_to_record_unit() {
    let mut compiler = common::session();
    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("main", graph).unwrap();

    compiler.register_stage("compile_unit").unwrap();
    compiler.compile(&mut unit).unwrap();

    assert!(compiler.get_unit("main").unwrap().is_some());
    assert!(compiler.name_service().contains("main"));
    assert_eq!(compiler.diagnostics().len(), 0);
    assert_eq!(
        compiler.events().by_kind("compiler.compile").unwrap()[0]
            .target
            .as_deref(),
        Some("main")
    );
    assert_eq!(
        compiler.events().by_kind("compiler.unit.register").unwrap()[0]
            .target
            .as_deref(),
        Some("main")
    );
}

#[test]
fn test_compiler_registry_registers_and_looks_up_values() {
    let mut compiler = common::session();

    let value = compiler
        .register_value("demo.value", RuntimeValue::Int(42))
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(42));
    assert_eq!(
        compiler.lookup_registered_value("demo.value").unwrap(),
        Some(&RuntimeValue::Int(42))
    );
    assert_eq!(
        compiler.require_registered_value("demo.value").unwrap(),
        &RuntimeValue::Int(42)
    );
    assert_eq!(compiler.registry().registered_names(), vec!["demo.value"]);
    assert_eq!(compiler.registry().version(), 1);
    assert_eq!(
        compiler
            .events()
            .by_kind("compiler.registry.value.register")
            .unwrap()[0]
            .target
            .as_deref(),
        Some("demo.value")
    );

    let missing = compiler.require_registered_value("missing").unwrap_err();
    assert_eq!(
        missing.message(),
        "compiler registry does not contain \"missing\""
    );

    let duplicate = compiler
        .register_value("demo.value", RuntimeValue::Int(43))
        .unwrap_err();
    assert_eq!(
        duplicate.message(),
        "compiler registry already contains \"demo.value\""
    );
}

#[test]
fn test_compiler_registry_snapshots_registered_values() {
    let mut compiler = common::session();

    compiler
        .register_value("demo.fn", RuntimeValue::Str("callable".into()))
        .unwrap();
    assert_eq!(
        compiler.lookup_registered_value("demo.fn").unwrap(),
        Some(&RuntimeValue::Str("callable".into()))
    );

    let snapshot = compiler.registry_snapshot();
    compiler
        .register_value("demo.extra", RuntimeValue::Bool(true))
        .unwrap();
    compiler
        .register_value("demo.alpha", RuntimeValue::Bool(false))
        .unwrap();
    assert_eq!(
        compiler.registry().registered_names(),
        vec!["demo.alpha", "demo.extra", "demo.fn"]
    );
    assert!(compiler
        .lookup_registered_value("demo.extra")
        .unwrap()
        .is_some());

    compiler.restore_registry_snapshot(snapshot).unwrap();

    assert!(compiler
        .lookup_registered_value("demo.extra")
        .unwrap()
        .is_none());
    assert_eq!(compiler.registry().registered_names(), vec!["demo.fn"]);
    assert_eq!(
        compiler
            .events()
            .by_kind("compiler.registry.restore")
            .unwrap()[0]
            .metadata,
        vec![
            ("registered_count".to_string(), "1".to_string()),
            ("registry_version".to_string(), "1".to_string()),
        ]
    );
}

#[test]
fn test_compiler_registry_rejects_empty_names() {
    let mut compiler = common::session();

    assert_eq!(
        compiler
            .register_value("", RuntimeValue::Null)
            .expect_err("empty registry name should be rejected"),
        caap_core::CaapError::compiler("compiler registry names must be non-empty strings")
    );
    assert_eq!(
        compiler
            .lookup_registered_value("")
            .expect_err("empty registry lookup should be rejected"),
        caap_core::CaapError::compiler("compiler registry names must be non-empty strings")
    );
}

#[test]
fn test_ctfe_compiler_registry_builtins_mutate_session_registry() {
    let mut compiler = common::session();
    let graph = parse("(ctfe_compiler_register_value compiler \"demo.value\" 42)").unwrap();
    let unit = Unit::from_graph("registry_builtins", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Int(42));
    assert_eq!(
        compiler.lookup_registered_value("demo.value").unwrap(),
        Some(&RuntimeValue::Int(42))
    );
    assert_eq!(
        compiler
            .events()
            .by_kind("compiler.registry.value.register")
            .unwrap()[0]
            .target
            .as_deref(),
        Some("demo.value")
    );
}

#[test]
fn test_ctfe_compiler_lookup_value_supports_default_and_missing_error() {
    let mut compiler = common::session();
    compiler
        .register_value("demo.value", RuntimeValue::Str("stored".into()))
        .unwrap();
    let graph = parse(
        "(do
          (ctfe_compiler_lookup_value compiler \"missing\" \"fallback\")
          (ctfe_compiler_lookup_value compiler \"demo.value\"))",
    )
    .unwrap();
    let unit = Unit::from_graph("registry_lookup", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Str("stored".into()));

    let missing_graph = parse("(ctfe_compiler_lookup_value compiler \"missing\")").unwrap();
    let missing_unit = Unit::from_graph("registry_missing", missing_graph).unwrap();
    let err = compiler
        .evaluation()
        .evaluate(&missing_unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap_err();
    let caap_core::EvalSignal::Error(error) = err else {
        panic!("expected lookup error");
    };
    assert_eq!(
        error.message(),
        "compiler registry does not contain \"missing\""
    );
}

#[test]
fn test_ctfe_compiler_emit_event_builtin() {
    let mut compiler = common::session();
    let graph = parse(
        "(ctfe_compiler_emit_event compiler \"demo\" \"action\" \"hello\" (map_of \"k\" \"v\"))",
    )
    .unwrap();
    let unit = Unit::from_graph("registry_event", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Null);
    let event = &compiler.events().by_kind("demo.action").unwrap()[0];
    assert_eq!(event.message, "hello");
    assert_eq!(event.metadata, vec![("k".to_string(), "v".to_string())]);
}

#[test]
fn test_ctfe_compiler_stage_register_builtin_builds_stage_contract() {
    let mut compiler = common::session();
    let graph = parse(
        "(do
          (ctfe_compiler_stage_register compiler \"parse\" null \"compile\" (list_of \"source\") null (list_of \"caap_source\"))
          (ctfe_compiler_stage_register compiler \"lower\" (list_of \"parse\") \"compile\" (list_of \"compile_unit\") \"parse\" (list_of \"unit\")))",
    )
    .unwrap();
    let unit = Unit::from_graph("stage_register", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(value, RuntimeValue::Null);
    assert_eq!(compiler.registered_stages(), vec!["lower", "parse"]);
    assert_eq!(
        compiler
            .provider_registry()
            .default_stage_for_family("compile")
            .unwrap(),
        "parse"
    );
    assert_eq!(
        compiler
            .provider_registry()
            .stage_for_input_kind("caap_source")
            .unwrap(),
        "parse"
    );
    assert_eq!(
        compiler
            .provider_registry()
            .resolve_stage("compile_unit")
            .unwrap(),
        "lower"
    );
    assert_eq!(
        compiler
            .provider_registry()
            .restart_stage_for("lower")
            .unwrap(),
        "parse"
    );
    let lower = compiler
        .provider_registry()
        .stage_spec("lower")
        .unwrap()
        .unwrap();
    assert_eq!(lower.requires, vec!["parse"]);
    assert_eq!(lower.family_label.as_deref(), Some("compile"));
    assert_eq!(lower.input_kinds, vec!["unit"]);
}

#[test]
fn test_ctfe_compiler_stage_register_accepts_alias_and_restart_policy() {
    let mut compiler = common::session();
    let graph = parse(
        "(do
          (ctfe_compiler_stage_register compiler \"parse\")
          (ctfe_compiler_stage_register
            compiler
            \"validate\"
            (list_of \"parse\")
            null
            (list_of \"compile\")
            \"parse\"))",
    )
    .unwrap();
    let unit = Unit::from_graph("stage_alias_restart", graph).unwrap();

    compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    assert_eq!(
        compiler
            .provider_registry()
            .resolve_stage("compile")
            .unwrap(),
        "validate"
    );
    assert_eq!(
        compiler
            .provider_registry()
            .restart_stage_for("validate")
            .unwrap(),
        "parse"
    );
    assert_eq!(
        compiler
            .queries()
            .plan_query("compile", caap_core::PhasePolicy::CompileTime)
            .unwrap()
            .steps
            .iter()
            .map(|step| step.stage.as_str())
            .collect::<Vec<_>>(),
        vec!["parse", "validate"]
    );
}

#[test]
fn test_ctfe_compiler_provider_register_runs_later_query_callback() {
    let mut compiler = common::session();
    let bootstrap_graph = parse(
        "(do
          (ctfe_compiler_stage_register compiler \"analyze\" null \"analysis\")
          (ctfe_compiler_provider_register
            compiler
            \"event_provider\"
            \"analyze\"
            (lambda (ctx root)
              (ctfe_provider_diagnostics_note
                ctx
                root
                (host_value_kind (ctfe_provider_unit ctx))
                \"provider.ran\"))
            null
            (list_of \"emit_diagnostics\")
            (map_of
              \"reads\" (list_of \"unit\")
              \"writes\" (list_of \"diagnostics\")
              \"cache_scope\" \"unit\"
              \"resume_policy\" \"safe\"
              \"input_schema\" null)))",
    )
    .unwrap();
    let bootstrap_unit = Unit::from_graph("provider_register", bootstrap_graph).unwrap();
    compiler
        .evaluation()
        .evaluate(&bootstrap_unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let providers = compiler.provider_registry().ordered_providers();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].name, "event_provider");
    assert_eq!(providers[0].stage, "analyze");
    assert_eq!(providers[0].family.as_deref(), Some("analysis"));
    assert_eq!(
        providers[0].effect_tags.to_strings(),
        vec!["emit_diagnostics"]
    );
    assert_eq!(providers[0].reads, vec!["unit"]);
    assert_eq!(providers[0].writes, vec!["diagnostics"]);
    assert_eq!(
        providers[0].cache_scope,
        caap_core::QueryProviderCacheScope::Unit
    );
    assert_eq!(providers[0].input_schema, None);

    let graph = parse("(int_add 1 2)").unwrap();
    let mut unit = Unit::from_graph("provider_target", graph).unwrap();
    let plan = compiler
        .queries()
        .query("analyze", &mut unit, caap_core::PhasePolicy::CompileTime)
        .unwrap();

    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].provider_names, vec!["event_provider"]);
    assert_eq!(
        plan.steps[0].effect_tags.to_strings(),
        vec!["emit_diagnostics"]
    );
    let diagnostic = &compiler.diagnostics()[0];
    assert_eq!(diagnostic.message, "unit");
    assert_eq!(diagnostic.code.as_deref(), Some("provider.ran"));
}

#[test]
fn test_ctfe_compiler_provider_register_rejects_invalid_callback_arity() {
    let mut compiler = common::session();
    let graph = parse(
        "(do
          (ctfe_compiler_stage_register compiler \"analyze\" null \"analysis\")
          (ctfe_compiler_provider_register
            compiler
            \"bad_provider\"
            \"analyze\"
            (lambda (ctx root extra) null)))",
    )
    .unwrap();
    let unit = Unit::from_graph("provider_register_bad_arity", graph).unwrap();

    let error = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap_err()
        .to_string();

    assert!(error.contains("query provider \"bad_provider\" callback ABI is invalid"));
    assert!(error.contains("closure must accept (ctx) or (ctx root), got 3 parameters"));
    assert!(compiler.provider_registry().ordered_providers().is_empty());
}

#[test]
fn test_ctfe_compiler_lists_registered_stages_and_providers() {
    let mut compiler = common::session();
    let graph = parse(
        "(do
          (ctfe_compiler_stage_register compiler \"parse\" null \"compile\" (list_of \"source\"))
          (ctfe_compiler_provider_register
            compiler
            \"noop_provider\"
            \"source\"
            (lambda (ctx _root) null)
            null
            (list_of \"emit_events\"))
          (list_of
            (size (ctfe_compiler_list_stages compiler))
            (get (get (ctfe_compiler_list_stages compiler) 0) \"name\")
            (get (get (ctfe_compiler_list_stages compiler) 0) \"family\")
            (size (ctfe_compiler_list_providers compiler))
            (get (get (ctfe_compiler_list_providers compiler) 0) \"name\")
            (get (get (get (ctfe_compiler_list_providers compiler) 0) \"effects\") \"emits\")))",
    )
    .unwrap();
    let unit = Unit::from_graph("compiler_lists", graph).unwrap();

    let value = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1));
    assert_eq!(items[1], RuntimeValue::Str("parse".into()));
    assert_eq!(items[2], RuntimeValue::Str("compile".into()));
    assert_eq!(items[3], RuntimeValue::Int(1));
    assert_eq!(items[4], RuntimeValue::Str("noop_provider".into()));
    assert_eq!(
        items[5],
        RuntimeValue::Tuple(vec![RuntimeValue::Str("emit_events".into())].into())
    );
}
