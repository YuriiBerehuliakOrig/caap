/// CTFE builtins for grammar construction, extension, and parsing.
///
/// Exposed primitives:
///   ctfe-grammar-new        src          → grammar-obj
///   ctfe-grammar-set-start  grammar name → grammar-obj (new clone)
///   ctfe-grammar-extend     grammar list → grammar-obj (new clone)
///   ctfe-grammar-describe   grammar      → structural grammar map
///   ctfe-grammar-rule-get   grammar name [default] → rule map/default/null
///   ctfe-grammar-analyze    grammar      → static analysis map
///   ctfe-grammar-conflicts  grammar      → focused conflict report map
///   ctfe-grammar-parse      text grammar [options] [semantics] → {"ok" bool ...}
///   ctfe-grammar-parse-tokens text grammar tokens [options] [semantics] → {"ok" bool ...}
///   ctfe-lex-token          kind text start end → token map
///   ctfe-lexer-tokenize     text specs          → token maps
use indexmap::IndexMap;
use std::{any::Any, cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};

use caap_peg::{
    analyze_grammar, Directive, Grammar, GrammarAnalysis, LexToken, MemoPolicy, ParseDriver,
    ParseEffect, ParseValue, ParseView, ParserConfig,
};
use regex::Regex;

use crate::{
    eval::{eval_args, Evaluator},
    values::{eval_err, EvalSignal, MapKey, RuntimeValue},
};

use super::args::{require_bool, require_string, require_usize};

mod engine;

// ── GrammarValue ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct GrammarValue {
    pub grammar: Grammar,
}

impl crate::values::HostObject for GrammarValue {
    fn type_name(&self) -> &'static str {
        "grammar"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ── ParseCacheValue ──────────────────────────────────────────────────────────

/// A reusable, mutable [`caap_peg::ParseCache`] handed to CAAP scripts as a host
/// object. `ctfe_grammar_parse_incremental` borrows it mutably to reuse work
/// across edits, so it is wrapped in a `RefCell` (the cache survives between
/// builtin calls via the script-held handle).
#[derive(Debug)]
pub struct ParseCacheValue {
    pub cache: RefCell<caap_peg::ParseCache>,
}

impl crate::values::HostObject for ParseCacheValue {
    fn type_name(&self) -> &'static str {
        "parse_cache"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Downcast a `RuntimeValue::HostObject` to a concrete host type `T`, erroring
/// with `"{context}: expected {what} object"` when the value is not a host
/// object or not a `T`. The single source of truth for the host-object downcast
/// boilerplate (grammar / parse-cache / ast-node / registry / peg-expr / builder).
pub(super) fn downcast_host_object<'a, T: std::any::Any>(
    value: &'a RuntimeValue,
    context: &str,
    what: &str,
) -> Result<&'a T, EvalSignal> {
    let RuntimeValue::HostObject(obj) = value else {
        return Err(eval_err(format!("{context}: expected {what} object")));
    };
    obj.as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| eval_err(format!("{context}: expected {what} object")))
}

fn downcast_parse_cache<'a>(
    value: &'a RuntimeValue,
    context: &str,
) -> Result<&'a ParseCacheValue, EvalSignal> {
    downcast_host_object(value, context, "parse-cache")
}

// ── AstNodeValue ─────────────────────────────────────────────────────────────

/// A concrete-syntax-tree node produced by `ctfe-grammar-parse-ast*`, kept as a
/// host object (with the source text it was parsed from) so it can feed
/// `ctfe-ast-changed-ranges` / `ctfe-ast-reparse-incremental`. Project it to an
/// inspectable map with `ctfe-ast-to-map`.
#[derive(Debug)]
pub struct AstNodeValue {
    pub node: caap_peg::AstNode,
    pub text: String,
}

impl crate::values::HostObject for AstNodeValue {
    fn type_name(&self) -> &'static str {
        "ast_node"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn downcast_ast_node<'a>(
    value: &'a RuntimeValue,
    context: &str,
) -> Result<&'a AstNodeValue, EvalSignal> {
    downcast_host_object(value, context, "ast-node")
}

fn ast_node_host_obj(node: caap_peg::AstNode, text: String) -> RuntimeValue {
    RuntimeValue::HostObject(Rc::new(AstNodeValue { node, text }))
}

// ── GrammarRegistryValue ─────────────────────────────────────────────────────

/// A namespaced cross-grammar import registry handed to CAAP as a host object.
/// `register` mutates it, so it is wrapped in a `RefCell`; attach it to a parse
/// with `ctfe-grammar-parse-with-registry` to resolve `name::rule` references.
#[derive(Debug)]
pub struct GrammarRegistryValue {
    pub registry: RefCell<caap_peg::GrammarRegistry>,
}

impl crate::values::HostObject for GrammarRegistryValue {
    fn type_name(&self) -> &'static str {
        "grammar_registry"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn downcast_grammar_registry<'a>(
    value: &'a RuntimeValue,
    context: &str,
) -> Result<&'a GrammarRegistryValue, EvalSignal> {
    downcast_host_object(value, context, "grammar-registry")
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn downcast_grammar<'a>(
    value: &'a RuntimeValue,
    context: &str,
) -> Result<&'a GrammarValue, EvalSignal> {
    downcast_host_object(value, context, "grammar")
}

pub(super) fn grammar_from_runtime_value(
    value: &RuntimeValue,
    context: &str,
) -> Result<Grammar, EvalSignal> {
    downcast_grammar(value, context).map(|value| value.grammar.clone())
}

pub(super) fn grammar_host_obj(grammar: Grammar) -> RuntimeValue {
    RuntimeValue::HostObject(Rc::new(GrammarValue { grammar }))
}

fn str_value(value: impl AsRef<str>) -> RuntimeValue {
    RuntimeValue::Str(Rc::from(value.as_ref()))
}

fn usize_value(value: usize, context: &str) -> Result<RuntimeValue, EvalSignal> {
    i64::try_from(value)
        .map(RuntimeValue::Int)
        .map_err(|_| eval_err(format!("{context}: value exceeds CAAP integer range")))
}

fn u64_value(value: u64, context: &str) -> Result<RuntimeValue, EvalSignal> {
    i64::try_from(value)
        .map(RuntimeValue::Int)
        .map_err(|_| eval_err(format!("{context}: value exceeds CAAP integer range")))
}

fn optional_bool_field(
    fields: &IndexMap<MapKey, RuntimeValue>,
    key: &str,
    context: &str,
) -> Result<Option<bool>, EvalSignal> {
    match fields.get(&MapKey::Str(Rc::from(key))) {
        Some(value) => require_bool(value, &format!("{context}: '{key}' must be a bool")).map(Some),
        None => Ok(None),
    }
}

fn optional_usize_field(
    fields: &IndexMap<MapKey, RuntimeValue>,
    key: &str,
    context: &str,
) -> Result<Option<usize>, EvalSignal> {
    match fields.get(&MapKey::Str(Rc::from(key))) {
        Some(RuntimeValue::Null) | None => Ok(None),
        Some(value) => require_usize(
            value,
            &format!("{context}: '{key}' must be a non-negative integer"),
        )
        .map(Some),
    }
}

fn list_value(items: Vec<RuntimeValue>) -> RuntimeValue {
    RuntimeValue::List(Rc::new(RefCell::new(items)))
}

