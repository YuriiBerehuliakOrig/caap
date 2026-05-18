/// Compiler query/evaluation CTFE builtins — initial port of
/// `caap/builtins/compiler/query.py` for APIs backed by current Rust state.
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use caap_peg_port::{PEGParser, ParserConfig};

use crate::artifacts::{
    ArtifactCacheStats, ArtifactFingerprint, ArtifactInvalidationRecord, ArtifactKey,
    ArtifactValue, SourceOrigin,
};
use crate::builtins::compiler_registry::{require_compiler_bridge, require_named_string};
use crate::builtins::compiler_units;
use crate::compiler::{
    package_dependency_module_names, parse_package_declarations_or_none, BootstrapTraceEvent,
    CompilerBridgeValue, EvaluationCapture, QueryArtifactProjection, QueryArtifactSource,
    QueryExecutionOptions, QueryPlanStep, QueryProvider, QueryProviderExecutionRecord,
    QueryProviderSchedule, SemanticPolicyRegistration, UnitBridgeValue,
};
use crate::diagnostics::{Diagnostic, DiagnosticFix, DiagnosticFrame};
use crate::eval::{eval_args, Evaluator};
use crate::frontend::{eval_source, parsed_source_to_ir, ParsedForm};
use crate::semantic::{PhasePolicy, SemanticValue};
use crate::surface_syntax::{
    compile_surface_grammar_from_syntax_state, parse_value_to_parsed_source,
    SurfaceBuiltinSemanticRuntime,
};
use crate::unit::{Unit, UnitSyntaxState};
use crate::values::{eval_err, BuiltinInfo, EvalSignal, HostFunction, MapKey, RuntimeValue};

pub fn register(ev: &mut Evaluator) {
    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-list-dir".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let _bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-list-dir expects a compiler bridge",
            )?;
            let path = require_named_string(
                &args[1],
                "ctfe-compiler-list-dir expects a non-empty string path",
            )?;
            let entries = list_dir(Path::new(&path)).map_err(eval_err)?;
            Ok(tuple(entries.iter().map(dir_entry_to_value).collect()))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-walk-dir".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let _bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-walk-dir expects a compiler bridge",
            )?;
            let path = require_named_string(
                &args[1],
                "ctfe-compiler-walk-dir expects a non-empty string path",
            )?;
            let entries = walk_dir(Path::new(&path)).map_err(eval_err)?;
            Ok(tuple(entries.iter().map(dir_entry_to_value).collect()))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-module-root-callbacks".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-module-root-callbacks expects a compiler bridge",
            )?;
            module_root_callbacks(bridge, args[0].clone())
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-execute-bootstrap-file".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-execute-bootstrap-file expects a compiler bridge",
            )?;
            let path = require_named_string(
                &args[1],
                "ctfe-compiler-execute-bootstrap-file expects a non-empty string path",
            )?;
            let capabilities = optional_string_sequence(
                args.get(2),
                "ctfe-compiler-execute-bootstrap-file expects a sequence of capability names",
                "ctfe-compiler-execute-bootstrap-file expects non-empty string capability names",
            )?;
            bridge
                .execute_bootstrap_file_with_capabilities(path, capabilities, args[0].clone())
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-execute-bootstrap-files".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-execute-bootstrap-files expects a compiler bridge",
            )?;
            let paths = string_sequence(
                &args[1],
                "ctfe-compiler-execute-bootstrap-files expects a sequence of paths",
                "ctfe-compiler-execute-bootstrap-files expects non-empty string paths",
            )?;
            let capabilities = optional_string_sequence(
                args.get(2),
                "ctfe-compiler-execute-bootstrap-files expects a sequence of capability names",
                "ctfe-compiler-execute-bootstrap-files expects non-empty string capability names",
            )?;
            for path in paths {
                bridge
                    .execute_bootstrap_file_with_capabilities(
                        path,
                        capabilities.clone(),
                        args[0].clone(),
                    )
                    .map_err(eval_err)?;
            }
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-order-bootstrap-plan".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let entries = bootstrap_plan_entries(&args[0])?;
            let ordered = order_bootstrap_plan_entries(&entries)?;
            Ok(tuple(ordered.into_iter().map(string).collect()))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-evaluate-bootstrap-file".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 2,
        max_arity: Some(6),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-evaluate-bootstrap-file expects a compiler bridge",
            )?;
            let path = require_named_string(
                &args[1],
                "ctfe-compiler-evaluate-bootstrap-file expects a non-empty string path",
            )?;
            let initial = initial_bindings(
                args.get(2),
                "ctfe-compiler-evaluate-bootstrap-file expects an initial bindings map when provided",
            )?;
            let capabilities = optional_string_sequence(
                args.get(3),
                "ctfe-compiler-evaluate-bootstrap-file expects a sequence of capability names",
                "ctfe-compiler-evaluate-bootstrap-file expects non-empty string capability names",
            )?;
            let skip_leading_forms = optional_nonnegative_usize(
                args.get(4),
                "ctfe-compiler-evaluate-bootstrap-file expects a non-negative integer skip count",
            )?;
            let prepare_pipeline = optional_bool(
                args.get(5),
                "ctfe-compiler-evaluate-bootstrap-file expects a boolean prepare-pipeline flag",
                true,
            )?;
            let capture = bridge
                .evaluate_bootstrap_file(
                    path,
                    initial,
                    capabilities,
                    skip_leading_forms,
                    prepare_pipeline,
                    args[0].clone(),
                )
                .map_err(eval_err)?;
            Ok(evaluation_capture_to_value(&capture))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-load-surface-file-template".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-load-surface-file-template expects a compiler bridge",
            )?;
            let path = require_named_string(
                &args[1],
                "ctfe-compiler-load-surface-file-template expects a non-empty string path",
            )?;
            let unit = bridge.load_surface_unit_template(path).map_err(eval_err)?;
            Ok(RuntimeValue::HostObject(Rc::new(unit)))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-parse-surface-file-forms".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-parse-surface-file-forms expects a compiler bridge",
            )?;
            let path = require_named_string(
                &args[1],
                "ctfe-compiler-parse-surface-file-forms expects a non-empty string path",
            )?;
            let leading_heads = optional_string_set(
                args.get(2),
                "ctfe-compiler-parse-surface-file-forms expects non-empty head names",
            )?;
            let forms = bridge
                .parse_surface_file_forms(path, leading_heads)
                .map_err(eval_err)?;
            Ok(tuple(forms.iter().map(form_record_to_value).collect()))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-cache-stats".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-cache-stats expects a compiler bridge",
            )?;
            Ok(cache_stats_to_value(&bridge.cache_stats()))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-current-bootstrap-path".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-current-bootstrap-path expects a compiler bridge",
            )?;
            Ok(bridge
                .current_bootstrap_path()
                .map(string)
                .unwrap_or(RuntimeValue::Null))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-current-bootstrap-capabilities".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-current-bootstrap-capabilities expects a compiler bridge",
            )?;
            Ok(tuple(
                bridge
                    .current_bootstrap_capabilities()
                    .iter()
                    .map(string)
                    .collect(),
            ))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-list-semantic-policies".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-list-semantic-policies expects a compiler bridge",
            )?;
            Ok(tuple(
                bridge
                    .list_semantic_policies()
                    .iter()
                    .map(semantic_policy_to_value)
                    .collect(),
            ))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-describe-semantic-policy".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-describe-semantic-policy expects a compiler bridge",
            )?;
            let name = require_named_string(
                &args[1],
                "ctfe-compiler-describe-semantic-policy expects a non-empty string name",
            )?;
            Ok(bridge
                .describe_semantic_policy(&name)
                .map_err(eval_err)?
                .map(|policy| semantic_policy_to_value(&policy))
                .unwrap_or(RuntimeValue::Null))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-bootstrap-trace".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-bootstrap-trace expects a compiler bridge",
            )?;
            Ok(tuple(
                bridge
                    .bootstrap_trace()
                    .iter()
                    .map(bootstrap_trace_to_value)
                    .collect(),
            ))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-query-plan".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-query-plan expects a compiler bridge",
            )?;
            let target = require_named_string(
                &args[1],
                "ctfe-compiler-query-plan expects a non-empty target name",
            )?;
            let plan = match query_plan_source_and_phase(&args)? {
                Some((source, phase)) => bridge
                    .plan_query_with_source_options(
                        target,
                        source,
                        phase,
                        QueryExecutionOptions::default(),
                    )
                    .map_err(eval_err)?,
                None => bridge
                    .plan_query(target, query_plan_default_phase(&args)?)
                    .map_err(eval_err)?,
            };
            Ok(tuple(
                plan.steps.iter().map(query_plan_step_to_value).collect(),
            ))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-query-artifact".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(5),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-query-artifact expects a compiler bridge",
            )?;
            let target = require_named_string(
                &args[1],
                "ctfe-compiler-query-artifact expects a non-empty target name",
            )?;
            let source = query_artifact_source(&args[2])?;
            let (phase, options) = phase_and_initial_options(
                &args,
                3,
                4,
                "ctfe-compiler-query-artifact expects a valid phase",
                "ctfe-compiler-query-artifact expects an initial bindings map when provided",
            )?;
            let artifact = bridge
                .query_artifact_with_options(target, source, phase, options)
                .map_err(eval_err)?;
            Ok(query_artifact_to_value(&artifact))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-explain-artifact".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            explain_artifact_value(&args[0])
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-explain-query".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(5),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-explain-query expects a compiler bridge",
            )?;
            let target = require_named_string(
                &args[1],
                "ctfe-compiler-explain-query expects a non-empty target name",
            )?;
            let source = query_artifact_source(&args[2])?;
            let (phase, options) = phase_and_initial_options(
                &args,
                3,
                4,
                "ctfe-compiler-explain-query expects a valid phase",
                "ctfe-compiler-explain-query expects an initial bindings map when provided",
            )?;
            let execution = bridge
                .query_execution_projection_with_options(
                    target.clone(),
                    source.clone(),
                    phase,
                    options,
                )
                .map_err(eval_err)?;
            Ok(map([
                ("target", string(target.as_str())),
                ("phase", string(phase.as_str())),
                ("origin", query_source_origin_to_value(&source)),
                (
                    "steps",
                    tuple(
                        execution
                            .plan
                            .steps
                            .iter()
                            .map(explain_plan_step_to_value)
                            .collect(),
                    ),
                ),
                ("result", query_artifact_to_value(&execution.artifact)),
            ]))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-explain-invalidation".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(5),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-explain-invalidation expects a compiler bridge",
            )?;
            let target = require_named_string(
                &args[1],
                "ctfe-compiler-explain-invalidation expects a non-empty target name",
            )?;
            let source = query_artifact_source(&args[2])?;
            let (phase, options) = phase_and_initial_options(
                &args,
                3,
                4,
                "ctfe-compiler-explain-invalidation expects a valid phase",
                "ctfe-compiler-explain-invalidation expects an initial bindings map when provided",
            )?;
            let execution = bridge
                .query_execution_projection_with_options(
                    target.clone(),
                    source.clone(),
                    phase,
                    options,
                )
                .map_err(eval_err)?;
            Ok(map([
                ("target", string(target.as_str())),
                ("phase", string(phase.as_str())),
                ("origin", query_source_origin_to_value(&source)),
                (
                    "steps",
                    tuple(
                        execution
                            .plan
                            .steps
                            .iter()
                            .zip(execution.invalidations.iter())
                            .map(|(step, invalidation)| {
                                invalidation_plan_step_to_value(step, invalidation.as_ref())
                            })
                            .collect(),
                    ),
                ),
                ("result", query_artifact_to_value(&execution.artifact)),
            ]))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-explain-provider-schedule".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(5),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-explain-provider-schedule expects a compiler bridge",
            )?;
            let target = require_named_string(
                &args[1],
                "ctfe-compiler-explain-provider-schedule expects a non-empty target name",
            )?;
            let source = query_artifact_source(&args[2])?;
            let (phase, options) = phase_and_initial_options(
                &args,
                3,
                4,
                "ctfe-compiler-explain-provider-schedule expects a valid phase",
                "ctfe-compiler-explain-provider-schedule expects an initial bindings map when provided",
            )?;
            let execution = bridge
                .query_execution_projection_with_options(target.clone(), source.clone(), phase, options)
                .map_err(eval_err)?;
            let families = provider_schedule_families_to_value(bridge, &execution.plan.steps)?;
            Ok(map([
                ("target", string(target.as_str())),
                ("phase", string(phase.as_str())),
                ("origin", query_source_origin_to_value(&source)),
                ("families", tuple(families)),
                ("result", query_artifact_to_value(&execution.artifact)),
            ]))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-explain-provider-execution".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(5),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-explain-provider-execution expects a compiler bridge",
            )?;
            let target = require_named_string(
                &args[1],
                "ctfe-compiler-explain-provider-execution expects a non-empty target name",
            )?;
            let source = query_artifact_source(&args[2])?;
            let (phase, options) = phase_and_initial_options(
                &args,
                3,
                4,
                "ctfe-compiler-explain-provider-execution expects a valid phase",
                "ctfe-compiler-explain-provider-execution expects an initial bindings map when provided",
            )?;
            let execution = bridge
                .query_execution_projection_with_options(target.clone(), source.clone(), phase, options)
                .map_err(eval_err)?;
            Ok(map([
                ("target", string(target.as_str())),
                ("phase", string(phase.as_str())),
                ("origin", query_source_origin_to_value(&source)),
                (
                    "executed",
                    tuple(
                        execution
                            .plan
                            .executed
                            .iter()
                            .map(provider_execution_record_to_value)
                            .collect(),
                    ),
                ),
                ("result", query_artifact_to_value(&execution.artifact)),
            ]))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-explain-name".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 4,
        max_arity: Some(6),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-explain-name expects a compiler bridge",
            )?;
            let target = require_named_string(
                &args[1],
                "ctfe-compiler-explain-name expects a non-empty target name",
            )?;
            let source = query_artifact_source(&args[2])?;
            let (phase, options) = phase_and_initial_options(
                &args,
                4,
                5,
                "ctfe-compiler-explain-name expects a valid phase",
                "ctfe-compiler-explain-name expects an initial bindings map when provided",
            )?;
            let execution = bridge
                .query_execution_projection_with_options(target.clone(), source, phase, options)
                .map_err(eval_err)?;
            let summary = compiler_units::explain_name(&execution.unit, &args[3])?;
            with_query_summary(summary, target.as_str(), phase)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-explain-rewrite".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 4,
        max_arity: Some(6),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-explain-rewrite expects a compiler bridge",
            )?;
            let target = require_named_string(
                &args[1],
                "ctfe-compiler-explain-rewrite expects a non-empty target name",
            )?;
            let source = query_artifact_source(&args[2])?;
            let (phase, options) = phase_and_initial_options(
                &args,
                4,
                5,
                "ctfe-compiler-explain-rewrite expects a valid phase",
                "ctfe-compiler-explain-rewrite expects an initial bindings map when provided",
            )?;
            let execution = bridge
                .query_execution_projection_with_options(target.clone(), source, phase, options)
                .map_err(eval_err)?;
            let summary = compiler_units::explain_rewrite(&execution.unit, &args[3])?;
            with_query_summary(summary, target.as_str(), phase)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-compile-unit".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 2,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-compile-unit expects a compiler bridge",
            )?;
            let unit =
                require_unit_bridge(&args[1], "ctfe-compiler-compile-unit expects a unit handle")?
                    .clone_unit();
            let (initial, raise_on_error) = compile_unit_options(&args)?;
            let compiled = bridge
                .compile_unit(&unit, raise_on_error, initial)
                .map_err(eval_err)?;
            Ok(RuntimeValue::HostObject(Rc::new(compiled)))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-evaluate-capture".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-evaluate-capture expects a compiler bridge",
            )?;
            let unit = require_unit_bridge(
                &args[1],
                "ctfe-compiler-evaluate-capture expects a unit handle",
            )?
            .clone_unit();
            let phase = phase_arg(
                &args[2],
                "ctfe-compiler-evaluate-capture expects a valid phase",
            )?;
            let initial = initial_bindings(
                args.get(3),
                "ctfe-compiler-evaluate-capture expects an initial map when provided",
            )?;
            let capture = bridge
                .evaluate_capture(&unit, phase, initial, args[0].clone())
                .map_err(eval_err)?;
            Ok(evaluation_capture_to_value(&capture))
        }),
    });
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirEntryProjection {
    name: String,
    path: String,
    kind: String,
    is_file: bool,
    is_dir: bool,
    is_symlink: bool,
}

