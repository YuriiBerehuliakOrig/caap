use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

use crate::bridges::SemanticEntryBridgeValue;
use crate::ir::NodeId;
use crate::semantic::{
    ControlPolicy, EffectPolicy, EntrySource, EvalPolicy, FoldPolicy, PhasePolicy, ScopePolicy,
    SemanticEntry, SemanticValue, StableId, SymbolKind,
};
use crate::values::{eval_err, BuiltinMetadata, EvalSignal, MapKey, RuntimeValue};

/// Map a resolved [`SymbolKind`] to the [`EntrySource`] used by semantic entries.
pub(super) fn entry_source_for_symbol_kind(kind: SymbolKind) -> EntrySource {
    match kind {
        SymbolKind::TopLevel => EntrySource::TopLevel,
        SymbolKind::Parameter => EntrySource::Parameter,
        SymbolKind::Local => EntrySource::Local,
        SymbolKind::Injected => EntrySource::Registered,
        SymbolKind::Builtin => EntrySource::Builtin,
        SymbolKind::External => EntrySource::External,
    }
}

/// Wrap a [`SemanticEntry`] as a host-object value.
pub(super) fn semantic_entry_handle(entry: SemanticEntry) -> RuntimeValue {
    RuntimeValue::HostObject(Rc::new(SemanticEntryBridgeValue::new(entry)))
}

pub(super) fn effect_policy_runtime_value(policy: &EffectPolicy) -> RuntimeValue {
    if policy.is_pure() {
        return RuntimeValue::Str("pure".into());
    }
    let tags = policy.tags();
    if tags.len() == 1 {
        return RuntimeValue::Str(tags[0].as_str().into());
    }
    RuntimeValue::List(Rc::new(RefCell::new(
        tags.into_iter()
            .map(|tag| RuntimeValue::Str(tag.into()))
            .collect(),
    )))
}

pub(super) fn builtin_policy_projection_fields(
    metadata: &BuiltinMetadata,
) -> Vec<(&'static str, RuntimeValue)> {
    vec![
        (
            "phase_policy",
            RuntimeValue::Str(metadata.phase_policy.as_str().into()),
        ),
        (
            "effect_policy",
            effect_policy_runtime_value(&metadata.effect_policy),
        ),
        (
            "eval_policy",
            RuntimeValue::Str(metadata.eval_policy.as_str().into()),
        ),
        (
            "control_policy",
            RuntimeValue::Str(metadata.control_policy.as_str().into()),
        ),
        (
            "scope_policy",
            RuntimeValue::Str(metadata.scope_policy.as_str().into()),
        ),
        (
            "fold_policy",
            RuntimeValue::Str(metadata.fold_policy.as_str().into()),
        ),
    ]
}

pub(super) fn optional_runtime_phase_policy(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<PhasePolicy>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(RuntimeValue::Str(text)) => PhasePolicy::parse_label(text.as_ref())
            .map(Some)
            .map_err(|_| eval_err(message)),
        Some(_) => Err(eval_err(message)),
    }
}

pub(super) fn optional_runtime_eval_policy(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<EvalPolicy>, EvalSignal> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        RuntimeValue::Null => Ok(None),
        RuntimeValue::Str(text) => EvalPolicy::parse_label(text.as_ref())
            .map(Some)
            .map_err(|_| eval_err(message)),
        _ => Err(eval_err(message)),
    }
}

pub(super) fn optional_runtime_control_policy(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<ControlPolicy>, EvalSignal> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        RuntimeValue::Null => Ok(None),
        RuntimeValue::Str(text) => ControlPolicy::parse_label(text.as_ref())
            .map(Some)
            .map_err(|_| eval_err(message)),
        _ => Err(eval_err(message)),
    }
}

pub(super) fn optional_runtime_scope_policy(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<ScopePolicy>, EvalSignal> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        RuntimeValue::Null => Ok(None),
        RuntimeValue::Str(text) => ScopePolicy::parse_label(text.as_ref())
            .map(Some)
            .map_err(|_| eval_err(message)),
        _ => Err(eval_err(message)),
    }
}

