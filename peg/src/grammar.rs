//! [`Grammar`] — a set of [`GrammarRule`]s with a start rule, inline imports,
//! and metadata, holding both source text and compiled expression trees. The
//! `with_*` builders panic on trusted input; their `try_*` twins return errors
//! for untrusted grammar text.

use crate::analysis::GrammarAnalysisState;
use crate::error::ParseError;
use crate::expr::{PegExpr, RuleTextParser};
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::{de::Error as DeError, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Per-grammar memo of the compiled form, parallel to `state.analysis_state`.
///
/// The compiled grammar is not serializable, so it lives here behind a
/// `#[serde(skip)]` field rather than in `GrammarState`. It is type-erased
/// (`Arc<dyn Any>`) on purpose: `grammar.rs` must not name
/// `parser_compile::CompiledGrammar`, which would create a `grammar` ↔
/// `parser_compile` module cycle. `parser_compile::get_compiled` downcasts it.
///
/// Invalidation is by `Grammar::version`: a `get(version)` after any mutation
/// (which bumps the version) misses and triggers a recompile.
#[derive(Default)]
pub(crate) struct CompiledMemo(RwLock<Option<(u64, Arc<dyn Any + Send + Sync>)>>);

impl CompiledMemo {
    /// Return the cached compiled value if it was stored for `version`.
    pub(crate) fn get(&self, version: u64) -> Option<Arc<dyn Any + Send + Sync>> {
        let guard = self.0.read().ok()?;
        match &*guard {
            Some((v, value)) if *v == version => Some(Arc::clone(value)),
            _ => None,
        }
    }

    /// Store the compiled value for `version`, replacing any older entry.
    pub(crate) fn set(&self, version: u64, value: Arc<dyn Any + Send + Sync>) {
        if let Ok(mut guard) = self.0.write() {
            *guard = Some((version, value));
        }
    }
}

// A grammar's identity is its content, not its compile cache: clones start
// empty (no cross-contamination), and equality/serialization ignore the cache.
impl Clone for CompiledMemo {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for CompiledMemo {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for CompiledMemo {}

impl std::fmt::Debug for CompiledMemo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CompiledMemo")
    }
}

/// A single grammar rule storing both its source text and compiled expression tree.
///
/// `source` and `expr` are always kept in sync. Use fallible constructors at
/// public boundaries that accept user grammar text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarRule {
    /// The rule name.
    pub name: String,
    /// Original PEG text for this rule body (retained for analysis and display).
    pub source: String,
    /// Ordered parameter names for parametric rules (e.g. `rule(x, y) <- ...`).
    /// Empty for non-parametric rules.
    pub params: Vec<String>,
    /// Parsed expression tree compiled from `source` at construction time.
    ///
    /// The parser uses this directly, avoiding repeated text-parsing on every
    /// `parse()` call.  Always consistent with `source`.
    /// Use [`GrammarRule::expr()`] for read access from outside the crate.
    pub(crate) expr: PegExpr,
}

impl GrammarRule {
    /// Try to create a rule from text, compiling `source` into a `PegExpr`.
    ///
    /// Use this at external mutation boundaries where invalid grammar text must
    /// be rejected before any grammar state is changed.
    pub fn try_from_source(
        name: impl Into<String>,
        source: impl Into<String>,
        params: Vec<String>,
    ) -> Result<Self, ParseError> {
        let name = name.into();
        let source = source.into();
        let expr = RuleTextParser::parse(&source)?;
        Ok(Self {
            name,
            source,
            params,
            expr,
        })
    }

    /// Create a rule from text, compiling `source` into a `PegExpr` immediately.
    ///
    /// Create a rule from trusted in-process text, compiling `source` into a
    /// `PegExpr` immediately.
    ///
    /// External grammar text must use [`GrammarRule::try_from_source`] so
    /// invalid source is rejected at the boundary instead of panicking.
    pub fn trusted_from_source(
        name: impl Into<String>,
        source: impl Into<String>,
        params: Vec<String>,
    ) -> Self {
        let name = name.into();
        let source = source.into();
        let expr = RuleTextParser::parse(&source).expect(
            "GrammarRule::trusted_from_source received invalid PEG source; use try_from_source",
        );
        Self {
            name,
            source,
            params,
            expr,
        }
    }

    /// Return the compiled expression tree for this rule.
    pub fn expr(&self) -> &PegExpr {
        &self.expr
    }

