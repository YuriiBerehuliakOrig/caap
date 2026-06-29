/// Integration tests for host function values, host service registry, and host builtins.
use caap_core::{
    frontend::parse,
    graph::GraphBuilder,
    ir::{IrLiteralData, NodeId},
    values::Environment,
    Evaluator, PhasePolicy, RuntimeValue, Unit,
};
use std::rc::Rc;

// ── helpers ───────────────────────────────────────────────────────────────────

fn lit_int(v: i64) -> IrLiteralData {
    IrLiteralData::Int(v)
}

trait TestGraphBuilderExt {
    fn name(&mut self, identifier: impl Into<String>) -> NodeId;
    fn literal(&mut self, value: IrLiteralData) -> NodeId;
    fn call(&mut self, callee: NodeId, args: Vec<NodeId>) -> NodeId;
}

impl TestGraphBuilderExt for GraphBuilder {
    fn name(&mut self, identifier: impl Into<String>) -> NodeId {
        self.try_name(identifier)
            .expect("test graph name must be valid")
    }

    fn literal(&mut self, value: IrLiteralData) -> NodeId {
        self.try_literal(value)
            .expect("test graph literal must be valid")
    }

    fn call(&mut self, callee: NodeId, args: Vec<NodeId>) -> NodeId {
        self.try_call(callee, args)
            .expect("test graph call must reference existing nodes")
    }
}

fn eval_one(b: &mut GraphBuilder, root_id: u32) -> RuntimeValue {
    b.graph.root_id = root_id;
    let env = Environment::new(None);
    let mut ev = Evaluator::new(std::mem::take(&mut b.graph));
    ev.eval(root_id, &env).expect("evaluation failed")
}

#[test]
fn test_apply() {
    // (apply (lambda (x) (int-add x 1)) (list_of 5)) → 6
    let mut b = GraphBuilder::new();
    let params_callee = b.name("__params__");
    let param_x = b.name("x");
    let params = b.call(params_callee, vec![param_x]);
    let add_fn = b.name("int_add");
    let body_x = b.name("x");
    let one = b.literal(lit_int(1));
    let body = b.call(add_fn, vec![body_x, one]);
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, body]);
    let apply_fn = b.name("apply");
    let list_fn = b.name("list_of");
    let five = b.literal(lit_int(5));
    let args_list = b.call(list_fn, vec![five]);
    let call_id = b.call(apply_fn, vec![lam, args_list]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(6));
}

#[test]
fn test_host_function_value_invokes_explicit_host_callable() {
    let mut b = GraphBuilder::new();
    let host_name = b.name("host_inc");
    let arg = b.literal(lit_int(41));
    let call_id = b.call(host_name, vec![arg]);
    let env = Environment::new(None);
    Environment::define(
        &env,
        "host_inc",
        RuntimeValue::HostFunction(Rc::new(
            caap_core::HostFunction::new(
                "host_inc",
                1,
                Some(1),
                Box::new(|args| {
                    let value = caap_core::require_int_strict(&args[0], "host_inc")?;
                    Ok(RuntimeValue::Int(value + 1))
                }),
            )
            .unwrap(),
        )),
    );
    let graph = std::mem::take(&mut b.graph);
    let mut ev = Evaluator::new(graph);

    assert_eq!(ev.eval(call_id, &env).unwrap(), RuntimeValue::Int(42));
}

#[test]
fn test_effect_scope_blocks_default_impure_host_function() {
    let graph = parse("(effect_scope (list_of) (host_inc 41))").unwrap();
    let root = graph.top_level_form_ids()[0];
    let env = Environment::new(None);
    Environment::define(
        &env,
        "host_inc",
        RuntimeValue::HostFunction(Rc::new(
            caap_core::HostFunction::new(
                "host_inc",
                1,
                Some(1),
                Box::new(|args| {
                    let value = caap_core::require_int_strict(&args[0], "host_inc")?;
                    Ok(RuntimeValue::Int(value + 1))
                }),
            )
            .unwrap(),
        )),
    );
    let mut ev = Evaluator::new(graph);

    let error = ev
        .eval(root, &env)
        .expect_err("pure effect scope must block default-impure host functions");
    assert!(
        error.to_string().contains(
            "host function host_inc requires effect(s) [impure] outside active effect scope []"
        ),
        "unexpected diagnostic: {error}"
    );
}

