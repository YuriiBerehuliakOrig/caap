//! Document symbols (outline), hover, and go-to-definition.

#![allow(deprecated)]

use std::collections::HashSet;

use caap_core::{ParsedForm, SourceSpan};
use lsp_types::{
    CompletionItem, CompletionItemKind, DocumentHighlight, DocumentHighlightKind, DocumentSymbol,
    InlayHint, InlayHintKind, InlayHintLabel, InsertTextFormat, Location, ParameterInformation,
    ParameterLabel, Position, Range, SignatureHelp, SignatureInformation, SymbolKind, TextEdit,
    Uri, WorkspaceEdit,
};

use crate::analyze::{position_in_span, span_to_range, Analysis, Definition, DefinitionKind};

/// Special-form snippet bodies, expanded after the user's opening `(`. Tabstops
/// follow LSP `InsertTextFormat::SNIPPET` syntax.
const SNIPPETS: &[(&str, &str)] = &[
    ("lambda", "lambda (${1:params}) ${2:body}"),
    ("bind", "bind ${1:name} ${2:value}"),
    ("if", "if ${1:cond} ${2:then} ${3:else}"),
    ("while", "while ${1:cond} ${2:body}"),
    ("do", "do ${1:body}"),
    (
        "define_class",
        "define_class ${1:registry} \"${2:Name}\" ${3:parent}",
    ),
];

pub fn document_symbols(analysis: &Analysis) -> Vec<DocumentSymbol> {
    // Nest by span containment: a definition whose defining form lies inside
    // another's (e.g. a function inside a `module`) becomes its child, so the
    // outline mirrors the file's structure rather than a flat list.
    let mut defs: Vec<&Definition> = analysis.definitions.iter().collect();
    // Containers first: earlier start wins; on a tie, the wider span (later end)
    // is the container and must precede what it contains.
    defs.sort_by(|a, b| {
        a.form_span
            .start
            .cmp(&b.form_span.start)
            .then(b.form_span.end.cmp(&a.form_span.end))
    });

    let mut roots: Vec<DocumentSymbol> = Vec::new();
    // Stack of indices into the *children* chain currently open. Each entry is a
    // path from a root down to the open container, letting us push deeper.
    let mut stack: Vec<DocumentSymbol> = Vec::new();
    let mut spans: Vec<(usize, usize)> = Vec::new();

    for def in defs {
        let (start, end) = (def.form_span.start, def.form_span.end);
        // Close any open containers that don't enclose this definition.
        while let Some(&(_, top_end)) = spans.last() {
            if start >= top_end {
                spans.pop();
                if let Some(finished) = stack.pop() {
                    attach(&mut roots, &mut stack, finished);
                }
            } else {
                break;
            }
        }
        stack.push(definition_to_symbol(def));
        spans.push((start, end));
    }
    while let Some(finished) = stack.pop() {
        spans.pop();
        attach(&mut roots, &mut stack, finished);
    }
    roots
}

/// Append `sym` as a child of the current innermost open container, or as a
/// new root when nothing is open.
fn attach(roots: &mut Vec<DocumentSymbol>, stack: &mut [DocumentSymbol], sym: DocumentSymbol) {
    if let Some(parent) = stack.last_mut() {
        parent.children.get_or_insert_with(Vec::new).push(sym);
    } else {
        roots.push(sym);
    }
}

fn definition_to_symbol(def: &Definition) -> DocumentSymbol {
    let kind = match def.kind {
        DefinitionKind::Function => SymbolKind::FUNCTION,
        DefinitionKind::Variable => SymbolKind::VARIABLE,
        DefinitionKind::Macro => SymbolKind::FUNCTION,
        DefinitionKind::Class => SymbolKind::CLASS,
        DefinitionKind::Interface => SymbolKind::INTERFACE,
        DefinitionKind::Module => SymbolKind::MODULE,
    };
    DocumentSymbol {
        name: def.name.clone(),
        detail: Some(def.detail.clone()),
        kind,
        tags: None,
        deprecated: None,
        range: span_to_range(&def.form_span),
        selection_range: span_to_range(&def.name_span),
        children: None,
    }
}

pub fn hover_at(analysis: &Analysis, source: &str, pos: Position) -> Option<String> {
    let word = word_at(analysis, source, pos)?;
    let body = if let Some(def) = analysis.definition_for(&word) {
        def.detail.clone()
    } else {
        format!("`{word}` — symbol")
    };
    Some(body)
}

