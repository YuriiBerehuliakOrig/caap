//! CTFE builtins exposing the caap-peg *engine* surface to CAAP scripts:
//! incremental parsing, AST + diff, validation/mutation/diff, prefix/profiled
//! parsing, and the cross-grammar registry. Split out of the parent `grammar`
//! module to keep the value-bridge core focused; as a child module it reuses the
//! parent's private host objects and helpers via `super::*`.
use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::builtins::args::{require_string, require_usize};
use crate::eval::{eval_args, Evaluator};
use crate::values::{eval_err, MapKey, RuntimeValue};

pub(super) fn register(ev: &mut Evaluator) {
    // ── Incremental parsing ─────────────────────────────────────────────────
    // ctfe-grammar-parse-cache → reusable parse-cache object
    ev.register_special(
        "ctfe_grammar_parse_cache",
        0,
        Some(0),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            eval_args(ev, call, env)?;
            Ok(RuntimeValue::HostObject(Rc::new(ParseCacheValue {
                cache: RefCell::new(caap_peg::ParseCache::new()),
            })))
        },
    );

    // ctfe-grammar-parse-incremental text grammar cache [options] [semantics]
    //   → {"ok" bool ...}; reuses prior work held in `cache` across edits.
    // Same option/semantics shape as `ctfe-grammar-parse`; `cache` must be a
    // `ctfe-grammar-parse-cache` handle, threaded across successive edits.
    ev.register_special(
        "ctfe_grammar_parse_incremental",
        3,
        Some(5),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let text = require_string(
                &args[0],
                "ctfe_grammar_parse_incremental: text must be a string",
            )?;
            let grammar = &downcast_grammar(&args[1], "ctfe_grammar_parse_incremental")?.grammar;
            let cache_obj = downcast_parse_cache(&args[2], "ctfe_grammar_parse_incremental")?;
            let (config, semantics) =
                parse_options_and_semantics(&args, 3, "ctfe_grammar_parse_incremental")?;
            let semantics = semantics.filter(|value| !matches!(value, RuntimeValue::Null));
            let mut cache = cache_obj.cache.borrow_mut();
            let result;
            let mut captured_err = None;
            if let Some(semantics) = semantics {
                let driver = make_caap_driver(ev, &semantics);
                result = caap_peg::ParseRequest::new(grammar)
                    .config(config)
                    .driver(&driver)
                    .run_incremental(&text, &mut cache);
                captured_err = driver.error.borrow_mut().take();
            } else {
                result = caap_peg::ParseRequest::new(grammar)
                    .config(config)
                    .run_incremental(&text, &mut cache);
            }
            if let Some(signal) = captured_err {
                return Err(signal);
            }
            Ok(parse_result_to_runtime(
                result.map(|value| (*value).clone()),
            ))
        },
    );

    // ctfe-grammar-apply-edits base-text edits → new text
    //   `edits` is a list of {"start" int "old_end" int "replacement" str} maps.
    ev.register_special(
        "ctfe_grammar_apply_edits",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let base = require_string(
                &args[0],
                "ctfe_grammar_apply_edits: base text must be a string",
            )?;
            let edits = incremental_edits_from_runtime(&args[1])?;
            let sequential =
                caap_peg::snapshot_edits_to_sequential(&base, &edits).map_err(|error| {
                    eval_err(format!("ctfe_grammar_apply_edits: {}", error.message))
                })?;
            let new_text = caap_peg::apply_edits(&base, &sequential).map_err(|error| {
                eval_err(format!("ctfe_grammar_apply_edits: {}", error.message))
            })?;
            Ok(str_value(new_text))
        },
    );

    // ── Cross-grammar registry ──────────────────────────────────────────────
    // ctfe-grammar-registry → empty namespaced grammar registry object
    ev.register_special(
        "ctfe_grammar_registry",
        0,
        Some(0),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            eval_args(ev, call, env)?;
            Ok(RuntimeValue::HostObject(Rc::new(GrammarRegistryValue {
                registry: RefCell::new(caap_peg::GrammarRegistry::new()),
            })))
        },
    );

    // ctfe-grammar-registry-register registry name grammar → registry (mutated)
    ev.register_special(
        "ctfe_grammar_registry_register",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let registry = downcast_grammar_registry(&args[0], "ctfe_grammar_registry_register")?;
            let name = require_string(
                &args[1],
                "ctfe_grammar_registry_register: name must be a string",
            )?;
            let grammar =
                grammar_from_runtime_value(&args[2], "ctfe_grammar_registry_register: grammar")?;
            registry
                .registry
                .borrow_mut()
                .register(&name, grammar)
                .map_err(|error| eval_err(format!("ctfe_grammar_registry_register: {error}")))?;
            Ok(args[0].clone())
        },
    );

    // ctfe-grammar-registry-list registry [namespace] → sorted list of names
    ev.register_special(
        "ctfe_grammar_registry_list",
        1,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let registry = downcast_grammar_registry(&args[0], "ctfe_grammar_registry_list")?;
            let namespace = optional_string_arg(
                &args,
                1,
                "ctfe_grammar_registry_list: namespace must be a string",
            )?;
            let mut names = registry.registry.borrow().list(namespace.as_deref());
            names.sort();
            Ok(string_list(&names))
        },
    );

    // ctfe-grammar-parse-with-registry text grammar registry [options] [semantics]
    //   → {ok, value ...}; resolves cross-grammar `name::rule` refs via the registry.
    ev.register_special(
        "ctfe_grammar_parse_with_registry",
        3,
        Some(5),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let text = require_string(
                &args[0],
                "ctfe_grammar_parse_with_registry: text must be a string",
            )?;
            let grammar = &downcast_grammar(&args[1], "ctfe_grammar_parse_with_registry")?.grammar;
            let registry_obj =
                downcast_grammar_registry(&args[2], "ctfe_grammar_parse_with_registry")?;
            let (config, semantics) =
                parse_options_and_semantics(&args, 3, "ctfe_grammar_parse_with_registry")?;
            let semantics = semantics.filter(|value| !matches!(value, RuntimeValue::Null));
            let registry = registry_obj.registry.borrow();
            let result;
            let mut captured_err = None;
            if let Some(semantics) = semantics {
                let driver = make_caap_driver(ev, &semantics);
                result = caap_peg::ParseRequest::new(grammar)
                    .config(config)
                    .registry(&registry)
                    .driver(&driver)
                    .run(&text);
                captured_err = driver.error.borrow_mut().take();
            } else {
                result = caap_peg::ParseRequest::new(grammar)
                    .config(config)
                    .registry(&registry)
                    .run(&text);
            }
            if let Some(signal) = captured_err {
                return Err(signal);
            }
            Ok(parse_result_to_runtime(result))
        },
    );

    // ── Prefix / profiled parsing ───────────────────────────────────────────
    // ctfe-grammar-parse-prefix text grammar [start_pos] [options]
    //   → {value, consumed, eof, errors}; matches a prefix from `start_pos`
    //   (default 0) without requiring the whole input to parse.
    ev.register_special(
        "ctfe_grammar_parse_prefix",
        2,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let text =
                require_string(&args[0], "ctfe_grammar_parse_prefix: text must be a string")?;
            let grammar = &downcast_grammar(&args[1], "ctfe_grammar_parse_prefix")?.grammar;
            let start_pos = optional_usize_arg(
                &args,
                2,
                "ctfe_grammar_parse_prefix: start_pos must be a non-negative integer",
            )?
            .unwrap_or(0);
            let config = parse_config_from_runtime(args.get(3), "ctfe_grammar_parse_prefix")?;
            let result = caap_peg::ParseRequest::new(grammar)
                .config(config)
                .run_prefix(&text, start_pos);
            Ok(prefix_result_map(result))
        },
    );

    // ctfe-grammar-parse-profiled text grammar [options] [semantics]
    //   → {ok, value, profile} | {ok:false, error}; same options/semantics shape
    //   as ctfe-grammar-parse, plus a rule-level profile.
    ev.register_special(
        "ctfe_grammar_parse_profiled",
        2,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let text = require_string(
                &args[0],
                "ctfe_grammar_parse_profiled: text must be a string",
            )?;
            let grammar = &downcast_grammar(&args[1], "ctfe_grammar_parse_profiled")?.grammar;
            let (config, semantics) =
                parse_options_and_semantics(&args, 2, "ctfe_grammar_parse_profiled")?;
            let semantics = semantics.filter(|value| !matches!(value, RuntimeValue::Null));
            let result;
            let mut captured_err = None;
            if let Some(semantics) = semantics {
                let driver = make_caap_driver(ev, &semantics);
                result = caap_peg::ParseRequest::new(grammar)
                    .config(config)
                    .driver(&driver)
                    .run_profiled(&text);
                captured_err = driver.error.borrow_mut().take();
            } else {
                result = caap_peg::ParseRequest::new(grammar)
                    .config(config)
                    .run_profiled(&text);
            }
            if let Some(signal) = captured_err {
                return Err(signal);
            }
            Ok(match result {
                Ok((value, profile)) => map_value(vec![
                    ("ok", RuntimeValue::Bool(true)),
                    ("value", pv_to_rv(value)),
                    ("profile", parse_profile_map(&profile)),
                ]),
                Err(error) => map_value(vec![
                    ("ok", RuntimeValue::Bool(false)),
                    ("error", str_value(error.message.as_ref())),
                ]),
            })
        },
    );

    // ── Validation / mutation / diff ────────────────────────────────────────
    // ctfe-grammar-validate grammar [label]
    //   → {ok, error_count, warning_count, issues:[{message,severity,rule,code}]}
    ev.register_special(
        "ctfe_grammar_validate",
        1,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let grammar = &downcast_grammar(&args[0], "ctfe_grammar_validate")?.grammar;
            let label =
                optional_string_arg(&args, 1, "ctfe_grammar_validate: label must be a string")?;
            let report = caap_peg::validate_grammar_with_label(grammar, label.as_deref());
            Ok(validation_report_map(&report))
        },
    );

    // ctfe-grammar-diff base target → {added_rules, removed_rules, changed_rules, ...}
    ev.register_special(
        "ctfe_grammar_diff",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let base = &downcast_grammar(&args[0], "ctfe_grammar_diff: base")?.grammar;
            let target = &downcast_grammar(&args[1], "ctfe_grammar_diff: target")?.grammar;
            Ok(grammar_diff_map(&caap_peg::diff_grammars(base, target)))
        },
    );

    // ctfe-grammar-signature grammar → int (stable identity for caching/equality)
    ev.register_special(
        "ctfe_grammar_signature",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let grammar = &downcast_grammar(&args[0], "ctfe_grammar_signature")?.grammar;
            // Bit-reinterpret the u64 hash as i64: lossless for equality.
            Ok(RuntimeValue::Int(
                caap_peg::grammar_signature(grammar) as i64
            ))
        },
    );

    // ctfe-grammar-rule-graph grammar → list of {rule, refs} (sorted by rule)
    ev.register_special(
        "ctfe_grammar_rule_graph",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let grammar = &downcast_grammar(&args[0], "ctfe_grammar_rule_graph")?.grammar;
            let mut entries: Vec<_> = caap_peg::rule_graph(grammar).into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Ok(list_value(
                entries
                    .into_iter()
                    .map(|(rule, refs)| {
                        map_value(vec![
                            ("rule", str_value(&rule)),
                            ("refs", string_list(&refs)),
                        ])
                    })
                    .collect(),
            ))
        },
    );

    // ctfe-grammar-nullable-rules grammar → sorted list of nullable rule names
    ev.register_special(
        "ctfe_grammar_nullable_rules",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let grammar = &downcast_grammar(&args[0], "ctfe_grammar_nullable_rules")?.grammar;
            let mut names: Vec<String> = caap_peg::compute_nullable_rules(grammar)
                .into_iter()
                .collect();
            names.sort();
            Ok(string_list(&names))
        },
    );

    // ctfe-grammar-remove-rule grammar name → new grammar (clone with rule removed)
    ev.register_special(
        "ctfe_grammar_remove_rule",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let mut grammar = downcast_grammar(&args[0], "ctfe_grammar_remove_rule")?
                .grammar
                .clone();
            let name = require_string(&args[1], "ctfe_grammar_remove_rule: name must be a string")?;
            caap_peg::remove_rule(&mut grammar, &name)
                .map_err(|error| eval_err(format!("ctfe_grammar_remove_rule: {error:?}")))?;
            Ok(grammar_host_obj(grammar))
        },
    );

    // ctfe-grammar-replace-rule grammar name source → new grammar (clone, rule rebound)
    ev.register_special(
        "ctfe_grammar_replace_rule",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let mut grammar = downcast_grammar(&args[0], "ctfe_grammar_replace_rule")?
                .grammar
                .clone();
            let name =
                require_string(&args[1], "ctfe_grammar_replace_rule: name must be a string")?;
            let source = require_string(
                &args[2],
                "ctfe_grammar_replace_rule: source must be a string",
            )?;
            caap_peg::replace_rule(&mut grammar, &name, &source)
                .map_err(|error| eval_err(format!("ctfe_grammar_replace_rule: {error:?}")))?;
            Ok(grammar_host_obj(grammar))
        },
    );

    // ctfe-grammar-set-metadata grammar key value [owner] → new grammar
    //   Sets `owner.key = value` (owner defaults to "__grammar__"), the channel
    //   the engine reads for `trivia`, `hard_keywords`, `soft_keywords`, etc. —
    //   so programmatically-built grammars can configure layout/keywords without
    //   going through the surface authoring DSL.
    ev.register_special(
        "ctfe_grammar_set_metadata",
        3,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let mut grammar = downcast_grammar(&args[0], "ctfe_grammar_set_metadata")?
                .grammar
                .clone();
            let key = require_string(&args[1], "ctfe_grammar_set_metadata: key must be a string")?;
            let value = crate::lsp::runtime_value_to_json(&args[2]);
            let owner = optional_string_arg(
                &args,
                3,
                "ctfe_grammar_set_metadata: owner must be a string",
            )?
            .unwrap_or_else(|| "__grammar__".to_string());
            grammar.set_metadata_value(owner, key, value);
            Ok(grammar_host_obj(grammar))
        },
    );

    // ── AST + incremental AST diff ──────────────────────────────────────────
    // ctfe-grammar-parse-ast text grammar [start_rule] [max_steps]
    //   → {"ok" bool "ast" ast-node | "error" str}. `ast` is a host object;
    //   project it with `ctfe-ast-to-map`, diff it with `ctfe-ast-*`.
    ev.register_special(
        "ctfe_grammar_parse_ast",
        2,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let text = require_string(&args[0], "ctfe_grammar_parse_ast: text must be a string")?;
            let grammar = &downcast_grammar(&args[1], "ctfe_grammar_parse_ast")?.grammar;
            let start_rule = optional_string_arg(
                &args,
                2,
                "ctfe_grammar_parse_ast: start_rule must be a string",
            )?;
            let max_steps = optional_usize_arg(
                &args,
                3,
                "ctfe_grammar_parse_ast: max_steps must be a non-negative integer",
            )?;
            match caap_peg::parse_ast_with_max_steps(
                grammar,
                &text,
                start_rule.as_deref(),
                max_steps,
            ) {
                Ok(node) => Ok(map_value(vec![
                    ("ok", RuntimeValue::Bool(true)),
                    ("ast", ast_node_host_obj(node, text)),
                ])),
                Err(error) => Ok(map_value(vec![
                    ("ok", RuntimeValue::Bool(false)),
                    ("error", str_value(error.message.as_ref())),
                ])),
            }
        },
    );

    // ctfe-grammar-parse-ast-tolerant text grammar [start_rule] → ast-node
    //   Always returns a tree (unmatched tail becomes an error node).
    ev.register_special(
        "ctfe_grammar_parse_ast_tolerant",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let text = require_string(
                &args[0],
                "ctfe_grammar_parse_ast_tolerant: text must be a string",
            )?;
            let grammar = &downcast_grammar(&args[1], "ctfe_grammar_parse_ast_tolerant")?.grammar;
            let start_rule = optional_string_arg(
                &args,
                2,
                "ctfe_grammar_parse_ast_tolerant: start_rule must be a string",
            )?;
            let node = caap_peg::parse_ast_tolerant(grammar, &text, start_rule.as_deref());
            Ok(ast_node_host_obj(node, text))
        },
    );

    // ctfe-ast-to-map ast-node → recursive structural map
    ev.register_special(
        "ctfe_ast_to_map",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let ast = downcast_ast_node(&args[0], "ctfe_ast_to_map")?;
            Ok(ast_node_to_runtime(&ast.node))
        },
    );

    // ctfe-ast-changed-ranges old-ast new-ast edit → list of {start,end}
    //   `edit` is {"start" int "old_end" int "new_end" int}.
    ev.register_special(
        "ctfe_ast_changed_ranges",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let old = downcast_ast_node(&args[0], "ctfe_ast_changed_ranges: old")?;
            let new = downcast_ast_node(&args[1], "ctfe_ast_changed_ranges: new")?;
            let edit = ast_edit_from_runtime(&args[2], "ctfe_ast_changed_ranges")?;
            let ranges =
                caap_peg::changed_ranges(&old.node, &old.text, &new.node, &new.text, &edit);
            Ok(list_value(
                ranges
                    .iter()
                    .map(|span| ast_span_map(span.start, span.end))
                    .collect(),
            ))
        },
    );

    // ctfe-ast-reparse-incremental old-ast new-ast edit → ast-node (subtrees reused)
    ev.register_special(
        "ctfe_ast_reparse_incremental",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let old = downcast_ast_node(&args[0], "ctfe_ast_reparse_incremental: old")?;
            let new = downcast_ast_node(&args[1], "ctfe_ast_reparse_incremental: new")?;
            let edit = ast_edit_from_runtime(&args[2], "ctfe_ast_reparse_incremental")?;
            let reused = caap_peg::reparse_ast_incremental(
                &old.node,
                &old.text,
                new.node.clone(),
                &new.text,
                &edit,
            );
            Ok(ast_node_host_obj(reused, new.text.clone()))
        },
    );

    // ── Error-recovery parsing ──────────────────────────────────────────────
    // ctfe-grammar-parse-recover text grammar [options]
    //   → {ok, forms:[value...], errors:[{message,start,end}...]}. Splits on
    //   sync points and keeps parsing past errors (multi-error reporting for
    //   editors/batch tooling). `options`: {"sync_tokens" [str...] "sync_regex"
    //   str "max_errors" int}.
    ev.register_special(
        "ctfe_grammar_parse_recover",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let text = require_string(
                &args[0],
                "ctfe_grammar_parse_recover: text must be a string",
            )?;
            let grammar = &downcast_grammar(&args[1], "ctfe_grammar_parse_recover")?.grammar;
            let config = recovery_config_from_runtime(args.get(2))?;
            let (forms, errors) = caap_peg::recover_parse(
                &text,
                grammar,
                |segment, grammar| caap_peg::ParseRequest::new(grammar).run(segment),
                &config,
            );
            Ok(map_value(vec![
                ("ok", RuntimeValue::Bool(errors.is_empty())),
                (
                    "forms",
                    list_value(forms.into_iter().map(pv_to_rv).collect()),
                ),
                (
                    "errors",
                    list_value(errors.iter().map(parse_error_to_runtime).collect()),
                ),
            ]))
        },
    );
}

