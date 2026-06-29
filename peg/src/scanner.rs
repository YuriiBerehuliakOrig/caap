//! Built-in lexer/scanner — turn source text into a [`LexToken`] stream without
//! an external tokenizer, so a grammar can match on `tok(...)` out of the box.
//!
//! A [`Scanner`] is an ordered list of token rules (each a *kind* + a pattern)
//! plus a set of *skip* (trivia) patterns. Scanning is **maximal munch**: at each
//! position the longest-matching token rule wins, ties broken by declaration
//! order — so declaring `kw("if")` before an identifier rule lets the keyword win
//! on an exact-length tie while `iffy` still lexes as one identifier. Trivia is
//! consumed greedily between tokens and produces no token.
//!
//! Patterns are matched **anchored at the current position** against the
//! remaining input (regex `^`/`\b` therefore see that position as the start of
//! the haystack). The produced tokens are gap-tolerant (trivia leaves holes) and
//! always satisfy the `validate_lex_tokens` contract (ordered, non-overlapping,
//! on char boundaries, text == source slice), so they feed straight into the
//! `tok(...)` path.
//!
//! ```
//! use caap_peg::{Grammar, ParseRequest, Scanner};
//!
//! let scanner = Scanner::new()
//!     .token("NUMBER", r"[0-9]+").unwrap()
//!     .literal("PLUS", "+")
//!     .skip(r"\s+").unwrap();
//! let grammar = Grammar::trusted_new("sum <- tok(NUMBER) (tok(PLUS) tok(NUMBER))*")
//!     .with_start_rule("sum");
//! let value = ParseRequest::new(&grammar).scan(&scanner).run("1 + 22 + 3").unwrap();
//! assert!(matches!(value, caap_peg::ParseValue::Node(..)));
//! ```

use regex::Regex;

use crate::error::ParseError;
use crate::types::LexToken;

/// How a single rule recognises text at the current position.
enum Matcher {
    /// Exact literal text (no regex cost, no escaping).
    Literal(String),
    /// A regex anchored at the current position (compiled with a leading `\A`).
    Regex(Regex),
}

impl Matcher {
    /// Length in bytes of the match at `pos`, or `None` if it does not match.
    /// A zero-width match is reported as `Some(0)`; callers reject it.
    fn match_len(&self, text: &str, pos: usize) -> Option<usize> {
        match self {
            Matcher::Literal(s) => text[pos..].starts_with(s.as_str()).then_some(s.len()),
            // `\A` pins the match to the start of the searched sub-slice (= pos).
            Matcher::Regex(re) => re.find(&text[pos..]).map(|m| m.end()),
        }
    }
}

/// Compile `pattern` anchored at the start of the haystack.
fn anchored(pattern: &str) -> Result<Regex, regex::Error> {
    Regex::new(&format!(r"\A(?:{pattern})"))
}

/// One token-producing rule: the kind tag emitted and the matcher that drives it.
struct ScanRule {
    kind: String,
    matcher: Matcher,
}

/// A declarative lexer: ordered token rules + trivia skip patterns.
///
/// Build one fluently with [`Scanner::token`] / [`Scanner::literal`] /
/// [`Scanner::skip`], then [`Scanner::scan`] text into a `Vec<LexToken>` (or hand
/// it to [`crate::ParseRequest::scan`] to scan-and-parse in one call).
#[derive(Default)]
pub struct Scanner {
    rules: Vec<ScanRule>,
    skips: Vec<Matcher>,
}

impl Scanner {
    /// An empty scanner. Add rules before scanning.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a regex token rule emitting `kind`. The pattern is anchored at the
    /// current position; returns an error if it does not compile.
    pub fn token(mut self, kind: impl Into<String>, pattern: &str) -> Result<Self, regex::Error> {
        self.rules.push(ScanRule {
            kind: kind.into(),
            matcher: Matcher::Regex(anchored(pattern)?),
        });
        Ok(self)
    }

    /// Add an exact-literal token rule emitting `kind` (no regex, no escaping).
    pub fn literal(mut self, kind: impl Into<String>, text: impl Into<String>) -> Self {
        self.rules.push(ScanRule {
            kind: kind.into(),
            matcher: Matcher::Literal(text.into()),
        });
        self
    }

    /// Add a regex trivia pattern: text it matches is consumed without producing
    /// a token. Returns an error if the pattern does not compile.
    pub fn skip(mut self, pattern: &str) -> Result<Self, regex::Error> {
        self.skips.push(Matcher::Regex(anchored(pattern)?));
        Ok(self)
    }

