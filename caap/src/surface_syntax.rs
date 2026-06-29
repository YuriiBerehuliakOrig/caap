//! Surface grammar materialization from syntax-unit state.
//!
//! Syntax authoring writes neutral rule specs into [`UnitSyntaxState`].  This
//! module is the bridge that turns those specs back into an executable PEG
//! grammar without baking any CAAP-specific syntax extension into the core
//! parser.

use caap_peg::{
    Directive, Grammar, GrammarScalar, ParseDriver, ParseEffect, ParseValue, ParseView, PegExpr,
    SpecCompiler,
};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::builtins::surface::{form_atom, SurfaceAtomSpec};
use crate::error::{CaapError, CaapResult};
use crate::eval::Evaluator;
use crate::frontend::{ParsedForm, ParsedSource};
use crate::ir::intern_string;
use crate::semantic::{PhasePolicy, SemanticValue};
use crate::source::{SourceSpan, SourceSpanLocator};
use crate::unit::UnitSyntaxState;
use crate::values::{ordered_runtime_map_entries, MapKey, RuntimeValue};

const BASE_RULE_NAMES: &[&str] = &[
    "forms", "form", "list", "string", "integer", "boolean", "null", "symbol",
];

pub fn compile_surface_grammar_from_syntax_state(syntax: &UnitSyntaxState) -> CaapResult<Grammar> {
    let spec = surface_grammar_spec_from_syntax_state(syntax)?;
    // The PEG `SpecCompiler` owns grammar structure, trivia/keyword metadata,
    // parametric headers, and imports — but the new (mechanism-vs-policy) peg has
    // no JSON-spec tag for semantic actions; `@name(e)` lives only in the
    // `PegExpr` tree and the textual grammar. So we compile the spec with the
    // surface `-> hook` transforms STRIPPED (keeping all metadata/structure),
    // then rebuild every rule that carried a transform directly as a `PegExpr`
    // via `builder::semantic_action`, and splice it back into the grammar.
    let stripped = strip_behavior_nodes(&spec);
    let mut grammar = SpecCompiler::new().compile(&stripped).map_err(|error| {
        CaapError::parse(format!("failed to compile surface syntax grammar: {error}"))
    })?;
    splice_surface_action_rules(&mut grammar, &spec)?;
    Ok(grammar)
}

/// Rebuild the `expr` of every rule whose spec carried a `["behavior", …]`
/// transform, wrapping the matched value in a `@hook` semantic action (the only
/// way to reach `PegExpr::SemanticAction` now that the JSON spec has no action
/// tag). Rules without transforms are left exactly as the `SpecCompiler` built
/// them.
fn splice_surface_action_rules(grammar: &mut Grammar, spec: &Value) -> CaapResult<()> {
    let Some(rule_entries) = spec.as_array().and_then(|arr| arr.get(3)?.as_array()) else {
        return Ok(());
    };
    for entry in rule_entries {
        let Some(entry) = entry.as_array() else {
            continue;
        };
        // ["rule", header, expr, ...metadata]
        let (Some(header), Some(rule_expr)) = (entry.get(1), entry.get(2)) else {
            continue;
        };
        if !json_contains_behavior(rule_expr) {
            continue;
        }
        let name = rule_name_from_header(header).ok_or_else(|| {
            CaapError::parse("surface grammar rule entry is missing a rule name".to_string())
        })?;
        let Some(slot) = grammar.rules.iter_mut().find(|rule| rule.name == name) else {
            continue;
        };
        // Rebuild the rule body with its `@hook` actions reattached as a
        // `PegExpr` and splice it back in, preserving all SpecCompiler metadata.
        let expr = json_value_to_peg_expr(rule_expr)?;
        let params = slot.params.clone();
        *slot = caap_peg::GrammarRule::from_expr(name, expr, params);
    }
    Ok(())
}

