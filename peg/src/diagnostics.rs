//! Editor-facing diagnostics: pretty rustc-style error rendering, LSP/UTF-16
//! offset projection, per-rule visit statistics, and conversion of a
//! [`ParseError`] into a structured [`Diagnostic`].

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::diagnostics_utils::{compute_line_offsets, line_col};
use crate::error::ParseError;

// ── UTF-16 / LSP offset projection ──────────────────────────────────────────

/// Clamp `byte_offset` down to the nearest UTF-8 character boundary `<= len`.
fn clamp_boundary(text: &str, byte_offset: usize) -> usize {
    let mut end = byte_offset.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Number of UTF-16 code units in `text[..byte_offset]`.
///
/// Byte offsets are the parser's native unit; editors speaking LSP use UTF-16
/// code units, so this projects a byte offset into that space.
pub fn byte_to_utf16(text: &str, byte_offset: usize) -> usize {
    let end = clamp_boundary(text, byte_offset);
    text[..end].chars().map(char::len_utf16).sum()
}

/// Project a byte offset to an LSP `Position`: `(line, character)`, both
/// **0-based**, with `character` measured in UTF-16 code units from line start.
pub fn lsp_position(text: &str, byte_offset: usize) -> (usize, usize) {
    let end = clamp_boundary(text, byte_offset);
    let prefix = &text[..end];
    let line = prefix.bytes().filter(|&b| b == b'\n').count();
    let line_start = prefix.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let character = text[line_start..end].chars().map(char::len_utf16).sum();
    (line, character)
}

// ── Pretty error rendering ──────────────────────────────────────────────────

/// Render a [`ParseError`] against its `source` as a multi-line, rustc-style
/// diagnostic. The caret spans the whole offending range (clamped to the line),
/// and its label prefers the deduplicated [`expected`](ParseError::normalized_expected)
/// set over the raw message:
///
/// ```text
/// error: expected ';'
///   --> 3:14
///    |
///  3 |   let x = 1
///    |              ^ expected: ';'
/// ```
pub fn render_parse_error(source: &str, error: &ParseError) -> String {
    let offsets = compute_line_offsets(source);
    let (line, col) = line_col(&offsets, error.span.start);
    let line_idx = line.saturating_sub(1);
    let line_text = source.lines().nth(line_idx).unwrap_or("");
    let gutter = line.to_string();
    let pad = " ".repeat(gutter.len());
    let caret_pad = " ".repeat(col.saturating_sub(1));
    // Underline the whole span (≥1 caret), clamped to what remains on this line so
    // a multi-line or run-to-EOF span can't paint past the rendered text.
    let remaining = line_text
        .chars()
        .count()
        .saturating_sub(col.saturating_sub(1));
    let width = error
        .span
        .end
        .saturating_sub(error.span.start)
        .clamp(1, remaining.max(1));
    let caret = "^".repeat(width);
    let label = caret_label(error);
    let msg = &error.message;
    format!(
        "error: {msg}\n{pad}--> {line}:{col}\n{pad} |\n{gutter} | {line_text}\n{pad} | {caret_pad}{caret} {label}\n"
    )
}

/// The caret-line label: the deduplicated expected set when present (and not
/// already echoed by the message), else the message itself.
fn caret_label(error: &ParseError) -> String {
    let expected = error.normalized_expected();
    if expected.is_empty() {
        return error.message.to_string();
    }
    let joined = expected.join(", ");
    if error.message.contains(&joined) {
        error.message.to_string()
    } else {
        format!("expected: {joined}")
    }
}

/// Render a batch of [`ParseError`]s (e.g. the error list from `recover_parse`)
/// against one `source`, ordered by position, each as [`render_parse_error`]
/// would, with a trailing `N error(s)` summary. Returns `"no errors\n"` for an
/// empty slice.
pub fn render_parse_errors(source: &str, errors: &[ParseError]) -> String {
    if errors.is_empty() {
        return "no errors\n".to_string();
    }
    let mut ordered: Vec<&ParseError> = errors.iter().collect();
    ordered.sort_by_key(|e| (e.span.start, e.span.end));
    let mut out = String::new();
    for error in &ordered {
        out.push_str(&render_parse_error(source, error));
        out.push('\n');
    }
    let n = errors.len();
    let noun = if n == 1 { "error" } else { "errors" };
    out.push_str(&format!("{n} {noun}\n"));
    out
}

/// Per-rule visit statistics collected during a single parse run.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuleVisitStat {
    /// Total number of times the rule was entered (including memoised hits).
    pub visit_count: usize,
    /// Number of positions at which the rule was tried at least once.
    pub positions_tried: usize,
    /// How many times the rule result was retrieved from the memo table.
    pub memo_hits: usize,
    /// Maximum recursion depth reached for this rule.
    pub max_depth: usize,
}

