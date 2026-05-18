//! PEG expression tree — `PegExpr`, `CompiledRegex`, `RuleTextParser`, `peg_expr_to_source`.
//!
//! Moved here from `parser.rs` so that `grammar.rs` can store compiled expression
//! trees directly in `GrammarRule`, eliminating repeated text-parsing on every
//! `parse()` call.

use regex::Regex as StdRegex;

use crate::behaviors::BehaviorEntry;
use crate::error::ParseError;

// ── CompiledRegex ─────────────────────────────────────────────────────────────

/// A compiled regex that also retains the source pattern for equality checks.
///
/// `regex::Regex` clones cheaply (internally `Arc`-backed), so cloning a
/// `CompiledRegex` does not recompile the pattern.
#[derive(Clone, Debug)]
pub struct CompiledRegex {
    pub pattern: String,
    pub inner: StdRegex,
}

impl CompiledRegex {
    pub fn new(
        pattern: impl Into<String>,
        err_start: usize,
        err_end: usize,
    ) -> Result<Self, ParseError> {
        let pattern = pattern.into();
        let inner = StdRegex::new(&pattern).map_err(|e| {
            ParseError::new(
                format!("invalid regular expression: {e}"),
                err_start,
                err_end,
            )
        })?;
        Ok(Self { pattern, inner })
    }
}

impl PartialEq for CompiledRegex {
    fn eq(&self, other: &Self) -> bool {
        self.pattern == other.pattern
    }
}

impl Eq for CompiledRegex {}

// ── PegExpr ───────────────────────────────────────────────────────────────────

/// A parsed PEG grammar expression tree.
///
/// Grammar rules now store `PegExpr` directly (in `GrammarRule.expr`) so the
/// parser can evaluate trees without reparsing text on every `parse()` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PegExpr {
    // ── Terminals ─────────────────────────────────────────────────────────
    Literal(String),
    Dot,
    Regex(CompiledRegex),
    // ── Structural combinators ────────────────────────────────────────────
    Ref(String),
    Sequence(Vec<PegExpr>),
    Choice(Vec<PegExpr>),
    And(Box<PegExpr>),
    Not(Box<PegExpr>),
    Cut,
    Optional(Box<PegExpr>),
    OneOrMore(Box<PegExpr>),
    ZeroOrMore(Box<PegExpr>),
    /// `element (separator element)*`
    SepOneOrMore {
        element: Box<PegExpr>,
        separator: Box<PegExpr>,
    },
    // ── Value bindings ────────────────────────────────────────────────────
    Named {
        name: String,
        expr: Box<PegExpr>,
    },
    // ── Error-label override ──────────────────────────────────────────────
    Expected {
        message: String,
        expr: Box<PegExpr>,
    },
    // ── Trivia control ────────────────────────────────────────────────────
    NoTrivia(Box<PegExpr>),
    // ── Layout-sensitive terminals ────────────────────────────────────────
    Newline,
    Indent,
    Dedent,
    // ── Semantic hooks ────────────────────────────────────────────────────
    SemanticAction {
        name: String,
        expr: Box<PegExpr>,
    },
    SemanticPredicate {
        name: String,
    },
    Behavior {
        entries: Vec<BehaviorEntry>,
        expr: Box<PegExpr>,
    },
    // ── Span capture ─────────────────────────────────────────────────────
    Capture {
        label: String,
        expr: Box<PegExpr>,
    },
    // ── Delimiter-bounded matching ────────────────────────────────────────
    Island {
        start: String,
        end: String,
        include_delims: bool,
    },
    RawBlock {
        start: String,
        end: String,
        delim_kind: String,
    },
    // ── Committed failure ─────────────────────────────────────────────────
    Eager(Box<PegExpr>),
    // ── Cross-grammar reference ───────────────────────────────────────────
    ImportedRef {
        grammar_name: String,
        rule_name: String,
    },
    // ── Parametric rules ──────────────────────────────────────────────────
    Parameter {
        name: String,
    },
    Call {
        rule: String,
        args: Vec<PegExpr>,
    },
    // ── Keyword terminals ─────────────────────────────────────────────────
    HardKeyword(String),
    SoftKeyword(String),
    // ── Grammar scope ─────────────────────────────────────────────────────
    GrammarScope {
        grammar_name: String,
        expr: Box<PegExpr>,
    },
    // ── Token-stream terminal ─────────────────────────────────────────────
    TokenRef {
        kind: Option<String>,
        text: Option<String>,
    },
    // ── Parse-time placeholder ────────────────────────────────────────────
    /// Rule source that failed to parse; surfaces as an error when the
    /// grammar is compiled for a parse call.
    Invalid(String),
}

