/// Compiler provider/stage CTFE builtins — port of the stage-registration
/// subset of `caap/builtins/compiler/provider_registration.py`.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::builtins::compiler_registry::{
    require_compiler_bridge, require_named_string, require_string,
};
use crate::compiler::{
    CompilerBridgeValue, QueryProviderRegistrationSpec, SemanticPolicyRegistration,
};
use crate::eval::{eval_args, Evaluator};
use crate::semantic::{ControlPolicy, EffectPolicy, EvalPolicy, PhasePolicy, ScopePolicy};
use crate::values::{eval_err, BuiltinInfo, EvalSignal, MapKey, RuntimeValue};

pub fn register(ev: &mut Evaluator) {
    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-diagnostic-explanation-register".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-diagnostic-explanation-register expects a compiler bridge",
            )?;
            let code = require_named_string(
                &args[1],
                "ctfe-compiler-diagnostic-explanation-register expects a non-empty diagnostic code",
            )?;
            register_diagnostic_explanation(bridge, &code, args[2].clone())?;
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-provider-register".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 4,
        max_arity: Some(7),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-register-semantic-policy".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 3,
        max_arity: Some(4),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-fact-schema-type-bridge-register".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-fact-schema-register".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 3,
        max_arity: Some(5),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-register-python-language-builtin-bridge".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-register-python-language-builtin-bridge expects a compiler bridge",
            )?;
            let name = require_named_string(
                &args[1],
                "ctfe-compiler-register-python-language-builtin-bridge expects a non-empty bridge name",
            )?;
            bridge
                .register_language_builtin_bridge(name)
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-stage-register".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 2,
        max_arity: Some(7),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
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
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-stage-alias-register".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-stage-alias-register expects a compiler bridge",
            )?;
            let stage = require_named_string(
                &args[1],
                "ctfe-compiler-stage-alias-register expects a non-empty stage name",
            )?;
            let alias = require_named_string(
                &args[2],
                "ctfe-compiler-stage-alias-register expects a non-empty alias",
            )?;
            bridge
                .register_stage_alias(stage, alias)
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-stage-restart-policy-register".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_compiler_registry(),
        min_arity: 3,
        max_arity: Some(3),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-stage-restart-policy-register expects a compiler bridge",
            )?;
            let stage = require_named_string(
                &args[1],
                "ctfe-compiler-stage-restart-policy-register expects a non-empty stage name",
            )?;
            let restart_stage = require_named_string(
                &args[2],
                "ctfe-compiler-stage-restart-policy-register expects a non-empty restart stage name",
            )?;
            bridge
                .register_stage_restart_policy(stage, restart_stage)
                .map_err(eval_err)?;
            Ok(RuntimeValue::Null)
        }),
    });
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiagnosticExplanationSpec {
    title: String,
    body: String,
    help: Vec<String>,
}

fn register_diagnostic_explanation(
    bridge: &CompilerBridgeValue,
    code: &str,
    spec_value: RuntimeValue,
) -> Result<(), EvalSignal> {
    let RuntimeValue::Map(spec_map) = &spec_value else {
        return Err(eval_err(
            "ctfe-compiler-diagnostic-explanation-register expects an explanation map",
        ));
    };

    let registry_name = "caap.diagnostics.explanations";
    let explanations = match bridge
        .lookup_registered_value(registry_name)
        .map_err(eval_err)?
    {
        Some(RuntimeValue::Map(map)) => map,
        Some(_) => {
            return Err(eval_err(format!(
                "compiler registry value {registry_name:?} must be a map"
            )))
        }
        None => {
            let map = Rc::new(RefCell::new(HashMap::new()));
            bridge
                .register_value(registry_name, RuntimeValue::Map(Rc::clone(&map)))
                .map_err(eval_err)?;
            map
        }
    };
    let spec = diagnostic_explanation_spec_from_map(&spec_map.borrow())?;
    explanations
        .borrow_mut()
        .insert(MapKey::Str(code.into()), spec_value);
    if let Some(spec) = spec {
        bridge
            .register_diagnostic_explanation(code.to_string(), spec.title, spec.body, spec.help)
            .map_err(eval_err)?;
    }
    Ok(())
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
    Ok(SemanticPolicyRegistration {
        name: name.to_string(),
        phase_policy: map_phase_policy(
            borrow.get(&MapKey::Str("phase".into())),
            PhasePolicy::Runtime,
        )?,
        effect_policy: map_effect_policy(borrow.get(&MapKey::Str("effect".into())))?,
        eval_policy: map_eval_policy(borrow.get(&MapKey::Str("eval".into())), EvalPolicy::Eager)?,
        control_policy: map_control_policy(
            borrow.get(&MapKey::Str("control".into())),
            ControlPolicy::Plain,
        )?,
        scope_policy: map_scope_policy(
            borrow.get(&MapKey::Str("scope".into())),
            ScopePolicy::None,
        )?,
        form_policy: map_form_policy(borrow.get(&MapKey::Str("form".into())))?,
        normalizer: RuntimeValue::Null,
        unit_id: None,
        stable_id: None,
    })
}