fn list_dir(path: &Path) -> Result<Vec<DirEntryProjection>, String> {
    let root = std::fs::canonicalize(path)
        .map_err(|error| format!("directory path resolution failed: {error}"))?;
    let mut entries = Vec::new();
    for entry in
        std::fs::read_dir(&root).map_err(|error| format!("directory read failed: {error}"))?
    {
        let entry = entry.map_err(|error| format!("directory entry read failed: {error}"))?;
        entries.push(dir_entry_projection(entry.path())?);
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

fn walk_dir(path: &Path) -> Result<Vec<DirEntryProjection>, String> {
    let root = std::fs::canonicalize(path)
        .map_err(|error| format!("directory path resolution failed: {error}"))?;
    let mut entries = Vec::new();
    walk_dir_inner(&root, &mut entries)?;
    Ok(entries)
}

fn walk_dir_inner(path: &Path, entries: &mut Vec<DirEntryProjection>) -> Result<(), String> {
    let mut children: Vec<PathBuf> = std::fs::read_dir(path)
        .map_err(|error| format!("directory read failed: {error}"))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| format!("directory entry read failed: {error}"))
        })
        .collect::<Result<_, _>>()?;
    children.sort_by(|left, right| {
        let left_name = left
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let right_name = right
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        left_name.cmp(right_name)
    });
    for child in children {
        let projection = dir_entry_projection(child.clone())?;
        let should_recurse = projection.is_dir && !projection.is_symlink;
        entries.push(projection);
        if should_recurse {
            walk_dir_inner(&child, entries)?;
        }
    }
    Ok(())
}

fn dir_entry_projection(path: PathBuf) -> Result<DirEntryProjection, String> {
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| format!("directory entry metadata failed: {error}"))?;
    let is_file = metadata.is_file();
    let is_dir = metadata.is_dir();
    let is_symlink = metadata.file_type().is_symlink();
    Ok(DirEntryProjection {
        name: path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string(),
        path: path.to_string_lossy().into_owned(),
        kind: if is_dir { "dir" } else { "file" }.to_string(),
        is_file,
        is_dir,
        is_symlink,
    })
}

fn dir_entry_to_value(entry: &DirEntryProjection) -> RuntimeValue {
    map([
        ("name", string(entry.name.as_str())),
        ("path", string(entry.path.as_str())),
        ("kind", string(entry.kind.as_str())),
        ("is_file", RuntimeValue::Bool(entry.is_file)),
        ("is_dir", RuntimeValue::Bool(entry.is_dir)),
        ("is_symlink", RuntimeValue::Bool(entry.is_symlink)),
    ])
}

