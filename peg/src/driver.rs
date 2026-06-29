//! Parse Effects Protocol (PEP) — a host-drivable control surface for the parser.
//!
//! The engine stays semantics-free: at each genuine decision point it raises a
//! typed [`ParseEffect`] and consumes a typed [`Directive`] returned by a
//! host-supplied [`ParseDriver`]. When no driver is attached the engine applies
//! its built-in policy ([`Directive::Proceed`] everywhere), which reproduces the
//! ordinary PEG behaviour exactly — so the mechanism is zero-cost when unused.
//!
//! This is the "mechanism vs policy" split: the engine owns *where* decisions
//! happen and provides neutral defaults; the host owns *what* the decision is.
//! All meaning (symbol tables, scopes, type environments) lives in the driver,
//! never in the parser.
//!
//! Four pieces:
//! - [`ParseEffect`] — *what the engine asks* (the decision point + payload).
//! - [`Directive`] — *what the engine should do* (a closed, total algebra, every
//!   variant expressible in the engine's existing operational semantics).
//! - [`ParseView`] — *what the host can latch onto* (read access to live parser
//!   state plus a scoped, isolated [`ParseView::sub_parse`]).
//! - [`ParseDriver`] — the host trait. `handle` answers effects; the
//!   `checkpoint`/`rollback`/`commit` trio keeps host state consistent across PEG
//!   backtracking; `memo_facet` keeps packrat memoisation sound.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::types::{ParseValue, ParserConfig, ParserOutputMode};

// ── GrammarScalar ───────────────────────────────────────────────────────────

/// Primitive scalar passed as an argument to a semantic hook (`@action(…)`,
/// `@?pred(…)`) — the value type carried by [`ParseEffect`] args and exposed on
/// [`ParseView::args`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GrammarScalar {
    /// The null scalar.
    Null,
    /// A boolean.
    Bool(bool),
    /// A 64-bit signed integer.
    Int(i64),
    /// A 64-bit float.
    Float(f64),
    /// A string.
    Str(String),
}

impl Eq for GrammarScalar {}

impl GrammarScalar {
    /// Whether this is [`Null`](GrammarScalar::Null).
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

impl std::fmt::Display for GrammarScalar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Null => write!(f, "null"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int(n) => write!(f, "{n}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Str(s) => write!(f, "{s}"),
        }
    }
}

// ── Effects: where a decision is made ───────────────────────────────────────

/// A decision point raised by the engine. Plain borrowed data — easy to match,
/// mock, and test.
#[derive(Debug)]
pub enum ParseEffect<'a> {
    /// A rule is about to be evaluated.
    RuleEnter {
        /// The rule name.
        rule: &'a str,
        /// Byte position the rule starts at.
        pos: usize,
    },
    /// A rule matched syntactically; the host may transform, reject, or fail it.
    RuleExit {
        /// The rule name.
        rule: &'a str,
        /// Byte position the rule started at.
        pos: usize,
        /// Byte position the match ended at (exclusive).
        end: usize,
        /// The matched value.
        value: &'a ParseValue,
    },
    /// A rule failed to match at `pos` (observation; fired on every failed rule
    /// body, including backtracked attempts). The AST builder consumes these.
    RuleFail {
        /// The rule name.
        rule: &'a str,
        /// Byte position the failed attempt started at.
        pos: usize,
    },
    /// An ordered choice is about to try its alternatives. The host may
    /// [`Directive::Restrict`] the candidate set / order.
    ChoiceEnter {
        /// The enclosing rule, if the choice is a rule body.
        rule: Option<&'a str>,
        /// Number of alternatives.
        alt_count: usize,
        /// Byte position the choice starts at.
        pos: usize,
    },
    /// An alternative matched syntactically. This is the canonical
    /// "did it match *semantically*?" hook: returning [`Directive::Reject`]
    /// discards the match and the choice continues with the next alternative.
    AltMatched {
        /// The enclosing rule, if any.
        rule: Option<&'a str>,
        /// Index of the matched alternative.
        index: usize,
        /// Byte position the alternative started at.
        pos: usize,
        /// Byte position the match ended at (exclusive).
        end: usize,
        /// The matched value.
        value: &'a ParseValue,
    },
    /// A `@!name(e)` guard matched its inner expression `e`. Same verdict
    /// semantics as [`ParseEffect::AltMatched`], but attached locally in the
    /// grammar instead of globally to every choice.
    Guard {
        /// The guard name.
        name: &'a str,
        /// Byte position the guarded expression started at.
        pos: usize,
        /// Byte position the match ended at (exclusive).
        end: usize,
        /// The matched value.
        value: &'a ParseValue,
    },
    /// A `@name(e)` semantic action (or behavior transform) matched its inner
    /// expression. The host returns [`Directive::Accept`] with the transformed
    /// value (the unified replacement for `SemanticRuntime::invoke_action`).
    SemanticAction {
        /// The action name.
        name: &'a str,
        /// Scalar arguments passed in the grammar.
        args: &'a [GrammarScalar],
        /// Byte position the inner expression started at.
        pos: usize,
        /// Byte position the match ended at (exclusive).
        end: usize,
        /// The matched value to transform.
        value: &'a ParseValue,
    },
    /// A `@?name` semantic predicate (or behavior predicate). The host returns
    /// `Proceed` to accept or `Reject` to fail (the unified replacement for
    /// `SemanticRuntime::invoke_predicate`). For a bare `@?name` the value is
    /// `Nil` and `pos == end`; behavior predicates carry the matched value.
    SemanticPredicate {
        /// The predicate name.
        name: &'a str,
        /// Scalar arguments passed in the grammar.
        args: &'a [GrammarScalar],
        /// Byte position the predicate was evaluated at.
        pos: usize,
        /// Byte position the match ended at (exclusive).
        end: usize,
        /// The matched value (or `Nil` for a bare predicate).
        value: &'a ParseValue,
    },
    /// The whole parse failed; the host may rewrite the diagnostic.
    Failed {
        /// Furthest byte position reached.
        furthest: usize,
        /// Tokens/labels expected at the failure position.
        expected: &'a [String],
    },
}

