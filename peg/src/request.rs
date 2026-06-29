//! `ParseRequest` — the single fluent entry point for parsing.
//!
//! Configure the parse on the builder (spans, a [`ParseDriver`], a token stream
//! or a [`Scanner`], a [`GrammarRegistry`], the start rule, output mode), then
//! pick a terminal for the result shape you want:
//!
//! | terminal | returns |
//! |----------|---------|
//! | [`run`](ParseRequest::run) | [`ParseValue`] |
//! | [`run_output`](ParseRequest::run_output) | [`ParseOutput`] (value, or an AST when [`ast`](ParseRequest::ast) is set) |
//! | [`run_profiled`](ParseRequest::run_profiled) | `(ParseValue, ParseProfile)` |
//! | [`run_prefix`](ParseRequest::run_prefix) | [`CompletedPrefixParse`] (parse a leading slice) |
//! | [`run_incremental`](ParseRequest::run_incremental) | `Arc<ParseValue>`, reusing a [`ParseCache`] across edits |

use std::borrow::Cow;
use std::sync::Arc;

use crate::driver::ParseDriver;
use crate::error::ParseError;
use crate::grammar::Grammar;
use crate::parser_engine::{PEGParser, ParseOutput};
use crate::parser_imports::hydrate_imports_from_registry;
use crate::profile::ParseProfile;
use crate::registry::GrammarRegistry;
use crate::scanner::Scanner;
use crate::types::{
    CompletedPrefixParse, LexToken, ParseCache, ParseValue, ParserConfig, ParserOutputMode,
};

/// Builder for a single parse over `grammar`.
///
/// ```
/// use caap_peg::{Grammar, ParseRequest};
///
/// let grammar = Grammar::trusted_new("root <- 'hi'").with_start_rule("root");
/// let value = ParseRequest::new(&grammar).spans().run("hi").unwrap();
/// assert!(value.is_spanned());
/// ```
pub struct ParseRequest<'a> {
    grammar: &'a Grammar,
    config: ParserConfig,
    driver: Option<&'a dyn ParseDriver>,
    tokens: Option<Vec<LexToken>>,
    scanner: Option<&'a Scanner>,
    registry: Option<&'a GrammarRegistry>,
    start_rule: Option<&'a str>,
}

impl<'a> ParseRequest<'a> {
    /// Start a request against `grammar` with default configuration.
    pub fn new(grammar: &'a Grammar) -> Self {
        Self {
            grammar,
            config: ParserConfig::default(),
            driver: None,
            tokens: None,
            scanner: None,
            registry: None,
            start_rule: None,
        }
    }

    /// Replace the parser configuration wholesale.
    ///
    /// `spans`/`return_spans` applied afterwards still override `config.return_spans`.
    pub fn config(mut self, config: ParserConfig) -> Self {
        self.config = config;
        self
    }

    /// Wrap the root result in a `SpannedValue`.
    pub fn spans(mut self) -> Self {
        self.config.return_spans = true;
        self
    }

    /// Set whether the root result is wrapped in a `SpannedValue`.
    pub fn return_spans(mut self, return_spans: bool) -> Self {
        self.config.return_spans = return_spans;
        self
    }

    /// Attach a [`ParseDriver`] — the single host control surface (Parse Effects
    /// Protocol) backing every semantic hook (`@action`/`@?pred`/`@!guard`) and
    /// global control. The driver can transform/reject/steer rule choices, keep
    /// transactional state across backtracking, run scoped sub-parses, and
    /// rewrite the final failure diagnostic. Works on both the scannerless and
    /// the token-stream paths.
    pub fn driver(mut self, driver: &'a dyn ParseDriver) -> Self {
        self.driver = Some(driver);
        self
    }

    /// Parse against a pre-produced token stream, enabling `tok(...)` expressions.
    /// Takes precedence over an attached [`scan`](Self::scan)ner.
    pub fn tokens(mut self, tokens: Vec<LexToken>) -> Self {
        self.tokens = Some(tokens);
        self
    }

    /// Tokenise the input with the built-in [`Scanner`] before parsing, enabling
    /// `tok(...)` expressions without an external lexer. The scanner runs at the
    /// terminal; a scan failure surfaces as the parse error. An explicit
    /// [`tokens`](Self::tokens) stream takes precedence over a scanner.
    pub fn scan(mut self, scanner: &'a Scanner) -> Self {
        self.scanner = Some(scanner);
        self
    }

