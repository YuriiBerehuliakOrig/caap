//! Trivia skipping — the [`SkipStrategy`] trait and built-in strategies
//! (whitespace, line comments, regex, and the full [`DefaultSkipStrategy`]),
//! selected from grammar metadata by `skip_strategy_from_metadata`.

use regex::Regex;
use std::collections::HashSet;

// ── Skip trait definitions ───────────────────────────────────────────────

/// Failure while skipping trivia (e.g. unterminated block comment).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkipError {
    /// The failure message.
    pub message: String,
    /// Byte position where skipping failed.
    pub pos: usize,
}

/// Callable that advances `pos` past trivia (whitespace / comments).
pub trait SkipStrategy: SkipStrategyClone + Send + Sync {
    /// Advance past trivia at `pos`, returning the new position.
    fn try_skip(&self, text: &str, pos: usize) -> Result<usize, SkipError>;
}

/// Blanket cloning support for boxed skip strategies.
pub trait SkipStrategyClone {
    /// Clone into a fresh boxed strategy.
    fn clone_box(&self) -> Box<dyn SkipStrategy>;
}

impl<T> SkipStrategyClone for T
where
    T: 'static + SkipStrategy + Clone,
{
    fn clone_box(&self) -> Box<dyn SkipStrategy> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn SkipStrategy> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// Optional extension point for parser protocol hooks. It is currently unused
/// by the parser cache key.
pub trait StatefulSkipStrategy: SkipStrategy {
    /// Project the layout state into a cache key (identity by default).
    fn layout_state_key(&self, _layout_state: usize) -> usize {
        _layout_state
    }
}

// ── Constants ─────────────────────────────────────────────────────────────

/// Default whitespace characters skipped as trivia.
pub const DEFAULT_WHITESPACE: &str = " \t\r\n";
/// Default line-comment prefixes.
pub const DEFAULT_LINE_COMMENTS: [&str; 1] = [";"];
/// Default block-comment delimiter pairs.
pub const DEFAULT_BLOCK_COMMENTS: [(&str, &str); 2] = [("#|", "|#"), ("/*", "*/")];

// ── Patterns ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) enum SkipMatch {
    Literal(String),
    Regex(Regex),
}

impl SkipMatch {
    fn literal(value: impl Into<String>) -> Self {
        Self::Literal(value.into())
    }

    fn regex(value: &str) -> Result<Self, regex::Error> {
        Ok(Self::Regex(Regex::new(value)?))
    }

