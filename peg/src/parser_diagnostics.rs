use crate::diagnostics_utils::{compute_line_offsets, line_col_u32};
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
    let (line, col) = line_col_u32(&offsets, clamped);
    ParseError::new(message, clamped, text.len())
        .with_location(line, col)
        .with_code(code)
}

pub(crate) fn parse_error_with_precomputed_location(
    message: impl Into<String>,
    pos: usize,
    text: &str,
    offsets: &[usize],
    code: impl Into<String>,
) -> ParseError {
    let clamped = pos.min(text.len());
    let (line, col) = line_col_u32(offsets, clamped);
    ParseError::new(message, clamped, text.len())
        .with_location(line, col)
        .with_code(code)
}
