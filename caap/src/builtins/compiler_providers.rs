/// Compiler provider/stage CTFE builtins — port of the stage-registration
/// subset of `caap/builtins/compiler/provider_registration.py`.
use indexmap::IndexMap;

use crate::builtins::compiler_registry::{
    require_compiler_bridge, require_named_string, require_string,
};
use crate::compiler::{QueryProviderRegistrationSpec, SemanticPolicyRegistration};
use crate::eval::{eval_args, Evaluator};
use crate::semantic::{EntrySource, SemanticEntry};
use crate::values::{eval_err, EvalSignal, MapKey, RuntimeValue};

use super::semantic_projection::{runtime_semantic_policy_fields, SemanticPolicyDefaults};

pub fn register(ev: &mut Evaluator) {
    ev.register_special(
        "ctfe_compiler_provider_register",
        4,
        Some(7),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-provider-register expects a compiler bridge",
            )?;
            let name = require_named_string(
                &args[1],
                "ctfe-compiler-provider-register expects a non-empty provider name",
            )?;
            let target = require_named_string(
                &args[2],
                "ctfe-compiler-provider-register expects a valid provider stage or target alias",
            )?;
            let callback = args[3].clone();
            let requires = optional_string_sequence(
                args.get(4),
                "ctfe-compiler-provider-register expects provider requirement names",
            )?;
            let effects = optional_string_sequence(
                args.get(5),
                "ctfe-compiler-provider-register expects provider effect names",
            )?;
            let spec = provider_registration_spec(args.get(6))?;
            bridge
                .register_provider(name, target, callback, requires, effects, spec)
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        },
    );

    ev.register_special(
        "ctfe_compiler_register_semantic_policy",
        3,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-register-semantic-policy expects a compiler bridge",
            )?;
            let name = require_named_string(
                &args[1],
                "ctfe-compiler-register-semantic-policy expects a non-empty string name",
            )?;
            let mut policy = semantic_policy_registration(&name, &args[2])?;
            policy.normalizer = args.get(3).cloned().unwrap_or(RuntimeValue::Null);
            bridge.register_semantic_policy(policy).map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        },
    );

    ev.register_special(
        "ctfe_compiler_fact_schema_type_bridge_register",
        3,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-fact-schema-type-bridge-register expects a compiler bridge",
            )?;
            let label = require_named_string(
                &args[1],
                "ctfe-compiler-fact-schema-type-bridge-register expects a non-empty type label",
            )?;
            let bridge_name = require_named_string(
                &args[2],
                "ctfe-compiler-fact-schema-type-bridge-register expects a non-empty bridge name",
            )?;
            bridge
                .register_fact_schema_type_bridge(label, bridge_name)
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        },
    );

    ev.register_special(
        "ctfe_compiler_fact_schema_register",
        3,
        Some(5),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-fact-schema-register expects a compiler bridge",
            )?;
            let predicate = require_named_string(
                &args[1],
                "ctfe-compiler-fact-schema-register expects a non-empty fact predicate",
            )?;
            let type_label = require_named_string(
                &args[2],
                "ctfe-compiler-fact-schema-register expects a non-empty fact schema type",
            )?;
            let allow_none = optional_bool(
                args.get(3),
                false,
                "ctfe-compiler-fact-schema-register allow_none expects a bool",
            )?;
            let description = match args.get(4) {
                None | Some(RuntimeValue::Null) => None,
                Some(value) => Some(require_string(
                    value,
                    "ctfe-compiler-fact-schema-register description expects a string",
                )?),
            };
            bridge
                .register_fact_schema(predicate, type_label, allow_none, description)
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        },
    );

    ev.register_special("ctfe_compiler_register_base_semantic_entries", 2, Some(2), crate::values::BuiltinMetadata::compile_time_compiler_registry(), |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-register-base-semantic-entries expects a compiler bridge",
            )?;
            let entries = base_semantic_entries(
                args.get(1),
                "ctfe-compiler-register-base-semantic-entries expects semantic entry descriptor maps",
            )?;
            if entries.is_empty() {
                return Err(eval_err(
                    "ctfe-compiler-register-base-semantic-entries expects at least one semantic entry descriptor",
                ));
            }
            bridge
                .register_base_semantic_entries(entries)
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        });

    ev.register_special(
        "ctfe_compiler_stage_register",
        2,
        Some(7),
        crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-stage-register expects a compiler bridge",
            )?;
            let name = require_named_string(
                &args[1],
                "ctfe-compiler-stage-register expects a non-empty stage name",
            )?;
            let requires = optional_string_sequence(
                args.get(2),
                "ctfe-compiler-stage-register expects dependency stage names",
            )?;
            let family_label = optional_named_string(
                args.get(3),
                "ctfe-compiler-stage-register expects a non-empty family label",
            )?;
            let aliases = optional_string_sequence(
                args.get(4),
                "ctfe-compiler-stage-register expects alias names",
            )?;
            let restart_stage = optional_named_string(
                args.get(5),
                "ctfe-compiler-stage-register expects a non-empty restart stage",
            )?;
            let input_kinds = optional_string_sequence(
                args.get(6),
                "ctfe-compiler-stage-register expects input kind names",
            )?;
            bridge
                .register_stage(
                    name,
                    requires,
                    family_label,
                    aliases,
                    restart_stage,
                    input_kinds,
                )
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        },
    );
}