fn module_root_callbacks(
    _bridge: &CompilerBridgeValue,
    compiler_value: RuntimeValue,
) -> Result<RuntimeValue, EvalSignal> {
    Ok(map([
        (
            "list-dir",
            host_fn("module-root.list-dir", 1, Some(1), {
                let compiler_value = compiler_value.clone();
                move |args| {
                    let path = require_named_string(
                        &args[0],
                        "module root list-dir callback expects a path string",
                    )?;
                    let bridge = require_compiler_bridge(
                        &compiler_value,
                        "module root list-dir callback expects a compiler bridge",
                    )?;
                    let resolved = bridge
                        .resolve_bootstrap_source_path(Path::new(&path))
                        .map_err(eval_err)?;
                    let entries = list_dir(&resolved).map_err(eval_err)?;
                    Ok(tuple(entries.iter().map(dir_entry_to_value).collect()))
                }
            })?,
        ),
        (
            "is-file",
            host_fn("module-root.is-file", 1, Some(1), {
                let compiler_value = compiler_value.clone();
                move |args| {
                    let path = require_named_string(
                        &args[0],
                        "module root is-file callback expects a path string",
                    )?;
                    let bridge = require_compiler_bridge(
                        &compiler_value,
                        "module root is-file callback expects a compiler bridge",
                    )?;
                    let resolved = match bridge.resolve_bootstrap_source_path(Path::new(&path)) {
                        Ok(resolved) => resolved,
                        Err(_) => return Ok(RuntimeValue::Bool(false)),
                    };
                    Ok(RuntimeValue::Bool(resolved.is_file()))
                }
            })?,
        ),
        (
            "collect-source-imports",
            host_fn("module-root.collect-source-imports", 1, Some(1), {
                let compiler_value = compiler_value.clone();
                move |args| {
                    let path = require_named_string(
                        &args[0],
                        "module root collect-source-imports callback expects a path string",
                    )?;
                    let bridge = require_compiler_bridge(
                        &compiler_value,
                        "module root collect-source-imports callback expects a compiler bridge",
                    )?;
                    let resolved = bridge
                        .resolve_bootstrap_source_path(Path::new(&path))
                        .map_err(eval_err)?;
                    let imports = parse_source_descriptor_or_none(&resolved)?
                        .map(|descriptor| package_dependency_module_names(&descriptor.imports))
                        .unwrap_or_default();
                    Ok(tuple(imports.into_iter().map(string).collect()))
                }
            })?,
        ),
        (
            "collect-source-syntax-imports",
            host_fn("module-root.collect-source-syntax-imports", 1, Some(1), {
                let compiler_value = compiler_value.clone();
                move |args| {
                    let path = require_named_string(
                        &args[0],
                        "module root collect-source-syntax-imports callback expects a path string",
                    )?;
                    let bridge = require_compiler_bridge(
                        &compiler_value,
                        "module root collect-source-syntax-imports callback expects a compiler bridge",
                    )?;
                    let resolved = bridge
                        .resolve_bootstrap_source_path(Path::new(&path))
                        .map_err(eval_err)?;
                    let imports = parse_source_descriptor_or_none(&resolved)?
                        .map(|descriptor| {
                            package_dependency_module_names(&descriptor.syntax_imports)
                        })
                        .unwrap_or_default();
                    Ok(tuple(imports.into_iter().map(string).collect()))
                }
            })?,
        ),
        (
            "collect-source-module-name",
            host_fn("module-root.collect-source-module-name", 1, Some(1), {
                let compiler_value = compiler_value.clone();
                move |args| {
                    let path = require_named_string(
                        &args[0],
                        "module root collect-source-module-name callback expects a path string",
                    )?;
                    let bridge = require_compiler_bridge(
                        &compiler_value,
                        "module root collect-source-module-name callback expects a compiler bridge",
                    )?;
                    let resolved = bridge
                        .resolve_bootstrap_source_path(Path::new(&path))
                        .map_err(eval_err)?;
                    Ok(parse_source_descriptor_or_none(&resolved)?
                        .map(|descriptor| string(descriptor.name))
                        .unwrap_or(RuntimeValue::Null))
                }
            })?,
        ),
        (
            "load-source-unit",
            host_fn("module-root.load-source-unit", 1, Some(1), {
                let compiler_value = compiler_value.clone();
                move |args| {
                    let path = require_named_string(
                        &args[0],
                        "module root load-source-unit callback expects a path string",
                    )?;
                    let bridge = require_compiler_bridge(
                        &compiler_value,
                        "module root load-source-unit callback expects a compiler bridge",
                    )?;
                    let resolved = bridge
                        .resolve_bootstrap_source_path(Path::new(&path))
                        .map_err(eval_err)?;
                    let descriptor = parse_source_descriptor_or_none(&resolved)?;
                    let unit = if let Some(descriptor) = descriptor {
                        if !descriptor.syntax_imports.is_empty()
                            && syntax_imports_are_registered(bridge, &descriptor)
                                .map_err(eval_err)?
                        {
                            load_dynamic_source_unit(
                                bridge,
                                &resolved,
                                &descriptor,
                                compiler_value.clone(),
                            )?
                        } else {
                            bridge
                                .load_surface_unit_template(resolved)
                                .map_err(eval_err)?
                        }
                    } else {
                        bridge
                            .load_surface_unit_template(resolved)
                            .map_err(eval_err)?
                    };
                    Ok(RuntimeValue::HostObject(Rc::new(unit)))
                }
            })?,
        ),
        (
            "compile-source-unit",
            host_fn("module-root.compile-source-unit", 2, Some(2), {
                let compiler_value = compiler_value.clone();
                move |args| {
                    let unit_bridge = require_unit_bridge(
                        &args[0],
                        "module root compile-source-unit callback expects a unit handle",
                    )?;
                    unit_bridge
                        .with_unit_mut(prepare_module_root_compile_unit)
                        .map_err(eval_err)?;
                    let unit = unit_bridge.clone_unit();
                    let bridge = require_compiler_bridge(
                        &compiler_value,
                        "module root compile-source-unit callback expects a compiler bridge",
                    )?;
                    let initial = initial_bindings(
                        args.get(1),
                        "module root compile-source-unit callback expects an initial map",
                    )?;
                    let compiled = bridge
                        .compile_unit(&unit, true, initial)
                        .map_err(eval_err)?;
                    let mut compiled_unit = compiled.clone_unit();
                    finalize_module_root_compiled_unit(&mut compiled_unit).map_err(eval_err)?;
                    unit_bridge.with_unit_mut(|unit| {
                        *unit = compiled_unit.clone();
                    });
                    Ok(RuntimeValue::HostObject(Rc::new(
                        UnitBridgeValue::from_unit(&compiled_unit),
                    )))
                }
            })?,
        ),
        (
            "register-compiled-unit",
            host_fn("module-root.register-compiled-unit", 2, Some(2), {
                let compiler_value = compiler_value.clone();
                move |args| {
                    let module_name = require_named_string(
                        &args[0],
                        "module root register-compiled-unit callback expects a module name",
                    )?;
                    let unit = require_unit_bridge(
                        &args[1],
                        "module root register-compiled-unit callback expects a unit handle",
                    )?
                    .clone_unit();
                    let bridge = require_compiler_bridge(
                        &compiler_value,
                        "module root register-compiled-unit callback expects a compiler bridge",
                    )?;
                    bridge
                        .register_compiled_unit(module_name, &unit)
                        .map_err(eval_err)?;
                    Ok(RuntimeValue::Null)
                }
            })?,
        ),
        (
            "prepare-source-syntax-unit",
            host_fn("module-root.prepare-source-syntax-unit", 2, Some(2), {
                let compiler_value = compiler_value.clone();
                move |args| {
                    let path = require_named_string(
                        &args[0],
                        "module root prepare-source-syntax-unit callback expects a path string",
                    )?;
                    let unit = require_unit_bridge(
                        &args[1],
                        "module root prepare-source-syntax-unit callback expects a unit handle",
                    )?;
                    let bridge = require_compiler_bridge(
                        &compiler_value,
                        "module root prepare-source-syntax-unit callback expects a compiler bridge",
                    )?;
                    let resolved = bridge
                        .resolve_bootstrap_source_path(Path::new(&path))
                        .map_err(eval_err)?;
                    let source = std::fs::read_to_string(&resolved)
                        .map_err(|error| eval_err(format!("source syntax read failed: {error}")))?;
                    if parse_source_descriptor_or_none(&resolved)?
                        .is_some_and(|descriptor| !descriptor.syntax_imports.is_empty())
                    {
                        return Ok(RuntimeValue::Null);
                    }
                    apply_source_syntax_declarations(unit, &source)?;
                    Ok(RuntimeValue::Null)
                }
            })?,
        ),
        (
            "evaluate-source-unit",
            host_fn("module-root.evaluate-source-unit", 2, Some(2), {
                let compiler_value = compiler_value.clone();
                move |args| {
                    let unit = require_unit_bridge(
                        &args[0],
                        "module root evaluate-source-unit callback expects a unit handle",
                    )?
                    .clone_unit();
                    let initial = initial_bindings(
                        args.get(1),
                        "module root evaluate-source-unit callback expects an initial map",
                    )?;
                    let bridge = require_compiler_bridge(
                        &compiler_value,
                        "module root evaluate-source-unit callback expects a compiler bridge",
                    )?;
                    let skip_leading_forms = module_root_runtime_skip_leading_forms(&unit);
                    let capture = bridge
                        .evaluate_capture_skipping(
                            &unit,
                            PhasePolicy::Runtime,
                            initial,
                            compiler_value.clone(),
                            skip_leading_forms,
                        )
                        .map_err(eval_err)?;
                    if let Some(value) = capture.value {
                        Ok(value)
                    } else {
                        Err(eval_err(
                            capture
                                .diagnostics
                                .first()
                                .map(|diagnostic| diagnostic.message.clone())
                                .unwrap_or_else(|| {
                                    "module root source evaluation failed".to_string()
                                }),
                        ))
                    }
                }
            })?,
        ),
        (
            "emit-source-unit",
            host_fn("module-root.emit-source-unit", 2, Some(2), {
                let compiler_value = compiler_value.clone();
                move |args| {
                    let unit = require_unit_bridge(
                        &args[0],
                        "module root emit-source-unit callback expects a unit handle",
                    )?
                    .clone_unit();
                    let emitter_name = require_named_string(
                        &args[1],
                        "module root emit-source-unit callback expects an emitter registry name",
                    )?;
                    let bridge = require_compiler_bridge(
                        &compiler_value,
                        "module root emit-source-unit callback expects a compiler bridge",
                    )?;
                    let emitter = bridge
                        .lookup_registered_value(&emitter_name)
                        .map_err(eval_err)?
                        .ok_or_else(|| {
                            eval_err(format!(
                                "module root emit-source-unit callback could not find emitter {emitter_name:?}",
                            ))
                        })?;
                    let unit_handle =
                        RuntimeValue::HostObject(Rc::new(UnitBridgeValue::from_unit(&unit)));
                    let mut evaluator = Evaluator::new(Default::default());
                    let result = evaluator.invoke_callback(&emitter, vec![unit_handle])?;
                    llvm_emitter_text_result(result)
                }
            })?,
        ),
    ]))
}

fn llvm_emitter_text_result(value: RuntimeValue) -> Result<RuntimeValue, EvalSignal> {
    let RuntimeValue::Map(fields) = value else {
        return Err(eval_err("llvm-ir emitter must return a map-like result"));
    };
    let fields = fields.borrow();
    let diagnostics = fields
        .get(&MapKey::Str("diagnostics".into()))
        .cloned()
        .unwrap_or(RuntimeValue::Tuple(Vec::new().into()));
    let diagnostic_messages = llvm_diagnostic_messages(&diagnostics)?;
    if !diagnostic_messages.is_empty() {
        return Err(eval_err(format!(
            "llvm-ir emitter returned diagnostics: {}",
            diagnostic_messages.join("; ")
        )));
    }
    let text = match fields.get(&MapKey::Str("text".into())) {
        None | Some(RuntimeValue::Null) => String::new(),
        Some(RuntimeValue::Str(text)) => text.to_string(),
        Some(other) => {
            return Err(eval_err(format!(
                "llvm-ir emitter text field must be string or null, got {other}"
            )));
        }
    };
    Ok(RuntimeValue::Str(text.into()))
}

fn llvm_diagnostic_messages(value: &RuntimeValue) -> Result<Vec<String>, EvalSignal> {
    let items: Vec<RuntimeValue> = match value {
        RuntimeValue::Tuple(items) => items.iter().cloned().collect(),
        RuntimeValue::List(items) => items.borrow().clone(),
        RuntimeValue::Null => Vec::new(),
        other => {
            return Err(eval_err(format!(
                "llvm-ir emitter diagnostics field must be a sequence or null, got {other}"
            )));
        }
    };
    items
        .iter()
        .map(llvm_error_diagnostic_message)
        .collect::<Result<Vec<_>, _>>()
        .map(|messages| messages.into_iter().flatten().collect())
}

fn llvm_error_diagnostic_message(value: &RuntimeValue) -> Result<Option<String>, EvalSignal> {
    let RuntimeValue::Map(fields) = value else {
        return Err(eval_err("llvm-ir emitter diagnostics entries must be maps"));
    };
    let fields = fields.borrow();
    let severity =
        runtime_optional_string_field(&fields, "severity")?.unwrap_or_else(|| "error".into());
    if severity != "error" {
        return Ok(None);
    }
    let code = runtime_optional_string_field(&fields, "code")?;
    let message = runtime_optional_string_field(&fields, "message")?
        .unwrap_or_else(|| "LLVM emitter error".into());
    let path = runtime_optional_string_field(&fields, "path")?;
    Ok(Some(match (code, path) {
        (Some(code), Some(path)) => format!("{code}: {message} ({path})"),
        (Some(code), None) => format!("{code}: {message}"),
        (None, Some(path)) => format!("{message} ({path})"),
        (None, None) => message,
    }))
}

fn runtime_optional_string_field(
    fields: &HashMap<MapKey, RuntimeValue>,
    key: &str,
) -> Result<Option<String>, EvalSignal> {
    match fields.get(&MapKey::Str(key.into())) {
        Some(RuntimeValue::Str(value)) => Ok(Some(value.to_string())),
        Some(RuntimeValue::Null) | None => Ok(None),
        Some(other) => Err(eval_err(format!(
            "llvm-ir emitter diagnostic field {key:?} must be string or null, got {other}"
        ))),
    }
}

const MODULE_ROOT_SETUP_HEADS: &[&str] = &[
    "stdlib.module.seed.api.load-module",
    "stdlib.pass-kit.register-compile-time-function",
    "stdlib.pass-kit.register-provider",
    "stdlib.pass-kit.source-setup",
    "module",
    "module-capability",
    "syntax-authoring-grammar",
    "define-syntax-rule",
    "syntax-hook",
    "syntax-metadata",
    "syntax-import",
    "import-symbols",
    "import-module",
    "import-namespace",
    "import-as",
    "export",
    "export-as",
    "reexport",
    "reexport-as",
    "reexport-module",
    "reexport-namespace",
];

const SOURCE_SETUP_REMOVABLE_HEADS: &[&str] = &[
    "syntax-import",
    "syntax-authoring-grammar",
    "define-syntax-rule",
    "syntax-hook",
    "syntax-metadata",
    "stdlib.pass-kit.source-define",
    "stdlib.pass-kit.source-setup",
];