pub fn definition_at(
    analysis: &Analysis,
    source: &str,
    uri: &Uri,
    pos: Position,
) -> Option<Location> {
    let word = word_at(analysis, source, pos)?;
    // 1. Top-level definition (function / type / module / top-level binding).
    if let Some(def) = analysis.definition_for(&word) {
        return Some(Location {
            uri: uri.clone(),
            range: span_to_range(&def.name_span),
        });
    }
    // 2. Function parameter or local binding: resolve within the enclosing
    //    definition to its declaration (the first in-scope occurrence).
    let range = local_declaration_range(analysis, source, &word, pos)?;
    Some(Location {
        uri: uri.clone(),
        range,
    })
}

/// Resolve a parameter / local `bind` name to its declaration: the first
/// occurrence of the word inside the smallest enclosing definition's form.
/// Bindings precede uses lexically, so the first in-scope occurrence is the
/// declaration. Callees (function/builtin calls) are excluded so we never jump
/// a call like `get`/`io` to a stray first occurrence.
fn local_declaration_range(
    analysis: &Analysis,
    source: &str,
    word: &str,
    pos: Position,
) -> Option<Range> {
    let enclosing = analysis
        .definitions
        .iter()
        .filter(|def| range_contains(&span_to_range(&def.form_span), pos))
        .min_by_key(|def| span_area(&def.form_span))?;

    let is_param = enclosing.params.iter().any(|p| p == word);
    if !is_param {
        // Not a known parameter: only treat it as a local variable if it is
        // never used as a call target anywhere (otherwise it is a function or
        // builtin, which has no in-body declaration to jump to).
        let used_as_callee = analysis
            .definitions
            .iter()
            .any(|def| def.calls.iter().any(|call| call.name == word));
        if used_as_callee {
            return None;
        }
    }

    let form = span_to_range(&enclosing.form_span);
    occurrences(source, word)
        .into_iter()
        .find(|occ| range_within(&form, occ))
}

/// True when `inner` is fully contained in `outer` (line/char ordered).
fn range_within(outer: &Range, inner: &Range) -> bool {
    (inner.start.line, inner.start.character) >= (outer.start.line, outer.start.character)
        && (inner.end.line, inner.end.character) <= (outer.end.line, outer.end.character)
}

/// Rough size of a span for "smallest enclosing" selection: line span dominates,
/// column breaks ties.
fn span_area(span: &SourceSpan) -> (u32, u32) {
    let line = span.end_line.saturating_sub(span.start_line) as u32;
    let col = span.end_col.saturating_sub(span.start_col) as u32;
    (line, col)
}

/// The lexical reach of the symbol under the cursor, used to bound references /
/// rename / highlight so a local never bleeds into an unrelated same-named global.
pub enum OccurrenceScope {
    /// A parameter or local binding: occurrences are confined to the enclosing
    /// definition's form, in the current file only.
    Local { form_span: SourceSpan },
    /// A top-level definition, callee, or top-level reference: search workspace-wide.
    Global,
}

/// Classify the identifier under the cursor as a local (parameter / local
/// binding) or a global. Heuristic, mirroring `local_declaration_range`: it is
/// **global** when it names a top-level definition in this file or is used as a
/// call target anywhere (a function/builtin), and **local** when it is a
/// parameter of, or an otherwise-unknown name inside, the smallest enclosing
/// definition. Not a full scope model — shadowing within one definition is still
/// merged — but it removes the dangerous local→global bleed.
pub fn occurrence_scope(
    analysis: &Analysis,
    source: &str,
    pos: Position,
) -> Option<(String, OccurrenceScope)> {
    let word = word_at(analysis, source, pos)?;
    // A top-level definition in this file is workspace-visible.
    if analysis.definition_for(&word).is_some() {
        return Some((word, OccurrenceScope::Global));
    }
    let Some(enclosing) = analysis
        .definitions
        .iter()
        .filter(|def| range_contains(&span_to_range(&def.form_span), pos))
        .min_by_key(|def| span_area(&def.form_span))
    else {
        // Cursor outside any definition body: treat as a global reference.
        return Some((word, OccurrenceScope::Global));
    };
    if enclosing.params.iter().any(|p| p == &word) {
        return Some((
            word,
            OccurrenceScope::Local {
                form_span: enclosing.form_span.clone(),
            },
        ));
    }
    // Used as a call target somewhere → a function/builtin, not a local.
    let used_as_callee = analysis
        .definitions
        .iter()
        .any(|def| def.calls.iter().any(|call| call.name == word));
    if used_as_callee {
        return Some((word, OccurrenceScope::Global));
    }
    Some((
        word,
        OccurrenceScope::Local {
            form_span: enclosing.form_span.clone(),
        },
    ))
}

