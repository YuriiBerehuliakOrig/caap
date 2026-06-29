//! [`SpecCompiler`] — compile a JSON list-format grammar spec
//! (`["grammar", name, root, [rules…]]`) into a [`Grammar`].

use serde_json::Value;
use std::collections::HashMap;

use crate::expr::PegExpr;
use crate::grammar::{Grammar, GrammarRule};

use super::spec_compiler_exprs::{expr_to_peg_expr, expr_to_source};
use super::spec_compiler_helpers::{
    expect_arr, expect_bool, expect_non_empty_str, expect_str, require_non_empty_str, string_array,
    string_values, type_name,
};

// ── Error ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
/// Why compiling a JSON grammar spec failed.
pub enum SpecCompileError {
    #[error("invalid spec format: {0}")]
    /// The spec was not in the expected list format.
    InvalidFormat(String),
    #[error("unknown expr tag: {0}")]
    /// An expression used an unrecognised tag.
    UnknownTag(String),
    #[error("missing required field: {0}")]
    /// A required field was absent.
    MissingField(String),
    #[error("type error: expected {expected}, got {actual} in {ctx}")]
    /// A value had the wrong type.
    TypeError {
        /// The expected type name.
        expected: &'static str,
        /// The actual type name.
        actual: String,
        /// Where the mismatch occurred.
        ctx: String,
    },
    #[error("duplicate rule: {0}")]
    /// Two rules shared a name.
    DuplicateRule(String),
}

// ── Top-level compiler ─────────────────────────────────────────────────────

/// Compiles a PEG grammar spec (JSON list format) into a `Grammar`.
///
/// # Spec format
///
/// ```json
/// ["grammar", "name", "root_rule", [rules...], options...]
/// ```
///
/// Where each rule is:
/// ```json
/// ["rule", "rule_name", expr, ...metadata]
/// ```
///
/// And `expr` is one of the tagged expression forms listed in `expr_to_source`.
#[derive(Debug, Default, Clone)]
pub struct SpecCompiler {
    /// Allow inline action expressions (e.g. `["action", ...]`).
    pub allow_inline_actions: bool,
}

impl SpecCompiler {
    /// A compiler with default options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Compile the spec value into a `Grammar`.
    pub fn compile(&self, spec: &Value) -> Result<Grammar, SpecCompileError> {
        let arr = expect_arr(spec, "spec root")?;
        if arr.len() < 4 || arr[0].as_str() != Some("grammar") {
            return Err(SpecCompileError::InvalidFormat(
                "expected [\"grammar\", name, root, [rules], ...]".to_string(),
            ));
        }

        let grammar_name = expect_str(&arr[1], "grammar name")?;
        let root_rule = expect_str(&arr[2], "grammar root rule")?;
        let rules_spec = expect_arr(&arr[3], "rules list")?;

        let mut ctx = CompileState::default();

        // Process top-level options (indices 4+)
        for entry in arr.iter().skip(4) {
            apply_top_level_option(&mut ctx, entry)?;
        }

        // Compile rules
        for rule_entry in rules_spec {
            compile_rule(&mut ctx, rule_entry)?;
        }

        // Build the Grammar
        let mut grammar = Grammar::trusted_new("").with_start_rule(root_rule.to_string());
        // Clear the auto-parsed empty rules
        grammar.rules.clear();
        grammar.text.clear();

        for rule in &ctx.rules {
            grammar.rules.push(GrammarRule {
                name: rule.name.clone(),
                source: rule.source.clone(),
                params: rule.params.clone(),
                expr: rule.expr.clone(),
            });
            if !grammar.text.is_empty() {
                grammar.text.push('\n');
            }
            grammar.text.push_str(&rule.name);
            grammar.text.push_str(" <- ");
            grammar.text.push_str(&rule.source);
        }

        // Store options as __grammar__ metadata
        let mut gmeta: HashMap<String, Value> = HashMap::new();
        if !ctx.hard_keywords.is_empty() {
            gmeta.insert(
                "hard_keywords".to_string(),
                string_array(&ctx.hard_keywords),
            );
        }
        if !ctx.soft_keywords.is_empty() {
            gmeta.insert(
                "soft_keywords".to_string(),
                string_array(&ctx.soft_keywords),
            );
        }
        if !ctx.strict_actions {
            gmeta.insert("strict_actions".to_string(), Value::Bool(false));
        }
        if let Some(ws) = &ctx.whitespace {
            gmeta.insert("whitespace".to_string(), Value::String(ws.clone()));
        }
        if !ctx.line_comments.is_empty() {
            gmeta.insert(
                "line_comments".to_string(),
                string_array(&ctx.line_comments),
            );
        }
        if ctx.indentation {
            gmeta.insert("indentation".to_string(), Value::Bool(true));
        }
        if !ctx.imports.is_empty() {
            let imports_val = Value::Object(
                ctx.imports
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect(),
            );
            gmeta.insert("imports".to_string(), imports_val);
        }
        // Merge grammar_metadata (lower priority than explicitly set keys)
        for (k, v) in &ctx.grammar_metadata {
            gmeta.entry(k.clone()).or_insert_with(|| v.clone());
        }
        if !gmeta.is_empty() {
            grammar.metadata.insert("__grammar__".to_string(), gmeta);
        }

        // Per-rule metadata
        for (rule_name, meta) in ctx.rule_metadata {
            grammar.metadata.insert(rule_name, meta);
        }

        // Store grammar name if useful
        if grammar_name != root_rule {
            grammar
                .metadata
                .entry("__grammar__".to_string())
                .or_default()
                .insert("name".to_string(), Value::String(grammar_name.to_string()));
        }

        Ok(grammar)
    }
}

// ── Internal state ─────────────────────────────────────────────────────────