fn diagnostic_explanation_spec_from_map(
    map: &HashMap<MapKey, RuntimeValue>,
) -> Result<Option<DiagnosticExplanationSpec>, EvalSignal> {
    let summary = optional_map_named_string(
        map,
        "summary",
        "ctfe-compiler-diagnostic-explanation-register expects summary to be a string",
    )?;
    let title = optional_map_named_string(
        map,
        "title",
        "ctfe-compiler-diagnostic-explanation-register expects title to be a string",
    )?
    .or_else(|| summary.clone());
    let body = optional_map_named_string(
        map,
        "body",
        "ctfe-compiler-diagnostic-explanation-register expects body to be a string",
    )?
    .or(summary);
    let Some(title) = title else {
        return Ok(None);
    };
    let Some(body) = body else {
        return Ok(None);
    };
    let help = diagnostic_help(map.get(&MapKey::Str("help".into())))?;
    Ok(Some(DiagnosticExplanationSpec { title, body, help }))
}

fn diagnostic_help(value: Option<&RuntimeValue>) -> Result<Vec<String>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(Vec::new()),
        Some(RuntimeValue::Str(help)) => Ok(vec![help.to_string()]),
        Some(value) => optional_string_sequence(
            Some(value),
            "ctfe-compiler-diagnostic-explanation-register expects help to be a string or sequence of strings",
        ),
    }
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

fn map_eval_policy(
    value: Option<&RuntimeValue>,
    default: EvalPolicy,
) -> Result<EvalPolicy, EvalSignal> {
    let Some(value) = value else {
        return Ok(default);
    };
    match require_named_string(value, "expected eval policy")?.as_str() {
        "eager" => Ok(EvalPolicy::Eager),
        "lazy_if" => Ok(EvalPolicy::LazyIf),
        "sequential" => Ok(EvalPolicy::Sequential),
        "special_form" => Ok(EvalPolicy::SpecialForm),
        _ => Err(eval_err("expected eval policy")),
    }
}

fn map_control_policy(
    value: Option<&RuntimeValue>,
    default: ControlPolicy,
) -> Result<ControlPolicy, EvalSignal> {
    let Some(value) = value else {
        return Ok(default);
    };
    match require_named_string(value, "expected control policy")?.as_str() {
        "plain" => Ok(ControlPolicy::Plain),
        "conditional_branch" => Ok(ControlPolicy::ConditionalBranch),
        "structured_exit" => Ok(ControlPolicy::StructuredExit),
        _ => Err(eval_err("expected control policy")),
    }
}

fn map_scope_policy(
    value: Option<&RuntimeValue>,
    default: ScopePolicy,
) -> Result<ScopePolicy, EvalSignal> {
    let Some(value) = value else {
        return Ok(default);
    };
    match require_named_string(value, "expected scope policy")?.as_str() {
        "none" => Ok(ScopePolicy::None),
        "lexical_binding" => Ok(ScopePolicy::LexicalBinding),
        _ => Err(eval_err("expected scope policy")),
    }
}

fn map_phase_policy(
    value: Option<&RuntimeValue>,
    default: PhasePolicy,
) -> Result<PhasePolicy, EvalSignal> {
    let Some(value) = value else {
        return Ok(default);
    };
    match require_named_string(value, "expected phase policy")?.as_str() {
        "runtime" => Ok(PhasePolicy::Runtime),
        "compile_time" | "compile-time" => Ok(PhasePolicy::CompileTime),
        "dual" => Ok(PhasePolicy::Dual),
        _ => Err(eval_err("expected phase policy")),
    }
}

fn map_effect_policy(value: Option<&RuntimeValue>) -> Result<EffectPolicy, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(EffectPolicy::pure()),
        Some(RuntimeValue::Str(text)) if text.as_ref() == "pure" => Ok(EffectPolicy::pure()),
        Some(RuntimeValue::Str(text)) => EffectPolicy::single(text.to_string()).map_err(eval_err),
        Some(RuntimeValue::Tuple(items)) => items
            .iter()
            .map(|item| require_named_string(item, "expected effect policy tag"))
            .collect::<Result<Vec<_>, _>>()
            .and_then(|tags| EffectPolicy::new(tags).map_err(eval_err)),
        Some(RuntimeValue::List(items)) => items
            .borrow()
            .iter()
            .map(|item| require_named_string(item, "expected effect policy tag"))
            .collect::<Result<Vec<_>, _>>()
            .and_then(|tags| EffectPolicy::new(tags).map_err(eval_err)),
        Some(_) => Err(eval_err("expected effect policy")),
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
    map: &std::collections::HashMap<MapKey, RuntimeValue>,
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
    map: &HashMap<MapKey, RuntimeValue>,
    key: &str,
    message: &str,
) -> Result<Option<String>, EvalSignal> {
    map.get(&MapKey::Str(key.into()))
        .map(|value| require_named_string(value, message))
        .transpose()
}

fn map_string_sequence(
    map: &HashMap<MapKey, RuntimeValue>,
    key: &str,
) -> Result<Vec<String>, EvalSignal> {
    optional_string_sequence(
        map.get(&MapKey::Str(key.into())),
        "ctfe-compiler-provider-register expects provider spec string sequences",
    )
}
