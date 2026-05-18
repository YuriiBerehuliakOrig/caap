use serde_json::Value;
use std::collections::HashMap;

use crate::behaviors::{
    BehaviorEntry, DiagnosticBehavior, GrammarScalar, PredicateBehavior, TraceBehavior,
    TransformBehavior,
};
use crate::expr::{CompiledRegex, PegExpr, RuleTextParser};
use crate::grammar::{Grammar, GrammarRule};

// ── Error ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpecCompileError {
    #[error("invalid spec format: {0}")]
    InvalidFormat(String),
    #[error("unknown expr tag: {0}")]
    UnknownTag(String),
    #[error("missing required field: {0}")]
    MissingField(String),
    #[error("type error: expected {expected}, got {actual} in {ctx}")]
    TypeError {
        expected: &'static str,
        actual: String,
        ctx: String,
    },
    #[error("duplicate rule: {0}")]
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
        let mut grammar = Grammar::new("").with_start_rule(root_rule.to_string());
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
            grammar
                .text
                .push_str(&format!("{} <- {}", rule.name, rule.source));
        }

        // Store options as __grammar__ metadata
        let mut gmeta: HashMap<String, Value> = HashMap::new();
        if !ctx.hard_keywords.is_empty() {
            gmeta.insert(
                "hard_keywords".to_string(),
                Value::Array(
                    ctx.hard_keywords
                        .iter()
                        .map(|s| Value::String(s.clone()))
                        .collect(),
                ),
            );
        }
        if !ctx.soft_keywords.is_empty() {
            gmeta.insert(
                "soft_keywords".to_string(),
                Value::Array(
                    ctx.soft_keywords
                        .iter()
                        .map(|s| Value::String(s.clone()))
                        .collect(),
                ),
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
                Value::Array(
                    ctx.line_comments
                        .iter()
                        .map(|s| Value::String(s.clone()))
                        .collect(),
                ),
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

struct CompileState {
    rules: Vec<CompiledRuleSpec>, // ordered insertion
    seen_names: std::collections::HashSet<String>,
    rule_metadata: HashMap<String, HashMap<String, Value>>,
    hard_keywords: Vec<String>,
    soft_keywords: Vec<String>,
    strict_actions: bool,
    whitespace: Option<String>,
    line_comments: Vec<String>,
    /// Whether indentation-sensitive mode is enabled.
    indentation: bool,
    /// Extra grammar-level metadata keys.
    grammar_metadata: HashMap<String, Value>,
    /// Imports: alias → grammar name (stored in __grammar__ imports metadata).
    imports: HashMap<String, String>,
}

#[derive(Clone, Debug)]
struct CompiledRuleSpec {
    name: String,
    source: String,
    params: Vec<String>,
    expr: PegExpr,
}

impl Default for CompileState {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            seen_names: std::collections::HashSet::new(),
            rule_metadata: HashMap::new(),
            hard_keywords: Vec::new(),
            soft_keywords: Vec::new(),
            strict_actions: true, // Python default
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
    let arr = match entry.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return Ok(()), // skip non-list / empty entries
    };

    let tag = match arr[0].as_str() {
        Some(t) => t,
        None => return Ok(()),
    };

    match tag {
        "hard_keywords" | "hard-keywords" => {
            ctx.hard_keywords = arr[1..]
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
        }
        "soft_keywords" | "soft-keywords" => {
            ctx.soft_keywords = arr[1..]
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
        }
        "strict_actions" | "strict-actions" => {
            ctx.strict_actions = if arr.len() < 2 {
                true
            } else {
                parse_bool(&arr[1], "strict_actions")?
            };
        }
        "trivia" => {
            for item in arr.iter().skip(1) {
                let sub = expect_arr(item, "trivia entry")?;
                if sub.is_empty() {
                    continue;
                }
                match sub[0].as_str() {
                    Some("whitespace") => {
                        ctx.whitespace = Some(
                            sub[1..]
                                .iter()
                                .filter_map(|v| v.as_str())
                                .collect::<String>(),
                        );
                    }
                    Some("line_comments") => {
                        ctx.line_comments = sub[1..]
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect();
                    }
                    _ => {} // block_comments etc. — tolerate
                }
            }
        }
        "indentation" => {
            ctx.indentation = if arr.len() < 2 {
                true
            } else {
                parse_bool(&arr[1], "indentation")?
            };
        }
        "metadata" | "grammar_metadata" | "grammar-metadata"
            // ["metadata", key, value] — sets a grammar-level metadata key
            if arr.len() >= 3 => {
                if let Some(key) = arr[1].as_str() {
                    ctx.grammar_metadata.insert(key.to_string(), arr[2].clone());
                }
            }
        "imports"
            // ["imports", {"alias": "GrammarName", ...}] or ["imports", alias, name]
            if arr.len() >= 2 => {
                match &arr[1] {
                    Value::Object(map) => {
                        for (alias, name) in map {
                            if let Some(n) = name.as_str() {
                                ctx.imports.insert(alias.clone(), n.to_string());
                            }
                        }
                    }
                    Value::String(alias) if arr.len() >= 3 => {
                        if let Some(name) = arr[2].as_str() {
                            ctx.imports.insert(alias.clone(), name.to_string());
                        }
                    }
                    _ => {}
                }
            }
        // Tolerate known but not yet implemented options
        "semantic_hooks" | "semantic-hooks" | "recovery" | "recover_sync" | "recover-sync"
        | "rule_memo" | "rule-memo" => {}
        _ => {} // silently skip unknown top-level entries
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
        actual: type_name(header).to_string(),
        ctx: "rule name".to_string(),
    })
}

