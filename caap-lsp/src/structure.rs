//! Structural-navigation providers (Horizon 7): folding ranges, selection
//! ranges, and the per-file call graph backing call hierarchy. All three are
//! pure functions over the parsed AST (plus the raw text for comment folding),
//! so they work on any plain s-expr `.caap` file and on the recoverable header
//! of grammar-extended files.

use caap_core::{ParsedForm, ParsedSource, SourceSpan};
use lsp_types::{FoldingRange, FoldingRangeKind, Position, Range, SelectionRange};

use crate::analyze::{position_in_span, span_to_range, Analysis, DefinitionKind};

/// Foldable regions: every multi-line parenthesized form, plus runs of two or
/// more consecutive comment-only lines.
pub fn folding_ranges(analysis: &Analysis, source: &str) -> Vec<FoldingRange> {
    // A balanced-delimiter scan over the raw text covers every multi-line region
    // uniformly — plain s-expr `( … )` forms and grammar-extended `{ … }` blocks
    // alike — so it works whether or not the base parser could read the body.
    let _ = analysis;
    let mut out = Vec::new();
    for span in delimiter_spans(source) {
        if span.end_line > span.start_line {
            out.push(FoldingRange {
                start_line: span.start_line,
                end_line: span.end_line,
                start_character: None,
                end_character: None,
                kind: Some(FoldingRangeKind::Region),
                collapsed_text: None,
            });
        }
    }
    collect_comment_folds(source, &mut out);
    // Nested forms sharing a line range (e.g. a `bind` and the `lambda` it wraps)
    // produce identical folds — keep one per (start, end) line pair.
    out.sort_by_key(|f| (f.start_line, f.end_line));
    out.dedup_by_key(|f| (f.start_line, f.end_line));
    out
}

/// A balanced-delimiter region from a raw-text scan, in 0-based LSP coordinates.
/// Used for grammar-extended files that have no base AST.
struct DelimSpan {
    start_line: u32,
    start_col: u32,
    end_line: u32,
    end_col: u32,
}

/// Scan `source` for balanced `() [] {}` regions, skipping string and `;`
/// comment content. Character-based columns (CAAP source is ASCII).
fn delimiter_spans(source: &str) -> Vec<DelimSpan> {
    let mut stack: Vec<(char, u32, u32)> = Vec::new();
    let mut out = Vec::new();
    let (mut line, mut col) = (0u32, 0u32);
    let mut in_string = false;
    let mut in_comment = false;
    let mut escaped = false;
    for ch in source.chars() {
        if ch == '\n' {
            line += 1;
            col = 0;
            in_comment = false;
            escaped = false;
            continue;
        }
        if in_comment {
            col += 1;
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            col += 1;
            continue;
        }
        match ch {
            ';' => in_comment = true,
            '"' => in_string = true,
            '(' | '[' | '{' => stack.push((ch, line, col)),
            ')' | ']' | '}' => {
                if let Some(&(open, sl, sc)) = stack.last() {
                    if delimiters_match(open, ch) {
                        stack.pop();
                        out.push(DelimSpan {
                            start_line: sl,
                            start_col: sc,
                            end_line: line,
                            end_col: col + 1,
                        });
                    }
                }
            }
            _ => {}
        }
        col += 1;
    }
    out
}

fn delimiters_match(open: char, close: char) -> bool {
    matches!((open, close), ('(', ')') | ('[', ']') | ('{', '}'))
}

/// Group consecutive comment-only lines (first non-whitespace char is `;`) into
/// foldable comment blocks of length two or more.
fn collect_comment_folds(source: &str, out: &mut Vec<FoldingRange>) {
    let mut run_start: Option<u32> = None;
    let mut prev = 0u32;
    let lines = source.lines().collect::<Vec<_>>();
    for (idx, line) in lines.iter().enumerate() {
        let idx = idx as u32;
        let is_comment = line.trim_start().starts_with(';');
        if is_comment {
            if run_start.is_none() {
                run_start = Some(idx);
            }
            prev = idx;
        } else if let Some(start) = run_start.take() {
            if prev > start {
                out.push(comment_fold(start, prev));
            }
        }
    }
    if let Some(start) = run_start {
        if prev > start {
            out.push(comment_fold(start, prev));
        }
    }
}

