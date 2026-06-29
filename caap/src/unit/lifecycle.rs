//! Lifecycle types: LinkBinding, UnitSyntaxState, UnitLifecycleEvent,
//! UnitAssemblyHook, UnitAssemblyPipeline.

use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use crate::error::{CaapError, CaapResult};
use crate::semantic::SemanticValue;

use super::core::Unit;

// ---------------------------------------------------------------------------
// LinkBinding
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LinkBinding {
    pub source_unit: String,
    pub source_name: String,
    pub local_name: String,
    pub syntax: bool,
}

impl LinkBinding {
    pub fn new(
        source_unit: impl Into<String>,
        source_name: impl Into<String>,
        local_name: impl Into<String>,
    ) -> CaapResult<Self> {
        Self::with_syntax(source_unit, source_name, local_name, false)
    }

    pub fn with_syntax(
        source_unit: impl Into<String>,
        source_name: impl Into<String>,
        local_name: impl Into<String>,
        syntax: bool,
    ) -> CaapResult<Self> {
        let source_unit = source_unit.into();
        let source_name = source_name.into();
        let local_name = local_name.into();
        if source_unit.is_empty() {
            return Err(CaapError::unit("link source unit must be non-empty"));
        }
        if source_name.is_empty() {
            return Err(CaapError::unit("link source name must be non-empty"));
        }
        if local_name.is_empty() {
            return Err(CaapError::unit("link local name must be non-empty"));
        }
        Ok(Self {
            source_unit,
            source_name,
            local_name,
            syntax,
        })
    }
}

impl<'de> Deserialize<'de> for LinkBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct LinkBindingData {
            source_unit: String,
            source_name: String,
            local_name: String,
            syntax: bool,
        }

        let data = LinkBindingData::deserialize(deserializer)?;
        Self::with_syntax(
            data.source_unit,
            data.source_name,
            data.local_name,
            data.syntax,
        )
        .map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// UnitSyntaxState
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UnitSyntaxState {
    pub language: String,
    pub source_path: Option<String>,
    pub source_fingerprint: Option<String>,
    pub revision: u64,
    pub grammar_rules: BTreeMap<String, SemanticValue>,
    pub grammar_metadata: BTreeMap<String, SemanticValue>,
    /// Formal parameters of parametric rules (rule name → param names). Empty for
    /// ordinary rules; surface-grammar compilation emits a `[name, params…, "->"]`
    /// header for any rule listed here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub grammar_rule_params: BTreeMap<String, Vec<String>>,
}

impl UnitSyntaxState {
    pub fn new(language: impl Into<String>) -> CaapResult<Self> {
        let language = language.into();
        if language.is_empty() {
            return Err(CaapError::unit("unit syntax language must be non-empty"));
        }
        Ok(Self {
            language,
            source_path: None,
            source_fingerprint: None,
            revision: 0,
            grammar_rules: BTreeMap::new(),
            grammar_metadata: BTreeMap::new(),
            grammar_rule_params: BTreeMap::new(),
        })
    }

    pub fn with_source(
        mut self,
        source_path: impl Into<String>,
        source_fingerprint: impl Into<String>,
    ) -> CaapResult<Self> {
        let source_path = source_path.into();
        let source_fingerprint = source_fingerprint.into();
        if source_path.is_empty() {
            return Err(CaapError::unit("unit syntax source path must be non-empty"));
        }
        if source_fingerprint.is_empty() {
            return Err(CaapError::unit(
                "unit syntax source fingerprint must be non-empty",
            ));
        }
        let revision = self.next_revision()?;
        self.source_path = Some(source_path);
        self.source_fingerprint = Some(source_fingerprint);
        self.revision = revision;
        Ok(self)
    }

    pub fn set_grammar_rule(
        &mut self,
        name: impl Into<String>,
        rule: SemanticValue,
    ) -> CaapResult<()> {
        let name = name.into();
        if name.is_empty() {
            return Err(CaapError::unit("syntax rule name must be non-empty"));
        }
        rule.validate()?;
        let revision = self.next_revision()?;
        self.grammar_rules.insert(name, rule);
        self.revision = revision;
        Ok(())
    }

    /// Record the formal parameters of a parametric rule. Empty `params` clears
    /// any prior entry (the rule becomes ordinary).
    pub fn set_grammar_rule_params(
        &mut self,
        name: impl Into<String>,
        params: Vec<String>,
    ) -> CaapResult<()> {
        let name = name.into();
        if name.is_empty() {
            return Err(CaapError::unit("syntax rule name must be non-empty"));
        }
        if params.iter().any(String::is_empty) {
            return Err(CaapError::unit(
                "syntax rule parameter name must be non-empty",
            ));
        }
        let revision = self.next_revision()?;
        if params.is_empty() {
            self.grammar_rule_params.remove(&name);
        } else {
            self.grammar_rule_params.insert(name, params);
        }
        self.revision = revision;
        Ok(())
    }

    pub fn set_grammar_metadata(
        &mut self,
        key: impl Into<String>,
        value: SemanticValue,
    ) -> CaapResult<()> {
        let key = key.into();
        if key.is_empty() {
            return Err(CaapError::unit("syntax metadata key must be non-empty"));
        }
        value.validate()?;
        let revision = self.next_revision()?;
        self.grammar_metadata.insert(key, value);
        self.revision = revision;
        Ok(())
    }

    pub fn grammar_metadata(&self, key: &str) -> Option<&SemanticValue> {
        self.grammar_metadata.get(key)
    }