impl RuleVisitStat {
    fn record_visit(&mut self, depth: usize, memo_hit: bool) {
        self.visit_count = self.visit_count.saturating_add(1);
        if memo_hit {
            self.memo_hits = self.memo_hits.saturating_add(1);
        }
        self.max_depth = self.max_depth.max(depth);
        if !memo_hit {
            self.positions_tried = self.positions_tried.saturating_add(1);
        }
    }
}

/// A snapshot of parser diagnostics collected during one `parse` / `parse_prefix` call.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParserDiagnosticsSnapshot {
    /// Per-rule statistics, keyed by rule name.
    pub rule_stats: HashMap<String, RuleVisitStat>,
    /// Total number of rule entry events (sum of all `visit_count`s).
    pub total_visits: usize,
    /// Number of distinct (rule, position) pairs evaluated.
    pub total_positions_tried: usize,
}

impl ParserDiagnosticsSnapshot {
    /// An empty diagnostics snapshot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a rule entry at the given position and depth.
    pub fn record_visit(&mut self, rule: &str, _pos: usize, depth: usize, memo_hit: bool) {
        let stat = self.rule_stats.entry(rule.to_string()).or_default();
        stat.record_visit(depth, memo_hit);
        // `positions_tried` is tracked per unique position via a separate structure;
        // here we just count every non-memo visit as a new position attempt.
        if !memo_hit {
            self.total_positions_tried = self.total_positions_tried.saturating_add(1);
        }
        self.total_visits = self.total_visits.saturating_add(1);
    }

    /// Return the rule with the highest `visit_count`, if any.
    pub fn hottest_rule(&self) -> Option<(&str, &RuleVisitStat)> {
        self.rule_stats
            .iter()
            .max_by_key(|(_, s)| s.visit_count)
            .map(|(name, stat)| (name.as_str(), stat))
    }

    /// Return rules sorted by descending visit count.
    pub fn top_rules(&self, limit: usize) -> Vec<(&str, &RuleVisitStat)> {
        let mut entries: Vec<_> = self
            .rule_stats
            .iter()
            .map(|(n, s)| (n.as_str(), s))
            .collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.1.visit_count));
        entries.truncate(limit);
        entries
    }

    /// Format a short human-readable summary.
    pub fn summary(&self) -> String {
        let hottest = self
            .hottest_rule()
            .map(|(n, s)| format!("'{n}' ({} visits)", s.visit_count))
            .unwrap_or_else(|| "—".to_string());
        format!(
            "total_visits={} positions_tried={} hottest={}",
            self.total_visits, self.total_positions_tried, hottest
        )
    }
}

/// Heuristic interpretation of parser diagnostics — identifies the likely
/// performance bottleneck based on visit statistics.
///
/// Mirrors `peg/engine/parser_diagnostics.py`'s `interpret_parser_diagnostics()`.
pub fn interpret_parser_diagnostics(snapshot: &ParserDiagnosticsSnapshot) -> String {
    if snapshot.total_visits == 0 {
        return "No parser positions were recorded.".to_string();
    }
    let unique = snapshot.total_positions_tried.max(1);
    let revisit_approx = snapshot.total_visits.saturating_sub(unique);
    let revisit_ratio = revisit_approx as f64 / unique as f64;
    let calls_per_unique = snapshot.total_visits as f64 / unique as f64;
    let max_depth = snapshot
        .rule_stats
        .values()
        .map(|s| s.max_depth)
        .max()
        .unwrap_or(0);
    let max_visits = snapshot
        .rule_stats
        .values()
        .map(|s| s.visit_count)
        .max()
        .unwrap_or(0);
    if max_visits >= 4 || revisit_ratio >= 0.20 {
        return "Likely repeated parsing / backtracking / re-dispatch: many positions were \
                visited multiple times."
            .to_string();
    }
    if max_depth >= 64.max(unique / 2) {
        return "Likely deep structural recursion: recursion depth is high while position \
                revisits stay modest."
            .to_string();
    }
    if calls_per_unique >= 12.0 && max_depth < 64.max(unique / 3) {
        return "Likely too many parse/helper layers per logical node: total call volume is high \
                without extreme depth or revisit pressure."
            .to_string();
    }
    "Mixed profile: no single category dominates cleanly.".to_string()
}

// ── IDE / Editor diagnostics ──────────────────────────────────────────────