/// Build a [`caap_peg::RecoveryConfig`] from an options map (all fields optional;
/// `sync_tokens` / `sync_regex` define synchronization points, `max_errors` caps
/// the run). Absent/null → defaults.
fn recovery_config_from_runtime(
    value: Option<&RuntimeValue>,
) -> Result<caap_peg::RecoveryConfig, EvalSignal> {
    let mut config = caap_peg::RecoveryConfig::default();
    let context = "ctfe_grammar_parse_recover";
    let (Some(value), false) = (value, matches!(value, Some(RuntimeValue::Null) | None)) else {
        return Ok(config);
    };
    let RuntimeValue::Map(fields) = value else {
        return Err(eval_err(format!("{context}: options must be a map")));
    };
    let fields = fields.borrow();
    if let Some(tokens) = fields.get(&MapKey::Str(Rc::from("sync_tokens"))) {
        let RuntimeValue::List(items) = tokens else {
            return Err(eval_err(format!(
                "{context}: 'sync_tokens' must be a list of strings"
            )));
        };
        config.sync_tokens = items
            .borrow()
            .iter()
            .map(|item| require_string(item, &format!("{context}: 'sync_tokens' entries")))
            .collect::<Result<Vec<_>, _>>()?;
    }
    if let Some(regex) = fields.get(&MapKey::Str(Rc::from("sync_regex"))) {
        if !matches!(regex, RuntimeValue::Null) {
            config.sync_regex = Some(require_string(
                regex,
                &format!("{context}: 'sync_regex' must be a string"),
            )?);
        }
    }
    if let Some(max_errors) = fields.get(&MapKey::Str(Rc::from("max_errors"))) {
        config.max_errors = require_usize(
            max_errors,
            &format!("{context}: 'max_errors' must be a non-negative integer"),
        )?;
    }
    Ok(config)
}

/// Project a [`caap_peg::ParseError`] into a `{message, start, end}` map.
fn parse_error_to_runtime(error: &caap_peg::ParseError) -> RuntimeValue {
    map_value(vec![
        ("message", str_value(error.message.as_ref())),
        ("start", RuntimeValue::Int(error.span.start as i64)),
        ("end", RuntimeValue::Int(error.span.end as i64)),
    ])
}