/// Occurrences of `word` confined to the line range of `form_span` (one file).
pub fn occurrences_in_span(source: &str, word: &str, form_span: &SourceSpan) -> Vec<Range> {
    let form = span_to_range(form_span);
    occurrences(source, word)
        .into_iter()
        .filter(|occ| range_within(&form, occ))
        .collect()
}

/// Highlight every occurrence of the identifier under the cursor. A local /
/// parameter is confined to its enclosing definition; a global spans the file.
pub fn document_highlights(
    analysis: &Analysis,
    source: &str,
    pos: Position,
) -> Option<Vec<DocumentHighlight>> {
    let (word, scope) = occurrence_scope(analysis, source, pos)?;
    let ranges = match scope {
        OccurrenceScope::Local { form_span } => occurrences_in_span(source, &word, &form_span),
        OccurrenceScope::Global => occurrences(source, &word),
    };
    let highlights = ranges
        .into_iter()
        .map(|range| DocumentHighlight {
            range,
            kind: Some(DocumentHighlightKind::TEXT),
        })
        .collect();
    Some(highlights)
}

/// All references to the identifier under the cursor, as `Location`s in the
/// current document. (Workspace-wide references are a future extension.)
pub fn references(
    analysis: &Analysis,
    source: &str,
    uri: &Uri,
    pos: Position,
) -> Option<Vec<Location>> {
    let word = word_at(analysis, source, pos)?;
    let locations = occurrences(source, &word)
        .into_iter()
        .map(|range| Location {
            uri: uri.clone(),
            range,
        })
        .collect();
    Some(locations)
}

/// Completion items: the document's known definitions (functions/types/etc.)
/// plus core special forms and defining keywords. The client filters by the
/// typed prefix, so we return the full set.
pub fn completions(analysis: &Analysis) -> Vec<CompletionItem> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for def in &analysis.definitions {
        if seen.insert(def.name.clone()) {
            items.push(CompletionItem {
                label: def.name.clone(),
                kind: Some(completion_kind(def.kind)),
                detail: Some(def.detail.clone()),
                ..Default::default()
            });
        }
    }
    // Special-form snippets (body templates, no outer paren: head completion
    // follows the user's `(`). Inserted before the plain keywords so the `seen`
    // set keeps the snippet variant for these forms.
    for (label, body) in SNIPPETS {
        if seen.insert((*label).to_string()) {
            items.push(CompletionItem {
                label: (*label).to_string(),
                kind: Some(CompletionItemKind::SNIPPET),
                insert_text: Some((*body).to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                detail: Some(format!("{label} (snippet)")),
                ..Default::default()
            });
        }
    }
    // Reserved words + builtins come from the kernel vocabulary (see
    // `crate::vocab`), not a hardcoded list, so completion tracks the real
    // language: special forms and literals as keywords, builtins as functions.
    for kw in crate::vocab::special_forms() {
        if seen.insert(kw.clone()) {
            items.push(CompletionItem {
                label: kw.clone(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            });
        }
    }
    for lit in crate::vocab::literals() {
        if seen.insert((*lit).to_string()) {
            items.push(CompletionItem {
                label: (*lit).to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            });
        }
    }
    for builtin in crate::vocab::builtins() {
        if seen.insert(builtin.clone()) {
            items.push(CompletionItem {
                label: builtin.clone(),
                kind: Some(CompletionItemKind::FUNCTION),
                ..Default::default()
            });
        }
    }
    items
}

fn completion_kind(kind: DefinitionKind) -> CompletionItemKind {
    match kind {
        DefinitionKind::Function => CompletionItemKind::FUNCTION,
        DefinitionKind::Macro => CompletionItemKind::FUNCTION,
        DefinitionKind::Class => CompletionItemKind::CLASS,
        DefinitionKind::Interface => CompletionItemKind::INTERFACE,
        DefinitionKind::Module => CompletionItemKind::MODULE,
        DefinitionKind::Variable => CompletionItemKind::VARIABLE,
    }
}

/// Validate that there is a renameable identifier under the cursor, returning
/// its range (for `textDocument/prepareRename`).
pub fn prepare_rename(analysis: &Analysis, source: &str, pos: Position) -> Option<Range> {
    let word = word_at(analysis, source, pos)?;
    occurrences(source, &word)
        .into_iter()
        .find(|range| range_contains(range, pos))
}

/// Rename every occurrence of the identifier under the cursor in the current
/// document. (Workspace-wide rename is a future extension.)
// `lsp_types::Uri` is effectively immutable here; using it as a map key is the
// idiomatic LSP pattern and does not mutate through interior cells.
#[allow(clippy::mutable_key_type)]
pub fn rename(
    analysis: &Analysis,
    source: &str,
    uri: &Uri,
    pos: Position,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    let word = word_at(analysis, source, pos)?;
    let edits: Vec<TextEdit> = occurrences(source, &word)
        .into_iter()
        .map(|range| TextEdit {
            range,
            new_text: new_name.to_string(),
        })
        .collect();
    if edits.is_empty() {
        return None;
    }
    let mut changes = std::collections::HashMap::new();
    changes.insert(uri.clone(), edits);
    Some(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    })
}