    /// Create a rule from a `PegExpr` tree directly, without going through PEG text.
    ///
    /// `source` is derived via [`crate::expr::peg_expr_to_source`] so it stays in sync with
    /// `expr` and remains available for display and analysis.
    pub fn from_expr(name: impl Into<String>, expr: PegExpr, params: Vec<String>) -> Self {
        use crate::expr::peg_expr_to_source;
        let source = peg_expr_to_source(&expr);
        Self {
            name: name.into(),
            source,
            params,
            expr,
        }
    }

    /// Update `source` and recompile `expr` to keep them in sync.
    ///
    /// Update trusted in-process `source` and recompile `expr`.
    ///
    /// External grammar text must use [`GrammarRule::try_set_source`] so
    /// invalid source is rejected before this rule is mutated.
    pub fn trusted_set_source(&mut self, source: impl Into<String>) {
        let source = source.into();
        self.expr = RuleTextParser::parse(&source).expect(
            "GrammarRule::trusted_set_source received invalid PEG source; use try_set_source",
        );
        self.source = source;
    }

    /// Update `source` only if it compiles successfully.
    pub fn try_set_source(&mut self, source: impl Into<String>) -> Result<(), ParseError> {
        let source = source.into();
        let expr = RuleTextParser::parse(&source)?;
        self.expr = expr;
        self.source = source;
        Ok(())
    }
}

// Custom serde: wire format is `{ name, source, params }` — `expr` is skipped
// (it's derived from `source` on deserialization).
impl Serialize for GrammarRule {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let field_count = if self.params.is_empty() { 2 } else { 3 };
        let mut st = s.serialize_struct("GrammarRule", field_count)?;
        st.serialize_field("name", &self.name)?;
        st.serialize_field("source", &self.source)?;
        if !self.params.is_empty() {
            st.serialize_field("params", &self.params)?;
        }
        st.end()
    }
}