pub(super) fn optional_runtime_fold_policy(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<FoldPolicy>, EvalSignal> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        RuntimeValue::Null => Ok(None),
        RuntimeValue::Str(text) => FoldPolicy::parse_label(text.as_ref())
            .map(Some)
            .map_err(|_| eval_err(message)),
        _ => Err(eval_err(message)),
    }
}

pub(super) fn optional_runtime_effect_policy(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<EffectPolicy>, EvalSignal> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        RuntimeValue::Null => Ok(None),
        RuntimeValue::Str(text) => EffectPolicy::parse_label(text.as_ref())
            .map(Some)
            .map_err(eval_err),
        RuntimeValue::Tuple(items) => items
            .iter()
            .map(|item| runtime_policy_tag(item, message))
            .collect::<Result<Vec<_>, _>>()
            .and_then(|tags| EffectPolicy::new(tags).map(Some).map_err(eval_err)),
        RuntimeValue::List(items) => items
            .borrow()
            .iter()
            .map(|item| runtime_policy_tag(item, message))
            .collect::<Result<Vec<_>, _>>()
            .and_then(|tags| EffectPolicy::new(tags).map(Some).map_err(eval_err)),
        _ => Err(eval_err(message)),
    }
}

#[derive(Clone, Copy)]
pub(super) struct SemanticPolicyDefaults {
    pub(super) phase_policy: PhasePolicy,
    pub(super) eval_policy: EvalPolicy,
    pub(super) control_policy: ControlPolicy,
    pub(super) scope_policy: ScopePolicy,
    pub(super) fold_policy: FoldPolicy,
}

impl Default for SemanticPolicyDefaults {
    fn default() -> Self {
        Self {
            phase_policy: PhasePolicy::Runtime,
            eval_policy: EvalPolicy::Eager,
            control_policy: ControlPolicy::Plain,
            scope_policy: ScopePolicy::None,
            fold_policy: FoldPolicy::Never,
        }
    }
}

pub(super) struct RuntimeSemanticPolicyFields {
    pub(super) phase_policy: PhasePolicy,
    pub(super) effect_policy: EffectPolicy,
    pub(super) eval_policy: EvalPolicy,
    pub(super) control_policy: ControlPolicy,
    pub(super) scope_policy: ScopePolicy,
    pub(super) fold_policy: FoldPolicy,
}

pub(super) struct RuntimeSemanticPolicyUpdates {
    pub(super) phase_policy: Option<PhasePolicy>,
    pub(super) effect_policy: Option<EffectPolicy>,
    pub(super) eval_policy: Option<EvalPolicy>,
    pub(super) control_policy: Option<ControlPolicy>,
    pub(super) scope_policy: Option<ScopePolicy>,
    pub(super) fold_policy: Option<FoldPolicy>,
}

pub(super) fn runtime_semantic_policy_fields(
    map: &IndexMap<MapKey, RuntimeValue>,
    defaults: SemanticPolicyDefaults,
) -> Result<RuntimeSemanticPolicyFields, EvalSignal> {
    Ok(RuntimeSemanticPolicyFields {
        phase_policy: optional_runtime_phase_policy(
            map.get(&MapKey::Str("phase_policy".into())),
            "expected phase policy",
        )?
        .unwrap_or(defaults.phase_policy),
        effect_policy: optional_runtime_effect_policy(
            map.get(&MapKey::Str("effect_policy".into())),
            "expected effect policy",
        )?
        .unwrap_or_else(EffectPolicy::pure),
        eval_policy: optional_runtime_eval_policy(
            map.get(&MapKey::Str("eval_policy".into())),
            "expected eval policy",
        )?
        .unwrap_or(defaults.eval_policy),
        control_policy: optional_runtime_control_policy(
            map.get(&MapKey::Str("control_policy".into())),
            "expected control policy",
        )?
        .unwrap_or(defaults.control_policy),
        scope_policy: optional_runtime_scope_policy(
            map.get(&MapKey::Str("scope_policy".into())),
            "expected scope policy",
        )?
        .unwrap_or(defaults.scope_policy),
        fold_policy: optional_runtime_fold_policy(
            map.get(&MapKey::Str("fold_policy".into())),
            "expected fold policy",
        )?
        .unwrap_or(defaults.fold_policy),
    })
}

