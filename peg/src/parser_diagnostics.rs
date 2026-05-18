use crate::diagnostics_utils::{compute_line_offsets, line_col};
use crate::error::ParseError;

/// Build a top-level parser error with a stable diagnostic code and line/column info.
pub(crate) fn parse_error_with_location(
    message: impl Into<String>,
    pos: usize,
    text: &str,
    code: impl Into<String>,
) -> ParseError {
    let clamped = pos.min(text.len());
    let offsets = compute_line_offsets(text);
    let (line, col) = line_col(&offsets, clamped);
    ParseError::new(message, clamped, text.len())
        .with_location(line, col)
        .with_code(code)
}
