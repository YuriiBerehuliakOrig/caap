//! Lowering: ParsedForm/AstNode -> IR. Split out of the frontend module root.
use super::*;

use crate::error::{CaapError, CaapResult};
use crate::graph::{GraphBuilder, IRGraph};
use crate::ir::{IrLiteralData, NodeId};
use crate::source::{SourceSpan, SourceSpanLocator};
use caap_peg::AstNode;

/// ParsedForm→IR lowering recurses with the form's nesting depth; grow the
/// native stack on demand (same policy as the evaluator and the CST converter).
pub(super) fn lower_parsed_form(
    form: &ParsedForm,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> CaapResult<NodeId> {
    crate::eval::grow_stack(|| lower_parsed_form_inner(form, b, labels))
}

fn lower_parsed_form_inner(
    form: &ParsedForm,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> CaapResult<NodeId> {
    match form {
        ParsedForm::List { items, span } => {
            if items.is_empty() {
                let id = b.try_literal(IrLiteralData::Null)?;
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
                Some("set!") => return lower_parsed_assignment(items, span, b, labels),
                Some("set") if items.len() == 3 && parsed_symbol_text(&items[1]).is_some() => {
                    return Err(CaapError::ir(
                        "variable assignment uses set!, not set; set is reserved for collection mutation",
                    ));
                }
                _ => {}
            }
            let mut ids = Vec::with_capacity(items.len());
            for item in items {
                ids.push(lower_parsed_form(item, b, labels)?);
            }
            let callee = ids[0];
            let args = ids[1..].to_vec();
            let id = b.try_call(callee, args)?;
            b.graph.set_source_span(id, span.clone())?;
            Ok(id)
        }
        ParsedForm::Symbol { text, span } => {
            let id = b.try_name(text.clone())?;
            b.graph.set_source_span(id, span.clone())?;
            Ok(id)
        }
        ParsedForm::String { value, span, .. } => {
            let id = b.try_literal(IrLiteralData::Str(value.clone()))?;
            b.graph.set_source_span(id, span.clone())?;
            Ok(id)
        }
        ParsedForm::Integer { value, span, .. } => {
            let id = b.try_literal(IrLiteralData::Int(*value))?;
            b.graph.set_source_span(id, span.clone())?;
            Ok(id)
        }
        ParsedForm::Float { value, span, .. } => {
            let id = b.try_literal(IrLiteralData::Float(*value))?;
            b.graph.set_source_span(id, span.clone())?;
            Ok(id)
        }
        ParsedForm::Boolean { value, span } => {
            let id = b.try_literal(IrLiteralData::Bool(*value))?;
            b.graph.set_source_span(id, span.clone())?;
            Ok(id)
        }
        ParsedForm::Null { span } => {
            let id = b.try_literal(IrLiteralData::Null)?;
            b.graph.set_source_span(id, span.clone())?;
            Ok(id)
        }
    }
}

pub(super) fn parsed_head_symbol(items: &[ParsedForm]) -> Option<&str> {
    match items.first() {
        Some(ParsedForm::Symbol { text, .. }) => Some(text.as_str()),
        _ => None,
    }
}

pub(super) fn parsed_symbol_text(form: &ParsedForm) -> Option<&str> {
    match form {
        ParsedForm::Symbol { text, .. } => Some(text.as_str()),
        _ => None,
    }
}

pub(super) fn parsed_string_text(form: &ParsedForm) -> Option<&str> {
    match form {
        ParsedForm::String { value, .. } => Some(value.as_str()),
        _ => None,
    }
}

/// Lower a lambda parameter form to a tuple-of-name-literals, mirroring the
/// AST-path `lower_params_list`: a bare symbol `args` becomes a single rest
/// parameter `&args`, and a dotted tail `(a b . rest)` marks `rest` as a rest
/// parameter (`&rest`).
pub(super) fn lower_parsed_params_list(
    form: &ParsedForm,
    b: &mut GraphBuilder,
) -> CaapResult<NodeId> {
    // Bare-symbol params: `(lambda args ...)` binds all arguments under `args`.
    if let ParsedForm::Symbol { text, span } = form {
        let id = b.try_literal(IrLiteralData::Tuple(vec![IrLiteralData::Str(format!(
            "&{text}"
        ))]))?;
        b.graph.set_source_span(id, span.clone())?;
        return Ok(id);
    }
    // Null params = zero parameters: `()` IS null in CAAP, and the eval-level
    // contract (`extract_param_names`) already accepts `LiteralNode(Null)` as
    // "no parameters" — post-parse producers (the stdlib expander) legally
    // emit that shape, so the lowering path must accept it too.
    if let ParsedForm::Null { span } = form {
        let id = b.try_literal(IrLiteralData::Tuple(Vec::new()))?;
        b.graph.set_source_span(id, span.clone())?;
        return Ok(id);
    }
    let items = parsed_list_items(form)
        .ok_or_else(|| CaapError::ir("lambda params: expected list or symbol"))?;
    let mut params = Vec::with_capacity(items.len());
    let mut i = 0;
    while i < items.len() {
        let name =
            parsed_symbol_text(&items[i]).ok_or_else(|| CaapError::ir("param: expected symbol"))?;
        if name == "." {
            if i + 2 != items.len() {
                return Err(CaapError::ir(
                    "lambda params: '.' must be followed by exactly one rest parameter",
                ));
            }
            let rest = parsed_symbol_text(&items[i + 1])
                .ok_or_else(|| CaapError::ir("param: expected symbol"))?;
            params.push(IrLiteralData::Str(format!("&{rest}")));
            i += 2;
        } else {
            params.push(IrLiteralData::Str(name.to_string()));
            i += 1;
        }
    }
    let id = b.try_literal(IrLiteralData::Tuple(params))?;
    b.graph.set_source_span(id, form.span().clone())?;
    Ok(id)
}

pub(super) fn parsed_list_items(form: &ParsedForm) -> Option<&[ParsedForm]> {
    match form {
        ParsedForm::List { items, .. } => Some(items.as_slice()),
        _ => None,
    }
}

pub(super) fn lower_parsed_body_expr(
    body_forms: &[ParsedForm],
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> CaapResult<NodeId> {
    match body_forms.len() {
        0 => b.try_literal(IrLiteralData::Null),
        1 => lower_parsed_form(&body_forms[0], b, labels),
        _ => {
            let do_fn = b.try_name("do")?;
            let mut args = Vec::with_capacity(body_forms.len());
            for form in body_forms {
                args.push(lower_parsed_form(form, b, labels)?);
            }
            b.try_call(do_fn, args)
        }
    }
}

pub(super) fn lower_parsed_lambda(
    items: &[ParsedForm],
    span: &SourceSpan,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> CaapResult<NodeId> {
    if items.len() < 2 {
        return Err(CaapError::ir("lambda: requires at least a params list"));
    }
    let params_id = lower_parsed_params_list(&items[1], b)?;
    let body_id = lower_parsed_body_expr(&items[2..], b, labels)?;
    let lambda_fn = b.try_name("lambda")?;
    let id = b.try_call(lambda_fn, vec![params_id, body_id])?;
    b.graph.set_source_span(id, span.clone())?;
    Ok(id)
}

pub(super) fn lower_parsed_block(
    items: &[ParsedForm],
    span: &SourceSpan,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> CaapResult<NodeId> {
    if items.len() < 2 {
        return Err(CaapError::ir("block: requires a body"));
    }
    let block_id = b.graph.allocate_id()?;
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
    let block_fn = b.try_name("block")?;
    b.try_call_with_id(block_id, block_fn, body_ids, None)?;
    b.graph.set_source_span(block_id, span.clone())?;
    Ok(block_id)
}

pub(super) fn lower_parsed_bind(
    items: &[ParsedForm],
    span: &SourceSpan,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> CaapResult<Option<NodeId>> {
    if items.len() < 2 {
        return Ok(None);
    }
    let Some(bindings) = parsed_list_items(&items[1]) else {
        return Ok(None);
    };
    if bindings.is_empty() {
        return lower_parsed_body_expr(&items[2..], b, labels).map(Some);
    }
    let mut binding_triples = Vec::with_capacity(bindings.len());
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
        binding_triples.push((name.to_string(), pair[0].span().clone(), &pair[1]));
    }
    let mut args = Vec::with_capacity(binding_triples.len() * 2 + 1);
    for (name, name_span, value) in binding_triples {
        let name_id = b.try_literal(IrLiteralData::Str(name.to_string()))?;
        b.graph.set_source_span(name_id, name_span)?;
        args.push(name_id);
        args.push(lower_parsed_form(value, b, labels)?);
    }
    args.push(lower_parsed_body_expr(&items[2..], b, labels)?);
    let bind_fn = b.try_name("bind")?;
    let id = b.try_call(bind_fn, args)?;
    b.graph.set_source_span(id, span.clone())?;
    Ok(Some(id))
}

pub(super) fn lower_parsed_leave(
    items: &[ParsedForm],
    span: &SourceSpan,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> CaapResult<NodeId> {
    if items.len() < 2 {
        return Err(CaapError::ir("leave: requires at least a label"));
    }
    let leave_fn = b.try_name("leave")?;
    let mut args = Vec::new();
    if let Some(label) = parsed_symbol_text(&items[1]) {
        let block_id = *labels
            .get(label)
            .ok_or_else(|| CaapError::ir(format!("leave: unknown block label '{label}'")))?;
        args.push(b.try_literal(IrLiteralData::Int(block_id as i64))?);
    } else if let Some(label) = parsed_string_text(&items[1]) {
        if let Some(block_id) = labels.get(label).copied() {
            args.push(b.try_literal(IrLiteralData::Int(block_id as i64))?);
        } else {
            args.push(lower_parsed_form(&items[1], b, labels)?);
        }
    } else {
        args.push(lower_parsed_form(&items[1], b, labels)?);
    }
    for item in &items[2..] {
        args.push(lower_parsed_form(item, b, labels)?);
    }
    let id = b.try_call(leave_fn, args)?;
    b.graph.set_source_span(id, span.clone())?;
    Ok(id)
}

pub(super) fn lower_parsed_assignment(
    items: &[ParsedForm],
    span: &SourceSpan,
    b: &mut GraphBuilder,
    labels: &mut std::collections::HashMap<String, NodeId>,
) -> CaapResult<NodeId> {
    if items.len() == 3 {
        if let Some(varname) = parsed_symbol_text(&items[1]) {
            let assign_fn = b.try_internal_name("assign_lexical")?;
            let target = b.try_name(varname)?;
            if let ParsedForm::Symbol { span, .. } = &items[1] {
                b.graph.set_source_span(target, span.clone())?;
            }
            let val_id = lower_parsed_form(&items[2], b, labels)?;
            let id = b.try_call(assign_fn, vec![target, val_id])?;
            b.graph.set_source_span(id, span.clone())?;
            return Ok(id);
        }
    }
    Err(CaapError::ir(
        "set! expects exactly a symbol name and a value expression",
    ))
}

// ── AST → ParsedForm conversion ────────────────────────────────────────────

pub(super) fn ast_to_parsed_source(
    root: &AstNode,
    source: &str,
    original_source: &str,
    source_offset: usize,
    source_path: Option<&str>,
) -> CaapResult<ParsedSource> {
    let line_offsets = compute_line_offsets(original_source);
    let forms = match root.rule.as_str() {
        "forms" => root
            .children
            .iter()
            .map(|form| ast_to_parsed_form(form, source, &line_offsets, source_offset, source_path))
            .collect::<CaapResult<Vec<_>>>()?,
        "form" | "list" | "symbol" | "string" | "integer" | "boolean" | "null" => {
            vec![ast_to_parsed_form(
                root,
                source,
                &line_offsets,
                source_offset,
                source_path,
            )?]
        }
        other => {
            return Err(CaapError::parse(format!(
                "unexpected AST root rule: {other}"
            )))
        }
    };
    Ok(ParsedSource { forms })
}

/// CST→ParsedForm conversion recurses with the input's nesting depth; grow the
/// native stack on demand so MAX_SURFACE_NESTING_DEPTH stays the only limit.
pub(super) fn ast_to_parsed_form(
    node: &AstNode,
    source: &str,
    line_offsets: &[usize],
    source_offset: usize,
    source_path: Option<&str>,
) -> CaapResult<ParsedForm> {
    crate::eval::grow_stack(|| {
        ast_to_parsed_form_inner(node, source, line_offsets, source_offset, source_path)
    })
}

fn ast_to_parsed_form_inner(
    node: &AstNode,
    source: &str,
    line_offsets: &[usize],
    source_offset: usize,
    source_path: Option<&str>,
) -> CaapResult<ParsedForm> {
    match node.rule.as_str() {
        "form" => match node.children.first() {
            Some(child) => {
                ast_to_parsed_form(child, source, line_offsets, source_offset, source_path)
            }
            None => Err(CaapError::parse(format!(
                "empty 'form' node at {}..{}",
                node.span.start, node.span.end
            ))),
        },
        "list" => {
            let start = content_start(source, node.span.start);
            let span = source_span_for_offsets(
                start,
                node.span.end,
                line_offsets,
                source_offset,
                source_path,
            )?;
            let items = node
                .children
                .iter()
                .map(|child| {
                    ast_to_parsed_form(child, source, line_offsets, source_offset, source_path)
                })
                .collect::<CaapResult<Vec<_>>>()?;
            Ok(ParsedForm::List { items, span })
        }
        "symbol" => {
            let start = content_start(source, node.span.start);
            let span = source_span_for_offsets(
                start,
                node.span.end,
                line_offsets,
                source_offset,
                source_path,
            )?;
            Ok(ParsedForm::Symbol {
                text: source[start..node.span.end].to_string(),
                span,
            })
        }
        "string" => {
            let start = content_start(source, node.span.start);
            let raw = source[start..node.span.end].to_string();
            let content = &raw[1..raw.len() - 1];
            let span = source_span_for_offsets(
                start,
                node.span.end,
                line_offsets,
                source_offset,
                source_path,
            )?;
            Ok(ParsedForm::String {
                value: unescape(content)?,
                raw,
                span,
            })
        }
        "integer" => {
            let start = content_start(source, node.span.start);
            let raw = source[start..node.span.end].to_string();
            let span = source_span_for_offsets(
                start,
                node.span.end,
                line_offsets,
                source_offset,
                source_path,
            )?;
            // The grammar "integer" rule also matches float literals; mirror the
            // IR lowering path (a '.', 'e', or 'E' makes it a float).
            if raw.contains('.') || raw.contains('e') || raw.contains('E') {
                let value = raw
                    .parse()
                    .map_err(|error| CaapError::parse(format!("invalid float '{raw}': {error}")))?;
                Ok(ParsedForm::Float { value, raw, span })
            } else {
                let value = raw.parse().map_err(|error| {
                    CaapError::parse(format!("invalid integer '{raw}': {error}"))
                })?;
                Ok(ParsedForm::Integer { value, raw, span })
            }
        }
        "boolean" => {
            let start = content_start(source, node.span.start);
            let span = source_span_for_offsets(
                start,
                node.span.end,
                line_offsets,
                source_offset,
                source_path,
            )?;
            Ok(ParsedForm::Boolean {
                value: &source[start..node.span.end] == "true",
                span,
            })
        }
        "null" => {
            let start = content_start(source, node.span.start);
            let span = source_span_for_offsets(
                start,
                node.span.end,
                line_offsets,
                source_offset,
                source_path,
            )?;
            Ok(ParsedForm::Null { span })
        }
        other => Err(CaapError::parse(format!(
            "unexpected AST rule '{other}' at {}..{}",
            node.span.start, node.span.end
        ))),
    }
}

// ── AST → IRGraph conversion ───────────────────────────────────────────────

pub(super) fn ast_to_ir(
    root: &AstNode,
    source: &str,
    original_source: &str,
    source_offset: usize,
    source_path: Option<&str>,
) -> CaapResult<IRGraph> {
    // Single lowering subsystem: project the AST to the typed surface model, then
    // lower that to IR. The same `ParsedForm` → IR path serves the static parser
    // and dynamic surface syntax, so lowering logic lives in exactly one place.
    let parsed = ast_to_parsed_source(root, source, original_source, source_offset, source_path)?;
    super::parsed_source_to_ir(&parsed)
}

// ── Special-form helpers ───────────────────────────────────────────────────

pub(super) fn source_span_for_offsets(
    start: usize,
    end: usize,
    line_offsets: &[usize],
    source_offset: usize,
    source_path: Option<&str>,
) -> CaapResult<SourceSpan> {
    let start = start + source_offset;
    let end = end + source_offset;
    let (start_line, start_col) = line_col(line_offsets, start);
    let (end_line, end_col) = line_col(line_offsets, end);
    SourceSpan::with_locator(
        Some(SourceSpanLocator {
            file_id: None,
            path: source_path.map(str::to_string),
        }),
        start,
        end,
        start_line,
        start_col,
        end_line,
        end_col,
    )
}

pub(super) fn compute_line_offsets(source: &str) -> Vec<usize> {
    let mut offsets = vec![0usize];
    for (idx, ch) in source.char_indices() {
        if ch == '\n' {
            offsets.push(idx + 1);
        }
    }
    offsets
}

pub(super) fn line_col(line_offsets: &[usize], pos: usize) -> (usize, usize) {
    let idx = line_offsets.partition_point(|&offset| offset <= pos);
    let line_idx = idx.saturating_sub(1);
    (line_idx + 1, pos - line_offsets[line_idx] + 1)
}

/// Advance `pos` past CAAP trivia (whitespace, `;` comments, `#|…|#`, `/*…*/`).
pub(super) fn content_start(source: &str, mut pos: usize) -> usize {
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

pub(super) fn unescape(s: &str) -> CaapResult<String> {
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
            None => {
                return Err(CaapError::parse(
                    "unterminated escape sequence in string literal",
                ))
            }
        }
    }
    Ok(out)
}

pub(super) fn escape_string_literal(value: &str) -> String {
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