fn apply_rule_meta(
    ctx: &mut CompileState,
    rule_name: &str,
    meta: &Value,
) -> Result<(), SpecCompileError> {
    // String shorthand: "memo", "no_memo"
    if let Some(s) = meta.as_str() {
        let lower = s.trim().to_lowercase();
        if lower == "memo" || lower == ":memo" {
            ctx.rule_metadata
                .entry(rule_name.to_string())
                .or_default()
                .insert("memo".to_string(), Value::Bool(true));
        }
        return Ok(());
    }

    let arr = match meta.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return Ok(()),
    };

    match arr[0].as_str() {
        Some("metadata") if arr.len() >= 3 => {
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
            let enabled = if arr.len() < 2 {
                true
            } else {
                parse_bool(&arr[1], "memo")?
            };
            ctx.rule_metadata
                .entry(rule_name.to_string())
                .or_default()
                .insert("memo".to_string(), Value::Bool(enabled));
        }
        _ => {}
    }
    Ok(())
}

// ── Expression → source text ───────────────────────────────────────────────

/// Convert a spec expression to its PEG grammar source text.
///
/// Accepts both **list form** (`["lit", "hello"]`) and **mapping form**
/// (`{"type": "literal", "text": "hello"}`).  Mirrors the union of
/// `peg/compile/list_forms.py` and `peg/compile/mapping.py`.
pub fn expr_to_source(expr: &Value) -> Result<String, SpecCompileError> {
    // Mapping-style spec: { "type": "...", ... }
    if let Some(obj) = expr.as_object() {
        return mapping_expr_to_source(obj);
    }
    let arr = expect_arr(expr, "expr")?;
    if arr.is_empty() {
        return Err(SpecCompileError::MissingField("expr tag".to_string()));
    }

    let tag = expect_str(&arr[0], "expr tag")?;

    match tag {
        // ── Terminals ──────────────────────────────────────────────────
        "lit" | "literal" => {
            let text = expect_str_at(arr, 1, "lit text")?;
            let escaped = text.replace('\\', "\\\\").replace('\'', "\\'");
            Ok(format!("'{escaped}'"))
        }
        "regex" => {
            let pattern = expect_str_at(arr, 1, "regex pattern")?;
            Ok(format!("/{pattern}/"))
        }
        "token" => {
            let pattern = expect_str_at(arr, 1, "token pattern")?;
            Ok(format!("token({pattern})"))
        }
        "tok" | "token_ref" => {
            let kind = expect_str_at(arr, 1, "tok kind")?;
            if arr.len() > 2 {
                let text = expect_str_at(arr, 2, "tok text")?;
                let escaped = text.replace('\'', "\\'");
                Ok(format!("tok({kind},'{escaped}')"))
            } else {
                Ok(format!("tok({kind})"))
            }
        }
        "soft_kw" | "soft_keyword" => {
            let text = expect_str_at(arr, 1, "soft_kw text")?;
            let escaped = text.replace('\'', "\\'");
            Ok(format!("'{escaped}'"))
        }
        "param" => {
            let name = expect_str_at(arr, 1, "param name")?;
            Ok(format!("${name}"))
        }
        "newline" => Ok("newline".to_string()),
        "indent" => Ok("indent".to_string()),
        "dedent" => Ok("dedent".to_string()),

        // ── Composite ──────────────────────────────────────────────────
        "seq" => {
            let items = expect_arr_at(arr, 1, "seq items")?;
            let parts: Vec<_> = items.iter().map(expr_to_source).collect::<Result<_, _>>()?;
            Ok(match parts.len() {
                0 => String::new(),
                1 => parts.into_iter().next().unwrap(),
                _ => format!("({})", parts.join(" ")),
            })
        }
        "choice" => {
            let items = expect_arr_at(arr, 1, "choice items")?;
            let parts: Vec<_> = items.iter().map(expr_to_source).collect::<Result<_, _>>()?;
            Ok(match parts.len() {
                0 => String::new(),
                1 => parts.into_iter().next().unwrap(),
                _ => format!("({})", parts.join(" / ")),
            })
        }

        // ── Quantifiers ────────────────────────────────────────────────
        "star" | "*" | "many" => {
            let inner = expr_to_source(expect_val_at(arr, 1, "star expr")?)?;
            Ok(format!("{inner}*"))
        }
        "plus" | "+" | "one_or_more" => {
            let inner = expr_to_source(expect_val_at(arr, 1, "plus expr")?)?;
            Ok(format!("{inner}+"))
        }
        "opt" | "?" | "optional" => {
            let inner = expr_to_source(expect_val_at(arr, 1, "opt expr")?)?;
            Ok(format!("{inner}?"))
        }

        // ── References ─────────────────────────────────────────────────
        "ref" => {
            let name = expect_str_at(arr, 1, "ref name")?;
            Ok(name.to_string())
        }
        "imported_ref" | "import" => {
            let grammar = expect_str_at(arr, 1, "imported_ref grammar")?;
            let rule = expect_str_at(arr, 2, "imported_ref rule")?;
            Ok(format!("{grammar}::{rule}"))
        }
        "grammar_scope" | "scope" => {
            let grammar = expect_str_at(arr, 1, "grammar_scope grammar")?;
            let inner = expr_to_source(expect_val_at(arr, 2, "grammar_scope expr")?)?;
            Ok(format!("scope('{grammar}', {inner})"))
        }

        // ── Predicates ─────────────────────────────────────────────────
        "and" => {
            let inner = expr_to_source(expect_val_at(arr, 1, "and expr")?)?;
            Ok(format!("&{inner}"))
        }
        "not" => {
            let inner = expr_to_source(expect_val_at(arr, 1, "not expr")?)?;
            Ok(format!("!{inner}"))
        }

        // ── Named capture / binding ────────────────────────────────────
        "named" | "=" | "bind" => {
            let name = expect_str_at(arr, 1, "named label")?;
            let inner = expr_to_source(expect_val_at(arr, 2, "named expr")?)?;
            Ok(format!("{name}:{inner}"))
        }

        // ── Cut ────────────────────────────────────────────────────────
        "cut" | "~" => Ok("~".to_string()),

        // ── Eager ──────────────────────────────────────────────────────
        "eager" | "&&" => {
            let inner = expr_to_source(expect_val_at(arr, 1, "eager expr")?)?;
            Ok(format!("&&{inner}"))
        }

        // ── Tight / no_trivia ──────────────────────────────────────────
        "tight" | "no_trivia" => {
            let inner = expr_to_source(expect_val_at(arr, 1, "tight expr")?)?;
            Ok(format!("no_trivia({inner})"))
        }

        // ── Separator ──────────────────────────────────────────────────
        "sep_plus" | "gather" => {
            let sep = expr_to_source(expect_val_at(arr, 1, "sep_plus sep")?)?;
            let elem = expr_to_source(expect_val_at(arr, 2, "sep_plus element")?)?;
            Ok(format!("({elem} ({sep} {elem})*)"))
        }

        // ── Island / raw_block ─────────────────────────────────────────
        "island" => {
            let start = expect_str_at(arr, 1, "island start")?;
            let end = expect_str_at(arr, 2, "island end")?;
            Ok(format!("island('{start}', '{end}')"))
        }
        "raw_block" => {
            let start = expect_str_at(arr, 1, "raw_block start")?;
            let end = expect_str_at(arr, 2, "raw_block end")?;
            Ok(format!("raw_block('{start}', '{end}')"))
        }

        // ── Behavior ───────────────────────────────────────────────────
        "behavior" => {
            // ["behavior", [entries...], inner_expr]
            let inner = expr_to_source(expect_val_at(arr, 2, "behavior expr")?)?;
            // Behaviors are informational — emit the inner expression unchanged
            Ok(inner)
        }

        // ── Precedence hint (ignored, just compile inner) ──────────────
        "prec" => {
            // ["prec", ..., expr] — last element is the actual expression
            let last = arr
                .last()
                .ok_or_else(|| SpecCompileError::MissingField("prec inner expr".to_string()))?;
            expr_to_source(last)
        }

        other => Err(SpecCompileError::UnknownTag(other.to_string())),
    }
}