fn semantic_policy_registration(
    name: &str,
    value: &RuntimeValue,
) -> Result<SemanticPolicyRegistration, EvalSignal> {
    let RuntimeValue::Map(map) = value else {
        return Err(eval_err(
            "ctfe-compiler-register-semantic-policy expects a policy map",
        ));
    };
    let borrow = map.borrow();
    reject_unknown_map_keys(
        &borrow,
        &[
            "phase_policy",
            "effect_policy",
            "eval_policy",
            "control_policy",
            "scope_policy",
            "fold_policy",
            "form_policy",
        ],
        "ctfe_compiler_register_semantic_policy",
    )?;
    let policies = runtime_semantic_policy_fields(&borrow, SemanticPolicyDefaults::default())?;
    Ok(SemanticPolicyRegistration {
        name: name.to_string(),
        phase_policy: policies.phase_policy,
        effect_policy: policies.effect_policy,
        eval_policy: policies.eval_policy,
        control_policy: policies.control_policy,
        scope_policy: policies.scope_policy,
        fold_policy: policies.fold_policy,
        form_policy: map_form_policy(borrow.get(&MapKey::Str("form_policy".into())))?,
        normalizer: RuntimeValue::Null,
        unit_id: None,
        stable_id: None,
    })
}

fn optional_named_string(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<String>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(value) => require_named_string(value, message).map(Some),
    }
}

fn optional_bool(
    value: Option<&RuntimeValue>,
    default: bool,
    message: &str,
) -> Result<bool, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(default),
        Some(RuntimeValue::Bool(value)) => Ok(*value),
        Some(_) => Err(eval_err(message)),
    }
}

fn optional_string_sequence(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Vec<String>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(Vec::new()),
        Some(RuntimeValue::Tuple(items)) => items
            .iter()
            .map(|item| require_named_string(item, message))
            .collect(),
        Some(RuntimeValue::List(items)) => items
            .borrow()
            .iter()
            .map(|item| require_named_string(item, message))
            .collect(),
        Some(_) => Err(eval_err(message)),
    }
}

