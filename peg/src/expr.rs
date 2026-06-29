//! PEG expression tree — `PegExpr`, `CompiledRegex`, `RuleTextParser`, `peg_expr_to_source`.
//!
//! Moved here from `parser.rs` so that `grammar.rs` can store compiled expression
//! trees directly in `GrammarRule`, eliminating repeated text-parsing on every
//! `parse()` call.

use std::sync::{Arc, OnceLock};

use regex::Regex as StdRegex;
use regex_automata::{
    dfa::{dense, Automaton, StartKind},
    Anchored, Input, MatchKind,
};

use crate::error::ParseError;

/// Dense DFA used solely to compute how far a regex terminal *examines* the
/// input (see [`CompiledRegex::examined_len`]).
type ExtentDfa = dense::DFA<Vec<u32>>;

/// Node tag wrapping the text a `recover(…)` terminal skipped over.
pub const RECOVER_TAG: &str = "<recovered>";

// ── CompiledRegex ─────────────────────────────────────────────────────────────

/// A compiled regex that also retains the source pattern for equality checks.
///
/// `regex::Regex` clones cheaply (internally `Arc`-backed), so cloning a
/// `CompiledRegex` does not recompile the pattern.
#[derive(Clone, Debug)]
pub struct CompiledRegex {
    /// The source pattern (retained for equality and round-tripping).
    pub pattern: String,
    /// The matcher, compiled **anchored** (`\A(?:…)`) so it can only match at the
    /// current position and never scans forward past it — both a perf win on
    /// failure and the precondition that makes the examined-extent computation a
    /// sound bound on the bytes the match actually depends on.
    pub inner: StdRegex,
    /// Lazily-built dense DFA over `pattern`, used only to compute the examined
    /// byte extent for incremental reuse. `None` means the DFA exceeded the build
    /// size limit (or failed to build); callers then treat the extent as
    /// "to end-of-input" (sound, conservative). Built on first use so a plain
    /// (non-incremental) parse never pays for it. Not part of identity.
    extent_dfa: OnceLock<Option<Arc<ExtentDfa>>>,
}

impl CompiledRegex {
    /// Compile `pattern` anchored at the current position. `err_start`/`err_end`
    /// locate a compile error in the surrounding grammar source.
    pub fn new(
        pattern: impl Into<String>,
        err_start: usize,
        err_end: usize,
    ) -> Result<Self, ParseError> {
        let pattern = pattern.into();
        // Anchor the matcher: a PEG regex terminal only ever matches at the
        // current position (the old code compiled unanchored and checked
        // `start == 0`), and anchoring stops the engine from scanning the rest
        // of the input on a failed/short match.
        let inner = StdRegex::new(&format!(r"\A(?:{pattern})")).map_err(|e| {
            ParseError::new(
                format!("invalid regular expression /{pattern}/: {e}"),
                err_start,
                err_end,
            )
        })?;
        Ok(Self {
            pattern,
            inner,
            extent_dfa: OnceLock::new(),
        })
    }

    /// Number of leading bytes of `hay` that this regex *examines* when matched
    /// anchored at its start — i.e. the offset at which the pattern's automaton
    /// can no longer extend (it dies). This is a sound upper bound on the bytes
    /// the match outcome depends on: lookahead and the greedy "stop byte" are
    /// included, and a failed match that scans far is reported as far as it read.
    ///
    /// Returns `None` when no DFA is available (oversized pattern); callers then
    /// fall back to the whole remaining input.
    pub(crate) fn examined_len(&self, hay: &[u8]) -> Option<usize> {
        let dfa = self
            .extent_dfa
            .get_or_init(|| build_extent_dfa(&self.pattern).map(Arc::new))
            .as_ref()?;
        Some(scan_examined_len(dfa, hay))
    }
}

/// Build a start-anchored dense DFA for `pattern`. The anchored start state has
/// no implicit `.*?` prefix, so the automaton dies (no live state) once the
/// anchored match cannot extend rather than staying alive scanning for a match
/// at a later offset; `MatchKind::LeftmostFirst` mirrors the real matcher so the
/// death offset is tight. Bounded so a pathological pattern can't blow up
/// memory/build time (returns `None` then, and callers fall back to end-of-input).
fn build_extent_dfa(pattern: &str) -> Option<ExtentDfa> {
    dense::Builder::new()
        .configure(
            dense::Config::new()
                .start_kind(StartKind::Anchored)
                .match_kind(MatchKind::LeftmostFirst)
                .dfa_size_limit(Some(1 << 20))
                .determinize_size_limit(Some(1 << 20)),
        )
        .build(pattern)
        .ok()
}

/// Feed `hay` into the start-anchored `dfa` and return how many bytes were read
/// before its automaton reached a dead/quit state (or end-of-input). That byte —
/// the one that proved no further match is possible — was examined, so it is
/// included in the count. The anchored start prevents the automaton from
/// restarting at a later offset, so this is the bytes the anchored match depends on.
fn scan_examined_len(dfa: &ExtentDfa, hay: &[u8]) -> usize {
    let input = Input::new(hay).anchored(Anchored::Yes);
    let Ok(mut sid) = dfa.start_state_forward(&input) else {
        return hay.len();
    };
    if dfa.is_dead_state(sid) {
        return 0;
    }
    for (i, &b) in hay.iter().enumerate() {
        sid = dfa.next_state(sid, b);
        if dfa.is_dead_state(sid) || dfa.is_quit_state(sid) {
            return i + 1;
        }
    }
    hay.len()
}

impl PartialEq for CompiledRegex {
    fn eq(&self, other: &Self) -> bool {
        self.pattern == other.pattern
    }
}

impl Eq for CompiledRegex {}

// ── CharClass ───────────────────────────────────────────────────────────────

/// A single-character class — the native, regex-free form of `[…]` terminals
/// (e.g. `[a-z]`, `[^"]`, `[A-Za-z0-9_]`). Matches exactly one Unicode scalar,
/// returning it as `ParseValue::Text`, identical to the old single-class regex
/// but without a regex engine: one byte/step and an exact read extent.
///
/// `ranges` are inclusive, sorted, and merged (canonical) so equality is
/// structural and the printer round-trips.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CharClass {
    /// Whether the class is negated (`[^…]`).
    pub negated: bool,
    /// Inclusive, sorted, merged character ranges.
    pub ranges: Vec<(char, char)>,
}

