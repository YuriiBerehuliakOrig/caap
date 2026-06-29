//! Programmatic grammar construction — [`GrammarBuilder`] and expression constructors.
//!
//! Build [`PegExpr`] trees and assemble them into a [`Grammar`] without writing
//! PEG text.  Useful for grammars generated from data, test fixtures, or
//! programmatic transformations.
//!
//! # Example
//! ```
//! use caap_peg::builder::{self, GrammarBuilder};
//!
//! let grammar = GrammarBuilder::new()
//!     .start("expr")
//!     .rule("expr", builder::choice(vec![
//!         builder::rule_ref("number"),
//!         builder::rule_ref("ident"),
//!     ]))
//!     .rule("number", builder::plus(builder::char_class("0-9").unwrap()))
//!     .rule("ident",  builder::plus(builder::char_class("a-zA-Z_").unwrap()))
//!     .build();
//! assert_eq!(grammar.start_rule, "expr");
//! assert_eq!(grammar.rule_count(), 3);
//! ```

use crate::error::ParseError;
use crate::expr::{CompiledRegex, PegExpr};
use crate::grammar::{rules_to_text, validate_import_alias, Grammar, GrammarRule, GrammarState};
use std::collections::HashMap;

// ── Terminal constructors ─────────────────────────────────────────────────────

/// Match a literal string exactly.
pub fn lit(s: impl Into<String>) -> PegExpr {
    PegExpr::Literal(s.into())
}

/// Match any single character.
pub fn dot() -> PegExpr {
    PegExpr::Dot
}

/// Match a character class.
///
/// Supply the class body without brackets: `char_class("a-z")` matches `[a-z]`.
/// Brackets are accepted too: `char_class("[a-z]")` is identical.
pub fn char_class(cls: impl Into<String>) -> Result<PegExpr, ParseError> {
    let inner = cls.into();
    let pattern = if inner.starts_with('[') && inner.ends_with(']') {
        inner
    } else {
        format!("[{inner}]")
    };
    CompiledRegex::new(pattern, 0, 0).map(PegExpr::Regex)
}

/// Match an arbitrary regex pattern (no surrounding delimiters needed).
pub fn regex(pattern: impl Into<String>) -> Result<PegExpr, ParseError> {
    CompiledRegex::new(pattern.into(), 0, 0).map(PegExpr::Regex)
}

// ── Rule reference ────────────────────────────────────────────────────────────

/// Reference another rule by name.
pub fn rule_ref(name: impl Into<String>) -> PegExpr {
    PegExpr::Ref(name.into())
}

// ── Structural combinators ────────────────────────────────────────────────────

/// Match all expressions in sequence.
pub fn seq(exprs: Vec<PegExpr>) -> PegExpr {
    PegExpr::Sequence(exprs)
}

/// Ordered choice: try alternatives left-to-right, return the first that matches.
pub fn choice(exprs: Vec<PegExpr>) -> PegExpr {
    PegExpr::Choice(exprs)
}

/// Match `e` zero or one times.
pub fn opt(e: PegExpr) -> PegExpr {
    PegExpr::Optional(Box::new(e))
}

/// Match `e` zero or more times.
pub fn star(e: PegExpr) -> PegExpr {
    PegExpr::ZeroOrMore(Box::new(e))
}

/// Match `e` one or more times.
pub fn plus(e: PegExpr) -> PegExpr {
    PegExpr::OneOrMore(Box::new(e))
}

/// Positive lookahead — succeeds if `e` matches, consumes nothing.
pub fn and(e: PegExpr) -> PegExpr {
    PegExpr::And(Box::new(e))
}

/// Negative lookahead — succeeds if `e` does not match, consumes nothing.
pub fn not(e: PegExpr) -> PegExpr {
    PegExpr::Not(Box::new(e))
}

/// Committed cut — failure after this point does not backtrack out of the
/// enclosing choice alternative.
pub fn cut() -> PegExpr {
    PegExpr::Cut
}

/// Eager (possessive) match — like `e` but suppresses internal backtracking.
pub fn eager(e: PegExpr) -> PegExpr {
    PegExpr::Eager(Box::new(e))
}

/// `element (separator element)*` — one or more elements separated by `sep`.
pub fn sep_plus(element: PegExpr, separator: PegExpr) -> PegExpr {
    PegExpr::SepOneOrMore {
        element: Box::new(element),
        separator: Box::new(separator),
    }
}

/// `element (separator element)*` — like `sep_plus` but keeps separator values.
///
/// Output: `Node("interspersed", [elem1, sep1, elem2, sep2, ...])`
pub fn interspersed(element: PegExpr, separator: PegExpr) -> PegExpr {
    PegExpr::Interspersed {
        element: Box::new(element),
        separator: Box::new(separator),
    }
}

// ── Value / annotation ────────────────────────────────────────────────────────

/// Bind the matched text to `name` in the parse result.
pub fn named(name: impl Into<String>, e: PegExpr) -> PegExpr {
    PegExpr::Named {
        name: name.into(),
        expr: Box::new(e),
    }
}

