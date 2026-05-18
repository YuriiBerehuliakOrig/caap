use regex::Regex;
use std::collections::HashSet;

// ── Skip trait definitions ───────────────────────────────────────────────

/// Failure while skipping trivia (e.g. unterminated block comment).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkipError {
    pub message: String,
    pub pos: usize,
}

/// Callable that advances `pos` past trivia (whitespace / comments).
pub trait SkipStrategy: SkipStrategyClone + Send + Sync {
    fn try_skip(&self, text: &str, pos: usize) -> Result<usize, SkipError>;

    fn skip(&self, text: &str, pos: usize) -> usize {
        self.try_skip(text, pos).unwrap_or(pos)
    }
}

/// Blanket cloning support for boxed skip strategies.
pub trait SkipStrategyClone {
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

/// Optional extension point mirroring parser protocol hooks from the Python
/// implementation. It is currently unused by the Rust parser cache key.
pub trait StatefulSkipStrategy: SkipStrategy {
    fn layout_state_key(&self, _layout_state: usize) -> usize {
        _layout_state
    }
}

// ── Constants ─────────────────────────────────────────────────────────────

pub const DEFAULT_WHITESPACE: &str = " \t\r\n";
pub const DEFAULT_LINE_COMMENTS: [&str; 1] = [";"];
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

fn parse_regex_or_literal(raw: &str) -> SkipMatch {
    // Keep behavior permissive: values starting with "regex:" are parsed as regex;
    // everything else is treated as a literal token. This allows ergonomic API use
    // while still supporting explicit regex creation through dedicated helper.
    if let Some(pattern) = raw.strip_prefix("regex:") {
        if let Ok(regex) = SkipMatch::regex(pattern) {
            return regex;
        }
    }
    SkipMatch::literal(raw)
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
pub struct RegexSkipStrategy {
    pattern: Regex,
}

impl RegexSkipStrategy {
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
pub struct LineCommentSkipStrategy {
    comment_prefix: u8,
}

impl LineCommentSkipStrategy {
    pub fn new(comment_char: char) -> Self {
        Self {
            comment_prefix: comment_char as u8,
        }
    }

    pub fn hash_comments() -> Self {
        Self::new('#')
    }

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

    pub fn new_with_raw_patterns(
        whitespace: &str,
        line_comments: impl IntoIterator<Item = String>,
        block_comments: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, regex::Error> {
        let line_patterns = line_comments
            .into_iter()
            .map(|pattern| parse_regex_or_literal(&pattern));
        let block_patterns = block_comments
            .into_iter()
            .map(|(start, end)| {
                let parsed_start = parse_regex_or_literal(&start);
                let parsed_end = parse_regex_or_literal(&end);
                Ok((parsed_start, parsed_end))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self::new(whitespace, line_patterns, block_patterns))
    }

    fn skip_line_comment(&self, text: &str, pos: usize) -> Option<usize> {
        for pattern in &self.line_comments {
            if let Some(_end) = pattern.end_at(text, pos) {
                let mut search_pos = pos;
                if search_pos < text.len() {
                    while search_pos < text.len() && text.as_bytes()[search_pos] != b'\n' {
                        search_pos += 1;
                    }
                }
                return Some(search_pos.saturating_add(1).min(text.len()));
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
pub type BoxedSkipStrategy = Box<dyn SkipStrategy>;

/// Build a concrete strategy from a metadata token:
/// - `None`/`""`/`"none"` => no skipping
/// - `"whitespace"` => whitespace-only skipping
/// - `"default"` => default trivia skipping with comment styles
/// - any other pattern => regex-based skipper using that pattern
pub fn skip_strategy_from_config(trivia: Option<&str>) -> Option<BoxedSkipStrategy> {
    match trivia? {
        "" | "none" => None,
        "whitespace" => Some(Box::new(WhitespaceSkipStrategy)),
        "default" => Some(Box::new(DefaultSkipStrategy::default())),
        pattern => Some(Box::new(RegexSkipStrategy::new(pattern).unwrap_or_else(
            |_| {
                RegexSkipStrategy::new(r"[ \t\r\n]+")
                    .unwrap_or_else(|_| RegexSkipStrategy::new("[ \\t\\r\\n]+").unwrap())
            },
        ))),
    }
}

/// Public constructor mirroring Python signature defaults.
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
        assert_eq!(s.skip("  hello", 0), 0);
        assert_eq!(s.skip("hello", 5), 5);
    }

    #[test]
    fn whitespace_skip_strategy_advances_past_spaces() {
        let s = WhitespaceSkipStrategy;
        assert_eq!(s.skip("   hello", 0), 3);
        assert_eq!(s.skip("\t\nhello", 0), 2);
        assert_eq!(s.skip("hello", 0), 0);
    }

    #[test]
    fn whitespace_skip_strategy_stops_at_non_whitespace() {
        let s = WhitespaceSkipStrategy;
        assert_eq!(s.skip("  x  ", 0), 2);
    }

    #[test]
    fn default_skip_strategy_skips_comments_and_whitespace() {
        let s = DefaultSkipStrategy::default();
        assert_eq!(s.skip("  ;x\nabc", 0), 5);
    }

    #[test]
    fn default_skip_strategy_skips_nested_comments() {
        let s = DefaultSkipStrategy::default();
        assert_eq!(s.skip("#| outer #| inner |# done |#abc", 0), 28);
    }

    #[test]
    fn regex_skip_strategy_advances_past_matches() {
        let s = RegexSkipStrategy::new(r"[ \t]+").unwrap();
        assert_eq!(s.skip("   hello", 0), 3);
        assert_eq!(s.skip("hello", 0), 0);
    }

    #[test]
    fn regex_skip_strategy_loops_until_no_match() {
        let s = RegexSkipStrategy::new(r" ").unwrap();
        assert_eq!(s.skip("    x", 0), 4);
    }

    #[test]
    fn line_comment_skip_strategy_skips_hash_comments() {
        let s = LineCommentSkipStrategy::hash_comments();
        let text = "   # this is a comment\nhello";
        let pos = s.skip(text, 0);
        assert_eq!(&text[pos..], "hello");
    }

    #[test]
    fn make_skipper_defaults_are_available() {
        let s = make_skipper();
        assert_eq!(s.skip("  ;x\nx", 0), 5);
    }

    #[test]
    fn no_skipper_constant_is_identity() {
        assert_eq!(NO_SKIPPER.skip(" abc", 2), 2);
    }

    #[test]
    fn skip_strategy_from_config_default() {
        let s = skip_strategy_from_config(Some("default")).expect("default should compile");
        assert_eq!(s.skip(" ;x\nabc", 0), 4);
    }

    #[test]
    fn skip_strategy_from_config_none() {
        assert!(skip_strategy_from_config(Some("none")).is_none());
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