const SOURCE_SETUP_CTFE_HEADS: &[&str] = &[
    "stdlib.pass-kit.source-define",
    "stdlib.pass-kit.source-setup",
];

fn module_root_runtime_skip_leading_forms(unit: &crate::unit::Unit) -> usize {
    unit.top_level_form_ids()
        .iter()
        .take_while(|node_id| is_module_root_runtime_setup_form(unit, **node_id))
        .count()
}

fn finalize_module_root_compiled_unit(unit: &mut crate::unit::Unit) -> Result<(), String> {
    retain_module_root_runtime_forms(unit)?;
    normalize_module_root_runtime_declaration_forms(unit)?;
    set_module_root_runtime_entry(unit);
    Ok(())
}

fn prepare_module_root_compile_unit(unit: &mut crate::unit::Unit) -> Result<(), String> {
    retain_module_root_runtime_forms(unit)?;
    ensure_module_root_runtime_entry(unit)?;
    normalize_module_root_runtime_declaration_forms(unit)?;
    set_module_root_runtime_entry(unit);
    Ok(())
}

fn retain_module_root_runtime_forms(unit: &mut crate::unit::Unit) -> Result<(), String> {
    let forms = unit.top_level_form_ids().to_vec();
    let retained: Vec<crate::ir::NodeId> = forms
        .iter()
        .copied()
        .filter(|node_id| !is_source_setup_removable_form(unit, *node_id))
        .collect();
    if retained.len() == forms.len() {
        return Ok(());
    }

    unit.ir_mut().set_top_level_form_ids(retained)?;
    for node_id in forms {
        if is_source_setup_removable_form(unit, node_id) && unit.ir().contains(node_id) {
            unit.erase_ir_subtree(node_id)?;
        }
    }
    Ok(())
}

fn normalize_module_root_runtime_declaration_forms(
    unit: &mut crate::unit::Unit,
) -> Result<(), String> {
    let forms = unit.top_level_form_ids().to_vec();
    if forms.len() < 2 {
        return Ok(());
    }
    let entry_id = *forms.last().expect("checked non-empty forms");
    let entry_head = top_level_call_head_name(unit, entry_id);
    if entry_head
        .as_deref()
        .map(|head| MODULE_ROOT_SETUP_HEADS.contains(&head) || head == "bind" || head == "do")
        .unwrap_or(false)
    {
        return Ok(());
    }

    let mut prefix = Vec::new();
    let mut binding_specs = Vec::new();
    let mut changed = false;
    for form_id in forms.iter().copied().take(forms.len() - 1) {
        let Some((name_spec, value_spec, body_specs)) = single_binding_form_specs(unit, form_id)?
        else {
            prefix.push(form_id);
            continue;
        };
        for body_spec in body_specs {
            let body_id = unit.ir_mut().insert_expr_spec(&body_spec)?;
            prefix.push(body_id);
        }
        binding_specs.push(name_spec);
        binding_specs.push(value_spec);
        changed = true;
    }
    if !changed {
        return Ok(());
    }

    let entry_spec = unit.ir().expr_spec_for_subtree(entry_id)?;
    let mut wrapper_args = binding_specs;
    wrapper_args.push(entry_spec);
    let wrapper_spec = crate::ir::ExprSpec::call(crate::ir::ExprSpec::name("bind")?, wrapper_args);
    let wrapper_id = unit.ir_mut().insert_expr_spec(&wrapper_spec)?;

    let mut retained = prefix;
    retained.push(wrapper_id);
    unit.ir_mut().set_top_level_form_ids(retained.clone())?;
    for form_id in forms {
        if !retained.contains(&form_id) && unit.ir().contains(form_id) {
            unit.erase_ir_subtree(form_id)?;
        }
    }
    unit.ir_mut().root_id = wrapper_id;
    Ok(())
}

fn single_binding_form_specs(
    unit: &crate::unit::Unit,
    node_id: crate::ir::NodeId,
) -> Result<
    Option<(
        crate::ir::ExprSpec,
        crate::ir::ExprSpec,
        Vec<crate::ir::ExprSpec>,
    )>,
    String,
> {
    if top_level_call_head_name(unit, node_id).as_deref() != Some("bind") {
        return Ok(None);
    }
    let Some(crate::ir::Node::Call(call)) = unit.ir().node(node_id) else {
        return Ok(None);
    };
    if call.args.len() != 3 {
        return Ok(None);
    }
    let Some(crate::ir::Node::Literal(literal)) = unit.ir().node(call.args[0]) else {
        return Ok(None);
    };
    if !matches!(&literal.value, crate::ir::IrLiteralData::Str(_)) {
        return Ok(None);
    }
    Ok(Some((
        unit.ir().expr_spec_for_subtree(call.args[0])?,
        unit.ir().expr_spec_for_subtree(call.args[1])?,
        vec![unit.ir().expr_spec_for_subtree(call.args[2])?],
    )))
}

fn set_module_root_runtime_entry(unit: &mut crate::unit::Unit) {
    let forms = unit.top_level_form_ids();
    let runtime_root = forms
        .iter()
        .copied()
        .rfind(|node_id| !is_module_root_runtime_setup_form(unit, *node_id))
        .or_else(|| forms.first().copied());
    if let Some(root_id) = runtime_root {
        unit.ir_mut().root_id = root_id;
    }
}

fn is_source_setup_removable_form(unit: &crate::unit::Unit, node_id: crate::ir::NodeId) -> bool {
    top_level_call_head_name(unit, node_id)
        .map(|name| SOURCE_SETUP_REMOVABLE_HEADS.contains(&name.as_str()))
        .unwrap_or(false)
        || is_source_setup_bind(unit, node_id)
}

fn is_source_setup_bind(unit: &crate::unit::Unit, node_id: crate::ir::NodeId) -> bool {
    if top_level_call_head_name(unit, node_id).as_deref() != Some("bind") {
        return false;
    }
    let Some(crate::ir::Node::Call(call)) = unit.ir().node(node_id) else {
        return false;
    };
    call.args.iter().skip(1).any(|arg_id| {
        top_level_call_head_name(unit, *arg_id)
            .map(|name| SOURCE_SETUP_CTFE_HEADS.contains(&name.as_str()))
            .unwrap_or(false)
    })
}

fn is_module_root_runtime_setup_form(unit: &crate::unit::Unit, node_id: crate::ir::NodeId) -> bool {
    top_level_call_head_name(unit, node_id)
        .map(|name| MODULE_ROOT_SETUP_HEADS.contains(&name.as_str()))
        .unwrap_or(false)
        || is_codegen_root_signature_form(unit, node_id)
}

fn is_codegen_root_signature_form(unit: &crate::unit::Unit, node_id: crate::ir::NodeId) -> bool {
    let Some(crate::ir::Node::Call(call)) = unit.ir().node(node_id) else {
        return false;
    };
    let Some(crate::ir::Node::Name(callee)) = unit.ir().node(call.callee) else {
        return false;
    };
    if callee.identifier.as_ref() != "do" {
        return false;
    }
    let Some(first_arg) = call.args.first() else {
        return false;
    };
    matches!(
        unit.ir().node(*first_arg),
        Some(crate::ir::Node::Literal(literal))
            if matches!(
                &literal.value,
                crate::ir::IrLiteralData::Str(value)
                    if value == "caap.codegen.root-signature"
            )
    )
}

fn ensure_module_root_runtime_entry(unit: &mut crate::unit::Unit) -> Result<(), String> {
    let has_runtime_form = unit.top_level_form_ids().iter().any(|node_id| {
        top_level_call_head_name(unit, *node_id)
            .map(|name| !MODULE_ROOT_SETUP_HEADS.contains(&name.as_str()))
            .unwrap_or(true)
    });
    if has_runtime_form {
        return Ok(());
    }
    unit.append_ir_top_level_with_spec(&crate::ir::ExprSpec::literal(
        crate::ir::IrLiteralData::Null,
    ))?;
    Ok(())
}

fn top_level_call_head_name(
    unit: &crate::unit::Unit,
    node_id: crate::ir::NodeId,
) -> Option<String> {
    let crate::ir::Node::Call(call) = unit.ir().node(node_id)? else {
        return None;
    };
    let crate::ir::Node::Name(callee) = unit.ir().node(call.callee)? else {
        return None;
    };
    Some(callee.identifier.to_string())
}

fn host_fn(
    name: &'static str,
    min_arity: usize,
    max_arity: Option<usize>,
    handler: impl Fn(Vec<RuntimeValue>) -> Result<RuntimeValue, EvalSignal> + 'static,
) -> Result<RuntimeValue, EvalSignal> {
    HostFunction::new(name, min_arity, max_arity, Box::new(handler))
        .map(|function| RuntimeValue::HostFunction(Rc::new(function)))
        .map_err(eval_err)
}

fn parse_source_descriptor_or_none(
    path: impl AsRef<Path>,
) -> Result<Option<crate::compiler::PackageDescriptor>, EvalSignal> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .map_err(|error| eval_err(format!("source declaration read failed: {error}")))?;
    parse_package_declarations_or_none(&text, path.to_string_lossy().to_string()).map_err(eval_err)
}

fn load_dynamic_source_unit(
    bridge: &CompilerBridgeValue,
    path: &Path,
    descriptor: &crate::compiler::PackageDescriptor,
    compiler_value: RuntimeValue,
) -> Result<UnitBridgeValue, EvalSignal> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| eval_err(format!("dynamic source read failed: {error}")))?;
    let source_path = path.to_string_lossy().to_string();
    emit_dynamic_source_event(
        bridge,
        "start",
        &source_path,
        [("bytes".to_string(), text.len().to_string())],
    );
    let mut parser_syntax = UnitSyntaxState::new("caap.dynamic").map_err(eval_err)?;
    let mut hooks = HashMap::new();

    for import in &descriptor.syntax_imports {
        emit_dynamic_source_event(
            bridge,
            "syntax-import",
            &source_path,
            [("module".to_string(), import.module_name.clone())],
        );
        let syntax_unit = bridge
            .lookup_compiled_unit(&import.module_name)
            .map_err(eval_err)?
            .ok_or_else(|| {
                eval_err(format!(
                    "syntax import {:?} has no registered compiled unit",
                    import.module_name
                ))
            })?
            .clone_unit();
        merge_syntax_state(&mut parser_syntax, syntax_unit.syntax_state()).map_err(eval_err)?;
        collect_inline_syntax_hooks(syntax_unit.syntax_state(), &mut hooks).map_err(eval_err)?;
        let named_hooks =
            syntax_hook_function_names(syntax_unit.syntax_state()).map_err(eval_err)?;
        if !named_hooks.is_empty() {
            let capture = bridge
                .evaluate_capture_skipping(
                    &syntax_unit,
                    PhasePolicy::CompileTime,
                    Vec::<(String, RuntimeValue)>::new(),
                    compiler_value.clone(),
                    module_root_runtime_skip_leading_forms(&syntax_unit),
                )
                .map_err(eval_err)?;
            collect_named_syntax_hooks(&named_hooks, &capture, &mut hooks).map_err(eval_err)?;
        }
    }

    let grammar = compile_surface_grammar_from_syntax_state(&parser_syntax).map_err(eval_err)?;
    emit_dynamic_source_event(
        bridge,
        "grammar",
        &source_path,
        [
            (
                "rules".to_string(),
                parser_syntax.grammar_rules.len().to_string(),
            ),
            ("hooks".to_string(), hooks.len().to_string()),
        ],
    );
    let runtime =
        SurfaceBuiltinSemanticRuntime::new(&text, Some(source_path.clone())).with_hooks(hooks);
    let parser_config =
        ParserConfig::default().with_max_steps(text.len().saturating_add(65_536).max(65_536));
    emit_dynamic_source_event(bridge, "parse-start", &source_path, std::iter::empty());
    let parse_value = PEGParser
        .parse_with_semantic(&grammar, text.trim(), &parser_config, Some(&runtime))
        .map_err(|error| {
            eval_err(format!(
                "dynamic source parse failed at {}..{} found {:?} stack {:?}: {}",
                error.span.start, error.span.end, error.found, error.rule_stack, error.message
            ))
        })?;
    emit_dynamic_source_event(bridge, "parse-finish", &source_path, std::iter::empty());
    if let Some(error) = runtime.error() {
        return Err(eval_err(error));
    }
    emit_dynamic_source_event(bridge, "decode-start", &source_path, std::iter::empty());
    let parsed = parse_value_to_parsed_source(&parse_value).map_err(eval_err)?;
    emit_dynamic_source_event(
        bridge,
        "decode-finish",
        &source_path,
        [("forms".to_string(), parsed.forms.len().to_string())],
    );
    emit_dynamic_source_event(bridge, "lower-start", &source_path, std::iter::empty());
    let graph = parsed_source_to_ir(&parsed).map_err(eval_err)?;
    emit_dynamic_source_event(
        bridge,
        "lower-finish",
        &source_path,
        [("nodes".to_string(), graph.node_count().to_string())],
    );
    let mut unit = Unit::from_graph(descriptor.name.clone(), graph).map_err(eval_err)?;
    let syntax = UnitSyntaxState::new("caap")
        .map_err(eval_err)?
        .with_source(
            source_path,
            ArtifactFingerprint::sha256(text.as_bytes()).to_string(),
        )
        .map_err(eval_err)?;
    unit.set_syntax_state(syntax);
    Ok(UnitBridgeValue::from_unit(&unit))
}