    /// Resolve missing `ImportedRef`/`GrammarScope` aliases from a registry.
    /// Inline imports already attached to the grammar take precedence.
    pub fn registry(mut self, registry: &'a GrammarRegistry) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Parse a different start rule than the grammar's default. Used by
    /// [`run_prefix`](Self::run_prefix).
    pub fn start_rule(mut self, rule: &'a str) -> Self {
        self.start_rule = Some(rule);
        self
    }

    /// Emit an [`AstNode`](crate::ast::AstNode) tree from
    /// [`run_output`](Self::run_output) instead of a `ParseValue`.
    pub fn ast(mut self) -> Self {
        self.config.output_mode = ParserOutputMode::Ast;
        self
    }

    // ── Terminals ──────────────────────────────────────────────────────────

    /// Run the parse over `text`, returning a [`ParseValue`].
    pub fn run(self, text: &str) -> Result<ParseValue, ParseError> {
        let grammar = resolve_grammar(self.grammar, self.registry)?;
        let tokens = resolve_tokens(self.tokens, self.scanner, text)?;
        let parser = PEGParser;
        match tokens {
            Some(tokens) => parser.parse_with_lex_tokens_and_driver(
                &grammar,
                text,
                &self.config,
                Arc::new(tokens),
                self.driver,
            ),
            None => parser.parse_with_driver(&grammar, text, &self.config, self.driver),
        }
    }

    /// Run the parse, returning a [`ParseOutput`] — a `ParseValue`, or an
    /// [`AstNode`](crate::ast::AstNode) when [`ast`](Self::ast) is set. The AST
    /// path is scannerless (it ignores `tokens`/`scan`/`driver`).
    pub fn run_output(self, text: &str) -> Result<ParseOutput, ParseError> {
        match self.config.output_mode {
            ParserOutputMode::Value => self.run(text).map(ParseOutput::Value),
            ParserOutputMode::Ast => {
                let grammar = resolve_grammar(self.grammar, self.registry)?;
                crate::ast::parse_ast_with_max_steps(
                    &grammar,
                    text,
                    self.start_rule,
                    Some(self.config.max_steps),
                )
                .map(ParseOutput::Ast)
            }
        }
    }

    /// Run the parse and return a per-rule [`ParseProfile`] alongside the value —
    /// call counts, memo/seed hit rates, and the hottest rules. Profiling is off
    /// on every other terminal, so it adds no overhead there.
    pub fn run_profiled(self, text: &str) -> Result<(ParseValue, ParseProfile), ParseError> {
        let grammar = resolve_grammar(self.grammar, self.registry)?;
        let tokens = resolve_tokens(self.tokens, self.scanner, text)?.map(Arc::new);
        PEGParser
            .run_full_parse(&grammar, text, &self.config, self.driver, tokens, true)
            .map(|(value, profile)| (value, profile.unwrap_or_default()))
    }

    /// Parse only a leading slice starting at `start_pos`, returning a
    /// [`CompletedPrefixParse`] (the value, bytes consumed, and whether EOF was
    /// reached) instead of requiring the whole input to match.
    pub fn run_prefix(self, text: &str, start_pos: usize) -> CompletedPrefixParse {
        let grammar = match resolve_grammar(self.grammar, self.registry) {
            Ok(grammar) => grammar,
            Err(err) => return CompletedPrefixParse::failed(err.message.to_string()),
        };
        PEGParser.parse_prefix(&grammar, text, start_pos, self.start_rule, &self.config)
    }

    /// Parse `text`, reusing unchanged subtrees from `cache` across edits, and
    /// returning a shared `Arc<ParseValue>`. Seed the same cache on each edit for
    /// sound incremental reuse.
    pub fn run_incremental(
        self,
        text: &str,
        cache: &mut ParseCache,
    ) -> Result<Arc<ParseValue>, ParseError> {
        let grammar = resolve_grammar(self.grammar, self.registry)?;
        PEGParser.parse_incremental_many(&grammar, text, &self.config, cache)
    }
}

/// Hydrate the grammar's imports from a registry when one is attached.
fn resolve_grammar<'g>(
    grammar: &'g Grammar,
    registry: Option<&GrammarRegistry>,
) -> Result<Cow<'g, Grammar>, ParseError> {
    match registry {
        Some(registry) => Ok(Cow::Owned(hydrate_imports_from_registry(
            grammar, registry,
        )?)),
        None => Ok(Cow::Borrowed(grammar)),
    }
}

/// Pick the token stream: an explicit one wins, else the scanner produces it,
/// else there is none (scannerless parse).
fn resolve_tokens(
    explicit: Option<Vec<LexToken>>,
    scanner: Option<&Scanner>,
    text: &str,
) -> Result<Option<Vec<LexToken>>, ParseError> {
    match explicit {
        Some(tokens) => Ok(Some(tokens)),
        None => match scanner {
            Some(scanner) => scanner.scan(text).map(Some),
            None => Ok(None),
        },
    }
}