    fn end_at(&self, text: &str, pos: usize) -> Option<usize> {
        if pos > text.len() {
            return None;
        }
        match self {
            Self::Literal(lit) => {
                if text[pos..].starts_with(lit) {
                    Some(pos + lit.len())
                } else {
                    None
                }
            }
            Self::Regex(regex) => regex
                .find_at(text, pos)
                .filter(|m| m.start() == pos)
                .map(|m| m.end()),
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn compute_whitespace_set(raw: &str) -> HashSet<u8> {
    raw.bytes().collect()
}

fn is_whitespace(whitespace: &HashSet<u8>, text: &str, pos: usize) -> bool {
    if pos >= text.len() {
        return false;
    }
    whitespace.contains(&text.as_bytes()[pos])
}

fn parse_regex_or_literal(raw: &str) -> Result<SkipMatch, regex::Error> {
    if let Some(pattern) = raw.strip_prefix("regex:") {
        return SkipMatch::regex(pattern);
    }
    Ok(SkipMatch::literal(raw))
}

// ── No-op skipper ────────────────────────────────────────────────────────

/// A [`SkipStrategy`] that does not consume anything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSkipStrategy;

impl SkipStrategy for NoSkipStrategy {
    fn try_skip(&self, _text: &str, pos: usize) -> Result<usize, SkipError> {
        Ok(pos)
    }
}

impl StatefulSkipStrategy for NoSkipStrategy {}

/// Public zero-advancement skipper constant.
pub const NO_SKIPPER: NoSkipStrategy = NoSkipStrategy;

// ── Regex skip ────────────────────────────────────────────────────────────

/// A regex-driven strategy that advances while a regex continues to match at the
/// current position.
#[derive(Debug, Clone)]
/// Skips runs matching a regex anchored at the current position.
pub struct RegexSkipStrategy {
    pattern: Regex,
}

impl RegexSkipStrategy {
    /// Build a regex skipper from `pattern`.
    pub fn new(pattern: &str) -> Result<Self, regex::Error> {
        Ok(Self {
            pattern: Regex::new(pattern)?,
        })
    }
}

impl SkipStrategy for RegexSkipStrategy {
    fn try_skip(&self, text: &str, mut pos: usize) -> Result<usize, SkipError> {
        loop {
            let suffix = &text[pos..];
            match self.pattern.find(suffix) {
                Some(m) if m.start() == 0 => pos += m.end(),
                _ => break,
            }
        }
        Ok(pos)
    }
}

impl StatefulSkipStrategy for RegexSkipStrategy {}

// ── ASCII whitespace and line-comment skipper ────────────────────────────

#[derive(Debug, Default, Clone)]
/// Skips ASCII whitespace only.
pub struct WhitespaceSkipStrategy;

impl SkipStrategy for WhitespaceSkipStrategy {
    fn try_skip(&self, text: &str, mut pos: usize) -> Result<usize, SkipError> {
        while pos < text.len() && text.as_bytes()[pos].is_ascii_whitespace() {
            pos += 1;
        }
        Ok(pos)
    }
}

impl StatefulSkipStrategy for WhitespaceSkipStrategy {}

#[derive(Debug, Clone)]
/// Skips whitespace and single-character-prefixed line comments.
pub struct LineCommentSkipStrategy {
    comment_prefix: u8,
}

impl LineCommentSkipStrategy {
    /// Build a skipper for line comments starting with `comment_char`.
    pub fn new(comment_char: char) -> Self {
        Self {
            comment_prefix: comment_char as u8,
        }
    }

    /// A `#`-comment skipper.
    pub fn hash_comments() -> Self {
        Self::new('#')
    }

    /// A `;`-comment skipper.
    pub fn semicolon_comments() -> Self {
        Self::new(';')
    }
}

impl SkipStrategy for LineCommentSkipStrategy {
    fn try_skip(&self, text: &str, mut pos: usize) -> Result<usize, SkipError> {
        let bytes = text.as_bytes();
        loop {
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if pos < bytes.len() && bytes[pos] == self.comment_prefix {
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
                continue;
            }
            break;
        }
        Ok(pos)
    }
}

impl StatefulSkipStrategy for LineCommentSkipStrategy {}

// ── Default skip strategy ────────────────────────────────────────────────

/// Full trivia skipper supporting whitespace, line comments and nested block comments.
#[derive(Debug, Clone)]
pub struct DefaultSkipStrategy {
    whitespace: HashSet<u8>,
    line_comments: Vec<SkipMatch>,
    block_comments: Vec<(SkipMatch, SkipMatch)>,
}

impl Default for DefaultSkipStrategy {
    fn default() -> Self {
        Self::new(
            DEFAULT_WHITESPACE,
            DEFAULT_LINE_COMMENTS
                .iter()
                .map(|comment| SkipMatch::literal(*comment)),
            DEFAULT_BLOCK_COMMENTS
                .iter()
                .map(|(start, end)| (SkipMatch::literal(*start), SkipMatch::literal(*end))),
        )
    }
}

impl DefaultSkipStrategy {
    pub(crate) fn new(
        whitespace: &str,
        line_comments: impl IntoIterator<Item = SkipMatch>,
        block_comments: impl IntoIterator<Item = (SkipMatch, SkipMatch)>,
    ) -> Self {
        Self {
            whitespace: compute_whitespace_set(whitespace),
            line_comments: line_comments.into_iter().collect(),
            block_comments: block_comments.into_iter().collect(),
        }
    }

    /// Build a default skipper from raw whitespace/comment pattern strings.
    pub fn new_with_raw_patterns(
        whitespace: &str,
        line_comments: impl IntoIterator<Item = String>,
        block_comments: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, regex::Error> {
        let line_patterns = line_comments
            .into_iter()
            .map(|pattern| parse_regex_or_literal(&pattern))
            .collect::<Result<Vec<_>, _>>()?;
        let block_patterns = block_comments
            .into_iter()
            .map(|(start, end)| {
                let parsed_start = parse_regex_or_literal(&start)?;
                let parsed_end = parse_regex_or_literal(&end)?;
                Ok((parsed_start, parsed_end))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self::new(whitespace, line_patterns, block_patterns))
    }

    fn skip_line_comment(&self, text: &str, pos: usize) -> Option<usize> {
        for pattern in &self.line_comments {
            if pattern.end_at(text, pos).is_some() {
                // Jump straight to the line's newline (memchr-backed) instead of
                // scanning byte by byte; consume it, or run to end-of-input.
                return Some(match text[pos..].find('\n') {
                    Some(off) => (pos + off + 1).min(text.len()),
                    None => text.len(),
                });
            }
        }
        None
    }

    fn skip_nested_block_comment(&self, text: &str, pos: usize) -> Option<Result<usize, usize>> {
        for (start, end) in &self.block_comments {
            let Some(first) = start.end_at(text, pos) else {
                continue;
            };
            let start_pos = pos;
            // Fast path: literal delimiters → jump to the earliest next open/close
            // marker (memchr/Two-Way-backed `str::find`) instead of testing every
            // byte position. Preserves the original "open checked first" priority.
            if let (SkipMatch::Literal(s), SkipMatch::Literal(e)) = (start, end) {
                let mut depth = 1usize;
                let mut cursor = first;
                loop {
                    let ns = text[cursor..].find(s.as_str()).map(|o| cursor + o);
                    let ne = text[cursor..].find(e.as_str()).map(|o| cursor + o);
                    match (ns, ne) {
                        (_, None) => return Some(Err(start_pos)),
                        (Some(o), Some(c)) if o <= c => {
                            depth = depth.saturating_add(1);
                            cursor = o + s.len();
                        }
                        (_, Some(c)) => {
                            depth = depth.saturating_sub(1);
                            cursor = c + e.len();
                            if depth == 0 {
                                return Some(Ok(cursor));
                            }
                        }
                    }
                }
            }
            // Fallback (regex delimiters): scan position by position.
            let mut depth = 1usize;
            let mut cursor = first;
            while cursor < text.len() {
                let mut advanced = false;
                if let Some(next_start) = start.end_at(text, cursor) {
                    depth = depth.saturating_add(1);
                    if next_start <= cursor {
                        break;
                    }
                    cursor = next_start;
                    advanced = true;
                } else if let Some(next_end) = end.end_at(text, cursor) {
                    depth = depth.saturating_sub(1);
                    cursor = next_end;
                    advanced = true;
                    if depth == 0 {
                        return Some(Ok(cursor));
                    }
                }

                if !advanced {
                    cursor += 1;
                }
            }
            if depth > 0 {
                return Some(Err(start_pos));
            }
            return Some(Ok(cursor));
        }
        None
    }
}

impl SkipStrategy for DefaultSkipStrategy {
    fn try_skip(&self, text: &str, mut pos: usize) -> Result<usize, SkipError> {
        while pos < text.len() {
            let start = pos;
            if is_whitespace(&self.whitespace, text, pos) {
                pos += 1;
                continue;
            }

            if let Some(after_comment) = self.skip_line_comment(text, pos) {
                pos = after_comment;
                continue;
            }

            if let Some(block_result) = self.skip_nested_block_comment(text, pos) {
                match block_result {
                    Ok(after_block) => pos = after_block,
                    Err(comment_start) => {
                        return Err(SkipError {
                            message: "Unterminated block comment".to_string(),
                            pos: comment_start,
                        });
                    }
                }
                continue;
            }

            if pos == start {
                break;
            }
        }
        Ok(pos)
    }
}

impl StatefulSkipStrategy for DefaultSkipStrategy {}

// Boxed helper and metadata-based factory.
/// A boxed, dynamically-dispatched [`SkipStrategy`].
pub type BoxedSkipStrategy = Box<dyn SkipStrategy>;

/// Build a concrete strategy from a metadata token:
/// - `None`/`""`/`"none"` => no skipping
/// - `"whitespace"` => whitespace-only skipping
/// - `"default"` => default trivia skipping with comment styles
/// - any other pattern => regex-based skipper using that pattern.
///
/// Invalid regex patterns are reported to the caller; parser metadata must not
/// silently change the grammar's trivia semantics.
pub fn skip_strategy_from_config(
    trivia: Option<&str>,
) -> Result<Option<BoxedSkipStrategy>, regex::Error> {
    match trivia {
        None | Some("") | Some("none") => Ok(None),
        Some("whitespace") => Ok(Some(Box::new(WhitespaceSkipStrategy))),
        Some("default") => Ok(Some(Box::new(DefaultSkipStrategy::default()))),
        Some(pattern) => Ok(Some(Box::new(RegexSkipStrategy::new(pattern)?))),
    }
}

/// Public constructor using the default skip strategy configuration.
pub fn make_skipper() -> DefaultSkipStrategy {
    DefaultSkipStrategy::default()
}

/// Public constructor for custom skip setup.
pub fn make_skipper_with_patterns(
    whitespace: &str,
    line_comments: &[&str],
    block_comments: &[(&str, &str)],
) -> Result<DefaultSkipStrategy, regex::Error> {
    DefaultSkipStrategy::new_with_raw_patterns(
        whitespace,
        line_comments.iter().map(|v| v.to_string()),
        block_comments
            .iter()
            .map(|(start, end)| (start.to_string(), end.to_string())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_skip_strategy_is_identity() {
        let s = NoSkipStrategy;
        assert_eq!(s.try_skip("  hello", 0).unwrap(), 0);
        assert_eq!(s.try_skip("hello", 5).unwrap(), 5);
    }

    #[test]
    fn whitespace_skip_strategy_advances_past_spaces() {
        let s = WhitespaceSkipStrategy;
        assert_eq!(s.try_skip("   hello", 0).unwrap(), 3);
        assert_eq!(s.try_skip("\t\nhello", 0).unwrap(), 2);
        assert_eq!(s.try_skip("hello", 0).unwrap(), 0);
    }

    #[test]
    fn whitespace_skip_strategy_stops_at_non_whitespace() {
        let s = WhitespaceSkipStrategy;
        assert_eq!(s.try_skip("  x  ", 0).unwrap(), 2);
    }

    #[test]
    fn default_skip_strategy_skips_comments_and_whitespace() {
        let s = DefaultSkipStrategy::default();
        assert_eq!(s.try_skip("  ;x\nabc", 0).unwrap(), 5);
    }

    #[test]
    fn default_skip_strategy_skips_nested_comments() {
        let s = DefaultSkipStrategy::default();
        assert_eq!(
            s.try_skip("#| outer #| inner |# done |#abc", 0).unwrap(),
            28
        );
    }

    #[test]
    fn block_comment_fast_path_handles_long_and_nested_bodies() {
        let s = DefaultSkipStrategy::default();
        // Long body — exercises the jump (vs byte-by-byte) path.
        let body = "x".repeat(5000);
        let text = format!("#| {body} |#tail");
        let pos = s.try_skip(&text, 0).unwrap();
        assert_eq!(&text[pos..], "tail");
        // Deeply nested.
        let nested = "#|a#|b#|c|#d|#e|#z";
        let p = s.try_skip(nested, 0).unwrap();
        assert_eq!(&nested[p..], "z");
        // Unterminated → error at the opening delimiter.
        let err = s.try_skip("#| never closed", 0).unwrap_err();
        assert_eq!(err.pos, 0);
    }

    #[test]
    fn line_comment_fast_path_runs_to_newline_or_eof() {
        let s = DefaultSkipStrategy::default();
        let text = format!("; {}\nrest", "c".repeat(4000));
        let pos = s.try_skip(&text, 0).unwrap();
        assert_eq!(&text[pos..], "rest");
        // Comment with no trailing newline consumes to end-of-input.
        let eof = "; trailing comment";
        assert_eq!(s.try_skip(eof, 0).unwrap(), eof.len());
    }

    #[test]
    fn regex_skip_strategy_advances_past_matches() {
        let s = RegexSkipStrategy::new(r"[ \t]+").unwrap();
        assert_eq!(s.try_skip("   hello", 0).unwrap(), 3);
        assert_eq!(s.try_skip("hello", 0).unwrap(), 0);
    }

    #[test]
    fn regex_skip_strategy_loops_until_no_match() {
        let s = RegexSkipStrategy::new(r" ").unwrap();
        assert_eq!(s.try_skip("    x", 0).unwrap(), 4);
    }

    #[test]
    fn line_comment_skip_strategy_skips_hash_comments() {
        let s = LineCommentSkipStrategy::hash_comments();
        let text = "   # this is a comment\nhello";
        let pos = s.try_skip(text, 0).unwrap();
        assert_eq!(&text[pos..], "hello");
    }

    #[test]
    fn make_skipper_defaults_are_available() {
        let s = make_skipper();
        assert_eq!(s.try_skip("  ;x\nx", 0).unwrap(), 5);
    }

    #[test]
    fn no_skipper_constant_is_identity() {
        assert_eq!(NO_SKIPPER.try_skip(" abc", 2).unwrap(), 2);
    }

    #[test]
    fn skip_strategy_from_config_default() {
        let s = skip_strategy_from_config(Some("default"))
            .expect("default should compile")
            .expect("default skipper is present");
        assert_eq!(s.try_skip(" ;x\nabc", 0).unwrap(), 4);
    }

    #[test]
    fn skip_strategy_from_config_none() {
        assert!(skip_strategy_from_config(Some("none")).unwrap().is_none());
    }

    #[test]
    fn skip_strategy_from_config_rejects_invalid_regex() {
        assert!(skip_strategy_from_config(Some("[")).is_err());
    }

    #[test]
    fn raw_pattern_constructor_rejects_invalid_explicit_regex() {
        assert!(make_skipper_with_patterns(" ", &["regex:["], &[]).is_err());
        assert!(make_skipper_with_patterns(" ", &[], &[("regex:[", "|#")]).is_err());
        assert!(make_skipper_with_patterns(" ", &[], &[("#|", "regex:[")]).is_err());
    }

    #[test]
    fn unterminated_block_comment_returns_error() {
        let s = DefaultSkipStrategy::default();
        let err = s.try_skip("#| broken", 0).expect_err("must fail");
        assert_eq!(err.message, "Unterminated block comment");
        assert_eq!(err.pos, 0);
    }

    #[test]
    fn default_skipper_skips_whitespace_and_comments() {
        let text = " \n; line\n#| block |#abc";
        let pos = make_skipper().try_skip(text, 0).expect("skip ok");
        assert_eq!(pos, text.find('a').unwrap());
    }
}