/// Signature help for the call enclosing the cursor: the callee name and its
/// lambda parameters (resolved from the parsed AST), with the active argument
/// highlighted. Works for files the s-expr parser can read.
pub fn signature_help(analysis: &Analysis, pos: Position) -> Option<SignatureHelp> {
    // Grammar-extended files have no base AST for the body; resolve from the
    // grammar-aware bootstrap call sites + the callee's recorded params instead.
    if analysis.has_surface_structure() {
        return signature_help_from_definitions(analysis, pos);
    }
    let (head, active) = enclosing_call(analysis, pos)?;
    let params = find_lambda_params(&analysis.parsed.forms, &head)?;
    let label = if params.is_empty() {
        format!("({head})")
    } else {
        format!("({head} {})", params.join(" "))
    };
    let parameters: Vec<ParameterInformation> = params
        .iter()
        .map(|p| ParameterInformation {
            label: ParameterLabel::Simple(p.clone()),
            documentation: None,
        })
        .collect();
    let active = (active as u32).min(params.len().saturating_sub(1) as u32);
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: None,
            parameters: Some(parameters),
            active_parameter: Some(active),
        }],
        active_signature: Some(0),
        active_parameter: Some(active),
    })
}

/// Inlay parameter-name hints: at each call whose callee is a function defined
/// in this file, prefix each argument with `param:`. Limited to calls within
/// the requested `range`.
pub fn inlay_hints(analysis: &Analysis, range: Range) -> Vec<InlayHint> {
    if analysis.has_surface_structure() {
        return inlay_hints_from_definitions(analysis, range);
    }
    let mut hints = Vec::new();
    for form in &analysis.parsed.forms {
        collect_inlay_hints(form, &analysis.parsed.forms, range, &mut hints);
    }
    hints
}

/// Signature help for grammar-extended files: pick the innermost recorded call
/// site (callee token or one of its argument spans) containing the cursor, then
/// take the callee definition's parameter names.
fn signature_help_from_definitions(analysis: &Analysis, pos: Position) -> Option<SignatureHelp> {
    let resolves = |name: &str| analysis.definitions.iter().any(|d| d.name == name);
    let mut chosen: Option<&crate::analyze::CallRef> = None;
    let mut best_extent = usize::MAX;
    for def in &analysis.definitions {
        for call in &def.calls {
            // Only consider calls to known functions (builtins/unresolved names
            // have no signature), and only those enclosing the cursor.
            if !resolves(&call.name) {
                continue;
            }
            // A placeholder arg span (offset before the callee — the `(0,0)`
            // sentinel a coarse grammar emits for a span-less argument) must not
            // register a cursor hit, or signature help spuriously triggers at the
            // file head. It still occupies its positional slot for param mapping.
            let is_real = |s: &SourceSpan| s.start >= call.span.start;
            let on_callee = position_in_span(pos, &call.span);
            let on_arg = call
                .arg_spans
                .iter()
                .any(|s| is_real(s) && position_in_span(pos, s));
            if !on_callee && !on_arg {
                continue;
            }
            let end = call
                .arg_spans
                .iter()
                .map(|s| s.end)
                .max()
                .unwrap_or(call.span.end)
                .max(call.span.end);
            let extent = end.saturating_sub(call.span.start);
            if extent < best_extent {
                best_extent = extent;
                chosen = Some(call);
            }
        }
    }
    let call = chosen?;
    let callee = analysis.definitions.iter().find(|d| d.name == call.name)?;
    let params = callee.params.clone();
    let label = if params.is_empty() {
        format!("({})", call.name)
    } else {
        format!("({} {})", call.name, params.join(" "))
    };
    let is_real = |s: &SourceSpan| s.start >= call.span.start;
    let active = call
        .arg_spans
        .iter()
        .position(|s| is_real(s) && position_in_span(pos, s))
        .unwrap_or_else(|| {
            call.arg_spans
                .iter()
                .take_while(|s| {
                    let r = span_to_range(s);
                    (r.end.line, r.end.character) <= (pos.line, pos.character)
                })
                .count()
        });
    let active = (active as u32).min(params.len().saturating_sub(1) as u32);
    let parameters: Vec<ParameterInformation> = params
        .iter()
        .map(|p| ParameterInformation {
            label: ParameterLabel::Simple(p.clone()),
            documentation: None,
        })
        .collect();
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: None,
            parameters: Some(parameters),
            active_parameter: Some(active),
        }],
        active_signature: Some(0),
        active_parameter: Some(active),
    })
}