fn emit_dynamic_source_event(
    bridge: &CompilerBridgeValue,
    action: &str,
    source_path: &str,
    metadata: impl IntoIterator<Item = (String, String)>,
) {
    let _ = bridge.emit_event(
        "dynamic-source",
        action,
        "dynamic source unit loading",
        std::iter::once(("source".to_string(), source_path.to_string())).chain(metadata),
    );
}

fn syntax_imports_are_registered(
    bridge: &CompilerBridgeValue,
    descriptor: &crate::compiler::PackageDescriptor,
) -> Result<bool, String> {
    for import in &descriptor.syntax_imports {
        if bridge.lookup_compiled_unit(&import.module_name)?.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn merge_syntax_state(
    target: &mut UnitSyntaxState,
    source: &UnitSyntaxState,
) -> Result<(), String> {
    for (name, rule) in &source.grammar_rules {
        target.set_grammar_rule(name.clone(), rule.clone())?;
    }
    for (key, value) in &source.grammar_metadata {
        target.set_grammar_metadata(key.clone(), value.clone())?;
    }
    Ok(())
}

fn syntax_hook_function_names(syntax: &UnitSyntaxState) -> Result<Vec<(String, String)>, String> {
    let Some(value) = syntax.grammar_metadata("semantic_hook_functions") else {
        return Ok(Vec::new());
    };
    let SemanticValue::Map(entries) = value else {
        return Err("semantic_hook_functions metadata must be a map".to_string());
    };
    let mut hooks = Vec::new();
    for (hook_ref, function_name) in entries {
        let SemanticValue::Str(function_name) = function_name else {
            return Err("semantic_hook_functions values must be strings".to_string());
        };
        hooks.push((hook_ref.clone(), function_name.clone()));
    }
    Ok(hooks)
}

fn collect_named_syntax_hooks(
    named_hooks: &[(String, String)],
    capture: &EvaluationCapture,
    hooks: &mut HashMap<String, RuntimeValue>,
) -> Result<(), String> {
    for (hook_ref, function_name) in named_hooks {
        let Some((_, value)) = capture
            .bindings
            .iter()
            .rev()
            .find(|(name, _)| name == function_name)
        else {
            return Err(format!(
                "syntax hook {hook_ref:?} references missing function {function_name:?}"
            ));
        };
        hooks.insert(hook_ref.clone(), value.clone());
    }
    Ok(())
}

fn collect_inline_syntax_hooks(
    syntax: &UnitSyntaxState,
    hooks: &mut HashMap<String, RuntimeValue>,
) -> Result<(), String> {
    let Some(value) = syntax.grammar_metadata("semantic_hook_inline_sources") else {
        return Ok(());
    };
    let SemanticValue::Map(entries) = value else {
        return Err("semantic_hook_inline_sources metadata must be a map".to_string());
    };
    for (hook_ref, source) in entries {
        let SemanticValue::Str(source) = source else {
            return Err("semantic_hook_inline_sources values must be strings".to_string());
        };
        hooks.insert(
            hook_ref.clone(),
            eval_source(source).map_err(|error| error.to_string())?,
        );
    }
    Ok(())
}

fn apply_source_syntax_declarations(
    unit: &UnitBridgeValue,
    source: &str,
) -> Result<(), EvalSignal> {
    for grammar in source_string_declaration_args(source, "syntax-authoring-grammar", 1)? {
        unit.with_unit_mut(|unit| {
            let mut syntax = unit.syntax_state().clone();
            crate::syntax_authoring::apply_authoring_grammar_source(&mut syntax, &grammar[0])?;
            unit.set_syntax_state(syntax);
            Ok::<(), String>(())
        })
        .map_err(eval_err)?;
    }
    for rule in source_syntax_rule_declarations(source)? {
        unit.with_unit_mut(|unit| {
            let mut syntax = unit.syntax_state().clone();
            match &rule.implementation {
                SourceSyntaxRuleImplementation::FunctionName(function_name) => {
                    crate::syntax_authoring::define_authoring_syntax_rule(
                        &mut syntax,
                        &rule.source,
                        function_name,
                    )?;
                }
                SourceSyntaxRuleImplementation::InlineSource(implementation_source) => {
                    crate::syntax_authoring::define_authoring_syntax_rule_inline_source(
                        &mut syntax,
                        &rule.source,
                        implementation_source,
                    )?;
                }
            }
            unit.set_syntax_state(syntax);
            Ok::<(), String>(())
        })
        .map_err(eval_err)?;
    }
    for hook in source_string_declaration_args(source, "syntax-hook", 2)? {
        unit.with_unit_mut(|unit| {
            let mut syntax = unit.syntax_state().clone();
            let hooks = syntax
                .grammar_metadata("semantic_hook_functions")
                .cloned()
                .unwrap_or_else(|| SemanticValue::Map(Vec::new()));
            let mut entries = match hooks {
                SemanticValue::Map(entries) => entries,
                _ => Vec::new(),
            };
            entries.retain(|(key, _)| key != &hook[0]);
            entries.push((hook[0].clone(), SemanticValue::Str(hook[1].clone())));
            syntax.set_grammar_metadata("semantic_hook_functions", SemanticValue::Map(entries))?;
            unit.set_syntax_state(syntax);
            Ok::<(), String>(())
        })
        .map_err(eval_err)?;
    }
    for metadata in source_string_declaration_args(source, "syntax-metadata", 2)? {
        unit.with_unit_mut(|unit| {
            let mut syntax = unit.syntax_state().clone();
            syntax.set_grammar_metadata(
                metadata[0].clone(),
                SemanticValue::Str(metadata[1].clone()),
            )?;
            unit.set_syntax_state(syntax);
            Ok::<(), String>(())
        })
        .map_err(eval_err)?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
struct SourceSyntaxRuleDeclaration {
    source: String,
    implementation: SourceSyntaxRuleImplementation,
}

#[derive(Clone, Debug, PartialEq)]
enum SourceSyntaxRuleImplementation {
    FunctionName(String),
    InlineSource(String),
}

fn source_syntax_rule_declarations(
    source: &str,
) -> Result<Vec<SourceSyntaxRuleDeclaration>, EvalSignal> {
    let trimmed_source = source.trim();
    let parsed = crate::frontend::parse_forms(trimmed_source).map_err(eval_err)?;
    let mut declarations = Vec::new();
    for form in parsed.forms {
        let ParsedForm::List { items, .. } = form else {
            continue;
        };
        let Some(ParsedForm::Symbol { text, .. }) = items.first() else {
            continue;
        };
        if text != "define-syntax-rule" {
            continue;
        }
        let [_, rule, implementation] = items.as_slice() else {
            continue;
        };
        let ParsedForm::String { value: rule, .. } = rule else {
            return Err(eval_err(
                "define-syntax-rule expects rule source as a string",
            ));
        };
        let implementation = match implementation {
            ParsedForm::String {
                value: function_name,
                ..
            } => SourceSyntaxRuleImplementation::FunctionName(function_name.clone()),
            ParsedForm::List { span, .. } => {
                let implementation_source =
                    trimmed_source.get(span.start..span.end).ok_or_else(|| {
                        eval_err("define-syntax-rule inline implementation span is out of bounds")
                    })?;
                SourceSyntaxRuleImplementation::InlineSource(
                    crate::syntax_authoring::extract_inline_lambda_source(implementation_source)
                        .map(str::to_string)
                        .map_err(eval_err)?,
                )
            }
            _ => {
                return Err(eval_err(
                    "define-syntax-rule expects a function-name string or inline lambda implementation",
                ));
            }
        };
        declarations.push(SourceSyntaxRuleDeclaration {
            source: rule.clone(),
            implementation,
        });
    }
    Ok(declarations)
}

fn source_string_declaration_args(
    source: &str,
    head: &str,
    arity: usize,
) -> Result<Vec<Vec<String>>, EvalSignal> {
    let parsed = crate::frontend::parse_forms(source).map_err(eval_err)?;
    let mut matches = Vec::new();
    for form in parsed.forms {
        let ParsedForm::List { items, .. } = form else {
            continue;
        };
        let Some(ParsedForm::Symbol { text, .. }) = items.first() else {
            continue;
        };
        if text != head || items.len() != arity + 1 {
            continue;
        }
        let mut args = Vec::with_capacity(arity);
        for item in &items[1..] {
            let ParsedForm::String { value, .. } = item else {
                return Err(eval_err(format!("{head} expects string arguments")));
            };
            args.push(value.clone());
        }
        matches.push(args);
    }
    Ok(matches)
}

fn optional_string_set(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<std::collections::BTreeSet<String>>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(RuntimeValue::Tuple(items)) => items
            .iter()
            .map(|item| require_named_string(item, message))
            .collect::<Result<std::collections::BTreeSet<_>, _>>()
            .map(Some),
        Some(RuntimeValue::List(items)) => items
            .borrow()
            .iter()
            .map(|item| require_named_string(item, message))
            .collect::<Result<std::collections::BTreeSet<_>, _>>()
            .map(Some),
        Some(_) => Err(eval_err(
            "ctfe-compiler-parse-surface-file-forms expects leading heads to be a sequence",
        )),
    }
}

fn string_sequence(
    value: &RuntimeValue,
    sequence_message: &str,
    item_message: &str,
) -> Result<Vec<String>, EvalSignal> {
    match value {
        RuntimeValue::Tuple(items) => items
            .iter()
            .map(|item| require_named_string(item, item_message))
            .collect(),
        RuntimeValue::List(items) => items
            .borrow()
            .iter()
            .map(|item| require_named_string(item, item_message))
            .collect(),
        _ => Err(eval_err(sequence_message)),
    }
}

#[derive(Clone, Debug)]
struct BootstrapPlanEntryValue {
    path: String,
    depends: Vec<String>,
}

fn bootstrap_plan_entries(
    value: &RuntimeValue,
) -> Result<Vec<BootstrapPlanEntryValue>, EvalSignal> {
    let items: Vec<RuntimeValue> = match value {
        RuntimeValue::Tuple(items) => items.iter().cloned().collect(),
        RuntimeValue::List(items) => items.borrow().iter().cloned().collect(),
        _ => {
            return Err(eval_err(
                "ctfe-compiler-order-bootstrap-plan expects a sequence of entries",
            ));
        }
    };
    items.iter().map(bootstrap_plan_entry).collect()
}

fn bootstrap_plan_entry(value: &RuntimeValue) -> Result<BootstrapPlanEntryValue, EvalSignal> {
    let RuntimeValue::Map(fields) = value else {
        return Err(eval_err(
            "ctfe-compiler-order-bootstrap-plan entries must be maps",
        ));
    };
    let fields = fields.borrow();
    let path = fields.get(&MapKey::Str("path".into())).ok_or_else(|| {
        bootstrap_plan_error("invalid_entry", "bootstrap plan entry is missing path")
    })?;
    let path = require_named_string(
        path,
        "ctfe-compiler-order-bootstrap-plan entry path must be a non-empty string",
    )?;
    let depends = match fields.get(&MapKey::Str("depends".into())) {
        Some(value) => string_sequence(
            value,
            "ctfe-compiler-order-bootstrap-plan entry depends must be a sequence",
            "ctfe-compiler-order-bootstrap-plan dependencies must be non-empty strings",
        )?,
        None => Vec::new(),
    };
    Ok(BootstrapPlanEntryValue { path, depends })
}

fn order_bootstrap_plan_entries(
    entries: &[BootstrapPlanEntryValue],
) -> Result<Vec<String>, EvalSignal> {
    let mut index = BTreeMap::<String, &BootstrapPlanEntryValue>::new();
    for entry in entries {
        if index.insert(entry.path.clone(), entry).is_some() {
            return Err(bootstrap_plan_error(
                "duplicate_entry",
                "bootstrap plan contains duplicate path",
            ));
        }
    }

    let mut ordered = Vec::with_capacity(entries.len());
    let mut visiting = BTreeSet::<String>::new();
    let mut visited = BTreeSet::<String>::new();
    for entry in entries {
        visit_bootstrap_plan_entry(
            &entry.path,
            &index,
            &mut visiting,
            &mut visited,
            &mut ordered,
        )?;
    }
    Ok(ordered)
}

fn visit_bootstrap_plan_entry(
    path: &str,
    index: &BTreeMap<String, &BootstrapPlanEntryValue>,
    visiting: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
    ordered: &mut Vec<String>,
) -> Result<(), EvalSignal> {
    if visited.contains(path) {
        return Ok(());
    }
    if !index.contains_key(path) {
        return Err(bootstrap_plan_error(
            "missing_dependency",
            "bootstrap plan dependency is not declared as an entry",
        ));
    }
    if !visiting.insert(path.to_string()) {
        return Err(bootstrap_plan_error(
            "cycle",
            "bootstrap plan contains a dependency cycle",
        ));
    }
    let entry = index
        .get(path)
        .expect("bootstrap plan entry existence checked above");
    for dependency in &entry.depends {
        visit_bootstrap_plan_entry(dependency, index, visiting, visited, ordered)?;
    }
    visiting.remove(path);
    visited.insert(path.to_string());
    ordered.push(path.to_string());
    Ok(())
}

fn bootstrap_plan_error(code: &str, message: &str) -> EvalSignal {
    eval_err(format!("stdlib.module.bootstrap_plan.{code}: {message}"))
}

fn optional_string_sequence(
    value: Option<&RuntimeValue>,
    sequence_message: &str,
    item_message: &str,
) -> Result<Vec<String>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(Vec::new()),
        Some(value) => string_sequence(value, sequence_message, item_message),
    }
}

fn form_record_to_value(form: &ParsedForm) -> RuntimeValue {
    map([
        ("kind", string(form_kind(form))),
        ("head", optional_string(form.head_symbol())),
        (
            "args",
            tuple(form_args(form).iter().map(form_record_to_value).collect()),
        ),
        ("value", form_atom_value(form)),
    ])
}

fn form_kind(form: &ParsedForm) -> &'static str {
    match form {
        ParsedForm::List { .. } => "list",
        ParsedForm::Symbol { .. } => "symbol",
        ParsedForm::String { .. } => "string",
        ParsedForm::Integer { .. } => "integer",
        ParsedForm::Boolean { .. } => "boolean",
        ParsedForm::Null { .. } => "null",
    }
}