// ── Directives: what the engine does ────────────────────────────────────────

/// The host's answer to a [`ParseEffect`]. A closed, total algebra: each variant
/// maps onto a control-flow primitive the engine already has, so the parser's
/// invariants (compositionality, backtracking, memo) stay intact. There is no
/// "jump to position" / "goto rule" — unscoped control transfer would break PEG
/// composition; use [`ParseView::sub_parse`] for disciplined steering instead.
#[derive(Debug, Default)]
pub enum Directive {
    /// Behave exactly as the parser normally would. The built-in default.
    #[default]
    Proceed,
    /// Treat the match as a success carrying this (possibly rewritten) value.
    Accept(ParseValue),
    /// Treat the current match as a failure → natural PEG backtrack. On a choice
    /// alternative this means "try the next alternative".
    Reject,
    /// Accept and commit: prune the remaining alternatives of the enclosing
    /// choice (equivalent to a `~` cut at this point).
    Commit,
    /// Only meaningful for [`ParseEffect::ChoiceEnter`]: replace the candidate
    /// alternatives with this ordered list of 0-based indices (a reorder and/or
    /// filter). Out-of-range indices are ignored.
    Restrict(Vec<usize>),
    /// Abort the parse with a host-authored hard error.
    Fail(String),
}

// ── Transactional state ─────────────────────────────────────────────────────

/// An opaque, host-owned snapshot token. The engine treats it as a black box: it
/// receives one from [`ParseDriver::checkpoint`] before a speculative branch and
/// hands the *same* token back to exactly one of [`ParseDriver::rollback`] (the
/// branch failed) or [`ParseDriver::commit`] (the branch is kept).
#[derive(Default)]
pub struct DriverCheckpoint(pub Option<Box<dyn std::any::Any>>);

impl DriverCheckpoint {
    /// The empty snapshot (used by stateless drivers; the default).
    pub fn none() -> Self {
        Self(None)
    }

    /// Wrap a host payload (e.g. a journal cursor or a cloned state) as a token.
    pub fn of<T: std::any::Any>(payload: T) -> Self {
        Self(Some(Box::new(payload)))
    }

    /// Recover the host payload previously stored with [`DriverCheckpoint::of`].
    pub fn take<T: std::any::Any>(self) -> Option<T> {
        self.0
            .and_then(|boxed| boxed.downcast::<T>().ok())
            .map(|boxed| *boxed)
    }
}

impl std::fmt::Debug for DriverCheckpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "DriverCheckpoint(..)"
        } else {
            "DriverCheckpoint(none)"
        })
    }
}