fn map_value(entries: Vec<(&'static str, RuntimeValue)>) -> RuntimeValue {
    RuntimeValue::Map(Rc::new(RefCell::new(
        entries
            .into_iter()
            .map(|(key, value)| (MapKey::Str(Rc::from(key)), value))
            .collect(),
    )))
}

fn map_field_value(value: &RuntimeValue, key: &str) -> Option<RuntimeValue> {
    let RuntimeValue::Map(map) = value else {
        return None;
    };
    map.borrow().get(&MapKey::Str(Rc::from(key))).cloned()
}

fn require_map_field(
    value: &RuntimeValue,
    key: &str,
    context: &str,
) -> Result<RuntimeValue, EvalSignal> {
    map_field_value(value, key).ok_or_else(|| eval_err(format!("{context}: missing '{key}'")))
}

fn string_list(values: &[String]) -> RuntimeValue {
    list_value(values.iter().map(str_value).collect())
}

fn string_pair_list(
    values: &[(String, String)],
    left_key: &'static str,
    right_key: &'static str,
) -> RuntimeValue {
    list_value(
        values
            .iter()
            .map(|(left, right)| {
                map_value(vec![
                    (left_key, str_value(left)),
                    (right_key, str_value(right)),
                ])
            })
            .collect(),
    )
}

fn json_to_runtime(value: &serde_json::Value) -> Result<RuntimeValue, EvalSignal> {
    match value {
        serde_json::Value::Null => Ok(RuntimeValue::Null),
        serde_json::Value::Bool(value) => Ok(RuntimeValue::Bool(*value)),
        serde_json::Value::Number(value) => {
            if let Some(int) = value.as_i64() {
                Ok(RuntimeValue::Int(int))
            } else if let Some(float) = value.as_f64() {
                Ok(RuntimeValue::Float(float))
            } else {
                Err(eval_err("ctfe_grammar_describe: unsupported JSON number"))
            }
        }
        serde_json::Value::String(value) => Ok(str_value(value)),
        serde_json::Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(json_to_runtime(item)?);
            }
            Ok(list_value(out))
        }
        serde_json::Value::Object(object) => {
            let mut entries: Vec<_> = object.iter().collect();
            entries.sort_by_key(|(key, _)| *key);
            let mut out = IndexMap::with_capacity(entries.len());
            for (key, value) in entries {
                out.insert(MapKey::Str(Rc::from(key.as_str())), json_to_runtime(value)?);
            }
            Ok(RuntimeValue::Map(Rc::new(RefCell::new(out))))
        }
    }
}

fn lex_token_to_runtime(token: &LexToken) -> Result<RuntimeValue, EvalSignal> {
    Ok(map_value(vec![
        ("kind", str_value(&token.kind)),
        ("text", str_value(&token.text)),
        ("start", usize_value(token.start, "ctfe_lex_token")?),
        ("end", usize_value(token.end, "ctfe_lex_token")?),
    ]))
}

fn lex_token_from_runtime(value: &RuntimeValue, context: &str) -> Result<LexToken, EvalSignal> {
    let kind = require_string(
        &require_map_field(value, "kind", context)?,
        &format!("{context}: token kind must be a string"),
    )?;
    if kind.is_empty() {
        return Err(eval_err(format!("{context}: token kind must be non-empty")));
    }
    let text = require_string(
        &require_map_field(value, "text", context)?,
        &format!("{context}: token text must be a string"),
    )?;
    let start = require_usize(
        &require_map_field(value, "start", context)?,
        &format!("{context}: token start must be a non-negative integer"),
    )?;
    let end = require_usize(
        &require_map_field(value, "end", context)?,
        &format!("{context}: token end must be a non-negative integer"),
    )?;
    Ok(LexToken::new(kind, text, start, end))
}

fn lex_tokens_from_runtime(
    value: &RuntimeValue,
    context: &str,
) -> Result<Vec<LexToken>, EvalSignal> {
    let RuntimeValue::List(items) = value else {
        return Err(eval_err(format!("{context}: tokens must be a list")));
    };
    let items = items.borrow().clone();
    let mut tokens = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        tokens.push(lex_token_from_runtime(
            item,
            &format!("{context}: token[{index}]"),
        )?);
    }
    Ok(tokens)
}

struct LexerSpec {
    kind: String,
    regex: Regex,
    skip: bool,
}

fn lexer_specs_from_runtime(value: &RuntimeValue) -> Result<Vec<LexerSpec>, EvalSignal> {
    let RuntimeValue::List(items) = value else {
        return Err(eval_err("ctfe_lexer_tokenize: specs must be a list"));
    };
    let items = items.borrow().clone();
    let mut specs = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let context = format!("ctfe_lexer_tokenize: spec[{index}]");
        let kind = require_string(
            &require_map_field(item, "kind", &context)?,
            &format!("{context}: kind must be a string"),
        )?;
        if kind.is_empty() {
            return Err(eval_err(format!("{context}: kind must be non-empty")));
        }
        let pattern = require_string(
            &require_map_field(item, "pattern", &context)?,
            &format!("{context}: pattern must be a string"),
        )?;
        if pattern.is_empty() {
            return Err(eval_err(format!("{context}: pattern must be non-empty")));
        }
        let skip = match map_field_value(item, "skip") {
            Some(value) => require_bool(&value, &format!("{context}: skip must be a bool"))?,
            None => false,
        };
        let regex = Regex::new(&format!("^(?:{pattern})"))
            .map_err(|error| eval_err(format!("{context}: invalid regex pattern: {error}")))?;
        specs.push(LexerSpec { kind, regex, skip });
    }
    if specs.is_empty() {
        return Err(eval_err(
            "ctfe_lexer_tokenize: at least one token spec is required",
        ));
    }
    Ok(specs)
}

fn tokenize_with_specs(text: &str, specs: &[LexerSpec]) -> Result<Vec<LexToken>, EvalSignal> {
    let mut tokens = Vec::new();
    let mut pos = 0;
    while pos < text.len() {
        let remaining = &text[pos..];
        let mut best: Option<(usize, usize)> = None;
        for (index, spec) in specs.iter().enumerate() {
            let Some(matched) = spec.regex.find(remaining) else {
                continue;
            };
            if matched.start() != 0 {
                continue;
            }
            let len = matched.end();
            if len == 0 {
                return Err(eval_err(format!(
                    "ctfe_lexer_tokenize: spec[{index}] matched empty text at byte {pos}"
                )));
            }
            if match best {
                Some((best_len, best_index)) => {
                    len > best_len || (len == best_len && index < best_index)
                }
                None => true,
            } {
                best = Some((len, index));
            }
        }
        let Some((len, spec_index)) = best else {
            return Err(eval_err(format!(
                "ctfe_lexer_tokenize: no token spec matched at byte {pos}"
            )));
        };
        let spec = &specs[spec_index];
        let end = pos + len;
        let text_slice = &text[pos..end];
        if !spec.skip {
            tokens.push(LexToken::new(&spec.kind, text_slice, pos, end));
        }
        pos = end;
    }
    Ok(tokens)
}

fn grammar_summary_map(alias: Option<&str>, grammar: &Grammar) -> Result<RuntimeValue, EvalSignal> {
    let mut entries = Vec::from([
        ("start_rule", str_value(&grammar.start_rule)),
        (
            "rule_count",
            usize_value(grammar.rules.len(), "ctfe_grammar_describe")?,
        ),
        (
            "version",
            u64_value(grammar.version, "ctfe_grammar_describe")?,
        ),
        ("sealed", RuntimeValue::Bool(grammar.state.sealed)),
    ]);
    if let Some(alias) = alias {
        entries.push(("alias", str_value(alias)));
    }
    Ok(map_value(entries))
}