/// A rule header is either a bare name string or a parametric `[name, param…, "->"]`
/// array whose first element is the rule name.
fn rule_name_from_header(header: &Value) -> Option<String> {
    match header {
        Value::String(name) => Some(name.clone()),
        Value::Array(parts) => parts.first().and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

/// Whether a spec expression contains a `["behavior", …]` transform anywhere.
fn json_contains_behavior(value: &Value) -> bool {
    match value {
        Value::Array(items) => {
            if items.first().and_then(Value::as_str) == Some("behavior") {
                return true;
            }
            items.iter().any(json_contains_behavior)
        }
        Value::Object(map) => map.values().any(json_contains_behavior),
        _ => false,
    }
}

/// Replace every `["behavior", behaviors, inner]` node with its `inner`, so the
/// `SpecCompiler` (which no longer knows the `behavior` tag) accepts the spec.
/// Semantic actions are reattached afterwards by [`splice_surface_action_rules`].
fn strip_behavior_nodes(value: &Value) -> Value {
    match value {
        Value::Array(items) => {
            if items.first().and_then(Value::as_str) == Some("behavior") {
                // ["behavior", behaviors, inner] → strip(inner)
                if let Some(inner) = items.get(2) {
                    return strip_behavior_nodes(inner);
                }
            }
            Value::Array(items.iter().map(strip_behavior_nodes).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, val)| (key.clone(), strip_behavior_nodes(val)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Convert a surface-grammar spec expression into a `PegExpr`, reattaching each
/// `["behavior", [["transform", hook, …]], inner]` transform as a `@hook`
/// semantic action. Behavior-free subtrees are compiled by the `SpecCompiler`
/// itself (full fidelity for every expression tag — `call`, `regex`, keywords,
/// …); only the structural combinators that can *enclose* a transform are
/// rebuilt here via `builder`, so we never re-enumerate the whole tag set.
fn json_value_to_peg_expr(value: &Value) -> CaapResult<PegExpr> {
    use caap_peg::builder;
    let items = value.as_array().ok_or_else(|| {
        CaapError::parse(format!(
            "surface grammar expression must be an array: {value}"
        ))
    })?;
    let tag = items.first().and_then(Value::as_str).ok_or_else(|| {
        CaapError::parse(format!(
            "surface grammar expression is missing a tag: {value}"
        ))
    })?;
    if tag == "behavior" {
        return behavior_to_peg_expr(items);
    }
    // No transform anywhere below → let the SpecCompiler build it (all tags).
    if !json_contains_behavior(value) {
        return compile_behavior_free_expr(value);
    }
    let child = |index: usize| -> CaapResult<PegExpr> {
        let node = items.get(index).ok_or_else(|| {
            CaapError::parse(format!(
                "surface grammar {tag:?} is missing operand {index}"
            ))
        })?;
        json_value_to_peg_expr(node)
    };
    let children = |index: usize| -> CaapResult<Vec<PegExpr>> {
        items
            .get(index)
            .and_then(Value::as_array)
            .ok_or_else(|| {
                CaapError::parse(format!(
                    "surface grammar {tag:?} expects a child list at {index}"
                ))
            })?
            .iter()
            .map(json_value_to_peg_expr)
            .collect()
    };
    let string_at = |index: usize| -> CaapResult<String> {
        items
            .get(index)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                CaapError::parse(format!(
                    "surface grammar {tag:?} expects a string at {index}"
                ))
            })
    };
    match tag {
        "seq" | "sequence" => Ok(builder::seq(children(1)?)),
        "choice" => Ok(builder::choice(children(1)?)),
        "many" | "star" | "zero_or_more" => Ok(builder::star(child(1)?)),
        "plus" | "one_or_more" => Ok(builder::plus(child(1)?)),
        "optional" | "opt" => Ok(builder::opt(child(1)?)),
        "named" | "bind" => Ok(builder::named(string_at(1)?, child(2)?)),
        other => Err(CaapError::parse(format!(
            "surface grammar transform is nested under unsupported combinator {other:?}"
        ))),
    }
}

/// Compile a behavior-free spec expression to a `PegExpr` by handing it to the
/// `SpecCompiler` as a throwaway one-rule grammar and lifting the rule's
/// expression. This reuses peg's complete JSON→`PegExpr` mapping (so every tag
/// — `call`, `island`, `regex` with `/`, keywords — round-trips exactly).
fn compile_behavior_free_expr(value: &Value) -> CaapResult<PegExpr> {
    let spec = json!([
        "grammar",
        "__surface_action_tmp__",
        "__r__",
        [["rule", "__r__", value]]
    ]);
    let grammar = SpecCompiler::new().compile(&spec).map_err(|error| {
        CaapError::parse(format!("failed to compile surface subexpression: {error}"))
    })?;
    let rule = grammar
        .rules
        .first()
        .ok_or_else(|| CaapError::parse("surface subexpression produced no rule".to_string()))?;
    Ok(rule.expr().clone())
}

/// `["behavior", [["transform", hook, …], …], inner]` → the inner expression
/// wrapped in one `@hook` semantic action per transform entry (outermost last).
/// `transform` scalar args are intentionally dropped: `PegExpr::SemanticAction`
/// carries no args, and surface hooks fall back to the matched text. Non-transform
/// behavior kinds never reach surface grammars and are rejected.
fn behavior_to_peg_expr(items: &[Value]) -> CaapResult<PegExpr> {
    use caap_peg::builder;
    let behaviors = items.get(1).and_then(Value::as_array).ok_or_else(|| {
        CaapError::parse("surface grammar behavior is missing its entry list".to_string())
    })?;
    let inner = items.get(2).ok_or_else(|| {
        CaapError::parse("surface grammar behavior is missing its inner expression".to_string())
    })?;
    let mut expr = json_value_to_peg_expr(inner)?;
    for entry in behaviors {
        let entry = entry.as_array().ok_or_else(|| {
            CaapError::parse("surface grammar behavior entry must be a list".to_string())
        })?;
        match entry.first().and_then(Value::as_str) {
            Some("transform") => {
                let hook = entry.get(1).and_then(Value::as_str).ok_or_else(|| {
                    CaapError::parse(
                        "surface grammar transform behavior is missing its hook name".to_string(),
                    )
                })?;
                expr = builder::semantic_action(hook, expr);
            }
            other => {
                return Err(CaapError::parse(format!(
                    "surface grammar behavior kind {other:?} is not supported; only \"transform\""
                )));
            }
        }
    }
    Ok(expr)
}

pub fn surface_grammar_spec_from_syntax_state(syntax: &UnitSyntaxState) -> CaapResult<Value> {
    let mut rules = base_surface_rules();
    for (name, expr) in &syntax.grammar_rules {
        rules.insert(name.clone(), semantic_value_to_json(expr)?);
    }

    let mut rule_entries = Vec::with_capacity(rules.len());
    for (name, expr) in rules {
        // A parametric rule gets a `[name, param…, "->"]` header so the engine
        // can bind `$param` / `call`-args; an ordinary rule's header is the name.
        let header = match syntax.grammar_rule_params.get(&name) {
            Some(params) if !params.is_empty() => {
                let mut head = Vec::with_capacity(params.len() + 2);
                head.push(Value::String(name.clone()));
                head.extend(params.iter().map(|p| Value::String(p.clone())));
                head.push(Value::String("->".to_string()));
                Value::Array(head)
            }
            _ => Value::String(name.clone()),
        };
        let mut entry = vec![Value::String("rule".to_string()), header, expr];
        if let Some(SemanticValue::Map(metadata)) = syntax.grammar_metadata.get(&name) {
            for (key, value) in metadata {
                entry.push(json!(["metadata", key, semantic_value_to_json(value)?]));
            }
        }
        rule_entries.push(Value::Array(entry));
    }

    let mut spec = vec![
        Value::String("grammar".to_string()),
        Value::String(syntax.language.clone()),
        Value::String("forms".to_string()),
        Value::Array(rule_entries),
    ];
    if !syntax.grammar_rules.is_empty() {
        // A grammar may opt into a trivia strategy via top-level `trivia`
        // metadata (e.g. "default" to allow `;` line and `#|…|#` block comments
        // in surface source); otherwise default to "default" so custom surface
        // grammars get comment support like ordinary CAAP files. Use "whitespace"
        // to skip only whitespace, or "none" to disable trivia entirely.
        //
        // Exception: if the grammar uses a default comment marker (`;`, `#|`,
        // `/*`) as a *literal token*, the "default" skipper would silently eat
        // that token as a comment — a silent parse failure. With no explicit
        // `trivia`, such grammars fall back to whitespace-only skipping so their
        // own punctuation wins. An explicit `trivia` always takes precedence.
        let trivia = match syntax.grammar_metadata.get("trivia") {
            Some(SemanticValue::Str(token)) => token.clone(),
            // `set comment = "<prefix>"` / `set comment = none`: the grammar
            // owns its comment convention. A prefix compiles to a regex skip
            // strategy (whitespace + prefix-to-EOL); none = whitespace only.
            // The kernel `;` default applies only when the directive is absent.
            _ => match syntax.grammar_metadata.get("comment") {
                Some(SemanticValue::Str(prefix)) if prefix.is_empty() => "whitespace".to_string(),
                Some(SemanticValue::Str(prefix)) => {
                    format!("(?:[ \t\r\n]+|{}[^\n]*)+", regex::escape(prefix))
                }
                _ if grammar_uses_comment_marker_token(syntax) => "whitespace".to_string(),
                _ => "default".to_string(),
            },
        };
        spec.push(json!(["grammar_metadata", "trivia", trivia]));
    }
    for (key, value) in &syntax.grammar_metadata {
        if key == "trivia"
            || BASE_RULE_NAMES.contains(&key.as_str())
            || syntax.grammar_rules.contains_key(key)
        {
            continue;
        }
        spec.push(json!([
            "grammar_metadata",
            key,
            semantic_value_to_json(value)?
        ]));
    }

    Ok(Value::Array(spec))
}

/// The span-trimming trivia convention for a grammar — mirrors EXACTLY the
/// decisions `surface_grammar_spec_from_syntax_state` bakes into the compiled
/// skip strategy, so diagnostics trim the same noise the parser skips.
pub fn driver_trivia_from_syntax_state(syntax: &UnitSyntaxState) -> DriverTrivia {
    match syntax.grammar_metadata.get("trivia") {
        Some(SemanticValue::Str(token)) => match token.as_str() {
            "none" => DriverTrivia {
                skip_whitespace: false,
                line_comments: Vec::new(),
                block_comments: Vec::new(),
            },
            "whitespace" => DriverTrivia::whitespace_only(),
            "default" => DriverTrivia::default_convention(),
            // custom regex strategy: best effort — whitespace trimming only
            _ => DriverTrivia::whitespace_only(),
        },
        _ => match syntax.grammar_metadata.get("comment") {
            Some(SemanticValue::Str(prefix)) if prefix.is_empty() => {
                DriverTrivia::whitespace_only()
            }
            Some(SemanticValue::Str(prefix)) => DriverTrivia {
                skip_whitespace: true,
                line_comments: vec![prefix.clone()],
                block_comments: Vec::new(),
            },
            _ if grammar_uses_comment_marker_token(syntax) => DriverTrivia::whitespace_only(),
            _ => DriverTrivia::default_convention(),
        },
    }
}

/// Default comment markers under [`caap_peg`]'s "default" skip strategy —
/// `DEFAULT_LINE_COMMENTS` / `DEFAULT_BLOCK_COMMENTS` (peg/src/skip.rs). A token
/// beginning with one of these at a trivia boundary starts a comment.
const DEFAULT_COMMENT_MARKERS: &[&str] = &[";", "#|", "/*"];

/// Does any rule use a default comment marker as a literal token? If so, the
/// "default" trivia skipper would consume that token as a comment, so the
/// grammar must skip whitespace only (unless it set `trivia` explicitly).
fn grammar_uses_comment_marker_token(syntax: &UnitSyntaxState) -> bool {
    syntax
        .grammar_rules
        .values()
        .any(semantic_value_has_comment_marker_literal)
}

/// Walks a rule's neutral spec for a `["literal", <text>]` node whose text
/// begins with a default comment marker.
fn semantic_value_has_comment_marker_literal(value: &SemanticValue) -> bool {
    match value {
        SemanticValue::List(items) => {
            if let [SemanticValue::Str(tag), SemanticValue::Str(text), ..] = items.as_slice() {
                if tag == "literal"
                    && DEFAULT_COMMENT_MARKERS
                        .iter()
                        .any(|marker| text.starts_with(marker))
                {
                    return true;
                }
            }
            items.iter().any(semantic_value_has_comment_marker_literal)
        }
        SemanticValue::Map(entries) => entries
            .iter()
            .any(|(_, value)| semantic_value_has_comment_marker_literal(value)),
        _ => false,
    }
}

pub fn semantic_value_to_json(value: &SemanticValue) -> CaapResult<Value> {
    match value {
        SemanticValue::Null => Ok(Value::Null),
        SemanticValue::Bool(value) => Ok(Value::Bool(*value)),
        SemanticValue::Int(value) => Ok(Value::Number((*value).into())),
        SemanticValue::Float(value) => serde_json::Number::from_f64(*value)
            .map(Value::Number)
            .ok_or_else(|| {
                CaapError::parse(format!(
                    "semantic float cannot be represented as JSON: {value}"
                ))
            }),
        SemanticValue::Str(value) => Ok(Value::String(value.clone())),
        SemanticValue::Node(node) => Err(CaapError::parse(format!(
            "semantic node references are not valid grammar spec values: {node}"
        ))),
        SemanticValue::List(items) => semantic_list_to_json(items),
        SemanticValue::Map(entries) => {
            let mut map = Map::new();
            for (key, value) in entries {
                map.insert(key.clone(), semantic_value_to_json(value)?);
            }
            Ok(Value::Object(map))
        }
    }
}

pub fn runtime_value_to_parse_value(value: &RuntimeValue) -> CaapResult<ParseValue> {
    match value {
        RuntimeValue::Null => Ok(ParseValue::Node(
            "__caap_rt_null".into(),
            Arc::new(Vec::new()),
        )),
        RuntimeValue::Bool(value) => Ok(ParseValue::Node(
            "__caap_rt_bool".into(),
            Arc::new(vec![ParseValue::Text(Arc::from(
                value.to_string().as_str(),
            ))]),
        )),
        RuntimeValue::Int(value) => Ok(ParseValue::Number(*value)),
        RuntimeValue::Float(value) => Ok(ParseValue::Node(
            "__caap_rt_float".into(),
            Arc::new(vec![ParseValue::Text(Arc::from(
                value.to_string().as_str(),
            ))]),
        )),
        RuntimeValue::Str(value) => Ok(ParseValue::Text(Arc::from(value.as_ref()))),
        RuntimeValue::Tuple(items) => items
            .iter()
            .map(runtime_value_to_parse_value)
            .collect::<CaapResult<Vec<_>>>()
            .map(|items| ParseValue::Node("__caap_rt_tuple".into(), Arc::new(items))),
        RuntimeValue::List(items) => items
            .borrow()
            .iter()
            .map(runtime_value_to_parse_value)
            .collect::<CaapResult<Vec<_>>>()
            .map(|items| ParseValue::Node("__caap_rt_list".into(), Arc::new(items))),
        RuntimeValue::Map(entries) => {
            let entries = entries.borrow();
            if let Some(value) = runtime_surface_form_map_to_parse_value(&entries)? {
                return Ok(value);
            }
            let ordered_entries = ordered_runtime_map_entries(&entries);
            let mut pairs = Vec::with_capacity(ordered_entries.len());
            for (key, value) in ordered_entries {
                pairs.push(ParseValue::Node(
                    "__caap_rt_pair".into(),
                    Arc::new(vec![
                        runtime_value_to_parse_value(&RuntimeValue::from(key.clone()))?,
                        runtime_value_to_parse_value(value)?,
                    ]),
                ));
            }
            Ok(ParseValue::Node("__caap_rt_map".into(), Arc::new(pairs)))
        }
        RuntimeValue::Bytes(_)
        | RuntimeValue::Closure(_)
        | RuntimeValue::Macro(_)
        | RuntimeValue::Builtin(_)
        | RuntimeValue::HostFunction(_)
        | RuntimeValue::HostObject(_)
        | RuntimeValue::Ref(_)
        | RuntimeValue::UninitializedTopLevel => Err(CaapError::parse(format!(
            "runtime value {value} cannot be embedded in a PEG parse value"
        ))),
    }
}

pub fn parse_value_to_runtime_value(value: &ParseValue) -> CaapResult<RuntimeValue> {
    match value {
        ParseValue::Nil => Ok(RuntimeValue::Null),
        ParseValue::Text(value) => Ok(RuntimeValue::Str(Rc::from(value.as_ref()))),
        ParseValue::Number(value) => Ok(RuntimeValue::Int(*value)),
        ParseValue::Named(_, value) => parse_value_to_runtime_value(value),
        ParseValue::SpannedValue { value, .. } => parse_value_to_runtime_value(value),
        ParseValue::Node(kind, children) if &**kind == "__caap_rt_null" => Ok(RuntimeValue::Null),
        ParseValue::Node(kind, children) if &**kind == "__caap_rt_bool" => {
            let Some(ParseValue::Text(value)) = children.first() else {
                return Err(CaapError::parse("encoded runtime bool is missing payload"));
            };
            match value.as_ref() {
                "true" => Ok(RuntimeValue::Bool(true)),
                "false" => Ok(RuntimeValue::Bool(false)),
                _ => Err(CaapError::parse(format!(
                    "encoded runtime bool has invalid payload: {value}"
                ))),
            }
        }
        ParseValue::Node(kind, children) if &**kind == "__caap_rt_float" => {
            let Some(ParseValue::Text(value)) = children.first() else {
                return Err(CaapError::parse("encoded runtime float is missing payload"));
            };
            value
                .parse::<f64>()
                .map(RuntimeValue::Float)
                .map_err(|error| {
                    CaapError::parse(format!("encoded runtime float is invalid: {error}"))
                })
        }
        ParseValue::Node(kind, children) if &**kind == "__caap_rt_surface_form" => {
            parse_encoded_surface_form(children)
        }
        ParseValue::Node(kind, children) if &**kind == "__caap_rt_tuple" => children
            .iter()
            .map(parse_value_to_runtime_value)
            .collect::<CaapResult<Vec<_>>>()
            .map(|items| RuntimeValue::Tuple(items.into())),
        ParseValue::Node(kind, children) if &**kind == "__caap_rt_list" => children
            .iter()
            .map(parse_value_to_runtime_value)
            .collect::<CaapResult<Vec<_>>>()
            .map(|items| RuntimeValue::List(Rc::new(RefCell::new(items)))),
        ParseValue::Node(kind, children) if &**kind == "__caap_rt_map" => {
            let mut map = IndexMap::new();
            for child in children.iter() {
                let ParseValue::Node(pair_kind, pair) = child else {
                    return Err(CaapError::parse(
                        "encoded runtime map contains a non-pair entry",
                    ));
                };
                if &**pair_kind != "__caap_rt_pair" || pair.len() != 2 {
                    return Err(CaapError::parse(
                        "encoded runtime map contains malformed pair entry",
                    ));
                }
                let key_value = parse_value_to_runtime_value(&pair[0])?;
                let key = MapKey::try_from(&key_value).map_err(|error| {
                    CaapError::parse(format!("encoded runtime map key is invalid: {error}"))
                })?;
                map.insert(key, parse_value_to_runtime_value(&pair[1])?);
            }
            Ok(RuntimeValue::Map(Rc::new(RefCell::new(map))))
        }
        ParseValue::Node(kind, children)
            if matches!(
                kind.as_ref(),
                "one_or_more" | "zero_or_more" | "sep_one_or_more"
            ) =>
        {
            structural_children_to_list(children)
        }
        ParseValue::Node(_, children) if children.len() == 1 => {
            parse_value_to_runtime_value(&children[0])
        }
        ParseValue::Node(_, children) => structural_children_to_list(children),
    }
}

/// Convert a structural node's children (sequence/repeat groupings) to a list.
/// When the children MIX lowered surface forms with raw unlabeled tokens (bare
/// `Text` from literal punctuation like `","` or un-hooked terminals), the raw
/// tokens are DROPPED: the punctuation did its syntactic job during the match
/// and is not data. A children set with no forms at all is kept verbatim —
/// custom hooks legitimately consume raw token sequences.
fn structural_children_to_list(children: &[ParseValue]) -> CaapResult<RuntimeValue> {
    /// Does any lowered form live anywhere under this value (looking THROUGH
    /// structural nodes, labels and spans — but a `__caap_rt_*` encoding is
    /// itself the form)?
    fn contains_lowered_form(value: &ParseValue) -> bool {
        match value {
            ParseValue::Node(kind, _) if kind.starts_with("__caap_rt_") => true,
            ParseValue::Node(_, children) => children.iter().any(contains_lowered_form),
            ParseValue::Named(_, inner) => contains_lowered_form(inner),
            ParseValue::SpannedValue { value, .. } => contains_lowered_form(value),
            _ => false,
        }
    }
    fn is_raw_token(value: &ParseValue) -> bool {
        matches!(value, ParseValue::Text(_) | ParseValue::Nil)
    }
    /// A structural (non-encoded) grouping node — its converted List result is
    /// SPLICED so repeats of multi-element sequences yield flat form lists.
    fn is_structural_node(value: &ParseValue) -> bool {
        match value {
            ParseValue::Node(kind, _) => !kind.starts_with("__caap_rt_"),
            ParseValue::Named(_, inner) => is_structural_node(inner),
            ParseValue::SpannedValue { value, .. } => is_structural_node(value),
            _ => false,
        }
    }
    let has_forms = children.iter().any(contains_lowered_form);
    let mut items = Vec::new();
    for child in children {
        if has_forms && is_raw_token(child) {
            continue;
        }
        let value = parse_value_to_runtime_value(child)?;
        match value {
            RuntimeValue::List(list) if has_forms && is_structural_node(child) => {
                items.extend(list.borrow().iter().cloned());
            }
            value => items.push(value),
        }
    }
    Ok(RuntimeValue::List(Rc::new(RefCell::new(items))))
}

fn runtime_surface_form_map_to_parse_value(
    fields: &IndexMap<MapKey, RuntimeValue>,
) -> CaapResult<Option<ParseValue>> {
    let Some(RuntimeValue::Str(kind)) = fields.get(&MapKey::Str(intern_string("kind"))) else {
        return Ok(None);
    };
    if !matches!(
        kind.as_ref(),
        "list" | "symbol" | "string" | "integer" | "boolean" | "null"
    ) {
        return Ok(None);
    }
    let span = required_span_field(fields)?;
    let value = fields
        .get(&MapKey::Str(intern_string("value")))
        .cloned()
        .unwrap_or(RuntimeValue::Null);
    let raw_text = optional_string_field(fields, "raw_text").unwrap_or_default();
    let rule = optional_string_field(fields, "rule").unwrap_or_else(|| kind.to_string());
    // None = not bracket-delimited; encodes as Nil so null survives the trip.
    let delimiter = optional_string_field(fields, "delimiter");
    let items = fields
        .get(&MapKey::Str(intern_string("items")))
        .cloned()
        .unwrap_or_else(|| RuntimeValue::Tuple(Vec::new().into()));
    Ok(Some(ParseValue::Node(
        "__caap_rt_surface_form".into(),
        Arc::new(vec![
            ParseValue::Text(Arc::from(&**kind)),
            runtime_value_to_parse_value(&value)?,
            ParseValue::Text(Arc::from(raw_text.as_str())),
            ParseValue::Text(Arc::from(rule.as_str())),
            match &delimiter {
                Some(delimiter) => ParseValue::Text(Arc::from(delimiter.as_str())),
                None => ParseValue::Nil,
            },
            ParseValue::Number(span.start as i64),
            ParseValue::Number(span.end as i64),
            ParseValue::Number(span.start_line as i64),
            ParseValue::Number(span.start_col as i64),
            ParseValue::Number(span.end_line as i64),
            ParseValue::Number(span.end_col as i64),
            runtime_value_to_parse_value(&items)?,
            // Carry the source path so grammar-lowered forms keep their file
            // location through the surface-form ↔ ParseValue round-trip (empty
            // string = no path). Required for source-level debugging.
            ParseValue::Text(Arc::from(span.path.as_deref().unwrap_or(""))),
        ]),
    )))
}

fn parse_encoded_surface_form(children: &[ParseValue]) -> CaapResult<RuntimeValue> {
    if children.len() != 13 {
        return Err(CaapError::parse("encoded surface form has malformed arity"));
    }
    let kind = encoded_text_child(children, 0, "kind")?;
    let value = parse_value_to_runtime_value(&children[1])?;
    let raw_text = encoded_text_child(children, 2, "raw_text")?.to_string();
    let rule = encoded_text_child(children, 3, "rule")?.to_string();
    let delimiter = match &children[4] {
        ParseValue::Nil => None,
        _ => Some(encoded_text_child(children, 4, "delimiter")?.to_string()),
    };
    // Restore the source path (empty = none) so the decoded span points back at
    // the originating file — see `runtime_surface_form_map_to_parse_value`.
    let path = encoded_text_child(children, 12, "path")?;
    let locator = if path.is_empty() {
        None
    } else {
        Some(SourceSpanLocator::new(None, Some(path.to_string()))?)
    };
    let span = SourceSpan::with_locator(
        locator,
        encoded_usize_child(children, 5, "start")?,
        encoded_usize_child(children, 6, "end")?,
        encoded_usize_child(children, 7, "start_line")?,
        encoded_usize_child(children, 8, "start_col")?,
        encoded_usize_child(children, 9, "end_line")?,
        encoded_usize_child(children, 10, "end_col")?,
    )?;
    let items = encoded_sequence_items(&parse_value_to_runtime_value(&children[11])?)?;
    Ok(form_atom(
        SurfaceAtomSpec::new(kind, value, span)
            .raw_text(raw_text)
            .rule(rule)
            .items(items)
            .delimiter(delimiter),
    ))
}

fn encoded_text_child<'a>(
    children: &'a [ParseValue],
    index: usize,
    label: &str,
) -> CaapResult<&'a str> {
    match children.get(index) {
        Some(ParseValue::Text(value)) => Ok(&**value),
        _ => Err(CaapError::parse(format!(
            "encoded surface form missing text child {label:?}"
        ))),
    }
}

fn encoded_usize_child(children: &[ParseValue], index: usize, label: &str) -> CaapResult<usize> {
    match children.get(index) {
        Some(ParseValue::Number(value)) if *value >= 0 => usize::try_from(*value).map_err(|_| {
            CaapError::parse(format!(
                "encoded surface form integer child {label:?} is too large"
            ))
        }),
        _ => Err(CaapError::parse(format!(
            "encoded surface form missing non-negative integer child {label:?}"
        ))),
    }
}

fn encoded_sequence_items(value: &RuntimeValue) -> CaapResult<Vec<RuntimeValue>> {
    match value {
        RuntimeValue::Null => Ok(Vec::new()),
        RuntimeValue::Tuple(items) => Ok(items.iter().cloned().collect()),
        RuntimeValue::List(items) => Ok(items.borrow().clone()),
        item => Ok(vec![item.clone()]),
    }
}

/// Collect labelled captures TRANSPARENTLY through structural nodes
/// (sequences, groups, repeats, optionals): same-name labels accumulate in
/// encounter order, including from inside `(...)` groups and `*`/`+`/`?`
/// repetitions. Descent stops at a `Named`'s interior (its content belongs to
/// that binding) and at encoded `__caap_rt_*` payloads (already-lowered forms
/// own their internals).
fn collect_named_bindings_deep<'a>(
    value: &'a ParseValue,
    out: &mut IndexMap<String, Vec<&'a ParseValue>>,
) {
    match value {
        ParseValue::Named(name, inner) => {
            out.entry(name.to_string())
                .or_default()
                .push(inner.as_ref());
        }
        ParseValue::SpannedValue { value, .. } => collect_named_bindings_deep(value, out),
        ParseValue::Node(kind, children) if !kind.starts_with("__caap_rt_") => {
            for child in children.iter() {
                collect_named_bindings_deep(child, out);
            }
        }
        _ => {}
    }
}

/// Whether any labelled capture exists anywhere under structural nodes (the
/// deep companion of `ParseValue::named_bindings().is_empty()`).
fn has_named_bindings_deep(value: &ParseValue) -> bool {
    match value {
        ParseValue::Named(..) => true,
        ParseValue::SpannedValue { value, .. } => has_named_bindings_deep(value),
        ParseValue::Node(kind, children) if !kind.starts_with("__caap_rt_") => {
            children.iter().any(has_named_bindings_deep)
        }
        _ => false,
    }
}

pub fn named_parse_bindings_to_runtime_map(
    value: &ParseValue,
) -> CaapResult<IndexMap<MapKey, RuntimeValue>> {
    let mut collected: IndexMap<String, Vec<&ParseValue>> = IndexMap::new();
    collect_named_bindings_deep(value, &mut collected);
    let mut fields = IndexMap::new();
    for (name, bindings) in collected {
        let converted = if bindings.len() == 1 {
            // Single binding keeps its legacy shape (a `label:rule*` repeat
            // already converts to a list).
            parse_value_to_runtime_value(bindings[0])?
        } else {
            // Same-name labels CONCATENATE in encounter order; list-valued
            // pieces (repeats) flatten into the concatenation.
            let mut items = Vec::new();
            for binding in bindings {
                match parse_value_to_runtime_value(binding)? {
                    RuntimeValue::List(list) => items.extend(list.borrow().iter().cloned()),
                    RuntimeValue::Tuple(tuple) => items.extend(tuple.iter().cloned()),
                    value => items.push(value),
                }
            }
            RuntimeValue::List(Rc::new(RefCell::new(items)))
        };
        fields.insert(MapKey::Str(Rc::from(name.as_str())), converted);
    }
    Ok(fields)
}

/// The one-call API's decode: the top-level forms as RICH runtime maps (all
/// fields the hooks produced — producing rule, honest delimiter, raw_text —
/// survive; the narrow `ParsedForm` round-trip would re-default them). Raw
/// unlabeled leftovers at the top level are dropped like everywhere else.
pub fn parse_value_to_rich_forms(value: &ParseValue) -> CaapResult<Vec<RuntimeValue>> {
    let converted = parse_value_to_runtime_value(value)?;
    let items = match converted {
        RuntimeValue::List(items) => items.borrow().clone(),
        RuntimeValue::Tuple(items) => items.to_vec(),
        single => vec![single],
    };
    Ok(items
        .into_iter()
        .filter(|item| matches!(item, RuntimeValue::Map(_)))
        .collect())
}

pub fn parse_value_to_parsed_source(value: &ParseValue) -> CaapResult<ParsedSource> {
    let runtime = parse_value_to_runtime_value(value)?;
    match runtime {
        RuntimeValue::List(items) => Ok(ParsedSource {
            forms: items
                .borrow()
                .iter()
                .map(runtime_surface_form_to_parsed_form)
                .collect::<CaapResult<Vec<_>>>()?,
        }),
        RuntimeValue::Tuple(items) => Ok(ParsedSource {
            forms: items
                .iter()
                .map(runtime_surface_form_to_parsed_form)
                .collect::<CaapResult<Vec<_>>>()?,
        }),
        form => Ok(ParsedSource {
            forms: vec![runtime_surface_form_to_parsed_form(&form)?],
        }),
    }
}

pub fn runtime_surface_form_to_parsed_form(value: &RuntimeValue) -> CaapResult<ParsedForm> {
    let RuntimeValue::Map(fields) = value else {
        return Err(CaapError::parse(format!(
            "surface parse result is not a form map: {value}"
        )));
    };
    let fields = fields.borrow();
    let kind = required_str_field(&fields, "kind")?;
    let span = required_span_field(&fields)?;
    match kind {
        "list" => {
            let items = optional_sequence_field(&fields, "items")?
                .into_iter()
                .map(|item| runtime_surface_form_to_parsed_form(&item))
                .collect::<CaapResult<Vec<_>>>()?;
            Ok(ParsedForm::List { items, span })
        }
        "symbol" => Ok(ParsedForm::Symbol {
            text: required_str_field(&fields, "value")?.to_string(),
            span,
        }),
        "string" => {
            let value = required_str_field(&fields, "value")?.to_string();
            let raw = optional_string_field(&fields, "raw_text").unwrap_or_else(|| {
                serde_json::to_string(&value).unwrap_or_else(|_| format!("\"{value}\""))
            });
            Ok(ParsedForm::String { value, raw, span })
        }
        "integer" => {
            let value = required_int_field(&fields, "value")?;
            let raw =
                optional_string_field(&fields, "raw_text").unwrap_or_else(|| value.to_string());
            Ok(ParsedForm::Integer { value, raw, span })
        }
        "boolean" => Ok(ParsedForm::Boolean {
            value: required_bool_field(&fields, "value")?,
            span,
        }),
        "null" => Ok(ParsedForm::Null { span }),
        other => Err(CaapError::parse(format!(
            "unknown surface form kind: {other}"
        ))),
    }
}

fn required_str_field<'a>(
    fields: &'a IndexMap<MapKey, RuntimeValue>,
    key: &str,
) -> CaapResult<&'a str> {
    match fields.get(&MapKey::Str(intern_string(key))) {
        Some(RuntimeValue::Str(value)) => Ok(value.as_ref()),
        _ => Err(CaapError::parse(format!(
            "surface form missing string field {key:?}"
        ))),
    }
}