// ── peg_expr_to_source ────────────────────────────────────────────────────────

/// Convert a `PegExpr` tree back to PEG text.
///
/// The output is a valid PEG expression string that round-trips through
/// `RuleTextParser::parse`.  Used when the caller needs the textual form of
/// a programmatically constructed expression.
pub fn peg_expr_to_source(expr: &PegExpr) -> String {
    match expr {
        PegExpr::Literal(s) => quote_string(s),
        PegExpr::Dot => ".".to_string(),
        PegExpr::Regex(r) => {
            // Char-class patterns already carry their brackets.
            if r.pattern.starts_with('[') && r.pattern.ends_with(']') {
                r.pattern.clone()
            } else {
                format!("/{}/", r.pattern)
            }
        }
        PegExpr::Ref(name) => name.clone(),
        PegExpr::Sequence(exprs) => {
            if exprs.is_empty() {
                return "\"\"".to_string();
            }
            exprs.iter().map(seq_item).collect::<Vec<_>>().join(" ")
        }
        PegExpr::Choice(exprs) => exprs.iter().map(choice_alt).collect::<Vec<_>>().join(" / "),
        PegExpr::And(e) => format!("&{}", prefix_operand(e)),
        PegExpr::Not(e) => format!("!{}", prefix_operand(e)),
        PegExpr::Cut => "~".to_string(),
        PegExpr::Optional(e) => format!("{}?", rep_operand(e)),
        PegExpr::OneOrMore(e) => format!("{}+", rep_operand(e)),
        PegExpr::ZeroOrMore(e) => format!("{}*", rep_operand(e)),
        PegExpr::SepOneOrMore { element, separator } => {
            format!(
                "sep_plus({}, {})",
                peg_expr_to_source(element),
                peg_expr_to_source(separator)
            )
        }
        PegExpr::Named { name, expr } => format!("{}:{}", name, rep_operand(expr)),
        PegExpr::Expected { message, expr } => {
            format!(
                "expected({}, {})",
                quote_string(message),
                peg_expr_to_source(expr)
            )
        }
        PegExpr::NoTrivia(e) => format!("no_trivia({})", peg_expr_to_source(e)),
        PegExpr::Newline => "newline".to_string(),
        PegExpr::Indent => "indent".to_string(),
        PegExpr::Dedent => "dedent".to_string(),
        PegExpr::SemanticAction { name, expr } => {
            format!("@{}({})", name, peg_expr_to_source(expr))
        }
        PegExpr::SemanticPredicate { name } => format!("@?{}", name),
        PegExpr::Behavior { expr, .. } => peg_expr_to_source(expr),
        PegExpr::Capture { label, expr } => {
            format!(
                "capture({}, {})",
                quote_string(label),
                peg_expr_to_source(expr)
            )
        }
        PegExpr::Island {
            start,
            end,
            include_delims,
        } => {
            if *include_delims {
                format!(
                    "island({}, {}, true)",
                    quote_string(start),
                    quote_string(end)
                )
            } else {
                format!("island({}, {})", quote_string(start), quote_string(end))
            }
        }
        PegExpr::RawBlock {
            start,
            end,
            delim_kind,
        } => {
            format!(
                "raw_block({}, {}, {})",
                quote_string(start),
                quote_string(end),
                quote_string(delim_kind)
            )
        }
        PegExpr::Eager(e) => format!("!!{}", prefix_operand(e)),
        PegExpr::ImportedRef {
            grammar_name,
            rule_name,
        } => {
            format!("{}::{}", grammar_name, rule_name)
        }
        PegExpr::Parameter { name } => format!("${}", name),
        PegExpr::Call { rule, args } => {
            let arg_str = args
                .iter()
                .map(peg_expr_to_source)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({})", rule, arg_str)
        }
        PegExpr::HardKeyword(kw) => format!("kw({})", quote_string(kw)),
        PegExpr::SoftKeyword(kw) => format!("soft_keyword({})", quote_string(kw)),
        PegExpr::GrammarScope { grammar_name, expr } => {
            format!(
                "scope({}, {})",
                quote_string(grammar_name),
                peg_expr_to_source(expr)
            )
        }
        PegExpr::TokenRef { kind, text } => match (kind, text) {
            (Some(k), Some(t)) => format!("tok({}, {})", k, quote_string(t)),
            (Some(k), None) => format!("tok({})", k),
            (None, Some(t)) => format!("tok({})", quote_string(t)),
            (None, None) => "tok()".to_string(),
        },
        PegExpr::Invalid(s) => s.clone(),
    }
}