/// Inlay parameter-name hints for grammar-extended files: at each recorded call
/// whose callee is a function with known params, label each argument span.
fn inlay_hints_from_definitions(analysis: &Analysis, range: Range) -> Vec<InlayHint> {
    let params_by_name: std::collections::HashMap<&str, &[String]> = analysis
        .definitions
        .iter()
        .filter(|d| !d.params.is_empty())
        .map(|d| (d.name.as_str(), d.params.as_slice()))
        .collect();
    let mut hints = Vec::new();
    for def in &analysis.definitions {
        for call in &def.calls {
            let Some(params) = params_by_name.get(call.name.as_str()) else {
                continue;
            };
            for (i, arg_span) in call.arg_spans.iter().enumerate() {
                let Some(param) = params.get(i) else {
                    break;
                };
                if param.starts_with('&') {
                    continue;
                }
                // Skip placeholder spans (an argument can't precede its callee);
                // the slot stays aligned so later args still get the right param.
                if arg_span.start < call.span.start {
                    continue;
                }
                let start = span_to_range(arg_span).start;
                if !range_contains(&range, start) {
                    continue;
                }
                hints.push(InlayHint {
                    position: start,
                    label: InlayHintLabel::String(format!("{param}:")),
                    kind: Some(InlayHintKind::PARAMETER),
                    text_edits: None,
                    tooltip: None,
                    padding_left: None,
                    padding_right: Some(true),
                    data: None,
                });
            }
        }
    }
    hints
}

fn collect_inlay_hints(
    form: &ParsedForm,
    all_forms: &[ParsedForm],
    range: Range,
    hints: &mut Vec<InlayHint>,
) {
    let ParsedForm::List { items, .. } = form else {
        return;
    };
    let head = match items.first() {
        Some(ParsedForm::Symbol { text, .. }) => Some(text.as_str()),
        _ => None,
    };
    // `bind` binding pairs `(name value)` are not calls — recurse into the
    // bound values and the body, but never treat a pair as an argument list.
    if head == Some("bind") {
        if let Some(ParsedForm::List { items: pairs, .. }) = items.get(1) {
            for pair in pairs {
                if let ParsedForm::List { items: kv, .. } = pair {
                    for value in kv.iter().skip(1) {
                        collect_inlay_hints(value, all_forms, range, hints);
                    }
                }
            }
        }
        for body in items.iter().skip(2) {
            collect_inlay_hints(body, all_forms, range, hints);
        }
        return;
    }
    for item in items {
        collect_inlay_hints(item, all_forms, range, hints);
    }
    let Some(text) = head else {
        return;
    };
    let Some(params) = find_lambda_params(all_forms, text) else {
        return;
    };
    for (i, arg) in items[1..].iter().enumerate() {
        if i >= params.len() {
            break;
        }
        // Skip a `&rest`-style trailing parameter.
        if params[i].starts_with('&') {
            continue;
        }
        let start = span_to_range(arg.span()).start;
        if !range_contains(&range, start) {
            continue;
        }
        hints.push(InlayHint {
            position: start,
            label: InlayHintLabel::String(format!("{}:", params[i])),
            kind: Some(InlayHintKind::PARAMETER),
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: Some(true),
            data: None,
        });
    }
}