// ── Memo soundness ──────────────────────────────────────────────────────────

/// How a rule's outcome depends on host semantic state. Packrat memoisation keys
/// on `(rule, pos)`; that is only sound when the outcome is a pure function of
/// the input at that position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoFacet {
    /// The rule's outcome does not depend on host state — safe to memoise (the
    /// default; identical to today's fast path).
    Pure,
    /// The rule's outcome depends on host state. The engine will not memoise it,
    /// keeping the cache sound. (The `u64` digest is reserved for a future
    /// context-keyed memo and is currently treated as "do not memoise".)
    Depends(u64),
}

// ── Read projections exposed to host hooks ──────────────────────────────────

/// Rust-safe projection of grammar data exposed to host hooks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarContext {
    /// The grammar's start rule.
    pub start_rule: String,
    /// Number of rules in the grammar.
    pub rule_count: usize,
    /// Import aliases attached to the grammar.
    pub import_aliases: Vec<String>,
    /// Metadata keys present on the grammar.
    pub metadata_keys: Vec<String>,
}

impl GrammarContext {
    /// Build a grammar-context projection from its parts.
    pub fn new(
        start_rule: impl Into<String>,
        rule_count: usize,
        import_aliases: Vec<String>,
        metadata_keys: Vec<String>,
    ) -> Self {
        Self {
            start_rule: start_rule.into(),
            rule_count,
            import_aliases,
            metadata_keys,
        }
    }
}

/// Rust-safe projection of parser config exposed to host hooks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParserConfigContext {
    /// Whether the root result is span-wrapped.
    pub return_spans: bool,
    /// Whether packrat memoisation is on.
    pub memo: bool,
    /// The input-size / step budget.
    pub max_steps: usize,
    /// The expression-nesting recursion-depth limit.
    pub max_depth: usize,
    /// Whether invalid-prefixed rules are exposed.
    pub include_invalid_rules: bool,
    /// The output mode, as `"value"` or `"ast"`.
    pub output_mode: String,
}

impl ParserConfigContext {
    /// Project a [`ParserConfig`] into the host-facing context.
    pub fn from_config(config: &ParserConfig) -> Self {
        let output_mode = match config.output_mode {
            ParserOutputMode::Value => "value",
            ParserOutputMode::Ast => "ast",
        };
        Self {
            return_spans: config.return_spans,
            memo: config.memo,
            max_steps: config.max_steps,
            max_depth: config.max_depth,
            include_invalid_rules: config.include_invalid_rules,
            output_mode: output_mode.to_string(),
        }
    }
}

/// Rust-safe projection of live parser state exposed to host hooks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParserStateContext {
    /// Whether trivia skipping is currently active.
    pub trivia_on: bool,
    /// Current parametric-rule call depth.
    pub param_depth: usize,
    /// Number of live memo entries.
    pub memo_entries: usize,
    /// Whether indentation tracking is enabled.
    pub indentation_enabled: bool,
    /// Current bracket nesting depth.
    pub bracket_depth: usize,
}

// ── View: what the host latches onto ────────────────────────────────────────

/// The result of a scoped sub-parse launched from a driver via
/// [`ParseView::sub_parse`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubParse {
    /// Whether the named rule matched at the requested position.
    pub ok: bool,
    /// Bytes consumed from the requested position.
    pub consumed: usize,
    /// The value produced, when `ok`.
    pub value: Option<ParseValue>,
}

/// Capability object that runs an isolated sub-parse against the same compiled
/// grammar. Implemented by the engine; the host only calls
/// [`ParseView::sub_parse`].
pub trait SubParseProvider {
    /// Run `rule` at byte position `pos` in an isolated sub-parse.
    fn run_sub_parse(&self, rule: &str, pos: usize) -> SubParse;
}

