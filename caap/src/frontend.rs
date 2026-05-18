/// CAAP surface-syntax frontend: parses source text into an `IRGraph`.
///
/// Uses the Rust PEG port (`caap_peg_port`) with the same grammar as
/// `caap/surface/grammar.py`:
///
///   form    = list | string | integer | boolean | null | symbol
///   list    = '(' form* ')'
///   string  = "(?:[^"\\]|\\.)*"
///   integer = -?(?:0|[1-9][0-9]*)
///   boolean = 'true' | 'false'
///   null    = 'null'
///   symbol  = [A-Za-z_+\-*/<>=!?$%&:.][A-Za-z0-9_+\-*/<>=!?$%&:.]*
///
/// Trivia (same as Python): whitespace, `;` line comments,
/// `#| … |#` and `/* … */` block comments.
use std::sync::LazyLock;

use caap_peg_port::{analyze_and_store, parse_ast_with_max_steps, AstNode, Grammar};
use serde::{Deserialize, Serialize};

use crate::eval::Evaluator;
use crate::graph::{GraphBuilder, IRGraph};
use crate::ir::{IrLiteralData, NodeId};
use crate::source::SourceSpan;
use crate::values::{eval_err, EvalResult};

// ── Grammar ────────────────────────────────────────────────────────────────

const GRAMMAR_TEXT: &str = concat!(
    "forms    <- form*\n",
    "form     <- list / string / integer / boolean / null / symbol\n",
    "list     <- '(' form* ')'\n",
    r#"string   <- /\"(?:[^\"\\]|\\.)*\"/"#,
    "\n",
    "integer  <- /-?(?:0|[1-9][0-9]*)/\n",
    "boolean  <- 'true' / 'false'\n",
    "null     <- 'null'\n",
    r"symbol   <- /[A-Za-z_+\-*\/<>=!?$%&:.][A-Za-z0-9_+\-*\/<>=!?$%&:.]*/",
    "\n",
);

static SURFACE_GRAMMAR: LazyLock<Grammar> = LazyLock::new(|| {
    let mut grammar = Grammar::new(GRAMMAR_TEXT).with_start_rule("forms");
    let analysis = analyze_and_store(&mut grammar);
    debug_assert!(
        analysis.errors.is_empty(),
        "built-in CAAP surface grammar must be valid: {:?}",
        analysis.errors
    );
    grammar.seal();
    grammar
});

fn surface_grammar() -> &'static Grammar {
    &SURFACE_GRAMMAR
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParsedSource {
    pub forms: Vec<ParsedForm>,
}

impl ParsedSource {
    pub fn is_empty(&self) -> bool {
        self.forms.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ParsedForm {
    List {
        items: Vec<ParsedForm>,
        span: SourceSpan,
    },
    Symbol {
        text: String,
        span: SourceSpan,
    },
    String {
        value: String,
        raw: String,
        span: SourceSpan,
    },
    Integer {
        value: i64,
        raw: String,
        span: SourceSpan,
    },
    Boolean {
        value: bool,
        span: SourceSpan,
    },
    Null {
        span: SourceSpan,
    },
}

impl ParsedForm {
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::List { span, .. }
            | Self::Symbol { span, .. }
            | Self::String { span, .. }
            | Self::Integer { span, .. }
            | Self::Boolean { span, .. }
            | Self::Null { span } => span,
        }
    }

    pub fn head_symbol(&self) -> Option<&str> {
        let Self::List { items, .. } = self else {
            return None;
        };
        match items.first() {
            Some(Self::Symbol { text, .. }) => Some(text),
            _ => None,
        }
    }
}

// ── Public API ─────────────────────────────────────────────────────────────

/// Parse CAAP surface text into a typed surface-form model.
pub fn parse_forms(source: &str) -> Result<ParsedSource, String> {
    parse_forms_with_optional_source_path(source, None)
}

/// Parse CAAP surface text into typed forms and attach a source path to spans.
pub fn parse_forms_with_source_path(
    source: &str,
    source_path: impl AsRef<str>,
) -> Result<ParsedSource, String> {
    parse_forms_with_optional_source_path(source, Some(source_path.as_ref()))
}

fn parse_forms_with_optional_source_path(
    source: &str,
    source_path: Option<&str>,
) -> Result<ParsedSource, String> {
    let grammar = surface_grammar();
    let trimmed = source.trim();
    let ast = parse_ast_with_max_steps(grammar, trimmed, Some("forms"), None)
        .map_err(|e| surface_parse_error_message("forms", trimmed, source_path, &e.message))?;
    ast_to_parsed_source(&ast, trimmed, source_path)
}

/// Reparse CAAP surface text with a specific base surface grammar rule.
///
/// This is the Rust equivalent of Python's `ParserSession.reparse()` for the
/// grammar currently owned by this frontend. It returns the typed surface-form
/// model for rules that project to surface forms (`form`, atom rules, `list`,
/// and `forms`).
pub fn reparse_surface_rule(rule_name: &str, source: &str) -> Result<ParsedSource, String> {
    let grammar = surface_grammar();
    let trimmed = source.trim();
    let ast = parse_ast_with_max_steps(grammar, trimmed, Some(rule_name), None)
        .map_err(|e| surface_parse_error_message(rule_name, trimmed, None, &e.message))?;
    ast_to_parsed_source(&ast, trimmed, None)
}

fn surface_parse_error_message(
    rule_name: &str,
    source: &str,
    source_path: Option<&str>,
    message: &str,
) -> String {
    if std::env::var_os("CAAP_RUST_SURFACE_PARSE_CONTEXT").is_none() {
        return message.to_string();
    }
    let path = source_path.unwrap_or("<inline>");
    let snippet: String = source.chars().take(240).collect();
    format!("surface parse failed rule={rule_name} source={path} snippet={snippet:?}: {message}")
}

/// Validate CAAP surface text without lowering it to IR.
pub fn check_source(source: &str) -> Result<(), String> {
    parse_forms(source).map(|_| ())
}

/// Format CAAP surface text into a stable canonical one-line representation.
pub fn format_source(source: &str) -> Result<String, String> {
    let parsed = parse_forms(source)?;
    Ok(format_parsed_source(&parsed))
}

/// Parse CAAP surface text and project the typed surface model as JSON.
pub fn ast_json(source: &str) -> Result<String, String> {
    let parsed = parse_forms(source)?;
    serde_json::to_string_pretty(&parsed)
        .map_err(|error| format!("failed to serialize parsed surface forms: {error}"))
}

