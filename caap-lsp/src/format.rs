//! Document formatting via `caap_core::frontend::canonicalize_source`.
//!
//! Canonicalization is lossy on surface comments, so formatting is refused when
//! the source contains any — we never silently drop comments. Grammar-extended
//! files (which the s-expr canonicalizer cannot parse) also yield no edit.

use lsp_types::{Position, Range, TextEdit};

/// Produce a single whole-document edit that canonicalizes the source, or
/// `None` when formatting is unsafe or a no-op (unparsable, contains comments,
/// or already canonical).
pub fn format_document(text: &str) -> Option<Vec<TextEdit>> {
    if source_contains_comments(text) {
        return None;
    }
    let canonical = caap_core::frontend::canonicalize_source(text).ok()?;
    if canonical == text {
        return None;
    }
    let end_line = text.lines().count() as u32;
    Some(vec![TextEdit {
        range: Range {
            start: Position::new(0, 0),
            // A position past the last line covers the whole document; the
            // client clamps it to the real end.
            end: Position::new(end_line + 1, 0),
        },
        new_text: canonical,
    }])
}

/// Whether the source contains surface comments (`;`, `#| |#`, `/* */`).
pub fn source_contains_comments(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        if b == b';'
            || (b == b'#' && bytes.get(i + 1) == Some(&b'|'))
            || (b == b'/' && bytes.get(i + 1) == Some(&b'*'))
        {
            return true;
        }
        i += 1;
    }
    false
}
