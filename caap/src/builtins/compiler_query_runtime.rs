/// Compiler query/evaluation CTFE builtins — initial port of
/// `caap/builtins/compiler/query.py` for APIs backed by current Rust state.
use std::rc::Rc;

use crate::builtins::compiler_registry::{require_compiler_bridge, require_named_string};
use crate::compiler::UnitBridgeValue;
use crate::eval::{eval_args, Evaluator};
use crate::values::{eval_err, RuntimeValue};

use super::compiler_query_helpers::{
    diagnostic_to_value, dir_entry_to_value, evaluation_capture_to_value, initial_bindings,
    invalidation_plan_step_to_value, list_dir, load_dynamic_surface_file_template,
    load_leading_parenthesized_surface_file_template, map, optional_nonnegative_usize,
    optional_string_sequence, phase_and_initial_options, phase_arg, provider_schedule_to_value,
    query_artifact_source, query_artifact_to_value, query_source_origin_to_value,
    require_string_set, require_unit_bridge, schedule_provider_to_value, semantic_policy_to_value,
    stage_to_value, string, surface_load_options, tuple,
};

pub fn register(ev: &mut Evaluator) {
    ev.register_special(
        "ctfe_compiler_list_dir",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_read_files(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-list-dir expects a compiler bridge",
            )?;
            let path = require_named_string(
                &args[1],
                "ctfe-compiler-list-dir expects a non-empty string path",
            )?;
            let resolved = bridge
                .resolve_bootstrap_source_path(std::path::Path::new(&path))
                .map_err(eval_err)?;
            let entries = list_dir(&resolved).map_err(eval_err)?;
            Ok(tuple(entries.iter().map(dir_entry_to_value).collect()))
        },
    );

    ev.register_special(
        "ctfe_compiler_is_file",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_read_files(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-is-file expects a compiler bridge",
            )?;
            let path = require_named_string(
                &args[1],
                "ctfe-compiler-is-file expects a non-empty string path",
            )?;
            bridge
                .bootstrap_source_path_is_file(std::path::Path::new(&path))
                .map(RuntimeValue::Bool)
                .map_err(eval_err)
        },
    );

    ev.register_special(
        "ctfe_compiler_list_stages",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-list-stages expects a compiler bridge",
            )?;
            Ok(tuple(
                bridge.list_stages().iter().map(stage_to_value).collect(),
            ))
        },
    );

    ev.register_special(
        "ctfe_compiler_list_providers",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-list-providers expects a compiler bridge",
            )?;
            Ok(tuple(
                bridge
                    .list_providers()
                    .iter()
                    .map(|provider| {
                        schedule_provider_to_value(
                            provider,
                            bridge.provider_dynamic_requires_for(&provider.name),
                        )
                    })
                    .collect(),
            ))
        },
    );

    ev.register_special(
        "ctfe_compiler_execute_bootstrap_file",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
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
        },
    );

    ev.register_special("ctfe_compiler_evaluate_bootstrap_file", 2, Some(5), crate::values::BuiltinMetadata::compile_time_compiler_registry(), |ev, call, env| {
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
            let capture = bridge
                .evaluate_bootstrap_file(
                    path,
                    initial,
                    capabilities,
                    skip_leading_forms,
                    args[0].clone(),
                )
                .map_err(eval_err)?;
            Ok(evaluation_capture_to_value(&capture))
        });

    // One-call grammar-parse API: merge the syntax units' grammars + inline
    // lower hooks, parse TEXT (not a file), return the lowered surface forms
    // as data. A parse failure of the text is DATA ({ok:false, error}), not an
    // evaluation error — language-building callers branch on it; setup
    // problems (bad units, bad start rule) stay hard errors.
    ev.register_special(
        "ctfe_grammar_parse_forms",
        3,
        Some(5),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-grammar-parse-forms expects a compiler bridge",
            )?;
            let units = super::compiler_query_helpers::syntax_unit_sequence(
                &args[1],
                "ctfe-grammar-parse-forms expects a list of grammar unit handles",
            )?;
            let text = require_named_string(
                &args[2],
                "ctfe-grammar-parse-forms expects a source text string",
            )?;
            let start = match args.get(3) {
                None | Some(RuntimeValue::Null) => None,
                Some(value) => Some(require_named_string(
                    value,
                    "ctfe-grammar-parse-forms start rule must be a non-empty string",
                )?),
            };
            // Optional REAL source path: spans in the returned forms point at
            // it instead of the synthetic <ctfe_grammar_parse_forms> marker
            // (pass null start to give a path without overriding the rule).
            let path = match args.get(4) {
                None | Some(RuntimeValue::Null) => None,
                Some(value) => Some(require_named_string(
                    value,
                    "ctfe-grammar-parse-forms path must be a non-empty string",
                )?),
            };
            let outcome = super::compiler_query_helpers::parse_dynamic_surface_text(
                bridge,
                &text,
                path.as_deref().unwrap_or("<ctfe_grammar_parse_forms>"),
                units,
                std::collections::HashMap::new(),
                start.as_deref(),
            )?;
            Ok(match outcome {
                Ok(parse_value) => {
                    // Rich surface-form maps STRAIGHT from the parse value —
                    // no ParsedForm narrowing, so producing-rule names, honest
                    // delimiters and raw_text survive to the caller.
                    let forms = crate::surface_syntax::parse_value_to_rich_forms(&parse_value)
                        .map_err(eval_err)?;
                    super::compiler_query_helpers::map([
                        ("ok", RuntimeValue::Bool(true)),
                        (
                            "forms",
                            RuntimeValue::List(std::rc::Rc::new(std::cell::RefCell::new(forms))),
                        ),
                    ])
                }
                Err(message) => super::compiler_query_helpers::map([
                    ("ok", RuntimeValue::Bool(false)),
                    ("error", RuntimeValue::Str(message.into())),
                ]),
            })
        },
    );

    ev.register_special("ctfe_compiler_load_surface_file_template", 2, Some(3), crate::values::BuiltinMetadata::compile_time_read_files(), |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-load-surface-file-template expects a compiler bridge",
            )?;
            let path = require_named_string(
                &args[1],
                "ctfe-compiler-load-surface-file-template expects a non-empty string path",
            )?;
            let options = surface_load_options(args.get(2))?;
            let resolved = bridge
                .resolve_bootstrap_source_path(std::path::Path::new(&path))
                .map_err(eval_err)?;
            let unit = if options.leading_parenthesized_forms_only {
                if !options.syntax_units.is_empty() || !options.hooks.is_empty() {
                    return Err(eval_err(
                        "ctfe-compiler-load-surface-file-template option leading_parenthesized_forms_only cannot be combined with syntax_units or hooks",
                    ));
                }
                load_leading_parenthesized_surface_file_template(
                    &resolved,
                    options.unit_id,
                    &options.leading_parenthesized_heads,
                )?
            } else if options.syntax_units.is_empty() {
                bridge
                    .load_surface_unit_template_with_unit_id(resolved, options.unit_id)
                    .map_err(eval_err)?
            } else {
                let unit_id = options.unit_id.ok_or_else(|| {
                    eval_err(
                        "ctfe-compiler-load-surface-file-template options require unit_id when syntax_units are provided",
                    )
                })?;
                load_dynamic_surface_file_template(
                    bridge,
                    &resolved,
                    &unit_id,
                    options.syntax_units,
                    options.hooks,
                )?
            };
            Ok(RuntimeValue::HostObject(Rc::new(unit)))
        });

    ev.register_special(
        "ctfe_compiler_current_bootstrap_context",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-current-bootstrap-context expects a compiler bridge",
            )?;
            let (path, capabilities) = bridge.current_bootstrap_context();
            Ok(map([
                ("path", path.map(string).unwrap_or(RuntimeValue::Null)),
                (
                    "capabilities",
                    tuple(capabilities.iter().map(string).collect()),
                ),
            ]))
        },
    );

    ev.register_special(
        "ctfe_compiler_provider_schedule",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-provider-schedule expects a compiler bridge",
            )?;
            let stage = require_named_string(
                &args[1],
                "ctfe-compiler-provider-schedule expects a non-empty stage or target name",
            )?;
            let satisfied = match args.get(2) {
                Some(value) => require_string_set(
                    value,
                    "ctfe-compiler-provider-schedule expects a list of satisfied provider names",
                )?,
                None => std::collections::BTreeSet::new(),
            };
            let schedule = bridge
                .provider_schedule_for_stage_with_satisfied(stage.clone(), &satisfied)
                .map_err(eval_err)?;
            Ok(provider_schedule_to_value(
                bridge,
                stage.as_str(),
                &schedule,
            ))
        },
    );

    ev.register_special(
        "ctfe_compiler_list_semantic_policies",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
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
        },
    );

    ev.register_special(
        "ctfe_compiler_query_execution",
        3,
        Some(5),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-query-execution expects a compiler bridge",
            )?;
            let target = require_named_string(
                &args[1],
                "ctfe-compiler-query-execution expects a non-empty target name",
            )?;
            let source = query_artifact_source(&args[2])?;
            let (phase, options) = phase_and_initial_options(
                &args,
                3,
                4,
                "ctfe-compiler-query-execution expects a valid phase",
                "ctfe-compiler-query-execution expects an initial bindings map when provided",
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
                (
                    "result",
                    execution
                        .artifact
                        .as_ref()
                        .map(query_artifact_to_value)
                        .unwrap_or(RuntimeValue::Null),
                ),
                (
                    "execution_diagnostics",
                    tuple(
                        execution
                            .execution_diagnostics
                            .iter()
                            .map(diagnostic_to_value)
                            .collect(),
                    ),
                ),
                (
                    "unit",
                    RuntimeValue::HostObject(Rc::new(UnitBridgeValue::from_unit_snapshot(
                        execution.unit.clone(),
                    ))),
                ),
            ]))
        },
    );

    ev.register_special(
        "ctfe_compiler_register_unit",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-register-unit expects a compiler bridge",
            )?;
            let unit_id = require_named_string(
                &args[1],
                "ctfe-compiler-register-unit expects a non-empty unit id",
            )?;
            let unit = require_unit_bridge(
                &args[2],
                "ctfe-compiler-register-unit expects a unit handle",
            )?
            .clone_unit_snapshot();
            bridge
                .register_compiled_unit(unit_id, &unit)
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        },
    );

    ev.register_special(
        "ctfe_compiler_lookup_unit",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-lookup-unit expects a compiler bridge",
            )?;
            let unit_id = require_named_string(
                &args[1],
                "ctfe-compiler-lookup-unit expects a non-empty unit id",
            )?;
            match bridge.lookup_compiled_unit(&unit_id).map_err(eval_err)? {
                Some(unit) => Ok(RuntimeValue::HostObject(Rc::new(unit))),
                None => Ok(args.get(2).cloned().unwrap_or(RuntimeValue::Null)),
            }
        },
    );

    ev.register_special(
        "ctfe_compiler_evaluate_capture",
        3,
        Some(5),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-evaluate-capture expects a compiler bridge",
            )?;
            let unit = require_unit_bridge(
                &args[1],
                "ctfe-compiler-evaluate-capture expects a unit handle",
            )?
            .clone_unit_snapshot();
            let phase = phase_arg(
                &args[2],
                "ctfe-compiler-evaluate-capture expects a valid phase",
            )?;
            let initial = initial_bindings(
                args.get(3),
                "ctfe-compiler-evaluate-capture expects an initial map when provided",
            )?;
            let skip_leading_forms = optional_nonnegative_usize(
                args.get(4),
                "ctfe-compiler-evaluate-capture expects a non-negative integer skip count",
            )?;
            let capture = bridge
                .evaluate_capture_skipping(
                    &unit,
                    phase,
                    initial,
                    args[0].clone(),
                    skip_leading_forms,
                )
                .map_err(eval_err)?;
            Ok(evaluation_capture_to_value(&capture))
        },
    );
}