impl CharClass {
    /// Build from raw (possibly unsorted/overlapping) ranges, normalising them.
    fn new(negated: bool, mut ranges: Vec<(char, char)>) -> Self {
        ranges.retain(|(lo, hi)| lo <= hi);
        ranges.sort_unstable();
        let mut merged: Vec<(char, char)> = Vec::with_capacity(ranges.len());
        for (lo, hi) in ranges {
            match merged.last_mut() {
                // Merge overlapping or directly adjacent ranges.
                Some((_, prev_hi)) if (*prev_hi as u32).saturating_add(1) >= lo as u32 => {
                    if hi > *prev_hi {
                        *prev_hi = hi;
                    }
                }
                _ => merged.push((lo, hi)),
            }
        }
        Self {
            negated,
            ranges: merged,
        }
    }

    /// Render back to `[…]` source (used by the printer and diagnostics).
    pub fn to_source(&self) -> String {
        char_class_to_source(self)
    }

    /// Whether `c` is a member (honouring negation).
    #[inline]
    pub fn contains(&self, c: char) -> bool {
        let in_ranges = self.ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi);
        in_ranges != self.negated
    }

    /// Parse the body of a `[…]` class (the text between the brackets, with
    /// escapes still written as `\x`). Returns `None` for anything not confidently
    /// representable as plain ranges (e.g. `\D`, POSIX classes, Unicode props), so
    /// the caller falls back to a regex — never a wrong class.
    pub fn parse_body(body: &str) -> Option<Self> {
        let mut chars = body.chars().peekable();
        let negated = chars.peek() == Some(&'^') && {
            chars.next();
            true
        };
        let mut ranges: Vec<(char, char)> = Vec::new();
        while let Some(atom) = read_class_atom(&mut chars)? {
            match atom {
                ClassAtom::Shorthand(rs) => ranges.extend(rs),
                ClassAtom::Ch(c1) => {
                    // A `-` between two single chars (with something after it) is a
                    // range; otherwise `-` is a literal handled on the next pass.
                    if chars.peek() == Some(&'-') {
                        let mut look = chars.clone();
                        look.next();
                        if look.peek().is_some() {
                            chars.next(); // consume '-'
                            match read_class_atom(&mut chars)? {
                                Some(ClassAtom::Ch(c2)) if c1 <= c2 => ranges.push((c1, c2)),
                                _ => return None,
                            }
                            continue;
                        }
                    }
                    ranges.push((c1, c1));
                }
            }
        }
        if ranges.is_empty() {
            return None;
        }
        Some(Self::new(negated, ranges))
    }
}

/// One parsed element of a char-class body.
enum ClassAtom {
    Ch(char),
    Shorthand(Vec<(char, char)>),
}

/// Read one atom from a class body. Returns `Ok(None)` at the end, `None`
/// (the outer `?`) for an unsupported construct.
fn read_class_atom(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Option<Option<ClassAtom>> {
    let Some(c) = chars.next() else {
        return Some(None);
    };
    if c != '\\' {
        return Some(Some(ClassAtom::Ch(c)));
    }
    let esc = chars.next()?;
    let atom = match esc {
        'n' => ClassAtom::Ch('\n'),
        't' => ClassAtom::Ch('\t'),
        'r' => ClassAtom::Ch('\r'),
        '0' => ClassAtom::Ch('\0'),
        '\\' | ']' | '[' | '-' | '^' | '/' | '"' | '\'' | '.' => ClassAtom::Ch(esc),
        'd' => ClassAtom::Shorthand(vec![('0', '9')]),
        'w' => ClassAtom::Shorthand(vec![('0', '9'), ('A', 'Z'), ('_', '_'), ('a', 'z')]),
        's' => ClassAtom::Shorthand(vec![('\t', '\n'), ('\u{0b}', '\r'), (' ', ' ')]),
        // `\D`, `\W`, `\S`, `\p{…}`, `\xNN`, … — not confidently a plain range.
        _ => return None,
    };
    Some(Some(atom))
}

/// Serialise a [`CharClass`] back to `[…]` source that re-parses to the same set.
fn char_class_to_source(class: &CharClass) -> String {
    let mut out = String::from("[");
    if class.negated {
        out.push('^');
    }
    for &(lo, hi) in &class.ranges {
        push_class_char(&mut out, lo);
        if lo != hi {
            out.push('-');
            push_class_char(&mut out, hi);
        }
    }
    out.push(']');
    out
}

/// Emit one class member char, escaping the characters that are special inside a
/// class so the printer round-trips through [`CharClass::parse_body`].
fn push_class_char(out: &mut String, c: char) {
    match c {
        '\n' => out.push_str("\\n"),
        '\t' => out.push_str("\\t"),
        '\r' => out.push_str("\\r"),
        '\0' => out.push_str("\\0"),
        '\\' | ']' | '[' | '^' | '-' => {
            out.push('\\');
            out.push(c);
        }
        // Other chars (incl. rare control chars) round-trip verbatim as literals:
        // none of them collide with class syntax.
        c => out.push(c),
    }
}

// ── Operator precedence ─────────────────────────────────────────────────────

/// The fixity / associativity of an operator precedence level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fixity {
    /// Infix, left-associative (`a - b - c` = `(a - b) - c`).
    InfixLeft,
    /// Infix, right-associative (`a ^ b ^ c` = `a ^ (b ^ c)`).
    InfixRight,
    /// Infix, non-associative: `a == b == c` is a parse error.
    InfixNon,
    /// Prefix unary (`-a`, `!a`).
    Prefix,
    /// Postfix unary (`a!`, `a++`).
    Postfix,
    /// Ternary / mixfix `a ? b : c` — the level's two operators are the open
    /// (`?`) and close (`:`) markers. Right-associative on the else branch.
    Ternary,
}

/// One precedence level: operators sharing a precedence and [`Fixity`]. Levels
/// are ordered low → high precedence in [`PegExpr::Precedence`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrecLevel {
    /// Associativity/position of this level's operators.
    pub fixity: Fixity,
    /// Operator matchers (e.g. `Literal("+")`); tried in order at each position.
    pub operators: Vec<PegExpr>,
}