/// Override the expected-label in error messages when `e` fails.
pub fn expected(msg: impl Into<String>, e: PegExpr) -> PegExpr {
    PegExpr::Expected {
        message: msg.into(),
        expr: Box::new(e),
    }
}

/// Capture the matched span under `label`.
pub fn capture(label: impl Into<String>, e: PegExpr) -> PegExpr {
    PegExpr::Capture {
        label: label.into(),
        expr: Box::new(e),
    }
}

// ── Trivia / layout ───────────────────────────────────────────────────────────

/// Disable automatic whitespace/comment skipping inside `e`.
pub fn no_trivia(e: PegExpr) -> PegExpr {
    PegExpr::NoTrivia(Box::new(e))
}

/// Match a newline in layout-sensitive mode.
pub fn newline() -> PegExpr {
    PegExpr::Newline
}

/// Match an indent token in layout-sensitive mode.
pub fn indent() -> PegExpr {
    PegExpr::Indent
}

/// Match a dedent token in layout-sensitive mode.
pub fn dedent() -> PegExpr {
    PegExpr::Dedent
}

// ── Keywords ──────────────────────────────────────────────────────────────────

/// Match `s` as a hard keyword (must not be followed by identifier chars).
pub fn keyword(s: impl Into<String>) -> PegExpr {
    PegExpr::HardKeyword(s.into())
}

/// Match `s` as a soft keyword (context-sensitive; no identifier boundary check).
pub fn soft_keyword(s: impl Into<String>) -> PegExpr {
    PegExpr::SoftKeyword(s.into())
}

// ── Semantic hooks ────────────────────────────────────────────────────────────

/// Attach a named semantic action to `e`.
pub fn semantic_action(name: impl Into<String>, e: PegExpr) -> PegExpr {
    PegExpr::SemanticAction {
        name: name.into(),
        expr: Box::new(e),
    }
}

/// A zero-width semantic predicate: succeeds or fails based on runtime logic.
pub fn semantic_predicate(name: impl Into<String>) -> PegExpr {
    PegExpr::SemanticPredicate { name: name.into() }
}

/// A semantic guard `@!name(e)`: matches `e`, then lets the host driver accept,
/// reject (backtrack), commit, or fail the match.
pub fn semantic_guard(name: impl Into<String>, e: PegExpr) -> PegExpr {
    PegExpr::SemanticGuard {
        name: name.into(),
        expr: Box::new(e),
    }
}

fn prec_level(fixity: crate::expr::Fixity, operators: Vec<PegExpr>) -> crate::expr::PrecLevel {
    crate::expr::PrecLevel { fixity, operators }
}

/// A left-associative infix precedence level.
pub fn infixl(operators: Vec<PegExpr>) -> crate::expr::PrecLevel {
    prec_level(crate::expr::Fixity::InfixLeft, operators)
}

/// A right-associative infix precedence level.
pub fn infixr(operators: Vec<PegExpr>) -> crate::expr::PrecLevel {
    prec_level(crate::expr::Fixity::InfixRight, operators)
}

/// A non-associative infix precedence level (`a == b == c` is an error).
pub fn infixn(operators: Vec<PegExpr>) -> crate::expr::PrecLevel {
    prec_level(crate::expr::Fixity::InfixNon, operators)
}

/// A ternary / mixfix level `open … close …` (e.g. `?` / `:`).
pub fn ternary(open: PegExpr, close: PegExpr) -> crate::expr::PrecLevel {
    prec_level(crate::expr::Fixity::Ternary, vec![open, close])
}

/// A prefix unary operator level.
pub fn prefix(operators: Vec<PegExpr>) -> crate::expr::PrecLevel {
    prec_level(crate::expr::Fixity::Prefix, operators)
}

/// A postfix unary operator level.
pub fn postfix(operators: Vec<PegExpr>) -> crate::expr::PrecLevel {
    prec_level(crate::expr::Fixity::Postfix, operators)
}

/// An operator-precedence expression over `operand` with infix `levels`
/// (lowest precedence first). See [`PegExpr::Precedence`].
pub fn precedence(operand: PegExpr, levels: Vec<crate::expr::PrecLevel>) -> PegExpr {
    PegExpr::Precedence {
        operand: Box::new(operand),
        levels,
    }
}

// ── Delimiter-bounded ─────────────────────────────────────────────────────────

/// Match content bounded by `start`/`end` delimiters (island grammar).
pub fn island(start: impl Into<String>, end: impl Into<String>, include_delims: bool) -> PegExpr {
    PegExpr::Island {
        start: start.into(),
        end: end.into(),
        include_delims,
    }
}

/// Match a raw block delimited by `start`/`end` with a given delimiter kind.
pub fn raw_block(
    start: impl Into<String>,
    end: impl Into<String>,
    delim_kind: impl Into<String>,
) -> PegExpr {
    PegExpr::RawBlock {
        start: start.into(),
        end: end.into(),
        delim_kind: delim_kind.into(),
    }
}

// ── Cross-grammar / parametric ────────────────────────────────────────────────