impl<'de> Deserialize<'de> for GrammarRule {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            name: String,
            source: String,
            #[serde(default)]
            params: Vec<String>,
        }
        let raw = Raw::deserialize(d)?;
        GrammarRule::try_from_source(raw.name.clone(), raw.source, raw.params).map_err(|error| {
            D::Error::custom(format!(
                "rule '{}' invalid source: {}",
                raw.name, error.message
            ))
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// Mutable bookkeeping attached to a [`Grammar`]: seal state, cached analysis,
/// and a monotonic version counter.
pub struct GrammarState {
    /// Whether the grammar is sealed against further edits.
    pub sealed: bool,
    /// Cached analysis results, invalidated on edit.
    pub analysis_state: Option<GrammarAnalysisState>,
    /// Version this state was computed at.
    pub version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// A parsed grammar: its rules, start rule, inline imports, and metadata.
pub struct Grammar {
    /// The rule evaluated by default.
    pub start_rule: String,
    /// Full grammar source text.
    pub text: String,
    /// The grammar's rules, in source order.
    pub rules: Vec<GrammarRule>,
    /// Per-owner metadata maps (e.g. the `__grammar__` configuration block).
    pub metadata: HashMap<String, HashMap<String, Value>>,
    /// Inline grammar imports: alias → grammar.  Used by `ImportedRef` and `GrammarScope`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub imports: HashMap<String, Box<Grammar>>,
    /// Monotonic version, bumped on every edit.
    pub version: u64,
    /// Seal/analysis/version bookkeeping.
    pub state: GrammarState,
    /// Per-grammar memo of the compiled form (see [`CompiledMemo`]). Not part of
    /// the grammar's identity: skipped by serde, ignored by `Eq`, reset on clone.
    #[serde(skip)]
    pub(crate) compiled: CompiledMemo,
}

impl Grammar {
    /// Create a grammar from trusted in-process grammar text.
    ///
    /// External grammar text must use [`Grammar::try_new`] so invalid grammar
    /// source is rejected at the boundary instead of panicking.
    pub fn trusted_new(text: impl Into<String>) -> Self {
        let text = text.into();
        Self::try_new(text)
            .expect("Grammar::trusted_new received invalid PEG grammar text; use try_new")
    }

    /// Parse grammar text, returning an error for invalid source.
    pub fn try_new(text: impl Into<String>) -> Result<Self, ParseError> {
        let text = text.into();
        let rules = try_parse_rules_from_text(&text)?;
        Ok(Self::from_parsed_text(text, rules))
    }

    fn from_parsed_text(text: String, rules: Vec<GrammarRule>) -> Self {
        Self {
            start_rule: "root".to_string(),
            text,
            rules,
            metadata: HashMap::new(),
            imports: HashMap::new(),
            version: 1,
            state: GrammarState {
                sealed: false,
                analysis_state: None,
                version: 0,
            },
            compiled: CompiledMemo::default(),
        }
    }

    /// Register an imported grammar under `alias` so that `ImportedRef` and
    /// `GrammarScope` can resolve cross-grammar references at parse time.
    pub fn with_import(self, alias: impl Into<String>, grammar: Grammar) -> Self {
        self.try_with_import(alias, grammar)
            .expect("trusted grammar import must advance grammar version")
    }

    /// Fallible [`with_import`](Self::with_import).
    pub fn try_with_import(
        mut self,
        alias: impl Into<String>,
        grammar: Grammar,
    ) -> Result<Self, ParseError> {
        let alias = validate_import_alias(alias)?;
        self.bump_version()?;
        self.imports.insert(alias, Box::new(grammar));
        self.clear_analysis_cache();
        Ok(self)
    }

    /// Attach an imported grammar under `alias` in place (panics on overflow).
    pub fn add_import(&mut self, alias: impl Into<String>, grammar: Grammar) {
        self.try_add_import(alias, grammar)
            .expect("trusted grammar import must advance grammar version");
    }

    /// Fallible [`add_import`](Self::add_import).
    pub fn try_add_import(
        &mut self,
        alias: impl Into<String>,
        grammar: Grammar,
    ) -> Result<(), ParseError> {
        let alias = validate_import_alias(alias)?;
        self.bump_version()?;
        self.imports.insert(alias, Box::new(grammar));
        self.clear_analysis_cache();
        Ok(())
    }

    /// Set the start rule (panics on version overflow).
    pub fn with_start_rule(self, start_rule: impl Into<String>) -> Self {
        self.try_with_start_rule(start_rule)
            .expect("trusted grammar start rule must advance grammar version")
    }

    /// Fallible [`with_start_rule`](Self::with_start_rule).
    pub fn try_with_start_rule(
        mut self,
        start_rule: impl Into<String>,
    ) -> Result<Self, ParseError> {
        self.bump_version()?;
        self.start_rule = start_rule.into();
        self.clear_analysis_cache();
        Ok(self)
    }

    /// Replace all rules (panics on version overflow).
    pub fn with_rules(self, rules: Vec<GrammarRule>) -> Self {
        self.try_with_rules(rules)
            .expect("trusted grammar rule replacement must advance grammar version")
    }

    /// Fallible [`with_rules`](Self::with_rules).
    pub fn try_with_rules(mut self, rules: Vec<GrammarRule>) -> Result<Self, ParseError> {
        self.bump_version()?;
        self.rules = rules;
        self.text = rules_to_text(&self.rules);
        self.clear_analysis_cache();
        Ok(self)
    }

    /// Attach a metadata map for `owner` (panics on version overflow).
    pub fn with_metadata(self, owner: impl Into<String>, metadata: HashMap<String, Value>) -> Self {
        self.try_with_metadata(owner, metadata)
            .expect("trusted grammar metadata must advance grammar version")
    }

    /// Fallible [`with_metadata`](Self::with_metadata).
    pub fn try_with_metadata(
        mut self,
        owner: impl Into<String>,
        metadata: HashMap<String, Value>,
    ) -> Result<Self, ParseError> {
        self.bump_version()?;
        self.metadata.insert(owner.into(), metadata);
        self.clear_analysis_cache();
        Ok(self)
    }

    /// Set a single metadata value under `owner`/`key` (panics on overflow).
    pub fn set_metadata_value(
        &mut self,
        owner: impl Into<String>,
        key: impl Into<String>,
        value: Value,
    ) {
        self.try_set_metadata_value(owner, key, value)
            .expect("trusted grammar metadata must advance grammar version");
    }

    /// Fallible [`set_metadata_value`](Self::set_metadata_value).
    pub fn try_set_metadata_value(
        &mut self,
        owner: impl Into<String>,
        key: impl Into<String>,
        value: Value,
    ) -> Result<(), ParseError> {
        self.bump_version()?;
        let owner = self.metadata.entry(owner.into()).or_default();
        owner.insert(key.into(), value);
        self.clear_analysis_cache();
        Ok(())
    }

    /// Number of rules in the grammar.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Look up a rule by name.
    pub fn get_rule(&self, name: &str) -> Option<&GrammarRule> {
        self.rules.iter().find(|rule| rule.name == name)
    }

    /// Add or replace a rule from PEG source (panics on parse/overflow error).
    pub fn set_rule(&mut self, name: impl Into<String>, source: impl Into<String>) {
        self.try_set_rule(name, source)
            .expect("trusted grammar rule source must parse and advance grammar version");
    }

    /// Fallible [`set_rule`](Self::set_rule).
    pub fn try_set_rule(
        &mut self,
        name: impl Into<String>,
        source: impl Into<String>,
    ) -> Result<(), ParseError> {
        let name = name.into();
        let source = source.into();
        let rule_index = self.rules.iter().position(|rule| rule.name == name);
        let rule = if let Some(index) = rule_index {
            GrammarRule::try_from_source(name.clone(), source, self.rules[index].params.clone())?
        } else {
            GrammarRule::try_from_source(name, source, Vec::new())?
        };
        let next_version = self.next_version()?;
        if let Some(index) = rule_index {
            self.rules[index] = rule;
        } else {
            self.rules.push(rule);
        }
        self.finish_rule_change(next_version);
        Ok(())
    }

    /// Return a new Grammar with additional or replaced rules.
    /// Each entry is `(rule_name, peg_source)` — delegates to [`Self::set_rule`].
    pub fn extend(self, rules: &[(&str, &str)]) -> Self {
        self.try_extend(rules)
            .expect("trusted grammar extension must parse and advance grammar version")
    }

    /// Fallible [`extend`](Self::extend).
    pub fn try_extend(mut self, rules: &[(&str, &str)]) -> Result<Self, ParseError> {
        for (name, src) in rules {
            self.try_set_rule(*name, *src)?;
        }
        Ok(self)
    }

    /// Remove a rule by name, returning whether it existed (panics on overflow).
    pub fn remove_rule(&mut self, name: &str) -> bool {
        self.try_remove_rule(name)
            .expect("trusted grammar rule removal must advance grammar version")
    }

    /// Fallible [`remove_rule`](Self::remove_rule).
    pub fn try_remove_rule(&mut self, name: &str) -> Result<bool, ParseError> {
        if let Some(index) = self.rules.iter().position(|rule| rule.name == name) {
            let next_version = self.next_version()?;
            self.rules.remove(index);
            self.finish_rule_change(next_version);
            return Ok(true);
        }
        Ok(false)
    }

    /// Advance the grammar version, erroring on overflow.
    pub fn bump_version(&mut self) -> Result<(), ParseError> {
        self.version = self.next_version()?;
        Ok(())
    }

    fn next_version(&self) -> Result<u64, ParseError> {
        self.version.checked_add(1).ok_or_else(|| {
            ParseError::new("grammar version overflow", 0, 0).with_code("grammar.version_overflow")
        })
    }

    fn finish_rule_change(&mut self, version: u64) {
        self.version = version;
        self.text = rules_to_text(&self.rules);
        self.clear_analysis_cache();
    }

    /// Drop any cached analysis results.
    pub fn clear_analysis_cache(&mut self) {
        self.state.analysis_state = None;
    }

    /// Seal the grammar against further edits.
    pub fn seal(&mut self) {
        self.state.sealed = true;
    }

    /// Unseal a sealed grammar.
    pub fn thaw(&mut self) {
        self.state.sealed = false;
    }

    /// Whether the grammar is sealed.
    pub fn is_sealed(&self) -> bool {
        self.state.sealed
    }
}

pub(crate) fn validate_import_alias(alias: impl Into<String>) -> Result<String, ParseError> {
    let alias = alias.into();
    if alias.is_empty() {
        return Err(
            ParseError::new("inline grammar import alias must be non-empty", 0, 0)
                .with_code("invalid_import_alias"),
        );
    }
    Ok(alias)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
/// A patch applied by [`crate::clone_grammar`]: new rule source and an optional
/// start-rule override.
pub struct GrammarPatch {
    /// PEG source for the rules to add/replace.
    pub source: String,
    /// Optional new start rule.
    pub start_rule: Option<String>,
}

/// Marker type namespacing the grammar-cloning entry points.
#[derive(Default)]
pub struct CloneGrammar;

pub(crate) fn try_parse_rules_from_text(text: &str) -> Result<Vec<GrammarRule>, ParseError> {
    let mut rules = Vec::new();
    for (line_idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (lhs, source) = if let Some((l, r)) = line.split_once("<-") {
            (l.trim(), r.trim())
        } else if let Some((l, r)) = line.split_once('=') {
            (l.trim(), r.trim())
        } else {
            return Err(ParseError::new(
                format!("invalid rule line {}: missing '<-' separator", line_idx + 1),
                0,
                line.len(),
            )
            .with_code("grammar.rule_header"));
        };
        let (name, params) = parse_rule_header(lhs, line_idx, line.len())?;
        rules.push(GrammarRule::try_from_source(
            name,
            source.to_string(),
            params,
        )?);
    }
    Ok(rules)
}

fn parse_rule_header(
    lhs: &str,
    line_idx: usize,
    line_len: usize,
) -> Result<(String, Vec<String>), ParseError> {
    if lhs.trim().is_empty() {
        return Err(ParseError::new(
            format!("invalid rule line {}: empty rule name", line_idx + 1),
            0,
            line_len,
        )
        .with_code("grammar.empty_rule_name"));
    }
    if lhs.ends_with(')') {
        if let Some(paren_pos) = lhs.rfind('(') {
            let rule_name = lhs[..paren_pos].trim().to_string();
            if rule_name.is_empty() {
                return Err(ParseError::new(
                    format!("invalid rule line {}: empty rule name", line_idx + 1),
                    0,
                    line_len,
                )
                .with_code("grammar.empty_rule_name"));
            }
            let params_str = &lhs[paren_pos + 1..lhs.len() - 1];
            let params: Vec<String> = params_str
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            return Ok((rule_name, params));
        }
    }
    Ok((lhs.to_string(), Vec::new()))
}

pub(crate) fn rules_to_text(rules: &[GrammarRule]) -> String {
    let mut out = String::new();
    for (idx, rule) in rules.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(&rule.name);
        if !rule.params.is_empty() {
            out.push('(');
            out.push_str(&rule.params.join(", "));
            out.push(')');
        }
        out.push_str(" <- ");
        out.push_str(&rule.source);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_rule_deserialize_rejects_invalid_source() {
        let payload = r#"{"name":"root","source":"[a"}"#;
        let err = serde_json::from_str::<GrammarRule>(payload)
            .expect_err("invalid rule source should fail deserialization");
        assert!(err.to_string().contains("invalid source"));
    }

    #[test]
    fn grammar_rule_deserialize_rebuilds_expr_from_source() {
        let payload = r#"{"name":"root","source":"[a]","params":["x"]}"#;
        let rule = serde_json::from_str::<GrammarRule>(payload).expect("rule deserializes");
        assert_eq!(rule.name, "root");
        assert_eq!(rule.params, vec!["x"]);
        assert!(matches!(rule.expr(), PegExpr::CharClass(_)));
    }

    #[test]
    fn trusted_rule_constructor_panics_on_invalid_source() {
        let panic = std::panic::catch_unwind(|| {
            GrammarRule::trusted_from_source("root", "[a", Vec::new());
        });
        assert!(panic.is_err());
    }

    #[test]
    fn trusted_rule_setter_panics_on_invalid_source_without_mutating() {
        let mut rule = GrammarRule::trusted_from_source("root", "[a]", Vec::new());
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rule.trusted_set_source("[a");
        }));
        assert!(panic.is_err());
        assert_eq!(rule.source, "[a]");
        assert!(matches!(rule.expr(), PegExpr::CharClass(_)));
    }

    #[test]
    fn grammar_start_rule_and_imports_advance_version() {
        let grammar = Grammar::trusted_new("root <- 'x'");
        let base_version = grammar.version;

        let grammar = grammar.with_start_rule("root");
        assert_eq!(grammar.version, base_version + 1);

        let import_version = grammar.version;
        let grammar = grammar.with_import("dep", Grammar::trusted_new("root <- 'y'"));
        assert_eq!(grammar.version, import_version + 1);
    }

    #[test]
    fn grammar_try_set_rule_rejects_version_overflow_without_mutating() {
        let mut grammar = Grammar::trusted_new("root <- 'x'");
        grammar.version = u64::MAX;

        let error = grammar.try_set_rule("root", "'y'").unwrap_err();

        assert_eq!(error.code.as_deref(), Some("grammar.version_overflow"));
        assert_eq!(grammar.get_rule("root").unwrap().source, "'x'");
        assert_eq!(grammar.version, u64::MAX);
    }

    #[test]
    fn grammar_try_remove_rule_rejects_version_overflow_without_mutating() {
        let mut grammar = Grammar::trusted_new("root <- 'x'\nhelper <- 'y'");
        grammar.version = u64::MAX;

        let error = grammar.try_remove_rule("helper").unwrap_err();

        assert_eq!(error.code.as_deref(), Some("grammar.version_overflow"));
        assert!(grammar.get_rule("helper").is_some());
        assert_eq!(grammar.version, u64::MAX);
    }
}
