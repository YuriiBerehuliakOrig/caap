//! Minimal CAAP Unit facade.
//!
//! CAAP treats `Unit` as the public assembly boundary over IR,
//! semantics, links, attributes, snapshots and transactions. This facade is
//! not that far yet; this facade intentionally owns only generic IR state and
//! leaves module/stdlib semantics out of core.

mod core;
mod lifecycle;
mod rewrite;
mod serial;

pub use core::{
    CrossUnitGraph, Unit, UnitAttributeSnapshot, UnitLinkState, DEFAULT_REWRITE_TOMBSTONE_LIMIT,
};
pub use lifecycle::{
    LinkBinding, UnitAssemblyCallback, UnitAssemblyHook, UnitAssemblyPipeline, UnitLifecycleEvent,
    UnitSyntaxState,
};
pub use rewrite::{RewriteRecord, RewriteTombstone};
pub use serial::{UnitSnapshot, UnitTemplate, UnitTransaction};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::{SemanticValue, StableId};

    #[test]
    fn unit_attribute_snapshot_restore_bumps_version() {
        let mut unit = Unit::empty("test.unit").unwrap();
        unit.set_attribute("answer", SemanticValue::Int(42))
            .unwrap();
        let snapshot = unit.capture_attribute_snapshot();
        unit.set_attribute("answer", SemanticValue::Int(7)).unwrap();
        let changed_version = unit.version();

        unit.restore_attribute_snapshot(snapshot).unwrap();
        assert_eq!(
            unit.attributes().get("answer"),
            Some(&SemanticValue::Int(42))
        );
        assert_eq!(unit.version(), changed_version);
    }

    #[test]
    fn unit_rejects_invalid_attribute_values() {
        let mut unit = Unit::empty("test.unit").unwrap();
        let invalid = SemanticValue::Map(vec![
            ("answer".to_string(), SemanticValue::Int(1)),
            ("answer".to_string(), SemanticValue::Int(2)),
        ]);

        let error = unit
            .set_attribute("payload", invalid)
            .unwrap_err()
            .to_string();

        assert!(error.contains("map keys must be unique"));
    }

    #[test]
    fn unit_set_attribute_rejects_version_overflow_without_mutating() {
        let mut unit = Unit::empty("test.unit").unwrap();
        unit.set_attribute("mode", SemanticValue::Str("old".into()))
            .unwrap();
        unit.version = u64::MAX;

        let error = unit
            .set_attribute("mode", SemanticValue::Str("new".into()))
            .unwrap_err()
            .to_string();

        assert!(error.contains("unit version overflow"));
        assert_eq!(
            unit.attributes().get("mode"),
            Some(&SemanticValue::Str("old".into()))
        );
        assert_eq!(unit.version(), u64::MAX);
    }

    #[test]
    fn unit_restore_attribute_snapshot_rejects_version_overflow_without_mutating() {
        let mut unit = Unit::empty("test.unit").unwrap();
        unit.set_attribute("mode", SemanticValue::Str("old".into()))
            .unwrap();
        let mut snapshot = unit.capture_attribute_snapshot();
        snapshot.attributes.insert(
            "mode".to_string(),
            SemanticValue::Str("restored".to_string()),
        );
        snapshot.version = u64::MAX;

        let error = unit
            .restore_attribute_snapshot(snapshot)
            .unwrap_err()
            .to_string();

        assert!(error.contains("unit version overflow"));
        assert_eq!(
            unit.attributes().get("mode"),
            Some(&SemanticValue::Str("old".into()))
        );
    }

    #[test]
    fn unit_semantics_mut_rejects_version_overflow_without_mutating_version() {
        let mut unit = Unit::empty("test.unit").unwrap();
        unit.version = u64::MAX;

        let error = unit.semantics_mut().unwrap_err().to_string();

        assert!(error.contains("unit version overflow"));
        assert_eq!(unit.version(), u64::MAX);
    }

    #[test]
    fn unit_snapshot_restore_restores_identity_and_attributes() {
        let mut unit = Unit::empty("before").unwrap();
        unit.set_attribute("mode", SemanticValue::Str("old".into()))
            .unwrap();
        let snapshot = unit.snapshot();

        unit.set_unit_id("after").unwrap();
        unit.set_attribute("mode", SemanticValue::Str("new".into()))
            .unwrap();
        unit.restore_snapshot(snapshot).unwrap();

        assert_eq!(unit.unit_id(), "before");
        assert_eq!(
            unit.attributes().get("mode"),
            Some(&SemanticValue::Str("old".into()))
        );
    }

    #[test]
    fn unit_snapshot_restore_rejects_invalid_attributes_without_mutation() {
        let mut unit = Unit::empty("valid.unit").unwrap();
        unit.set_attribute("mode", SemanticValue::Str("old".into()))
            .unwrap();
        let mut snapshot = unit.snapshot();
        snapshot.attributes.insert(
            "payload".to_string(),
            SemanticValue::Map(vec![
                ("answer".to_string(), SemanticValue::Int(1)),
                ("answer".to_string(), SemanticValue::Int(2)),
            ]),
        );

        let error = unit.restore_snapshot(snapshot).unwrap_err().to_string();

        assert!(error.contains("map keys must be unique"));
        assert_eq!(unit.unit_id(), "valid.unit");
        assert_eq!(
            unit.attributes().get("mode"),
            Some(&SemanticValue::Str("old".into()))
        );
        assert!(!unit.attributes().contains_key("payload"));
    }

    #[test]
    fn node_stable_id_uses_structural_identity_not_node_address() {
        let graph = crate::frontend::parse("(int_add 1 2)").unwrap();
        let unit = Unit::from_graph("stable.unit", graph).unwrap();
        let node = unit.top_level_form_ids()[0];
        let stable_id = unit.node_stable_id(node).unwrap();

        let shifted_graph = crate::frontend::parse("(int_add 0 0)\n(int_add 1 2)").unwrap();
        let shifted_unit = Unit::from_graph("stable.unit", shifted_graph).unwrap();
        let shifted_node = shifted_unit.top_level_form_ids()[1];
        let shifted_stable_id = shifted_unit.node_stable_id(shifted_node).unwrap();

        assert_eq!(stable_id, shifted_stable_id);
        assert!(!stable_id.as_str().contains(&format!("node:{node}")));
    }

    #[test]
    fn unit_prunes_oldest_erased_rewrite_tombstones() {
        let graph = crate::frontend::parse("a\nb\nc").unwrap();
        let mut unit = Unit::from_graph("tombstone.unit", graph).unwrap();
        let forms = unit.top_level_form_ids().to_vec();
        let first_stable_id = unit.node_stable_id(forms[0]).unwrap();
        let second_stable_id = unit.node_stable_id(forms[1]).unwrap();
        let third_stable_id = unit.node_stable_id(forms[2]).unwrap();

        unit.record_erase_rewrite_tombstones("provider", "lower", None, forms[0])
            .unwrap();
        unit.record_erase_rewrite_tombstones("provider", "lower", None, forms[1])
            .unwrap();
        unit.record_erase_rewrite_tombstones("provider", "lower", None, forms[2])
            .unwrap();

        let removed = unit.prune_erased_rewrite_tombstones(2).unwrap();

        assert_eq!(removed, 1);
        assert!(unit
            .get_erased_rewrite_tombstone(first_stable_id.as_str())
            .is_none());
        assert!(unit
            .get_erased_rewrite_tombstone(second_stable_id.as_str())
            .is_some());
        assert!(unit
            .get_erased_rewrite_tombstone(third_stable_id.as_str())
            .is_some());
    }

    #[test]
    fn unit_prune_rewrite_tombstones_rejects_version_overflow_without_mutating() {
        let graph = crate::frontend::parse("a\nb").unwrap();
        let mut unit = Unit::from_graph("tombstone.overflow", graph).unwrap();
        let forms = unit.top_level_form_ids().to_vec();
        unit.record_erase_rewrite_tombstones("provider", "lower", None, forms[0])
            .unwrap();
        unit.record_erase_rewrite_tombstones("provider", "lower", None, forms[1])
            .unwrap();
        let tombstones = unit.erased_rewrite_tombstones().clone();
        unit.version = u64::MAX;

        let error = unit
            .prune_erased_rewrite_tombstones(1)
            .unwrap_err()
            .to_string();

        assert!(error.contains("unit version overflow"));
        assert_eq!(unit.erased_rewrite_tombstones(), &tombstones);
        assert_eq!(unit.version(), u64::MAX);
    }

    #[test]
    fn link_binding_deserialize_rejects_empty_names() {
        let err = serde_json::from_str::<LinkBinding>(
            r#"{"source_unit":"","source_name":"x","local_name":"x","syntax":false}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("source unit must be non-empty"));
    }

    #[test]
    fn unit_lifecycle_event_deserialize_rejects_empty_fields() {
        let err = serde_json::from_str::<UnitLifecycleEvent>(
            r#"{"kind":"","detail":"x","unit_version":1}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("kind must be non-empty"));
    }

    #[test]
    fn unit_link_state_deserialize_rejects_empty_identity() {
        let err = serde_json::from_str::<UnitLinkState>(
            r#"{"unit_id":"","bindings":[],"public_names":["x"]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("id must be non-empty"));

        let err = serde_json::from_str::<UnitLinkState>(
            r#"{"unit_id":"u","bindings":[],"public_names":[""]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("public names must be non-empty"));
    }

    #[test]
    fn unit_link_state_rejects_duplicate_local_bindings() {
        let first = LinkBinding::new("dep.a", "value", "local").unwrap();
        let second = LinkBinding::new("dep.b", "other", "local").unwrap();

        let error = UnitLinkState::new("u", [first, second], [])
            .unwrap_err()
            .to_string();

        assert!(error.contains("duplicated"));
        assert!(error.contains("local"));
    }

    #[test]
    fn unit_rejects_duplicate_link_binding_local_names() {
        let mut unit = Unit::empty("u").unwrap();
        unit.add_link_binding(LinkBinding::new("dep.a", "value", "local").unwrap())
            .unwrap();

        let error = unit
            .add_link_binding(LinkBinding::new("dep.b", "other", "local").unwrap())
            .unwrap_err()
            .to_string();

        assert!(error.contains("duplicated"));
        assert!(error.contains("local"));
    }

    #[test]
    fn unit_syntax_state_deserialize_rejects_invalid_fields() {
        let err = serde_json::from_str::<UnitSyntaxState>(
            r#"{"language":"","source_path":null,"source_fingerprint":null,"revision":0,"grammar_rules":{},"grammar_metadata":{}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("language must be non-empty"));

        let err = serde_json::from_str::<UnitSyntaxState>(
            r#"{"language":"caap","source_path":"","source_fingerprint":null,"revision":0,"grammar_rules":{},"grammar_metadata":{}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("source path must be non-empty"));

        let err = serde_json::from_str::<UnitSyntaxState>(
            r#"{"language":"caap","source_path":"main.caap","source_fingerprint":null,"revision":0,"grammar_rules":{},"grammar_metadata":{}}"#,
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("source path and fingerprint must be recorded together"));
    }

    #[test]
    fn unit_syntax_state_rejects_revision_overflow_without_mutating_rules() {
        let mut syntax = UnitSyntaxState::new("caap").unwrap();
        syntax.revision = u64::MAX;

        let error = syntax
            .set_grammar_rule("expr", SemanticValue::Str("literal".to_string()))
            .unwrap_err()
            .to_string();

        assert!(error.contains("unit syntax revision overflow"));
        assert_eq!(syntax.revision, u64::MAX);
        assert!(!syntax.grammar_rules.contains_key("expr"));
    }

    #[test]
    fn unit_syntax_state_rejects_revision_overflow_without_mutating_metadata() {
        let mut syntax = UnitSyntaxState::new("caap").unwrap();
        syntax.revision = u64::MAX;

        let error = syntax
            .set_grammar_metadata("mode", SemanticValue::Str("strict".to_string()))
            .unwrap_err()
            .to_string();

        assert!(error.contains("unit syntax revision overflow"));
        assert_eq!(syntax.revision, u64::MAX);
        assert!(!syntax.grammar_metadata.contains_key("mode"));
    }

    #[test]
    fn unit_template_deserialize_rejects_invalid_snapshot() {
        let template = Unit::empty("unit.template").unwrap().to_template();
        let mut value = serde_json::to_value(template).unwrap();
        value["unit_id"] = serde_json::Value::String(String::new());

        let err = serde_json::from_value::<UnitTemplate>(value).unwrap_err();
        assert!(err.to_string().contains("template id must be non-empty"));
    }

    #[test]
    fn unit_template_validation_rejects_invalid_semantic_snapshot() {
        let mut template = Unit::empty("unit.template").unwrap().to_template();
        template.semantics.stable_ids = vec![
            ("node:1".to_string(), StableId::new("stable:1").unwrap()),
            ("node:1".to_string(), StableId::new("stable:2").unwrap()),
        ];

        let error = template.validate().unwrap_err().to_string();

        assert!(error.contains("semantic snapshot is invalid"));
        assert!(error.contains("duplicated"));
    }

    #[test]
    fn unit_template_validation_rejects_duplicate_link_local_names() {
        let mut template = Unit::empty("unit.template").unwrap().to_template();
        template.link_bindings = vec![
            LinkBinding::new("dep.a", "value", "local").unwrap(),
            LinkBinding::new("dep.b", "other", "local").unwrap(),
        ];

        let error = template.validate().unwrap_err().to_string();

        assert!(error.contains("link local name is duplicated"));
        assert!(error.contains("local"));
    }

    #[test]
    fn unit_template_validation_rejects_invalid_syntax_state() {
        let mut template = Unit::empty("unit.template").unwrap().to_template();
        template.syntax_state.source_path = Some("main.caap".to_string());
        template.syntax_state.source_fingerprint = None;

        let error = template.validate().unwrap_err().to_string();

        assert!(error.contains("source path and fingerprint must be recorded together"));
    }

    #[test]
    fn rewrite_record_deserialize_rejects_empty_provider() {
        let err = serde_json::from_str::<RewriteRecord>(
            r#"{"provider_name":"","stage":"lower","family_label":null,"operation":"replace","sources":[],"generation":1}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("provider name must be non-empty"));
    }

    #[test]
    fn rewrite_tombstone_deserialize_rejects_empty_chain() {
        let err = serde_json::from_str::<RewriteTombstone>(
            r#"{"stable_id":"unit:u:node:1","latest":{"provider_name":"p","stage":"lower","family_label":null,"operation":"erase","sources":[1],"generation":1},"chain":[]}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("chain must be non-empty"));
    }

    #[test]
    fn rewrite_provenance_rejects_missing_target_without_generation_mutation() {
        let mut unit = Unit::empty("rewrite.missing_target").unwrap();

        let err = unit
            .record_rewrite_provenance("provider", "lower", None, "replace", [1], [])
            .unwrap_err();

        assert!(err.to_string().contains("target node does not exist"));
        assert_eq!(unit.rewrite_generation, 0);
    }

    #[test]
    fn rewrite_provenance_rejects_generation_outside_semantic_integer_range() {
        let graph = crate::frontend::parse("old_value").unwrap();
        let mut unit = Unit::from_graph("rewrite.generation_overflow", graph).unwrap();
        unit.rewrite_generation = i64::MAX as u64;

        let err = unit
            .record_rewrite_provenance("provider", "lower", None, "replace", [unit.root_id()], [])
            .unwrap_err();

        assert!(err.to_string().contains("semantic integer range"));
        assert_eq!(unit.rewrite_generation, i64::MAX as u64);
    }
}