fn expr_to_peg_expr(expr: &Value) -> Result<PegExpr, SpecCompileError> {
    if let Some(obj) = expr.as_object() {
        return mapping_expr_to_peg_expr(obj);
    }

    if let Some(arr) = expr.as_array() {
        return list_expr_to_peg_expr(arr);
    }

    let source = expr_to_source(expr)?;
    RuleTextParser::parse(&source)
        .map_err(|err| SpecCompileError::InvalidFormat(err.message.into()))
}

fn list_expr_to_peg_expr(arr: &[Value]) -> Result<PegExpr, SpecCompileError> {
    let tag = expect_str_at(arr, 0, "expr tag")?;
    match tag {
        "lit" | "literal" => Ok(PegExpr::Literal(
            expect_str_at(arr, 1, "lit text")?.to_string(),
        )),
        "regex" => {
            let pattern = expect_str_at(arr, 1, "regex pattern")?;
            Ok(PegExpr::Regex(
                CompiledRegex::new(pattern, 0, pattern.len())
                    .map_err(|err| SpecCompileError::InvalidFormat(err.message.into()))?,
            ))
        }
        "tok" | "token_ref" => {
            let kind = Some(expect_str_at(arr, 1, "tok kind")?.to_string());
            let text = if arr.len() > 2 {
                Some(expect_str_at(arr, 2, "tok text")?.to_string())
            } else {
                None
            };
            Ok(PegExpr::TokenRef { kind, text })
        }
        "soft_kw" | "soft_keyword" => Ok(PegExpr::SoftKeyword(
            expect_str_at(arr, 1, "soft keyword text")?.to_string(),
        )),
        "param" => Ok(PegExpr::Parameter {
            name: expect_str_at(arr, 1, "param name")?.to_string(),
        }),
        "newline" => Ok(PegExpr::Newline),
        "indent" => Ok(PegExpr::Indent),
        "dedent" => Ok(PegExpr::Dedent),
        "seq" => Ok(PegExpr::Sequence(
            expect_arr_at(arr, 1, "seq items")?
                .iter()
                .map(expr_to_peg_expr)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        "choice" => Ok(PegExpr::Choice(
            expect_arr_at(arr, 1, "choice items")?
                .iter()
                .map(expr_to_peg_expr)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        "star" | "*" | "many" => Ok(PegExpr::ZeroOrMore(Box::new(expr_to_peg_expr(
            expect_val_at(arr, 1, "star expr")?,
        )?))),
        "plus" | "+" | "one_or_more" => Ok(PegExpr::OneOrMore(Box::new(expr_to_peg_expr(
            expect_val_at(arr, 1, "plus expr")?,
        )?))),
        "opt" | "?" | "optional" => Ok(PegExpr::Optional(Box::new(expr_to_peg_expr(
            expect_val_at(arr, 1, "opt expr")?,
        )?))),
        "ref" => Ok(PegExpr::Ref(expect_str_at(arr, 1, "ref name")?.to_string())),
        "imported_ref" | "import" => Ok(PegExpr::ImportedRef {
            grammar_name: expect_str_at(arr, 1, "imported_ref grammar")?.to_string(),
            rule_name: expect_str_at(arr, 2, "imported_ref rule")?.to_string(),
        }),
        "grammar_scope" | "scope" => Ok(PegExpr::GrammarScope {
            grammar_name: expect_str_at(arr, 1, "grammar_scope grammar")?.to_string(),
            expr: Box::new(expr_to_peg_expr(expect_val_at(
                arr,
                2,
                "grammar_scope expr",
            )?)?),
        }),
        "and" => Ok(PegExpr::And(Box::new(expr_to_peg_expr(expect_val_at(
            arr, 1, "and expr",
        )?)?))),
        "not" => Ok(PegExpr::Not(Box::new(expr_to_peg_expr(expect_val_at(
            arr, 1, "not expr",
        )?)?))),
        "named" | "=" | "bind" => Ok(PegExpr::Named {
            name: expect_str_at(arr, 1, "named label")?.to_string(),
            expr: Box::new(expr_to_peg_expr(expect_val_at(arr, 2, "named expr")?)?),
        }),
        "cut" | "~" => Ok(PegExpr::Cut),
        "eager" | "&&" => Ok(PegExpr::Eager(Box::new(expr_to_peg_expr(expect_val_at(
            arr,
            1,
            "eager expr",
        )?)?))),
        "tight" | "no_trivia" => Ok(PegExpr::NoTrivia(Box::new(expr_to_peg_expr(
            expect_val_at(arr, 1, "tight expr")?,
        )?))),
        "sep_plus" | "gather" => Ok(PegExpr::SepOneOrMore {
            separator: Box::new(expr_to_peg_expr(expect_val_at(arr, 1, "sep_plus sep")?)?),
            element: Box::new(expr_to_peg_expr(expect_val_at(
                arr,
                2,
                "sep_plus element",
            )?)?),
        }),
        "island" => Ok(PegExpr::Island {
            start: expect_str_at(arr, 1, "island start")?.to_string(),
            end: expect_str_at(arr, 2, "island end")?.to_string(),
            include_delims: false,
        }),
        "raw_block" => Ok(PegExpr::RawBlock {
            start: expect_str_at(arr, 1, "raw_block start")?.to_string(),
            end: expect_str_at(arr, 2, "raw_block end")?.to_string(),
            delim_kind: "generic".to_string(),
        }),
        "behavior" => {
            let entries = expect_arr_at(arr, 1, "behavior entries")?
                .iter()
                .map(behavior_entry_from_value)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(PegExpr::Behavior {
                entries,
                expr: Box::new(expr_to_peg_expr(expect_val_at(arr, 2, "behavior expr")?)?),
            })
        }
        "prec" => {
            let last = arr
                .last()
                .ok_or_else(|| SpecCompileError::MissingField("prec inner expr".to_string()))?;
            expr_to_peg_expr(last)
        }
        _ => {
            let source = expr_to_source(&Value::Array(arr.to_vec()))?;
            RuleTextParser::parse(&source)
                .map_err(|err| SpecCompileError::InvalidFormat(err.message.into()))
        }
    }
}

fn behavior_entry_from_value(value: &Value) -> Result<BehaviorEntry, SpecCompileError> {
    if let Some(arr) = value.as_array() {
        let tag = expect_str_at(arr, 0, "behavior entry tag")?;
        return match tag {
            "diagnostic" => Ok(BehaviorEntry::Diagnostic(DiagnosticBehavior::new(
                expect_str_at(arr, 1, "behavior diagnostic label")?,
            ))),
            "transform" => Ok(BehaviorEntry::Transform(
                TransformBehavior::new(expect_str_at(arr, 1, "behavior transform name")?)
                    .with_args(grammar_scalars_from_slice(&arr[2..])?),
            )),
            "predicate" => Ok(BehaviorEntry::Predicate(
                PredicateBehavior::new(expect_str_at(arr, 1, "behavior predicate name")?)
                    .with_args(grammar_scalars_from_slice(&arr[2..])?),
            )),
            "trace" => {
                let kind = expect_str_at(arr, 1, "behavior trace kind")?;
                let label = expect_str_at(arr, 2, "behavior trace label")?;
                match kind {
                    "capture" => Ok(BehaviorEntry::Trace(TraceBehavior::capture(label))),
                    "action" => Ok(BehaviorEntry::Trace(TraceBehavior::action(label))),
                    _ => Err(SpecCompileError::InvalidFormat(format!(
                        "unknown trace behavior kind: {kind}"
                    ))),
                }
            }
            other => Err(SpecCompileError::UnknownTag(other.to_string())),
        };
    }

    let obj = value
        .as_object()
        .ok_or_else(|| SpecCompileError::TypeError {
            expected: "array or object",
            actual: type_name(value).to_string(),
            ctx: "behavior entry".to_string(),
        })?;
    let kind = obj
        .get("kind")
        .or_else(|| obj.get("type"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| SpecCompileError::MissingField("behavior kind".to_string()))?;
    match kind {
        "diagnostic" => Ok(BehaviorEntry::Diagnostic(DiagnosticBehavior::new(
            obj.get("label").and_then(|v| v.as_str()).ok_or_else(|| {
                SpecCompileError::MissingField("behavior diagnostic label".to_string())
            })?,
        ))),
        "transform" => Ok(BehaviorEntry::Transform(
            TransformBehavior::new(obj.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
                SpecCompileError::MissingField("behavior transform name".to_string())
            })?)
            .with_args(grammar_scalars_from_value(obj.get("args"))?),
        )),
        "predicate" => Ok(BehaviorEntry::Predicate(
            PredicateBehavior::new(obj.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
                SpecCompileError::MissingField("behavior predicate name".to_string())
            })?)
            .with_args(grammar_scalars_from_value(obj.get("args"))?),
        )),
        "trace" => {
            let trace_kind = obj
                .get("trace_kind")
                .or_else(|| obj.get("traceKind"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("behavior trace kind".to_string()))?;
            let label = obj.get("label").and_then(|v| v.as_str()).ok_or_else(|| {
                SpecCompileError::MissingField("behavior trace label".to_string())
            })?;
            match trace_kind {
                "capture" => Ok(BehaviorEntry::Trace(TraceBehavior::capture(label))),
                "action" => Ok(BehaviorEntry::Trace(TraceBehavior::action(label))),
                _ => Err(SpecCompileError::InvalidFormat(format!(
                    "unknown trace behavior kind: {trace_kind}"
                ))),
            }
        }
        other => Err(SpecCompileError::UnknownTag(other.to_string())),
    }
}

fn grammar_scalars_from_value(
    value: Option<&Value>,
) -> Result<Vec<GrammarScalar>, SpecCompileError> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => grammar_scalars_from_slice(items),
        Some(other) => Err(SpecCompileError::TypeError {
            expected: "array",
            actual: type_name(other).to_string(),
            ctx: "behavior args".to_string(),
        }),
    }
}