/// A byte-offset + line/column position within source text.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourcePoint {
    /// Byte offset into the source.
    pub offset: usize,
    /// 1-based line.
    pub line: usize,
    /// 1-based column.
    pub column: usize,
}

/// A start–end pair of `SourcePoint`s.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceRange {
    /// Start point.
    pub start: SourcePoint,
    /// End point.
    pub end: SourcePoint,
}

/// File identity metadata (path, URI, numeric id).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceLocator {
    /// Numeric source id, if any.
    pub source_id: Option<u64>,
    /// File path, if any.
    pub path: Option<String>,
    /// File URI, if any.
    pub uri: Option<String>,
}

/// A rich source span annotating an error with file info and line/col coordinates.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceSpan {
    /// Numeric file id, if any.
    pub file_id: Option<u64>,
    /// Start byte offset.
    pub start: usize,
    /// End byte offset.
    pub end: usize,
    /// File path, if any.
    pub path: Option<String>,
    /// 1-based start line.
    pub start_line: usize,
    /// 1-based start column.
    pub start_col: usize,
    /// 1-based end line.
    pub end_line: usize,
    /// 1-based end column.
    pub end_col: usize,
}

impl SourceSpan {
    /// Project the span's file identity into a [`SourceLocator`].
    pub fn locator(&self) -> SourceLocator {
        let uri = self.path.as_ref().map(|p| {
            if p.contains("://") {
                p.clone()
            } else {
                format!("file://{p}")
            }
        });
        SourceLocator {
            source_id: self.file_id,
            path: self.path.clone(),
            uri,
        }
    }

    /// The span's start/end as a [`SourceRange`].
    pub fn range(&self) -> SourceRange {
        SourceRange {
            start: SourcePoint {
                offset: self.start,
                line: self.start_line,
                column: self.start_col,
            },
            end: SourcePoint {
                offset: self.end,
                line: self.end_line,
                column: self.end_col,
            },
        }
    }
}

/// A structured error or warning suitable for IDE / editor display.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// `"error"`, `"warning"`, `"info"`, or `"hint"`.
    pub severity: String,
    /// The diagnostic message.
    pub message: String,
    /// Optional machine-readable code.
    pub code: Option<String>,
    /// Rich source span, if available.
    pub span: Option<SourceSpan>,
    /// Human-readable file:line:col string.
    pub location: Option<String>,
    /// File identity, if available.
    pub source: Option<SourceLocator>,
    /// Line/column range, if available.
    pub range: Option<SourceRange>,
    /// Free-form explanatory notes.
    pub notes: Vec<String>,
    /// Contextual breadcrumbs (e.g. rule stack).
    pub context: Vec<String>,
    /// Related sub-diagnostics.
    pub related: Vec<Diagnostic>,
}

/// Convert a `ParseError` into a `Diagnostic` with rich source-span metadata.
pub fn peg_error_to_diagnostic(
    parse_error: &crate::error::ParseError,
    filename: Option<&str>,
    file_id: Option<u64>,
    offset: i64,
    message: Option<&str>,
    code: Option<&str>,
    context: Option<Vec<String>>,
) -> Diagnostic {
    peg_error_to_diagnostic_with_source(PegDiagnosticSource {
        parse_error,
        filename,
        file_id,
        offset,
        message,
        code,
        context,
        source_text: None,
    })
}

/// Inputs to [`peg_error_to_diagnostic_with_source`].
pub struct PegDiagnosticSource<'a> {
    /// The parse error to convert.
    pub parse_error: &'a crate::error::ParseError,
    /// Source file name, if any.
    pub filename: Option<&'a str>,
    /// Numeric file id, if any.
    pub file_id: Option<u64>,
    /// Byte offset applied to the error position.
    pub offset: i64,
    /// Message override.
    pub message: Option<&'a str>,
    /// Code override.
    pub code: Option<&'a str>,
    /// Optional context breadcrumbs.
    pub context: Option<Vec<String>>,
    /// Source text, enabling accurate line/col computation.
    pub source_text: Option<&'a str>,
}