#[test]
fn test_effect_scope_allows_pure_host_function_metadata() {
    let graph = parse("(effect_scope (list_of) (host_inc 41))").unwrap();
    let root = graph.top_level_form_ids()[0];
    let env = Environment::new(None);
    Environment::define(
        &env,
        "host_inc",
        RuntimeValue::HostFunction(Rc::new(
            caap_core::HostFunction::new(
                "host_inc",
                1,
                Some(1),
                Box::new(|args| {
                    let value = caap_core::require_int_strict(&args[0], "host_inc")?;
                    Ok(RuntimeValue::Int(value + 1))
                }),
            )
            .unwrap()
            .with_effect_policy(caap_core::semantic::EffectPolicy::pure()),
        )),
    );
    let mut ev = Evaluator::new(graph);

    assert_eq!(ev.eval(root, &env).unwrap(), RuntimeValue::Int(42));
}

#[test]
fn test_host_function_value_checks_arity() {
    let mut b = GraphBuilder::new();
    let host_name = b.name("host_zero");
    let arg = b.literal(lit_int(1));
    let call_id = b.call(host_name, vec![arg]);
    let env = Environment::new(None);
    Environment::define(
        &env,
        "host_zero",
        RuntimeValue::HostFunction(Rc::new(
            caap_core::HostFunction::new(
                "host_zero",
                0,
                Some(0),
                Box::new(|_| Ok(RuntimeValue::Null)),
            )
            .unwrap(),
        )),
    );
    let graph = std::mem::take(&mut b.graph);
    let mut ev = Evaluator::new(graph);

    assert!(ev.eval(call_id, &env).is_err());
}