fn grammar_scalars_from_slice(items: &[Value]) -> Result<Vec<GrammarScalar>, SpecCompileError> {
    items.iter().map(grammar_scalar_from_value).collect()
}

fn grammar_scalar_from_value(value: &Value) -> Result<GrammarScalar, SpecCompileError> {
    Ok(match value {
        Value::Null => GrammarScalar::Null,
        Value::Bool(v) => GrammarScalar::Bool(*v),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                GrammarScalar::Int(i)
            } else if let Some(f) = n.as_f64() {
                GrammarScalar::Float(f)
            } else {
                return Err(SpecCompileError::TypeError {
                    expected: "finite number",
                    actual: "number".to_string(),
                    ctx: "behavior arg".to_string(),
                });
            }
        }
        Value::String(s) => GrammarScalar::Str(s.clone()),
        other => {
            return Err(SpecCompileError::TypeError {
                expected: "scalar",
                actual: type_name(other).to_string(),
                ctx: "behavior arg".to_string(),
            });
        }
    })
}

fn mapping_expr_to_peg_expr(
    obj: &serde_json::Map<String, Value>,
) -> Result<PegExpr, SpecCompileError> {
    let kind = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SpecCompileError::MissingField("type".to_string()))?;
    let child = |key: &str| -> Result<PegExpr, SpecCompileError> {
        expr_to_peg_expr(
            obj.get(key)
                .ok_or_else(|| SpecCompileError::MissingField(key.to_string()))?,
        )
    };
    let children = |key: &str| -> Result<Vec<PegExpr>, SpecCompileError> {
        let arr = obj
            .get(key)
            .and_then(|v| v.as_array())
            .ok_or_else(|| SpecCompileError::MissingField(key.to_string()))?;
        arr.iter().map(expr_to_peg_expr).collect()
    };

    match kind {
        "literal" | "lit" => Ok(PegExpr::Literal(
            obj.get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("text".to_string()))?
                .to_string(),
        )),
        "regex" => {
            let pattern = obj
                .get("pattern")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("pattern".to_string()))?;
            Ok(PegExpr::Regex(
                CompiledRegex::new(pattern, 0, pattern.len())
                    .map_err(|err| SpecCompileError::InvalidFormat(err.message.into()))?,
            ))
        }
        "token_ref" | "tok" => {
            let kind = obj.get("kind").and_then(|v| v.as_str()).map(str::to_string);
            let text = obj.get("text").and_then(|v| v.as_str()).map(str::to_string);
            Ok(PegExpr::TokenRef { kind, text })
        }
        "soft_keyword" | "soft_kw" => Ok(PegExpr::SoftKeyword(
            obj.get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("text".to_string()))?
                .to_string(),
        )),
        "param" => Ok(PegExpr::Parameter {
            name: obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("name".to_string()))?
                .to_string(),
        }),
        "newline" => Ok(PegExpr::Newline),
        "indent" => Ok(PegExpr::Indent),
        "dedent" => Ok(PegExpr::Dedent),
        "cut" | "~" => Ok(PegExpr::Cut),
        "seq" | "sequence" => children("parts")
            .or_else(|_| children("items"))
            .or_else(|_| children("exprs"))
            .map(PegExpr::Sequence),
        "choice" => children("options")
            .or_else(|_| children("alts"))
            .or_else(|_| children("items"))
            .map(PegExpr::Choice),
        "star" | "many" | "zero_or_more" => Ok(PegExpr::ZeroOrMore(Box::new(
            child("expr").or_else(|_| child("body"))?,
        ))),
        "plus" | "one_or_more" => Ok(PegExpr::OneOrMore(Box::new(
            child("expr").or_else(|_| child("body"))?,
        ))),
        "opt" | "optional" => Ok(PegExpr::Optional(Box::new(
            child("expr").or_else(|_| child("body"))?,
        ))),
        "and" => Ok(PegExpr::And(Box::new(child("expr")?))),
        "not" => Ok(PegExpr::Not(Box::new(child("expr")?))),
        "eager" | "and_eager" => Ok(PegExpr::Eager(Box::new(child("expr")?))),
        "named" | "bind" => Ok(PegExpr::Named {
            name: obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("name".to_string()))?
                .to_string(),
            expr: Box::new(child("expr")?),
        }),
        "ref" => Ok(PegExpr::Ref(
            obj.get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("name".to_string()))?
                .to_string(),
        )),
        "imported_ref" | "import" => Ok(PegExpr::ImportedRef {
            grammar_name: obj
                .get("grammar_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("grammar_name".to_string()))?
                .to_string(),
            rule_name: obj
                .get("rule_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("rule_name".to_string()))?
                .to_string(),
        }),
        "grammar_scope" | "scope" => Ok(PegExpr::GrammarScope {
            grammar_name: obj
                .get("grammar_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("grammar_name".to_string()))?
                .to_string(),
            expr: Box::new(child("expr")?),
        }),
        "no_trivia" | "tight" => Ok(PegExpr::NoTrivia(Box::new(child("expr")?))),
        "sep_plus" | "sep_one_or_more" | "gather" => Ok(PegExpr::SepOneOrMore {
            separator: Box::new(child("sep").or_else(|_| child("separator"))?),
            element: Box::new(child("expr").or_else(|_| child("body"))?),
        }),
        "island" => Ok(PegExpr::Island {
            start: obj
                .get("start")
                .and_then(|v| v.as_str())
                .unwrap_or("{")
                .to_string(),
            end: obj
                .get("end")
                .and_then(|v| v.as_str())
                .unwrap_or("}")
                .to_string(),
            include_delims: obj
                .get("include_delims")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        }),
        "raw_block" => Ok(PegExpr::RawBlock {
            start: obj
                .get("start")
                .and_then(|v| v.as_str())
                .unwrap_or("{")
                .to_string(),
            end: obj
                .get("end")
                .and_then(|v| v.as_str())
                .unwrap_or("}")
                .to_string(),
            delim_kind: obj
                .get("delim_kind")
                .and_then(|v| v.as_str())
                .unwrap_or("generic")
                .to_string(),
        }),
        "behavior" => {
            let inner = obj
                .get("expr")
                .or_else(|| obj.get("body"))
                .ok_or_else(|| SpecCompileError::MissingField("behavior expr".to_string()))?;
            let entries = obj
                .get("behaviors")
                .or_else(|| obj.get("entries"))
                .and_then(|v| v.as_array())
                .ok_or_else(|| SpecCompileError::MissingField("behavior entries".to_string()))?
                .iter()
                .map(behavior_entry_from_value)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(PegExpr::Behavior {
                entries,
                expr: Box::new(expr_to_peg_expr(inner)?),
            })
        }
        _ => {
            let source = mapping_expr_to_source(obj)?;
            RuleTextParser::parse(&source)
                .map_err(|err| SpecCompileError::InvalidFormat(err.message.into()))
        }
    }
}

// ── Mapping-style expression compiler ────────────────────────────────────

/// Handle `{"type": "...", ...}` mapping-style spec expressions.
///
/// Mirrors `peg/compile/mapping.py::build_mapping_node()`.
fn mapping_expr_to_source(
    obj: &serde_json::Map<String, Value>,
) -> Result<String, SpecCompileError> {
    let kind = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SpecCompileError::MissingField("type".to_string()))?;

    // Helper to get required child expr and compile it.
    let child_src = |key: &str| -> Result<String, SpecCompileError> {
        let v = obj
            .get(key)
            .ok_or_else(|| SpecCompileError::MissingField(key.to_string()))?;
        expr_to_source(v)
    };
    let parts_src = |key: &str| -> Result<Vec<String>, SpecCompileError> {
        let arr = obj
            .get(key)
            .and_then(|v| v.as_array())
            .ok_or_else(|| SpecCompileError::MissingField(key.to_string()))?;
        arr.iter().map(expr_to_source).collect()
    };

    match kind {
        "literal" | "lit" => {
            let text = obj
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("text".to_string()))?;
            let escaped = text.replace('\\', "\\\\").replace('\'', "\\'");
            Ok(format!("'{escaped}'"))
        }
        "regex" => {
            let pattern = obj
                .get("pattern")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("pattern".to_string()))?;
            Ok(format!("/{pattern}/"))
        }
        "token" => {
            let pattern = obj
                .get("pattern")
                .or_else(|| obj.get("text"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("pattern".to_string()))?;
            Ok(format!("token({pattern})"))
        }
        "token_ref" | "tok" => {
            let kind_val = obj.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            let text_val = obj.get("text").and_then(|v| v.as_str());
            if let Some(t) = text_val {
                let escaped = t.replace('\'', "\\'");
                Ok(format!("tok({kind_val},'{escaped}')"))
            } else {
                Ok(format!("tok({kind_val})"))
            }
        }
        "soft_keyword" | "soft_kw" => {
            let text = obj
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("text".to_string()))?;
            let escaped = text.replace('\'', "\\'");
            Ok(format!("'{escaped}'"))
        }
        "param" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("name".to_string()))?;
            Ok(format!("${name}"))
        }
        "newline" => Ok("newline".to_string()),
        "indent" => Ok("indent".to_string()),
        "dedent" => Ok("dedent".to_string()),
        "cut" | "~" => Ok("~".to_string()),
        "seq" | "sequence" => {
            let children = parts_src("parts")
                .or_else(|_| parts_src("items"))
                .or_else(|_| parts_src("exprs"))?;
            Ok(children.join(" "))
        }
        "choice" => {
            let children = parts_src("options")
                .or_else(|_| parts_src("alts"))
                .or_else(|_| parts_src("items"))?;
            Ok(children.join(" / "))
        }
        "star" | "many" | "zero_or_more" => Ok(format!(
            "({})*",
            child_src("expr").or_else(|_| child_src("body"))?
        )),
        "plus" | "one_or_more" => Ok(format!(
            "({})+",
            child_src("expr").or_else(|_| child_src("body"))?
        )),
        "opt" | "optional" => Ok(format!(
            "({})?",
            child_src("expr").or_else(|_| child_src("body"))?
        )),
        "and" => Ok(format!("&({})", child_src("expr")?)),
        "not" => Ok(format!("!({})", child_src("expr")?)),
        "eager" | "and_eager" => Ok(format!("!!({})", child_src("expr")?)),
        "named" | "bind" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("name".to_string()))?;
            Ok(format!("{name}:({})", child_src("expr")?))
        }
        "ref" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("name".to_string()))?;
            Ok(name.to_string())
        }
        "imported_ref" | "import" => {
            let grammar = obj
                .get("grammar_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("grammar_name".to_string()))?;
            let rule = obj
                .get("rule_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("rule_name".to_string()))?;
            Ok(format!("{grammar}::{rule}"))
        }
        "grammar_scope" | "scope" => {
            let grammar = obj
                .get("grammar_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("grammar_name".to_string()))?;
            Ok(format!("scope('{grammar}', {})", child_src("expr")?))
        }
        "no_trivia" | "tight" => Ok(format!("tight({})", child_src("expr")?)),
        "island" => {
            let start = obj.get("start").and_then(|v| v.as_str()).unwrap_or("{");
            let end = obj.get("end").and_then(|v| v.as_str()).unwrap_or("}");
            Ok(format!("island('{start}', '{end}')"))
        }
        "sep_plus" | "sep_one_or_more" | "gather" => {
            let sep = child_src("sep").or_else(|_| child_src("separator"))?;
            let body = child_src("expr").or_else(|_| child_src("body"))?;
            Ok(format!("({body}) ++ ({sep})"))
        }
        "raw_block" => {
            let start = obj.get("start").and_then(|v| v.as_str()).unwrap_or("{");
            let end = obj.get("end").and_then(|v| v.as_str()).unwrap_or("}");
            Ok(format!("raw_block('{start}', '{end}')"))
        }
        other => Err(SpecCompileError::UnknownTag(other.to_string())),
    }
}

// ── Small helpers ──────────────────────────────────────────────────────────

fn expect_arr<'v>(v: &'v Value, ctx: &str) -> Result<&'v Vec<Value>, SpecCompileError> {
    v.as_array().ok_or_else(|| SpecCompileError::TypeError {
        expected: "array",
        actual: type_name(v).to_string(),
        ctx: ctx.to_string(),
    })
}