/// Like `peg_error_to_diagnostic` but accepts the original source text to compute accurate line/col.
pub fn peg_error_to_diagnostic_with_source(request: PegDiagnosticSource<'_>) -> Diagnostic {
    let pos = apply_diagnostic_offset(request.parse_error.span.start, request.offset);
    let found = request.parse_error.found.as_deref();
    let end = if found == Some("EOF") || found.is_none() {
        pos
    } else {
        pos.saturating_add(1)
    };

    let (start_line, start_col): (usize, usize) = if let Some(text) = request.source_text {
        let offsets = crate::diagnostics_utils::compute_line_offsets(text);
        crate::diagnostics_utils::line_col(&offsets, pos)
    } else {
        (1, 1)
    };
    let end_line = start_line;
    let end_col = if found == Some("EOF") {
        start_col
    } else {
        start_col + 1
    };

    let span = SourceSpan {
        file_id: request.file_id,
        start: pos,
        end,
        path: request.filename.map(str::to_string),
        start_line,
        start_col,
        end_line,
        end_col,
    };

    let notes = build_diagnostic_notes(request.parse_error);
    let location = request
        .filename
        .map(|f| format!("{f}:{start_line}:{start_col}"));

    let diagnostic_code = request
        .code
        .map(str::to_string)
        .or_else(|| request.parse_error.code.as_deref().map(str::to_string))
        .unwrap_or_else(|| "parse.unexpected".to_string());

    Diagnostic {
        severity: "error".to_string(),
        message: request.message.unwrap_or("parse error").to_string(),
        code: Some(diagnostic_code),
        source: Some(span.locator()),
        range: Some(span.range()),
        span: Some(span),
        location,
        notes,
        context: request.context.unwrap_or_default(),
        related: Vec::new(),
    }
}

fn apply_diagnostic_offset(pos: usize, offset: i64) -> usize {
    if offset >= 0 {
        pos.saturating_add(usize::try_from(offset).unwrap_or(usize::MAX))
    } else {
        pos.saturating_sub(usize::try_from(offset.unsigned_abs()).unwrap_or(usize::MAX))
    }
}

