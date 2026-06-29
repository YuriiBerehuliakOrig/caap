use std::borrow::Borrow;
use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{CaapError, CaapResult};

/// Which evaluation phase a symbol may be called in.
///
/// - `Runtime` — callable only at runtime.
/// - `CompileTime` — callable only during compile-time evaluation (CTFE).
/// - `Dual` — callable in either phase (a coarse per-symbol flag, not partial
///   evaluation).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum PhasePolicy {
    Runtime,
    CompileTime,
    Dual,
}

impl PhasePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::CompileTime => "compile_time",
            Self::Dual => "dual",
        }
    }

    pub fn parse_label(value: &str) -> CaapResult<Self> {
        match value {
            "runtime" => Ok(Self::Runtime),
            "compile_time" => Ok(Self::CompileTime),
            "dual" => Ok(Self::Dual),
            _ => Err(CaapError::semantic(format!(
                "phase policy must be one of runtime, compile_time, or dual: {value:?}"
            ))),
        }
    }
}

/// How the evaluator treats a callee's operands before handing it control.
///
/// - `Eager` — evaluate every operand left-to-right, then call (ordinary functions).
/// - `LazyIf` — the `if` policy: evaluate the first operand (the condition), then
///   only the taken branch, never both.
/// - `Sequential` — the `do` policy: evaluate operands in order for their effects,
///   yielding the last.
/// - `SpecialForm` — pass operands UNEVALUATED; the callee drives evaluation
///   itself (macros / syntactic forms).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum EvalPolicy {
    Eager,
    LazyIf,
    Sequential,
    SpecialForm,
}

impl EvalPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eager => "eager",
            Self::LazyIf => "lazy_if",
            Self::Sequential => "sequential",
            Self::SpecialForm => "special_form",
        }
    }

    pub fn parse_label(value: &str) -> CaapResult<Self> {
        match value {
            "eager" => Ok(Self::Eager),
            "lazy_if" => Ok(Self::LazyIf),
            "sequential" => Ok(Self::Sequential),
            "special_form" => Ok(Self::SpecialForm),
            _ => Err(CaapError::semantic(format!(
                "eval policy must be one of eager, lazy_if, sequential, or special_form: {value:?}"
            ))),
        }
    }
}

/// The control-flow shape a callee introduces (drives tail-position analysis).
///
/// - `Plain` — an ordinary call with no special control flow.
/// - `ConditionalBranch` — selects among operand branches (`if`/`match`), so a
///   tail position re-arms through the chosen branch.
/// - `StructuredExit` — a non-local structured exit (`leave`-style).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum ControlPolicy {
    Plain,
    ConditionalBranch,
    StructuredExit,
}

impl ControlPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::ConditionalBranch => "conditional_branch",
            Self::StructuredExit => "structured_exit",
        }
    }

    pub fn parse_label(value: &str) -> CaapResult<Self> {
        match value {
            "plain" => Ok(Self::Plain),
            "conditional_branch" => Ok(Self::ConditionalBranch),
            "structured_exit" => Ok(Self::StructuredExit),
            _ => Err(CaapError::semantic(format!(
                "control policy must be one of plain, conditional_branch, or structured_exit: {value:?}"
            ))),
        }
    }
}

/// Whether a callee introduces lexical bindings.
///
/// - `None` — introduces no bindings.
/// - `LexicalBinding` — binds names visible to its body (`bind` / `lambda`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum ScopePolicy {
    None,
    LexicalBinding,
}

impl ScopePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::LexicalBinding => "lexical_binding",
        }
    }

    pub fn parse_label(value: &str) -> CaapResult<Self> {
        match value {
            "none" => Ok(Self::None),
            "lexical_binding" => Ok(Self::LexicalBinding),
            _ => Err(CaapError::semantic(format!(
                "scope policy must be one of none or lexical_binding: {value:?}"
            ))),
        }
    }
}

/// Foldability of a callee under partial evaluation.
///
/// This is the policy axis that the partial-evaluation passes (binding-time
/// analysis, specialization) read to decide whether a call may be reduced at
/// compile time once its inputs are static. The kernel only classifies; the
/// folding decision and the residual rewrite live in stdlib passes.
///
/// - `Always` — fold whenever all inputs are static (pure compile-time reads).
/// - `RuntimePure` — fold when all inputs are static and the call is pure
///   relative to runtime (no I/O, mutation, hardware, runtime allocation).
/// - `Never` — never fold (runtime effects, mutation, structured special forms
///   that require dedicated partial-evaluation handling).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum FoldPolicy {
    Always,
    RuntimePure,
    Never,
}