fn base_semantic_entries(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Vec<SemanticEntry>, EvalSignal> {
    match value {
        Some(RuntimeValue::Tuple(items)) => items
            .iter()
            .map(|item| base_semantic_entry(item, message))
            .collect(),
        Some(RuntimeValue::List(items)) => items
            .borrow()
            .iter()
            .map(|item| base_semantic_entry(item, message))
            .collect(),
        _ => Err(eval_err(message)),
    }
}

fn base_semantic_entry(value: &RuntimeValue, message: &str) -> Result<SemanticEntry, EvalSignal> {
    let RuntimeValue::Map(map) = value else {
        return Err(eval_err(message));
    };
    let borrow = map.borrow();
    reject_unknown_map_keys(
        &borrow,
        &[
            "name",
            "source",
            "phase_policy",
            "effect_policy",
            "eval_policy",
            "control_policy",
            "scope_policy",
            "fold_policy",
        ],
        "ctfe_compiler_register_base_semantic_entries",
    )?;
    let name = optional_map_named_string(
        &borrow,
        "name",
        "ctfe-compiler-register-base-semantic-entries expects entry name to be a non-empty string",
    )?
    .ok_or_else(|| eval_err("ctfe-compiler-register-base-semantic-entries requires entry name"))?;
    let source = map_entry_source(borrow.get(&MapKey::Str("source".into())))?;
    let policies = runtime_semantic_policy_fields(&borrow, SemanticPolicyDefaults::default())?;
    let mut entry = SemanticEntry::new(name, source).map_err(eval_err)?;
    entry.phase_policy = policies.phase_policy;
    entry.effect_policy = policies.effect_policy;
    entry.eval_policy = policies.eval_policy;
    entry.control_policy = policies.control_policy;
    entry.scope_policy = policies.scope_policy;
    entry.fold_policy = policies.fold_policy;
    Ok(entry)
}

fn map_entry_source(value: Option<&RuntimeValue>) -> Result<EntrySource, EvalSignal> {
    let Some(value) = value else {
        return Err(eval_err(
            "ctfe-compiler-register-base-semantic-entries requires entry source",
        ));
    };
    match require_named_string(value, "expected semantic entry source")?.as_str() {
        "builtin" => Ok(EntrySource::Builtin),
        "registered" => Ok(EntrySource::Registered),
        "external" => Ok(EntrySource::External),
        _ => Err(eval_err(
            "base semantic entry source must be builtin, registered, or external",
        )),
    }
}

fn map_form_policy(value: Option<&RuntimeValue>) -> Result<String, EvalSignal> {
    let Some(value) = value else {
        return Ok("none".to_string());
    };
    let policy = require_named_string(value, "expected semantic form policy")?;
    match policy.as_str() {
        "none" | "callable_constructor" | "binding_region" | "control_region" | "control_exit" => {
            Ok(policy)
        }
        _ => Err(eval_err("expected semantic form policy")),
    }
}

fn provider_registration_spec(
    value: Option<&RuntimeValue>,
) -> Result<QueryProviderRegistrationSpec, EvalSignal> {
    let Some(value) = value else {
        return Ok(QueryProviderRegistrationSpec::new());
    };
    let RuntimeValue::Map(map) = value else {
        if matches!(value, RuntimeValue::Null) {
            return Ok(QueryProviderRegistrationSpec::new());
        }
        return Err(eval_err(
            "ctfe-compiler-provider-register expects a provider spec map when provided",
        ));
    };
    let borrow = map.borrow();
    reject_unknown_map_keys(
        &borrow,
        &[
            "family",
            "input_schema",
            "requires_data",
            "provides_data",
            "reads",
            "writes",
            "cache_scope",
            "resume_policy",
        ],
        "ctfe_compiler_provider_register",
    )?;
    Ok(QueryProviderRegistrationSpec {
        family: optional_map_named_string(
            &borrow,
            "family",
            "ctfe-compiler-provider-register expects a non-empty family label when provided",
        )?,
        input_schema: optional_map_string(&borrow, "input_schema")?,
        requires_data: map_string_sequence(&borrow, "requires_data")?,
        provides_data: map_string_sequence(&borrow, "provides_data")?,
        reads: map_string_sequence(&borrow, "reads")?,
        writes: map_string_sequence(&borrow, "writes")?,
        cache_scope: optional_map_named_string(
            &borrow,
            "cache_scope",
            "ctfe-compiler-provider-register expects a non-empty cache_scope",
        )?
        .unwrap_or_else(|| "none".to_string()),
        resume_policy: optional_map_named_string(
            &borrow,
            "resume_policy",
            "ctfe-compiler-provider-register expects a non-empty resume_policy",
        )?
        .unwrap_or_else(|| "safe".to_string()),
    })
}

fn optional_map_string(
    map: &indexmap::IndexMap<MapKey, RuntimeValue>,
    key: &str,
) -> Result<Option<String>, EvalSignal> {
    map.get(&MapKey::Str(key.into()))
        .map(|value| {
            if matches!(value, RuntimeValue::Null) {
                return Ok(None);
            }
            require_string(
                value,
                "ctfe-compiler-provider-register expects provider spec strings",
            )
            .map(Some)
        })
        .transpose()
        .map(Option::flatten)
}

fn optional_map_named_string(
    map: &IndexMap<MapKey, RuntimeValue>,
    key: &str,
    message: &str,
) -> Result<Option<String>, EvalSignal> {
    map.get(&MapKey::Str(key.into()))
        .map(|value| require_named_string(value, message))
        .transpose()
}

fn map_string_sequence(
    map: &IndexMap<MapKey, RuntimeValue>,
    key: &str,
) -> Result<Vec<String>, EvalSignal> {
    optional_string_sequence(
        map.get(&MapKey::Str(key.into())),
        "ctfe-compiler-provider-register expects provider spec string sequences",
    )
}

fn reject_unknown_map_keys(
    map: &IndexMap<MapKey, RuntimeValue>,
    allowed: &[&str],
    context: &str,
) -> Result<(), EvalSignal> {
    for key in map.keys() {
        let MapKey::Str(key) = key else {
            return Err(eval_err(format!(
                "{context} descriptor keys must be strings"
            )));
        };
        if !allowed.iter().any(|allowed| key.as_ref() == *allowed) {
            return Err(eval_err(format!(
                "{context} descriptor contains unknown field {key:?}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::{PhasePolicy, ScopePolicy};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn string(value: &str) -> RuntimeValue {
        RuntimeValue::Str(value.into())
    }

    fn map(entries: impl IntoIterator<Item = (&'static str, RuntimeValue)>) -> RuntimeValue {
        RuntimeValue::Map(Rc::new(RefCell::new(
            entries
                .into_iter()
                .map(|(key, value)| (MapKey::Str(key.into()), value))
                .collect(),
        )))
    }

    #[test]
    fn semantic_policy_registration_rejects_legacy_policy_fields() {
        let value = map([("phase", string("runtime"))]);

        let error = semantic_policy_registration("demo.policy", &value)
            .unwrap_err()
            .to_string();

        assert!(error.contains("unknown field"));
        assert!(error.contains("phase"));
    }

    #[test]
    fn base_semantic_entry_accepts_explicit_policy_fields() {
        let value = map([
            ("name", string("demo")),
            ("source", string("builtin")),
            ("phase_policy", string("compile_time")),
            ("effect_policy", string("read_ir")),
            ("scope_policy", string("lexical_binding")),
        ]);

        let entry = base_semantic_entry(&value, "expected base entry").unwrap();

        assert_eq!(entry.phase_policy, PhasePolicy::CompileTime);
        assert!(entry.effect_policy.allows("read_ir"));
        assert_eq!(entry.scope_policy, ScopePolicy::LexicalBinding);
    }

    #[test]
    fn base_semantic_entry_rejects_legacy_policy_fields() {
        let value = map([
            ("name", string("demo")),
            ("source", string("builtin")),
            ("scope", string("lexical_binding")),
        ]);

        let error = base_semantic_entry(&value, "expected base entry")
            .unwrap_err()
            .to_string();

        assert!(error.contains("unknown field"));
        assert!(error.contains("scope"));
    }

    #[test]
    fn provider_registration_spec_rejects_unknown_fields() {
        let value = map([("cache_scope", string("none")), ("cache", string("none"))]);

        let error = provider_registration_spec(Some(&value))
            .unwrap_err()
            .to_string();

        assert!(error.contains("unknown field"));
        assert!(error.contains("cache"));
    }
}