/// Read access to live parser state at the moment an effect is raised, plus a
/// disciplined steering escape hatch ([`ParseView::sub_parse`]).
///
/// The view is **cheap to construct**: it only stores borrows. The richer
/// projections ([`ParseView::named`], [`items`](ParseView::items),
/// [`grammar`](ParseView::grammar), [`config`](ParseView::config),
/// [`state`](ParseView::state)) are computed on demand, so observers that read
/// nothing (the AST builder, `on_event`) pay nothing.
pub struct ParseView<'a> {
    /// The full source text being parsed.
    pub source: &'a str,
    /// Byte offset the effect was raised at.
    pub pos: usize,
    /// The matched span `(start, end)` when the effect carries a value.
    pub span: Option<(usize, usize)>,
    /// The matched source text (the slice of `span`), or `""` when not applicable.
    pub matched_text: &'a str,
    /// Scalar arguments (used by behavior predicates); empty for plain hooks.
    pub args: &'a [GrammarScalar],
    /// The rule-call stack (outermost first); the current rule is the last entry.
    pub rule_stack: &'a [&'a str],
    /// The grammar's start rule.
    pub start_rule: &'a str,
    pub(crate) value: Option<&'a ParseValue>,
    pub(crate) import_aliases: &'a [String],
    pub(crate) metadata_keys: &'a [String],
    pub(crate) rule_count: usize,
    pub(crate) config_context: &'a ParserConfigContext,
    pub(crate) trivia_on: bool,
    pub(crate) param_depth: usize,
    pub(crate) memo_entries: usize,
    pub(crate) indentation_enabled: bool,
    pub(crate) bracket_depth: usize,
    pub(crate) sub: Option<&'a dyn SubParseProvider>,
}

impl<'a> ParseView<'a> {
    /// Named bindings (`name:e`) collected from the matched value (computed on
    /// demand; empty when the effect carries no value).
    pub fn named(&self) -> HashMap<String, ParseValue> {
        match self.value {
            Some(value) => value
                .named_bindings()
                .into_iter()
                .map(|(key, val)| (key, val.clone()))
                .collect(),
            None => HashMap::new(),
        }
    }

    /// The matched value's child items (computed on demand).
    pub fn items(&self) -> Vec<ParseValue> {
        match self.value {
            Some(value) => view_items(value),
            None => Vec::new(),
        }
    }

    /// Rust-safe projection of the grammar (computed on demand).
    pub fn grammar(&self) -> GrammarContext {
        GrammarContext::new(
            self.start_rule,
            self.rule_count,
            self.import_aliases.to_vec(),
            self.metadata_keys.to_vec(),
        )
    }

    /// Rust-safe projection of the parser configuration (computed on demand).
    pub fn config(&self) -> ParserConfigContext {
        self.config_context.clone()
    }

    /// Rust-safe projection of live parser state (computed on demand).
    pub fn state(&self) -> ParserStateContext {
        ParserStateContext {
            trivia_on: self.trivia_on,
            param_depth: self.param_depth,
            memo_entries: self.memo_entries,
            indentation_enabled: self.indentation_enabled,
            bracket_depth: self.bracket_depth,
        }
    }

    /// The unconsumed input from [`ParseView::pos`] onward.
    pub fn remaining(&self) -> &'a str {
        self.source.get(self.pos..).unwrap_or("")
    }

    /// The source slice `[start, end)`, or `""` if out of bounds / not on a
    /// character boundary.
    pub fn slice(&self, start: usize, end: usize) -> &'a str {
        if start <= end && self.source.is_char_boundary(start) && self.source.is_char_boundary(end)
        {
            self.source.get(start..end).unwrap_or("")
        } else {
            ""
        }
    }

    /// The rule currently being evaluated, if any.
    pub fn current_rule(&self) -> Option<&'a str> {
        self.rule_stack.last().copied()
    }

    /// Run an isolated, transactionally-clean sub-parse of `rule` starting at
    /// `pos`. The sub-parse uses the same compiled grammar but a fresh memo and
    /// **does not** re-enter the driver (so it cannot recurse infinitely). This
    /// is the disciplined way to "steer" — semantic lookahead, speculative
    /// resolution — without breaking PEG composition.
    pub fn sub_parse(&self, rule: &str, pos: usize) -> SubParse {
        match self.sub {
            Some(provider) => provider.run_sub_parse(rule, pos),
            None => SubParse {
                ok: false,
                consumed: 0,
                value: None,
            },
        }
    }
}

/// The child items of a matched value, as exposed via [`ParseView::items`].
fn view_items(value: &ParseValue) -> Vec<ParseValue> {
    match value {
        ParseValue::Node(_, children) => (**children).clone(),
        ParseValue::SpannedValue { value, .. } => match value.as_ref() {
            ParseValue::Node(_, children) => (**children).clone(),
            inner => vec![inner.clone()],
        },
        other if other.is_nil() => Vec::new(),
        other => vec![other.clone()],
    }
}