pub fn format_parsed_source(parsed: &ParsedSource) -> String {
    parsed
        .forms
        .iter()
        .map(format_parsed_form)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn format_parsed_form(form: &ParsedForm) -> String {
    match form {
        ParsedForm::List { items, .. } => {
            if items.is_empty() {
                "()".to_string()
            } else {
                format!(
                    "({})",
                    items
                        .iter()
                        .map(format_parsed_form)
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            }
        }
        ParsedForm::Symbol { text, .. } => text.clone(),
        ParsedForm::String { value, .. } => format!("\"{}\"", escape_string_literal(value)),
        ParsedForm::Integer { value, .. } => value.to_string(),
        ParsedForm::Boolean { value, .. } => value.to_string(),
        ParsedForm::Null { .. } => "null".to_string(),
    }
}

/// Parse CAAP surface text and return an `IRGraph`.
///
/// All top-level forms are registered in the graph's `top_level_forms` list.
pub fn parse(source: &str) -> Result<IRGraph, String> {
    parse_with_optional_source_path(source, None)
}

/// Parse CAAP surface text and attach a source path to every lowered span.
pub fn parse_with_source_path(
    source: &str,
    source_path: impl AsRef<str>,
) -> Result<IRGraph, String> {
    parse_with_optional_source_path(source, Some(source_path.as_ref()))
}

/// Lower an already typed surface model into IR.
///
/// Dynamic syntax parsing produces the same surface-form model as the base
/// parser after semantic hooks have run, so it enters the compiler through
/// this lowering path instead of being reparsed as text.
pub fn parsed_source_to_ir(parsed: &ParsedSource) -> Result<IRGraph, String> {
    let mut builder = GraphBuilder::new();
    let mut labels: std::collections::HashMap<String, NodeId> = std::collections::HashMap::new();
    for form in &parsed.forms {
        let id = lower_parsed_form(form, &mut builder, &mut labels)?;
        builder.graph.add_top_level_form(id)?;
    }
    Ok(builder.graph)
}

fn parse_with_optional_source_path(
    source: &str,
    source_path: Option<&str>,
) -> Result<IRGraph, String> {
    let grammar = surface_grammar();
    // Trim surrounding whitespace so trailing newlines (common in raw-string literals)
    // don't leave unconsumed input after form*.
    let trimmed = source.trim();
    let ast = parse_ast_with_max_steps(grammar, trimmed, Some("forms"), None)
        .map_err(|e| surface_parse_error_message("forms", trimmed, source_path, &e.message))?;
    ast_to_ir(&ast, trimmed, source_path)
}

fn lower_parsed_form(
    form: &ParsedForm,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> Result<NodeId, String> {
    match form {
        ParsedForm::List { items, span } => {
            if items.is_empty() {
                let id = b.literal(IrLiteralData::Null);
                b.graph.set_source_span(id, span.clone())?;
                return Ok(id);
            }
            match parsed_head_symbol(items) {
                Some("lambda") => return lower_parsed_lambda(items, span, b, labels),
                Some("block") => return lower_parsed_block(items, span, b, labels),
                Some("leave") => return lower_parsed_leave(items, span, b, labels),
                Some("bind") => {
                    if let Some(id) = lower_parsed_bind(items, span, b, labels)? {
                        return Ok(id);
                    }
                }
                Some("set") => return lower_parsed_set(items, span, b, labels),
                _ => {}
            }
            let mut ids = Vec::with_capacity(items.len());
            for item in items {
                ids.push(lower_parsed_form(item, b, labels)?);
            }
            let callee = ids[0];
            let args = ids[1..].to_vec();
            let id = b.call(callee, args);
            b.graph.set_source_span(id, span.clone())?;
            Ok(id)
        }
        ParsedForm::Symbol { text, span } => {
            let id = b.name(text.clone());
            b.graph.set_source_span(id, span.clone())?;
            Ok(id)
        }
        ParsedForm::String { value, span, .. } => {
            let id = b.literal(IrLiteralData::Str(value.clone()));
            b.graph.set_source_span(id, span.clone())?;
            Ok(id)
        }
        ParsedForm::Integer { value, span, .. } => {
            let id = b.literal(IrLiteralData::Int(*value));
            b.graph.set_source_span(id, span.clone())?;
            Ok(id)
        }
        ParsedForm::Boolean { value, span } => {
            let id = b.literal(IrLiteralData::Bool(*value));
            b.graph.set_source_span(id, span.clone())?;
            Ok(id)
        }
        ParsedForm::Null { span } => {
            let id = b.literal(IrLiteralData::Null);
            b.graph.set_source_span(id, span.clone())?;
            Ok(id)
        }
    }
}

fn parsed_head_symbol(items: &[ParsedForm]) -> Option<&str> {
    match items.first() {
        Some(ParsedForm::Symbol { text, .. }) => Some(text.as_str()),
        _ => None,
    }
}

fn parsed_symbol_text(form: &ParsedForm) -> Option<&str> {
    match form {
        ParsedForm::Symbol { text, .. } => Some(text.as_str()),
        _ => None,
    }
}

fn parsed_string_text(form: &ParsedForm) -> Option<&str> {
    match form {
        ParsedForm::String { value, .. } => Some(value.as_str()),
        _ => None,
    }
}

fn parsed_list_items(form: &ParsedForm) -> Option<&[ParsedForm]> {
    match form {
        ParsedForm::List { items, .. } => Some(items.as_slice()),
        _ => None,
    }
}

fn lower_parsed_body_expr(
    body_forms: &[ParsedForm],
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> Result<NodeId, String> {
    match body_forms.len() {
        0 => Ok(b.literal(IrLiteralData::Null)),
        1 => lower_parsed_form(&body_forms[0], b, labels),
        _ => {
            let do_fn = b.name("do");
            let mut args = Vec::with_capacity(body_forms.len());
            for form in body_forms {
                args.push(lower_parsed_form(form, b, labels)?);
            }
            Ok(b.call(do_fn, args))
        }
    }
}

fn lower_parsed_lambda(
    items: &[ParsedForm],
    span: &SourceSpan,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> Result<NodeId, String> {
    if items.len() < 2 {
        return Err("lambda: requires at least a params list".to_string());
    }
    let params = parsed_list_items(&items[1])
        .ok_or_else(|| "lambda params: expected list form".to_string())?;
    let mut param_literals = Vec::with_capacity(params.len());
    for param in params {
        let name = parsed_symbol_text(param).ok_or_else(|| "param: expected symbol".to_string())?;
        param_literals.push(IrLiteralData::Str(name.to_string()));
    }
    let params_id = b.literal(IrLiteralData::Tuple(param_literals));
    b.graph
        .set_source_span(params_id, items[1].span().clone())?;
    let body_id = lower_parsed_body_expr(&items[2..], b, labels)?;
    let lambda_fn = b.name("lambda");
    let id = b.call(lambda_fn, vec![params_id, body_id]);
    b.graph.set_source_span(id, span.clone())?;
    Ok(id)
}

fn lower_parsed_block(
    items: &[ParsedForm],
    span: &SourceSpan,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> Result<NodeId, String> {
    if items.len() < 2 {
        return Err("block: requires a body".to_string());
    }
    let block_id = b.graph.allocate_id();
    let explicit_symbol_label = if items.len() > 2 {
        parsed_symbol_text(&items[1]).map(str::to_string)
    } else {
        None
    };
    let explicit_string_label = if items.len() > 2 {
        parsed_string_text(&items[1]).map(str::to_string)
    } else {
        None
    };
    let body_start = if let Some(label) = explicit_symbol_label.as_deref() {
        labels.insert(label.to_string(), block_id);
        2
    } else {
        if let Some(label) = explicit_string_label.as_deref() {
            labels.insert(label.to_string(), block_id);
        }
        1
    };
    let mut body_ids = Vec::new();
    for form in &items[body_start..] {
        body_ids.push(lower_parsed_form(form, b, labels)?);
    }
    if let Some(label) = explicit_symbol_label
        .as_deref()
        .or(explicit_string_label.as_deref())
    {
        labels.remove(label);
    }
    let block_fn = b.name("block");
    b.try_call_with_id(block_id, block_fn, body_ids, None)?;
    b.graph.set_source_span(block_id, span.clone())?;
    Ok(block_id)
}

fn lower_parsed_bind(
    items: &[ParsedForm],
    span: &SourceSpan,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> Result<Option<NodeId>, String> {
    if items.len() < 2 {
        return Ok(None);
    }
    let Some(bindings) = parsed_list_items(&items[1]) else {
        return Ok(None);
    };
    if bindings.is_empty() {
        return lower_parsed_body_expr(&items[2..], b, labels).map(Some);
    }
    let mut args = Vec::with_capacity(bindings.len() * 2 + 1);
    for binding in bindings {
        let Some(pair) = parsed_list_items(binding) else {
            return Ok(None);
        };
        if pair.len() != 2 {
            return Ok(None);
        }
        let Some(name) = parsed_symbol_text(&pair[0]) else {
            return Ok(None);
        };
        args.push(b.literal(IrLiteralData::Str(name.to_string())));
        args.push(lower_parsed_form(&pair[1], b, labels)?);
    }
    args.push(lower_parsed_body_expr(&items[2..], b, labels)?);
    let bind_fn = b.name("bind");
    let id = b.call(bind_fn, args);
    b.graph.set_source_span(id, span.clone())?;
    Ok(Some(id))
}

fn lower_parsed_leave(
    items: &[ParsedForm],
    span: &SourceSpan,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> Result<NodeId, String> {
    if items.len() < 2 {
        return Err("leave: requires at least a label".to_string());
    }
    let leave_fn = b.name("leave");
    let mut args = Vec::new();
    if let Some(label) = parsed_symbol_text(&items[1]) {
        let block_id = *labels
            .get(label)
            .ok_or_else(|| format!("leave: unknown block label '{label}'"))?;
        args.push(b.literal(IrLiteralData::Int(block_id as i64)));
    } else if let Some(label) = parsed_string_text(&items[1]) {
        if let Some(block_id) = labels.get(label).copied() {
            args.push(b.literal(IrLiteralData::Int(block_id as i64)));
        } else {
            args.push(lower_parsed_form(&items[1], b, labels)?);
        }
    } else {
        args.push(lower_parsed_form(&items[1], b, labels)?);
    }
    for item in &items[2..] {
        args.push(lower_parsed_form(item, b, labels)?);
    }
    let id = b.call(leave_fn, args);
    b.graph.set_source_span(id, span.clone())?;
    Ok(id)
}

fn lower_parsed_set(
    items: &[ParsedForm],
    span: &SourceSpan,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> Result<NodeId, String> {
    if items.len() == 3 {
        if let Some(varname) = parsed_symbol_text(&items[1]) {
            let set_var_fn = b.name("set-var");
            let name_lit = b.literal(IrLiteralData::Str(varname.into()));
            let val_id = lower_parsed_form(&items[2], b, labels)?;
            let id = b.call(set_var_fn, vec![name_lit, val_id]);
            b.graph.set_source_span(id, span.clone())?;
            return Ok(id);
        }
    }
    let mut ids = Vec::with_capacity(items.len());
    for item in items {
        ids.push(lower_parsed_form(item, b, labels)?);
    }
    let callee = ids[0];
    let args = ids[1..].to_vec();
    let id = b.call(callee, args);
    b.graph.set_source_span(id, span.clone())?;
    Ok(id)
}

/// Parse and evaluate CAAP source text; returns the value of the last top-level form.
pub fn eval_source(source: &str) -> EvalResult {
    let graph = parse(source).map_err(eval_err)?;
    let mut ev = Evaluator::new(graph);
    ev.run()
}

/// Build an `Evaluator` from CAAP source text without running it.
pub fn evaluator_from_source(source: &str) -> Result<Evaluator, String> {
    let graph = parse(source)?;
    Ok(Evaluator::new(graph))
}

// ── AST → ParsedForm conversion ────────────────────────────────────────────

fn ast_to_parsed_source(
    root: &AstNode,
    source: &str,
    source_path: Option<&str>,
) -> Result<ParsedSource, String> {
    let line_offsets = compute_line_offsets(source);
    let forms = match root.rule.as_str() {
        "forms" => root
            .children
            .iter()
            .map(|form| ast_to_parsed_form(form, source, &line_offsets, source_path))
            .collect::<Result<Vec<_>, _>>()?,
        "form" | "list" | "symbol" | "string" | "integer" | "boolean" | "null" => {
            vec![ast_to_parsed_form(
                root,
                source,
                &line_offsets,
                source_path,
            )?]
        }
        other => return Err(format!("unexpected AST root rule: {other}")),
    };
    Ok(ParsedSource { forms })
}

fn ast_to_parsed_form(
    node: &AstNode,
    source: &str,
    line_offsets: &[usize],
    source_path: Option<&str>,
) -> Result<ParsedForm, String> {
    match node.rule.as_str() {
        "form" => match node.children.first() {
            Some(child) => ast_to_parsed_form(child, source, line_offsets, source_path),
            None => Err(format!(
                "empty 'form' node at {}..{}",
                node.span.start, node.span.end
            )),
        },
        "list" => {
            let start = content_start(source, node.span.start);
            let span = source_span_for_offsets(start, node.span.end, line_offsets, source_path)?;
            let items = node
                .children
                .iter()
                .map(|child| ast_to_parsed_form(child, source, line_offsets, source_path))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ParsedForm::List { items, span })
        }
        "symbol" => {
            let start = content_start(source, node.span.start);
            let span = source_span_for_offsets(start, node.span.end, line_offsets, source_path)?;
            Ok(ParsedForm::Symbol {
                text: source[start..node.span.end].to_string(),
                span,
            })
        }
        "string" => {
            let start = content_start(source, node.span.start);
            let raw = source[start..node.span.end].to_string();
            let content = &raw[1..raw.len() - 1];
            let span = source_span_for_offsets(start, node.span.end, line_offsets, source_path)?;
            Ok(ParsedForm::String {
                value: unescape(content)?,
                raw,
                span,
            })
        }
        "integer" => {
            let start = content_start(source, node.span.start);
            let raw = source[start..node.span.end].to_string();
            let value = raw
                .parse()
                .map_err(|error| format!("invalid integer '{raw}': {error}"))?;
            let span = source_span_for_offsets(start, node.span.end, line_offsets, source_path)?;
            Ok(ParsedForm::Integer { value, raw, span })
        }
        "boolean" => {
            let start = content_start(source, node.span.start);
            let span = source_span_for_offsets(start, node.span.end, line_offsets, source_path)?;
            Ok(ParsedForm::Boolean {
                value: &source[start..node.span.end] == "true",
                span,
            })
        }
        "null" => {
            let start = content_start(source, node.span.start);
            let span = source_span_for_offsets(start, node.span.end, line_offsets, source_path)?;
            Ok(ParsedForm::Null { span })
        }
        other => Err(format!(
            "unexpected AST rule '{other}' at {}..{}",
            node.span.start, node.span.end
        )),
    }
}

// ── AST → IRGraph conversion ───────────────────────────────────────────────

fn ast_to_ir(root: &AstNode, source: &str, source_path: Option<&str>) -> Result<IRGraph, String> {
    let mut builder = GraphBuilder::new();
    let line_offsets = compute_line_offsets(source);
    let mut labels: std::collections::HashMap<String, NodeId> = std::collections::HashMap::new();
    match root.rule.as_str() {
        "forms" => {
            for form_node in &root.children {
                let id = lower_node(
                    form_node,
                    source,
                    &line_offsets,
                    source_path,
                    &mut builder,
                    &mut labels,
                )?;
                builder.graph.add_top_level_form(id)?;
            }
        }
        "form" => {
            let id = lower_node(
                root,
                source,
                &line_offsets,
                source_path,
                &mut builder,
                &mut labels,
            )?;
            builder.graph.add_top_level_form(id)?;
        }
        other => return Err(format!("unexpected AST root rule: {other}")),
    }
    Ok(builder.graph)
}

fn lower_node(
    node: &AstNode,
    source: &str,
    line_offsets: &[usize],
    source_path: Option<&str>,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> Result<NodeId, String> {
    match node.rule.as_str() {
        // Dispatch wrapper — recurse into the single matched child.
        "form" => match node.children.first() {
            Some(child) => lower_node(child, source, line_offsets, source_path, b, labels),
            None => Err(format!(
                "empty 'form' node at {}..{}",
                node.span.start, node.span.end
            )),
        },

        // List: ( form* )  →  CallNode or literal null for ()
        "list" => {
            if node.children.is_empty() {
                return Ok(b.literal(IrLiteralData::Null));
            }
            // Detect special forms by examining the head symbol.
            if let Some(head) = head_symbol(node, source) {
                match head {
                    "lambda" => {
                        return lower_lambda(node, source, line_offsets, source_path, b, labels)
                    }
                    "block" => {
                        return lower_block(node, source, line_offsets, source_path, b, labels)
                    }
                    "leave" => {
                        return lower_leave(node, source, line_offsets, source_path, b, labels)
                    }
                    "bind" => {
                        if let Some(id) =
                            lower_bind(node, source, line_offsets, source_path, b, labels)?
                        {
                            return Ok(id);
                        }
                    }
                    "set" => return lower_set(node, source, line_offsets, source_path, b, labels),
                    _ => {}
                }
            }
            // Generic call.
            let mut ids = Vec::with_capacity(node.children.len());
            for child in &node.children {
                ids.push(lower_node(
                    child,
                    source,
                    line_offsets,
                    source_path,
                    b,
                    labels,
                )?);
            }
            let callee = ids[0];
            let args = ids[1..].to_vec();
            let id = b.call(callee, args);
            attach_span(&mut b.graph, id, node, line_offsets, source_path)?;
            Ok(id)
        }

        // Symbol → NameNode
        "symbol" => {
            let start = content_start(source, node.span.start);
            let text = &source[start..node.span.end];
            let id = b.name(text.to_string());
            attach_span_offsets(
                &mut b.graph,
                id,
                start,
                node.span.end,
                line_offsets,
                source_path,
            )?;
            Ok(id)
        }

        // Integer (or float if '.' present)
        "integer" => {
            let start = content_start(source, node.span.start);
            let text = &source[start..node.span.end];
            if text.contains('.') {
                let f: f64 = text
                    .parse()
                    .map_err(|e| format!("invalid float '{text}': {e}"))?;
                let id = b.literal(IrLiteralData::Float(f));
                attach_span_offsets(
                    &mut b.graph,
                    id,
                    start,
                    node.span.end,
                    line_offsets,
                    source_path,
                )?;
                Ok(id)
            } else {
                let i: i64 = text
                    .parse()
                    .map_err(|e| format!("invalid integer '{text}': {e}"))?;
                let id = b.literal(IrLiteralData::Int(i));
                attach_span_offsets(
                    &mut b.graph,
                    id,
                    start,
                    node.span.end,
                    line_offsets,
                    source_path,
                )?;
                Ok(id)
            }
        }

        // String: content starts at the `"` character (after trivia)
        "string" => {
            let start = content_start(source, node.span.start);
            let raw = &source[start..node.span.end];
            let content = &raw[1..raw.len() - 1]; // strip surrounding quotes
            let id = b.literal(IrLiteralData::Str(unescape(content)?));
            attach_span_offsets(
                &mut b.graph,
                id,
                start,
                node.span.end,
                line_offsets,
                source_path,
            )?;
            Ok(id)
        }

        // Boolean
        "boolean" => {
            let start = content_start(source, node.span.start);
            let v = &source[start..node.span.end] == "true";
            let id = b.literal(IrLiteralData::Bool(v));
            attach_span_offsets(
                &mut b.graph,
                id,
                start,
                node.span.end,
                line_offsets,
                source_path,
            )?;
            Ok(id)
        }

        // Null
        "null" => {
            let start = content_start(source, node.span.start);
            let id = b.literal(IrLiteralData::Null);
            attach_span_offsets(
                &mut b.graph,
                id,
                start,
                node.span.end,
                line_offsets,
                source_path,
            )?;
            Ok(id)
        }

        other => Err(format!(
            "unexpected AST rule '{other}' at {}..{}",
            node.span.start, node.span.end
        )),
    }
}

// ── Special-form helpers ───────────────────────────────────────────────────

/// Return the symbol text of the first child of a `list` node, if it is a symbol.
fn head_symbol<'s>(list_node: &AstNode, source: &'s str) -> Option<&'s str> {
    let form = list_node.children.first()?;
    let sym = match form.rule.as_str() {
        "form" => form.children.first()?,
        "symbol" => form,
        _ => return None,
    };
    if sym.rule != "symbol" {
        return None;
    }
    let start = content_start(source, sym.span.start);
    if start > sym.span.end {
        return None;
    }
    Some(&source[start..sym.span.end])
}

/// Return the symbol text from a `form` or `symbol` AST node.
fn symbol_text<'s>(node: &AstNode, source: &'s str) -> Option<&'s str> {
    let sym = match node.rule.as_str() {
        "form" => node.children.first()?,
        "symbol" => node,
        _ => return None,
    };
    if sym.rule != "symbol" {
        return None;
    }
    let start = content_start(source, sym.span.start);
    if start > sym.span.end {
        return None;
    }
    Some(&source[start..sym.span.end])
}

/// Return an unescaped string literal from a `form` or `string` AST node.
fn string_literal_text(node: &AstNode, source: &str) -> Result<Option<String>, String> {
    let string_node = match node.rule.as_str() {
        "form" => match node.children.first() {
            Some(child) if child.rule == "string" => child,
            _ => return Ok(None),
        },
        "string" => node,
        _ => return Ok(None),
    };
    let start = content_start(source, string_node.span.start);
    if start >= string_node.span.end {
        return Ok(None);
    }
    let raw = &source[start..string_node.span.end];
    if raw.len() < 2 || !raw.starts_with('"') || !raw.ends_with('"') {
        return Ok(None);
    }
    Ok(Some(unescape(&raw[1..raw.len() - 1])?))
}

fn list_node_from_form(node: &AstNode) -> Option<&AstNode> {
    match node.rule.as_str() {
        "form" => {
            let inner = node.children.first()?;
            (inner.rule == "list").then_some(inner)
        }
        "list" => Some(node),
        _ => None,
    }
}

fn lower_body_expr(
    body_forms: &[AstNode],
    source: &str,
    line_offsets: &[usize],
    source_path: Option<&str>,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> Result<NodeId, String> {
    match body_forms.len() {
        0 => Ok(b.literal(IrLiteralData::Null)),
        1 => lower_node(&body_forms[0], source, line_offsets, source_path, b, labels),
        _ => {
            let do_fn = b.name("do");
            let mut args = Vec::with_capacity(body_forms.len());
            for form in body_forms {
                args.push(lower_node(
                    form,
                    source,
                    line_offsets,
                    source_path,
                    b,
                    labels,
                )?);
            }
            Ok(b.call(do_fn, args))
        }
    }
}

/// Lower `(lambda (param ...) body ...)`.
///
/// The params list is converted to the canonical literal tuple of parameter
/// names used by the Python surface profile and IR builders.
fn lower_lambda(
    node: &AstNode,
    source: &str,
    line_offsets: &[usize],
    source_path: Option<&str>,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> Result<NodeId, String> {
    // children: [form("lambda"), form(params-list), form(body)...]
    if node.children.len() < 2 {
        return Err("lambda: requires at least a params list".to_string());
    }
    let params_form = &node.children[1];
    let params_id = lower_params_list(params_form, source, line_offsets, source_path, b)?;
    let body_id = lower_body_expr(
        &node.children[2..],
        source,
        line_offsets,
        source_path,
        b,
        labels,
    )?;
    let lambda_fn = b.name("lambda");
    let id = b.call(lambda_fn, vec![params_id, body_id]);
    attach_span(&mut b.graph, id, node, line_offsets, source_path)?;
    Ok(id)
}

/// Convert a `(param ...)` list form into the canonical params literal.
fn lower_params_list(
    form_node: &AstNode,
    source: &str,
    line_offsets: &[usize],
    source_path: Option<&str>,
    b: &mut GraphBuilder,
) -> Result<NodeId, String> {
    let list_node = match form_node.rule.as_str() {
        "form" => {
            let inner = form_node
                .children
                .first()
                .ok_or_else(|| "lambda params: empty form".to_string())?;
            inner
        }
        "list" => form_node,
        _ => {
            return Err(format!(
                "lambda params: expected list, got {}",
                form_node.rule
            ))
        }
    };
    if list_node.rule != "list" {
        return Err(format!(
            "lambda params: expected list form, got {}",
            list_node.rule
        ));
    }
    let mut params = Vec::new();
    for child in &list_node.children {
        let name = symbol_text(child, source)
            .ok_or_else(|| format!("param: expected symbol, got {}", child.rule))?;
        params.push(IrLiteralData::Str(name.to_string()));
    }
    let id = b.literal(IrLiteralData::Tuple(params));
    attach_span(&mut b.graph, id, list_node, line_offsets, source_path)?;
    Ok(id)
}

/// Lower `(block [label] body ...)`.
///
/// Pre-allocates the block NodeId so that nested `(leave label ...)` can
/// reference it by ID via `labels`.
fn lower_block(
    node: &AstNode,
    source: &str,
    line_offsets: &[usize],
    source_path: Option<&str>,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> Result<NodeId, String> {
    if node.children.len() < 2 {
        return Err("block: requires a body".to_string());
    }
    // Pre-allocate the block CallNode's ID.
    let block_id = b.graph.allocate_id();

    let first_form = &node.children[1];
    let explicit_symbol_label = if node.children.len() > 2 {
        symbol_text(first_form, source).map(str::to_string)
    } else {
        None
    };
    let explicit_string_label = if node.children.len() > 2 {
        string_literal_text(first_form, source)?
    } else {
        None
    };

    let body_start = if let Some(label) = explicit_symbol_label.as_deref() {
        labels.insert(label.to_string(), block_id);
        2
    } else {
        if let Some(label) = explicit_string_label.as_deref() {
            labels.insert(label.to_string(), block_id);
        }
        1
    };

    let mut body_ids = Vec::new();
    for child in &node.children[body_start..] {
        body_ids.push(lower_node(
            child,
            source,
            line_offsets,
            source_path,
            b,
            labels,
        )?);
    }

    if let Some(label) = explicit_symbol_label
        .as_deref()
        .or(explicit_string_label.as_deref())
    {
        labels.remove(label);
    }

    let block_fn = b.name("block");
    b.try_call_with_id(block_id, block_fn, body_ids, None)?;
    attach_span(&mut b.graph, block_id, node, line_offsets, source_path)?;
    Ok(block_id)
}

/// Lower multi-binding `(bind ((name value) ...) body ...)` to canonical
/// `(bind "name" value ... body-expr)`. Flat `(bind name value)` is left for
/// the runtime because it defines in the current environment.
fn lower_bind(
    node: &AstNode,
    source: &str,
    line_offsets: &[usize],
    source_path: Option<&str>,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> Result<Option<NodeId>, String> {
    if node.children.len() < 2 {
        return Ok(None);
    }
    let Some(bindings_node) = list_node_from_form(&node.children[1]) else {
        return Ok(None);
    };

    if bindings_node.children.is_empty() {
        return lower_body_expr(
            &node.children[2..],
            source,
            line_offsets,
            source_path,
            b,
            labels,
        )
        .map(Some);
    }

    let mut args = Vec::with_capacity(bindings_node.children.len() * 2 + 1);
    for binding_form in &bindings_node.children {
        let Some(binding_node) = list_node_from_form(binding_form) else {
            return Ok(None);
        };
        if binding_node.children.len() != 2 {
            return Ok(None);
        }
        let Some(name) = symbol_text(&binding_node.children[0], source) else {
            return Ok(None);
        };
        args.push(b.literal(IrLiteralData::Str(name.to_string())));
        args.push(lower_node(
            &binding_node.children[1],
            source,
            line_offsets,
            source_path,
            b,
            labels,
        )?);
    }

    args.push(lower_body_expr(
        &node.children[2..],
        source,
        line_offsets,
        source_path,
        b,
        labels,
    )?);
    let bind_fn = b.name("bind");
    let id = b.call(bind_fn, args);
    attach_span(&mut b.graph, id, node, line_offsets, source_path)?;
    Ok(Some(id))
}

/// Lower `(leave label [value])`.
fn lower_leave(
    node: &AstNode,
    source: &str,
    line_offsets: &[usize],
    source_path: Option<&str>,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> Result<NodeId, String> {
    // children: [form("leave"), form(label), form(value)?]
    if node.children.len() < 2 {
        return Err("leave: requires at least a label".to_string());
    }
    let label_form = &node.children[1];
    let leave_fn = b.name("leave");
    let mut args = Vec::new();
    if let Some(label) = symbol_text(label_form, source) {
        let block_id = *labels
            .get(label)
            .ok_or_else(|| format!("leave: unknown block label '{label}'"))?;
        args.push(b.literal(IrLiteralData::Int(block_id as i64)));
    } else if let Some(label) = string_literal_text(label_form, source)? {
        if let Some(block_id) = labels.get(&label).copied() {
            args.push(b.literal(IrLiteralData::Int(block_id as i64)));
        } else {
            args.push(lower_node(
                label_form,
                source,
                line_offsets,
                source_path,
                b,
                labels,
            )?);
        }
    } else {
        args.push(lower_node(
            label_form,
            source,
            line_offsets,
            source_path,
            b,
            labels,
        )?);
    }
    for child in &node.children[2..] {
        args.push(lower_node(
            child,
            source,
            line_offsets,
            source_path,
            b,
            labels,
        )?);
    }
    let id = b.call(leave_fn, args);
    attach_span(&mut b.graph, id, node, line_offsets, source_path)?;
    Ok(id)
}

/// Lower `(set varname expr)` — variable mutation special form.
///
/// Emits `(set-var "varname" expr)` so the `set-var` builtin can mutate the
/// binding in the environment chain without evaluating the name.
fn lower_set(
    node: &AstNode,
    source: &str,
    line_offsets: &[usize],
    source_path: Option<&str>,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> Result<NodeId, String> {
    // children: [form("set"), form(varname), form(expr)]
    if node.children.len() != 3 {
        // Not a 2-arg set — fall through to generic call (container set).
        let mut ids = Vec::with_capacity(node.children.len());
        for child in &node.children {
            ids.push(lower_node(
                child,
                source,
                line_offsets,
                source_path,
                b,
                labels,
            )?);
        }
        let callee = ids[0];
        let args = ids[1..].to_vec();
        let id = b.call(callee, args);
        attach_span(&mut b.graph, id, node, line_offsets, source_path)?;
        return Ok(id);
    }
    let name_form = &node.children[1];
    if let Some(varname) = symbol_text(name_form, source) {
        let set_var_fn = b.name("set-var");
        let name_lit = b.literal(IrLiteralData::Str(varname.into()));
        let val_id = lower_node(
            &node.children[2],
            source,
            line_offsets,
            source_path,
            b,
            labels,
        )?;
        let id = b.call(set_var_fn, vec![name_lit, val_id]);
        attach_span(&mut b.graph, id, node, line_offsets, source_path)?;
        Ok(id)
    } else {
        // Not a symbol — treat as generic 3-arg container set.
        let mut ids = Vec::with_capacity(node.children.len());
        for child in &node.children {
            ids.push(lower_node(
                child,
                source,
                line_offsets,
                source_path,
                b,
                labels,
            )?);
        }
        let callee = ids[0];
        let args = ids[1..].to_vec();
        let id = b.call(callee, args);
        attach_span(&mut b.graph, id, node, line_offsets, source_path)?;
        Ok(id)
    }
}

fn attach_span(
    graph: &mut IRGraph,
    id: NodeId,
    node: &AstNode,
    line_offsets: &[usize],
    source_path: Option<&str>,
) -> Result<(), String> {
    attach_span_offsets(
        graph,
        id,
        node.span.start,
        node.span.end,
        line_offsets,
        source_path,
    )
}

fn attach_span_offsets(
    graph: &mut IRGraph,
    id: NodeId,
    start: usize,
    end: usize,
    line_offsets: &[usize],
    source_path: Option<&str>,
) -> Result<(), String> {
    Ok(graph.set_source_span(
        id,
        source_span_for_offsets(start, end, line_offsets, source_path)?,
    )?)
}

fn source_span_for_offsets(
    start: usize,
    end: usize,
    line_offsets: &[usize],
    source_path: Option<&str>,
) -> Result<SourceSpan, String> {
    let (start_line, start_col) = line_col(line_offsets, start);
    let (end_line, end_col) = line_col(line_offsets, end);
    SourceSpan::with_locator(
        None,
        start,
        end,
        source_path.map(str::to_string),
        start_line,
        start_col,
        end_line,
        end_col,
    )
}

fn compute_line_offsets(source: &str) -> Vec<usize> {
    let mut offsets = vec![0usize];
    for (idx, ch) in source.char_indices() {
        if ch == '\n' {
            offsets.push(idx + 1);
        }
    }
    offsets
}

fn line_col(line_offsets: &[usize], pos: usize) -> (usize, usize) {
    let idx = line_offsets.partition_point(|&offset| offset <= pos);
    let line_idx = idx.saturating_sub(1);
    (line_idx + 1, pos - line_offsets[line_idx] + 1)
}

/// Advance `pos` past CAAP trivia (whitespace, `;` comments, `#|…|#`, `/*…*/`).
fn content_start(source: &str, mut pos: usize) -> usize {
    let b = source.as_bytes();
    loop {
        // whitespace
        while pos < b.len() && matches!(b[pos], b' ' | b'\t' | b'\r' | b'\n') {
            pos += 1;
        }
        // ';' line comment
        if pos < b.len() && b[pos] == b';' {
            while pos < b.len() && b[pos] != b'\n' {
                pos += 1;
            }
            continue;
        }
        // '#| … |#' block comment
        if pos + 1 < b.len() && b[pos] == b'#' && b[pos + 1] == b'|' {
            pos += 2;
            while pos + 1 < b.len() {
                if b[pos] == b'|' && b[pos + 1] == b'#' {
                    pos += 2;
                    break;
                }
                pos += 1;
            }
            continue;
        }
        // '/* … */' block comment
        if pos + 1 < b.len() && b[pos] == b'/' && b[pos + 1] == b'*' {
            pos += 2;
            while pos + 1 < b.len() {
                if b[pos] == b'*' && b[pos + 1] == b'/' {
                    pos += 2;
                    break;
                }
                pos += 1;
            }
            continue;
        }
        break;
    }
    pos
}

fn unescape(s: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => return Err("unterminated escape sequence in string literal".to_string()),
        }
    }
    Ok(out)
}

fn escape_string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::values::RuntimeValue;

    fn run(src: &str) -> RuntimeValue {
        eval_source(src).expect("eval_source failed")
    }

    // ── Atoms ──────────────────────────────────────────────────────────────

    #[test]
    fn parse_null() {
        assert_eq!(run("null"), RuntimeValue::Null);
    }

    #[test]
    fn parse_bool_true() {
        assert_eq!(run("true"), RuntimeValue::Bool(true));
    }

    #[test]
    fn parse_bool_false() {
        assert_eq!(run("false"), RuntimeValue::Bool(false));
    }

    #[test]
    fn parse_positive_int() {
        assert_eq!(run("42"), RuntimeValue::Int(42));
    }

    #[test]
    fn parse_negative_int() {
        assert_eq!(run("-7"), RuntimeValue::Int(-7));
    }

    #[test]
    fn parse_zero() {
        assert_eq!(run("0"), RuntimeValue::Int(0));
    }

    #[test]
    fn parse_string() {
        assert_eq!(run(r#""hello""#), RuntimeValue::Str("hello".into()));
    }

    #[test]
    fn parse_string_escape_newline() {
        assert_eq!(run(r#""a\nb""#), RuntimeValue::Str("a\nb".into()));
    }

    #[test]
    fn parse_empty_list_is_null() {
        assert_eq!(run("()"), RuntimeValue::Null);
    }

    // ── Calls ──────────────────────────────────────────────────────────────

    #[test]
    fn parse_add() {
        assert_eq!(run("(int-add 1 2)"), RuntimeValue::Int(3));
    }

    #[test]
    fn parse_nested() {
        assert_eq!(run("(int-add (int-mul 2 3) 4)"), RuntimeValue::Int(10));
    }

    #[test]
    fn parse_forms_exposes_typed_surface_model() {
        let parsed = parse_forms(r#"(int-add 1 "a\n") null"#).unwrap();
        assert_eq!(parsed.forms.len(), 2);

        assert!(
            matches!(&parsed.forms[0], ParsedForm::List { .. }),
            "expected top-level list form"
        );
        let ParsedForm::List { items, span } = &parsed.forms[0] else {
            return;
        };
        assert_eq!(span.start, 0);
        assert_eq!(span.start_line, 1);
        assert_eq!(parsed.forms[0].head_symbol(), Some("int-add"));
        assert_eq!(
            items[0],
            ParsedForm::Symbol {
                text: "int-add".to_string(),
                span: SourceSpan::new(1, 8, 1, 2, 1, 9).unwrap(),
            }
        );
        assert_eq!(
            items[1],
            ParsedForm::Integer {
                value: 1,
                raw: "1".to_string(),
                span: SourceSpan::new(9, 10, 1, 10, 1, 11).unwrap(),
            }
        );
        assert_eq!(
            items[2],
            ParsedForm::String {
                value: "a\n".to_string(),
                raw: r#""a\n""#.to_string(),
                span: SourceSpan::new(11, 16, 1, 12, 1, 17).unwrap(),
            }
        );

        assert_eq!(
            parsed.forms[1],
            ParsedForm::Null {
                span: SourceSpan::new(18, 22, 1, 19, 1, 23).unwrap(),
            }
        );
    }

    #[test]
    fn parse_forms_keeps_dotted_symbols_whole() {
        let parsed = parse_forms("(stdlib.pass-kit.register-compile-time-function)").unwrap();
        assert!(
            matches!(&parsed.forms[0], ParsedForm::List { .. }),
            "expected top-level list form"
        );
        let ParsedForm::List { items, .. } = &parsed.forms[0] else {
            return;
        };
        assert_eq!(items.len(), 1);
        assert_eq!(
            parsed.forms[0].head_symbol(),
            Some("stdlib.pass-kit.register-compile-time-function")
        );
        assert_eq!(
            items[0],
            ParsedForm::Symbol {
                text: "stdlib.pass-kit.register-compile-time-function".to_string(),
                span: SourceSpan::new(1, 47, 1, 2, 1, 48).unwrap(),
            }
        );
    }

    #[test]
    fn eval_resolves_qualified_name_through_map_prefix() {
        assert_eq!(
            run(r#"(bind ((stdlib.pass-kit (map-of "answer" 42))) stdlib.pass-kit.answer)"#),
            RuntimeValue::Int(42)
        );
    }

    #[test]
    fn check_and_format_source_use_typed_surface_forms() {
        check_source(" ; ignored\n( int-add 1 (int-mul 2 3) )").unwrap();
        assert!(check_source("(int-add 1 @bad)").is_err());
        assert_eq!(
            format_source(
                r#"
                  ( int-add 1
                    (string-size "a\nb\"c") )
                  null
                "#
            )
            .unwrap(),
            "(int-add 1 (string-size \"a\\nb\\\"c\"))\nnull"
        );
    }

    #[test]
    fn ast_json_projects_typed_surface_forms() {
        let json = ast_json("(int-add 1)").unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["forms"][0]["List"]["span"]["start"], 0);
        assert_eq!(
            value["forms"][0]["List"]["items"][0]["Symbol"]["text"],
            "int-add"
        );
        assert_eq!(value["forms"][0]["List"]["items"][1]["Integer"]["value"], 1);
    }

    #[test]
    fn parse_multiple_top_forms_returns_last() {
        assert_eq!(run("1 2 3"), RuntimeValue::Int(3));
    }

    #[test]
    fn lower_bind_with_empty_binding_list_to_body() {
        assert_eq!(run("(bind () 42)"), RuntimeValue::Int(42));
        assert_eq!(run("(bind () 1 2)"), RuntimeValue::Int(2));
    }

    #[test]
    fn parsed_source_to_ir_lowers_special_forms() {
        let parsed = ParsedSource {
            forms: vec![ParsedForm::List {
                items: vec![
                    ParsedForm::Symbol {
                        text: "bind".to_string(),
                        span: SourceSpan::new(1, 5, 1, 2, 1, 6).unwrap(),
                    },
                    ParsedForm::List {
                        items: vec![ParsedForm::List {
                            items: vec![
                                ParsedForm::Symbol {
                                    text: "x".to_string(),
                                    span: SourceSpan::new(8, 9, 1, 9, 1, 10).unwrap(),
                                },
                                ParsedForm::Integer {
                                    value: 41,
                                    raw: "41".to_string(),
                                    span: SourceSpan::new(10, 12, 1, 11, 1, 13).unwrap(),
                                },
                            ],
                            span: SourceSpan::new(7, 13, 1, 8, 1, 14).unwrap(),
                        }],
                        span: SourceSpan::new(6, 14, 1, 7, 1, 15).unwrap(),
                    },
                    ParsedForm::List {
                        items: vec![
                            ParsedForm::Symbol {
                                text: "int-add".to_string(),
                                span: SourceSpan::new(16, 23, 1, 17, 1, 24).unwrap(),
                            },
                            ParsedForm::Symbol {
                                text: "x".to_string(),
                                span: SourceSpan::new(24, 25, 1, 25, 1, 26).unwrap(),
                            },
                            ParsedForm::Integer {
                                value: 1,
                                raw: "1".to_string(),
                                span: SourceSpan::new(26, 27, 1, 27, 1, 28).unwrap(),
                            },
                        ],
                        span: SourceSpan::new(15, 28, 1, 16, 1, 29).unwrap(),
                    },
                ],
                span: SourceSpan::new(0, 29, 1, 1, 1, 30).unwrap(),
            }],
        };
        let graph = parsed_source_to_ir(&parsed).unwrap();
        let mut ev = Evaluator::new(graph);
        assert_eq!(ev.run().unwrap(), RuntimeValue::Int(42));
    }

    // ── Trivia ─────────────────────────────────────────────────────────────

    #[test]
    fn line_comment_skipped() {
        assert_eq!(run("; ignored\n42"), RuntimeValue::Int(42));
    }

    #[test]
    fn block_comment_hash_pipe() {
        assert_eq!(run("#| ignored |# 99"), RuntimeValue::Int(99));
    }

    #[test]
    fn block_comment_c_style() {
        assert_eq!(run("/* ignored */ 77"), RuntimeValue::Int(77));
    }

    // ── Real programs ──────────────────────────────────────────────────────

    #[test]
    fn eval_factorial() {
        let src = r#"
            (do
              (bind fact
                (lambda (n)
                  (if (eq n 0)
                    1
                    (int-mul n (fact (int-add n -1))))))
              (fact 5))
        "#;
        assert_eq!(run(src), RuntimeValue::Int(120));
    }

    #[test]
    fn eval_fibonacci() {
        let src = r#"
            (do
              (bind fib
                (lambda (n)
                  (if (lt n 2)
                    n
                    (int-add (fib (int-add n -1)) (fib (int-add n -2))))))
              (fib 8))
        "#;
        assert_eq!(run(src), RuntimeValue::Int(21));
    }

    #[test]
    fn eval_sequence_map() {
        let src = r#"
            (sequence-map (list-of 1 2 3 4 5) (lambda (x) (int-mul x x)))
        "#;
        let result = run(src);
        assert!(matches!(&result, RuntimeValue::List(_)), "expected list");
        let RuntimeValue::List(l) = result else {
            return;
        };
        let v = l.borrow().clone();
        assert_eq!(
            v,
            vec![
                RuntimeValue::Int(1),
                RuntimeValue::Int(4),
                RuntimeValue::Int(9),
                RuntimeValue::Int(16),
                RuntimeValue::Int(25),
            ]
        );
    }

    #[test]
    fn eval_string_concat() {
        assert_eq!(
            run(r#"(string-concat-many "hello" " " "world")"#),
            RuntimeValue::Str("hello world".into())
        );
    }

    #[test]
    fn eval_do_bind() {
        let src = "(do (bind x 10) (bind y 20) (int-add x y))";
        assert_eq!(run(src), RuntimeValue::Int(30));
    }

    #[test]
    fn eval_canonicalized_multi_bind() {
        let src = "(bind ((x 1) (y 2)) (int-add x y))";
        assert_eq!(run(src), RuntimeValue::Int(3));
    }

    #[test]
    fn eval_if_true() {
        assert_eq!(run("(if true 1 2)"), RuntimeValue::Int(1));
    }

    #[test]
    fn eval_if_false() {
        assert_eq!(run("(if false 1 2)"), RuntimeValue::Int(2));
    }

    #[test]
    fn eval_block_leave() {
        assert_eq!(run("(block b (leave b 42) 99)"), RuntimeValue::Int(42));
    }

    #[test]
    fn eval_canonical_block_null_label() {
        assert_eq!(run("(block null 7)"), RuntimeValue::Int(7));
    }

    #[test]
    fn eval_canonical_block_string_label_leave() {
        assert_eq!(
            run(r#"(block "exit" (leave "exit" 42) 99)"#),
            RuntimeValue::Int(42)
        );
    }

    #[test]
    fn eval_while() {
        let src = r#"
            (do
              (bind n 0)
              (while (lt n 5) (set n (int-add n 1)))
              n)
        "#;
        assert_eq!(run(src), RuntimeValue::Int(5));
    }

    #[test]
    fn eval_negative_int_arg() {
        assert_eq!(run("(int-add -3 4)"), RuntimeValue::Int(1));
    }

    #[test]
    fn eval_hyphen_in_builtin_name() {
        // '-' in a symbol is valid; 'string-concat-many' is a single symbol
        assert_eq!(
            run(r#"(string-concat-many "a" "b")"#),
            RuntimeValue::Str("ab".into())
        );
    }

    #[test]
    fn eval_sequence_filter() {
        let src = "(sequence-filter (list-of 1 2 3 4 5) (lambda (x) (gt x 2)))";
        let result = run(src);
        assert!(matches!(&result, RuntimeValue::List(_)), "expected list");
        let RuntimeValue::List(l) = result else {
            return;
        };
        let v = l.borrow().clone();
        assert_eq!(
            v,
            vec![
                RuntimeValue::Int(3),
                RuntimeValue::Int(4),
                RuntimeValue::Int(5)
            ]
        );
    }

    #[test]
    fn eval_map_of() {
        let src = r#"(map-of "a" 1 "b" 2)"#;
        let result = run(src);
        assert!(matches!(&result, RuntimeValue::Map(_)), "expected map");
        let RuntimeValue::Map(m) = result else {
            return;
        };
        use crate::values::MapKey;
        let map = m.borrow();
        assert_eq!(
            map.get(&MapKey::Str("a".into())),
            Some(&RuntimeValue::Int(1))
        );
        assert_eq!(
            map.get(&MapKey::Str("b".into())),
            Some(&RuntimeValue::Int(2))
        );
    }

    #[test]
    fn eval_gensym_is_unique() {
        let src = "(eq (gensym) (gensym))";
        assert_eq!(run(src), RuntimeValue::Bool(false));
    }

    #[test]
    fn parse_error_on_bad_input() {
        let errors = parse("(add 1 @bad)").expect_err("expected error");
        assert!(!errors.is_empty());
    }
}