fn grammar_description(grammar: &Grammar) -> Result<RuntimeValue, EvalSignal> {
    let mut imports: Vec<_> = grammar.imports.iter().collect();
    imports.sort_by_key(|(alias, _)| *alias);
    let imports = imports
        .into_iter()
        .map(|(alias, imported)| grammar_summary_map(Some(alias), imported))
        .collect::<Result<Vec<_>, _>>()?;

    let rules = grammar
        .rules
        .iter()
        .map(|rule| {
            Ok(map_value(vec![
                ("name", str_value(&rule.name)),
                ("source", str_value(&rule.source)),
                ("params", string_list(&rule.params)),
            ]))
        })
        .collect::<Result<Vec<_>, EvalSignal>>()?;

    let mut metadata_entries = Vec::new();
    let mut scopes: Vec<_> = grammar.metadata.iter().collect();
    scopes.sort_by_key(|(scope, _)| *scope);
    for (scope, fields) in scopes {
        let mut fields: Vec<_> = fields.iter().collect();
        fields.sort_by_key(|(key, _)| *key);
        for (key, value) in fields {
            metadata_entries.push(map_value(vec![
                ("scope", str_value(scope)),
                ("key", str_value(key)),
                ("value", json_to_runtime(value)?),
            ]));
        }
    }

    Ok(map_value(vec![
        ("start_rule", str_value(&grammar.start_rule)),
        (
            "rule_count",
            usize_value(grammar.rules.len(), "ctfe_grammar_describe")?,
        ),
        (
            "version",
            u64_value(grammar.version, "ctfe_grammar_describe")?,
        ),
        ("sealed", RuntimeValue::Bool(grammar.state.sealed)),
        ("source", str_value(&grammar.text)),
        ("rules", list_value(rules)),
        ("imports", list_value(imports)),
        ("metadata", list_value(metadata_entries)),
    ]))
}

fn grammar_rule_map(
    rule: &caap_peg::GrammarRule,
    index: usize,
    context: &str,
) -> Result<RuntimeValue, EvalSignal> {
    Ok(map_value(vec![
        ("name", str_value(&rule.name)),
        ("source", str_value(&rule.source)),
        ("params", string_list(&rule.params)),
        ("index", usize_value(index, context)?),
    ]))
}

fn parse_config_from_runtime(
    value: Option<&RuntimeValue>,
    context: &str,
) -> Result<ParserConfig, EvalSignal> {
    let mut config = ParserConfig::default();
    let Some(value) = value else {
        return Ok(config);
    };
    if matches!(value, RuntimeValue::Null) {
        return Ok(config);
    }
    let RuntimeValue::Map(fields) = value else {
        return Err(eval_err(format!("{context}: parse options must be a map")));
    };
    let fields = fields.borrow();
    if let Some(return_spans) = optional_bool_field(&fields, "return_spans", context)? {
        config.return_spans = return_spans;
    }
    if let Some(memo) = optional_bool_field(&fields, "memo", context)? {
        config.memo = memo;
    }
    if let Some(max_steps) = optional_usize_field(&fields, "max_steps", context)? {
        config.max_steps = max_steps;
    }
    if let Some(RuntimeValue::Map(policy_fields)) =
        fields.get(&MapKey::Str(Rc::from("memo_policy")))
    {
        let policy_fields = policy_fields.borrow();
        let global_budget = optional_usize_field(&policy_fields, "global_budget", context)?;
        config.memo_policy = Some(
            MemoPolicy::new(global_budget)
                .map_err(|error| eval_err(format!("{context}: invalid memo_policy: {error}")))?,
        );
    } else if fields.contains_key(&MapKey::Str(Rc::from("memo_policy"))) {
        return Err(eval_err(format!("{context}: 'memo_policy' must be a map")));
    }
    Ok(config)
}

fn parse_options_and_semantics(
    args: &[RuntimeValue],
    first_optional: usize,
    context: &str,
) -> Result<(ParserConfig, Option<RuntimeValue>), EvalSignal> {
    let options = args.get(first_optional);
    let semantics = args
        .get(first_optional + 1)
        .cloned()
        .or_else(|| options.filter(|value| has_semantic_hooks(value)).cloned());
    Ok((parse_config_from_runtime(options, context)?, semantics))
}

fn grammar_analysis_map(analysis: &GrammarAnalysis) -> Result<RuntimeValue, EvalSignal> {
    let mut refs: Vec<_> = analysis.refs.iter().collect();
    refs.sort_by_key(|(rule, _)| *rule);
    let refs = refs
        .into_iter()
        .map(|(rule, refs)| map_value(vec![("rule", str_value(rule)), ("refs", string_list(refs))]))
        .collect();

    let param_arity_mismatches = analysis
        .param_arity_mismatches
        .iter()
        .map(|mismatch| {
            Ok(map_value(vec![
                ("caller", str_value(&mismatch.caller)),
                ("callee", str_value(&mismatch.callee)),
                (
                    "expected",
                    usize_value(mismatch.expected, "ctfe_grammar_analyze")?,
                ),
                ("got", usize_value(mismatch.got, "ctfe_grammar_analyze")?),
            ]))
        })
        .collect::<Result<Vec<_>, EvalSignal>>()?;

    let dead_choice_alternatives = analysis
        .dead_choice_alternatives
        .iter()
        .map(|(rule, dead, live)| {
            Ok(map_value(vec![
                ("rule", str_value(rule)),
                (
                    "dead_alt_index",
                    usize_value(*dead, "ctfe_grammar_analyze")?,
                ),
                (
                    "live_alt_index",
                    usize_value(*live, "ctfe_grammar_analyze")?,
                ),
            ]))
        })
        .collect::<Result<Vec<_>, EvalSignal>>()?;

    let prefix_shadowed_choice_alternatives = analysis
        .prefix_shadowed_choice_alternatives
        .iter()
        .map(|(rule, dead, live, prefix)| {
            Ok(map_value(vec![
                ("rule", str_value(rule)),
                (
                    "dead_alt_index",
                    usize_value(*dead, "ctfe_grammar_analyze")?,
                ),
                (
                    "live_alt_index",
                    usize_value(*live, "ctfe_grammar_analyze")?,
                ),
                ("prefix", str_value(prefix)),
            ]))
        })
        .collect::<Result<Vec<_>, EvalSignal>>()?;

    let overlapping_prefixes = analysis
        .overlapping_prefixes
        .iter()
        .map(|(rule, alt1, alt2, prefix)| {
            Ok(map_value(vec![
                ("rule", str_value(rule)),
                ("alt1_index", usize_value(*alt1, "ctfe_grammar_analyze")?),
                ("alt2_index", usize_value(*alt2, "ctfe_grammar_analyze")?),
                ("common_prefix", str_value(prefix)),
            ]))
        })
        .collect::<Result<Vec<_>, EvalSignal>>()?;

    let left_recursive_sccs = analysis
        .left_recursive_sccs
        .iter()
        .map(|component| string_list(component))
        .collect();

    Ok(map_value(vec![
        (
            "rule_count",
            usize_value(analysis.rule_count, "ctfe_grammar_analyze")?,
        ),
        (
            "has_start_rule",
            RuntimeValue::Bool(analysis.has_start_rule),
        ),
        (
            "has_duplicate_rule_names",
            RuntimeValue::Bool(analysis.has_duplicate_rule_names),
        ),
        ("refs", list_value(refs)),
        ("reachable", string_list(&analysis.reachable)),
        ("unreachable", string_list(&analysis.unreachable)),
        (
            "missing_refs",
            string_pair_list(&analysis.missing_refs, "rule", "target"),
        ),
        ("left_recursive", string_list(&analysis.left_recursive)),
        ("duplicates", string_list(&analysis.duplicates)),
        ("param_arity_mismatches", list_value(param_arity_mismatches)),
        (
            "undeclared_params",
            string_pair_list(&analysis.undeclared_params, "rule", "param"),
        ),
        (
            "unused_params",
            string_pair_list(&analysis.unused_params, "rule", "param"),
        ),
        (
            "non_choice_commits",
            string_pair_list(&analysis.non_choice_commits, "rule", "kind"),
        ),
        (
            "nullable_repetition",
            string_pair_list(&analysis.nullable_repetition, "rule", "kind"),
        ),
        (
            "dead_choice_alternatives",
            list_value(dead_choice_alternatives),
        ),
        (
            "prefix_shadowed_choice_alternatives",
            list_value(prefix_shadowed_choice_alternatives),
        ),
        ("overlapping_prefixes", list_value(overlapping_prefixes)),
        ("unproductive", string_list(&analysis.unproductive)),
        (
            "invalid_rules",
            string_pair_list(&analysis.invalid_rules, "rule", "message"),
        ),
        ("left_recursive_sccs", list_value(left_recursive_sccs)),
        ("warnings", string_list(&analysis.warnings)),
        ("errors", string_list(&analysis.errors)),
    ]))
}

