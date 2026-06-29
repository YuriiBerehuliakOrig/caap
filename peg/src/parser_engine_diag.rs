use std::collections::{HashMap, HashSet};

// ── Diagnostics tracking ───────────────────────────────────────────────────

pub(crate) struct DiagnosticsState {
    /// Furthest position reached during parsing (best error position).
    pub(crate) furthest: usize,
    /// Expected token labels at each position >= furthest.
    pub(crate) expected: HashMap<usize, HashSet<String>>,
}

impl DiagnosticsState {
    pub(crate) fn new() -> Self {
        Self {
            furthest: 0,
            expected: HashMap::new(),
        }
    }

    pub(crate) fn record_expected(&mut self, pos: usize, label: impl Into<String>) {
        if pos >= self.furthest {
            if pos > self.furthest {
                self.furthest = pos;
            }
            self.expected.entry(pos).or_default().insert(label.into());
        }
    }

    /// Lazy variant: only evaluates `label` when `pos >= furthest`, avoiding
    /// `format!` allocations for positions that will never be reported.
    #[inline]
    pub(crate) fn record_expected_lazy(&mut self, pos: usize, label: impl FnOnce() -> String) {
        if pos >= self.furthest {
            if pos > self.furthest {
                self.furthest = pos;
            }
            self.expected.entry(pos).or_default().insert(label());
        }
    }

    pub(crate) fn expected_at_furthest(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .expected
            .get(&self.furthest)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        v.sort_unstable();
        v
    }
}

pub(crate) fn format_tok_label(kind: &Option<String>, text: &Option<String>) -> String {
    match (kind, text) {
        (Some(k), Some(t)) => format!("tok({k},{t:?})"),
        (Some(k), None) => format!("tok({k})"),
        (None, Some(t)) => format!("tok({t:?})"),
        (None, None) => "tok(<any>)".to_string(),
    }
}

// ── Layout state ───────────────────────────────────────────────────────────

pub(crate) struct LayoutState {
    pub(crate) indent_stack: Vec<usize>,
    /// Number of open brackets (paren/bracket/brace); when > 0, newlines are
    /// treated as regular whitespace.
    pub(crate) bracket_depth: usize,
    pub(crate) at_line_start: bool,
    pub(crate) indentation_enabled: bool,
}

impl LayoutState {
    pub(crate) fn new(indentation_enabled: bool) -> Self {
        Self {
            indent_stack: vec![0],
            bracket_depth: 0,
            at_line_start: true,
            indentation_enabled,
        }
    }
}

// ── Layout helpers ─────────────────────────────────────────────────────────

/// Match `\r\n`, `\r`, or `\n` at `pos`.  Returns the position after the match.
pub(crate) fn match_newline_at(text: &str, pos: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if pos >= bytes.len() {
        return None;
    }
    match bytes[pos] {
        b'\r' => {
            if pos + 1 < bytes.len() && bytes[pos + 1] == b'\n' {
                Some(pos + 2)
            } else {
                Some(pos + 1)
            }
        }
        b'\n' => Some(pos + 1),
        _ => None,
    }
}

/// Return `(indent_width, end_pos)` where `indent_width` is the column of the
/// first non-whitespace character (tabs count as 4 spaces).
pub(crate) fn measure_indent(text: &str, pos: usize) -> (usize, usize) {
    let mut width = 0usize;
    let mut cur = pos;
    let bytes = text.as_bytes();
    while cur < bytes.len() {
        match bytes[cur] {
            b' ' => {
                width += 1;
                cur += 1;
            }
            b'\t' => {
                width += 4;
                cur += 1;
            }
            _ => break,
        }
    }
    (width, cur)
}