fn comment_fold(start_line: u32, end_line: u32) -> FoldingRange {
    FoldingRange {
        start_line,
        end_line,
        start_character: None,
        end_character: None,
        kind: Some(FoldingRangeKind::Comment),
        collapsed_text: None,
    }
}

/// For each requested position, the chain of enclosing forms from innermost to
/// outermost (the LSP `SelectionRange` is a linked list via `parent`).
pub fn selection_ranges(
    analysis: &Analysis,
    source: &str,
    positions: &[Position],
) -> Vec<SelectionRange> {
    positions
        .iter()
        .map(|pos| selection_range_at(analysis, source, *pos))
        .collect()
}

fn selection_range_at(analysis: &Analysis, source: &str, pos: Position) -> SelectionRange {
    // Every enclosing range that contains the position. From the base AST when
    // present, else from the raw-text delimiter scan (grammar-extended files).
    // Prefer the base-AST enclosing spans (finer: includes leaf tokens). When
    // nothing in the AST contains the cursor — e.g. inside a grammar-extended
    // body the parser couldn't read — fall back to the raw-text delimiter scan.
    let mut spans: Vec<&SourceSpan> = Vec::new();
    for form in &analysis.parsed.forms {
        collect_containing(form, pos, &mut spans);
    }
    let mut chain: Vec<Range> = if spans.is_empty() {
        delimiter_spans(source)
            .into_iter()
            .map(|s| {
                Range::new(
                    Position::new(s.start_line, s.start_col),
                    Position::new(s.end_line, s.end_col),
                )
            })
            .filter(|r| range_contains(r, pos))
            .collect()
    } else {
        spans.into_iter().map(span_to_range).collect()
    };
    // Largest→smallest so we can link parents outward; drop exact duplicates.
    chain.sort_by_key(|r| std::cmp::Reverse(range_extent(r)));
    chain.dedup();

    let mut node: Option<SelectionRange> = None;
    for range in chain {
        node = Some(SelectionRange {
            range,
            parent: node.map(Box::new),
        });
    }
    node.unwrap_or(SelectionRange {
        range: Range::new(pos, pos),
        parent: None,
    })
}

/// Approximate range size for nesting order: line extent dominates, then columns.
fn range_extent(r: &Range) -> u64 {
    let lines = (r.end.line.saturating_sub(r.start.line)) as u64;
    let cols = (r.end.character.saturating_sub(r.start.character)) as u64;
    (lines << 20) | cols.min((1 << 20) - 1)
}

fn range_contains(r: &Range, pos: Position) -> bool {
    let start = (r.start.line, r.start.character);
    let end = (r.end.line, r.end.character);
    let p = (pos.line, pos.character);
    start <= p && p <= end
}

fn collect_containing<'a>(form: &'a ParsedForm, pos: Position, out: &mut Vec<&'a SourceSpan>) {
    let span = form.span();
    if !position_in_span(pos, span) {
        return;
    }
    out.push(span);
    if let ParsedForm::List { items, .. } = form {
        for item in items {
            collect_containing(item, pos, out);
        }
    }
}

/// A call site found inside a function body: the callee name and the span of
/// the head symbol (the range a `from`/`to` link should reveal).
#[derive(Clone, Debug)]
pub struct CallSite {
    pub callee: String,
    pub span: SourceSpan,
}

/// A top-level callable (lambda-valued `bind` or `defmacro`) and the calls in
/// its body. Backs call hierarchy.
#[derive(Clone, Debug)]
pub struct FunctionInfo {
    pub name: String,
    pub name_span: SourceSpan,
    pub form_span: SourceSpan,
    pub calls: Vec<CallSite>,
}