pub(super) struct CompileState {
    pub(super) rules: Vec<CompiledRuleSpec>, // ordered insertion
    pub(super) seen_names: std::collections::HashSet<String>,
    pub(super) rule_metadata: HashMap<String, HashMap<String, Value>>,
    pub(super) hard_keywords: Vec<String>,
    pub(super) soft_keywords: Vec<String>,
    pub(super) strict_actions: bool,
    pub(super) whitespace: Option<String>,
    pub(super) line_comments: Vec<String>,
    /// Whether indentation-sensitive mode is enabled.
    pub(super) indentation: bool,
    /// Extra grammar-level metadata keys.
    pub(super) grammar_metadata: HashMap<String, Value>,
    /// Imports: alias → grammar name (stored in __grammar__ imports metadata).
    pub(super) imports: HashMap<String, String>,
}

#[derive(Clone, Debug)]
pub(super) struct CompiledRuleSpec {
    pub(super) name: String,
    pub(super) source: String,
    pub(super) params: Vec<String>,
    pub(super) expr: PegExpr,
}

impl Default for CompileState {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            seen_names: std::collections::HashSet::new(),
            rule_metadata: HashMap::new(),
            hard_keywords: Vec::new(),
            soft_keywords: Vec::new(),
            strict_actions: true,
            whitespace: None,
            line_comments: Vec::new(),
            indentation: false,
            grammar_metadata: HashMap::new(),
            imports: HashMap::new(),
        }
    }
}

// ── Top-level option handlers ──────────────────────────────────────────────

fn apply_top_level_option(ctx: &mut CompileState, entry: &Value) -> Result<(), SpecCompileError> {
    let arr = expect_arr(entry, "grammar top-level option")?;
    if arr.is_empty() {
        return Err(SpecCompileError::InvalidFormat(
            "grammar top-level option must not be empty".to_string(),
        ));
    }

    let tag = expect_str(&arr[0], "grammar top-level option tag")?;

    match tag {
        "hard_keywords" => {
            ctx.hard_keywords = string_values(&arr[1..], "hard_keywords")?;
        }
        "soft_keywords" => {
            ctx.soft_keywords = string_values(&arr[1..], "soft_keywords")?;
        }
        "strict_actions" => {
            if arr.len() > 2 {
                return Err(SpecCompileError::InvalidFormat(
                    "strict_actions option expects at most one boolean argument".to_string(),
                ));
            }
            ctx.strict_actions = if arr.len() < 2 {
                true
            } else {
                expect_bool(&arr[1], "strict_actions")?
            };
        }
        "trivia" => {
            for item in arr.iter().skip(1) {
                let sub = expect_arr(item, "trivia entry")?;
                if sub.is_empty() {
                    return Err(SpecCompileError::InvalidFormat(
                        "trivia entry must not be empty".to_string(),
                    ));
                }
                match expect_str(&sub[0], "trivia entry tag")? {
                    "whitespace" => {
                        ctx.whitespace =
                            Some(string_values(&sub[1..], "trivia whitespace")?.concat());
                    }
                    "line_comments" => {
                        ctx.line_comments = string_values(&sub[1..], "trivia line_comments")?;
                    }
                    other => {
                        return Err(SpecCompileError::InvalidFormat(format!(
                            "unsupported trivia entry '{other}'"
                        )));
                    }
                }
            }
        }
        "indentation" => {
            if arr.len() > 2 {
                return Err(SpecCompileError::InvalidFormat(
                    "indentation option expects at most one boolean argument".to_string(),
                ));
            }
            ctx.indentation = if arr.len() < 2 {
                true
            } else {
                expect_bool(&arr[1], "indentation")?
            };
        }
        "grammar_metadata"
            // ["grammar_metadata", key, value] sets a grammar-level metadata key.
            if arr.len() == 3 => {
                let key = expect_str(&arr[1], "grammar metadata key")?;
                ctx.grammar_metadata.insert(key.to_string(), arr[2].clone());
            }
        "imports"
            if arr.len() >= 2 => {
                if arr.len() != 2 {
                    return Err(SpecCompileError::InvalidFormat(
                        "imports option expects exactly one object argument".to_string(),
                    ));
                }
                let Some(map) = arr[1].as_object() else {
                    return Err(SpecCompileError::TypeError {
                        expected: "object",
                        actual: type_name(&arr[1]).to_string(),
                        ctx: "imports payload".to_string(),
                    });
                };
                for (alias, name) in map {
                    let alias = require_non_empty_str(alias, "imports alias")?;
                    let n = expect_non_empty_str(name, "imports map value")?;
                    ctx.imports.insert(alias.to_string(), n.to_string());
                }
            }
        "grammar_metadata" => {
            return if arr.len() < 3 {
                Err(SpecCompileError::MissingField(
                    "grammar metadata key/value".to_string(),
                ))
            } else {
                Err(SpecCompileError::InvalidFormat(
                    "metadata option expects exactly key and value".to_string(),
                ))
            };
        }
        "imports" => {
            return Err(SpecCompileError::MissingField("imports payload".to_string()));
        }
        "semantic_hooks" | "recovery" | "recover_sync" | "rule_memo" => {
            return Err(SpecCompileError::InvalidFormat(format!(
                "unsupported grammar top-level option '{tag}'"
            )));
        }
        other => {
            return Err(SpecCompileError::UnknownTag(other.to_string()));
        }
    }
    Ok(())
}

// ── Rule compilation ───────────────────────────────────────────────────────