fn form_args(form: &ParsedForm) -> &[ParsedForm] {
    match form {
        ParsedForm::List { items, .. } if !items.is_empty() => &items[1..],
        ParsedForm::List { items, .. } => items,
        _ => &[],
    }
}

fn form_atom_value(form: &ParsedForm) -> RuntimeValue {
    match form {
        ParsedForm::Symbol { text, .. } => string(text),
        ParsedForm::String { value, .. } => string(value),
        ParsedForm::Integer { value, .. } => RuntimeValue::Int(*value),
        ParsedForm::Boolean { value, .. } => RuntimeValue::Bool(*value),
        ParsedForm::Null { .. } => RuntimeValue::Null,
        ParsedForm::List { .. } => RuntimeValue::Null,
    }
}

fn query_plan_source_and_phase(
    args: &[RuntimeValue],
) -> Result<Option<(QueryArtifactSource, PhasePolicy)>, EvalSignal> {
    let Some(source_value) = args.get(2) else {
        return Ok(None);
    };
    if matches!(source_value, RuntimeValue::Null) {
        return Ok(None);
    }
    if args.len() == 3 && query_plan_phase_label_value(source_value) {
        return Err(eval_err(
            "ctfe-compiler-query-plan phase must be the fourth argument; pass null as the source when planning without a source",
        ));
    }
    let source = query_artifact_source_with_message(
        source_value,
        "ctfe-compiler-query-plan expects a unit handle or path-like source",
    )?;
    let phase = args
        .get(3)
        .map(|value| phase_arg(value, "ctfe-compiler-query-plan expects a valid phase"))
        .transpose()?
        .unwrap_or(PhasePolicy::CompileTime);
    Ok(Some((source, phase)))
}

fn query_plan_default_phase(args: &[RuntimeValue]) -> Result<PhasePolicy, EvalSignal> {
    let Some(value) = args.get(3) else {
        return Ok(PhasePolicy::CompileTime);
    };
    phase_arg(value, "ctfe-compiler-query-plan expects a valid phase")
}

fn query_plan_phase_label_value(value: &RuntimeValue) -> bool {
    matches!(
        value,
        RuntimeValue::Str(label)
            if matches!(
                label.as_ref(),
                "runtime" | "compile_time" | "compile-time" | "dual"
            )
    )
}

fn phase_arg(value: &RuntimeValue, message: &str) -> Result<PhasePolicy, EvalSignal> {
    match value {
        RuntimeValue::Null => Ok(PhasePolicy::CompileTime),
        RuntimeValue::Str(value) => match value.as_ref() {
            "runtime" => Ok(PhasePolicy::Runtime),
            "compile_time" | "compile-time" => Ok(PhasePolicy::CompileTime),
            "dual" => Ok(PhasePolicy::Dual),
            _ => Err(eval_err(message)),
        },
        _ => Err(eval_err(message)),
    }
}

fn phase_and_initial_options(
    args: &[RuntimeValue],
    phase_index: usize,
    initial_index: usize,
    phase_message: &str,
    initial_message: &str,
) -> Result<(PhasePolicy, QueryExecutionOptions), EvalSignal> {
    let phase = args
        .get(phase_index)
        .map(|value| phase_arg(value, phase_message))
        .transpose()?
        .unwrap_or(PhasePolicy::CompileTime);
    let initial = initial_bindings(args.get(initial_index), initial_message)?;
    Ok((
        phase,
        QueryExecutionOptions::new().with_initial_bindings(initial),
    ))
}

fn compile_unit_options(
    args: &[RuntimeValue],
) -> Result<(Vec<(String, RuntimeValue)>, bool), EvalSignal> {
    let mut initial = Vec::new();
    let mut raise_on_error = true;
    match args.get(2) {
        None | Some(RuntimeValue::Null) => {}
        Some(RuntimeValue::Bool(flag)) if args.len() == 3 => {
            raise_on_error = *flag;
        }
        Some(value) => {
            initial = initial_bindings(
                Some(value),
                if args.len() == 3 {
                    "ctfe-compiler-compile-unit expects an initial bindings map when the third argument is not a boolean"
                } else {
                    "ctfe-compiler-compile-unit expects an initial bindings map as the third argument"
                },
            )?;
            if args.len() == 4 {
                let RuntimeValue::Bool(flag) = &args[3] else {
                    return Err(eval_err(
                        "ctfe-compiler-compile-unit expects a boolean raise_on_error flag as the fourth argument",
                    ));
                };
                raise_on_error = *flag;
            }
        }
    }
    Ok((initial, raise_on_error))
}

fn initial_bindings(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Vec<(String, RuntimeValue)>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(Vec::new()),
        Some(RuntimeValue::Map(map)) => map
            .borrow()
            .iter()
            .map(|(key, value)| match key {
                MapKey::Str(name) if !name.is_empty() => Ok((name.to_string(), value.clone())),
                _ => Err(eval_err(message)),
            })
            .collect(),
        Some(_) => Err(eval_err(message)),
    }
}

fn optional_nonnegative_usize(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<usize, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(0),
        Some(RuntimeValue::Int(value)) if *value >= 0 => Ok(*value as usize),
        Some(_) => Err(eval_err(message)),
    }
}

fn optional_bool(
    value: Option<&RuntimeValue>,
    message: &str,
    default: bool,
) -> Result<bool, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(default),
        Some(RuntimeValue::Bool(value)) => Ok(*value),
        Some(_) => Err(eval_err(message)),
    }
}

fn require_unit_bridge<'a>(
    value: &'a RuntimeValue,
    message: &str,
) -> Result<&'a UnitBridgeValue, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    object
        .as_any()
        .downcast_ref::<UnitBridgeValue>()
        .ok_or_else(|| eval_err(message))
}

fn query_artifact_source(value: &RuntimeValue) -> Result<QueryArtifactSource, EvalSignal> {
    query_artifact_source_with_message(
        value,
        "ctfe-compiler-query-artifact expects a unit handle or path-like source",
    )
}

fn query_artifact_source_with_message(
    value: &RuntimeValue,
    message: &str,
) -> Result<QueryArtifactSource, EvalSignal> {
    if let RuntimeValue::Str(path) = value {
        return Ok(QueryArtifactSource::Path(path.to_string()));
    }
    let unit = require_unit_bridge(value, message)?;
    Ok(QueryArtifactSource::Unit(Box::new(unit.clone_unit())))
}

fn query_source_origin_to_value(source: &QueryArtifactSource) -> RuntimeValue {
    match source {
        QueryArtifactSource::Unit(unit) => {
            map([("kind", string("unit")), ("id", string(unit.unit_id()))])
        }
        QueryArtifactSource::Path(path) => {
            map([("kind", string("path")), ("path", string(path.as_str()))])
        }
        QueryArtifactSource::Text(text) => map([
            ("kind", string("text")),
            ("digest", string(short_source_digest(text))),
        ]),
    }
}