/// Per-file callables for call hierarchy: the base-AST walk when the file
/// parsed as plain s-expr, otherwise the grammar-aware bootstrap definitions
/// (which carry `form_span`/`calls` for surface-DSL files that have no AST).
pub fn analysis_function_infos(analysis: &Analysis) -> Vec<FunctionInfo> {
    if analysis.has_surface_structure() {
        definition_function_infos(analysis)
    } else {
        function_infos(&analysis.parsed)
    }
}

/// Build `FunctionInfo`s from grammar-aware bootstrap definitions (functions and
/// macros), using their recorded call sites.
pub fn definition_function_infos(analysis: &Analysis) -> Vec<FunctionInfo> {
    analysis
        .definitions
        .iter()
        .filter(|d| matches!(d.kind, DefinitionKind::Function | DefinitionKind::Macro))
        .map(|d| FunctionInfo {
            name: d.name.clone(),
            name_span: d.name_span.clone(),
            form_span: d.form_span.clone(),
            calls: d
                .calls
                .iter()
                .map(|c| CallSite {
                    callee: c.name.clone(),
                    span: c.span.clone(),
                })
                .collect(),
        })
        .collect()
}

/// Every top-level callable in a parsed source, with the calls in its body.
pub fn function_infos(parsed: &ParsedSource) -> Vec<FunctionInfo> {
    let mut out = Vec::new();
    for form in &parsed.forms {
        collect_functions(form, &mut out);
    }
    out
}

fn collect_functions(form: &ParsedForm, out: &mut Vec<FunctionInfo>) {
    let ParsedForm::List { items, span } = form else {
        return;
    };
    let head = match items.first() {
        Some(ParsedForm::Symbol { text, .. }) => text.as_str(),
        _ => return,
    };
    // Kernel `bind`: its named-lambda bindings are functions in the outline. Ask
    // caap_core::language for the binding shape and the lambda body instead of
    // re-reading "bind binds its pairs" / "lambda's body is child 2+" here.
    if let Some(names) = caap_core::language::introduced_names(form) {
        for name in names {
            if name.role != caap_core::language::NameRole::Local {
                continue;
            }
            if let Some(body) = name.value.and_then(caap_core::language::lambda_body) {
                out.push(function_from_body(
                    name.text,
                    name.span,
                    name.form_span,
                    body,
                ));
            }
        }
        return;
    }
    match head {
        "defmacro" | "define_macro" => {
            if let Some(ParsedForm::Symbol {
                text,
                span: name_span,
            }) = items.get(1)
            {
                let mut calls = Vec::new();
                for child in &items[2..] {
                    collect_calls(child, &mut calls);
                }
                out.push(FunctionInfo {
                    name: text.clone(),
                    name_span: name_span.clone(),
                    form_span: span.clone(),
                    calls,
                });
            }
        }
        // Descend into `(do ...)` to reach nested top-level callables.
        "do" => {
            for child in &items[1..] {
                collect_functions(child, out);
            }
        }
        _ => {}
    }
}

fn function_from_body(
    name: &str,
    name_span: &SourceSpan,
    form_span: &SourceSpan,
    body: &[ParsedForm],
) -> FunctionInfo {
    let mut calls = Vec::new();
    for form in body {
        collect_calls(form, &mut calls);
    }
    FunctionInfo {
        name: name.to_string(),
        name_span: name_span.clone(),
        form_span: form_span.clone(),
        calls,
    }
}

/// Every head-position symbol reachable in the form is a call site; recurse into
/// arguments so nested calls are captured too. Control forms (`if`, `let`, …)
/// are recorded too but filtered out later by whether the callee resolves to a
/// known definition.
fn collect_calls(form: &ParsedForm, out: &mut Vec<CallSite>) {
    if let ParsedForm::List { items, .. } = form {
        if let Some(ParsedForm::Symbol { text, span }) = items.first() {
            out.push(CallSite {
                callee: text.clone(),
                span: span.clone(),
            });
        }
        for item in items {
            collect_calls(item, out);
        }
    }
}