fn conflict(
    severity: &'static str,
    kind: &'static str,
    entries: Vec<(&'static str, RuntimeValue)>,
) -> RuntimeValue {
    let mut all = Vec::from([("severity", str_value(severity)), ("kind", str_value(kind))]);
    all.extend(entries);
    map_value(all)
}

fn grammar_conflicts_map(analysis: &GrammarAnalysis) -> Result<RuntimeValue, EvalSignal> {
    let mut conflicts = Vec::new();

    if !analysis.has_start_rule {
        conflicts.push(conflict("error", "missing_start_rule", Vec::new()));
    }
    for rule in &analysis.duplicates {
        conflicts.push(conflict(
            "error",
            "duplicate_rule",
            vec![("rule", str_value(rule))],
        ));
    }
    for (rule, target) in &analysis.missing_refs {
        conflicts.push(conflict(
            "error",
            "missing_ref",
            vec![("rule", str_value(rule)), ("target", str_value(target))],
        ));
    }
    for mismatch in &analysis.param_arity_mismatches {
        conflicts.push(conflict(
            "error",
            "param_arity_mismatch",
            vec![
                ("caller", str_value(&mismatch.caller)),
                ("callee", str_value(&mismatch.callee)),
                (
                    "expected",
                    usize_value(mismatch.expected, "ctfe_grammar_conflicts")?,
                ),
                ("got", usize_value(mismatch.got, "ctfe_grammar_conflicts")?),
            ],
        ));
    }
    for (rule, param) in &analysis.undeclared_params {
        conflicts.push(conflict(
            "error",
            "undeclared_param",
            vec![("rule", str_value(rule)), ("param", str_value(param))],
        ));
    }
    for (rule, dead, live) in &analysis.dead_choice_alternatives {
        conflicts.push(conflict(
            "error",
            "dead_choice_alternative",
            vec![
                ("rule", str_value(rule)),
                (
                    "dead_alt_index",
                    usize_value(*dead, "ctfe_grammar_conflicts")?,
                ),
                (
                    "live_alt_index",
                    usize_value(*live, "ctfe_grammar_conflicts")?,
                ),
            ],
        ));
    }
    for (rule, dead, live, prefix) in &analysis.prefix_shadowed_choice_alternatives {
        conflicts.push(conflict(
            "error",
            "prefix_shadowed_choice_alternative",
            vec![
                ("rule", str_value(rule)),
                (
                    "dead_alt_index",
                    usize_value(*dead, "ctfe_grammar_conflicts")?,
                ),
                (
                    "live_alt_index",
                    usize_value(*live, "ctfe_grammar_conflicts")?,
                ),
                ("prefix", str_value(prefix)),
            ],
        ));
    }
    for rule in &analysis.unproductive {
        conflicts.push(conflict(
            "error",
            "unproductive_rule",
            vec![("rule", str_value(rule))],
        ));
    }
    for (rule, message) in &analysis.invalid_rules {
        conflicts.push(conflict(
            "error",
            "invalid_rule",
            vec![("rule", str_value(rule)), ("message", str_value(message))],
        ));
    }

    for rule in &analysis.left_recursive {
        conflicts.push(conflict(
            "warning",
            "left_recursive_rule",
            vec![("rule", str_value(rule))],
        ));
    }
    for rule in &analysis.unreachable {
        conflicts.push(conflict(
            "warning",
            "unreachable_rule",
            vec![("rule", str_value(rule))],
        ));
    }
    for (rule, param) in &analysis.unused_params {
        conflicts.push(conflict(
            "warning",
            "unused_param",
            vec![("rule", str_value(rule)), ("param", str_value(param))],
        ));
    }
    for (rule, kind) in &analysis.non_choice_commits {
        conflicts.push(conflict(
            "warning",
            "non_choice_commit",
            vec![("rule", str_value(rule)), ("commit", str_value(kind))],
        ));
    }
    for (rule, kind) in &analysis.nullable_repetition {
        conflicts.push(conflict(
            "warning",
            "nullable_repetition",
            vec![("rule", str_value(rule)), ("repeat", str_value(kind))],
        ));
    }
    for (rule, alt1, alt2, prefix) in &analysis.overlapping_prefixes {
        conflicts.push(conflict(
            "warning",
            "overlapping_prefix",
            vec![
                ("rule", str_value(rule)),
                ("alt1_index", usize_value(*alt1, "ctfe_grammar_conflicts")?),
                ("alt2_index", usize_value(*alt2, "ctfe_grammar_conflicts")?),
                ("common_prefix", str_value(prefix)),
            ],
        ));
    }

    Ok(map_value(vec![
        ("has_conflicts", RuntimeValue::Bool(!conflicts.is_empty())),
        (
            "conflict_count",
            usize_value(conflicts.len(), "ctfe_grammar_conflicts")?,
        ),
        ("conflicts", list_value(conflicts)),
        ("errors", string_list(&analysis.errors)),
        ("warnings", string_list(&analysis.warnings)),
    ]))
}

/// Convert a `ParseValue` tree into a `RuntimeValue` tree that CAAP code can
/// inspect with `get`, `length`, etc.
fn pv_to_rv(pv: ParseValue) -> RuntimeValue {
    match pv {
        ParseValue::Nil => RuntimeValue::Null,
        ParseValue::Text(s) => RuntimeValue::Str(s.as_ref().into()),
        ParseValue::Number(n) => RuntimeValue::Int(n),
        ParseValue::Node(name, children) => {
            let kids: Vec<RuntimeValue> = children.iter().cloned().map(pv_to_rv).collect();
            let map = Rc::new(RefCell::new(IndexMap::from([
                (
                    MapKey::Str(Rc::from("kind")),
                    RuntimeValue::Str(name.as_ref().into()),
                ),
                (
                    MapKey::Str(Rc::from("children")),
                    RuntimeValue::List(Rc::new(RefCell::new(kids))),
                ),
            ])));
            RuntimeValue::Map(map)
        }
        ParseValue::Named(name, inner) => {
            let map = Rc::new(RefCell::new(IndexMap::from([
                (
                    MapKey::Str(Rc::from("name")),
                    RuntimeValue::Str(name.as_ref().into()),
                ),
                (
                    MapKey::Str(Rc::from("value")),
                    pv_to_rv(ParseValue::unwrap_arc(inner)),
                ),
            ])));
            RuntimeValue::Map(map)
        }
        ParseValue::SpannedValue { value, .. } => pv_to_rv(ParseValue::unwrap_arc(value)),
    }
}

/// Convert a `RuntimeValue` returned by a CAAP semantic action back into a
/// `ParseValue`. This is the inverse of [`pv_to_rv`] for every shape that
/// function produces, so an action that returns its input (or a transformed
/// copy) round-trips faithfully:
///   `Null`→`Nil`, `Str`→`Text`, `Int`→`Number`,
///   `{"kind","children"}`→`Node`, `{"name","value"}`→`Named`.
/// Action-introduced values that have no parse-tree shape map sensibly:
///   `Bool`→`Text("true"/"false")`, `List`→`Node("list", …)`,
///   `Tuple`→`Node("tuple", …)`, integral `Float`→`Number` else `Text`.
/// Callables, host objects and other maps have no representation and map to
/// `Nil`.
fn rv_to_pv(value: RuntimeValue) -> ParseValue {
    match value {
        RuntimeValue::Null => ParseValue::Nil,
        RuntimeValue::Str(s) => ParseValue::Text(Arc::from(s.as_ref())),
        RuntimeValue::Int(n) => ParseValue::Number(n),
        RuntimeValue::Float(f) => {
            const I64_MIN_F64: f64 = -9_223_372_036_854_775_808.0;
            const I64_MAX_EXCLUSIVE_F64: f64 = 9_223_372_036_854_775_808.0;
            if f.is_finite()
                && f.fract() == 0.0
                && (I64_MIN_F64..I64_MAX_EXCLUSIVE_F64).contains(&f)
            {
                ParseValue::Number(f as i64)
            } else {
                ParseValue::Text(Arc::from(f.to_string().as_str()))
            }
        }
        RuntimeValue::Bool(b) => ParseValue::Text(Arc::from(if b { "true" } else { "false" })),
        RuntimeValue::Map(m) => {
            let mb = m.borrow();
            let kind = mb.get(&MapKey::Str(Rc::from("kind")));
            let children = mb.get(&MapKey::Str(Rc::from("children")));
            if let (Some(RuntimeValue::Str(k)), Some(RuntimeValue::List(kids))) = (kind, children) {
                let items: Vec<ParseValue> = kids.borrow().iter().cloned().map(rv_to_pv).collect();
                return ParseValue::Node(Arc::from(k.as_ref()), Arc::new(items));
            }
            let name = mb.get(&MapKey::Str(Rc::from("name")));
            let inner = mb.get(&MapKey::Str(Rc::from("value")));
            if let (Some(RuntimeValue::Str(n)), Some(v)) = (name, inner) {
                return ParseValue::Named(Arc::from(n.as_ref()), Arc::new(rv_to_pv(v.clone())));
            }
            ParseValue::Nil
        }
        RuntimeValue::List(items) => {
            let kids: Vec<ParseValue> = items.borrow().iter().cloned().map(rv_to_pv).collect();
            ParseValue::Node(Arc::from("list"), Arc::new(kids))
        }
        RuntimeValue::Tuple(items) => {
            let kids: Vec<ParseValue> = items.iter().cloned().map(rv_to_pv).collect();
            ParseValue::Node(Arc::from("tuple"), Arc::new(kids))
        }
        _ => ParseValue::Nil,
    }
}

fn parse_result_to_runtime(result: Result<ParseValue, caap_peg::ParseError>) -> RuntimeValue {
    let map = match result {
        Ok(pv) => Rc::new(RefCell::new(IndexMap::from([
            (MapKey::Str(Rc::from("ok")), RuntimeValue::Bool(true)),
            (MapKey::Str(Rc::from("value")), pv_to_rv(pv)),
        ]))),
        Err(e) => Rc::new(RefCell::new(IndexMap::from([
            (MapKey::Str(Rc::from("ok")), RuntimeValue::Bool(false)),
            (
                MapKey::Str(Rc::from("error")),
                RuntimeValue::Str(Rc::from(e.message.as_ref())),
            ),
        ]))),
    };
    RuntimeValue::Map(map)
}

/// Decode a list of `{"start" int "old_end" int "replacement" str}` maps into
/// `caap_peg::IncrementalEdit`s for `ctfe-grammar-apply-edits`.
fn incremental_edits_from_runtime(
    value: &RuntimeValue,
) -> Result<Vec<caap_peg::IncrementalEdit>, EvalSignal> {
    let context = "ctfe_grammar_apply_edits";
    let RuntimeValue::List(items) = value else {
        return Err(eval_err(format!(
            "{context}: edits must be a list of {{start, old_end, replacement}} maps"
        )));
    };
    let items = items.borrow();
    let mut edits = Vec::with_capacity(items.len());
    for item in items.iter() {
        let start = require_usize(
            &require_map_field(item, "start", context)?,
            &format!("{context}: edit 'start'"),
        )?;
        let old_end = require_usize(
            &require_map_field(item, "old_end", context)?,
            &format!("{context}: edit 'old_end'"),
        )?;
        let replacement = require_string(
            &require_map_field(item, "replacement", context)?,
            &format!("{context}: edit 'replacement' must be a string"),
        )?;
        let edit = caap_peg::IncrementalEdit::new(start, old_end, replacement)
            .ok_or_else(|| eval_err(format!("{context}: edit 'start' must be <= 'old_end'")))?;
        edits.push(edit);
    }
    Ok(edits)
}

/// `{start,end}` byte-span map (offsets always fit `i64`).
fn ast_span_map(start: usize, end: usize) -> RuntimeValue {
    map_value(vec![
        ("start", RuntimeValue::Int(start as i64)),
        ("end", RuntimeValue::Int(end as i64)),
    ])
}

/// Recursively project an [`caap_peg::AstNode`] into an inspectable CAAP map:
/// `{rule, span:{start,end}, action, error, children:[...], captures:[{label,node}]}`.
fn ast_node_to_runtime(node: &caap_peg::AstNode) -> RuntimeValue {
    let children = node.children.iter().map(ast_node_to_runtime).collect();
    let captures = node
        .captures
        .iter()
        .map(|capture| {
            map_value(vec![
                ("label", str_value(&capture.label)),
                ("node", ast_node_to_runtime(&capture.node)),
            ])
        })
        .collect();
    map_value(vec![
        ("rule", str_value(&node.rule)),
        ("span", ast_span_map(node.span.start, node.span.end)),
        ("action", str_value(&node.action)),
        ("error", RuntimeValue::Bool(node.error)),
        ("children", list_value(children)),
        ("captures", list_value(captures)),
    ])
}

/// Decode a `{start,old_end,new_end}` map into an [`caap_peg::AstEdit`].
fn ast_edit_from_runtime(
    value: &RuntimeValue,
    context: &str,
) -> Result<caap_peg::AstEdit, EvalSignal> {
    let start = require_usize(
        &require_map_field(value, "start", context)?,
        &format!("{context}: edit 'start'"),
    )?;
    let old_end = require_usize(
        &require_map_field(value, "old_end", context)?,
        &format!("{context}: edit 'old_end'"),
    )?;
    let new_end = require_usize(
        &require_map_field(value, "new_end", context)?,
        &format!("{context}: edit 'new_end'"),
    )?;
    Ok(caap_peg::AstEdit::new(start, old_end, new_end))
}

/// `null`/absent → `None`; a string → `Some(String)`; otherwise an error.
fn optional_string_arg(
    args: &[RuntimeValue],
    index: usize,
    context: &str,
) -> Result<Option<String>, EvalSignal> {
    match args.get(index) {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(value) => require_string(value, context).map(Some),
    }
}

/// `null`/absent → `None`; a non-negative integer → `Some(usize)`; else an error.
fn optional_usize_arg(
    args: &[RuntimeValue],
    index: usize,
    context: &str,
) -> Result<Option<usize>, EvalSignal> {
    match args.get(index) {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(value) => require_usize(value, context).map(Some),
    }
}

/// Pull a `name → closure` map out of a `{"actions"/"predicates" {...}}` spec.
fn extract_closure_map(spec: &RuntimeValue, key: &str) -> HashMap<String, RuntimeValue> {
    let mut out = HashMap::new();
    let RuntimeValue::Map(spec) = spec else {
        return out;
    };
    if let Some(RuntimeValue::Map(inner)) = spec.borrow().get(&MapKey::Str(Rc::from(key))) {
        for (k, v) in inner.borrow().iter() {
            if let MapKey::Str(name) = k {
                out.insert(name.to_string(), v.clone());
            }
        }
    }
    out
}

fn has_semantic_hooks(spec: &RuntimeValue) -> bool {
    let RuntimeValue::Map(spec) = spec else {
        return false;
    };
    let spec = spec.borrow();
    spec.contains_key(&MapKey::Str(Rc::from("actions")))
        || spec.contains_key(&MapKey::Str(Rc::from("predicates")))
        || spec.contains_key(&MapKey::Str(Rc::from("guards")))
        || spec.contains_key(&MapKey::Str(Rc::from("auto_scope")))
}

/// A [`ParseDriver`] that dispatches grammar actions/predicates to CAAP
/// closures. Each closure receives one context map argument:
/// `{"value" <tree> "rule-stack" (list-of ...) "pos" <int>}`.
///
/// The parse runs synchronously, so borrowing the evaluator through a `RefCell`
/// for the duration of the parse is sound. The first CAAP error is captured and
/// surfaced after parsing.
struct CaapParseDriver<'a> {
    ev: RefCell<&'a mut Evaluator>,
    actions: HashMap<String, RuntimeValue>,
    predicates: HashMap<String, RuntimeValue>,
    guards: HashMap<String, RuntimeValue>,
    auto_scope: bool,
    error: RefCell<Option<EvalSignal>>,
}