pub(super) fn runtime_semantic_policy_updates(
    map: &IndexMap<MapKey, RuntimeValue>,
    context: &str,
) -> Result<RuntimeSemanticPolicyUpdates, EvalSignal> {
    reject_legacy_runtime_semantic_policy_keys(map, context)?;
    Ok(RuntimeSemanticPolicyUpdates {
        phase_policy: optional_runtime_phase_policy(
            map.get(&MapKey::Str("phase_policy".into())),
            &format!("{context} expects phase_policy runtime, compile_time, or dual"),
        )?,
        effect_policy: optional_runtime_effect_policy(
            map.get(&MapKey::Str("effect_policy".into())),
            &format!("{context} expects effect_policy"),
        )?,
        eval_policy: optional_runtime_eval_policy(
            map.get(&MapKey::Str("eval_policy".into())),
            &format!("{context} expects eval_policy"),
        )?,
        control_policy: optional_runtime_control_policy(
            map.get(&MapKey::Str("control_policy".into())),
            &format!("{context} expects control_policy"),
        )?,
        scope_policy: optional_runtime_scope_policy(
            map.get(&MapKey::Str("scope_policy".into())),
            &format!("{context} expects scope_policy"),
        )?,
        fold_policy: optional_runtime_fold_policy(
            map.get(&MapKey::Str("fold_policy".into())),
            &format!("{context} expects fold_policy always, runtime_pure, or never"),
        )?,
    })
}

fn reject_legacy_runtime_semantic_policy_keys(
    map: &IndexMap<MapKey, RuntimeValue>,
    context: &str,
) -> Result<(), EvalSignal> {
    for legacy in ["phase", "effect", "eval", "control", "scope"] {
        if map.contains_key(&MapKey::Str(legacy.into())) {
            return Err(eval_err(format!(
                "{context} uses legacy policy field {legacy:?}; use {legacy}_policy"
            )));
        }
    }
    Ok(())
}

fn runtime_policy_tag(value: &RuntimeValue, message: &str) -> Result<String, EvalSignal> {
    match value {
        RuntimeValue::Str(text) if !text.is_empty() => Ok(text.to_string()),
        _ => Err(eval_err(message)),
    }
}

pub(super) fn semantic_value_to_plain_runtime(value: &SemanticValue) -> RuntimeValue {
    semantic_value_to_runtime_with_nodes(value, &|node_id| RuntimeValue::Int(node_id as i64))
}

pub(super) fn semantic_value_to_runtime_with_nodes(
    value: &SemanticValue,
    node_value: &impl Fn(NodeId) -> RuntimeValue,
) -> RuntimeValue {
    match value {
        SemanticValue::Null => RuntimeValue::Null,
        SemanticValue::Bool(value) => RuntimeValue::Bool(*value),
        SemanticValue::Int(value) => RuntimeValue::Int(*value),
        SemanticValue::Float(value) => RuntimeValue::Float(*value),
        SemanticValue::Str(value) => RuntimeValue::Str(value.as_str().into()),
        SemanticValue::Node(node_id) => node_value(*node_id),
        SemanticValue::List(items) => RuntimeValue::List(Rc::new(RefCell::new(
            items
                .iter()
                .map(|item| semantic_value_to_runtime_with_nodes(item, node_value))
                .collect(),
        ))),
        SemanticValue::Map(entries) => {
            let mut map = IndexMap::new();
            for (key, value) in entries {
                map.insert(
                    MapKey::Str(key.as_str().into()),
                    semantic_value_to_runtime_with_nodes(value, node_value),
                );
            }
            RuntimeValue::Map(Rc::new(RefCell::new(map)))
        }
    }
}

