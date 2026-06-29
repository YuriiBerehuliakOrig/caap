use super::*;

use serde::{Deserialize, Serialize};

use crate::error::{CaapError, CaapResult};
use crate::ir::NodeId;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    TopLevel,
    Parameter,
    Local,
    Injected,
    Builtin,
    External,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TopLevel => "top_level",
            Self::Parameter => "parameter",
            Self::Local => "local",
            Self::Injected => "registered",
            Self::Builtin => "builtin",
            Self::External => "external",
        }
    }

    pub fn parse_label(value: &str) -> CaapResult<Self> {
        match value {
            "top_level" => Ok(Self::TopLevel),
            "parameter" => Ok(Self::Parameter),
            "local" => Ok(Self::Local),
            "registered" => Ok(Self::Injected),
            "builtin" => Ok(Self::Builtin),
            "external" => Ok(Self::External),
            _ => Err(CaapError::semantic(format!(
                "symbol kind must be one of top_level, parameter, local, registered, builtin, or external: {value:?}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SymbolEntry {
    pub name: String,
    pub kind: SymbolKind,
    pub phase_policy: PhasePolicy,
    pub node_id: Option<NodeId>,
    pub public: bool,
    pub public_names: Vec<String>,
}

impl SymbolEntry {
    pub fn new(
        name: impl Into<String>,
        kind: SymbolKind,
        phase_policy: PhasePolicy,
        node_id: Option<NodeId>,
    ) -> CaapResult<Self> {
        let name = name.into();
        if name.is_empty() {
            return Err(CaapError::semantic("symbol name must be non-empty"));
        }
        Ok(Self {
            name,
            kind,
            phase_policy,
            node_id,
            public: false,
            public_names: Vec::new(),
        })
    }

    pub fn with_public_names(
        mut self,
        public_names: impl IntoIterator<Item = String>,
    ) -> CaapResult<Self> {
        let mut seen_names = std::collections::BTreeSet::new();
        let mut names = Vec::new();
        for name in public_names {
            if name.is_empty() {
                return Err(CaapError::semantic("public symbol names must be non-empty"));
            }
            if !seen_names.insert(name.clone()) {
                return Err(CaapError::semantic(format!(
                    "public symbol name is duplicated: {name}"
                )));
            }
            names.push(name);
        }
        names.sort();
        self.public = !names.is_empty();
        self.public_names = names;
        Ok(self)
    }
}

#[cfg(test)]
mod symbol_entry_tests {
    use super::*;

    #[test]
    fn effect_policy_rejects_duplicate_tags() {
        let error = EffectPolicy::new(["read_ir".to_string(), "read_ir".to_string()])
            .unwrap_err()
            .to_string();

        assert!(error.contains("duplicated"));
        assert!(error.contains("read_ir"));
    }

    #[test]
    fn effect_policy_rejects_non_canonical_tags() {
        for tag in [
            "read-ir", "READ-ir", " read-ir", "read-ir ", "-read", "read--ir",
        ] {
            let error = EffectPolicy::single(tag.to_string())
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("effect policy tag"),
                "unexpected error for {tag:?}: {error}"
            );
        }
    }

    #[test]
    fn effect_policy_parse_label_accepts_pure_or_canonical_single_tag() {
        assert!(EffectPolicy::parse_label("pure").unwrap().is_pure());

        let policy = EffectPolicy::parse_label("read_ir").unwrap();
        assert!(policy.allows("read_ir"));

        let error = EffectPolicy::parse_label("read-ir")
            .unwrap_err()
            .to_string();
        assert!(error.contains("effect policy tag"));
    }

    #[test]
    fn effect_tag_deserialize_rejects_invalid_tags() {
        let error = serde_json::from_str::<EffectTag>(r#""read-ir""#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported characters"));
    }

    #[test]
    fn capability_name_deserialize_rejects_invalid_names() {
        let error = serde_json::from_str::<CapabilityName>(r#""sys.fs.*""#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("wildcard"));
    }

    #[test]
    fn phase_policy_parse_label_accepts_only_canonical_labels() {
        assert_eq!(
            PhasePolicy::parse_label("runtime").unwrap(),
            PhasePolicy::Runtime
        );
        assert_eq!(
            PhasePolicy::parse_label("compile_time").unwrap(),
            PhasePolicy::CompileTime
        );
        assert_eq!(PhasePolicy::parse_label("dual").unwrap(), PhasePolicy::Dual);

        let error = PhasePolicy::parse_label("compile-time")
            .unwrap_err()
            .to_string();
        assert!(error.contains("runtime, compile_time, or dual"));
    }

    #[test]
    fn eval_policy_parse_label_accepts_only_canonical_labels() {
        assert_eq!(EvalPolicy::parse_label("eager").unwrap(), EvalPolicy::Eager);
        assert_eq!(
            EvalPolicy::parse_label("lazy_if").unwrap(),
            EvalPolicy::LazyIf
        );
        assert_eq!(
            EvalPolicy::parse_label("sequential").unwrap(),
            EvalPolicy::Sequential
        );
        assert_eq!(
            EvalPolicy::parse_label("special_form").unwrap(),
            EvalPolicy::SpecialForm
        );

        let error = EvalPolicy::parse_label("special-form")
            .unwrap_err()
            .to_string();
        assert!(error.contains("eager, lazy_if, sequential, or special_form"));
    }

    #[test]
    fn control_policy_parse_label_accepts_only_canonical_labels() {
        assert_eq!(
            ControlPolicy::parse_label("plain").unwrap(),
            ControlPolicy::Plain
        );
        assert_eq!(
            ControlPolicy::parse_label("conditional_branch").unwrap(),
            ControlPolicy::ConditionalBranch
        );
        assert_eq!(
            ControlPolicy::parse_label("structured_exit").unwrap(),
            ControlPolicy::StructuredExit
        );

        let error = ControlPolicy::parse_label("structured-exit")
            .unwrap_err()
            .to_string();
        assert!(error.contains("plain, conditional_branch, or structured_exit"));
    }

    #[test]
    fn scope_policy_parse_label_accepts_only_canonical_labels() {
        assert_eq!(ScopePolicy::parse_label("none").unwrap(), ScopePolicy::None);
        assert_eq!(
            ScopePolicy::parse_label("lexical_binding").unwrap(),
            ScopePolicy::LexicalBinding
        );

        let error = ScopePolicy::parse_label("lexical-binding")
            .unwrap_err()
            .to_string();
        assert!(error.contains("none or lexical_binding"));
    }

    #[test]
    fn symbol_kind_parse_label_accepts_only_canonical_labels() {
        assert_eq!(
            SymbolKind::parse_label("top_level").unwrap(),
            SymbolKind::TopLevel
        );
        assert_eq!(
            SymbolKind::parse_label("registered").unwrap(),
            SymbolKind::Injected
        );

        let top_level_error = SymbolKind::parse_label("top-level")
            .unwrap_err()
            .to_string();
        assert!(top_level_error.contains("top_level"));

        let injected_error = SymbolKind::parse_label("injected").unwrap_err().to_string();
        assert!(injected_error.contains("registered"));
    }

    #[test]
    fn entry_source_parse_label_accepts_only_canonical_labels() {
        assert_eq!(
            EntrySource::parse_label("top_level").unwrap(),
            EntrySource::TopLevel
        );

        let error = EntrySource::parse_label("top-level")
            .unwrap_err()
            .to_string();
        assert!(error.contains("top_level"));
    }

    #[test]
    fn public_names_reject_duplicates() {
        let error = SymbolEntry::new("local", SymbolKind::TopLevel, PhasePolicy::Runtime, None)
            .unwrap()
            .with_public_names(["exported".to_string(), "exported".to_string()])
            .unwrap_err()
            .to_string();

        assert!(error.contains("duplicated"));
        assert!(error.contains("exported"));
    }

    #[test]
    fn semantic_value_map_rejects_duplicate_keys() {
        let error = SemanticValue::map([
            ("answer".to_string(), SemanticValue::Int(1)),
            ("answer".to_string(), SemanticValue::Int(2)),
        ])
        .unwrap_err()
        .to_string();

        assert!(error.contains("map keys must be unique"));
    }

    #[test]
    fn semantic_value_validate_rejects_nested_duplicate_map_keys() {
        let value = SemanticValue::List(vec![SemanticValue::Map(vec![
            ("answer".to_string(), SemanticValue::Int(1)),
            ("answer".to_string(), SemanticValue::Int(2)),
        ])]);

        let error = value.validate().unwrap_err().to_string();

        assert!(error.contains("map keys must be unique"));
    }

    #[test]
    fn semantic_value_deserialize_rejects_duplicate_map_keys() {
        let error = serde_json::from_str::<SemanticValue>(
            r#"{"Map":[["answer",{"Int":"1"}],["answer",{"Int":"2"}]]}"#,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("map keys must be unique"));
    }

    #[test]
    fn semantic_value_int_serializes_as_string_to_preserve_json_precision() {
        let value = SemanticValue::Int(i64::MAX);

        let json = serde_json::to_string(&value).unwrap();
        let restored = serde_json::from_str::<SemanticValue>(&json).unwrap();

        assert_eq!(json, r#"{"Int":"9223372036854775807"}"#);
        assert_eq!(restored, value);
    }

    #[test]
    fn semantic_value_int_deserialize_rejects_json_number() {
        let error = serde_json::from_str::<SemanticValue>(r#"{"Int":1}"#)
            .unwrap_err()
            .to_string();

        assert!(error.contains("invalid type"));
    }
}

/// Where a [`SymbolEntry`] came from — classifies a name for scope rules and
/// tooling. The string form is [`EntrySource::as_str`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum EntrySource {
    /// A kernel builtin, available before any user code runs.
    Builtin,
    /// A top-level binding declared in a unit.
    TopLevel,
    /// A value registered into the compiler at compile time (CTFE / bootstrap).
    Registered,
    /// A lambda/function parameter.
    Parameter,
    /// A binding local to a `bind`/block scope.
    Local,
    /// A name brought in from another unit.
    External,
}

impl EntrySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::TopLevel => "top_level",
            Self::Registered => "registered",
            Self::Parameter => "parameter",
            Self::Local => "local",
            Self::External => "external",
        }
    }

    pub fn parse_label(value: &str) -> CaapResult<Self> {
        match value {
            "builtin" => Ok(Self::Builtin),
            "top_level" => Ok(Self::TopLevel),
            "registered" => Ok(Self::Registered),
            "parameter" => Ok(Self::Parameter),
            "local" => Ok(Self::Local),
            "external" => Ok(Self::External),
            _ => Err(CaapError::semantic(format!(
                "entry source must be one of builtin, top_level, registered, parameter, local, or external: {value:?}"
            ))),
        }
    }

    pub fn symbol_kind(self) -> SymbolKind {
        match self {
            Self::Builtin => SymbolKind::Builtin,
            Self::TopLevel => SymbolKind::TopLevel,
            Self::Registered => SymbolKind::Injected,
            Self::Parameter => SymbolKind::Parameter,
            Self::Local => SymbolKind::Local,
            Self::External => SymbolKind::External,
        }
    }
}