/// Build a driver from a `{actions, predicates, guards, auto_scope}` semantics
/// spec. `actions`/`predicates`/`guards` are `name → closure` maps; each closure
/// receives one `{value, rule_stack, pos}` context map. `auto_scope` enables the
/// `@?in_<rule>` / `@?not_in_<rule>` scope predicates with no registration.
fn make_caap_driver<'a>(ev: &'a mut Evaluator, semantics: &RuntimeValue) -> CaapParseDriver<'a> {
    CaapParseDriver {
        ev: RefCell::new(ev),
        actions: extract_closure_map(semantics, "actions"),
        predicates: extract_closure_map(semantics, "predicates"),
        guards: extract_closure_map(semantics, "guards"),
        auto_scope: map_field_value(semantics, "auto_scope")
            .map(|value| crate::values::is_truthy(&value))
            .unwrap_or(false),
        error: RefCell::new(None),
    }
}

fn directive_verdict(accept: bool) -> Directive {
    if accept {
        Directive::Proceed
    } else {
        Directive::Reject
    }
}

impl CaapParseDriver<'_> {
    fn context_map(&self, value: RuntimeValue, rule_stack: &[&str], pos: usize) -> RuntimeValue {
        let stack: Vec<RuntimeValue> = rule_stack
            .iter()
            .map(|s| RuntimeValue::Str(Rc::from(*s)))
            .collect();
        RuntimeValue::Map(Rc::new(RefCell::new(IndexMap::from([
            (MapKey::Str(Rc::from("value")), value),
            (
                MapKey::Str(Rc::from("rule_stack")),
                RuntimeValue::List(Rc::new(RefCell::new(stack))),
            ),
            (MapKey::Str(Rc::from("pos")), RuntimeValue::Int(pos as i64)),
        ]))))
    }

    fn call(&self, closure: &RuntimeValue, arg: RuntimeValue) -> Option<RuntimeValue> {
        if self.error.borrow().is_some() {
            return None;
        }
        // The evaluator is borrowed for the duration of the hook. A hook that
        // re-enters the parser (e.g. calls `ctfe_grammar_parse` on this same
        // evaluator) would alias the borrow; surface that as a clean error
        // instead of a `RefCell` panic.
        let result = match self.ev.try_borrow_mut() {
            Ok(mut ev) => ev.invoke_callback(closure, vec![arg]),
            Err(_) => Err(eval_err(
                "grammar hook re-entered the parser (recursive parsing from an action/predicate is not supported)",
            )),
        };
        match result {
            Ok(value) => Some(value),
            Err(signal) => {
                *self.error.borrow_mut() = Some(signal);
                None
            }
        }
    }

    fn run_action(
        &self,
        name: &str,
        value: ParseValue,
        rule_stack: &[&str],
        pos: usize,
    ) -> ParseValue {
        let Some(closure) = self.actions.get(name) else {
            return value;
        };
        let arg = self.context_map(pv_to_rv(value.clone()), rule_stack, pos);
        match self.call(closure, arg) {
            Some(result) => rv_to_pv(result),
            None => value,
        }
    }

    fn run_predicate(
        &self,
        name: &str,
        value: &ParseValue,
        rule_stack: &[&str],
        pos: usize,
    ) -> bool {
        self.run_bool_hook(&self.predicates, name, value, rule_stack, pos)
    }

    fn run_guard(&self, name: &str, value: &ParseValue, rule_stack: &[&str], pos: usize) -> bool {
        self.run_bool_hook(&self.guards, name, value, rule_stack, pos)
    }

    /// Run a `name → closure` boolean hook (predicate or guard); a registered
    /// closure's truthiness is the verdict, an unregistered name accepts, and a
    /// CAAP error rejects (and is surfaced after the parse).
    fn run_bool_hook(
        &self,
        hooks: &HashMap<String, RuntimeValue>,
        name: &str,
        value: &ParseValue,
        rule_stack: &[&str],
        pos: usize,
    ) -> bool {
        let Some(closure) = hooks.get(name) else {
            return true;
        };
        let arg = self.context_map(pv_to_rv(value.clone()), rule_stack, pos);
        match self.call(closure, arg) {
            Some(result) => crate::values::is_truthy(&result),
            None => false,
        }
    }
}