    pub(super) fn validate(&self) -> CaapResult<()> {
        if self.language.is_empty() {
            return Err(CaapError::unit("unit syntax language must be non-empty"));
        }
        if self.source_path.as_ref().is_some_and(String::is_empty) {
            return Err(CaapError::unit("unit syntax source path must be non-empty"));
        }
        if self
            .source_fingerprint
            .as_ref()
            .is_some_and(String::is_empty)
        {
            return Err(CaapError::unit(
                "unit syntax source fingerprint must be non-empty",
            ));
        }
        if self.source_path.is_some() != self.source_fingerprint.is_some() {
            return Err(CaapError::unit(
                "unit syntax source path and fingerprint must be recorded together",
            ));
        }
        if self.grammar_rules.keys().any(String::is_empty) {
            return Err(CaapError::unit("syntax rule name must be non-empty"));
        }
        for value in self.grammar_rules.values() {
            value.validate()?;
        }
        if self.grammar_metadata.keys().any(String::is_empty) {
            return Err(CaapError::unit("syntax metadata key must be non-empty"));
        }
        for value in self.grammar_metadata.values() {
            value.validate()?;
        }
        Ok(())
    }

    fn next_revision(&self) -> CaapResult<u64> {
        self.revision
            .checked_add(1)
            .ok_or_else(|| CaapError::unit("unit syntax revision overflow"))
    }
}

impl<'de> Deserialize<'de> for UnitSyntaxState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct UnitSyntaxStateData {
            language: String,
            source_path: Option<String>,
            source_fingerprint: Option<String>,
            revision: u64,
            grammar_rules: BTreeMap<String, SemanticValue>,
            grammar_metadata: BTreeMap<String, SemanticValue>,
            #[serde(default)]
            grammar_rule_params: BTreeMap<String, Vec<String>>,
        }

        let data = UnitSyntaxStateData::deserialize(deserializer)?;
        let state = Self {
            language: data.language,
            source_path: data.source_path,
            source_fingerprint: data.source_fingerprint,
            revision: data.revision,
            grammar_rules: data.grammar_rules,
            grammar_metadata: data.grammar_metadata,
            grammar_rule_params: data.grammar_rule_params,
        };
        state
            .validate()
            .map_err(serde::de::Error::custom)
            .map(|()| state)
    }
}

// ---------------------------------------------------------------------------
// UnitLifecycleEvent
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UnitLifecycleEvent {
    pub kind: String,
    pub detail: String,
    pub unit_version: u64,
}

impl UnitLifecycleEvent {
    pub fn new(
        kind: impl Into<String>,
        detail: impl Into<String>,
        unit_version: u64,
    ) -> CaapResult<Self> {
        let kind = kind.into();
        let detail = detail.into();
        if kind.is_empty() {
            return Err(CaapError::unit(
                "unit lifecycle event kind must be non-empty",
            ));
        }
        if detail.is_empty() {
            return Err(CaapError::unit(
                "unit lifecycle event detail must be non-empty",
            ));
        }
        Ok(Self {
            kind,
            detail,
            unit_version,
        })
    }
}

impl<'de> Deserialize<'de> for UnitLifecycleEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct UnitLifecycleEventData {
            kind: String,
            detail: String,
            unit_version: u64,
        }

        let data = UnitLifecycleEventData::deserialize(deserializer)?;
        Self::new(data.kind, data.detail, data.unit_version).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// UnitAssemblyHook / UnitAssemblyPipeline
// ---------------------------------------------------------------------------

pub type UnitAssemblyCallback = dyn Fn(&mut Unit) -> Result<(), String>;

#[derive(Clone)]
pub struct UnitAssemblyHook {
    pub(super) name: String,
    pub(super) callback: Rc<UnitAssemblyCallback>,
}

impl UnitAssemblyHook {
    pub fn new(
        name: impl Into<String>,
        callback: impl Fn(&mut Unit) -> Result<(), String> + 'static,
    ) -> CaapResult<Self> {
        let name = name.into();
        if name.is_empty() {
            return Err(CaapError::unit("unit assembly hook name must be non-empty"));
        }
        Ok(Self {
            name,
            callback: Rc::new(callback),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Debug for UnitAssemblyHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnitAssemblyHook")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Default)]
pub struct UnitAssemblyPipeline {
    pub(super) hooks: Vec<UnitAssemblyHook>,
}

impl fmt::Debug for UnitAssemblyPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnitAssemblyPipeline")
            .field("hooks", &self.hook_names())
            .finish()
    }
}

impl UnitAssemblyPipeline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_hook(
        &mut self,
        name: impl Into<String>,
        callback: impl Fn(&mut Unit) -> Result<(), String> + 'static,
    ) -> CaapResult<()> {
        let hook = UnitAssemblyHook::new(name, callback)?;
        if self.hooks.iter().any(|existing| existing.name == hook.name) {
            return Err(CaapError::unit(format!(
                "unit assembly hook already registered: {}",
                hook.name
            )));
        }
        self.hooks.push(hook);
        Ok(())
    }

    pub fn hook_names(&self) -> Vec<&str> {
        self.hooks.iter().map(UnitAssemblyHook::name).collect()
    }

    pub fn apply(&self, unit: &mut Unit) -> CaapResult<()> {
        for hook in &self.hooks {
            unit.record_lifecycle("assembly_hook", format!("start:{}", hook.name))?;
            match (hook.callback)(unit) {
                Ok(()) => {
                    unit.record_lifecycle("assembly_hook", format!("finish:{}", hook.name))?
                }
                Err(error) => {
                    unit.record_lifecycle("assembly_hook_error", hook.name.clone())?;
                    return Err(CaapError::unit(format!(
                        "unit assembly hook {} failed: {error}",
                        hook.name
                    )));
                }
            }
        }
        Ok(())
    }
}