fn quote_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Wrap in parens when expr has lower precedence than a prefix operator operand.
fn prefix_operand(expr: &PegExpr) -> String {
    match expr {
        PegExpr::Sequence(_) | PegExpr::Choice(_) => format!("({})", peg_expr_to_source(expr)),
        _ => peg_expr_to_source(expr),
    }
}

/// Wrap in parens when expr has lower precedence than a repetition operand.
///
/// Named bindings (`x:e`) are also wrapped because `x:e?` parses as `x:(e?)`.
fn rep_operand(expr: &PegExpr) -> String {
    match expr {
        PegExpr::Sequence(_) | PegExpr::Choice(_) | PegExpr::Named { .. } => {
            format!("({})", peg_expr_to_source(expr))
        }
        _ => peg_expr_to_source(expr),
    }
}

/// Wrap in parens when expr has lower precedence than a sequence item.
fn seq_item(expr: &PegExpr) -> String {
    match expr {
        PegExpr::Sequence(_) | PegExpr::Choice(_) => format!("({})", peg_expr_to_source(expr)),
        _ => peg_expr_to_source(expr),
    }
}

/// Wrap in parens when expr is a nested choice inside another choice.
fn choice_alt(expr: &PegExpr) -> String {
    match expr {
        PegExpr::Choice(_) => format!("({})", peg_expr_to_source(expr)),
        _ => peg_expr_to_source(expr),
    }
}

// ── RuleTextParser ────────────────────────────────────────────────────────────

/// Hand-written recursive-descent parser that converts a PEG expression string
/// into a `PegExpr` tree.
///
/// Moved from `parser.rs` so `grammar.rs` can call it without a circular dep.
pub(crate) struct RuleTextParser<'a> {
    src: &'a str,
    offset: usize,
}

impl<'a> RuleTextParser<'a> {
    pub fn parse(src: &'a str) -> Result<PegExpr, ParseError> {
        let mut parser = Self { src, offset: 0 };
        parser.skip_whitespace();
        let expr = parser.parse_choice()?;
        parser.skip_whitespace();
        if parser.offset < parser.src.len() {
            return Err(ParseError::new(
                "unexpected trailing tokens in grammar expression",
                parser.offset,
                parser.src.len(),
            ));
        }
        Ok(expr)
    }

    fn parse_choice(&mut self) -> Result<PegExpr, ParseError> {
        let first = self.parse_sequence()?;
        let mut alternatives = vec![first];
        loop {
            self.skip_whitespace();
            if self.peek() != Some('/') {
                break;
            }
            self.consume_char();
            self.skip_whitespace();
            let expr = self.parse_sequence()?;
            alternatives.push(expr);
        }
        if alternatives.len() == 1 {
            Ok(alternatives.pop().expect("one alternative"))
        } else {
            Ok(PegExpr::Choice(alternatives))
        }
    }