// ── The host trait ──────────────────────────────────────────────────────────

/// A host-supplied policy that drives the parser. Object-safe: attach it as
/// `&dyn ParseDriver`; the driver owns its own semantic state via interior
/// mutability (e.g. a `RefCell` symbol table).
///
/// Every method has a default, so a driver only implements what it needs:
/// observe-only (override nothing but `handle`), reject/steer (`handle`),
/// stateful with backtracking (`checkpoint`/`rollback`/`commit`), and
/// memo-correct (`memo_facet`).
pub trait ParseDriver {
    /// Answer a decision point. The default makes the engine behave normally.
    fn handle(&self, effect: &ParseEffect<'_>, view: &ParseView<'_>) -> Directive {
        let _ = (effect, view);
        Directive::Proceed
    }

    /// Snapshot host state before the engine explores a speculative branch.
    fn checkpoint(&self) -> DriverCheckpoint {
        DriverCheckpoint::none()
    }

    /// Restore host state because a speculative branch failed and is unwound.
    fn rollback(&self, snapshot: DriverCheckpoint) {
        let _ = snapshot;
    }

    /// Discard a snapshot because the branch it guarded is being kept.
    fn commit(&self, snapshot: DriverCheckpoint) {
        let _ = snapshot;
    }

    /// Declare whether a rule's outcome depends on host state, so the engine can
    /// keep memoisation sound. Defaults to [`MemoFacet::Pure`].
    fn memo_facet(&self, rule: &str) -> MemoFacet {
        let _ = rule;
        MemoFacet::Pure
    }
}

// ── Ergonomic builder for the common case ───────────────────────────────────