impl ParseDriver for CaapParseDriver<'_> {
    fn handle(&self, effect: &ParseEffect<'_>, view: &ParseView<'_>) -> Directive {
        match effect {
            ParseEffect::SemanticAction { name, value, .. } => {
                if !self.actions.contains_key(*name) {
                    return Directive::Proceed;
                }
                let transformed =
                    self.run_action(name, (*value).clone(), view.rule_stack, view.pos);
                Directive::Accept(transformed)
            }
            ParseEffect::SemanticPredicate { name, value, .. } => {
                if self.predicates.contains_key(*name) {
                    return directive_verdict(self.run_predicate(
                        name,
                        value,
                        view.rule_stack,
                        view.pos,
                    ));
                }
                // Auto-scope sugar: `@?in_<rule>` / `@?not_in_<rule>` succeed based
                // on the live rule stack, with no explicit registration.
                if self.auto_scope {
                    if let Some(rule) = name.strip_prefix("in_") {
                        return directive_verdict(view.rule_stack.contains(&rule));
                    }
                    if let Some(rule) = name.strip_prefix("not_in_") {
                        return directive_verdict(!view.rule_stack.contains(&rule));
                    }
                }
                Directive::Proceed
            }
            ParseEffect::Guard { name, value, .. } => {
                if !self.guards.contains_key(*name) {
                    return Directive::Proceed;
                }
                directive_verdict(self.run_guard(name, value, view.rule_stack, view.pos))
            }
            _ => Directive::Proceed,
        }
    }
}

