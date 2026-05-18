use serde::{Deserialize, Serialize};

#[allow(clippy::borrowed_box)]
fn boxed_strings_is_empty(values: &Box<[String]>) -> bool {
    values.is_empty()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParseSpan {
    pub start: usize,
    pub end: usize,
}

impl ParseSpan {
    pub fn from_bounds(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParseError {
    /// Stable machine-readable diagnostic code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Box<str>>,
    pub message: Box<str>,
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

    pub fn with_location(mut self, line: u32, col: u32) -> Self {
        self.line = Some(line);
        self.col = Some(col);
        self
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into().into_boxed_str());
        self
    }

    pub fn with_rule_stack(mut self, stack: Vec<String>) -> Self {
        self.rule_stack = stack.into_boxed_slice();
        self
    }

    /// Return a clone with the span rewritten to absolute document coordinates.
    pub fn at_absolute_pos(mut self, abs_start: usize, abs_end: usize) -> Self {
        self.span = ParseSpan::from_bounds(abs_start, abs_end);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::ParseError;

    #[test]
    fn parse_error_stays_small_enough_for_result_err() {
        assert!(std::mem::size_of::<ParseError>() <= 128);
    }
}