type GuardFn = Box<dyn Fn(&ParseValue, &ParseView<'_>) -> Directive>;
type ActionFn = Box<dyn Fn(ParseValue, &ParseView<'_>) -> ParseValue>;
type PredicateFn = Box<dyn Fn(&ParseView<'_>) -> bool>;
type EventFn = Box<dyn Fn(&ParseEffect<'_>, &ParseView<'_>)>;
type InterceptFn = Box<dyn Fn(&ParseEffect<'_>, &ParseView<'_>) -> Directive>;

/// A builder for the common driver shape: named handlers for the grammar's
/// semantic hooks (`@!guard`, `@action`, `@?pred`) plus an optional event
/// observer — with no host state. The unified, ergonomic replacement for the
/// old `SemanticRuntimeBuilder`.
///
/// For stateful control (symbol tables, scopes) implement [`ParseDriver`]
/// directly so you can also provide `checkpoint`/`rollback`/`memo_facet`.
///
/// ```
/// use caap_peg::{Grammar, ParseRequest, ParseValue, ParseDriverBuilder};
///
/// // `@!kw(e)` is a guard that accepts only the keyword "let".
/// let grammar = Grammar::trusted_new("root <- @!kw(/[a-z]+/)").with_start_rule("root");
/// let driver = ParseDriverBuilder::new()
///     .accept_if("kw", |value, _view| matches!(value, ParseValue::Text(t) if &**t == "let"))
///     .build();
///
/// assert!(ParseRequest::new(&grammar).driver(&driver).run("let").is_ok());
/// assert!(ParseRequest::new(&grammar).driver(&driver).run("xyz").is_err());
/// ```
#[derive(Default)]
pub struct ParseDriverBuilder {
    guards: HashMap<String, GuardFn>,
    actions: HashMap<String, ActionFn>,
    predicates: HashMap<String, PredicateFn>,
    on_event: Option<EventFn>,
    intercept: Option<InterceptFn>,
    auto_scope: bool,
}

impl ParseDriverBuilder {
    /// An empty driver builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a handler for `@!name(e)`. Returns a [`Directive`].
    pub fn guard<F>(mut self, name: impl Into<String>, handler: F) -> Self
    where
        F: Fn(&ParseValue, &ParseView<'_>) -> Directive + 'static,
    {
        self.guards.insert(name.into(), Box::new(handler));
        self
    }

    /// Convenience: register a boolean guard — `true` accepts, `false` rejects.
    pub fn accept_if<F>(self, name: impl Into<String>, predicate: F) -> Self
    where
        F: Fn(&ParseValue, &ParseView<'_>) -> bool + 'static,
    {
        self.guard(name, move |value, view| {
            if predicate(value, view) {
                Directive::Proceed
            } else {
                Directive::Reject
            }
        })
    }

    /// Register a `@name(e)` semantic action: transform the matched value.
    pub fn action<F>(mut self, name: impl Into<String>, handler: F) -> Self
    where
        F: Fn(ParseValue, &ParseView<'_>) -> ParseValue + 'static,
    {
        self.actions.insert(name.into(), Box::new(handler));
        self
    }

    /// Register a `@?name` semantic predicate: `true` accepts, `false` rejects.
    pub fn predicate<F>(mut self, name: impl Into<String>, handler: F) -> Self
    where
        F: Fn(&ParseView<'_>) -> bool + 'static,
    {
        self.predicates.insert(name.into(), Box::new(handler));
        self
    }

    /// Observe every effect (the unified replacement for `with_trace`). The
    /// observer cannot change control flow; it only watches.
    pub fn on_event<F>(mut self, observer: F) -> Self
    where
        F: Fn(&ParseEffect<'_>, &ParseView<'_>) + 'static,
    {
        self.on_event = Some(Box::new(observer));
        self
    }

    /// Intercept *any* effect for global control (e.g. `AltMatched`,
    /// `RuleExit`, `ChoiceEnter`). Called for every effect; if it returns a
    /// non-`Proceed` [`Directive`], that wins, otherwise the named
    /// guard/action/predicate registries are consulted. The escape hatch for
    /// control beyond named hooks without implementing [`ParseDriver`] by hand.
    pub fn intercept<F>(mut self, handler: F) -> Self
    where
        F: Fn(&ParseEffect<'_>, &ParseView<'_>) -> Directive + 'static,
    {
        self.intercept = Some(Box::new(handler));
        self
    }

    /// Auto-generate scope predicates: `@?in_<rule>` succeeds iff `<rule>` is on
    /// the rule stack; `@?not_in_<rule>` is its complement. No explicit
    /// registration needed.
    pub fn with_auto_scope(mut self) -> Self {
        self.auto_scope = true;
        self
    }

    /// Finalise the builder into a ready-to-attach [`BuiltDriver`].
    pub fn build(self) -> BuiltDriver {
        BuiltDriver {
            guards: self.guards,
            actions: self.actions,
            predicates: self.predicates,
            on_event: self.on_event,
            intercept: self.intercept,
            auto_scope: self.auto_scope,
        }
    }
}

/// A [`ParseDriver`] produced by [`ParseDriverBuilder`]: routes the grammar's
/// semantic-hook effects to registered handlers and proceeds on everything else.
pub struct BuiltDriver {
    guards: HashMap<String, GuardFn>,
    actions: HashMap<String, ActionFn>,
    predicates: HashMap<String, PredicateFn>,
    on_event: Option<EventFn>,
    intercept: Option<InterceptFn>,
    auto_scope: bool,
}

impl ParseDriver for BuiltDriver {
    fn handle(&self, effect: &ParseEffect<'_>, view: &ParseView<'_>) -> Directive {
        if let Some(observer) = &self.on_event {
            observer(effect, view);
        }
        if let Some(intercept) = &self.intercept {
            let directive = intercept(effect, view);
            if !matches!(directive, Directive::Proceed) {
                return directive;
            }
        }
        match effect {
            ParseEffect::Guard { name, value, .. } => self
                .guards
                .get(*name)
                .map(|handler| handler(value, view))
                .unwrap_or(Directive::Proceed),
            ParseEffect::SemanticAction { name, value, .. } => match self.actions.get(*name) {
                Some(handler) => Directive::Accept(handler((*value).clone(), view)),
                None => Directive::Proceed,
            },
            ParseEffect::SemanticPredicate { name, .. } => {
                if self.auto_scope {
                    if let Some(rule) = name.strip_prefix("in_") {
                        return verdict(view.rule_stack.contains(&rule));
                    }
                    if let Some(rule) = name.strip_prefix("not_in_") {
                        return verdict(!view.rule_stack.contains(&rule));
                    }
                }
                match self.predicates.get(*name) {
                    Some(handler) => verdict(handler(view)),
                    None => Directive::Proceed,
                }
            }
            _ => Directive::Proceed,
        }
    }
}

fn verdict(accept: bool) -> Directive {
    if accept {
        Directive::Proceed
    } else {
        Directive::Reject
    }
}