/// Extract an ordered `Vec<(String, String)>` from a `RuntimeValue::List` of
/// two-element inner lists/tuples `(list-of name src)`.
fn extract_rule_pairs(
    value: &RuntimeValue,
    context: &str,
) -> Result<Vec<(String, String)>, EvalSignal> {
    let items = match value {
        RuntimeValue::List(lst) => lst.borrow().clone(),
        _ => {
            return Err(eval_err(format!(
                "{context}: rules must be a list of (name source) pairs"
            )))
        }
    };
    let mut pairs = Vec::with_capacity(items.len());
    for item in &items {
        let inner = match item {
            RuntimeValue::List(lst) => lst.borrow().clone(),
            _ => {
                return Err(eval_err(format!(
                    "{context}: each rule entry must be a list of (name source)"
                )))
            }
        };
        if inner.len() != 2 {
            return Err(eval_err(format!(
                "{context}: each rule entry must have exactly 2 elements"
            )));
        }
        let name = require_string(&inner[0], &format!("{context}: rule name must be a string"))?;
        let src = require_string(
            &inner[1],
            &format!("{context}: rule source must be a string"),
        )?;
        pairs.push((name, src));
    }
    Ok(pairs)
}

/// Project a [`caap_peg::ValidationReport`] into
/// `{ok, error_count, warning_count, issues:[{message,severity,rule,code}]}`.
fn validation_report_map(report: &caap_peg::ValidationReport) -> RuntimeValue {
    let issues = report
        .issues
        .iter()
        .map(|issue| {
            let severity = match issue.severity {
                caap_peg::Severity::Error => "error",
                caap_peg::Severity::Warning => "warning",
            };
            map_value(vec![
                ("message", str_value(&issue.message)),
                ("severity", str_value(severity)),
                (
                    "rule",
                    issue
                        .rule
                        .as_deref()
                        .map(str_value)
                        .unwrap_or(RuntimeValue::Null),
                ),
                (
                    "code",
                    issue
                        .code
                        .as_deref()
                        .map(str_value)
                        .unwrap_or(RuntimeValue::Null),
                ),
            ])
        })
        .collect();
    map_value(vec![
        ("ok", RuntimeValue::Bool(report.ok())),
        (
            "error_count",
            RuntimeValue::Int(report.errors().count() as i64),
        ),
        (
            "warning_count",
            RuntimeValue::Int(report.warnings().count() as i64),
        ),
        ("issues", list_value(issues)),
    ])
}

/// Project a [`caap_peg::GrammarDiff`] into a CAAP map.
fn grammar_diff_map(diff: &caap_peg::GrammarDiff) -> RuntimeValue {
    map_value(vec![
        ("added_rules", string_list(&diff.added_rules)),
        ("removed_rules", string_list(&diff.removed_rules)),
        ("changed_rules", string_list(&diff.changed_rules)),
        ("changed_params", string_list(&diff.changed_params)),
        ("changed_metadata", string_list(&diff.changed_metadata)),
        ("start_changed", RuntimeValue::Bool(diff.start_changed)),
        (
            "grammar_metadata_changed",
            RuntimeValue::Bool(diff.grammar_metadata_changed),
        ),
        (
            "metadata_changed",
            RuntimeValue::Bool(diff.metadata_changed),
        ),
    ])
}

/// Project a [`caap_peg::CompletedPrefixParse`] into
/// `{value, consumed, eof, errors}`.
fn prefix_result_map(result: caap_peg::CompletedPrefixParse) -> RuntimeValue {
    let value = result.value.map(pv_to_rv).unwrap_or(RuntimeValue::Null);
    let errors = list_value(result.errors.iter().map(str_value).collect());
    map_value(vec![
        ("value", value),
        ("consumed", RuntimeValue::Int(result.consumed as i64)),
        ("eof", RuntimeValue::Bool(result.eof)),
        ("errors", errors),
    ])
}

