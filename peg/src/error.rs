//! [`ParseError`] — the failure type returned by every parsing entry point, with
//! a byte span, an optional machine-readable code, the expected/found context,
//! and the active rule stack for diagnostics.

use serde::{Deserialize, Serialize};

#[allow(clippy::borrowed_box)]
fn boxed_strings_is_empty(values: &Box<[String]>) -> bool {
    values.is_empty()
}

/// A half-open byte range `[start, end)` into the source text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParseSpan {
    /// Inclusive start byte offset.
    pub start: usize,
    /// Exclusive end byte offset.
    pub end: usize,
}

impl ParseSpan {
    /// Build a span from explicit `[start, end)` byte bounds.
    pub fn from_bounds(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

/// A parse failure: a message plus the location and context needed to report it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParseError {
    /// Stable machine-readable diagnostic code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Box<str>>,
    /// Human-readable failure message.
    pub message: Box<str>,
    /// Byte span the failure is attributed to.
    pub span: ParseSpan,
    /// Tokens or labels the parser expected at the failure position.
    #[serde(default, skip_serializing_if = "boxed_strings_is_empty")]
    pub expected: Box<[String]>,
    /// The text that was actually found at the failure position (if available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub found: Option<Box<str>>,
    /// 1-based line number when location is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// 1-based column number when location is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub col: Option<u32>,
    /// Active rule stack at the failure site (innermost last).
    #[serde(default, skip_serializing_if = "boxed_strings_is_empty")]
    pub rule_stack: Box<[String]>,
}

impl ParseError {
    /// Build an error with `message` over the byte span `[start, end)`.
    pub fn new(message: impl Into<String>, start: usize, end: usize) -> Self {
        Self {
            message: message.into().into_boxed_str(),
            code: None,
            span: ParseSpan::from_bounds(start, end),
            expected: Box::default(),
            found: None,
            line: None,
            col: None,
            rule_stack: Box::default(),
        }
    }

    /// Build a richer error with expected/found context.
    pub fn with_context(
        message: impl Into<String>,
        start: usize,
        end: usize,
        expected: Vec<String>,
        found: Option<String>,
    ) -> Self {
        Self {
            message: message.into().into_boxed_str(),
            code: None,
            span: ParseSpan::from_bounds(start, end),
            expected: expected.into_boxed_slice(),
            found: found.map(String::into_boxed_str),
            line: None,
            col: None,
            rule_stack: Box::default(),
        }
    }

    /// Attach a 1-based line/column location.
    pub fn with_location(mut self, line: u32, col: u32) -> Self {
        self.line = Some(line);
        self.col = Some(col);
        self
    }

    /// Attach a stable machine-readable diagnostic code.
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into().into_boxed_str());
        self
    }

    /// Attach the active rule stack (outermost first) at the failure site.
    pub fn with_rule_stack(mut self, stack: Vec<String>) -> Self {
        self.rule_stack = stack.into_boxed_slice();
        self
    }

    /// Return a clone with the span rewritten to absolute document coordinates.
    pub fn at_absolute_pos(mut self, abs_start: usize, abs_end: usize) -> Self {
        self.span = ParseSpan::from_bounds(abs_start, abs_end);
        self
    }

    /// The `expected` labels with duplicates removed and a stable order applied.
    ///
    /// A parser that tries the same alternative from several rule paths can list
    /// one expectation many times; this is what a diagnostic should display. The
    /// order is shortest-first then lexicographic, so the tersest expectation
    /// (usually the most actionable, e.g. `';'` over `statement`) leads.
    pub fn normalized_expected(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<String> = self
            .expected
            .iter()
            .filter(|label| seen.insert(label.as_str()))
            .cloned()
            .collect();
        out.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
        out
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ParseError {}

#[cfg(test)]
mod tests {
    use super::ParseError;

    #[test]
    fn parse_error_stays_small_enough_for_result_err() {
        assert!(std::mem::size_of::<ParseError>() <= 128);
    }

    #[test]
    fn normalized_expected_dedups_and_orders_shortest_first() {
        let err = ParseError::with_context(
            "x",
            0,
            0,
            vec![
                "statement".into(),
                "';'".into(),
                "statement".into(),
                "'}'".into(),
            ],
            None,
        );
        assert_eq!(err.normalized_expected(), vec!["';'", "'}'", "statement"]);
    }
}
