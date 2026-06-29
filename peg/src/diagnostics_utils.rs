//! Diagnostics helpers for line/column mapping.

/// Byte offsets of each line start (first entry is always `0`).
pub fn compute_line_offsets(text: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (i, ch) in text.char_indices() {
        if ch == '\n' {
            offsets.push(i + ch.len_utf8());
        }
    }
    offsets
}

/// Return 1-based `(line, column)` for a byte position in `text`.
pub fn line_col(offsets: &[usize], pos: usize) -> (usize, usize) {
    if offsets.is_empty() {
        return (1, pos.saturating_add(1));
    }
    let line_idx = match offsets.binary_search(&pos) {
        Ok(idx) => idx,
        Err(idx) => idx.saturating_sub(1),
    };
    let line = line_idx.saturating_add(1);
    let col = pos.saturating_sub(offsets[line_idx]).saturating_add(1);
    (line, col)
}

/// Return 1-based `(line, column)` for `ParseError`, whose public location
/// fields intentionally remain compact `u32`s.
pub fn line_col_u32(offsets: &[usize], pos: usize) -> (u32, u32) {
    let (line, col) = line_col(offsets, pos);
    (
        u32::try_from(line).unwrap_or(u32::MAX),
        u32::try_from(col).unwrap_or(u32::MAX),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_first_line() {
        let text = "abc\ndef";
        let offsets = compute_line_offsets(text);
        assert_eq!(line_col(&offsets, 0), (1, 1));
        assert_eq!(line_col(&offsets, 2), (1, 3));
        assert_eq!(line_col(&offsets, 4), (2, 1));
    }

    #[test]
    fn line_col_keeps_usize_width_and_u32_projection_saturates() {
        assert_eq!(line_col(&[], usize::MAX), (1, usize::MAX));
        assert_eq!(line_col_u32(&[], usize::MAX), (1, u32::MAX));
    }
}