pub(super) fn runtime_value_to_semantic_with_nodes(
    value: &RuntimeValue,
    map_key_message: &str,
    value_message: &str,
    node_value: &impl Fn(&RuntimeValue) -> Option<SemanticValue>,
) -> Result<SemanticValue, EvalSignal> {
    match value {
        RuntimeValue::Null => Ok(SemanticValue::Null),
        RuntimeValue::Bool(value) => Ok(SemanticValue::Bool(*value)),
        RuntimeValue::Int(value) => Ok(SemanticValue::Int(*value)),
        RuntimeValue::Float(value) => Ok(SemanticValue::Float(*value)),
        RuntimeValue::Str(value) => Ok(SemanticValue::Str(value.to_string())),
        RuntimeValue::Tuple(items) => items
            .iter()
            .map(|item| {
                runtime_value_to_semantic_with_nodes(
                    item,
                    map_key_message,
                    value_message,
                    node_value,
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map(SemanticValue::List),
        RuntimeValue::List(items) => items
            .borrow()
            .iter()
            .map(|item| {
                runtime_value_to_semantic_with_nodes(
                    item,
                    map_key_message,
                    value_message,
                    node_value,
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map(SemanticValue::List),
        RuntimeValue::Map(map) => {
            let mut entries = Vec::new();
            for (key, value) in map.borrow().iter() {
                let MapKey::Str(key) = key else {
                    return Err(eval_err(map_key_message));
                };
                entries.push((
                    key.to_string(),
                    runtime_value_to_semantic_with_nodes(
                        value,
                        map_key_message,
                        value_message,
                        node_value,
                    )?,
                ));
            }
            SemanticValue::map(entries).map_err(eval_err)
        }
        _ => node_value(value).ok_or_else(|| eval_err(value_message)),
    }
}

pub(super) fn semantic_entry_to_runtime_value(entry: &SemanticEntry) -> RuntimeValue {
    runtime_map_entries([
        ("name", RuntimeValue::Str(entry.name.as_str().into())),
        ("source", RuntimeValue::Str(entry.source.as_str().into())),
        (
            "phase_policy",
            RuntimeValue::Str(entry.phase_policy.as_str().into()),
        ),
        (
            "effect_policy",
            effect_policy_runtime_value(&entry.effect_policy),
        ),
        (
            "eval_policy",
            RuntimeValue::Str(entry.eval_policy.as_str().into()),
        ),
        (
            "control_policy",
            RuntimeValue::Str(entry.control_policy.as_str().into()),
        ),
        (
            "scope_policy",
            RuntimeValue::Str(entry.scope_policy.as_str().into()),
        ),
        (
            "fold_policy",
            RuntimeValue::Str(entry.fold_policy.as_str().into()),
        ),
        (
            "node_id",
            entry
                .node_id
                .map(|node_id| RuntimeValue::Int(node_id as i64))
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "unit_id",
            entry
                .unit_id
                .as_deref()
                .map(|unit_id| RuntimeValue::Str(unit_id.into()))
                .unwrap_or(RuntimeValue::Null),
        ),
        ("value", semantic_value_to_plain_runtime(&entry.value)),
        (
            "stable_id",
            entry
                .stable_id
                .as_ref()
                .map(|stable_id| RuntimeValue::Str(stable_id.as_str().into()))
                .unwrap_or(RuntimeValue::Null),
        ),
    ])
}

fn runtime_map_entries<const N: usize>(entries: [(&str, RuntimeValue); N]) -> RuntimeValue {
    let mut map = IndexMap::new();
    for (key, value) in entries {
        map.insert(MapKey::Str(key.into()), value);
    }
    RuntimeValue::Map(Rc::new(RefCell::new(map)))
}

fn reject_legacy_semantic_entry_policy_keys(
    entries: &[(String, SemanticValue)],
) -> Result<(), EvalSignal> {
    for legacy in ["phase", "effect", "eval", "control", "scope"] {
        if entries.iter().any(|(key, _)| key == legacy) {
            return Err(eval_err(format!(
                "resolved-name entry uses legacy policy field {legacy:?}; use {legacy}_policy"
            )));
        }
    }
    Ok(())
}

pub(super) fn resolved_name_fact_entry(
    value: &SemanticValue,
) -> Option<Result<SemanticEntry, EvalSignal>> {
    let SemanticValue::Map(entries) = value else {
        return None;
    };
    semantic_map_get(entries, "entry").map(semantic_entry_from_semantic_value)
}

pub(super) fn semantic_entry_from_semantic_value(
    value: &SemanticValue,
) -> Result<SemanticEntry, EvalSignal> {
    let SemanticValue::Map(entries) = value else {
        return Err(eval_err("resolved-name entry must be a map"));
    };
    let name = required_semantic_str(entries, "name", "resolved-name entry requires name")?;
    let source = entry_source_label(required_semantic_str(
        entries,
        "source",
        "resolved-name entry requires source",
    )?)?;
    reject_legacy_semantic_entry_policy_keys(entries)?;
    let mut entry = SemanticEntry::new(name, source).map_err(eval_err)?;
    entry.phase_policy =
        optional_semantic_phase(entries, "phase_policy")?.unwrap_or(PhasePolicy::Runtime);
    entry.effect_policy = optional_semantic_effect_policy(entries, "effect_policy")?
        .unwrap_or_else(EffectPolicy::pure);
    entry.eval_policy =
        optional_semantic_eval_policy(entries, "eval_policy")?.unwrap_or(EvalPolicy::Eager);
    entry.control_policy = optional_semantic_control_policy(entries, "control_policy")?
        .unwrap_or(ControlPolicy::Plain);
    entry.scope_policy =
        optional_semantic_scope_policy(entries, "scope_policy")?.unwrap_or(ScopePolicy::None);
    entry.fold_policy =
        optional_semantic_fold_policy(entries, "fold_policy")?.unwrap_or(FoldPolicy::Never);
    entry.node_id = optional_semantic_node_id(entries, "node_id")?;
    entry.unit_id = optional_semantic_str(entries, "unit_id")?;
    entry.value = semantic_map_get(entries, "value")
        .cloned()
        .unwrap_or(SemanticValue::Null);
    entry.stable_id = optional_semantic_str(entries, "stable_id")?
        .map(StableId::new)
        .transpose()
        .map_err(eval_err)?;
    Ok(entry)
}

fn semantic_map_get<'a>(
    entries: &'a [(String, SemanticValue)],
    key: &str,
) -> Option<&'a SemanticValue> {
    entries
        .iter()
        .find_map(|(entry_key, value)| (entry_key == key).then_some(value))
}

fn required_semantic_str<'a>(
    entries: &'a [(String, SemanticValue)],
    key: &str,
    message: &str,
) -> Result<&'a str, EvalSignal> {
    optional_semantic_str_ref(entries, key)?
        .ok_or_else(|| eval_err(message))
        .and_then(|value| {
            if value.is_empty() {
                Err(eval_err(message))
            } else {
                Ok(value)
            }
        })
}

fn optional_semantic_str(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<String>, EvalSignal> {
    optional_semantic_str_ref(entries, key).map(|value| value.map(str::to_string))
}

fn optional_semantic_str_ref<'a>(
    entries: &'a [(String, SemanticValue)],
    key: &str,
) -> Result<Option<&'a str>, EvalSignal> {
    match semantic_map_get(entries, key) {
        None | Some(SemanticValue::Null) => Ok(None),
        Some(SemanticValue::Str(value)) => Ok(Some(value.as_str())),
        Some(_) => Err(eval_err(format!(
            "resolved-name entry {key} must be a string"
        ))),
    }
}

fn optional_semantic_node_id(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<NodeId>, EvalSignal> {
    match semantic_map_get(entries, key) {
        None | Some(SemanticValue::Null) => Ok(None),
        Some(SemanticValue::Node(node_id)) => Ok(Some(*node_id)),
        Some(SemanticValue::Int(value)) if *value >= 0 => Ok(Some(*value as NodeId)),
        Some(_) => Err(eval_err(format!(
            "resolved-name entry {key} must be a non-negative integer"
        ))),
    }
}

fn optional_semantic_phase(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<PhasePolicy>, EvalSignal> {
    optional_semantic_str_ref(entries, key)?
        .map(|label| PhasePolicy::parse_label(label).map_err(eval_err))
        .transpose()
}

fn optional_semantic_eval_policy(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<EvalPolicy>, EvalSignal> {
    optional_semantic_str_ref(entries, key)?
        .map(|label| {
            EvalPolicy::parse_label(label)
                .map_err(|_| eval_err("resolved-name entry eval policy is invalid"))
        })
        .transpose()
}

fn optional_semantic_control_policy(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<ControlPolicy>, EvalSignal> {
    optional_semantic_str_ref(entries, key)?
        .map(|label| {
            ControlPolicy::parse_label(label)
                .map_err(|_| eval_err("resolved-name entry control policy is invalid"))
        })
        .transpose()
}

fn optional_semantic_scope_policy(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<ScopePolicy>, EvalSignal> {
    optional_semantic_str_ref(entries, key)?
        .map(|label| {
            ScopePolicy::parse_label(label)
                .map_err(|_| eval_err("resolved-name entry scope policy is invalid"))
        })
        .transpose()
}

fn optional_semantic_fold_policy(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<FoldPolicy>, EvalSignal> {
    optional_semantic_str_ref(entries, key)?
        .map(|label| {
            FoldPolicy::parse_label(label)
                .map_err(|_| eval_err("resolved-name entry fold policy is invalid"))
        })
        .transpose()
}

fn optional_semantic_effect_policy(
    entries: &[(String, SemanticValue)],
    key: &str,
) -> Result<Option<EffectPolicy>, EvalSignal> {
    match semantic_map_get(entries, key) {
        None | Some(SemanticValue::Null) => Ok(None),
        Some(SemanticValue::Str(value)) => EffectPolicy::parse_label(value.as_str())
            .map(Some)
            .map_err(eval_err),
        Some(SemanticValue::List(items)) => items
            .iter()
            .map(|item| match item {
                SemanticValue::Str(value) if !value.is_empty() => Ok(value.clone()),
                _ => Err(eval_err("resolved-name entry effect policy is invalid")),
            })
            .collect::<Result<Vec<_>, _>>()
            .and_then(|tags| EffectPolicy::new(tags).map(Some).map_err(eval_err)),
        Some(_) => Err(eval_err("resolved-name entry effect policy is invalid")),
    }
}

fn entry_source_label(value: &str) -> Result<EntrySource, EvalSignal> {
    EntrySource::parse_label(value).map_err(|_| eval_err("resolved-name entry source is invalid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn semantic_str(value: &str) -> SemanticValue {
        SemanticValue::Str(value.to_string())
    }

    fn semantic_entry(
        entries: impl IntoIterator<Item = (&'static str, SemanticValue)>,
    ) -> SemanticValue {
        SemanticValue::Map(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    #[test]
    fn semantic_entry_policy_fields_use_canonical_policy_labels() {
        let value = semantic_entry([
            ("name", semantic_str("demo")),
            ("source", semantic_str("builtin")),
            ("effect_policy", semantic_str("read_ir")),
            ("eval_policy", semantic_str("special_form")),
            ("control_policy", semantic_str("structured_exit")),
            ("scope_policy", semantic_str("lexical_binding")),
        ]);

        let entry = semantic_entry_from_semantic_value(&value).unwrap();

        assert!(entry.effect_policy.allows("read_ir"));
        assert_eq!(entry.eval_policy, EvalPolicy::SpecialForm);
        assert_eq!(entry.control_policy, ControlPolicy::StructuredExit);
        assert_eq!(entry.scope_policy, ScopePolicy::LexicalBinding);
    }

    #[test]
    fn semantic_entry_policy_fields_reject_non_canonical_labels() {
        let value = semantic_entry([
            ("name", semantic_str("demo")),
            ("source", semantic_str("builtin")),
            ("scope_policy", semantic_str("lexical-binding")),
        ]);

        let error = semantic_entry_from_semantic_value(&value)
            .unwrap_err()
            .to_string();

        assert!(error.contains("resolved-name entry scope policy is invalid"));
    }
}