fn short_source_digest(text: &str) -> String {
    ArtifactFingerprint::sha256(text.as_bytes())
        .to_string()
        .chars()
        .take(12)
        .collect()
}

fn cache_stats_to_value(stats: &ArtifactCacheStats) -> RuntimeValue {
    map([
        ("hits", RuntimeValue::Int(stats.hits as i64)),
        ("misses", RuntimeValue::Int(stats.misses as i64)),
        ("generation", RuntimeValue::Int(stats.generation as i64)),
    ])
}

fn bootstrap_trace_to_value(event: &BootstrapTraceEvent) -> RuntimeValue {
    map([
        ("action", string(event.action.as_str())),
        ("target", string(event.target.as_str())),
        ("depth", RuntimeValue::Int(event.depth as i64)),
        ("succeeded", RuntimeValue::Bool(event.succeeded)),
    ])
}

fn query_plan_step_to_value(step: &QueryPlanStep) -> RuntimeValue {
    map([
        ("stage", string(step.stage.as_str())),
        (
            "key",
            step.artifact_key
                .as_ref()
                .map(artifact_key_to_value)
                .unwrap_or(RuntimeValue::Null),
        ),
        ("cached", RuntimeValue::Bool(step.cached)),
    ])
}

fn evaluation_capture_to_value(capture: &EvaluationCapture) -> RuntimeValue {
    map([
        (
            "result",
            capture.value.clone().unwrap_or(RuntimeValue::Null),
        ),
        ("unit_id", string(capture.unit_id.as_str())),
        ("phase", string(capture.phase.as_str())),
        (
            "diagnostics",
            tuple(
                capture
                    .diagnostics
                    .iter()
                    .map(diagnostic_to_value)
                    .collect(),
            ),
        ),
        (
            "bindings",
            map_from_bindings(capture.bindings.iter().map(|(name, value)| (name, value))),
        ),
        (
            "skipped_forms",
            RuntimeValue::Int(capture.skipped_forms as i64),
        ),
    ])
}

pub(crate) fn query_artifact_to_value(artifact: &QueryArtifactProjection) -> RuntimeValue {
    map([
        ("artifact_kind", string(artifact.artifact_kind.as_str())),
        ("stage", string(artifact.stage.as_str())),
        ("family", string(artifact.family.as_str())),
        ("phase", string(artifact.phase.as_str())),
        ("key", artifact_key_to_value(&artifact.key)),
        (
            "origin_key",
            artifact
                .origin_key
                .as_ref()
                .map(artifact_key_to_value)
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "dependencies",
            tuple(
                artifact
                    .dependencies
                    .iter()
                    .map(artifact_key_to_value)
                    .collect(),
            ),
        ),
        (
            "diagnostics",
            tuple(
                artifact
                    .diagnostics
                    .iter()
                    .map(diagnostic_to_value)
                    .collect(),
            ),
        ),
        ("iterations", RuntimeValue::Int(artifact.iterations as i64)),
        (
            "execution_summary",
            tuple(
                artifact
                    .execution_summary
                    .iter()
                    .map(provider_execution_record_to_value)
                    .collect(),
            ),
        ),
        (
            "reads_subjects",
            tuple(artifact.reads_subjects.iter().map(string).collect()),
        ),
        (
            "writes_subjects",
            tuple(artifact.writes_subjects.iter().map(string).collect()),
        ),
        (
            "read_cells",
            tuple(artifact.read_cells.iter().map(string).collect()),
        ),
        (
            "write_cells",
            tuple(artifact.write_cells.iter().map(string).collect()),
        ),
        (
            "reads_files",
            tuple(artifact.reads_files.iter().map(string).collect()),
        ),
        (
            "writes_files",
            tuple(artifact.writes_files.iter().map(string).collect()),
        ),
        ("value", artifact_value_to_value(&artifact.value)),
    ])
}

fn explain_artifact_value(artifact: &RuntimeValue) -> Result<RuntimeValue, EvalSignal> {
    let RuntimeValue::Map(fields) = artifact else {
        return Err(eval_err(
            "ctfe-compiler-explain-artifact expects a query artifact map",
        ));
    };
    let fields = fields.borrow();
    let artifact_kind = required_map_value(&fields, "artifact_kind")?;
    let key = required_map_value(&fields, "key")?;
    let origin_key = fields
        .get(&MapKey::Str("origin_key".into()))
        .cloned()
        .unwrap_or(RuntimeValue::Null);
    let dependencies = required_map_value(&fields, "dependencies")?;
    let diagnostics = required_map_value(&fields, "diagnostics")?;
    let phase = fields
        .get(&MapKey::Str("phase".into()))
        .cloned()
        .unwrap_or(RuntimeValue::Null);
    let stage = required_map_value(&fields, "stage")?;
    let family = fields
        .get(&MapKey::Str("family".into()))
        .cloned()
        .unwrap_or(RuntimeValue::Null);
    let iterations = fields
        .get(&MapKey::Str("iterations".into()))
        .cloned()
        .unwrap_or(RuntimeValue::Null);
    let reads_subjects = optional_artifact_sequence(&fields, "reads_subjects");
    let writes_subjects = optional_artifact_sequence(&fields, "writes_subjects");
    let read_cells = optional_artifact_sequence(&fields, "read_cells");
    let write_cells = optional_artifact_sequence(&fields, "write_cells");
    let reads_files = optional_artifact_sequence(&fields, "reads_files");
    let writes_files = optional_artifact_sequence(&fields, "writes_files");
    Ok(map([
        ("artifact_kind", artifact_kind),
        ("origin_key", origin_key),
        ("key", key.clone()),
        (
            "dependency_count",
            RuntimeValue::Int(sequence_len(&dependencies, "artifact dependencies")? as i64),
        ),
        (
            "diagnostics_count",
            RuntimeValue::Int(sequence_len(&diagnostics, "artifact diagnostics")? as i64),
        ),
        ("diagnostics", diagnostics),
        ("iterations", iterations),
        ("phase", phase),
        ("unit_id", artifact_unit_id_from_key(&key)),
        ("stage", stage),
        ("family", family),
        ("reads_subjects", reads_subjects),
        ("writes_subjects", writes_subjects),
        ("read_cells", read_cells),
        ("write_cells", write_cells),
        ("reads_files", reads_files),
        ("writes_files", writes_files),
    ]))
}

fn optional_artifact_sequence(fields: &HashMap<MapKey, RuntimeValue>, key: &str) -> RuntimeValue {
    fields
        .get(&MapKey::Str(key.into()))
        .cloned()
        .unwrap_or_else(|| tuple(Vec::new()))
}

fn required_map_value(
    fields: &HashMap<MapKey, RuntimeValue>,
    key: &str,
) -> Result<RuntimeValue, EvalSignal> {
    fields
        .get(&MapKey::Str(key.into()))
        .cloned()
        .ok_or_else(|| eval_err(format!("query artifact map is missing {key:?}")))
}

fn sequence_len(value: &RuntimeValue, label: &str) -> Result<usize, EvalSignal> {
    match value {
        RuntimeValue::Tuple(items) => Ok(items.len()),
        RuntimeValue::List(items) => Ok(items.borrow().len()),
        _ => Err(eval_err(format!("{label} must be a tuple or list"))),
    }
}

fn artifact_unit_id_from_key(key: &RuntimeValue) -> RuntimeValue {
    let RuntimeValue::Tuple(parts) = key else {
        return RuntimeValue::Null;
    };
    if !matches!(parts.first(), Some(RuntimeValue::Str(kind)) if kind.as_ref() == "query-stage") {
        return RuntimeValue::Null;
    }
    parts.get(3).cloned().unwrap_or(RuntimeValue::Null)
}

fn explain_plan_step_to_value(step: &QueryPlanStep) -> RuntimeValue {
    map([
        ("stage", string(step.stage.as_str())),
        ("family", RuntimeValue::Null),
        (
            "key",
            step.artifact_key
                .as_ref()
                .map(artifact_key_to_value)
                .unwrap_or(RuntimeValue::Null),
        ),
        ("cached", RuntimeValue::Bool(step.cached)),
        ("cached_artifact", RuntimeValue::Null),
    ])
}

fn invalidation_plan_step_to_value(
    step: &QueryPlanStep,
    invalidation: Option<&ArtifactInvalidationRecord>,
) -> RuntimeValue {
    map([
        ("stage", string(step.stage.as_str())),
        ("family", RuntimeValue::Null),
        (
            "key",
            step.artifact_key
                .as_ref()
                .map(artifact_key_to_value)
                .unwrap_or(RuntimeValue::Null),
        ),
        ("cached", RuntimeValue::Bool(step.cached)),
        (
            "invalidation",
            invalidation
                .map(invalidation_record_to_value)
                .unwrap_or(RuntimeValue::Null),
        ),
    ])
}

fn invalidation_record_to_value(record: &ArtifactInvalidationRecord) -> RuntimeValue {
    map([
        ("reason_kind", string(record.reason_kind.as_str())),
        (
            "lineage_kind",
            optional_string(record.lineage_kind.as_deref()),
        ),
        (
            "invalidated_key",
            artifact_key_to_value(&record.invalidated_key),
        ),
        (
            "replacement_key",
            record
                .replacement_key
                .as_ref()
                .map(artifact_key_to_value)
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "upstream_key",
            record
                .upstream_key
                .as_ref()
                .map(artifact_key_to_value)
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "changed_inputs",
            tuple(record.changed_inputs.iter().map(string).collect()),
        ),
    ])
}

fn provider_schedule_families_to_value(
    bridge: &CompilerBridgeValue,
    steps: &[QueryPlanStep],
) -> Result<Vec<RuntimeValue>, EvalSignal> {
    let mut satisfied = BTreeSet::new();
    let mut families = Vec::with_capacity(steps.len());
    for step in steps {
        let groups = bridge
            .provider_schedule_for_stage_with_satisfied(step.stage.clone(), &satisfied)
            .map_err(eval_err)?;
        let family = provider_schedule_family_to_value(bridge, step, &groups);
        for provider in groups.groups.into_iter().flatten() {
            satisfied.insert(provider.name);
        }
        families.push(family);
    }
    Ok(families)
}