fn optional_string_field(fields: &IndexMap<MapKey, RuntimeValue>, key: &str) -> Option<String> {
    match fields.get(&MapKey::Str(intern_string(key))) {
        Some(RuntimeValue::Str(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn required_int_field(fields: &IndexMap<MapKey, RuntimeValue>, key: &str) -> CaapResult<i64> {
    match fields.get(&MapKey::Str(intern_string(key))) {
        Some(RuntimeValue::Int(value)) => Ok(*value),
        _ => Err(CaapError::parse(format!(
            "surface form missing integer field {key:?}"
        ))),
    }
}

fn required_bool_field(fields: &IndexMap<MapKey, RuntimeValue>, key: &str) -> CaapResult<bool> {
    match fields.get(&MapKey::Str(intern_string(key))) {
        Some(RuntimeValue::Bool(value)) => Ok(*value),
        Some(RuntimeValue::Str(value)) => match value.as_ref() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(CaapError::parse(format!(
                "surface bool field {key:?} is invalid: {value}"
            ))),
        },
        _ => Err(CaapError::parse(format!(
            "surface form missing bool field {key:?}"
        ))),
    }
}

fn optional_sequence_field(
    fields: &IndexMap<MapKey, RuntimeValue>,
    key: &str,
) -> CaapResult<Vec<RuntimeValue>> {
    match fields.get(&MapKey::Str(intern_string(key))) {
        None | Some(RuntimeValue::Null) => Ok(Vec::new()),
        Some(RuntimeValue::Tuple(items)) => Ok(items.iter().cloned().collect()),
        Some(RuntimeValue::List(items)) => Ok(items.borrow().clone()),
        Some(item) => Ok(vec![item.clone()]),
    }
}

fn required_span_field(fields: &IndexMap<MapKey, RuntimeValue>) -> CaapResult<SourceSpan> {
    let RuntimeValue::Map(span) = fields
        .get(&MapKey::Str(intern_string("span")))
        .ok_or_else(|| CaapError::parse("surface form missing span field"))?
    else {
        return Err(CaapError::parse("surface form span field must be a map"));
    };
    let span = span.borrow();
    let start = required_usize_field(&span, "start")?;
    let end = required_usize_field(&span, "end")?;
    let start_line = required_usize_field(&span, "start_line")?;
    let start_col = required_usize_field(&span, "start_col")?;
    let end_line = required_usize_field(&span, "end_line")?;
    let end_col = required_usize_field(&span, "end_col")?;
    // Preserve the optional source path so grammar-lowered forms keep their
    // file location through the surface-form → ParsedForm → IR round-trip.
    let locator = match span.get(&MapKey::Str(intern_string("path"))) {
        Some(RuntimeValue::Str(path)) => {
            Some(SourceSpanLocator::new(None, Some(path.to_string()))?)
        }
        _ => None,
    };
    SourceSpan::with_locator(
        locator, start, end, start_line, start_col, end_line, end_col,
    )
}

fn required_usize_field(fields: &IndexMap<MapKey, RuntimeValue>, key: &str) -> CaapResult<usize> {
    match fields.get(&MapKey::Str(intern_string(key))) {
        Some(RuntimeValue::Int(value)) if *value >= 0 => usize::try_from(*value).map_err(|_| {
            CaapError::parse(format!("surface span integer field {key:?} is too large"))
        }),
        _ => Err(CaapError::parse(format!(
            "surface span missing non-negative integer field {key:?}"
        ))),
    }
}

/// The trivia conventions active for span trimming: what counts as ignorable
/// noise BEFORE a token (the engine's match spans include leading trivia the
/// sequence consumed; diagnostics must point at the token itself).
#[derive(Clone, Debug)]
pub struct DriverTrivia {
    pub skip_whitespace: bool,
    pub line_comments: Vec<String>,
    pub block_comments: Vec<(String, String)>,
}

impl DriverTrivia {
    /// The kernel default: whitespace + `;` line comments + `#|…|#` and
    /// `/*…*/` blocks (mirrors peg's `DefaultSkipStrategy`).
    pub fn default_convention() -> Self {
        Self {
            skip_whitespace: true,
            line_comments: caap_peg::skip::DEFAULT_LINE_COMMENTS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            block_comments: caap_peg::skip::DEFAULT_BLOCK_COMMENTS
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        }
    }

    pub fn whitespace_only() -> Self {
        Self {
            skip_whitespace: true,
            line_comments: Vec::new(),
            block_comments: Vec::new(),
        }
    }
}

pub struct SurfaceBuiltinDriver<'a> {
    source: &'a str,
    source_offset: usize,
    source_path: Option<String>,
    hooks: HashMap<String, RuntimeValue>,
    evaluator: RefCell<Evaluator>,
    error: RefCell<Option<String>>,
    hook_calls: Cell<usize>,
    trivia: DriverTrivia,
}

impl<'a> SurfaceBuiltinDriver<'a> {
    pub fn new(source: &'a str, source_path: Option<String>) -> Self {
        Self {
            source,
            source_offset: 0,
            source_path,
            hooks: HashMap::new(),
            evaluator: RefCell::new(Evaluator::with_phase(
                crate::graph::IRGraph::new(),
                PhasePolicy::CompileTime,
            )),
            error: RefCell::new(None),
            hook_calls: Cell::new(0),
            trivia: DriverTrivia::default_convention(),
        }
    }

    pub fn with_trivia(mut self, trivia: DriverTrivia) -> Self {
        self.trivia = trivia;
        self
    }

    pub fn with_source_offset(mut self, offset: usize) -> Self {
        self.source_offset = offset;
        self
    }

    pub fn with_hooks(mut self, hooks: HashMap<String, RuntimeValue>) -> Self {
        self.hooks = hooks;
        self
    }

    pub fn error(&self) -> Option<String> {
        self.error.borrow().clone()
    }

    fn set_error(&self, error: impl Into<String>) {
        let mut slot = self.error.borrow_mut();
        if slot.is_none() {
            *slot = Some(error.into());
        }
    }

    fn invoke_builtin_surface_hook(
        &self,
        name: &str,
        value: ParseValue,
        span_offsets: Option<(usize, usize)>,
        matched_text: &str,
        args: &[GrammarScalar],
        producing_rule: Option<&str>,
    ) -> CaapResult<ParseValue> {
        let runtime_value = if !has_named_bindings_deep(&value) {
            parse_value_to_runtime_value(&value)?
        } else {
            RuntimeValue::Map(Rc::new(RefCell::new(named_parse_bindings_to_runtime_map(
                &value,
            )?)))
        };
        let span = self.span_from_offsets(span_offsets).ok_or_else(|| {
            CaapError::semantic(format!(
                "surface semantic action {name:?} requires a local span"
            ))
        })?;
        let matched = if matched_text.is_empty() {
            runtime_value.to_string()
        } else {
            matched_text.to_string()
        };
        let result = match name {
            "surface.symbol" => {
                let RuntimeValue::Str(text) = runtime_value else {
                    return Err(CaapError::semantic(format!(
                        "surface.symbol expects parsed text, got {}",
                        runtime_value_summary(&runtime_value, 0)
                    )));
                };
                crate::builtins::surface::form_symbol(text.to_string(), span)
            }
            "surface.integer" => {
                let value = match runtime_value {
                    RuntimeValue::Int(value) => value,
                    RuntimeValue::Str(text) => text.parse::<i64>().map_err(|error| {
                        CaapError::semantic(format!("surface.integer parse failed: {error}"))
                    })?,
                    other => {
                        return Err(CaapError::semantic(format!(
                            "surface.integer expects parsed integer text, got {}",
                            runtime_value_summary(&other, 0)
                        )));
                    }
                };
                crate::builtins::surface::form_integer(value, span)
            }
            "surface.string" => {
                let RuntimeValue::Str(text) = runtime_value else {
                    return Err(CaapError::semantic(format!(
                        "surface.string expects parsed string text, got {}",
                        runtime_value_summary(&runtime_value, 0)
                    )));
                };
                let raw = text.to_string();
                let decoded = serde_json::from_str::<String>(&raw).map_err(|error| {
                    CaapError::semantic(format!("surface.string decode failed: {error}"))
                })?;
                crate::builtins::surface::form_atom(
                    SurfaceAtomSpec::new(
                        "string",
                        RuntimeValue::Str(Rc::from(decoded.as_str())),
                        span,
                    )
                    .raw_text(raw),
                )
            }
            "surface.keyword_string" => {
                let keyword = args
                    .first()
                    .map(grammar_scalar_to_string)
                    .unwrap_or(matched);
                crate::builtins::surface::form_atom(
                    SurfaceAtomSpec::new(
                        "string",
                        RuntimeValue::Str(Rc::from(keyword.as_str())),
                        span,
                    )
                    .raw_text(keyword)
                    .rule("keyword_string"),
                )
            }
            "surface.boolean" => crate::builtins::surface::form_atom(
                SurfaceAtomSpec::new("boolean", RuntimeValue::Bool(matched == "true"), span)
                    .raw_text(matched),
            ),
            "surface.null" => crate::builtins::surface::form_null(span),
            "surface.list" => {
                // Delimiter source of truth: an explicit grammar arg
                // (`-> surface.list("brace")`) wins; otherwise the REAL first
                // matched character decides — `(`/`[`/`{` → paren/bracket/
                // brace, anything else → null (the rule is not bracket-
                // delimited) — never the old hardcoded "paren".
                let delimiter = match args.first() {
                    Some(arg) => Some(grammar_scalar_to_string(arg)),
                    None => {
                        let trimmed = span_offsets
                            .map(|(start, end)| {
                                self.trim_leading_trivia(
                                    start + self.source_offset,
                                    end + self.source_offset,
                                )
                            })
                            .unwrap_or(0);
                        match self.source.as_bytes().get(trimmed) {
                            Some(b'(') => Some("paren".to_string()),
                            Some(b'[') => Some("bracket".to_string()),
                            Some(b'{') => Some("brace".to_string()),
                            _ => None,
                        }
                    }
                };
                let items = surface_named_items(&runtime_value, "items")?;
                crate::builtins::surface::form_list(items, span, delimiter)
            }
            _ => {
                let Some(hook) = self.hooks.get(name) else {
                    self.set_error(format!("unknown surface semantic action {name:?}"));
                    return Ok(value);
                };
                let result = self.invoke_custom_hook(name, hook, runtime_value, span)?;
                return Ok(stamp_producing_rule_encoded(result, producing_rule));
            }
        };
        let result = stamp_producing_rule(result, producing_rule);
        runtime_value_to_parse_value(&result)
    }

    fn invoke_custom_hook(
        &self,
        name: &str,
        hook: &RuntimeValue,
        value: RuntimeValue,
        span: SourceSpan,
    ) -> CaapResult<ParseValue> {
        self.trace_hook_call(name, &value, &span);
        let value_summary = runtime_value_summary(&value, 0);
        // The evaluator is borrowed while the hook runs. A hook that re-enters
        // surface parsing on this same driver would alias the borrow; report a
        // clean error rather than panicking inside `RefCell`.
        let mut evaluator = self.evaluator.try_borrow_mut().map_err(|_| {
            CaapError::semantic(format!(
                "surface semantic hook {name:?} re-entered the parser (recursive surface parsing from a hook is not supported)"
            ))
        })?;
        let result = evaluator
            .invoke_callback(
                hook,
                vec![value, crate::builtins::surface::span_to_value(&span)],
            )
            .map_err(|error| {
                let previous = self
                    .error()
                    .map(|error| format!("; previous semantic error: {error}"))
                    .unwrap_or_default();
                CaapError::semantic(format!(
                    "surface semantic hook {name:?} failed for {value_summary}: {error}{previous}"
                ))
            })?;
        runtime_value_to_parse_value(&result)
    }

    fn trace_hook_call(&self, name: &str, value: &RuntimeValue, span: &SourceSpan) {
        if !should_trace_surface_hooks() {
            return;
        }
        let call = self.hook_calls.get().saturating_add(1);
        self.hook_calls.set(call);
        if call <= 32 || call.is_multiple_of(100) || call >= 4000 {
            eprintln!(
                "[caap-trace] surface-hook.call count={call} hook={name} span={}..{}@{}:{} value={}",
                span.start,
                span.end,
                span.start_line,
                span.start_col,
                runtime_value_summary(value, 0)
            );
        }
    }

    /// Advance `start` past leading trivia (whitespace + the active comment
    /// conventions): the engine's match span starts where the EXPRESSION
    /// started, which includes trivia the first token's match skipped —
    /// diagnostics must point at the token, not the gap before it.
    fn trim_leading_trivia(&self, mut start: usize, end: usize) -> usize {
        let bytes = self.source.as_bytes();
        'outer: while start < end {
            if self.trivia.skip_whitespace && matches!(bytes[start], b' ' | b'\t' | b'\r' | b'\n') {
                start += 1;
                continue;
            }
            let rest = &self.source[start..];
            for prefix in &self.trivia.line_comments {
                if !prefix.is_empty() && rest.starts_with(prefix.as_str()) {
                    match rest.find('\n') {
                        Some(nl) => {
                            start += nl + 1;
                            continue 'outer;
                        }
                        None => return end,
                    }
                }
            }
            for (open, close) in &self.trivia.block_comments {
                if rest.starts_with(open.as_str()) {
                    match rest[open.len()..].find(close.as_str()) {
                        Some(at) => {
                            start += open.len() + at + close.len();
                            continue 'outer;
                        }
                        None => return end,
                    }
                }
            }
            break;
        }
        start
    }

    fn span_from_offsets(&self, span: Option<(usize, usize)>) -> Option<SourceSpan> {
        let (start, end) = span?;
        // Engine offsets are parse-text-relative; trim_leading_trivia indexes
        // the FULL source, so shift into its frame first.
        let start = start.checked_add(self.source_offset)?;
        let end = end.checked_add(self.source_offset)?;
        let start = self.trim_leading_trivia(start, end);
        let (start_line, start_col) = line_col_for_offset(self.source, start);
        let (end_line, end_col) = line_col_for_offset(self.source, end);
        SourceSpan::with_locator(
            Some(SourceSpanLocator {
                file_id: None,
                path: self.source_path.clone(),
            }),
            start,
            end,
            start_line,
            start_col,
            end_line,
            end_col,
        )
        .ok()
    }
}

fn runtime_value_summary(value: &RuntimeValue, depth: usize) -> String {
    if depth >= 3 {
        return "...".to_string();
    }
    match value {
        RuntimeValue::Null => "null".to_string(),
        RuntimeValue::Bool(value) => format!("bool({value})"),
        RuntimeValue::Int(value) => format!("int({value})"),
        RuntimeValue::Float(value) => format!("float({value})"),
        RuntimeValue::Bytes(value) => format!("bytes(len={})", value.len()),
        RuntimeValue::Str(value) => {
            let text = value.as_ref();
            let preview: String = text.chars().take(24).collect();
            if text.chars().count() > 24 {
                format!("str({preview}...)")
            } else {
                format!("str({preview})")
            }
        }
        RuntimeValue::Tuple(items) => format!("tuple(len={})", items.len()),
        RuntimeValue::List(items) => format!("list(len={})", items.borrow().len()),
        RuntimeValue::Map(fields) => {
            let fields = fields.borrow();
            let mut keys = fields
                .keys()
                .take(8)
                .map(|key| key.to_string())
                .collect::<Vec<_>>();
            keys.sort();
            let detail = if let Some(item) = fields.get(&MapKey::Str(Rc::from("item"))) {
                format!(", item={}", runtime_value_summary(item, depth + 1))
            } else if let Some(params) = fields.get(&MapKey::Str(Rc::from("params"))) {
                format!(", params={}", runtime_value_summary(params, depth + 1))
            } else {
                String::new()
            };
            format!(
                "map(len={}, keys=[{}]{detail})",
                fields.len(),
                keys.join(", ")
            )
        }
        RuntimeValue::Closure(_) => "closure".to_string(),
        RuntimeValue::Macro(_) => "macro".to_string(),
        RuntimeValue::Builtin(value) => format!("builtin({})", value.name),
        RuntimeValue::HostFunction(value) => format!("host_function({})", value.name),
        RuntimeValue::HostObject(value) => format!("host_object({})", value.type_name()),
        RuntimeValue::Ref(_) => "ref".to_string(),
        RuntimeValue::UninitializedTopLevel => "uninitialized".to_string(),
    }
}

fn should_trace_surface_hooks() -> bool {
    if std::env::var_os("CAAP_LIVE_TRACE").is_none() {
        return false;
    }
    let Some(filter) = std::env::var("CAAP_LIVE_TRACE_FILTER")
        .ok()
        .filter(|filter| !filter.trim().is_empty())
    else {
        return true;
    };
    filter
        .split(',')
        .map(str::trim)
        .any(|needle| matches!(needle, "surface_hook" | "surface" | "hook"))
}

impl ParseDriver for SurfaceBuiltinDriver<'_> {
    fn handle(&self, effect: &ParseEffect<'_>, view: &ParseView<'_>) -> Directive {
        match effect {
            ParseEffect::SemanticAction {
                name, value, args, ..
            } => match self.invoke_builtin_surface_hook(
                name,
                (*value).clone(),
                view.span,
                view.matched_text,
                args,
                view.rule_stack.last().copied(),
            ) {
                Ok(transformed) => Directive::Accept(transformed),
                Err(error) => {
                    self.set_error(error);
                    Directive::Accept((*value).clone())
                }
            },
            _ => Directive::Proceed,
        }
    }
}