fn build_diagnostic_notes(err: &crate::error::ParseError) -> Vec<String> {
    let mut notes = Vec::new();
    let expected = err.normalized_expected();
    if !expected.is_empty() {
        notes.push(format!("expected: {}", expected.join(", ")));
    } else {
        notes.push("expected: EOF".to_string());
    }
    if let Some(found) = &err.found {
        notes.push(format!("got: {found}"));
    } else {
        notes.push("got: unexpected token".to_string());
    }
    let skip_msgs = ["Failed to parse", "Extra input"];
    if !skip_msgs.iter().any(|m| *m == err.message.as_ref()) {
        notes.push(format!("parser: {}", err.message));
    }
    notes
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_visit_increments_totals() {
        let mut snap = ParserDiagnosticsSnapshot::new();
        snap.record_visit("root", 0, 0, false);
        snap.record_visit("root", 0, 0, true); // memo hit
        snap.record_visit("item", 1, 1, false);
        assert_eq!(snap.total_visits, 3);
        assert_eq!(snap.total_positions_tried, 2);
    }

    #[test]
    fn record_visit_tracks_per_rule_stats() {
        let mut snap = ParserDiagnosticsSnapshot::new();
        snap.record_visit("root", 0, 0, false);
        snap.record_visit("root", 2, 1, false);
        snap.record_visit("root", 2, 0, true);
        let stat = snap.rule_stats.get("root").unwrap();
        assert_eq!(stat.visit_count, 3);
        assert_eq!(stat.memo_hits, 1);
        assert_eq!(stat.positions_tried, 2);
        assert_eq!(stat.max_depth, 1);
    }

    #[test]
    fn record_visit_saturates_diagnostic_counters() {
        let mut snap = ParserDiagnosticsSnapshot::new();
        snap.total_visits = usize::MAX;
        snap.total_positions_tried = usize::MAX;
        snap.rule_stats.insert(
            "root".to_string(),
            RuleVisitStat {
                visit_count: usize::MAX,
                positions_tried: usize::MAX,
                memo_hits: usize::MAX,
                max_depth: 1,
            },
        );

        snap.record_visit("root", 0, 4, false);
        snap.record_visit("root", 0, 8, true);

        let stat = snap.rule_stats.get("root").unwrap();
        assert_eq!(stat.visit_count, usize::MAX);
        assert_eq!(stat.positions_tried, usize::MAX);
        assert_eq!(stat.memo_hits, usize::MAX);
        assert_eq!(stat.max_depth, 8);
        assert_eq!(snap.total_visits, usize::MAX);
        assert_eq!(snap.total_positions_tried, usize::MAX);
    }

    #[test]
    fn hottest_rule_returns_most_visited() {
        let mut snap = ParserDiagnosticsSnapshot::new();
        snap.record_visit("a", 0, 0, false);
        snap.record_visit("b", 0, 0, false);
        snap.record_visit("b", 1, 0, false);
        let (name, _) = snap.hottest_rule().unwrap();
        assert_eq!(name, "b");
    }

    #[test]
    fn top_rules_is_sorted_and_capped() {
        let mut snap = ParserDiagnosticsSnapshot::new();
        for _ in 0..5 {
            snap.record_visit("busy", 0, 0, false);
        }
        snap.record_visit("once", 0, 0, false);
        let top = snap.top_rules(1);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].0, "busy");
    }

    #[test]
    fn summary_contains_key_metrics() {
        let mut snap = ParserDiagnosticsSnapshot::new();
        snap.record_visit("root", 0, 0, false);
        let s = snap.summary();
        assert!(s.contains("total_visits=1"));
        assert!(s.contains("root"));
    }

    #[test]
    fn empty_snapshot_summary() {
        let snap = ParserDiagnosticsSnapshot::new();
        let s = snap.summary();
        assert!(s.contains("total_visits=0"));
    }

    #[test]
    fn render_parse_error_underlines_full_span_and_lists_expected() {
        use crate::error::ParseError;
        let source = "let x = 1";
        // Span [4,5) over "x", expected a dedup set.
        let err = ParseError::with_context(
            "unexpected identifier",
            4,
            5,
            vec!["';'".into(), "'='".into(), "';'".into()],
            Some("x".into()),
        );
        let out = render_parse_error(source, &err);
        // Caret underlines the one-char span and the label is the ordered set.
        assert!(out.contains("^ expected: ';', '='"), "got:\n{out}");
        assert!(out.contains("--> 1:5"));
    }

    #[test]
    fn render_parse_error_clamps_caret_to_line() {
        use crate::error::ParseError;
        let source = "ab";
        // A run-to-EOF style span far past the line must not paint past the text.
        let err = ParseError::new("unterminated", 0, 999);
        let out = render_parse_error(source, &err);
        assert!(out.contains("| ^^ unterminated"), "got:\n{out}");
    }

    #[test]
    fn render_parse_errors_orders_and_summarises() {
        use crate::error::ParseError;
        let source = "a;b;c";
        let errors = vec![
            ParseError::new("second", 2, 3),
            ParseError::new("first", 0, 1),
        ];
        let out = render_parse_errors(source, &errors);
        let first_at = out.find("first").unwrap();
        let second_at = out.find("second").unwrap();
        assert!(first_at < second_at, "errors must be position-ordered");
        assert!(out.trim_end().ends_with("2 errors"));
    }

    #[test]
    fn render_parse_errors_empty_is_no_errors() {
        assert_eq!(render_parse_errors("x", &[]), "no errors\n");
    }

    #[test]
    fn peg_error_to_diagnostic_basic() {
        use crate::error::ParseError;
        let err = ParseError::new("expected: 'hello'", 3, 10);
        let diag = peg_error_to_diagnostic(&err, Some("test.peg"), None, 0, None, None, None);
        assert_eq!(diag.severity, "error");
        assert_eq!(diag.message, "parse error");
        assert_eq!(diag.code.as_deref(), Some("parse.unexpected"));
        assert_eq!(diag.location.as_deref(), Some("test.peg:1:1"));
        assert!(diag.span.is_some());
        assert_eq!(diag.span.as_ref().unwrap().start, 3);
    }

    #[test]
    fn peg_error_to_diagnostic_with_source_computes_line_col() {
        use crate::error::ParseError;
        let source = "line1\nline2\nline3";
        // Error at offset 12 = start of "line3" ("line1\n"=6 + "line2\n"=6 = 12)
        let err = ParseError::new("expected something", 12, source.len());
        let diag = peg_error_to_diagnostic_with_source(PegDiagnosticSource {
            parse_error: &err,
            filename: Some("src.peg"),
            file_id: None,
            offset: 0,
            message: None,
            code: None,
            context: None,
            source_text: Some(source),
        });
        let span = diag.span.unwrap();
        assert_eq!(span.start_line, 3);
        assert_eq!(span.start_col, 1);
    }

    #[test]
    fn peg_error_to_diagnostic_offsets_saturate_without_wrapping() {
        use crate::error::ParseError;
        let err = ParseError::new("expected something", usize::MAX - 1, usize::MAX);
        let diag = peg_error_to_diagnostic(&err, None, None, i64::MAX, None, None, None);
        let span = diag.span.unwrap();
        assert_eq!(span.start, usize::MAX);
        assert_eq!(span.end, usize::MAX);

        let err = ParseError::new("expected something", 3, 3);
        let diag = peg_error_to_diagnostic(&err, None, None, i64::MIN, None, None, None);
        assert_eq!(diag.span.unwrap().start, 0);
    }
}