#[test]
fn test_runtime_evaluator_rejects_compile_time_builtin() {
    let graph = parse("(ctfe_compiler_lookup_value compiler \"missing\")").unwrap();
    let mut ev = Evaluator::new(graph);

    let err = ev
        .run()
        .expect_err("runtime evaluator must reject CTFE builtin");

    assert!(
        err.to_string()
            .contains("builtin ctfe_compiler_lookup_value is not available in phase runtime"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_compile_time_evaluator_rejects_runtime_host_function() {
    let mut b = GraphBuilder::new();
    let host_name = b.name("runtime_only");
    let call_id = b.call(host_name, vec![]);
    let env = Environment::new(None);
    Environment::define(
        &env,
        "runtime_only",
        RuntimeValue::HostFunction(Rc::new(
            caap_core::HostFunction::new(
                "runtime_only",
                0,
                Some(0),
                Box::new(|_| Ok(RuntimeValue::Null)),
            )
            .unwrap(),
        )),
    );
    let graph = std::mem::take(&mut b.graph);
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);

    let err = ev
        .eval(call_id, &env)
        .expect_err("compile-time evaluator must reject runtime host function");

    assert!(
        err.to_string()
            .contains("host function runtime_only is not available in phase compile_time"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_compile_time_evaluator_allows_dual_language_builtin() {
    let graph = parse("(int_add 1 2)").unwrap();
    let mut ev = Evaluator::with_phase(graph, PhasePolicy::CompileTime);

    assert_eq!(ev.run().unwrap(), RuntimeValue::Int(3));
}

fn test_host_export_metadata(
    _name: &str,
    min_arity: usize,
    max_arity: Option<usize>,
    params: Vec<caap_core::HostExportParameter>,
    result: &str,
) -> caap_core::HostExportMetadata {
    caap_core::HostExportMetadata {
        module: Some("test.host".to_string()),
        policy: "none".to_string(),
        effect: "impure".to_string(),
        kind: "function".to_string(),
        capability_kind: Some("test.host".to_string()),
        signature: caap_core::HostExportSignature {
            params,
            result: result.to_string(),
        },
        min_arity,
        max_arity,
    }
}

#[test]
fn test_host_service_registry_exports_explicit_runtime_function() {
    let mut registry = caap_core::HostServiceRegistry::new();
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());
    registry
        .register_function_with_metadata(
            "math",
            "inc",
            caap_core::PhasePolicy::Runtime,
            caap_core::HostFunction::new(
                "math.inc",
                1,
                Some(1),
                Box::new(|args| {
                    let value = caap_core::require_int_strict(&args[0], "math.inc")?;
                    Ok(RuntimeValue::Int(value + 1))
                }),
            )
            .unwrap(),
            test_host_export_metadata(
                "inc",
                1,
                Some(1),
                vec![caap_core::HostExportParameter::new("value", "int")],
                "int",
            ),
        )
        .unwrap();

    assert_eq!(registry.library_names(), vec!["math"]);
    let exported = registry
        .export("math", "inc", caap_core::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(function) = exported else {
        panic!("expected exported host function");
    };
    assert_eq!(function.phase_policy, PhasePolicy::Runtime);
}

#[test]
fn test_host_service_registry_gates_custom_exports_by_metadata_capability() {
    let build_registry = || {
        let mut registry = caap_core::HostServiceRegistry::new();
        registry
            .register_function_with_metadata(
                "math",
                "inc",
                caap_core::PhasePolicy::Runtime,
                caap_core::HostFunction::new(
                    "math.inc",
                    1,
                    Some(1),
                    Box::new(|args| {
                        let value = caap_core::require_int_strict(&args[0], "math.inc")?;
                        Ok(RuntimeValue::Int(value + 1))
                    }),
                )
                .unwrap(),
                test_host_export_metadata(
                    "inc",
                    1,
                    Some(1),
                    vec![caap_core::HostExportParameter::new("value", "int")],
                    "int",
                ),
            )
            .unwrap();
        registry
    };

    let mut registry = build_registry();
    registry
        .allow_only_capabilities(["test.host".to_string()])
        .unwrap();
    assert!(registry
        .export("math", "inc", caap_core::PhasePolicy::Runtime)
        .is_ok());

    let mut registry = build_registry();
    registry
        .allow_only_capabilities(["math.inc".to_string()])
        .unwrap();
    let error = registry
        .export("math", "inc", caap_core::PhasePolicy::Runtime)
        .expect_err("custom export must use metadata capability, not library.export fallback");
    assert_eq!(
        error.message(),
        "host capability denied: math.inc (requires test.host)"
    );
}

#[test]
fn test_host_service_registry_rejects_invalid_metadata_capability() {
    let mut metadata = test_host_export_metadata(
        "inc",
        1,
        Some(1),
        vec![caap_core::HostExportParameter::new("value", "int")],
        "int",
    );
    metadata.capability_kind = Some("test..host".to_string());
    let mut registry = caap_core::HostServiceRegistry::new();
    let error = registry
        .register_function_with_metadata(
            "math",
            "inc",
            caap_core::PhasePolicy::Runtime,
            caap_core::HostFunction::new(
                "math.inc",
                1,
                Some(1),
                Box::new(|_| Ok(RuntimeValue::Null)),
            )
            .unwrap(),
            metadata,
        )
        .expect_err("invalid metadata capability should fail at registration");

    assert!(error
        .message()
        .contains("host service metadata capability for math.inc is invalid"));
}

#[test]
fn test_host_service_registry_rejects_metadata_signature_contract_mismatch() {
    let metadata = test_host_export_metadata(
        "inc",
        1,
        Some(1),
        vec![
            caap_core::HostExportParameter::new("value", "int"),
            caap_core::HostExportParameter::new("extra", "int"),
        ],
        "int",
    );
    let mut registry = caap_core::HostServiceRegistry::new();
    let error = registry
        .register_function_with_metadata(
            "math",
            "inc",
            caap_core::PhasePolicy::Runtime,
            caap_core::HostFunction::new(
                "math.inc",
                1,
                Some(1),
                Box::new(|_| Ok(RuntimeValue::Null)),
            )
            .unwrap(),
            metadata,
        )
        .expect_err("metadata signature must not exceed function arity");

    assert!(error
        .message()
        .contains("host service metadata signature for math.inc has more parameters"));
}

#[test]
fn test_host_service_registry_enforces_phase() {
    let mut registry = caap_core::HostServiceRegistry::new();
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());
    registry
        .register_function_with_metadata(
            "compile",
            "emit",
            caap_core::PhasePolicy::CompileTime,
            caap_core::HostFunction::new(
                "compile.emit",
                0,
                Some(0),
                Box::new(|_| Ok(RuntimeValue::Null)),
            )
            .unwrap(),
            test_host_export_metadata("emit", 0, Some(0), Vec::new(), "null"),
        )
        .unwrap();

    assert!(registry
        .export("compile", "emit", caap_core::PhasePolicy::Runtime)
        .is_err());
    assert!(registry
        .export("compile", "emit", caap_core::PhasePolicy::CompileTime)
        .is_ok());
}

#[test]
fn test_host_service_registry_enforces_capability_policy() {
    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry
        .allow_only_capabilities(["sys.time".to_string()])
        .unwrap();

    assert!(registry
        .export("path", "basename", caap_core::PhasePolicy::Runtime)
        .is_ok());
    assert!(registry
        .export("time", "unix_millis", caap_core::PhasePolicy::Runtime)
        .is_ok());
    assert!(registry
        .export("time", "now_unix_ns", caap_core::PhasePolicy::Runtime)
        .is_ok());

    let err = registry
        .export("fs", "write_text", caap_core::PhasePolicy::Runtime)
        .expect_err("fs.write_text should require an explicit capability");
    assert_eq!(
        err.message(),
        "host capability denied: fs.write_text (requires sys.fs.write)"
    );
}

#[test]
fn test_host_capability_policy_rejects_malformed_names() {
    let error = caap_core::HostCapabilityPolicy::allow_only(["fs..read".to_string()])
        .unwrap_err()
        .to_string();

    assert!(error.contains("segments must be non-empty"));
}

#[test]
fn test_host_capability_policy_matches_only_exact_exports() {
    let policy =
        caap_core::HostCapabilityPolicy::allow_only(["sys.fs.read_text".to_string()]).unwrap();

    assert!(policy.allows_capability(Some("sys.fs.read_text")));
    assert!(!policy.allows_capability(Some("sys.fs.write_text")));
}

#[test]
fn test_host_capability_policy_rejects_wildcard_grants() {
    let prefix_error = caap_core::HostCapabilityPolicy::allow_only(["sys.fs.*".to_string()])
        .unwrap_err()
        .to_string();
    assert!(prefix_error.contains("wildcard grants are not supported"));

    let global_error = caap_core::HostCapabilityPolicy::allow_only(["*".to_string()])
        .unwrap_err()
        .to_string();
    assert!(global_error.contains("wildcard grants are not supported"));
}

#[test]
fn test_host_service_registry_registers_explicit_system_libraries() {
    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());

    assert_eq!(
        registry.library_names(),
        vec!["fs", "io", "net", "os", "path", "process", "rand", "time"]
    );

    let basename = registry
        .export("path", "basename", caap_core::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(basename) = basename else {
        panic!("expected host function");
    };
    assert_eq!(
        (basename.handler)(vec![RuntimeValue::Str("/tmp/demo.caap".into())]).unwrap(),
        RuntimeValue::Str("demo.caap".into())
    );
    assert!((basename.handler)(vec![RuntimeValue::Str("/".into())])
        .unwrap_err()
        .to_string()
        .contains("path has no final component"));

    let dirname = registry
        .export("path", "dirname", caap_core::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(dirname) = dirname else {
        panic!("expected host function");
    };
    assert_eq!(
        (dirname.handler)(vec![RuntimeValue::Str("demo.caap".into())]).unwrap(),
        RuntimeValue::Str(".".into())
    );
    assert_eq!(
        (dirname.handler)(vec![RuntimeValue::Str("/".into())]).unwrap(),
        RuntimeValue::Str("/".into())
    );

    let exists = registry
        .export("fs", "exists", caap_core::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(exists) = exists else {
        panic!("expected host function");
    };
    let manifest_path = format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"));
    assert_eq!(
        (exists.handler)(vec![RuntimeValue::Str(manifest_path.into())]).unwrap(),
        RuntimeValue::Bool(true)
    );

    // Dual-phase: time exports are visible at compile time too (the per-phase
    // HostSystemPolicy, not a phase gate, decides what calls are allowed).
    assert!(registry
        .export("time", "unix_millis", caap_core::PhasePolicy::CompileTime)
        .is_ok());
    assert!(registry
        .export("time", "now_unix_ns", caap_core::PhasePolicy::CompileTime)
        .is_ok());
}

/// The capability backstop installed on the runtime state re-checks the policy
/// inside `dispatch`, so tightening capabilities *after* a function is bound
/// stops it being callable immediately — defense in depth behind bind-time
/// gating, which only runs when the export is first resolved.
#[test]
fn test_capability_gate_re_checks_policy_at_dispatch_time() {
    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());

    // Bind fs.exists while the filesystem capability is granted.
    let exists = registry
        .export("fs", "exists", caap_core::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(exists) = exists else {
        panic!("expected host function");
    };
    let manifest = format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"));
    assert_eq!(
        (exists.handler)(vec![RuntimeValue::Str(manifest.clone().into())]).unwrap(),
        RuntimeValue::Bool(true)
    );

    // Revoke all capabilities after binding. The already-held handler must now be
    // denied at dispatch by the backstop, not silently keep working.
    registry
        .allow_only_capabilities(["sys.net".to_string()])
        .unwrap();
    let denied = (exists.handler)(vec![RuntimeValue::Str(manifest.into())]).unwrap_err();
    assert!(
        denied
            .to_string()
            .contains("host capability denied: fs.exists"),
        "expected a capability denial, got {denied}"
    );
}

/// A typed runtime failure keeps its `SysErrorKind` classification as the
/// evaluation error's category, so diagnostics and tooling can see the kind
/// (e.g. `not_found`) instead of only a flattened message.
#[test]
fn test_sys_error_kind_surfaces_as_evaluation_category() {
    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());

    let read_text = registry
        .export("fs", "read_text", caap_core::PhasePolicy::Runtime)
        .unwrap();
    let RuntimeValue::HostFunction(read_text) = read_text else {
        panic!("expected host function");
    };
    let missing = format!("{}/no-such-caap-file-xyz", env!("CARGO_MANIFEST_DIR"));
    let error = (read_text.handler)(vec![RuntimeValue::Str(missing.into())])
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("[not_found]"),
        "expected a not_found category, got {error}"
    );
}

#[test]
fn test_host_service_registry_owns_system_export_metadata() {
    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());

    let fs = registry.library("fs").unwrap().unwrap();
    let read_text = fs.export("read_text").unwrap().unwrap();
    assert_eq!(read_text.metadata.module.as_deref(), Some("sys.fs"));
    assert_eq!(read_text.metadata.policy, "fs_read_path");
    assert_eq!(read_text.metadata.effect, "impure");
    assert!(!read_text.metadata.is_pure());
    assert_eq!(read_text.metadata.kind, "function");
    assert_eq!(
        read_text.metadata.capability_kind.as_deref(),
        Some("sys.fs")
    );
    assert_eq!(read_text.metadata.signature.result, "string");
    assert_eq!(read_text.metadata.signature.params.len(), 1);
    assert_eq!(read_text.metadata.signature.params[0].name, "path");
    assert_eq!(read_text.metadata.signature.params[0].type_name, "string");
    assert_eq!(read_text.metadata.min_arity, 1);
    assert_eq!(read_text.metadata.max_arity, Some(1));
    assert!(!read_text.metadata.is_variadic());

    assert!(registry.library("env").unwrap().is_none());
    let os = registry.library("os").unwrap().unwrap();
    let env_get = os.export("env_get").unwrap().unwrap();
    assert_eq!(env_get.metadata.module.as_deref(), Some("sys.env"));
    assert_eq!(env_get.metadata.policy, "env_get");
    assert_eq!(env_get.metadata.capability_kind.as_deref(), Some("sys.env"));
    assert_eq!(env_get.metadata.signature.params[0].name, "name");
    assert_eq!(env_get.metadata.signature.result, "string|null");

    assert!(registry.library("format").unwrap().is_none());

    let path = registry.library("path").unwrap().unwrap();
    let basename = path.export("basename").unwrap().unwrap();
    assert_eq!(basename.metadata.module.as_deref(), Some("sys.path"));
    assert_eq!(basename.metadata.capability_kind, None);
    assert_eq!(basename.metadata.policy, "none");

    let time = registry.library("time").unwrap().unwrap();
    let unix_millis = time.export("unix_millis").unwrap().unwrap();
    assert_eq!(unix_millis.metadata.module.as_deref(), Some("sys.time"));
    assert_eq!(
        unix_millis.metadata.capability_kind.as_deref(),
        Some("sys.time")
    );
}

#[test]
fn test_host_service_export_requires_explicit_metadata_contract() {
    let mut library = caap_core::HostServiceLibrary::new("plugin").unwrap();
    let result = library.register_function(
        "dynamic",
        caap_core::PhasePolicy::Runtime,
        caap_core::HostFunction::new(
            "plugin.dynamic",
            0,
            Some(0),
            Box::new(|_| Ok(RuntimeValue::Null)),
        )
        .unwrap(),
    );

    let error = result.expect_err("unknown host exports must not get generated metadata");
    assert!(format!("{error}").contains("missing an explicit metadata contract"));
}

#[test]
fn test_compiler_host_compile_time_system_libraries_use_sandbox_policy() {
    let mut host = caap_core::CompilerHost::new();
    host.register_default_compile_time_system_libraries()
        .unwrap();

    // The export EXISTS at compile time (sys exports are dual-phase); what the
    // sandbox denies is the BEHAVIOUR: calling read_line must hit the
    // allow_stdin_read=false policy, not silently read stdin.
    let read_line = host
        .compile_time_services()
        .export("io", "read_line", caap_core::PhasePolicy::CompileTime)
        .expect("compile-time io.read_line export exists; the sandbox gates calls");
    let RuntimeValue::HostFunction(read_line) = read_line else {
        panic!("expected io.read-line host function");
    };
    let err = (read_line.handler)(vec![]).expect_err("sandbox must deny stdin reads");
    assert!(
        format!("{err}").contains("io.read_line"),
        "stdin denial should name the operation: {err}"
    );

    let env_get = host
        .compile_time_services()
        .export("os", "env_get", caap_core::PhasePolicy::CompileTime)
        .unwrap();
    let RuntimeValue::HostFunction(env_get) = env_get else {
        panic!("expected os.env-get host function");
    };
    assert_eq!(
        (env_get.handler)(vec![RuntimeValue::Str("PATH".into())]).unwrap(),
        RuntimeValue::Null
    );
}

#[test]
fn test_host_service_registry_exports_library_to_environment_explicitly() {
    let mut registry = caap_core::HostServiceRegistry::new();
    registry.register_default_system_libraries().unwrap();
    registry.set_system_policy(caap_core::HostSystemPolicy::allow_all());
    registry.set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());
    let env = Environment::new(None);

    let bindings = registry
        .export_library_to_environment("path", caap_core::PhasePolicy::Runtime, &env)
        .unwrap();
    assert_eq!(
        bindings,
        vec![
            "path.basename",
            "path.dirname",
            "path.extension",
            "path.is_absolute",
            "path.join",
            "path.normalize",
            "path.split",
            "path.stem",
            "path.strip_prefix",
            "path.with_extension",
        ]
    );

    let exported = Environment::lookup(&env, "path.basename").unwrap();
    let RuntimeValue::HostFunction(exported) = exported else {
        panic!("expected path.basename host function");
    };
    assert_eq!(
        (exported.handler)(vec![RuntimeValue::Str("/tmp/demo.caap".into())]).unwrap(),
        RuntimeValue::Str("demo.caap".into())
    );
}

#[test]
fn test_host_service_builtins_export_compile_time_and_runtime_services() {
    let mut host = caap_core::CompilerHost::new();
    host.compile_time_services_mut()
        .unwrap()
        .register_function_with_metadata(
            "math",
            "inc",
            caap_core::PhasePolicy::CompileTime,
            caap_core::HostFunction::new(
                "math.inc",
                1,
                Some(1),
                Box::new(|args| {
                    let value = caap_core::require_int_strict(&args[0], "math.inc")?;
                    Ok(RuntimeValue::Int(value + 1))
                }),
            )
            .unwrap(),
            test_host_export_metadata(
                "inc",
                1,
                Some(1),
                vec![caap_core::HostExportParameter::new("value", "int")],
                "int",
            ),
        )
        .unwrap();
    host.compile_time_services_mut()
        .unwrap()
        .set_capability_policy(caap_core::HostCapabilityPolicy::allow_all());
    host.register_default_runtime_system_libraries().unwrap();
    let mut compiler = host.new_session();
    let value = compiler
        .bootstrap()
        .execute_text_with_capabilities(
            "(bind inc (host_service_export \"math\" \"inc\" \"compile_time\")
          (bind runtime_basename
            (host_service_export \"path\" \"basename\" \"runtime\")
            (list_of
              (inc 41)
              (host_value_kind runtime_basename))))",
            "host_service_builtins",
            ["sys".to_string(), "test.host".to_string()],
        )
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(42));
    assert_eq!(items[1], RuntimeValue::Str("host_function".into()));
}

#[test]
fn test_host_service_builtins_require_explicit_capability() {
    let mut host = caap_core::CompilerHost::new();
    host.register_default_compile_time_system_libraries()
        .unwrap();
    let mut compiler = host.new_session();
    let graph = parse("(host_service_export \"fs\" \"read_text\" \"compile_time\")").unwrap();
    let unit = Unit::from_graph("host_service_denied", graph).unwrap();

    let err = compiler
        .evaluation()
        .evaluate(&unit, caap_core::PhasePolicy::CompileTime, [])
        .expect_err("host-service-export must require the fs.read_text capability");

    // Without any grant, binding fs.read_text is denied and names the specific
    // capability the unit would need.
    let message = err.to_string();
    assert!(
        message.contains("capability denied") && message.contains("sys.fs.read"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_host_service_export_honors_fine_grained_capability_grant() {
    let mut host = caap_core::CompilerHost::new();
    host.register_default_runtime_system_libraries().unwrap();
    let mut compiler = host.new_session();

    // A unit granted only sys.fs.read may bind fs.read_text.
    let read = compiler.bootstrap().execute_text_with_capabilities(
        "(host_service_export \"fs\" \"read_text\" \"runtime\")",
        "fs_reader",
        ["sys.fs.read".to_string()],
    );
    assert!(read.is_ok(), "fs.read_text should be allowed: {read:?}");

    // The same grant does not let it bind fs.write_text.
    let write = compiler
        .bootstrap()
        .execute_text_with_capabilities(
            "(host_service_export \"fs\" \"write_text\" \"runtime\")",
            "fs_writer",
            ["sys.fs.read".to_string()],
        )
        .expect_err("fs.write_text should require sys.fs.write");
    assert!(write.to_string().contains("sys.fs.write"), "{write}");
}

#[test]
fn test_host_service_builtins_project_libraries_catalog_and_capability_exports() {
    let mut host = caap_core::CompilerHost::new();
    host.register_default_runtime_system_libraries().unwrap();
    let mut compiler = host.new_session();
    let value = compiler
        .bootstrap()
        .execute_text_with_capabilities(
            "(bind libraries (host_service_libraries \"runtime\")
          (bind catalog (host_service_library_catalog \"os\" \"runtime\")
            (bind guarded_current_exe
              (host_service_capability_export
                (host_service_capability \"sys.path\")
                \"os\"
                \"current_exe\"
                \"runtime\")
              (list_of
                (contains libraries \"os\")
                (get (get catalog 2) \"module\")
                (get (get catalog 2) \"capability_kind\")
                (eq (value_type (guarded_current_exe
                    (host_service_capability \"sys.path\"))) \"string\")))))",
            "host_service_catalog_builtins",
            ["sys".to_string()],
        )
        .unwrap();

    let RuntimeValue::List(items) = value else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(true));
    assert_eq!(items[1], RuntimeValue::Str("sys.path".into()));
    assert_eq!(items[2], RuntimeValue::Str("sys.path".into()));
    assert_eq!(items[3], RuntimeValue::Bool(true));
}

#[test]
fn test_host_service_capability_export_requires_covering_projection() {
    let mut host = caap_core::CompilerHost::new();
    host.register_default_runtime_system_libraries().unwrap();
    let mut compiler = host.new_session();

    let err = compiler
        .bootstrap()
        .execute_text_with_capabilities(
            "(host_service_capability_export
              (host_service_capability \"sys.env\")
              \"os\"
              \"current_exe\"
              \"runtime\")",
            "host_service_capability_projection",
            ["sys".to_string()],
        )
        .expect_err("sys.env must not project a sys.path host export");

    let message = err.to_string();
    assert!(message.contains("cannot project"), "{message}");
    assert!(message.contains("sys.path"), "{message}");
    assert!(message.contains("sys.env"), "{message}");
}