fn range_contains(range: &Range, pos: Position) -> bool {
    let after_start = (pos.line, pos.character) >= (range.start.line, range.start.character);
    let before_end = (pos.line, pos.character) <= (range.end.line, range.end.character);
    after_start && before_end
}

/// The innermost call (parenthesized list whose head is a symbol) containing
/// `pos`, plus the index of the argument the cursor is on.
fn enclosing_call(analysis: &Analysis, pos: Position) -> Option<(String, usize)> {
    for form in &analysis.parsed.forms {
        if let Some(found) = find_enclosing_call(form, pos) {
            return Some(found);
        }
    }
    None
}

fn find_enclosing_call(form: &ParsedForm, pos: Position) -> Option<(String, usize)> {
    let ParsedForm::List { items, span } = form else {
        return None;
    };
    if !position_in_span(pos, span) {
        return None;
    }
    // Prefer the innermost matching call.
    for item in items {
        if let Some(found) = find_enclosing_call(item, pos) {
            return Some(found);
        }
    }
    let head = match items.first() {
        Some(ParsedForm::Symbol { text, .. }) => text.clone(),
        _ => return None,
    };
    // Active arg = number of arguments that end at or before the cursor.
    let active = items[1..]
        .iter()
        .take_while(|item| {
            let r = span_to_range(item.span());
            (r.end.line, r.end.character) <= (pos.line, pos.character)
        })
        .count();
    Some((head, active))
}

/// Find the parameter list of a top-level `(bind name (lambda (params) ...))`
/// (single or grouped binding) matching `name`.
fn find_lambda_params(forms: &[ParsedForm], name: &str) -> Option<Vec<String>> {
    for form in forms {
        if let Some(params) = lambda_params_in_form(form, name) {
            return Some(params);
        }
    }
    None
}

fn lambda_params_in_form(form: &ParsedForm, name: &str) -> Option<Vec<String>> {
    // A `bind` introducing `name`: the kernel describes the binding shape, so
    // look the name up among the introduced locals and read its lambda params.
    if let Some(names) = caap_core::language::introduced_names(form) {
        return names
            .iter()
            .find(|n| n.role == caap_core::language::NameRole::Local && n.text == name)
            .and_then(|n| lambda_params(n.value));
    }
    // Otherwise descend through sequencing/grouping forms (`do`, …) to reach a
    // nested binding of `name`.
    let ParsedForm::List { items, .. } = form else {
        return None;
    };
    items
        .iter()
        .skip(1)
        .find_map(|child| lambda_params_in_form(child, name))
}

fn lambda_params(value: Option<&ParsedForm>) -> Option<Vec<String>> {
    // The kernel owns `lambda`'s shape — ask it for the parameter names rather
    // than re-reading "params are the symbols in child 1" here. A `bind`
    // introduces locals (not parameters), so reject it.
    let names = caap_core::language::introduced_names(value?)?;
    if names
        .iter()
        .any(|n| n.role == caap_core::language::NameRole::Local)
    {
        return None;
    }
    Some(names.iter().map(|n| n.text.to_string()).collect())
}

/// The identifier under the cursor (AST symbol or raw-text fallback).
pub fn symbol_at_cursor(analysis: &Analysis, source: &str, pos: Position) -> Option<String> {
    word_at(analysis, source, pos)
}

/// Find every whole-token occurrence of `word` in `source`, skipping string
/// literals and line comments. Grammar-agnostic: works uniformly for plain and
/// grammar-extended files (which have no parsed AST).
pub fn occurrences(source: &str, word: &str) -> Vec<Range> {
    if word.is_empty() {
        return Vec::new();
    }
    let is_ident = |c: char| c.is_alphanumeric() || "-_.!?*+<>=/&:$%".contains(c);
    let mut out = Vec::new();
    for (line_idx, line) in source.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        let mut in_string = false;
        while i < chars.len() {
            let c = chars[i];
            if in_string {
                if c == '\\' {
                    i += 2;
                    continue;
                }
                if c == '"' {
                    in_string = false;
                }
                i += 1;
                continue;
            }
            if c == '"' {
                in_string = true;
                i += 1;
                continue;
            }
            if c == ';' {
                break;
            }
            if is_ident(c) {
                let start = i;
                while i < chars.len() && is_ident(chars[i]) {
                    i += 1;
                }
                let token: String = chars[start..i].iter().collect();
                if token == word {
                    out.push(Range {
                        start: Position::new(line_idx as u32, start as u32),
                        end: Position::new(line_idx as u32, i as u32),
                    });
                }
            } else {
                i += 1;
            }
        }
    }
    out
}

