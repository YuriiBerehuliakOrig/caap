use crate::analysis::GrammarAnalysisState;
use crate::expr::{PegExpr, RuleTextParser};
use std::collections::HashMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// A single grammar rule storing both its source text and compiled expression tree.
///
/// `source` and `expr` are always kept in sync — use `GrammarRule::from_source` to
/// construct and `GrammarRule::set_source` to mutate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarRule {
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
    /// Create a rule from text, compiling `source` into a `PegExpr` immediately.
    ///
    /// If `source` is syntactically invalid, `expr` is set to
    /// `PegExpr::Invalid(error_message)` and the error surfaces when the
    /// grammar is compiled for a parse call (same behaviour as before).
    pub fn from_source(
        name: impl Into<String>,
        source: impl Into<String>,
        params: Vec<String>,
    ) -> Self {
        let name = name.into();
        let source = source.into();
        let expr =
            RuleTextParser::parse(&source).unwrap_or_else(|e| PegExpr::Invalid(e.message.into()));
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

    /// Update `source` and recompile `expr` to keep them in sync.
    pub fn set_source(&mut self, source: impl Into<String>) {
        let source = source.into();
        self.expr =
            RuleTextParser::parse(&source).unwrap_or_else(|e| PegExpr::Invalid(e.message.into()));
        self.source = source;
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
        Ok(GrammarRule::from_source(raw.name, raw.source, raw.params))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GrammarState {
    pub sealed: bool,
    pub analysis_state: Option<GrammarAnalysisState>,
    pub version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Grammar {
    pub start_rule: String,
    pub text: String,
    pub rules: Vec<GrammarRule>,
    pub metadata: HashMap<String, HashMap<String, Value>>,
    /// Inline grammar imports: alias → grammar.  Used by `ImportedRef` and `GrammarScope`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub imports: HashMap<String, Box<Grammar>>,
    pub version: u32,
    pub state: GrammarState,
}

impl Grammar {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let rules = parse_rules_from_text(&text);
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
        }
    }

    /// Register an imported grammar under `alias` so that `ImportedRef` and
    /// `GrammarScope` can resolve cross-grammar references at parse time.
    pub fn with_import(mut self, alias: impl Into<String>, grammar: Grammar) -> Self {
        self.imports.insert(alias.into(), Box::new(grammar));
        self
    }

    pub fn add_import(&mut self, alias: impl Into<String>, grammar: Grammar) {
        self.imports.insert(alias.into(), Box::new(grammar));
    }

    pub fn with_start_rule(mut self, start_rule: impl Into<String>) -> Self {
        self.start_rule = start_rule.into();
        self.clear_analysis_cache();
        self
    }

    pub fn with_rules(mut self, rules: Vec<GrammarRule>) -> Self {
        self.rules = rules;
        self.version = self.version.saturating_add(1);
        self.text = rules_to_text(&self.rules);
        self.clear_analysis_cache();
        self
    }

    pub fn with_metadata(
        mut self,
        owner: impl Into<String>,
        metadata: HashMap<String, Value>,
    ) -> Self {
        self.metadata.insert(owner.into(), metadata);
        self.version = self.version.saturating_add(1);
        self.clear_analysis_cache();
        self
    }

    pub fn set_metadata_value(
        &mut self,
        owner: impl Into<String>,
        key: impl Into<String>,
        value: Value,
    ) {
        let owner = self.metadata.entry(owner.into()).or_default();
        owner.insert(key.into(), value);
        self.version = self.version.saturating_add(1);
        self.clear_analysis_cache();
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    pub fn get_rule(&self, name: &str) -> Option<&GrammarRule> {
        self.rules.iter().find(|rule| rule.name == name)
    }

    pub fn set_rule(&mut self, name: impl Into<String>, source: impl Into<String>) {
        let name = name.into();
        let source = source.into();
        if let Some(existing) = self.rules.iter_mut().find(|rule| rule.name == name) {
            existing.set_source(source);
        } else {
            self.rules
                .push(GrammarRule::from_source(name, source, Vec::new()));
        }
        self.version = self.version.saturating_add(1);
        self.text = rules_to_text(&self.rules);
        self.clear_analysis_cache();
    }

    pub fn remove_rule(&mut self, name: &str) -> bool {
        let before = self.rules.len();
        self.rules.retain(|rule| rule.name != name);
        if self.rules.len() != before {
            self.version = self.version.saturating_add(1);
            self.text = rules_to_text(&self.rules);
            self.clear_analysis_cache();
            return true;
        }
        false
    }

    pub fn clear_analysis_cache(&mut self) {
        self.state.analysis_state = None;
    }

    pub fn seal(&mut self) {
        self.state.sealed = true;
    }

    pub fn thaw(&mut self) {
        self.state.sealed = false;
    }

    pub fn is_sealed(&self) -> bool {
        self.state.sealed
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GrammarPatch {
    pub source: String,
    pub start_rule: Option<String>,
}

#[derive(Default)]
pub struct CloneGrammar;

pub(crate) fn parse_rules_from_text(text: &str) -> Vec<GrammarRule> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let (lhs, source, sep) = if let Some((l, r)) = line.split_once("<-") {
                (l.trim(), r.trim(), "<-")
            } else if let Some((l, r)) = line.split_once('=') {
                (l.trim(), r.trim(), "=")
            } else {
                return None;
            };
            // Parse optional parameter list: `rule_name(x, y, ...)` or just `rule_name`
            let (name, params) = if lhs.ends_with(')') {
                if let Some(paren_pos) = lhs.rfind('(') {
                    let rule_name = lhs[..paren_pos].trim().to_string();
                    let params_str = &lhs[paren_pos + 1..lhs.len() - 1];
                    let params: Vec<String> = params_str
                        .split(',')
                        .map(|p| p.trim().to_string())
                        .filter(|p| !p.is_empty())
                        .collect();
                    (rule_name, params)
                } else {
                    (lhs.to_string(), Vec::new())
                }
            } else {
                (lhs.to_string(), Vec::new())
            };
            let _ = sep; // used for parsing
            Some(GrammarRule::from_source(name, source.to_string(), params))
        })
        .collect()
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