/// Stamp the PRODUCING RULE's name into a constructed surface-form map: the
/// `rule` field is the grammar rule that emitted the form (the hook kind used
/// to leak here instead). No-op for non-form results.
fn stamp_producing_rule(value: RuntimeValue, producing_rule: Option<&str>) -> RuntimeValue {
    let Some(rule) = producing_rule else {
        return value;
    };
    if let RuntimeValue::Map(fields) = &value {
        let mut fields = fields.borrow_mut();
        if fields.contains_key(&MapKey::Str(intern_string("kind"))) {
            fields.insert(
                MapKey::Str(intern_string("rule")),
                RuntimeValue::Str(Rc::from(rule)),
            );
        }
    }
    value
}

/// The encoded-ParseValue twin of [`stamp_producing_rule`] for custom hooks
/// (their result is already encoded as `__caap_rt_surface_form`, where the
/// rule lives at child index 3).
fn stamp_producing_rule_encoded(value: ParseValue, producing_rule: Option<&str>) -> ParseValue {
    let Some(rule) = producing_rule else {
        return value;
    };
    match value {
        ParseValue::Node(kind, children) if &*kind == "__caap_rt_surface_form" => {
            let mut children = children.as_ref().clone();
            if children.len() > 3 {
                children[3] = ParseValue::Text(rule.into());
            }
            ParseValue::Node(kind, std::sync::Arc::new(children))
        }
        other => other,
    }
}

