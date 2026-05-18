use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a rule entry at the given position and depth.
    pub fn record_visit(&mut self, rule: &str, _pos: usize, depth: usize, memo_hit: bool) {
        let stat = self.rule_stats.entry(rule.to_string()).or_default();
        stat.visit_count += 1;
        if memo_hit {
            stat.memo_hits += 1;
        }
        if depth > stat.max_depth {
            stat.max_depth = depth;
        }
        // `positions_tried` is tracked per unique position via a separate structure;
        // here we just count every non-memo visit as a new position attempt.
        if !memo_hit {
            stat.positions_tried += 1;
            self.total_positions_tried += 1;
        }
        self.total_visits += 1;
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
    pub offset: usize,
    pub line: usize,
    pub column: usize,
}

/// A start–end pair of `SourcePoint`s.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceRange {
    pub start: SourcePoint,
    pub end: SourcePoint,
}

/// File identity metadata (path, URI, numeric id).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceLocator {
    pub source_id: Option<u64>,
    pub path: Option<String>,
    pub uri: Option<String>,
}

/// A rich source span annotating an error with file info and line/col coordinates.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub file_id: Option<u64>,
    pub start: usize,
    pub end: usize,
    pub path: Option<String>,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

impl SourceSpan {
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
    pub message: String,
    pub code: Option<String>,
    pub span: Option<SourceSpan>,
    /// Human-readable file:line:col string.
    pub location: Option<String>,
    pub source: Option<SourceLocator>,
    pub range: Option<SourceRange>,
    pub notes: Vec<String>,
    pub context: Vec<String>,
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
    peg_error_to_diagnostic_with_source(
        parse_error,
        filename,
        file_id,
        offset,
        message,
        code,
        context,
        None,
    )
}

/// Like `peg_error_to_diagnostic` but accepts the original source text to compute accurate line/col.
pub fn peg_error_to_diagnostic_with_source(
    parse_error: &crate::error::ParseError,
    filename: Option<&str>,
    file_id: Option<u64>,
    offset: i64,
    message: Option<&str>,
    code: Option<&str>,
    context: Option<Vec<String>>,
    source_text: Option<&str>,
) -> Diagnostic {
    let pos = (parse_error.span.start as i64 + offset).max(0) as usize;
    let found = parse_error.found.as_deref();
    let end = if found == Some("EOF") || found.is_none() {
        pos
    } else {
        pos + 1
    };

    let (start_line, start_col): (usize, usize) = if let Some(text) = source_text {
        let offsets = crate::diagnostics_utils::compute_line_offsets(text);
        let (l, c) = crate::diagnostics_utils::line_col(&offsets, pos);
        (l as usize, c as usize)
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
        file_id,
        start: pos,
        end,
        path: filename.map(str::to_string),
        start_line,
        start_col,
        end_line,
        end_col,
    };

    let notes = build_diagnostic_notes(parse_error);
    let location = filename.map(|f| format!("{f}:{start_line}:{start_col}"));

    let diagnostic_code = code
        .map(str::to_string)
        .or_else(|| parse_error.code.as_deref().map(str::to_string))
        .unwrap_or_else(|| "parse.unexpected".to_string());

    Diagnostic {
        severity: "error".to_string(),
        message: message.unwrap_or("parse error").to_string(),
        code: Some(diagnostic_code),
        source: Some(span.locator()),
        range: Some(span.range()),
        span: Some(span),
        location,
        notes,
        context: context.unwrap_or_default(),
        related: Vec::new(),
    }
}

fn build_diagnostic_notes(err: &crate::error::ParseError) -> Vec<String> {
    let mut notes = Vec::new();
    if !err.expected.is_empty() {
        notes.push(format!("expected: {}", err.expected.join(", ")));
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
        let diag = peg_error_to_diagnostic_with_source(
            &err,
            Some("src.peg"),
            None,
            0,
            None,
            None,
            None,
            Some(source),
        );
        let span = diag.span.unwrap();
        assert_eq!(span.start_line, 3);
        assert_eq!(span.start_col, 1);
    }
}