impl FoldPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::RuntimePure => "runtime_pure",
            Self::Never => "never",
        }
    }

    pub fn parse_label(value: &str) -> CaapResult<Self> {
        match value {
            "always" => Ok(Self::Always),
            "runtime_pure" => Ok(Self::RuntimePure),
            "never" => Ok(Self::Never),
            _ => Err(CaapError::semantic(format!(
                "fold policy must be one of always, runtime_pure, or never: {value:?}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(transparent)]
pub struct EffectTag(String);

impl EffectTag {
    pub fn new(value: impl AsRef<str>) -> CaapResult<Self> {
        let value = value.as_ref();
        validate_effect_policy_tag(value)?;
        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for EffectTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Borrow<str> for EffectTag {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for EffectTag {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(transparent)]
pub struct CapabilityName(String);

impl CapabilityName {
    pub fn new(value: impl AsRef<str>) -> CaapResult<Self> {
        let value = value.as_ref();
        validate_capability_name(value)?;
        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn covers(&self, requested: &CapabilityName) -> bool {
        self.0 == requested.0
            || requested
                .0
                .strip_prefix(&self.0)
                .is_some_and(|rest| rest.starts_with('.'))
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl From<CapabilityName> for String {
    fn from(value: CapabilityName) -> Self {
        value.into_string()
    }
}

impl fmt::Display for CapabilityName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Borrow<str> for CapabilityName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for CapabilityName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EffectSet {
    tags: Vec<EffectTag>,
}

impl EffectSet {
    pub fn empty() -> Self {
        Self { tags: Vec::new() }
    }

    pub fn from_unique_strings(
        values: impl IntoIterator<Item = String>,
        label: &str,
    ) -> CaapResult<Self> {
        let mut seen = BTreeSet::new();
        for value in values {
            let tag = EffectTag::new(&value).map_err(|error| {
                CaapError::semantic(format!("{label} is invalid ({value:?}): {error}"))
            })?;
            if !seen.insert(tag.clone()) {
                return Err(CaapError::semantic(format!("{label} is duplicated: {tag}")));
            }
        }
        Ok(Self {
            tags: seen.into_iter().collect(),
        })
    }

    pub fn from_string_set(
        values: impl IntoIterator<Item = String>,
        label: &str,
    ) -> CaapResult<Self> {
        let mut seen = BTreeSet::new();
        for value in values {
            let tag = EffectTag::new(&value).map_err(|error| {
                CaapError::semantic(format!("{label} is invalid ({value:?}): {error}"))
            })?;
            seen.insert(tag);
        }
        Ok(Self {
            tags: seen.into_iter().collect(),
        })
    }

    pub fn from_builtin_tags(tags: impl IntoIterator<Item = BuiltinEffectTag>) -> Self {
        let mut seen = BTreeSet::new();
        for tag in tags {
            seen.insert(EffectTag(tag.as_str().to_string()));
        }
        Self {
            tags: seen.into_iter().collect(),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &EffectTag> {
        self.tags.iter()
    }

    pub fn iter_strs(&self) -> impl Iterator<Item = &str> {
        self.tags.iter().map(EffectTag::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    pub fn contains(&self, expected: &EffectTag) -> bool {
        self.tags.binary_search(expected).is_ok()
    }

    pub fn contains_str(&self, expected: &str) -> bool {
        self.tags
            .binary_search_by(|tag| tag.as_str().cmp(expected))
            .is_ok()
    }

    pub fn contains_builtin(&self, expected: BuiltinEffectTag) -> bool {
        self.tags
            .binary_search_by(|tag| tag.as_str().cmp(expected.as_str()))
            .is_ok()
    }

    pub fn is_subset_of(&self, allowed: &EffectSet) -> bool {
        self.tags.iter().all(|tag| allowed.contains(tag))
    }

    pub fn to_strings(&self) -> Vec<String> {
        self.tags
            .iter()
            .map(|tag| tag.as_str().to_string())
            .collect()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EffectPolicy {
    tags: EffectSet,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuiltinEffectTag {
    CompilerRegistry,
    EmitDiagnostics,
    EmitEvents,
    HostServices,
    Impure,
    Mutation,
    ReadAttributes,
    ReadFacts,
    ReadFiles,
    ReadIr,
    ReadSymbols,
    RequestRestart,
    UseFiles,
    UseHostServices,
    WriteAttributes,
    WriteFacts,
    WriteFiles,
    WriteIr,
    WriteSymbols,
}

impl BuiltinEffectTag {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CompilerRegistry => "compiler_registry",
            Self::EmitDiagnostics => "emit_diagnostics",
            Self::EmitEvents => "emit_events",
            Self::HostServices => "host_services",
            Self::Impure => "impure",
            Self::Mutation => "mutation",
            Self::ReadAttributes => "read_attributes",
            Self::ReadFacts => "read_facts",
            Self::ReadFiles => "read_files",
            Self::ReadIr => "read_ir",
            Self::ReadSymbols => "read_symbols",
            Self::RequestRestart => "request_restart",
            Self::UseFiles => "use_files",
            Self::UseHostServices => "use_host_services",
            Self::WriteAttributes => "write_attributes",
            Self::WriteFacts => "write_facts",
            Self::WriteFiles => "write_files",
            Self::WriteIr => "write_ir",
            Self::WriteSymbols => "write_symbols",
        }
    }
}

impl EffectPolicy {
    pub fn new(tags: impl IntoIterator<Item = String>) -> CaapResult<Self> {
        let tags = EffectSet::from_unique_strings(tags, "effect policy tag")?;
        Ok(Self { tags })
    }

    pub fn pure() -> Self {
        Self {
            tags: EffectSet::empty(),
        }
    }

    pub fn single(tag: impl Into<String>) -> CaapResult<Self> {
        Self::new([tag.into()])
    }

    pub fn parse_label(value: &str) -> CaapResult<Self> {
        if value == "pure" {
            Ok(Self::pure())
        } else {
            Self::single(value.to_string())
        }
    }

    pub fn builtin(tag: BuiltinEffectTag) -> Self {
        Self {
            tags: EffectSet::from_builtin_tags([tag]),
        }
    }

    pub fn builtins(tags: impl IntoIterator<Item = BuiltinEffectTag>) -> Self {
        Self {
            tags: EffectSet::from_builtin_tags(tags),
        }
    }

    pub fn tags(&self) -> Vec<String> {
        self.tags.to_strings()
    }

    pub fn effect_set(&self) -> &EffectSet {
        &self.tags
    }

    pub fn is_pure(&self) -> bool {
        self.tags.is_empty()
    }

    pub fn allows(&self, tag: &str) -> bool {
        self.tags.contains_str(tag)
    }

    pub fn is_subset_of(&self, allowed: &EffectSet) -> bool {
        self.tags.is_subset_of(allowed)
    }
}

fn validate_effect_policy_tag(tag: &str) -> CaapResult<()> {
    if tag.is_empty() {
        return Err(CaapError::semantic("effect policy tags must be non-empty"));
    }
    if tag.trim() != tag {
        return Err(CaapError::semantic(format!(
            "effect policy tags must not contain leading or trailing whitespace: {tag:?}"
        )));
    }
    if tag
        .chars()
        .any(|ch| !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'))
    {
        return Err(CaapError::semantic(format!(
            "effect policy tag contains unsupported characters: {tag:?}"
        )));
    }
    if tag.starts_with('_') || tag.ends_with('_') || tag.contains("__") {
        return Err(CaapError::semantic(format!(
            "effect policy tags must be snake_case with non-empty segments: {tag:?}"
        )));
    }
    Ok(())
}

fn validate_capability_name(capability: &str) -> CaapResult<()> {
    if capability.is_empty() {
        return Err(CaapError::semantic("capability name must be non-empty"));
    }
    if capability.trim() != capability {
        return Err(CaapError::semantic(
            "capability name must not contain leading or trailing whitespace",
        ));
    }
    if capability.chars().any(char::is_control) {
        return Err(CaapError::semantic(
            "capability name must not contain control characters",
        ));
    }
    for segment in capability.split('.') {
        if segment.is_empty() {
            return Err(CaapError::semantic(
                "capability name segments must be non-empty",
            ));
        }
        if segment.contains(char::is_whitespace) {
            return Err(CaapError::semantic(
                "capability name segments must not contain whitespace",
            ));
        }
        if segment.contains('*') {
            return Err(CaapError::semantic(
                "capability wildcard grants are not supported; grant concrete capabilities explicitly",
            ));
        }
    }
    Ok(())
}