fn compile_rule(ctx: &mut CompileState, entry: &Value) -> Result<(), SpecCompileError> {
    let arr = expect_arr(entry, "rule entry")?;
    if arr.len() < 3 || arr[0].as_str() != Some("rule") {
        return Err(SpecCompileError::InvalidFormat(
            "rule entry must be [\"rule\", name, expr, ...]".to_string(),
        ));
    }

    let rule_name = parse_rule_name(&arr[1])?;
    if !ctx.seen_names.insert(rule_name.clone()) {
        return Err(SpecCompileError::DuplicateRule(rule_name));
    }

    let source = expr_to_source(&arr[2])?;
    let expr = expr_to_peg_expr(&arr[2])?;
    let params = parse_rule_params(&arr[1])?;
    ctx.rules.push(CompiledRuleSpec {
        name: rule_name.clone(),
        source,
        params,
        expr,
    });

    // Optional metadata entries (indices 3+)
    for meta in arr.iter().skip(3) {
        apply_rule_meta(ctx, &rule_name, meta)?;
    }

    Ok(())
}

fn parse_rule_params(header: &Value) -> Result<Vec<String>, SpecCompileError> {
    let Some(arr) = header.as_array() else {
        return Ok(Vec::new());
    };
    let mut params = Vec::new();
    for item in arr.iter().skip(1) {
        if item.as_str() == Some("->") {
            break;
        }
        params.push(expect_str(item, "rule parameter")?.to_string());
    }
    Ok(params)
}

fn parse_rule_name(header: &Value) -> Result<String, SpecCompileError> {
    // header can be a plain string or a list: ["name", param1, ...]
    if let Some(s) = header.as_str() {
        return Ok(s.to_string());
    }
    if let Some(arr) = header.as_array() {
        if let Some(s) = arr.first().and_then(|v| v.as_str()) {
            return Ok(s.to_string());
        }
    }
    if let Some(obj) = header.as_object() {
        if let Some(s) = obj.get("name").and_then(|v| v.as_str()) {
            return Ok(s.to_string());
        }
    }
    Err(SpecCompileError::TypeError {
        expected: "string or rule header",
        actual: super::spec_compiler_helpers::type_name(header).to_string(),
        ctx: "rule name".to_string(),
    })
}

