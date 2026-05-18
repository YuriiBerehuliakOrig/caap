//! Diagnostics helpers: line/column mapping and error-context formatting.

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
pub fn line_col(offsets: &[usize], pos: usize) -> (u32, u32) {
    if offsets.is_empty() {
        return (1, pos as u32 + 1);
    }
    let line_idx = match offsets.binary_search(&pos) {
        Ok(idx) => idx,
        Err(idx) => idx.saturating_sub(1),
    };
    let line = line_idx as u32 + 1;
    let col = (pos - offsets[line_idx]) as u32 + 1;
    (line, col)
}

/// Human-readable label for an expected-token string from the parser.
pub fn expected_label(value: &str) -> String {
    if let Some(rest) = value.strip_prefix("tok:SOFT_KEYWORD:") {
        return format!("soft keyword {rest}");
    }
    if value == "tok:SOFT_KEYWORD" {
        return "soft keyword".to_string();
    }
    if let Some(payload) = value.strip_prefix("tok:") {
        if let Some((kind, text)) = payload.split_once(':') {
            return format!("token {kind}={text}");
        }
        return format!("token {payload}");
    }
    if let Some(rest) = value.strip_prefix("soft_kw:") {
        return format!("soft keyword {rest}");
    }
    if let Some(rest) = value.strip_prefix("token:") {
        return format!("token pattern /{rest}/");
    }
    if value.starts_with("<param:") && value.ends_with('>') {
        return format!("param {}", &value[7..value.len() - 1]);
    }
    if value.starts_with("<rec:") && value.ends_with('>') {
        return format!("recursion guard {}", &value[5..value.len() - 1]);
    }
    if value.starts_with('/') && value.ends_with('/') {
        return format!("pattern {value}");
    }
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return format!("literal {value}");
    }
    value.to_string()
}

/// Sort key for expected labels (literals first, then tokens, etc.).
pub fn expected_sort_key(label: &str) -> (u8, &str) {
    if label.starts_with("literal ") {
        (0, label)
    } else if label.starts_with("token ") {
        (1, label)
    } else if label.starts_with("soft keyword") {
        (2, label)
    } else if label.starts_with("pattern ") {
        (3, label)
    } else {
        (4, label)
    }
}

/// Collapse a set of expected labels to a bounded summary tuple.
pub fn summarize_expected<'a>(
    expected: impl IntoIterator<Item = &'a str>,
    limit: usize,
) -> Vec<String> {
    let mut labels: Vec<String> = expected
        .into_iter()
        .map(expected_label)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    labels.sort_by(|a, b| expected_sort_key(a).cmp(&expected_sort_key(b)));
    if labels.len() <= limit {
        return labels;
    }
    let kept = limit.saturating_sub(1).max(1);
    let more = labels.len() - kept;
    labels.truncate(kept);
    labels.push(format!("...(+{more} more)"));
    labels
}

/// Describe the token at `pos` for error messages.
pub fn describe_found(text: &str, pos: usize) -> String {
    if pos >= text.len() {
        return "EOF".to_string();
    }
    let suffix = &text[pos..];
    let ch = suffix.chars().next().unwrap();
    if ch == '\n' {
        return "newline".to_string();
    }
    if ch.is_whitespace() {
        return format!("whitespace {ch:?}");
    }
    let snippet: String = suffix.chars().take(16).collect();
    if suffix.chars().count() > 16 {
        format!("{snippet:?}...")
    } else {
        format!("{snippet:?}")
    }
}

/// Build a caret snippet around `pos` (mirrors Python `format_context`).
pub fn format_context(text: &str, pos: usize, offsets: &[usize], window: usize) -> String {
    let (line, col) = line_col(offsets, pos);
    let line_start = offsets.get(line as usize - 1).copied().unwrap_or(0);
    let line_end = offsets
        .get(line as usize)
        .map(|&o| o.saturating_sub(1))
        .unwrap_or(text.len());
    let mut segment: String = text[line_start..line_end]
        .trim_end_matches('\n')
        .to_string();
    let caret_pos = if segment.len() > window {
        let prefix_len = col as usize - 1;
        let start = prefix_len.saturating_sub(window / 2);
        let end = (start + window).min(segment.len());
        segment = segment[start..end].to_string();
        col as usize - 1 - start
    } else {
        col as usize - 1
    };
    let caret_line = format!("{}^", " ".repeat(caret_pos));
    format!("{segment}\n{caret_line}")
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
    fn describe_found_eof() {
        assert_eq!(describe_found("hi", 2), "EOF");
    }

    #[test]
    fn summarize_expected_truncates() {
        let labels: Vec<String> = (0..12).map(|i| format!("literal '{i}'")).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let summary = summarize_expected(refs, 8);
        assert!(summary.last().unwrap().contains("more"));
    }
}