/// Project a [`caap_peg::ParseProfile`] into a CAAP map (rules sorted by name).
fn parse_profile_map(profile: &caap_peg::ParseProfile) -> RuntimeValue {
    let mut rules: Vec<_> = profile.rules.iter().collect();
    rules.sort_by(|left, right| left.0.cmp(right.0));
    let rule_list = rules
        .into_iter()
        .map(|(name, stats)| {
            map_value(vec![
                ("rule", str_value(name)),
                ("calls", RuntimeValue::Int(stats.calls as i64)),
                ("memo_hits", RuntimeValue::Int(stats.memo_hits as i64)),
                ("seed_hits", RuntimeValue::Int(stats.seed_hits as i64)),
                ("body_runs", RuntimeValue::Int(stats.body_runs as i64)),
                ("failures", RuntimeValue::Int(stats.failures as i64)),
            ])
        })
        .collect();
    map_value(vec![
        ("expr_steps", RuntimeValue::Int(profile.expr_steps as i64)),
        ("furthest", RuntimeValue::Int(profile.furthest as i64)),
        (
            "total_calls",
            RuntimeValue::Int(profile.total_calls() as i64),
        ),
        (
            "total_body_runs",
            RuntimeValue::Int(profile.total_body_runs() as i64),
        ),
        (
            "memo_hit_rate",
            RuntimeValue::Float(profile.memo_hit_rate()),
        ),
        ("rules", list_value(rule_list)),
    ])
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(ev: &mut Evaluator) {
    // ctfe-grammar-new src → grammar-obj
    ev.register_special(
        "ctfe_grammar_new",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let src = require_string(&args[0], "ctfe_grammar_new: source must be a string")?;
            Grammar::try_new(src)
                .map(grammar_host_obj)
                .map_err(|error| eval_err(error.to_string()))
        },
    );

    // ctfe-grammar-set-start grammar name → grammar-obj (new clone with new start rule)
    ev.register_special(
        "ctfe_grammar_set_start",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let gv = downcast_grammar(&args[0], "ctfe_grammar_set_start")?;
            let name = require_string(&args[1], "ctfe_grammar_set_start: name must be a string")?;
            gv.grammar
                .clone()
                .try_with_start_rule(name)
                .map(grammar_host_obj)
                .map_err(|error| eval_err(error.to_string()))
        },
    );

    // ctfe-grammar-extend grammar rules → grammar-obj (new clone with added/replaced rules)
    // rules = (list-of (list-of "name" "src") ...)
    ev.register_special(
        "ctfe_grammar_extend",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let gv = downcast_grammar(&args[0], "ctfe_grammar_extend")?;
            let pairs = extract_rule_pairs(&args[1], "ctfe_grammar_extend")?;
            let pairs_ref: Vec<(&str, &str)> = pairs
                .iter()
                .map(|(n, s)| (n.as_str(), s.as_str()))
                .collect();
            gv.grammar
                .clone()
                .try_extend(&pairs_ref)
                .map(grammar_host_obj)
                .map_err(|error| eval_err(error.to_string()))
        },
    );

    // ctfe-grammar-describe grammar → structural grammar map.
    ev.register_special(
        "ctfe_grammar_describe",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let grammar = &downcast_grammar(&args[0], "ctfe_grammar_describe")?.grammar;
            grammar_description(grammar)
        },
    );

    // ctfe-grammar-rule-get grammar name [default] → rule descriptor/default/null.
    ev.register_special(
        "ctfe_grammar_rule_get",
        2,
        Some(3),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let grammar = &downcast_grammar(&args[0], "ctfe_grammar_rule_get")?.grammar;
            let name = require_string(
                &args[1],
                "ctfe_grammar_rule_get: rule name must be a string",
            )?;
            let default = args.get(2).cloned().unwrap_or(RuntimeValue::Null);
            let Some((index, rule)) = grammar
                .rules
                .iter()
                .enumerate()
                .find(|(_, rule)| rule.name == name)
            else {
                return Ok(default);
            };
            grammar_rule_map(rule, index, "ctfe_grammar_rule_get")
        },
    );

    // ctfe-grammar-analyze grammar → static PEG analysis map.
    ev.register_special(
        "ctfe_grammar_analyze",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let grammar = &downcast_grammar(&args[0], "ctfe_grammar_analyze")?.grammar;
            grammar_analysis_map(&analyze_grammar(grammar))
        },
    );

    // ctfe-grammar-conflicts grammar → focused grammar conflict/ambiguity report.
    ev.register_special(
        "ctfe_grammar_conflicts",
        1,
        Some(1),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let grammar = &downcast_grammar(&args[0], "ctfe_grammar_conflicts")?.grammar;
            grammar_conflicts_map(&analyze_grammar(grammar))
        },
    );

    // ctfe-lex-token kind text start end → token map for tok(...) grammars.
    ev.register_special(
        "ctfe_lex_token",
        4,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let kind = require_string(&args[0], "ctfe_lex_token: kind must be a string")?;
            if kind.is_empty() {
                return Err(eval_err("ctfe_lex_token: kind must be non-empty"));
            }
            let text = require_string(&args[1], "ctfe_lex_token: text must be a string")?;
            let start = require_usize(
                &args[2],
                "ctfe_lex_token: start must be a non-negative integer",
            )?;
            let end = require_usize(
                &args[3],
                "ctfe_lex_token: end must be a non-negative integer",
            )?;
            lex_token_to_runtime(&LexToken::new(kind, text, start, end))
        },
    );

    // ctfe-lexer-tokenize text specs → token maps.
    // specs = (list-of (map-of "kind" str "pattern" regex ["skip" bool]) ...)
    ev.register_special(
        "ctfe_lexer_tokenize",
        2,
        Some(2),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let text = require_string(&args[0], "ctfe_lexer_tokenize: text must be a string")?;
            let specs = lexer_specs_from_runtime(&args[1])?;
            let tokens = tokenize_with_specs(&text, &specs)?;
            let values = tokens
                .iter()
                .map(lex_token_to_runtime)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(list_value(values))
        },
    );

    // ctfe-grammar-parse text grammar [options] [semantics] → {"ok" bool ...}
    // Optional `semantics` is {"actions" {name closure ...} "predicates" {...}
    //   "guards" {name closure ...} "auto_scope" bool}. actions transform a
    //   match (@name), predicates/guards accept-or-reject (@?name / @!name), and
    //   auto_scope enables @?in_<rule>/@?not_in_<rule>. Each closure takes one
    //   {value, rule_stack, pos} context map.
    // Optional `options` is {"memo" bool "memo_policy" {"global_budget" int|null}
    // "max_steps" int "return_spans" bool}.
    // Each closure receives {"value" <tree> "rule-stack" (list ...) "pos" int}.
    ev.register_special(
        "ctfe_grammar_parse",
        2,
        Some(4),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let text = require_string(&args[0], "ctfe_grammar_parse: text must be a string")?;
            // Borrow (don't clone) the grammar so its per-grammar compiled cache
            // survives across repeated parses of the same grammar object.
            let grammar = &downcast_grammar(&args[1], "ctfe_grammar_parse")?.grammar;
            let (config, semantics) = parse_options_and_semantics(&args, 2, "ctfe_grammar_parse")?;
            let semantics = semantics.filter(|value| !matches!(value, RuntimeValue::Null));
            let result;
            let mut captured_err = None;
            if let Some(semantics) = semantics {
                let driver = make_caap_driver(ev, &semantics);
                result = caap_peg::ParseRequest::new(grammar)
                    .config(config)
                    .driver(&driver)
                    .run(&text);
                captured_err = driver.error.borrow_mut().take();
            } else {
                result = caap_peg::ParseRequest::new(grammar)
                    .config(config)
                    .run(&text);
            }
            if let Some(signal) = captured_err {
                return Err(signal);
            }
            Ok(parse_result_to_runtime(result))
        },
    );

    // ctfe-grammar-parse-tokens text grammar tokens [options] [semantics] → {"ok" bool ...}
    // `tokens` must be the output of `ctfe-lexer-tokenize`, `ctfe-lex-token`, or
    // an equivalent list of {kind,text,start,end} maps.
    ev.register_special(
        "ctfe_grammar_parse_tokens",
        3,
        Some(5),
        crate::values::BuiltinMetadata::compile_time_pure(),
        |ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let text =
                require_string(&args[0], "ctfe_grammar_parse_tokens: text must be a string")?;
            // Borrow (don't clone) so the grammar's compiled cache is reused.
            let grammar = &downcast_grammar(&args[1], "ctfe_grammar_parse_tokens")?.grammar;
            let tokens = lex_tokens_from_runtime(&args[2], "ctfe_grammar_parse_tokens")?;
            let (config, semantics) =
                parse_options_and_semantics(&args, 3, "ctfe_grammar_parse_tokens")?;
            let semantics = semantics.filter(|value| !matches!(value, RuntimeValue::Null));
            let result;
            let mut captured_err = None;
            if let Some(semantics) = semantics {
                let driver = make_caap_driver(ev, &semantics);
                result = caap_peg::ParseRequest::new(grammar)
                    .config(config)
                    .driver(&driver)
                    .tokens(tokens)
                    .run(&text);
                captured_err = driver.error.borrow_mut().take();
            } else {
                result = caap_peg::ParseRequest::new(grammar)
                    .config(config)
                    .tokens(tokens)
                    .run(&text);
            }
            if let Some(signal) = captured_err {
                return Err(signal);
            }
            Ok(parse_result_to_runtime(result))
        },
    );

    engine::register(ev);
}

#[cfg(test)]
mod tests {
    use super::{pv_to_rv, rv_to_pv};
    use caap_peg::ParseValue;
    use std::sync::Arc;

    fn assert_round_trip(pv: ParseValue) {
        assert_eq!(
            rv_to_pv(pv_to_rv(pv.clone())),
            pv,
            "round trip mismatch for {pv:?}"
        );
    }

    #[test]
    fn parse_value_round_trips_through_runtime_value() {
        assert_round_trip(ParseValue::Nil);
        assert_round_trip(ParseValue::Text(Arc::from("hello")));
        assert_round_trip(ParseValue::Number(42));
        assert_round_trip(ParseValue::Number(-7));
        assert_round_trip(ParseValue::Named(
            Arc::from("field"),
            Arc::new(ParseValue::Text(Arc::from("v"))),
        ));
        // Nested node with mixed children, including a deeper node and a named value.
        assert_round_trip(ParseValue::Node(
            Arc::from("expr"),
            Arc::new(vec![
                ParseValue::Text(Arc::from("a")),
                ParseValue::Number(1),
                ParseValue::Node(
                    Arc::from("inner"),
                    Arc::new(vec![
                        ParseValue::Nil,
                        ParseValue::Named(
                            Arc::from("k"),
                            Arc::new(ParseValue::Node(Arc::from("leaf"), Arc::new(vec![]))),
                        ),
                    ]),
                ),
            ]),
        ));
    }

    #[test]
    fn action_only_values_map_to_parse_values() {
        // Bool, integral/non-integral Float, List and Tuple have no direct
        // parse-tree shape but still convert deterministically.
        assert_eq!(
            rv_to_pv(crate::values::RuntimeValue::Bool(true)),
            ParseValue::Text(Arc::from("true"))
        );
        assert_eq!(
            rv_to_pv(crate::values::RuntimeValue::Float(3.0)),
            ParseValue::Number(3)
        );
        assert_eq!(
            rv_to_pv(crate::values::RuntimeValue::Float(2.5)),
            ParseValue::Text(Arc::from("2.5"))
        );
        assert!(matches!(
            rv_to_pv(crate::values::RuntimeValue::Float(
                9_223_372_036_854_775_808.0
            )),
            ParseValue::Text(_)
        ));
    }
}