fn apply_rule_meta(
    ctx: &mut CompileState,
    rule_name: &str,
    meta: &Value,
) -> Result<(), SpecCompileError> {
    let arr = match meta.as_array() {
        Some(a) if !a.is_empty() => a,
        Some(_) => {
            return Err(SpecCompileError::InvalidFormat(
                "rule metadata entry must not be empty".to_string(),
            ));
        }
        _ => {
            return Err(SpecCompileError::TypeError {
                expected: "metadata list",
                actual: super::spec_compiler_helpers::type_name(meta).to_string(),
                ctx: "rule metadata".to_string(),
            });
        }
    };

    match arr[0].as_str() {
        Some("metadata") if arr.len() == 3 => {
            let key = expect_str(&arr[1], "metadata key")?;
            ctx.rule_metadata
                .entry(rule_name.to_string())
                .or_default()
                .insert(key.to_string(), arr[2].clone());
        }
        Some("type") if arr.len() == 2 => {
            let return_type = expect_str(&arr[1], "rule return type")?;
            ctx.rule_metadata
                .entry(rule_name.to_string())
                .or_default()
                .insert(
                    "return_type".to_string(),
                    Value::String(return_type.to_string()),
                );
        }
        Some("memo") => {
            if arr.len() > 2 {
                return Err(SpecCompileError::InvalidFormat(
                    "rule memo metadata expects at most one value".to_string(),
                ));
            }
            let enabled = if arr.len() < 2 {
                true
            } else {
                expect_bool(&arr[1], "memo")?
            };
            ctx.rule_metadata
                .entry(rule_name.to_string())
                .or_default()
                .insert("memo".to_string(), Value::Bool(enabled));
        }
        Some("metadata") => {
            return Err(SpecCompileError::InvalidFormat(
                "rule metadata entry expects exactly key and value".to_string(),
            ));
        }
        Some("type") => {
            return Err(SpecCompileError::InvalidFormat(
                "rule type metadata expects exactly one value".to_string(),
            ));
        }
        Some(other) => return Err(SpecCompileError::UnknownTag(other.to_string())),
        None => {
            return Err(SpecCompileError::TypeError {
                expected: "metadata tag string",
                actual: super::spec_compiler_helpers::type_name(&arr[0]).to_string(),
                ctx: "rule metadata tag".to_string(),
            });
        }
    }
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::PegExpr;
    use crate::spec_compiler_exprs::expr_to_source;
    use serde_json::json;

    fn compile(spec: Value) -> Grammar {
        SpecCompiler::new().compile(&spec).expect("compile ok")
    }

    fn compile_err(spec: Value) -> SpecCompileError {
        SpecCompiler::new().compile(&spec).unwrap_err()
    }

    // ── expr_to_source ─────────────────────────────────────────────────

    #[test]
    fn expr_lit() {
        let src = expr_to_source(&json!(["lit", "hello"])).unwrap();
        assert_eq!(src, "'hello'");
    }

    #[test]
    fn expr_lit_escapes_apostrophe() {
        let src = expr_to_source(&json!(["lit", "it's"])).unwrap();
        assert_eq!(src, "'it\\'s'");
    }

    #[test]
    fn expr_regex() {
        let src = expr_to_source(&json!(["regex", "[a-z]+"])).unwrap();
        assert_eq!(src, "/[a-z]+/");
    }

    #[test]
    fn expr_ref() {
        let src = expr_to_source(&json!(["ref", "expr"])).unwrap();
        assert_eq!(src, "expr");
    }

    #[test]
    fn expr_seq() {
        let src = expr_to_source(&json!(["seq", [["lit", "a"], ["ref", "b"]]])).unwrap();
        assert_eq!(src, "('a' b)");
    }

    #[test]
    fn expr_seq_single_unwraps() {
        let src = expr_to_source(&json!(["seq", [["lit", "x"]]])).unwrap();
        assert_eq!(src, "'x'");
    }

    #[test]
    fn expr_choice() {
        let src = expr_to_source(&json!(["choice", [["lit", "a"], ["lit", "b"]]])).unwrap();
        assert_eq!(src, "('a' / 'b')");
    }

    #[test]
    fn expr_quantifiers() {
        assert_eq!(
            expr_to_source(&json!(["star", ["ref", "x"]])).unwrap(),
            "x*"
        );
        assert_eq!(
            expr_to_source(&json!(["plus", ["ref", "x"]])).unwrap(),
            "x+"
        );
        assert_eq!(expr_to_source(&json!(["opt", ["ref", "x"]])).unwrap(), "x?");
    }

    #[test]
    fn expr_predicates() {
        assert_eq!(
            expr_to_source(&json!(["and", ["ref", "ws"]])).unwrap(),
            "&ws"
        );
        assert_eq!(
            expr_to_source(&json!(["not", ["lit", "end"]])).unwrap(),
            "!'end'"
        );
    }

    #[test]
    fn expr_named() {
        let src = expr_to_source(&json!(["named", "lhs", ["ref", "expr"]])).unwrap();
        assert_eq!(src, "lhs:expr");
    }

    #[test]
    fn expr_cut() {
        assert_eq!(expr_to_source(&json!(["cut"])).unwrap(), "~");
        assert_eq!(expr_to_source(&json!(["~"])).unwrap(), "~");
    }

    #[test]
    fn expr_sep_plus() {
        let src = expr_to_source(&json!(["sep_plus", ["lit", ","], ["ref", "item"]])).unwrap();
        assert!(src.contains("item"));
        assert!(src.contains("','"));
    }

    #[test]
    fn expr_unknown_tag_errors() {
        let err = expr_to_source(&json!(["zap", "x"])).unwrap_err();
        assert!(matches!(err, SpecCompileError::UnknownTag(_)));
    }

    // ── Full grammar compilation ───────────────────────────────────────

    #[test]
    fn compile_simple_grammar() {
        let g = compile(json!([
            "grammar",
            "simple",
            "root",
            [["rule", "root", ["lit", "hello"]]]
        ]));
        assert_eq!(g.start_rule, "root");
        assert!(g.get_rule("root").is_some());
        assert_eq!(g.get_rule("root").unwrap().source, "'hello'");
    }

    #[test]
    fn compile_authoring_operator_aliases() {
        let g = compile(json!([
            "grammar",
            "aliases",
            "root",
            [[
                "rule",
                "root",
                [
                    "seq",
                    [
                        ["literal", "["],
                        ["many", ["regex", "[0-9]+"]],
                        ["literal", "]"]
                    ]
                ]
            ]]
        ]));
        assert_eq!(g.get_rule("root").unwrap().source, "('[' /[0-9]+/* ']')");
    }

    #[test]
    fn compile_grammar_with_choice() {
        let g = compile(json!([
            "grammar",
            "g",
            "top",
            [["rule", "top", ["choice", [["lit", "a"], ["lit", "b"]]]]]
        ]));
        assert_eq!(g.get_rule("top").unwrap().source, "('a' / 'b')");
    }

    #[test]
    fn compile_grammar_with_sequence() {
        let g = compile(json!([
            "grammar",
            "g",
            "root",
            [
                ["rule", "root", ["seq", [["lit", "x"], ["ref", "rest"]]]],
                ["rule", "rest", ["lit", "y"]]
            ]
        ]));
        assert_eq!(g.rule_count(), 2);
        assert_eq!(g.get_rule("root").unwrap().source, "('x' rest)");
    }

    #[test]
    fn compile_core_nodes_directly_to_expr_variants() {
        let g = compile(json!([
            "grammar",
            "g",
            "start",
            [["rule", "start", ["soft_kw", "async"]]]
        ]));
        assert!(
            matches!(g.get_rule("start").unwrap().expr(), PegExpr::SoftKeyword(text) if text == "async")
        );
    }

    #[test]
    fn compile_grammar_with_hard_keywords() {
        let g = compile(json!([
            "grammar",
            "g",
            "root",
            [["rule", "root", ["lit", "x"]]],
            ["hard_keywords", "if", "else", "for"]
        ]));
        let gmeta = g.metadata.get("__grammar__").expect("grammar meta");
        let kws = gmeta.get("hard_keywords").expect("hard_keywords");
        let arr = kws.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0].as_str().unwrap(), "if");
    }

    #[test]
    fn compile_grammar_strict_actions_false() {
        let g = compile(json!([
            "grammar",
            "g",
            "root",
            [["rule", "root", ["lit", "x"]]],
            ["strict_actions", false]
        ]));
        let gmeta = g.metadata.get("__grammar__");
        if let Some(m) = gmeta {
            if let Some(v) = m.get("strict_actions") {
                assert_eq!(v.as_bool(), Some(false));
            }
        }
    }

    #[test]
    fn compile_grammar_with_trivia() {
        let g = compile(json!([
            "grammar",
            "g",
            "root",
            [["rule", "root", ["lit", "x"]]],
            ["trivia", ["whitespace", " \t\n"], ["line_comments", "//"]]
        ]));
        let gmeta = g.metadata.get("__grammar__").expect("grammar meta");
        let ws = gmeta.get("whitespace").expect("whitespace");
        assert_eq!(ws.as_str().unwrap(), " \t\n");
    }

    #[test]
    fn compile_invalid_spec_format_errors() {
        let err = compile_err(json!(["not_grammar", "x", "y", []]));
        assert!(matches!(err, SpecCompileError::InvalidFormat(_)));
    }

    #[test]
    fn compile_unknown_top_level_option_errors() {
        let err = compile_err(json!([
            "grammar",
            "g",
            "root",
            [["rule", "root", ["lit", "x"]]],
            ["unknown_option", true]
        ]));
        assert!(matches!(err, SpecCompileError::UnknownTag(tag) if tag == "unknown_option"));
    }

    #[test]
    fn compile_top_level_options_reject_alias_tags() {
        for option in [
            json!(["hard-keywords", "if"]),
            json!(["soft-keywords", "match"]),
            json!(["strict-actions", false]),
            json!(["metadata", "custom_key", 42]),
            json!(["grammar-metadata", "custom_key", 42]),
            json!(["semantic-hooks"]),
            json!(["recover-sync"]),
            json!(["rule-memo"]),
        ] {
            let err = compile_err(json!([
                "grammar",
                "g",
                "root",
                [["rule", "root", ["lit", "x"]]],
                option
            ]));
            assert!(
                matches!(err, SpecCompileError::UnknownTag(_)),
                "expected UnknownTag for alias option, got {err:?}"
            );
        }
    }

    #[test]
    fn compile_malformed_top_level_option_errors() {
        let err = compile_err(json!([
            "grammar",
            "g",
            "root",
            [["rule", "root", ["lit", "x"]]],
            "not-an-option-list"
        ]));
        assert!(matches!(err, SpecCompileError::TypeError { .. }));
    }

    #[test]
    fn compile_top_level_option_rejects_non_string_keyword() {
        let err = compile_err(json!([
            "grammar",
            "g",
            "root",
            [["rule", "root", ["lit", "x"]]],
            ["hard_keywords", "if", 123]
        ]));
        assert!(matches!(err, SpecCompileError::TypeError { .. }));
    }

    #[test]
    fn compile_top_level_options_reject_extra_arguments() {
        for option in [
            json!(["grammar_metadata", "key", "value", "extra"]),
            json!(["imports", "dep", "Grammar", "extra"]),
            json!(["imports", {"dep": "Grammar"}, "extra"]),
            json!(["strict_actions", true, false]),
            json!(["indentation", true, false]),
        ] {
            let err = compile_err(json!([
                "grammar",
                "g",
                "root",
                [["rule", "root", ["lit", "x"]]],
                option
            ]));
            assert!(
                matches!(err, SpecCompileError::InvalidFormat(_)),
                "expected InvalidFormat, got {err:?}"
            );
        }
    }

    #[test]
    fn compile_boolean_options_reject_coerced_values() {
        for option in [
            json!(["strict_actions", "false"]),
            json!(["strict_actions", 0]),
            json!(["indentation", "on"]),
            json!(["indentation", 1]),
        ] {
            let err = compile_err(json!([
                "grammar",
                "g",
                "root",
                [["rule", "root", ["lit", "x"]]],
                option
            ]));
            assert!(
                matches!(
                    err,
                    SpecCompileError::TypeError {
                        expected: "bool",
                        ..
                    }
                ),
                "expected bool TypeError, got {err:?}"
            );
        }
    }

    #[test]
    fn compile_imports_accepts_object_mapping_only() {
        let g = compile(json!([
            "grammar",
            "g",
            "root",
            [["rule", "root", ["lit", "x"]]],
            ["imports", {"dep": "Grammar"}]
        ]));
        let gmeta = g.metadata.get("__grammar__").expect("grammar meta");
        assert_eq!(
            gmeta
                .get("imports")
                .and_then(|v| v.as_object())
                .and_then(|imports| imports.get("dep"))
                .and_then(|v| v.as_str()),
            Some("Grammar")
        );
    }

    #[test]
    fn compile_imports_rejects_positional_tuple_shape() {
        let err = compile_err(json!([
            "grammar",
            "g",
            "root",
            [["rule", "root", ["lit", "x"]]],
            ["imports", "dep", "Grammar"]
        ]));
        assert!(matches!(err, SpecCompileError::InvalidFormat(_)));
    }

    #[test]
    fn compile_imports_rejects_non_object_payload() {
        let err = compile_err(json!([
            "grammar",
            "g",
            "root",
            [["rule", "root", ["lit", "x"]]],
            ["imports", "dep"]
        ]));
        assert!(matches!(
            err,
            SpecCompileError::TypeError {
                expected: "object",
                ctx,
                ..
            } if ctx == "imports payload"
        ));
    }

    #[test]
    fn compile_imports_rejects_empty_alias_or_target() {
        for option in [
            json!(["imports", {"": "Grammar"}]),
            json!(["imports", {"dep": ""}]),
        ] {
            let err = compile_err(json!([
                "grammar",
                "g",
                "root",
                [["rule", "root", ["lit", "x"]]],
                option
            ]));
            assert!(
                matches!(err, SpecCompileError::InvalidFormat(_)),
                "expected InvalidFormat, got {err:?}"
            );
        }
    }

    #[test]
    fn compile_trivia_rejects_unknown_entry() {
        let err = compile_err(json!([
            "grammar",
            "g",
            "root",
            [["rule", "root", ["lit", "x"]]],
            ["trivia", ["block_comments", "/*", "*/"]]
        ]));
        assert!(matches!(err, SpecCompileError::InvalidFormat(_)));
    }

    #[test]
    fn compile_duplicate_rule_errors() {
        let err = compile_err(json!([
            "grammar",
            "g",
            "root",
            [
                ["rule", "root", ["lit", "x"]],
                ["rule", "root", ["lit", "y"]]
            ]
        ]));
        assert!(matches!(err, SpecCompileError::DuplicateRule(_)));
    }

    #[test]
    fn compile_rule_with_metadata() {
        let g = compile(json!([
            "grammar",
            "g",
            "root",
            [[
                "rule",
                "root",
                ["lit", "x"],
                ["metadata", "return_type", "Expr"]
            ]]
        ]));
        let meta = g.metadata.get("root").expect("rule meta");
        assert_eq!(
            meta.get("return_type").and_then(|v| v.as_str()),
            Some("Expr")
        );
    }

    #[test]
    fn compile_rule_with_memo() {
        let g = compile(json!([
            "grammar",
            "g",
            "root",
            [["rule", "root", ["lit", "x"], ["memo"]]]
        ]));
        let meta = g.metadata.get("root");
        if let Some(m) = meta {
            assert_eq!(m.get("memo").and_then(|v| v.as_bool()), Some(true));
        }
    }

    #[test]
    fn compile_rule_with_memo_disabled() {
        let g = compile(json!([
            "grammar",
            "g",
            "root",
            [["rule", "root", ["lit", "x"], ["memo", false]]]
        ]));
        let meta = g.metadata.get("root").expect("rule meta");
        assert_eq!(meta.get("memo").and_then(|v| v.as_bool()), Some(false));
    }

    #[test]
    fn compile_rule_memo_rejects_coerced_values() {
        for metadata in [json!(["memo", "false"]), json!(["memo", 0])] {
            let err = compile_err(json!([
                "grammar",
                "g",
                "root",
                [["rule", "root", ["lit", "x"], metadata]]
            ]));
            assert!(
                matches!(
                    err,
                    SpecCompileError::TypeError {
                        expected: "bool",
                        ..
                    }
                ),
                "expected bool TypeError, got {err:?}"
            );
        }
    }

    #[test]
    fn compile_rule_metadata_rejects_string_shorthands() {
        for metadata in ["memo", "no_memo", "no-memo", ":memo", ":no_memo"] {
            let err = compile_err(json!([
                "grammar",
                "g",
                "root",
                [["rule", "root", ["lit", "x"], metadata]]
            ]));
            assert!(matches!(
                err,
                SpecCompileError::TypeError {
                    expected: "metadata list",
                    ctx,
                    ..
                } if ctx == "rule metadata"
            ));
        }
    }

    #[test]
    fn compile_rule_metadata_rejects_malformed_entries() {
        for metadata in [
            json!("unknown_meta"),
            json!([]),
            json!({ "metadata": true }),
            json!(["metadata", "key"]),
            json!(["metadata", "key", "value", "extra"]),
            json!(["type"]),
            json!(["type", "Expr", "extra"]),
            json!(["memo", true, false]),
            json!([123, "value"]),
            json!(["unknown", "value"]),
        ] {
            let err = compile_err(json!([
                "grammar",
                "g",
                "root",
                [["rule", "root", ["lit", "x"], metadata]]
            ]));
            assert!(
                matches!(
                    err,
                    SpecCompileError::InvalidFormat(_)
                        | SpecCompileError::TypeError { .. }
                        | SpecCompileError::UnknownTag(_)
                ),
                "expected metadata contract error, got {err:?}"
            );
        }
    }

    #[test]
    fn compile_empty_seq_and_choice() {
        // Edge cases that should not panic
        let src_seq = expr_to_source(&json!(["seq", []])).unwrap();
        assert_eq!(src_seq, "");
        let src_choice = expr_to_source(&json!(["choice", []])).unwrap();
        assert_eq!(src_choice, "");
    }

    #[test]
    fn compile_nested_quantifiers() {
        // opt(star(item)) = (item*)? written as item*?
        let src = expr_to_source(&json!(["opt", ["star", ["ref", "item"]]])).unwrap();
        assert_eq!(src, "item*?");
        // opt(plus(item)) = (item+)?  written as item+?
        let src2 = expr_to_source(&json!(["opt", ["plus", ["ref", "item"]]])).unwrap();
        assert_eq!(src2, "item+?");
    }

    #[test]
    fn compile_indentation_flag_stored_in_meta() {
        let g = compile(json!([
            "grammar",
            "g",
            "root",
            [["rule", "root", ["lit", "x"]]],
            ["indentation", true]
        ]));
        let gmeta = g.metadata.get("__grammar__").expect("grammar meta");
        assert_eq!(
            gmeta.get("indentation").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn compile_tok_expr_generates_proper_syntax() {
        let src = expr_to_source(&json!(["tok", "NAME"])).unwrap();
        assert_eq!(src, "tok(NAME)");
        let src2 = expr_to_source(&json!(["tok", "NAME", "hello"])).unwrap();
        assert_eq!(src2, "tok(NAME,'hello')");
    }

    #[test]
    fn mapping_tok_expr_preserves_text_only_and_any_token_refs() {
        use crate::spec_compiler_exprs::expr_to_peg_expr;
        let src = expr_to_source(&json!({"type": "tok", "text": "hello"})).unwrap();
        assert_eq!(src, "tok('hello')");
        let expr = expr_to_peg_expr(&json!({"type": "tok", "text": "hello"})).unwrap();
        assert_eq!(
            expr,
            PegExpr::TokenRef {
                kind: None,
                text: Some("hello".to_string())
            }
        );

        let src = expr_to_source(&json!({"type": "tok"})).unwrap();
        assert_eq!(src, "tok()");
        let expr = expr_to_peg_expr(&json!({"type": "tok"})).unwrap();
        assert_eq!(
            expr,
            PegExpr::TokenRef {
                kind: None,
                text: None
            }
        );
    }

    #[test]
    fn expected_compiles_to_pegexpr_expected_in_both_spec_forms() {
        use crate::spec_compiler_exprs::expr_to_peg_expr;
        // Array form: ["expected", message, inner]. The source repr keeps the
        // inner expression (like `behavior`/`prec`); the PegExpr carries the
        // diagnostic message.
        let arr = json!(["expected", "'}' to close", ["lit", "}"]]);
        assert_eq!(expr_to_source(&arr).unwrap(), "'}'");
        assert_eq!(
            expr_to_peg_expr(&arr).unwrap(),
            PegExpr::Expected {
                message: "'}' to close".to_string(),
                expr: Box::new(PegExpr::Literal("}".to_string())),
            }
        );
        // Object form: {"type":"expected","message":..,"expr":..}.
        let obj = json!({"type": "expected", "message": "x", "expr": {"type": "lit", "text": "y"}});
        assert_eq!(
            expr_to_peg_expr(&obj).unwrap(),
            PegExpr::Expected {
                message: "x".to_string(),
                expr: Box::new(PegExpr::Literal("y".to_string())),
            }
        );
        // A whole grammar using `expected` compiles.
        let g = compile(json!([
            "grammar",
            "g",
            "forms",
            [["rule", "start", ["expected", "msg", ["lit", "a"]]]]
        ]));
        assert!(g.get_rule("start").is_some());
    }

    #[test]
    fn call_compiles_to_pegexpr_call_in_both_spec_forms() {
        use crate::spec_compiler_exprs::expr_to_peg_expr;
        // Array form: ["call", rule, args…]
        let arr = json!(["call", "wrapped", ["lit", "a"], ["param", "x"]]);
        assert_eq!(expr_to_source(&arr).unwrap(), "wrapped('a', $x)");
        assert_eq!(
            expr_to_peg_expr(&arr).unwrap(),
            PegExpr::Call {
                rule: "wrapped".to_string(),
                args: vec![
                    PegExpr::Literal("a".to_string()),
                    PegExpr::Parameter {
                        name: "x".to_string()
                    },
                ],
            }
        );
        // Object form.
        let obj = json!({"type": "call", "rule": "r", "args": [{"type": "param", "name": "p"}]});
        assert_eq!(
            expr_to_peg_expr(&obj).unwrap(),
            PegExpr::Call {
                rule: "r".to_string(),
                args: vec![PegExpr::Parameter {
                    name: "p".to_string()
                }],
            }
        );
        // A grammar with a parametric rule (header carries the param) compiles,
        // and `call`/`param` survive into the rule body.
        let g = compile(json!([
            "grammar",
            "g",
            "forms",
            [
                [
                    "rule",
                    ["wrapped", "x", "->"],
                    ["seq", [["lit", "("], ["param", "x"], ["lit", ")"]]]
                ],
                ["rule", "start", ["call", "wrapped", ["regex", "[a-z]+"]]]
            ]
        ]));
        assert!(
            matches!(g.get_rule("start").unwrap().expr(), PegExpr::Call { rule, .. } if rule == "wrapped")
        );
    }

    #[test]
    fn tok_expr_rejects_malformed_kind_and_text_fields() {
        use crate::spec_compiler_exprs::expr_to_peg_expr;
        let err = expr_to_source(&json!(["tok", ""])).unwrap_err();
        assert!(
            matches!(&err, SpecCompileError::InvalidFormat(message) if message.contains("tok kind")),
            "expected invalid tok kind, got {err:?}"
        );

        let err = expr_to_peg_expr(&json!(["tok", "NAME", ""])).unwrap_err();
        assert!(
            matches!(&err, SpecCompileError::InvalidFormat(message) if message.contains("tok text")),
            "expected invalid tok text, got {err:?}"
        );

        let err = expr_to_source(&json!({"type": "tok", "kind": 1})).unwrap_err();
        assert!(
            matches!(&err, SpecCompileError::TypeError { ctx, .. } if ctx == "tok kind"),
            "expected tok kind type error, got {err:?}"
        );

        let err = expr_to_peg_expr(&json!({"type": "tok", "text": ""})).unwrap_err();
        assert!(
            matches!(&err, SpecCompileError::InvalidFormat(message) if message.contains("tok text")),
            "expected invalid tok text, got {err:?}"
        );
    }

    #[test]
    fn compile_grammar_metadata_top_level() {
        let g = compile(json!([
            "grammar",
            "g",
            "root",
            [["rule", "root", ["lit", "x"]]],
            ["grammar_metadata", "custom_key", 42]
        ]));
        let gmeta = g.metadata.get("__grammar__").expect("grammar meta");
        assert_eq!(gmeta.get("custom_key").and_then(|v| v.as_i64()), Some(42));
    }

    // ── Mapping-format expr_to_source ────────────────────────────────────

    #[test]
    fn mapping_literal() {
        let src = expr_to_source(&json!({"type": "literal", "text": "hello"})).unwrap();
        assert_eq!(src, "'hello'");
    }

    #[test]
    fn mapping_regex() {
        let src = expr_to_source(&json!({"type": "regex", "pattern": "[a-z]+"})).unwrap();
        assert_eq!(src, "/[a-z]+/");
    }

    #[test]
    fn mapping_ref() {
        let src = expr_to_source(&json!({"type": "ref", "name": "expr"})).unwrap();
        assert_eq!(src, "expr");
    }

    #[test]
    fn mapping_seq() {
        let src = expr_to_source(&json!({
            "type": "seq",
            "parts": [{"type": "literal", "text": "a"}, {"type": "ref", "name": "b"}]
        }))
        .unwrap();
        assert_eq!(src, "'a' b");
    }

    #[test]
    fn mapping_choice() {
        let src = expr_to_source(&json!({
            "type": "choice",
            "options": [{"type": "literal", "text": "a"}, {"type": "literal", "text": "b"}]
        }))
        .unwrap();
        assert_eq!(src, "'a' / 'b'");
    }

    #[test]
    fn mapping_star() {
        let src =
            expr_to_source(&json!({"type": "star", "expr": {"type": "ref", "name": "x"}})).unwrap();
        assert_eq!(src, "(x)*");
    }

    #[test]
    fn mapping_plus() {
        let src =
            expr_to_source(&json!({"type": "plus", "expr": {"type": "ref", "name": "x"}})).unwrap();
        assert_eq!(src, "(x)+");
    }

    #[test]
    fn mapping_optional() {
        let src =
            expr_to_source(&json!({"type": "optional", "expr": {"type": "ref", "name": "x"}}))
                .unwrap();
        assert_eq!(src, "(x)?");
    }

    #[test]
    fn mapping_named() {
        let src = expr_to_source(&json!({
            "type": "named",
            "name": "lhs",
            "expr": {"type": "ref", "name": "expr"}
        }))
        .unwrap();
        assert_eq!(src, "lhs:(expr)");
    }

    #[test]
    fn mapping_cut() {
        let src = expr_to_source(&json!({"type": "cut"})).unwrap();
        assert_eq!(src, "~");
    }

    #[test]
    fn mapping_sep_plus() {
        let src = expr_to_source(&json!({
            "type": "sep_plus",
            "sep": {"type": "literal", "text": ","},
            "expr": {"type": "ref", "name": "item"}
        }))
        .unwrap();
        assert!(src.contains("item") && src.contains("','"));
    }

    #[test]
    fn mapping_no_trivia() {
        let src =
            expr_to_source(&json!({"type": "tight", "expr": {"type": "literal", "text": "x"}}))
                .unwrap();
        assert_eq!(src, "tight('x')");
    }

    #[test]
    fn mapping_island_requires_explicit_delimiters() {
        use crate::spec_compiler_exprs::expr_to_peg_expr;
        let err = expr_to_source(&json!({"type": "island"})).unwrap_err();
        assert!(matches!(err, SpecCompileError::MissingField(field) if field == "island start"));
        let err = expr_to_peg_expr(&json!({"type": "island", "start": "<"})).unwrap_err();
        assert!(matches!(err, SpecCompileError::MissingField(field) if field == "island end"));
    }

    #[test]
    fn mapping_island_options_are_strict() {
        use crate::spec_compiler_exprs::expr_to_peg_expr;
        let src = expr_to_source(&json!({
            "type": "island",
            "start": "<",
            "end": ">",
            "include_delims": true
        }))
        .unwrap();
        assert_eq!(src, "island(\"<\", \">\", true)");

        let expr = expr_to_peg_expr(&json!(["island", "<", ">", true])).unwrap();
        assert!(matches!(
            expr,
            PegExpr::Island {
                include_delims: true,
                ..
            }
        ));

        let err = expr_to_source(&json!({
            "type": "island",
            "start": "<",
            "end": ">",
            "include_delims": "yes"
        }))
        .unwrap_err();
        assert!(matches!(
            err,
            SpecCompileError::TypeError {
                expected: "bool",
                ctx,
                ..
            } if ctx == "island include_delims"
        ));

        let err = expr_to_peg_expr(&json!(["island", "<", ">", 1])).unwrap_err();
        assert!(matches!(
            err,
            SpecCompileError::TypeError {
                expected: "bool",
                ctx,
                ..
            } if ctx == "island include_delims"
        ));
    }

    #[test]
    fn mapping_raw_block_requires_explicit_delimiters() {
        use crate::spec_compiler_exprs::expr_to_peg_expr;
        let err = expr_to_source(&json!({"type": "raw_block"})).unwrap_err();
        assert!(matches!(err, SpecCompileError::MissingField(field) if field == "raw_block start"));
        let err = expr_to_peg_expr(&json!({"type": "raw_block", "start": "{"})).unwrap_err();
        assert!(matches!(err, SpecCompileError::MissingField(field) if field == "raw_block end"));
    }

    #[test]
    fn mapping_raw_block_options_are_strict() {
        use crate::spec_compiler_exprs::expr_to_peg_expr;
        let src = expr_to_source(&json!({
            "type": "raw_block",
            "start": "{",
            "end": "}",
            "delim_kind": "brace"
        }))
        .unwrap();
        assert_eq!(src, "raw_block(\"{\", \"}\", \"brace\")");

        let expr = expr_to_peg_expr(&json!(["raw_block", "{", "}"])).unwrap();
        assert!(matches!(
            expr,
            PegExpr::RawBlock {
                ref delim_kind,
                ..
            } if delim_kind == "block"
        ));

        let expr = expr_to_peg_expr(&json!(["raw_block", "{", "}", "brace"])).unwrap();
        assert!(matches!(
            expr,
            PegExpr::RawBlock {
                ref delim_kind,
                ..
            } if delim_kind == "brace"
        ));

        let err = expr_to_source(&json!({
            "type": "raw_block",
            "start": "{",
            "end": "}",
            "delim_kind": false
        }))
        .unwrap_err();
        assert!(matches!(
            err,
            SpecCompileError::TypeError {
                expected: "string",
                ctx,
                ..
            } if ctx == "raw_block delim_kind"
        ));

        let err = expr_to_peg_expr(&json!(["raw_block", "{", "}", false])).unwrap_err();
        assert!(matches!(
            err,
            SpecCompileError::TypeError {
                expected: "string",
                ctx,
                ..
            } if ctx == "raw_block delim_kind"
        ));
    }

    #[test]
    fn mapping_expr_rejects_legacy_object_alias_fields() {
        let err =
            expr_to_source(&json!({"type": "seq", "items": [{"type": "literal", "text": "x"}]}))
                .unwrap_err();
        assert!(matches!(err, SpecCompileError::MissingField(field) if field == "parts"));

        let err = expr_to_source(&json!({"type": "star", "body": {"type": "ref", "name": "x"}}))
            .unwrap_err();
        assert!(matches!(err, SpecCompileError::MissingField(field) if field == "expr"));

        let err = expr_to_source(&json!({
            "type": "sep_plus",
            "separator": {"type": "literal", "text": ","},
            "body": {"type": "ref", "name": "item"}
        }))
        .unwrap_err();
        assert!(matches!(err, SpecCompileError::MissingField(field) if field == "sep"));
    }
}