/// Reference a rule in an imported grammar by alias and rule name.
pub fn imported_ref(grammar_name: impl Into<String>, rule_name: impl Into<String>) -> PegExpr {
    PegExpr::ImportedRef {
        grammar_name: grammar_name.into(),
        rule_name: rule_name.into(),
    }
}

/// Reference a parameter in a parametric rule.
pub fn param(name: impl Into<String>) -> PegExpr {
    PegExpr::Parameter { name: name.into() }
}

/// Call a parametric rule with the given arguments.
pub fn call(rule: impl Into<String>, args: Vec<PegExpr>) -> PegExpr {
    PegExpr::Call {
        rule: rule.into(),
        args,
    }
}

/// Wrap `e` in a named grammar scope for cross-grammar disambiguation.
pub fn grammar_scope(grammar_name: impl Into<String>, e: PegExpr) -> PegExpr {
    PegExpr::GrammarScope {
        grammar_name: grammar_name.into(),
        expr: Box::new(e),
    }
}

/// Match a token-stream token by optional kind and/or text.
pub fn token_ref(kind: Option<String>, text: Option<String>) -> PegExpr {
    PegExpr::TokenRef { kind, text }
}

// ── GrammarBuilder ────────────────────────────────────────────────────────────

/// Assembles a [`Grammar`] from programmatically constructed [`PegExpr`] trees.
///
/// Rules are appended in the order they are added.  The first rule added
/// becomes the default start rule unless [`GrammarBuilder::start`] is called
/// explicitly.
///
/// # Example
/// ```
/// use caap_peg::builder::{self, GrammarBuilder};
///
/// let grammar = GrammarBuilder::new()
///     .start("root")
///     .rule("root", builder::star(builder::rule_ref("item")))
///     .rule("item", builder::lit("x"))
///     .build();
/// assert_eq!(grammar.start_rule, "root");
/// assert_eq!(grammar.rule_count(), 2);
/// ```
pub struct GrammarBuilder {
    start_rule: Option<String>,
    rules: Vec<GrammarRule>,
    imports: HashMap<String, Box<Grammar>>,
}

impl Default for GrammarBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl GrammarBuilder {
    /// An empty driver builder.
    pub fn new() -> Self {
        Self {
            start_rule: None,
            rules: Vec::new(),
            imports: HashMap::new(),
        }
    }

    /// Set the start rule name.  Defaults to the first rule added, or `"root"`.
    pub fn start(mut self, rule: impl Into<String>) -> Self {
        self.start_rule = Some(rule.into());
        self
    }

    /// Add a non-parametric rule built from a `PegExpr`.
    pub fn rule(mut self, name: impl Into<String>, expr: PegExpr) -> Self {
        self.rules.push(GrammarRule::from_expr(name, expr, vec![]));
        self
    }

    /// Add a parametric rule (e.g. `list(sep) <- item (sep item)*`).
    pub fn parametric(
        mut self,
        name: impl Into<String>,
        params: Vec<String>,
        expr: PegExpr,
    ) -> Self {
        self.rules.push(GrammarRule::from_expr(name, expr, params));
        self
    }

    /// Register an imported grammar under `alias` for cross-grammar references.
    pub fn import(mut self, alias: impl Into<String>, grammar: Grammar) -> Self {
        let alias = validate_import_alias(alias)
            .expect("trusted grammar builder import alias must be non-empty");
        self.imports.insert(alias, Box::new(grammar));
        self
    }

    /// Consume the builder and produce a [`Grammar`].
    pub fn build(self) -> Grammar {
        let start_rule = self
            .start_rule
            .or_else(|| self.rules.first().map(|r| r.name.clone()))
            .unwrap_or_else(|| "root".to_string());
        let text = rules_to_text(&self.rules);
        Grammar {
            start_rule,
            text,
            rules: self.rules,
            metadata: HashMap::new(),
            imports: self.imports,
            version: 1,
            state: GrammarState {
                sealed: false,
                analysis_state: None,
                version: 0,
            },
            compiled: Default::default(),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: tests that build a grammar and then *parse* with it live in
    // `tests/grammar_builder.rs` (an end-to-end scenario). These unit tests
    // cover only the builder's own output structure, in isolation.

    #[test]
    fn default_start_rule_is_first_added() {
        let grammar = GrammarBuilder::new()
            .rule("first", dot())
            .rule("second", dot())
            .build();
        assert_eq!(grammar.start_rule, "first");
    }

    #[test]
    fn parametric_rule_round_trips() {
        let grammar = GrammarBuilder::new()
            .start("list")
            .parametric(
                "list",
                vec!["sep".to_string()],
                sep_plus(rule_ref("item"), param("sep")),
            )
            .rule("item", plus(char_class("a-z").unwrap()))
            .build();
        assert_eq!(grammar.rules[0].params, vec!["sep"]);
        assert_eq!(grammar.rules[0].name, "list");
    }

    #[test]
    fn from_expr_source_round_trips() {
        let expr = plus(char_class("a-z").unwrap());
        let rule = GrammarRule::from_expr("word", expr.clone(), vec![]);
        assert_eq!(rule.name, "word");
        assert_eq!(rule.expr(), &expr);
        assert!(!rule.source.is_empty());
    }
}