    fn parse_sequence(&mut self) -> Result<PegExpr, ParseError> {
        let mut items = Vec::new();
        while !self.eof() {
            self.skip_whitespace();
            // `)` ends a group; `,` is an argument separator;
            // `/` at non-start position starts the next choice alternative.
            if matches!(self.peek(), Some(')') | Some(','))
                || (!items.is_empty() && matches!(self.peek(), Some('/')))
            {
                break;
            }
            if self.eof() {
                break;
            }
            items.push(self.parse_repetition()?);
        }
        Ok(if items.len() == 1 {
            items.into_iter().next().expect("single sequence item")
        } else {
            PegExpr::Sequence(items)
        })
    }

    fn parse_repetition(&mut self) -> Result<PegExpr, ParseError> {
        let mut expr = self.parse_atom()?;
        loop {
            match self.peek() {
                Some('*') => {
                    self.consume_char();
                    expr = PegExpr::ZeroOrMore(Box::new(expr));
                }
                Some('+') => {
                    self.consume_char();
                    expr = PegExpr::OneOrMore(Box::new(expr));
                }
                Some('?') => {
                    self.consume_char();
                    expr = PegExpr::Optional(Box::new(expr));
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_atom(&mut self) -> Result<PegExpr, ParseError> {
        self.skip_whitespace();
        let Some(ch) = self.peek() else {
            return Ok(PegExpr::Sequence(vec![]));
        };
        match ch {
            '&' => {
                self.consume_char();
                Ok(PegExpr::And(Box::new(self.parse_atom()?)))
            }
            '!' => {
                self.consume_char();
                // `!!expr` → Eager; `!expr` → Not.
                if self.peek() == Some('!') {
                    self.consume_char();
                    Ok(PegExpr::Eager(Box::new(self.parse_atom()?)))
                } else {
                    Ok(PegExpr::Not(Box::new(self.parse_atom()?)))
                }
            }
            '~' => {
                self.consume_char();
                Ok(PegExpr::Cut)
            }
            '"' | '\'' => self.parse_string_literal(),
            '(' => {
                self.consume_char();
                let expr = self.parse_choice()?;
                self.skip_whitespace();
                if self.consume_char() != Some(')') {
                    return Err(ParseError::new(
                        "expected closing ')'",
                        self.offset,
                        self.offset,
                    ));
                }
                Ok(expr)
            }
            '.' => {
                self.consume_char();
                Ok(PegExpr::Dot)
            }
            '$' => {
                self.consume_char();
                let name = self.parse_ident()?;
                Ok(PegExpr::Parameter { name })
            }
            '[' => self.parse_regex_like(),
            '/' => self.parse_regex_like(),
            '@' => {
                self.consume_char();
                if self.peek() == Some('?') {
                    self.consume_char();
                    let name = self.parse_ident()?;
                    return Ok(PegExpr::SemanticPredicate { name });
                }
                let name = self.parse_ident()?;
                self.skip_whitespace();
                if self.peek() == Some('(') {
                    self.consume_char();
                    let inner = self.parse_choice()?;
                    self.skip_whitespace();
                    if self.consume_char() != Some(')') {
                        return Err(ParseError::new(
                            "expected ')' after @action expression",
                            self.offset,
                            self.offset,
                        ));
                    }
                    return Ok(PegExpr::SemanticAction {
                        name,
                        expr: Box::new(inner),
                    });
                }
                Ok(PegExpr::SemanticPredicate { name })
            }
            _ if Self::is_ident_start(ch) => {
                let ident = self.parse_ident()?;
                match ident.as_str() {
                    "newline" => return Ok(PegExpr::Newline),
                    "indent" => return Ok(PegExpr::Indent),
                    "dedent" => return Ok(PegExpr::Dedent),
                    _ => {}
                }
                self.skip_whitespace();
                if self.peek() == Some('(') {
                    match ident.as_str() {
                        "no_trivia" | "tight" => {
                            self.consume_char();
                            let inner = self.parse_choice()?;
                            self.skip_whitespace();
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' after no_trivia/tight expression",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::NoTrivia(Box::new(inner)));
                        }
                        "sep_plus" | "gather" => {
                            self.consume_char();
                            self.skip_whitespace();
                            let element = self.parse_choice()?;
                            self.skip_whitespace();
                            if self.peek() == Some(',') {
                                self.consume_char();
                            }
                            self.skip_whitespace();
                            let separator = self.parse_choice()?;
                            self.skip_whitespace();
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' after sep_plus/gather separator",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::SepOneOrMore {
                                element: Box::new(element),
                                separator: Box::new(separator),
                            });
                        }
                        "expected" => {
                            self.consume_char();
                            self.skip_whitespace();
                            if !matches!(self.peek(), Some('"') | Some('\'')) {
                                return Err(ParseError::new(
                                    "expected string literal as first argument to expected()",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            let msg_literal = self.parse_string_literal()?;
                            let msg = match msg_literal {
                                PegExpr::Literal(s) => s,
                                _ => unreachable!(),
                            };
                            self.skip_whitespace();
                            let inner = if self.peek() == Some(',') {
                                self.consume_char();
                                self.skip_whitespace();
                                self.parse_choice()?
                            } else {
                                PegExpr::Sequence(vec![])
                            };
                            self.skip_whitespace();
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' after expected() expression",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::Expected {
                                message: msg,
                                expr: Box::new(inner),
                            });
                        }
                        "capture" => {
                            self.consume_char();
                            self.skip_whitespace();
                            let label_lit = self.parse_string_literal()?;
                            let label = match label_lit {
                                PegExpr::Literal(s) => s,
                                _ => unreachable!(),
                            };
                            self.skip_whitespace();
                            if self.peek() == Some(',') {
                                self.consume_char();
                            }
                            self.skip_whitespace();
                            let inner = self.parse_choice()?;
                            self.skip_whitespace();
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' after capture() expression",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::Capture {
                                label,
                                expr: Box::new(inner),
                            });
                        }
                        "island" => {
                            self.consume_char();
                            self.skip_whitespace();
                            let start = self.parse_string_literal_value()?;
                            self.skip_whitespace();
                            if self.peek() == Some(',') {
                                self.consume_char();
                            }
                            self.skip_whitespace();
                            let end = self.parse_string_literal_value()?;
                            self.skip_whitespace();
                            let include_delims = if self.peek() == Some(',') {
                                self.consume_char();
                                self.skip_whitespace();
                                let kw = self.parse_ident()?;
                                kw == "true"
                            } else {
                                false
                            };
                            self.skip_whitespace();
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' after island() expression",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::Island {
                                start,
                                end,
                                include_delims,
                            });
                        }
                        "raw_block" => {
                            self.consume_char();
                            self.skip_whitespace();
                            let start = self.parse_string_literal_value()?;
                            self.skip_whitespace();
                            if self.peek() == Some(',') {
                                self.consume_char();
                            }
                            self.skip_whitespace();
                            let end = self.parse_string_literal_value()?;
                            self.skip_whitespace();
                            let delim_kind = if self.peek() == Some(',') {
                                self.consume_char();
                                self.skip_whitespace();
                                self.parse_string_literal_value()?
                            } else {
                                "block".to_string()
                            };
                            self.skip_whitespace();
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' after raw_block() expression",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::RawBlock {
                                start,
                                end,
                                delim_kind,
                            });
                        }
                        "eager" => {
                            self.consume_char();
                            let inner = self.parse_choice()?;
                            self.skip_whitespace();
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' after eager() expression",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::Eager(Box::new(inner)));
                        }
                        "tok" | "token_ref" => {
                            self.consume_char();
                            self.skip_whitespace();
                            let kind: Option<String> = if self
                                .peek()
                                .map(|c| c.is_alphanumeric() || c == '_')
                                .unwrap_or(false)
                            {
                                Some(self.parse_ident()?)
                            } else if self.peek() == Some('"') || self.peek() == Some('\'') {
                                Some(self.parse_string_literal_value()?)
                            } else {
                                None
                            };
                            self.skip_whitespace();
                            let text: Option<String> = if self.peek() == Some(',') {
                                self.consume_char();
                                self.skip_whitespace();
                                Some(self.parse_string_literal_value()?)
                            } else {
                                None
                            };
                            self.skip_whitespace();
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' after tok() expression",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::TokenRef { kind, text });
                        }
                        "kw" | "hard_keyword" => {
                            self.consume_char();
                            self.skip_whitespace();
                            let word = self.parse_string_literal_value()?;
                            self.skip_whitespace();
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' after kw() expression",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::HardKeyword(word));
                        }
                        "soft_keyword" => {
                            self.consume_char();
                            self.skip_whitespace();
                            let word = self.parse_string_literal_value()?;
                            self.skip_whitespace();
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' after soft_keyword() expression",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::SoftKeyword(word));
                        }
                        "scope" | "grammar_scope" => {
                            self.consume_char();
                            self.skip_whitespace();
                            let grammar_name = self.parse_string_literal_value()?;
                            self.skip_whitespace();
                            if self.peek() == Some(',') {
                                self.consume_char();
                            }
                            self.skip_whitespace();
                            let inner = self.parse_choice()?;
                            self.skip_whitespace();
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' after scope() expression",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::GrammarScope {
                                grammar_name,
                                expr: Box::new(inner),
                            });
                        }
                        _ => {
                            self.consume_char();
                            let mut args = Vec::new();
                            self.skip_whitespace();
                            if self.peek() != Some(')') {
                                args.push(self.parse_choice()?);
                                loop {
                                    self.skip_whitespace();
                                    if self.peek() != Some(',') {
                                        break;
                                    }
                                    self.consume_char();
                                    self.skip_whitespace();
                                    args.push(self.parse_choice()?);
                                }
                            }
                            self.skip_whitespace();
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' in rule call",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::Call { rule: ident, args });
                        }
                    }
                }
                // `grammar_name::rule_name` — ImportedRef
                if self.peek() == Some(':') {
                    let after_colon = self.src[self.offset + 1..].chars().next();
                    if after_colon == Some(':') {
                        self.consume_char();
                        self.consume_char();
                        self.skip_whitespace();
                        let rule_name = self.parse_ident()?;
                        return Ok(PegExpr::ImportedRef {
                            grammar_name: ident,
                            rule_name,
                        });
                    }
                }
                // Named binding: `name:inner_atom_with_repetition`
                if self.peek() == Some(':') {
                    self.consume_char();
                    self.skip_whitespace();
                    let inner = self.parse_repetition()?;
                    return Ok(PegExpr::Named {
                        name: ident,
                        expr: Box::new(inner),
                    });
                }
                Ok(PegExpr::Ref(ident))
            }
            _ => Err(ParseError::new(
                format!("unexpected token '{ch}'"),
                self.offset,
                self.offset + ch.len_utf8(),
            )),
        }
    }

    fn parse_string_literal_value(&mut self) -> Result<String, ParseError> {
        match self.parse_string_literal()? {
            PegExpr::Literal(s) => Ok(s),
            _ => unreachable!(),
        }
    }

    fn parse_string_literal(&mut self) -> Result<PegExpr, ParseError> {
        let quote = self
            .consume_char()
            .expect("string literal starts with delimiter");
        let mut value = String::new();
        while let Some(ch) = self.peek() {
            self.consume_char();
            if ch == quote {
                return Ok(PegExpr::Literal(value));
            }
            if ch == '\\' {
                let escaped = match self.consume_char() {
                    Some('n') => '\n',
                    Some('r') => '\r',
                    Some('t') => '\t',
                    Some('\\') => '\\',
                    Some('0') => '\0',
                    Some('\'') => '\'',
                    Some('"') => '"',
                    Some(other) => other,
                    None => {
                        return Err(ParseError::new(
                            "unterminated escape sequence",
                            self.offset,
                            self.offset,
                        ));
                    }
                };
                value.push(escaped);
            } else {
                value.push(ch);
            }
        }
        Err(ParseError::new(
            "unterminated string literal",
            self.offset,
            self.src.len(),
        ))
    }

    fn parse_ident(&mut self) -> Result<String, ParseError> {
        let start = self.offset;
        if !self.peek().map(Self::is_ident_start).unwrap_or(false) {
            return Err(ParseError::new(
                "expected identifier",
                self.offset,
                self.offset,
            ));
        }
        let mut name = String::new();
        while let Some(ch) = self.peek() {
            if Self::is_ident_continue(ch) {
                self.consume_char();
                name.push(ch);
            } else {
                break;
            }
        }
        if start == self.offset {
            Err(ParseError::new(
                "expected identifier",
                start,
                start.saturating_add(1),
            ))
        } else {
            Ok(name)
        }
    }

    fn parse_regex_like(&mut self) -> Result<PegExpr, ParseError> {
        let start = self.offset;
        let pattern = if self.peek() == Some('/') {
            let _ = self.consume_char();
            let mut inner = String::new();
            let mut terminated = false;
            while let Some(ch) = self.consume_char() {
                if ch == '\\' {
                    if let Some(escaped) = self.consume_char() {
                        inner.push('\\');
                        inner.push(escaped);
                    } else {
                        return Err(ParseError::new(
                            "unterminated regex escape",
                            self.offset,
                            self.offset,
                        ));
                    }
                    continue;
                }
                if ch == '/' {
                    terminated = true;
                    break;
                }
                inner.push(ch);
            }
            if !terminated {
                return Err(ParseError::new(
                    "unterminated regex literal",
                    start,
                    self.src.len(),
                ));
            }
            inner
        } else {
            let mut inner = String::new();
            let mut terminated = false;
            self.consume_char(); // consume '['
            while let Some(ch) = self.consume_char() {
                if ch == '\\' {
                    if let Some(escaped) = self.consume_char() {
                        inner.push('\\');
                        inner.push(escaped);
                    } else {
                        return Err(ParseError::new(
                            "unterminated character class escape",
                            self.offset,
                            self.offset,
                        ));
                    }
                    continue;
                }
                if ch == ']' {
                    terminated = true;
                    break;
                }
                inner.push(ch);
            }
            if !terminated {
                return Err(ParseError::new(
                    "unterminated character class",
                    start,
                    self.src.len(),
                ));
            }
            format!("[{inner}]")
        };

        let compiled = CompiledRegex::new(pattern, start, self.offset)?;
        Ok(PegExpr::Regex(compiled))
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.consume_char();
                continue;
            }
            if ch != '#' {
                break;
            }
            while let Some(inner) = self.consume_char() {
                if inner == '\n' {
                    break;
                }
            }
        }
    }

    fn is_ident_start(ch: char) -> bool {
        ch.is_ascii_alphabetic() || ch == '_'
    }

    fn is_ident_continue(ch: char) -> bool {
        Self::is_ident_start(ch) || ch.is_ascii_digit()
    }

    fn peek(&self) -> Option<char> {
        self.src[self.offset..].chars().next()
    }

    fn consume_char(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.offset += ch.len_utf8();
        Some(ch)
    }

    fn eof(&self) -> bool {
        self.offset >= self.src.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_regex_eq_by_pattern() {
        let a = CompiledRegex::new("[a-z]+".to_string(), 0, 0).unwrap();
        let b = CompiledRegex::new("[a-z]+".to_string(), 0, 0).unwrap();
        let c = CompiledRegex::new("[A-Z]+".to_string(), 0, 0).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn roundtrip_literal() {
        let expr = RuleTextParser::parse("\"hello\"").unwrap();
        assert_eq!(expr, PegExpr::Literal("hello".to_string()));
        assert_eq!(peg_expr_to_source(&expr), "\"hello\"");
    }

    #[test]
    fn roundtrip_choice() {
        let expr = RuleTextParser::parse("[a-z]+ / [0-9]+").unwrap();
        let src = peg_expr_to_source(&expr);
        let re_parsed = RuleTextParser::parse(&src).unwrap();
        assert_eq!(expr, re_parsed);
    }

    #[test]
    fn invalid_regex_returns_error() {
        let err = RuleTextParser::parse("/[unclosed/");
        assert!(err.is_err());
    }
}