// ── PegExpr ───────────────────────────────────────────────────────────────────

/// A parsed PEG grammar expression tree.
///
/// Grammar rules now store `PegExpr` directly (in `GrammarRule.expr`) so the
/// parser can evaluate trees without reparsing text on every `parse()` call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PegExpr {
    // ── Terminals ─────────────────────────────────────────────────────────
    /// An exact string literal (`"…"` / `'…'`).
    Literal(String),
    /// Any single character (`.`).
    Dot,
    /// An arbitrary regex terminal (`/…/`), anchored at the current position.
    Regex(CompiledRegex),
    /// A single-character class `[…]` matched natively (no regex engine).
    CharClass(CharClass),
    /// Grammar-level error recovery: `recover("sync", …)`. Used as a fallback
    /// alternative, it skips input up to and including the earliest of its sync
    /// literals (or to end-of-input), always succeeding with a `<recovered>`
    /// node wrapping the skipped text — so a malformed region is localised
    /// instead of failing the whole parse.
    Recover {
        /// Sync literals; recovery skips to and past the earliest one found.
        syncs: Vec<String>,
    },
    // ── Structural combinators ────────────────────────────────────────────
    /// Reference to another rule by name.
    Ref(String),
    /// Match each sub-expression in order.
    Sequence(Vec<PegExpr>),
    /// Ordered choice; the first matching alternative wins.
    Choice(Vec<PegExpr>),
    /// Positive lookahead `&e` (no input consumed).
    And(Box<PegExpr>),
    /// Negative lookahead `!e` (no input consumed).
    Not(Box<PegExpr>),
    /// Commit point `~`: a later failure in the sequence is fatal to the choice.
    Cut,
    /// Zero or one `e?`.
    Optional(Box<PegExpr>),
    /// One or more `e+`.
    OneOrMore(Box<PegExpr>),
    /// Zero or more `e*`.
    ZeroOrMore(Box<PegExpr>),
    /// Counted repetition `e{min}` / `e{min,}` / `e{min,max}`. `max == None`
    /// means unbounded.
    Repeat {
        /// The repeated sub-expression.
        expr: Box<PegExpr>,
        /// Minimum number of repetitions.
        min: usize,
        /// Maximum number of repetitions (`None` = unbounded).
        max: Option<usize>,
    },
    /// Operator-precedence expression (precedence climbing, no left recursion):
    /// an `operand` combined by infix operators grouped into precedence
    /// `levels` (lowest precedence first). Produces left/right-nested
    /// `Node("binop", [lhs, op, rhs])` values per associativity.
    Precedence {
        /// The base operand expression.
        operand: Box<PegExpr>,
        /// Operator levels, lowest precedence first.
        levels: Vec<PrecLevel>,
    },
    /// `element (separator element)*`
    SepOneOrMore {
        /// The repeated element.
        element: Box<PegExpr>,
        /// The separator between elements (dropped from the result).
        separator: Box<PegExpr>,
    },
    /// `element (separator element)*` — like `SepOneOrMore` but keeps separator values.
    ///
    /// Output: `Node("interspersed", [elem1, sep1, elem2, sep2, ...])`
    Interspersed {
        /// The repeated element.
        element: Box<PegExpr>,
        /// The separator between elements (kept in the result).
        separator: Box<PegExpr>,
    },
    // ── Value bindings ────────────────────────────────────────────────────
    /// `name:e` — wrap the match of `e` as a named binding.
    Named {
        /// The binding name.
        name: String,
        /// The bound sub-expression.
        expr: Box<PegExpr>,
    },
    // ── Error-label override ──────────────────────────────────────────────
    /// `expected("msg", e)` — replace the failure label for `e` with `message`.
    Expected {
        /// The replacement failure label.
        message: String,
        /// The sub-expression whose label is overridden.
        expr: Box<PegExpr>,
    },
    // ── Trivia control ────────────────────────────────────────────────────
    /// `no_trivia(e)` / `tight(e)` — disable trivia skipping while matching `e`.
    NoTrivia(Box<PegExpr>),
    /// `with_trivia("spec", e)`: evaluate `e` under a *different* trivia skipper
    /// (`spec` uses the same vocabulary as the `__grammar__.trivia` metadata:
    /// `"none"`, `"whitespace"`, `"default"`, or a regex). Scoped — the previous
    /// skipper is restored after `e`. Lets one grammar mix lexical conventions
    /// (e.g. a `;`-significant region inside an otherwise `;`-comment grammar).
    WithTrivia {
        /// Trivia spec (`"none"`, `"whitespace"`, `"default"`, or a regex).
        spec: String,
        /// The sub-expression evaluated under `spec`.
        expr: Box<PegExpr>,
    },
    // ── Layout-sensitive terminals ────────────────────────────────────────
    /// A newline (`\r?\n` / `\r`), layout-aware when indentation is on.
    Newline,
    /// An indentation increase.
    Indent,
    /// A matching indentation decrease.
    Dedent,
    // ── Semantic hooks ────────────────────────────────────────────────────
    /// `@name(e)` — transform the match of `e` via the host action `name`.
    SemanticAction {
        /// The host action name.
        name: String,
        /// The sub-expression whose value is transformed.
        expr: Box<PegExpr>,
    },
    /// `@?name` / `@name` — succeed or fail based on the host predicate `name`.
    SemanticPredicate {
        /// The host predicate name.
        name: String,
    },
    /// `@!name(e)` — a semantic guard. Matches `e`, then asks the host
    /// [`crate::driver::ParseDriver`] whether the match is *semantically* valid
    /// via a [`crate::driver::ParseEffect::Guard`] effect. The host's
    /// [`crate::driver::Directive`] decides accept / reject (backtrack) / commit
    /// / fail. With no driver attached the inner value passes through unchanged.
    SemanticGuard {
        /// The host guard name.
        name: String,
        /// The guarded sub-expression.
        expr: Box<PegExpr>,
    },
    // ── Span capture ─────────────────────────────────────────────────────
    /// `capture("label", e)` — wrap the match of `e` in a `SpannedValue`.
    Capture {
        /// The capture label.
        label: String,
        /// The captured sub-expression.
        expr: Box<PegExpr>,
    },
    // ── Delimiter-bounded matching ────────────────────────────────────────
    /// `island("s", "e")` — text between delimiters (no nesting).
    Island {
        /// Opening delimiter.
        start: String,
        /// Closing delimiter.
        end: String,
        /// Whether to keep the delimiters in the matched text.
        include_delims: bool,
    },
    /// `raw_block("s", "e", "kind")` — nested balanced delimiters (`s` ≠ `e`).
    RawBlock {
        /// Opening delimiter.
        start: String,
        /// Closing delimiter.
        end: String,
        /// A label describing the block kind (carried into the result node).
        delim_kind: String,
    },
    // ── Committed failure ─────────────────────────────────────────────────
    /// `!!e` — match `e`, escalating its failure to a hard error.
    Eager(Box<PegExpr>),
    // ── Lookbehind ────────────────────────────────────────────────────────
    /// `&<e` (positive) / `!<e` (negative): assert that `e` matches a suffix
    /// ending exactly at the current position. Consumes no input.
    LookBehind {
        /// The suffix expression asserted to end at the current position.
        expr: Box<PegExpr>,
        /// `true` for negative lookbehind (`!<e`), `false` for positive (`&<e`).
        negative: bool,
    },
    /// `backref("name")`: match input text equal to the most recent `name:`
    /// binding's captured text (context-sensitive; see the matcher).
    Backref(String),
    // ── Cross-grammar reference ───────────────────────────────────────────
    /// `grammar::rule` — reference a rule in an imported grammar.
    ImportedRef {
        /// The imported grammar's alias.
        grammar_name: String,
        /// The referenced rule name within that grammar.
        rule_name: String,
    },
    // ── Parametric rules ──────────────────────────────────────────────────
    /// `$name` — reference a parameter inside a parametric rule body.
    Parameter {
        /// The parameter name.
        name: String,
    },
    /// `rule(arg1, arg2)` — call a parametric rule with argument expressions.
    Call {
        /// The called rule name.
        rule: String,
        /// The argument expressions.
        args: Vec<PegExpr>,
    },
    // ── Keyword terminals ─────────────────────────────────────────────────
    /// `kw("word")` — a hard keyword (not followed by an identifier char).
    HardKeyword(String),
    /// A contextual keyword. Matched identically to [`PegExpr::HardKeyword`]
    /// (word-boundary check) on purpose: a soft keyword's contextual nature is
    /// expressed by where it appears in the grammar, not by a runtime keyword
    /// list. The distinct variant is kept so grammars can document intent and
    /// so a future contextual policy can hook in without a grammar change.
    SoftKeyword(String),
    // ── Grammar scope ─────────────────────────────────────────────────────
    /// `scope("grammar", e)` — evaluate `e` against an imported grammar.
    GrammarScope {
        /// The imported grammar's alias to evaluate against.
        grammar_name: String,
        /// The sub-expression evaluated in that grammar's scope.
        expr: Box<PegExpr>,
    },
    // ── Token-stream terminal ─────────────────────────────────────────────
    /// `tok(KIND)` / `tok(KIND, "text")` / `tok("text")` — a token from a stream.
    TokenRef {
        /// Required token kind, if any.
        kind: Option<String>,
        /// Required token text, if any.
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
        PegExpr::CharClass(class) => class.to_source(),
        PegExpr::Ref(name) => name.clone(),
        PegExpr::Sequence(exprs) => {
            if exprs.is_empty() {
                return "\"\"".to_string();
            }
            let rendered: Vec<String> = exprs.iter().map(seq_item).collect();
            let mut out = String::new();
            for (i, item) in rendered.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                // An item ending in an identifier char directly before a
                // parenthesised item would re-read as a call `ident(...)`; group
                // it to keep them separate sequence elements.
                let ends_ident = item
                    .chars()
                    .last()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
                let next_paren = rendered.get(i + 1).is_some_and(|n| n.starts_with('('));
                if ends_ident && next_paren {
                    out.push('(');
                    out.push_str(item);
                    out.push(')');
                } else {
                    out.push_str(item);
                }
            }
            out
        }
        PegExpr::Choice(exprs) => exprs.iter().map(choice_alt).collect::<Vec<_>>().join(" / "),
        PegExpr::And(e) => format!("&{}", prefix_operand(e)),
        PegExpr::Not(e) => {
            // `!` followed by an operand that itself renders starting with `!`
            // would read as `!!` (Eager); parenthesise to keep it a `Not`.
            let inner = prefix_operand(e);
            if inner.starts_with('!') || inner.starts_with('<') {
                format!("!({})", peg_expr_to_source(e))
            } else {
                format!("!{inner}")
            }
        }
        PegExpr::Cut => "~".to_string(),
        PegExpr::Optional(e) => format!("{}?", rep_operand(e)),
        PegExpr::OneOrMore(e) => format!("{}+", rep_operand(e)),
        PegExpr::ZeroOrMore(e) => format!("{}*", rep_operand(e)),
        PegExpr::Repeat { expr, min, max } => {
            let inner = rep_operand(expr);
            match max {
                Some(mx) if mx == min => format!("{inner}{{{min}}}"),
                Some(mx) => format!("{inner}{{{min},{mx}}}"),
                None => format!("{inner}{{{min},}}"),
            }
        }
        PegExpr::Precedence { operand, levels } => {
            let mut out = format!("prec({}", peg_expr_to_source(operand));
            for level in levels {
                let kw = match level.fixity {
                    Fixity::InfixLeft => "infixl",
                    Fixity::InfixRight => "infixr",
                    Fixity::InfixNon => "infixn",
                    Fixity::Prefix => "prefix",
                    Fixity::Postfix => "postfix",
                    Fixity::Ternary => "ternary",
                };
                let ops = level
                    .operators
                    .iter()
                    .map(peg_expr_to_source)
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(", {kw}({ops})"));
            }
            out.push(')');
            out
        }
        PegExpr::SepOneOrMore { element, separator } => {
            format!(
                "sep_plus({}, {})",
                peg_expr_to_source(element),
                peg_expr_to_source(separator)
            )
        }
        PegExpr::Interspersed { element, separator } => {
            format!(
                "interspersed({}, {})",
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
        PegExpr::WithTrivia { spec, expr } => {
            format!(
                "with_trivia({}, {})",
                quote_string(spec),
                peg_expr_to_source(expr)
            )
        }
        PegExpr::Newline => "newline".to_string(),
        PegExpr::Indent => "indent".to_string(),
        PegExpr::Dedent => "dedent".to_string(),
        PegExpr::SemanticAction { name, expr } => {
            format!("@{}({})", name, peg_expr_to_source(expr))
        }
        PegExpr::SemanticPredicate { name } => format!("@?{}", name),
        PegExpr::SemanticGuard { name, expr } => {
            format!("@!{}({})", name, peg_expr_to_source(expr))
        }
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
        PegExpr::LookBehind { expr, negative } => {
            let sign = if *negative { "!<" } else { "&<" };
            format!("{sign}{}", prefix_operand(expr))
        }
        PegExpr::Backref(name) => format!("backref({})", quote_string(name)),
        PegExpr::Recover { syncs } => {
            let list = syncs
                .iter()
                .map(|s| quote_string(s))
                .collect::<Vec<_>>()
                .join(", ");
            format!("recover({list})")
        }
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
///
/// A prefix operator (`&`, `!`, `&<`, `!<`) parses its operand with `parse_atom`
/// (no trailing postfix), so a repetition/optional operand must be parenthesised
/// — otherwise `&e*` would re-read as `(&e)*` instead of `&(e*)`.
fn prefix_operand(expr: &PegExpr) -> String {
    match expr {
        PegExpr::Sequence(_)
        | PegExpr::Choice(_)
        | PegExpr::Optional(_)
        | PegExpr::ZeroOrMore(_)
        | PegExpr::OneOrMore(_)
        | PegExpr::Repeat { .. }
        // `&x:e` would bind the `:` operand greedily on reparse — group it.
        | PegExpr::Named { .. } => format!("({})", peg_expr_to_source(expr)),
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
        match alternatives.len() {
            0 => Ok(PegExpr::Sequence(vec![])),
            1 => Ok(alternatives.remove(0)),
            _ => Ok(PegExpr::Choice(alternatives)),
        }
    }

    fn parse_sequence(&mut self) -> Result<PegExpr, ParseError> {
        let mut items = Vec::new();
        while !self.eof() {
            self.skip_whitespace();
            // `)` ends a group; `,` is an argument separator. A `/` after the
            // first item is the choice operator only when written spaced
            // (`a / b`); `/` immediately followed by regex content (`a /re/`)
            // stays a regex literal so a regex can follow another atom.
            let slash_is_choice = !items.is_empty()
                && self.peek() == Some('/')
                && self.peek_next().is_none_or(|c| c.is_whitespace());
            if matches!(self.peek(), Some(')') | Some(',')) || slash_is_choice {
                break;
            }
            if self.eof() {
                break;
            }
            items.push(self.parse_repetition()?);
        }
        Ok(match items.len() {
            1 => items.remove(0),
            _ => PegExpr::Sequence(items),
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
                Some('{') => {
                    expr = self.parse_counted_repetition(expr)?;
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    /// Parse `{m}`, `{m,}` or `{m,n}` following an atom into a `Repeat`.
    fn parse_counted_repetition(&mut self, expr: PegExpr) -> Result<PegExpr, ParseError> {
        self.consume_char(); // '{'
        self.skip_whitespace();
        let min = self.parse_usize()?;
        self.skip_whitespace();
        let max = if self.peek() == Some(',') {
            self.consume_char();
            self.skip_whitespace();
            if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                let n = self.parse_usize()?;
                if n < min {
                    return Err(ParseError::new(
                        format!("repetition upper bound {n} is less than lower bound {min}"),
                        self.offset,
                        self.offset,
                    ));
                }
                Some(n)
            } else {
                None
            }
        } else {
            Some(min)
        };
        self.skip_whitespace();
        if self.consume_char() != Some('}') {
            return Err(ParseError::new(
                "expected '}' to close counted repetition",
                self.offset,
                self.offset,
            ));
        }
        Ok(PegExpr::Repeat {
            expr: Box::new(expr),
            min,
            max,
        })
    }

    fn parse_usize(&mut self) -> Result<usize, ParseError> {
        let start = self.offset;
        let mut digits = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() {
                self.consume_char();
                digits.push(ch);
            } else {
                break;
            }
        }
        digits.parse::<usize>().map_err(|_| {
            ParseError::new(
                "expected a repetition count",
                start,
                self.offset.max(start + 1),
            )
        })
    }

    fn parse_atom(&mut self) -> Result<PegExpr, ParseError> {
        self.skip_whitespace();
        let Some(ch) = self.peek() else {
            return Ok(PegExpr::Sequence(vec![]));
        };
        match ch {
            '&' => {
                self.consume_char();
                // `&<e` → positive lookbehind; `&e` → positive lookahead.
                if self.peek() == Some('<') {
                    self.consume_char();
                    return Ok(PegExpr::LookBehind {
                        expr: Box::new(self.parse_atom()?),
                        negative: false,
                    });
                }
                Ok(PegExpr::And(Box::new(self.parse_atom()?)))
            }
            '!' => {
                self.consume_char();
                // `!<e` → negative lookbehind; `!!e` → Eager; `!e` → Not.
                if self.peek() == Some('<') {
                    self.consume_char();
                    return Ok(PegExpr::LookBehind {
                        expr: Box::new(self.parse_atom()?),
                        negative: true,
                    });
                }
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
            '"' | '\'' => self.parse_string_literal().map(PegExpr::Literal),
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
                // `@!name(e)` — semantic guard over an inner expression.
                if self.peek() == Some('!') {
                    self.consume_char();
                    let name = self.parse_ident()?;
                    self.skip_whitespace();
                    if self.consume_char() != Some('(') {
                        return Err(ParseError::new(
                            "expected '(' after @!guard name",
                            self.offset,
                            self.offset,
                        ));
                    }
                    let inner = self.parse_choice()?;
                    self.skip_whitespace();
                    if self.consume_char() != Some(')') {
                        return Err(ParseError::new(
                            "expected ')' after @!guard expression",
                            self.offset,
                            self.offset,
                        ));
                    }
                    return Ok(PegExpr::SemanticGuard {
                        name,
                        expr: Box::new(inner),
                    });
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
                // Case-insensitive literal: `i"..."` / `i'...'` written tight.
                // Compiled to a `(?i)`-anchored regex (so it is not treated as a
                // fixed-text terminal and yields the actual matched casing).
                if ident == "i" && matches!(self.peek(), Some('"') | Some('\'')) {
                    let start = self.offset;
                    let literal = self.parse_string_literal()?;
                    let pattern = format!("(?i){}", regex::escape(&literal));
                    let compiled = CompiledRegex::new(pattern, start, self.offset)?;
                    return Ok(PegExpr::Regex(compiled));
                }
                // A `(` is a builtin form or a parametric call ONLY when written
                // tight (no trivia after the name). With whitespace, the name is a
                // plain `Ref` and `( … )` is a separate grouped expression in the
                // sequence — mirroring the tight-regex rule (`a /re/` vs `a / b`).
                // Without this, `a ('c')*` would mis-parse as the call `a('c')*`.
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
                        "with_trivia" => {
                            self.consume_char();
                            self.skip_whitespace();
                            let spec = self.parse_string_literal_value()?;
                            self.skip_whitespace();
                            if self.peek() == Some(',') {
                                self.consume_char();
                            }
                            self.skip_whitespace();
                            let inner = self.parse_choice()?;
                            self.skip_whitespace();
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' after with_trivia() expression",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::WithTrivia {
                                spec,
                                expr: Box::new(inner),
                            });
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
                        "interspersed" => {
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
                                    "expected ')' after interspersed separator",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::Interspersed {
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
                            let msg = self.parse_string_literal()?;
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
                            let label = self.parse_string_literal()?;
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
                        "backref" => {
                            self.consume_char();
                            self.skip_whitespace();
                            let name = self.parse_string_literal_value()?;
                            self.skip_whitespace();
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' after backref() name",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::Backref(name));
                        }
                        "recover" => {
                            self.consume_char();
                            self.skip_whitespace();
                            let mut syncs = Vec::new();
                            loop {
                                syncs.push(self.parse_string_literal_value()?);
                                self.skip_whitespace();
                                if self.peek() == Some(',') {
                                    self.consume_char();
                                    self.skip_whitespace();
                                    continue;
                                }
                                break;
                            }
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' after recover() sync list",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::Recover { syncs });
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
                        "prec" => {
                            self.consume_char();
                            self.skip_whitespace();
                            let operand = self.parse_choice()?;
                            let mut levels = Vec::new();
                            loop {
                                self.skip_whitespace();
                                if self.peek() != Some(',') {
                                    break;
                                }
                                self.consume_char();
                                self.skip_whitespace();
                                let kw = self.parse_ident()?;
                                let fixity = match kw.as_str() {
                                    "infixl" => Fixity::InfixLeft,
                                    "infixr" => Fixity::InfixRight,
                                    "infixn" => Fixity::InfixNon,
                                    "prefix" => Fixity::Prefix,
                                    "postfix" => Fixity::Postfix,
                                    "ternary" => Fixity::Ternary,
                                    other => {
                                        return Err(ParseError::new(
                                            format!("expected infixl/infixr/infixn/prefix/postfix/ternary in prec(), got '{other}'"),
                                            self.offset,
                                            self.offset,
                                        ));
                                    }
                                };
                                self.skip_whitespace();
                                if self.consume_char() != Some('(') {
                                    return Err(ParseError::new(
                                        "expected '(' after infixl/infixr",
                                        self.offset,
                                        self.offset,
                                    ));
                                }
                                let mut operators = Vec::new();
                                self.skip_whitespace();
                                if self.peek() != Some(')') {
                                    operators.push(self.parse_choice()?);
                                    loop {
                                        self.skip_whitespace();
                                        if self.peek() != Some(',') {
                                            break;
                                        }
                                        self.consume_char();
                                        self.skip_whitespace();
                                        operators.push(self.parse_choice()?);
                                    }
                                }
                                self.skip_whitespace();
                                if self.consume_char() != Some(')') {
                                    return Err(ParseError::new(
                                        "expected ')' after infixl/infixr operators",
                                        self.offset,
                                        self.offset,
                                    ));
                                }
                                levels.push(PrecLevel { fixity, operators });
                            }
                            if self.consume_char() != Some(')') {
                                return Err(ParseError::new(
                                    "expected ')' after prec() expression",
                                    self.offset,
                                    self.offset,
                                ));
                            }
                            return Ok(PegExpr::Precedence {
                                operand: Box::new(operand),
                                levels,
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
        self.parse_string_literal()
    }

    fn parse_string_literal(&mut self) -> Result<String, ParseError> {
        let Some(quote @ ('"' | '\'')) = self.consume_char() else {
            return Err(ParseError::new(
                "expected string literal delimiter",
                self.offset,
                self.offset,
            ));
        };
        let mut value = String::new();
        while let Some(ch) = self.peek() {
            self.consume_char();
            if ch == quote {
                return Ok(value);
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
            // Prefer the native single-char class; fall back to a regex only for
            // class bodies we cannot represent as plain ranges.
            if let Some(class) = CharClass::parse_body(&inner) {
                return Ok(PegExpr::CharClass(class));
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

    /// The character one past `peek()`, without consuming.
    fn peek_next(&self) -> Option<char> {
        self.src[self.offset..].chars().nth(1)
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

    fn examined(pattern: &str, hay: &str) -> usize {
        CompiledRegex::new(pattern.to_string(), 0, 0)
            .unwrap()
            .examined_len(hay.as_bytes())
            .expect("small patterns build a DFA")
    }

    // The DFA death offset is a *sound upper bound* on the bytes a match
    // depends on (it may over-report by a byte or two). The assertions below pin
    // the soundness floor (it must read at least the bytes the engine examines)
    // and the tightness ceiling (it must not scan the whole input for bounded
    // terminals), without depending on the exact engine-internal value.

    #[test]
    fn examined_len_includes_greedy_stop_byte() {
        // `[a-z]+` reads "abc" then the '!' that stops it (≥4); it must not run
        // off to the end of the input.
        let n = examined("[a-z]+", "abc!def");
        assert!((4..=5).contains(&n), "got {n}");
        // At end-of-input there is no stop byte beyond the match.
        assert_eq!(examined("[a-z]+", "abc"), 3);
        // A single class examines the one matching byte (the all-matches DFA may
        // read one byte past it before detecting it cannot extend — still sound).
        assert!((1..=2).contains(&examined("[a-z]", "x9")));
        assert!((1..=2).contains(&examined("[a-z]", "99")));
    }

    #[test]
    fn examined_len_is_tight_for_delimited_string() {
        // A quoted string examines through the closing quote (≥5) but NOT the
        // trailing text — the negated class stops at `"`, so it never reaches the
        // end of a 10-byte input.
        let n = examined(r#""[^"\\]*""#, r#""abc" tail"#);
        assert!((5..=8).contains(&n), "got {n}");
    }

    #[test]
    fn examined_len_covers_backtracking_overshoot() {
        // `[a-z]+x` matches "abcx" (4 bytes) but greedy `[a-z]+` first reads
        // "abcxz" and the following '!' before backtracking — all of it is
        // examined (≥6). The naive "consumed + 1" heuristic would record 5 and be
        // unsound; the DFA death offset gets it right.
        let n = examined("[a-z]+x", "abcxz!");
        assert!(n >= 6, "examined must cover the backtracked bytes, got {n}");
    }

    #[test]
    fn examined_len_reports_far_scan_for_open_delimiter() {
        // An unterminated string genuinely scans to end-of-input for the close.
        assert_eq!(examined(r#""[^"\\]*""#, r#""abcdef"#), 7);
    }

    #[test]
    fn char_class_parses_ranges_singles_and_negation() {
        let c = CharClass::parse_body("a-z").unwrap();
        assert!(!c.negated && c.contains('m') && !c.contains('A'));
        let n = CharClass::parse_body("^\"\\\\").unwrap(); // [^"\\]
        assert!(n.negated && !n.contains('"') && !n.contains('\\') && n.contains('a'));
        let multi = CharClass::parse_body("A-Za-z0-9_").unwrap();
        assert!(multi.contains('Q') && multi.contains('7') && multi.contains('_'));
        assert!(!multi.contains('-'));
    }

    #[test]
    fn char_class_expands_shorthands() {
        let d = CharClass::parse_body("\\d").unwrap();
        assert!(d.contains('5') && !d.contains('a'));
        let w = CharClass::parse_body("\\w").unwrap();
        assert!(w.contains('a') && w.contains('Z') && w.contains('0') && w.contains('_'));
    }

    #[test]
    fn char_class_falls_back_for_unsupported_bodies() {
        // `\D` / `\W` / `\S` (negated shorthands) are not confidently plain
        // ranges → caller keeps a regex.
        assert!(CharClass::parse_body("\\D").is_none());
        assert!(CharClass::parse_body("\\W").is_none());
        assert!(CharClass::parse_body("a\\S").is_none());
    }

    #[test]
    fn char_class_round_trips_through_source() {
        for body in ["a-z", "^\"\\\\", "A-Za-z0-9_", "0-9", "^\\]a", "+\\-*"] {
            let class = CharClass::parse_body(body).expect("parses");
            let src = class.to_source();
            let expr = RuleTextParser::parse(&src).expect("reparses");
            match expr {
                PegExpr::CharClass(re) => assert_eq!(re, class, "round-trip mismatch for {src:?}"),
                other => panic!("expected CharClass from {src:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn with_trivia_parses_and_round_trips() {
        let expr = RuleTextParser::parse("with_trivia('whitespace', 'x')").unwrap();
        match &expr {
            PegExpr::WithTrivia { spec, expr: inner } => {
                assert_eq!(spec, "whitespace");
                assert_eq!(**inner, PegExpr::Literal("x".to_string()));
            }
            other => panic!("expected WithTrivia, got {other:?}"),
        }
        assert_eq!(
            RuleTextParser::parse(&peg_expr_to_source(&expr)).unwrap(),
            expr
        );
    }

    #[test]
    fn recover_parses_and_round_trips() {
        let expr = RuleTextParser::parse("recover(\".\", \";\")").unwrap();
        match &expr {
            PegExpr::Recover { syncs } => {
                assert_eq!(syncs, &vec![".".to_string(), ";".to_string()])
            }
            other => panic!("expected Recover, got {other:?}"),
        }
        let rendered = peg_expr_to_source(&expr);
        assert_eq!(RuleTextParser::parse(&rendered).unwrap(), expr);
    }

    #[test]
    fn bare_char_class_compiles_to_native_terminal() {
        assert!(matches!(
            RuleTextParser::parse("[a-z]").unwrap(),
            PegExpr::CharClass(_)
        ));
        // Quantified: the class is the atom, the quantifier wraps it (unchanged).
        assert!(matches!(
            RuleTextParser::parse("[a-z]+").unwrap(),
            PegExpr::OneOrMore(b) if matches!(*b, PegExpr::CharClass(_))
        ));
        // A `/regex/` literal stays a regex even when it is a single class.
        assert!(matches!(
            RuleTextParser::parse("/[a-z]/").unwrap(),
            PegExpr::Regex(_)
        ));
    }

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
    fn regex_can_follow_another_atom_in_sequence() {
        // A `/regex/` after another atom is a sequence element, not a choice.
        // `/` is the choice operator only when written spaced (`a / b`).
        let expr = RuleTextParser::parse("@?in_scope /[a-z]+/").unwrap();
        assert_eq!(
            expr,
            PegExpr::Sequence(vec![
                PegExpr::SemanticPredicate {
                    name: "in_scope".to_string()
                },
                PegExpr::Regex(CompiledRegex::new("[a-z]+".to_string(), 0, 0).unwrap()),
            ])
        );
        // Spaced choice is unchanged.
        let choice = RuleTextParser::parse("/[a-z]+/ / /[0-9]+/").unwrap();
        assert!(matches!(choice, PegExpr::Choice(_)));
    }

    #[test]
    fn invalid_regex_returns_error() {
        let err = RuleTextParser::parse("/[unclosed/");
        assert!(err.is_err());
    }

    #[test]
    fn semantic_guard_parses_and_roundtrips() {
        let expr = RuleTextParser::parse("@!even([0-9]+)").unwrap();
        match &expr {
            PegExpr::SemanticGuard { name, expr: inner } => {
                assert_eq!(name, "even");
                assert!(matches!(**inner, PegExpr::OneOrMore(_)));
            }
            other => panic!("expected SemanticGuard, got {other:?}"),
        }
        let src = peg_expr_to_source(&expr);
        assert_eq!(src, "@!even([0-9]+)");
        assert_eq!(RuleTextParser::parse(&src).unwrap(), expr);
    }

    #[test]
    fn semantic_guard_requires_parentheses() {
        assert!(RuleTextParser::parse("@!bare").is_err());
    }

    #[test]
    fn counted_repetition_parses_and_roundtrips() {
        for (src, expect_min, expect_max) in [
            ("[a]{3}", 3usize, Some(3usize)),
            ("[a]{2,}", 2, None),
            ("[a]{2,5}", 2, Some(5)),
        ] {
            let expr = RuleTextParser::parse(src).unwrap();
            match &expr {
                PegExpr::Repeat { min, max, .. } => {
                    assert_eq!(*min, expect_min);
                    assert_eq!(*max, expect_max);
                }
                other => panic!("expected Repeat for {src:?}, got {other:?}"),
            }
            // Round-trips through source.
            let rendered = peg_expr_to_source(&expr);
            assert_eq!(RuleTextParser::parse(&rendered).unwrap(), expr);
        }
    }

    #[test]
    fn counted_repetition_rejects_inverted_bounds() {
        assert!(RuleTextParser::parse("[a]{5,2}").is_err());
    }

    // Deterministic structural property test: random `PegExpr` trees must
    // survive `peg_expr_to_source` → `RuleTextParser::parse` unchanged. This
    // stresses the printer's parenthesisation / precedence handling.
    fn rng_next(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    fn gen_expr(seed: &mut u64, depth: u32) -> PegExpr {
        let leaf = |seed: &mut u64| -> PegExpr {
            match rng_next(seed) % 8 {
                0 => PegExpr::Literal("ab".to_string()),
                1 => PegExpr::Dot,
                2 => PegExpr::Ref("foo".to_string()),
                3 => PegExpr::HardKeyword("kw".to_string()),
                4 => PegExpr::Backref("n".to_string()),
                5 => PegExpr::SemanticPredicate {
                    name: "p".to_string(),
                },
                6 => PegExpr::CharClass(CharClass::parse_body("^a-z\\]").unwrap()),
                // A non-class regex still prints as `/…/` and round-trips as Regex.
                _ => CompiledRegex::new("x+y".to_string(), 0, 0)
                    .map(PegExpr::Regex)
                    .unwrap(),
            }
        };
        if depth == 0 {
            return leaf(seed);
        }
        let kid = |seed: &mut u64| Box::new(gen_expr(seed, depth - 1));
        match rng_next(seed) % 12 {
            0 => PegExpr::Sequence(vec![gen_expr(seed, depth - 1), gen_expr(seed, depth - 1)]),
            1 => PegExpr::Choice(vec![gen_expr(seed, depth - 1), gen_expr(seed, depth - 1)]),
            2 => PegExpr::Optional(kid(seed)),
            3 => PegExpr::ZeroOrMore(kid(seed)),
            4 => PegExpr::OneOrMore(kid(seed)),
            5 => PegExpr::And(kid(seed)),
            6 => PegExpr::Not(kid(seed)),
            7 => PegExpr::LookBehind {
                expr: kid(seed),
                negative: rng_next(seed).is_multiple_of(2),
            },
            8 => PegExpr::Named {
                name: "x".to_string(),
                expr: kid(seed),
            },
            9 => PegExpr::SemanticGuard {
                name: "g".to_string(),
                expr: kid(seed),
            },
            10 => PegExpr::Repeat {
                expr: kid(seed),
                min: (rng_next(seed) % 3) as usize,
                max: Some((3 + rng_next(seed) % 3) as usize),
            },
            _ => leaf(seed),
        }
    }

    #[test]
    fn fuzz_structural_roundtrip() {
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        for _ in 0..4000 {
            let expr = gen_expr(&mut seed, 4);
            let src = peg_expr_to_source(&expr);
            let reparsed = RuleTextParser::parse(&src)
                .unwrap_or_else(|e| panic!("parse failed for {src:?}: {}", e.message));
            assert_eq!(reparsed, expr, "roundtrip mismatch for source {src:?}");
        }
    }

    #[test]
    fn lookbehind_and_backref_roundtrip() {
        for src in ["&<'b'", "!<[a-z]", "backref(\"name\")"] {
            let expr = RuleTextParser::parse(src).unwrap();
            let rendered = peg_expr_to_source(&expr);
            assert_eq!(
                RuleTextParser::parse(&rendered).unwrap(),
                expr,
                "roundtrip failed for {src:?} (rendered {rendered:?})"
            );
        }
        assert!(matches!(
            RuleTextParser::parse("&<'b'").unwrap(),
            PegExpr::LookBehind {
                negative: false,
                ..
            }
        ));
        assert!(matches!(
            RuleTextParser::parse("!<'b'").unwrap(),
            PegExpr::LookBehind { negative: true, .. }
        ));
    }

    #[test]
    fn precedence_parses_and_roundtrips() {
        let expr = RuleTextParser::parse("prec(num, infixl('+', '-'), infixr('^'))").unwrap();
        match &expr {
            PegExpr::Precedence { operand, levels } => {
                assert_eq!(**operand, PegExpr::Ref("num".to_string()));
                assert_eq!(levels.len(), 2);
                assert_eq!(levels[0].fixity, Fixity::InfixLeft);
                assert_eq!(levels[0].operators.len(), 2);
                assert_eq!(levels[1].fixity, Fixity::InfixRight);
            }
            other => panic!("expected Precedence, got {other:?}"),
        }
        let rendered = peg_expr_to_source(&expr);
        assert_eq!(RuleTextParser::parse(&rendered).unwrap(), expr);
    }
}