fn provider_schedule_family_to_value(
    bridge: &CompilerBridgeValue,
    step: &QueryPlanStep,
    groups: &QueryProviderSchedule,
) -> RuntimeValue {
    map([
        ("stage", string(step.stage.as_str())),
        (
            "groups",
            tuple(
                groups
                    .groups
                    .iter()
                    .enumerate()
                    .map(|(index, providers)| {
                        map([
                            ("index", RuntimeValue::Int(index as i64)),
                            (
                                "providers",
                                tuple(
                                    providers
                                        .iter()
                                        .map(|provider| {
                                            schedule_provider_to_value(
                                                provider,
                                                bridge
                                                    .provider_dynamic_requires_for(&provider.name),
                                            )
                                        })
                                        .collect(),
                                ),
                            ),
                            (
                                "barrier_after",
                                provider_schedule_barrier_to_value(
                                    index,
                                    groups.barriers.get(index).and_then(Option::as_ref),
                                ),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

fn provider_schedule_barrier_to_value(index: usize, reasons: Option<&Vec<String>>) -> RuntimeValue {
    match reasons {
        Some(reasons) => map([
            ("next_group_index", RuntimeValue::Int(index as i64 + 1)),
            ("reasons", string_tuple(reasons.iter())),
        ]),
        None => RuntimeValue::Null,
    }
}

fn schedule_provider_to_value(
    provider: &QueryProvider,
    dynamic_requires: Vec<String>,
) -> RuntimeValue {
    map([
        ("name", string(provider.name.clone())),
        ("stage", string(provider.stage.clone())),
        ("family", optional_string(provider.family.as_deref())),
        ("phase_policy", string(provider.phase_policy.as_str())),
        ("internal", RuntimeValue::Bool(false)),
        (
            "effects",
            map([
                ("reads", string_tuple(provider.reads.iter())),
                ("writes", string_tuple(provider.writes.iter())),
                ("emits", string_tuple(provider.effect_tags.iter())),
                ("uses", tuple(Vec::new())),
            ]),
        ),
        ("requires", string_tuple(provider.requires.iter())),
        ("dynamic_requires", string_tuple(dynamic_requires.iter())),
        ("requires_data", string_tuple(provider.requires_data.iter())),
        ("provides_data", string_tuple(provider.provides_data.iter())),
        (
            "input_schema",
            optional_string(provider.input_schema.as_deref()),
        ),
        ("reads", string_tuple(provider.reads.iter())),
        ("writes", string_tuple(provider.writes.iter())),
        ("cache_scope", string(provider.cache_scope.clone())),
        ("resume_policy", string(provider.resume_policy.clone())),
    ])
}

fn provider_execution_record_to_value(record: &QueryProviderExecutionRecord) -> RuntimeValue {
    map([
        ("provider_name", string(record.provider_name.as_str())),
        ("stage", string(record.stage.as_str())),
        ("family", optional_string(record.family.as_deref())),
        (
            "provider_contract",
            provider_execution_contract_to_value(record),
        ),
        ("iteration", RuntimeValue::Int(record.iteration as i64)),
        ("changed", RuntimeValue::Bool(record.changed)),
        (
            "diagnostics_emitted",
            RuntimeValue::Int(record.diagnostics_emitted as i64),
        ),
        ("rolled_back", RuntimeValue::Bool(record.rolled_back)),
        (
            "stopped_by_error",
            RuntimeValue::Bool(record.stopped_by_error),
        ),
        ("outcome_kind", string(record.outcome_kind.as_str())),
        (
            "diagnostic_codes",
            tuple(record.diagnostic_codes.iter().map(string).collect()),
        ),
        (
            "artifact_dependencies",
            tuple(
                record
                    .artifact_dependencies
                    .iter()
                    .map(artifact_key_to_value)
                    .collect(),
            ),
        ),
        (
            "rewrite_count",
            RuntimeValue::Int(record.rewrite_count as i64),
        ),
        (
            "erased_count",
            RuntimeValue::Int(record.erased_count as i64),
        ),
        (
            "touched_node_kinds",
            tuple(record.touched_node_kinds.iter().map(string).collect()),
        ),
        (
            "reads_subjects",
            tuple(record.reads_subjects.iter().map(string).collect()),
        ),
        (
            "writes_subjects",
            tuple(record.writes_subjects.iter().map(string).collect()),
        ),
        (
            "read_cells",
            tuple(record.read_cells.iter().map(string).collect()),
        ),
        (
            "write_cells",
            tuple(record.write_cells.iter().map(string).collect()),
        ),
        (
            "reads_files",
            tuple(record.reads_files.iter().map(string).collect()),
        ),
        (
            "writes_files",
            tuple(record.writes_files.iter().map(string).collect()),
        ),
        (
            "change_domains",
            tuple(record.change_domains.iter().map(string).collect()),
        ),
        (
            "restart_requested",
            RuntimeValue::Bool(record.restart_requested),
        ),
        (
            "restart_stage",
            optional_string(record.restart_stage.as_deref()),
        ),
        (
            "outcome_summary",
            map_from_string_pairs(record.outcome_summary.iter()),
        ),
    ])
}

fn provider_execution_contract_to_value(record: &QueryProviderExecutionRecord) -> RuntimeValue {
    map([
        ("phase_policy", string(record.phase_policy.as_str())),
        ("internal", RuntimeValue::Bool(false)),
        (
            "effects",
            map([
                ("reads", string_tuple(record.reads.iter())),
                ("writes", string_tuple(record.writes.iter())),
                ("emits", string_tuple(record.effect_tags.iter())),
                ("uses", tuple(Vec::new())),
            ]),
        ),
        ("requires", string_tuple(record.requires.iter())),
        ("requires_data", string_tuple(record.requires_data.iter())),
        ("provides_data", string_tuple(record.provides_data.iter())),
        ("reads", string_tuple(record.reads.iter())),
        ("writes", string_tuple(record.writes.iter())),
        ("cache_scope", string(record.cache_scope.as_str())),
        ("resume_policy", string(record.resume_policy.as_str())),
    ])
}

fn with_query_summary(
    summary: RuntimeValue,
    target: &str,
    phase: PhasePolicy,
) -> Result<RuntimeValue, EvalSignal> {
    let RuntimeValue::Map(fields) = &summary else {
        return Err(eval_err("compiler explain summary must be a map"));
    };
    fields.borrow_mut().insert(
        MapKey::Str("query".into()),
        map([
            ("target", string(target)),
            ("phase", string(phase.as_str())),
        ]),
    );
    Ok(summary)
}

fn artifact_key_to_value(key: &ArtifactKey) -> RuntimeValue {
    tuple(key.parts().iter().map(string).collect())
}

fn artifact_value_to_value(value: &ArtifactValue) -> RuntimeValue {
    match value {
        ArtifactValue::Text(text) => map([("kind", string("text")), ("value", string(text))]),
        ArtifactValue::Bytes(bytes) => map([
            ("kind", string("bytes")),
            (
                "value",
                tuple(
                    bytes
                        .iter()
                        .map(|byte| RuntimeValue::Int(*byte as i64))
                        .collect(),
                ),
            ),
        ]),
        ArtifactValue::Source(source) => {
            let (origin_kind, origin_value) = match &source.origin {
                SourceOrigin::Inline { label } => ("inline", label.as_str()),
                SourceOrigin::Path { path, .. } => ("path", path.as_str()),
            };
            map([
                ("kind", string("source")),
                ("origin_kind", string(origin_kind)),
                ("origin", string(origin_value)),
                ("fingerprint", string(source.fingerprint.as_str())),
                ("text", string(source.text.as_str())),
            ])
        }
        ArtifactValue::Semantic(value) => map([
            ("kind", string("semantic")),
            ("value", semantic_value_to_runtime(value)),
        ]),
    }
}

fn semantic_value_to_runtime(value: &SemanticValue) -> RuntimeValue {
    match value {
        SemanticValue::Null => RuntimeValue::Null,
        SemanticValue::Bool(value) => RuntimeValue::Bool(*value),
        SemanticValue::Int(value) => RuntimeValue::Int(*value),
        SemanticValue::Float(value) => RuntimeValue::Float(*value),
        SemanticValue::Str(value) => string(value),
        SemanticValue::Node(node_id) => RuntimeValue::Int(*node_id as i64),
        SemanticValue::List(items) => RuntimeValue::List(Rc::new(RefCell::new(
            items.iter().map(semantic_value_to_runtime).collect(),
        ))),
        SemanticValue::Map(entries) => {
            let mut map = HashMap::new();
            for (key, value) in entries {
                map.insert(
                    MapKey::Str(key.as_str().into()),
                    semantic_value_to_runtime(value),
                );
            }
            RuntimeValue::Map(Rc::new(RefCell::new(map)))
        }
    }
}

fn semantic_policy_to_value(policy: &SemanticPolicyRegistration) -> RuntimeValue {
    map([
        ("name", string(policy.name.as_str())),
        ("source", string("registered")),
        ("phase", string(policy.phase_policy.as_str())),
        ("effect", string(effect_policy_label(policy))),
        ("eval", string(policy.eval_policy.as_str())),
        ("control", string(policy.control_policy.as_str())),
        ("scope", string(policy.scope_policy.as_str())),
        ("form", string(policy.form_policy.as_str())),
        ("has_normalizer", RuntimeValue::Bool(true)),
        ("unit_id", optional_string(policy.unit_id.as_deref())),
        ("stable_id", optional_string(policy.stable_id.as_deref())),
    ])
}

fn effect_policy_label(policy: &SemanticPolicyRegistration) -> String {
    if policy.effect_policy.is_pure() {
        "pure".to_string()
    } else {
        policy.effect_policy.tags().join("|")
    }
}

fn diagnostic_to_value(diagnostic: &Diagnostic) -> RuntimeValue {
    map([
        ("severity", string(diagnostic.severity.as_str())),
        ("message", string(diagnostic.message.as_str())),
        ("code", optional_string(diagnostic.code.as_deref())),
        ("label", optional_string(diagnostic.label.as_deref())),
        ("location", optional_string(diagnostic.location.as_deref())),
        (
            "notes",
            tuple(diagnostic.notes.iter().map(string).collect()),
        ),
        ("help", tuple(diagnostic.help.iter().map(string).collect())),
        (
            "context",
            tuple(diagnostic.context.iter().map(string).collect()),
        ),
        (
            "fixes",
            tuple(
                diagnostic
                    .fixes
                    .iter()
                    .map(diagnostic_fix_to_value)
                    .collect(),
            ),
        ),
        (
            "stack_trace",
            tuple(
                diagnostic
                    .stack_trace
                    .iter()
                    .map(diagnostic_frame_to_value)
                    .collect(),
            ),
        ),
    ])
}

fn diagnostic_fix_to_value(fix: &DiagnosticFix) -> RuntimeValue {
    map([
        ("label", string(fix.label.as_str())),
        ("kind", string(fix.kind.as_str())),
        ("metadata", map_from_string_pairs(&fix.metadata)),
    ])
}

fn diagnostic_frame_to_value(frame: &DiagnosticFrame) -> RuntimeValue {
    map([
        ("name", string(frame.name.as_str())),
        ("location", optional_string(frame.location.as_deref())),
    ])
}

fn map<const N: usize>(entries: [(&str, RuntimeValue); N]) -> RuntimeValue {
    let mut map = HashMap::new();
    for (key, value) in entries {
        map.insert(MapKey::Str(key.into()), value);
    }
    RuntimeValue::Map(Rc::new(RefCell::new(map)))
}

fn map_from_string_pairs<'a>(
    entries: impl IntoIterator<Item = &'a (String, String)>,
) -> RuntimeValue {
    let mut map = HashMap::new();
    for (key, value) in entries {
        map.insert(MapKey::Str(key.clone().into()), string(value));
    }
    RuntimeValue::Map(Rc::new(RefCell::new(map)))
}

fn map_from_bindings<'a>(
    entries: impl IntoIterator<Item = (&'a String, &'a RuntimeValue)>,
) -> RuntimeValue {
    let mut map = HashMap::new();
    for (key, value) in entries {
        map.insert(MapKey::Str(key.clone().into()), value.clone());
    }
    RuntimeValue::Map(Rc::new(RefCell::new(map)))
}

fn tuple(items: Vec<RuntimeValue>) -> RuntimeValue {
    RuntimeValue::Tuple(items.into())
}

fn string_tuple<'a>(items: impl IntoIterator<Item = &'a String>) -> RuntimeValue {
    tuple(items.into_iter().map(string).collect())
}

fn string(value: impl AsRef<str>) -> RuntimeValue {
    RuntimeValue::Str(value.as_ref().into())
}

fn optional_string(value: Option<&str>) -> RuntimeValue {
    value.map(string).unwrap_or(RuntimeValue::Null)
}