    /// Add an exact-literal trivia pattern (consumed, never tokenised).
    pub fn skip_literal(mut self, text: impl Into<String>) -> Self {
        self.skips.push(Matcher::Literal(text.into()));
        self
    }

    /// Longest trivia match at `pos` (`0` if none / only zero-width).
    fn skip_len(&self, text: &str, pos: usize) -> usize {
        self.skips
            .iter()
            .filter_map(|m| m.match_len(text, pos))
            .max()
            .unwrap_or(0)
    }

    /// Tokenise `text`. Trivia is skipped greedily; at each remaining position the
    /// longest-matching token rule wins (ties → declaration order). Fails at the
    /// first byte no rule and no skip can advance past.
    pub fn scan(&self, text: &str) -> Result<Vec<LexToken>, ParseError> {
        let mut tokens = Vec::new();
        let mut pos = 0;
        while pos < text.len() {
            let skipped = self.skip_len(text, pos);
            if skipped > 0 {
                pos += skipped;
                continue;
            }
            // Maximal munch: scan every rule, keep the longest; `>` (not `>=`)
            // preserves declaration order on a length tie.
            let mut best: Option<(usize, &str)> = None;
            for rule in &self.rules {
                let Some(len) = rule.matcher.match_len(text, pos) else {
                    continue;
                };
                if len == 0 {
                    continue; // a zero-width rule must never tokenise
                }
                if best.is_none_or(|(best_len, _)| len > best_len) {
                    best = Some((len, &rule.kind));
                }
            }
            match best {
                Some((len, kind)) => {
                    let end = pos + len;
                    tokens.push(LexToken::new(kind, &text[pos..end], pos, end));
                    pos = end;
                }
                None => {
                    return Err(ParseError::new(
                        format!("scanner: no token rule matches at byte {pos}"),
                        pos,
                        text.len(),
                    ));
                }
            }
        }
        Ok(tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arith() -> Scanner {
        Scanner::new()
            .token("NUMBER", r"[0-9]+")
            .unwrap()
            .literal("PLUS", "+")
            .literal("STAR", "*")
            .skip(r"\s+")
            .unwrap()
    }

    #[test]
    fn scans_numbers_operators_and_skips_whitespace() {
        let toks = arith().scan("12 + 3*45").unwrap();
        let view: Vec<(&str, &str)> = toks
            .iter()
            .map(|t| (t.kind.as_str(), t.text.as_str()))
            .collect();
        assert_eq!(
            view,
            vec![
                ("NUMBER", "12"),
                ("PLUS", "+"),
                ("NUMBER", "3"),
                ("STAR", "*"),
                ("NUMBER", "45"),
            ]
        );
        // Spans line up with the source slices (validate_lex_tokens contract).
        assert_eq!(toks[0].start, 0);
        assert_eq!(toks[0].end, 2);
        assert_eq!(toks[2].start, 5); // after "12 + "
    }

    #[test]
    fn maximal_munch_prefers_the_longest_rule() {
        let s = Scanner::new()
            .literal("PLUS", "+")
            .literal("PLUSPLUS", "++");
        let toks = s.scan("+++").unwrap();
        // "++" (len 2) beats "+" (len 1), then a trailing "+".
        let view: Vec<&str> = toks.iter().map(|t| t.kind.as_str()).collect();
        assert_eq!(view, vec!["PLUSPLUS", "PLUS"]);
    }

    #[test]
    fn declaration_order_breaks_length_ties_keyword_over_ident() {
        let s = Scanner::new()
            .token("IF", r"if\b")
            .unwrap()
            .token("IDENT", r"[a-z]+")
            .unwrap();
        assert_eq!(s.scan("if").unwrap()[0].kind, "IF");
        // Longer identifier wins despite the keyword prefix matching.
        assert_eq!(s.scan("iffy").unwrap()[0].kind, "IDENT");
    }

    #[test]
    fn errors_on_an_unrecognised_byte() {
        let err = arith().scan("12 @ 3").unwrap_err();
        assert!(
            err.message.contains("no token rule matches"),
            "{}",
            err.message
        );
        assert_eq!(err.span.start, 3);
    }

    #[test]
    fn trailing_trivia_is_consumed() {
        let toks = arith().scan("7   ").unwrap();
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].kind, "NUMBER");
    }

    #[test]
    fn bad_pattern_is_a_compile_error() {
        assert!(Scanner::new().token("X", r"[").is_err());
        assert!(Scanner::new().skip(r"(").is_err());
    }
}