fn surface_named_items(value: &RuntimeValue, key: &str) -> CaapResult<Vec<RuntimeValue>> {
    let RuntimeValue::Map(fields) = value else {
        return Ok(Vec::new());
    };
    let Some(items) = fields.borrow().get(&MapKey::Str(Rc::from(key))).cloned() else {
        return Ok(Vec::new());
    };
    match items {
        RuntimeValue::Null => Ok(Vec::new()),
        RuntimeValue::Tuple(items) => Ok(items.iter().cloned().collect()),
        RuntimeValue::List(items) => Ok(items.borrow().clone()),
        item => Ok(vec![item]),
    }
}

fn grammar_scalar_to_string(value: &GrammarScalar) -> String {
    match value {
        GrammarScalar::Str(value) => value.clone(),
        GrammarScalar::Int(value) => value.to_string(),
        GrammarScalar::Float(value) => value.to_string(),
        GrammarScalar::Bool(value) => value.to_string(),
        GrammarScalar::Null => "null".to_string(),
    }
}

fn line_col_for_offset(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (index, ch) in source.char_indices() {
        if index >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn semantic_list_to_json(items: &[SemanticValue]) -> CaapResult<Value> {
    let Some(SemanticValue::Str(tag)) = items.first() else {
        return items
            .iter()
            .map(semantic_value_to_json)
            .collect::<CaapResult<Vec<_>>>()
            .map(Value::Array);
    };
    match tag.as_str() {
        "seq" | "choice" => {
            let children = items[1..]
                .iter()
                .map(semantic_value_to_json)
                .collect::<CaapResult<Vec<_>>>()?;
            Ok(json!([tag, children]))
        }
        _ => items
            .iter()
            .map(semantic_value_to_json)
            .collect::<CaapResult<Vec<_>>>()
            .map(Value::Array),
    }
}

fn base_surface_rules() -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("forms".to_string(), json!(["many", ["ref", "form"]])),
        (
            "form".to_string(),
            json!([
                "choice",
                [
                    ["ref", "list"],
                    ["ref", "string"],
                    ["ref", "integer"],
                    ["ref", "boolean"],
                    ["ref", "null"],
                    ["ref", "symbol"]
                ]
            ]),
        ),
        (
            "list".to_string(),
            json!([
                "seq",
                [
                    ["literal", "("],
                    ["named", "items", ["many", ["ref", "form"]]],
                    ["literal", ")"]
                ]
            ]),
        ),
        (
            "string".to_string(),
            json!(["regex", r#""(?:[^"\\]|\\.)*""#]),
        ),
        (
            "integer".to_string(),
            json!(["regex", r#"-?(?:0|[1-9][0-9]*)"#]),
        ),
        (
            "boolean".to_string(),
            json!(["choice", [["literal", "true"], ["literal", "false"]]]),
        ),
        ("null".to_string(), json!(["literal", "null"])),
        (
            "symbol".to_string(),
            json!([
                "regex",
                r#"[A-Za-z_+\-*\/<>=!?$%&:.][A-Za-z0-9_+\-*\/<>=!?$%&:.]*"#
            ]),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use caap_peg::{PEGParser, ParseValue, ParserConfig};

    use super::*;
    use crate::syntax_authoring::apply_authoring_grammar_source;

    #[test]
    fn compiles_base_surface_grammar_from_empty_syntax_state() {
        let syntax = UnitSyntaxState::new("caap").unwrap();
        let grammar = compile_surface_grammar_from_syntax_state(&syntax).unwrap();

        assert_eq!(grammar.start_rule, "forms");
        assert!(grammar.get_rule("form").is_some());
        assert!(grammar.get_rule("integer").is_some());
    }

    #[test]
    fn syntax_state_rules_override_base_rules_and_compile_many_literal_aliases() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(
            &mut syntax,
            r#"
add rule bracket = "[" items:integer* "]"
replace rule form = bracket | integer
"#,
        )
        .unwrap();

        let grammar = compile_surface_grammar_from_syntax_state(&syntax).unwrap();
        assert!(grammar.get_rule("bracket").is_some());

        let parser = PEGParser;
        let parsed = parser
            .parse(&grammar, "[1 2 3]", &ParserConfig::default())
            .unwrap();
        assert!(!matches!(parsed, ParseValue::Nil));
    }

    #[test]
    fn carries_rule_and_grammar_metadata() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(&mut syntax, r#"add rule demo = symbol -> surface.symbol"#)
            .unwrap();
        syntax
            .set_grammar_metadata(
                "semantic_hook_functions",
                SemanticValue::Map(vec![(
                    "surface.symbol".to_string(),
                    SemanticValue::Str("surface_symbol".to_string()),
                )]),
            )
            .unwrap();

        let grammar = compile_surface_grammar_from_syntax_state(&syntax).unwrap();
        assert!(grammar
            .metadata
            .get("demo")
            .and_then(|metadata| metadata.get("semantic_hooks"))
            .is_some());
        assert!(grammar
            .metadata
            .get("__grammar__")
            .and_then(|metadata| metadata.get("semantic_hook_functions"))
            .is_some());
    }

    #[test]
    fn runtime_parse_value_bridge_roundtrips_structured_values() {
        let value = RuntimeValue::Map(Rc::new(RefCell::new(IndexMap::from([
            (
                MapKey::Str(Rc::from("name")),
                RuntimeValue::Str(Rc::from("demo")),
            ),
            (
                MapKey::Str(Rc::from("items")),
                RuntimeValue::List(Rc::new(RefCell::new(vec![
                    RuntimeValue::Int(1),
                    RuntimeValue::Bool(true),
                ]))),
            ),
        ]))));

        let encoded = runtime_value_to_parse_value(&value).unwrap();
        let decoded = parse_value_to_runtime_value(&encoded).unwrap();

        let RuntimeValue::Map(fields) = decoded else {
            panic!("expected decoded map");
        };
        assert_eq!(
            fields.borrow().get(&MapKey::Str(Rc::from("name"))),
            Some(&RuntimeValue::Str(Rc::from("demo")))
        );
        assert!(matches!(
            fields.borrow().get(&MapKey::Str(Rc::from("items"))),
            Some(RuntimeValue::List(_))
        ));
    }

    #[test]
    fn runtime_parse_value_bridge_orders_map_entries_deterministically() {
        let value = RuntimeValue::Map(Rc::new(RefCell::new(IndexMap::from([
            (
                MapKey::Str(Rc::from("z")),
                RuntimeValue::Str(Rc::from("last")),
            ),
            (MapKey::Null, RuntimeValue::Str(Rc::from("null"))),
            (MapKey::Int(2), RuntimeValue::Str(Rc::from("int"))),
            (MapKey::Bool(false), RuntimeValue::Str(Rc::from("bool"))),
            (
                MapKey::Str(Rc::from("a")),
                RuntimeValue::Str(Rc::from("first")),
            ),
        ]))));

        let encoded = runtime_value_to_parse_value(&value).unwrap();
        let ParseValue::Node(kind, pairs) = encoded else {
            panic!("expected encoded map node");
        };
        assert_eq!(&*kind, "__caap_rt_map");
        let keys: Vec<ParseValue> = pairs
            .iter()
            .map(|pair| {
                let ParseValue::Node(kind, children) = pair else {
                    panic!("expected encoded pair");
                };
                assert_eq!(&**kind, "__caap_rt_pair");
                children[0].clone()
            })
            .collect();

        // Deterministic = INSERTION order (the runtime map is an IndexMap):
        // entries come back exactly as constructed, not key-sorted.
        assert_eq!(
            keys,
            vec![
                ParseValue::Text("z".into()),
                ParseValue::Node("__caap_rt_null".into(), Arc::new(Vec::new())),
                ParseValue::Number(2),
                ParseValue::Node(
                    "__caap_rt_bool".into(),
                    Arc::new(vec![ParseValue::Text("false".into())])
                ),
                ParseValue::Text("a".into()),
            ]
        );
    }

    #[test]
    fn named_parse_bindings_project_to_runtime_map() {
        let value = ParseValue::Node(
            "pair".into(),
            Arc::new(vec![
                ParseValue::Named(Arc::from("left"), Arc::new(ParseValue::Text("x".into()))),
                ParseValue::Named(Arc::from("right"), Arc::new(ParseValue::Number(42))),
            ]),
        );

        let fields = named_parse_bindings_to_runtime_map(&value).unwrap();
        assert_eq!(
            fields.get(&MapKey::Str(Rc::from("left"))),
            Some(&RuntimeValue::Str(Rc::from("x")))
        );
        assert_eq!(
            fields.get(&MapKey::Str(Rc::from("right"))),
            Some(&RuntimeValue::Int(42))
        );
    }

    #[test]
    fn builtin_surface_runtime_lowers_integer_transform() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(
            &mut syntax,
            r#"replace rule form = integer -> surface.integer"#,
        )
        .unwrap();
        let grammar = compile_surface_grammar_from_syntax_state(&syntax)
            .unwrap()
            .with_start_rule("form");
        let runtime = SurfaceBuiltinDriver::new("42", None);

        let parsed = caap_peg::ParseRequest::new(&grammar)
            .driver(&runtime)
            .run("42")
            .unwrap();
        assert_eq!(runtime.error(), None);
        let RuntimeValue::Map(fields) = parse_value_to_runtime_value(&parsed).unwrap() else {
            panic!("expected surface form map");
        };
        assert_eq!(
            fields.borrow().get(&MapKey::Str(Rc::from("kind"))),
            Some(&RuntimeValue::Str(Rc::from("integer")))
        );
        assert_eq!(
            fields.borrow().get(&MapKey::Str(Rc::from("value"))),
            Some(&RuntimeValue::Int(42))
        );
    }

    #[test]
    fn builtin_surface_runtime_rejects_named_capture_for_scalar_surface_hook() {
        let runtime = SurfaceBuiltinDriver::new("42", None);
        let value = ParseValue::Named(Arc::from("value"), Arc::new(ParseValue::Text("42".into())));

        let error = runtime
            .invoke_builtin_surface_hook("surface.symbol", value, Some((0, 2)), "42", &[], None)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("surface.symbol expects parsed text"),
            "{error}"
        );
    }

    #[test]
    fn builtin_surface_runtime_preserves_full_source_offsets() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(
            &mut syntax,
            r#"replace rule form = integer -> surface.integer"#,
        )
        .unwrap();
        let grammar = compile_surface_grammar_from_syntax_state(&syntax)
            .unwrap()
            .with_start_rule("form");
        let source = "\n\n42\n";
        let parse_source = source.trim();
        let source_offset = source.find(parse_source).unwrap();
        let runtime = SurfaceBuiltinDriver::new(source, None).with_source_offset(source_offset);

        let parsed = caap_peg::ParseRequest::new(&grammar)
            .driver(&runtime)
            .run(parse_source)
            .unwrap();
        assert_eq!(runtime.error(), None);
        let RuntimeValue::Map(fields) = parse_value_to_runtime_value(&parsed).unwrap() else {
            panic!("expected surface form map");
        };
        let RuntimeValue::Map(span) = fields
            .borrow()
            .get(&MapKey::Str(Rc::from("span")))
            .cloned()
            .unwrap()
        else {
            panic!("expected span map");
        };
        let span = span.borrow();
        assert_eq!(
            span.get(&MapKey::Str(Rc::from("start"))),
            Some(&RuntimeValue::Int(2))
        );
        assert_eq!(
            span.get(&MapKey::Str(Rc::from("end"))),
            Some(&RuntimeValue::Int(4))
        );
        assert_eq!(
            span.get(&MapKey::Str(Rc::from("start_line"))),
            Some(&RuntimeValue::Int(3))
        );
        assert_eq!(
            span.get(&MapKey::Str(Rc::from("start_col"))),
            Some(&RuntimeValue::Int(1))
        );
    }

    #[test]
    fn builtin_surface_runtime_uses_named_list_items() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(
            &mut syntax,
            r#"replace rule form = list -> surface.list | integer -> surface.integer"#,
        )
        .unwrap();
        let grammar = compile_surface_grammar_from_syntax_state(&syntax)
            .unwrap()
            .with_start_rule("form");
        let runtime = SurfaceBuiltinDriver::new("(1)", None);

        let parsed = caap_peg::ParseRequest::new(&grammar)
            .driver(&runtime)
            .run("(1)")
            .unwrap();
        assert_eq!(runtime.error(), None);
        let RuntimeValue::Map(fields) = parse_value_to_runtime_value(&parsed).unwrap() else {
            panic!("expected surface form map");
        };
        assert_eq!(
            fields.borrow().get(&MapKey::Str(Rc::from("kind"))),
            Some(&RuntimeValue::Str(Rc::from("list")))
        );
        assert!(matches!(
            fields.borrow().get(&MapKey::Str(Rc::from("items"))),
            Some(RuntimeValue::Tuple(items)) if items.len() == 1
        ));
    }

    #[test]
    fn parse_value_to_parsed_source_decodes_surface_forms() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(
            &mut syntax,
            r#"replace rule form = list -> surface.list | integer -> surface.integer"#,
        )
        .unwrap();
        let grammar = compile_surface_grammar_from_syntax_state(&syntax).unwrap();
        let runtime = SurfaceBuiltinDriver::new("(1 2)", None);
        let parsed = caap_peg::ParseRequest::new(&grammar)
            .driver(&runtime)
            .run("(1 2)")
            .unwrap();

        let source = parse_value_to_parsed_source(&parsed).unwrap();
        assert_eq!(source.forms.len(), 1);
        let ParsedForm::List { items, .. } = &source.forms[0] else {
            panic!("expected list form");
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], ParsedForm::Integer { value: 1, .. }));
        assert!(matches!(&items[1], ParsedForm::Integer { value: 2, .. }));
    }

    #[test]
    fn runtime_surface_form_decode_rejects_non_string_symbol_payload() {
        let span =
            crate::builtins::surface::span_to_value(&SourceSpan::new(0, 1, 1, 1, 1, 2).unwrap());
        let form = RuntimeValue::Map(Rc::new(RefCell::new(IndexMap::from([
            (
                MapKey::Str(Rc::from("kind")),
                RuntimeValue::Str(Rc::from("symbol")),
            ),
            (MapKey::Str(Rc::from("value")), RuntimeValue::Int(42)),
            (MapKey::Str(Rc::from("span")), span),
        ]))));

        let error = runtime_surface_form_to_parsed_form(&form)
            .unwrap_err()
            .to_string();

        assert!(error.contains("surface form missing string field \"value\""));
    }

    #[test]
    fn runtime_surface_form_decode_rejects_string_integer_payload() {
        let span =
            crate::builtins::surface::span_to_value(&SourceSpan::new(0, 2, 1, 1, 1, 3).unwrap());
        let form = RuntimeValue::Map(Rc::new(RefCell::new(IndexMap::from([
            (
                MapKey::Str(Rc::from("kind")),
                RuntimeValue::Str(Rc::from("integer")),
            ),
            (
                MapKey::Str(Rc::from("value")),
                RuntimeValue::Str(Rc::from("42")),
            ),
            (MapKey::Str(Rc::from("span")), span),
        ]))));

        let error = runtime_surface_form_to_parsed_form(&form)
            .unwrap_err()
            .to_string();

        assert!(error.contains("surface form missing integer field \"value\""));
    }

    #[test]
    fn runtime_surface_form_decode_rejects_negative_span_offsets() {
        let span = RuntimeValue::Map(Rc::new(RefCell::new(IndexMap::from([
            (MapKey::Str(Rc::from("start")), RuntimeValue::Int(-1)),
            (MapKey::Str(Rc::from("end")), RuntimeValue::Int(1)),
            (MapKey::Str(Rc::from("start_line")), RuntimeValue::Int(1)),
            (MapKey::Str(Rc::from("start_col")), RuntimeValue::Int(1)),
            (MapKey::Str(Rc::from("end_line")), RuntimeValue::Int(1)),
            (MapKey::Str(Rc::from("end_col")), RuntimeValue::Int(2)),
        ]))));
        let form = RuntimeValue::Map(Rc::new(RefCell::new(IndexMap::from([
            (
                MapKey::Str(Rc::from("kind")),
                RuntimeValue::Str(Rc::from("symbol")),
            ),
            (
                MapKey::Str(Rc::from("value")),
                RuntimeValue::Str(Rc::from("x")),
            ),
            (MapKey::Str(Rc::from("span")), span),
        ]))));

        let error = runtime_surface_form_to_parsed_form(&form)
            .unwrap_err()
            .to_string();

        assert!(error.contains("surface span missing non-negative integer field \"start\""));
    }

    #[test]
    fn parses_c_like_empty_map_value_declaration_prefix() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(
            &mut syntax,
            r#"
add rule c_ident = /[A-Za-z_][A-Za-z0-9_.-]*/
add rule c_expr = c_cond_expr
add rule c_cond_expr = c_or_expr
add rule c_or_expr = c_and_expr
add rule c_and_expr = c_eq_expr
add rule c_eq_expr = c_cmp_expr
add rule c_cmp_expr = c_add_expr
add rule c_add_expr = c_unary_expr
add rule c_unary_expr = c_primary_expr
add rule c_primary_expr = c_map_expr | c_ident_expr
add rule c_ident_expr = name:c_ident
add rule c_map_expr = "{" pairs:c_map_pair_list? "}"
add rule c_map_pair = key:c_ident ":" value:c_expr
add rule c_map_pair_tail = "," pair:c_map_pair
add rule c_map_pair_list = first:c_map_pair rest:c_map_pair_tail*
add rule c_value_form = "auto" name:c_ident "=" value:c_expr ";"
replace rule form = c_value_form
"#,
        )
        .unwrap();
        let grammar = compile_surface_grammar_from_syntax_state(&syntax).unwrap();
        PEGParser
            .parse(
                &grammar,
                "auto seen_diagnostics = {};",
                &ParserConfig::default().with_max_steps(4096),
            )
            .unwrap();
    }

    fn spec_trivia(spec: &Value) -> Option<String> {
        spec.as_array()?.iter().find_map(|entry| {
            let entry = entry.as_array()?;
            if entry.len() == 3
                && entry[0] == json!("grammar_metadata")
                && entry[1] == json!("trivia")
            {
                entry[2].as_str().map(str::to_string)
            } else {
                None
            }
        })
    }

    #[test]
    fn grammar_using_semicolon_token_defaults_to_whitespace_trivia() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(
            &mut syntax,
            "add rule c_ident = /[A-Za-z_]+/\nadd rule stmt = name:c_ident \";\"\nreplace rule form = stmt\n",
        )
        .unwrap();
        let spec = surface_grammar_spec_from_syntax_state(&syntax).unwrap();
        assert_eq!(spec_trivia(&spec).as_deref(), Some("whitespace"));
    }

    #[test]
    fn grammar_without_comment_marker_tokens_keeps_default_trivia() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(
            &mut syntax,
            "add rule word = /[A-Za-z]+/\nreplace rule form = word\n",
        )
        .unwrap();
        let spec = surface_grammar_spec_from_syntax_state(&syntax).unwrap();
        assert_eq!(spec_trivia(&spec).as_deref(), Some("default"));
    }

    #[test]
    fn explicit_trivia_metadata_overrides_comment_marker_autodetect() {
        let mut syntax = UnitSyntaxState::new("demo").unwrap();
        apply_authoring_grammar_source(
            &mut syntax,
            "add rule c_ident = /[A-Za-z_]+/\nadd rule stmt = name:c_ident \";\"\nreplace rule form = stmt\n",
        )
        .unwrap();
        syntax
            .set_grammar_metadata("trivia", SemanticValue::Str("default".to_string()))
            .unwrap();
        let spec = surface_grammar_spec_from_syntax_state(&syntax).unwrap();
        assert_eq!(spec_trivia(&spec).as_deref(), Some("default"));
    }
}