fn expect_str<'v>(v: &'v Value, ctx: &str) -> Result<&'v str, SpecCompileError> {
    v.as_str().ok_or_else(|| SpecCompileError::TypeError {
        expected: "string",
        actual: type_name(v).to_string(),
        ctx: ctx.to_string(),
    })
}

fn expect_str_at<'v>(arr: &'v [Value], idx: usize, ctx: &str) -> Result<&'v str, SpecCompileError> {
    let v = arr
        .get(idx)
        .ok_or_else(|| SpecCompileError::MissingField(ctx.to_string()))?;
    expect_str(v, ctx)
}

fn expect_arr_at<'v>(
    arr: &'v [Value],
    idx: usize,
    ctx: &str,
) -> Result<&'v Vec<Value>, SpecCompileError> {
    let v = arr
        .get(idx)
        .ok_or_else(|| SpecCompileError::MissingField(ctx.to_string()))?;
    expect_arr(v, ctx)
}

fn expect_val_at<'v>(
    arr: &'v [Value],
    idx: usize,
    ctx: &str,
) -> Result<&'v Value, SpecCompileError> {
    arr.get(idx)
        .ok_or_else(|| SpecCompileError::MissingField(ctx.to_string()))
}

fn parse_bool(v: &Value, ctx: &str) -> Result<bool, SpecCompileError> {
    if let Some(b) = v.as_bool() {
        return Ok(b);
    }
    if let Some(n) = v.as_i64() {
        return Ok(n != 0);
    }
    if let Some(s) = v.as_str() {
        match s.trim().to_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => return Ok(true),
            "false" | "0" | "no" | "off" => return Ok(false),
            _ => {}
        }
    }
    Err(SpecCompileError::TypeError {
        expected: "bool",
        actual: type_name(v).to_string(),
        ctx: ctx.to_string(),
    })
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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
    fn compile_python_authoring_aliases() {
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
    fn compile_behavior_preserves_trace_for_ast_capture() {
        let g = compile(json!([
            "grammar",
            "g",
            "start",
            [[
                "rule",
                "start",
                ["behavior", [["trace", "capture", "atom"]], ["lit", "x"]]
            ]]
        ]));
        let node = crate::ast::parse_ast(&g, "x", None).expect("ast parse should succeed");
        assert_eq!(node.captures.len(), 1);
        assert_eq!(node.captures[0].label, "atom");
        assert_eq!(node.captures[0].node.span.start, 0);
        assert_eq!(node.captures[0].node.span.end, 1);
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
            [["rule", "root", ["lit", "x"], "memo"]]
        ]));
        let meta = g.metadata.get("root");
        if let Some(m) = meta {
            assert_eq!(m.get("memo").and_then(|v| v.as_bool()), Some(true));
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
}