/// Resolve the symbol under the cursor. Prefer the parsed surface AST (exact
/// spans); fall back to a raw-text identifier scan when the base s-expr parse
/// produced no forms — the case for grammar-extended files, whose definitions
/// still come from the bootstrap analysis.
fn word_at(analysis: &Analysis, source: &str, pos: Position) -> Option<String> {
    if let Some((text, _span)) = symbol_at(analysis, pos) {
        return Some(text.to_string());
    }
    identifier_at(source, pos)
}

/// Extract the CAAP identifier token spanning `pos` from raw source text.
/// LSP columns are UTF-16 code units; we treat them as `char` offsets, which
/// is exact for the ASCII identifiers CAAP definitions use in practice.
pub fn identifier_at(source: &str, pos: Position) -> Option<String> {
    let line = source.lines().nth(pos.line as usize)?;
    let chars: Vec<char> = line.chars().collect();
    let col = (pos.character as usize).min(chars.len());
    let is_ident = |c: char| c.is_alphanumeric() || "-_.!?*+<>=/&:$%".contains(c);

    let mut start = col;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && is_ident(chars[end]) {
        end += 1;
    }
    if start >= end {
        return None;
    }
    Some(chars[start..end].iter().collect())
}

/// A module/symbol reference the cursor sits on inside an import form.
pub struct ImportTarget {
    pub module: String,
    /// The specific imported symbol under the cursor, when it is not the
    /// module-name string itself (e.g. `println` in `(import-symbols "sys.io"
    /// "println")`).
    pub symbol: Option<String>,
}

/// When `pos` is inside the string arguments of an import form, return the
/// referenced module (the first string argument) and, if the cursor is on a
/// later string, that imported symbol. Used for go-to-definition / hover on
/// `(syntax-import ...)`, `(import-namespace ...)`, `(import-symbols ...)`.
pub fn import_target_at(analysis: &Analysis, pos: Position) -> Option<ImportTarget> {
    for form in &analysis.parsed.forms {
        let ParsedForm::List { items, span } = form else {
            continue;
        };
        if !position_in_span(pos, span) {
            continue;
        }
        let head = match items.first() {
            Some(ParsedForm::Symbol { text, .. }) => text.as_str(),
            _ => continue,
        };
        if !matches!(
            head,
            "syntax_import" | "import_namespace" | "import_symbols" | "import"
        ) {
            continue;
        }
        // Collect the string arguments (module first, then symbols).
        let strings: Vec<(&String, &SourceSpan)> = items[1..]
            .iter()
            .filter_map(|item| match item {
                ParsedForm::String { value, span, .. } => Some((value, span)),
                _ => None,
            })
            .collect();
        let (module, module_span) = strings.first()?;
        let symbol = strings
            .iter()
            .skip(1)
            .find(|(_, span)| position_in_span(pos, span))
            .map(|(value, _)| (*value).clone());
        // Only resolve when the cursor is actually on one of the strings.
        let on_a_string = position_in_span(pos, module_span) || symbol.is_some();
        if !on_a_string {
            continue;
        }
        return Some(ImportTarget {
            module: (*module).clone(),
            symbol,
        });
    }
    None
}

fn symbol_at(analysis: &Analysis, pos: Position) -> Option<(&str, &SourceSpan)> {
    let mut hit: Option<(&str, &SourceSpan)> = None;
    for form in &analysis.parsed.forms {
        visit(form, pos, &mut hit);
        if hit.is_some() {
            break;
        }
    }
    hit
}

fn visit<'a>(form: &'a ParsedForm, pos: Position, hit: &mut Option<(&'a str, &'a SourceSpan)>) {
    match form {
        ParsedForm::List { items, span } => {
            if !position_in_span(pos, span) {
                return;
            }
            for item in items {
                visit(item, pos, hit);
                if hit.is_some() {
                    return;
                }
            }
        }
        ParsedForm::Symbol { text, span } if position_in_span(pos, span) => {
            *hit = Some((text.as_str(), span));
        }
        _ => {}
    }
}
