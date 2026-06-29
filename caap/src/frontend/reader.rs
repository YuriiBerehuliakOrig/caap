//! Segmental-reader **mechanism**: read top-level forms one at a time, consulting
//! a set of [`ReaderDirective`]s that may reshape how the *following* forms are
//! read (grammar replacement, named grammars, scoped grammar regions).
//!
//! The loop here is pure mechanism — it knows nothing about specific directives.
//! The directive *set* is policy: [`default_reader_directives`] supplies the
//! built-in ones, but a caller can pass its own list to [`read_segmental`], so
//! the reader is an explicit, swappable extension point (principles #4/#6/#14).
//! Directives are recognised by inspection (no evaluation), which keeps the read
//! phase pure so the assembled graph stays whole-program.
use std::collections::HashMap;

use caap_peg::Grammar;

use crate::error::{CaapError, CaapResult};
use crate::graph::IRGraph;

use super::{parse_next_form, ParsedForm, ParsedSource};

/// Mutable state threaded through a segmental read. Directives drive a read by
/// calling its methods; the loop owns it and never inspects directive specifics.
pub struct ReaderState {
    active: Grammar,
    registry: HashMap<String, Grammar>,
    scope_stack: Vec<Grammar>,
}

impl ReaderState {
    fn new() -> Self {
        Self {
            active: base_reader_grammar(),
            registry: HashMap::new(),
            scope_stack: Vec::new(),
        }
    }

    /// Replace `rule` in the live (active) grammar — `extend_syntax` semantics.
    pub fn extend_active_rule(&mut self, rule: &str, source: &str) -> CaapResult<()> {
        replace_grammar_rule(&mut self.active, rule, source)
    }

    /// Register or extend a named grammar (a base clone with rule overrides).
    pub fn define_named_grammar(
        &mut self,
        name: String,
        rule: &str,
        source: &str,
    ) -> CaapResult<()> {
        let grammar = self
            .registry
            .entry(name)
            .or_insert_with(base_reader_grammar);
        replace_grammar_rule(grammar, rule, source)
    }

    /// Push the active grammar and switch to the named grammar (`begin_scope`).
    pub fn enter_scope(&mut self, name: &str) -> CaapResult<()> {
        let scoped = self
            .registry
            .get(name)
            .ok_or_else(|| CaapError::parse(format!("begin_scope: unknown grammar {name:?}")))?
            .clone();
        self.scope_stack
            .push(std::mem::replace(&mut self.active, scoped));
        Ok(())
    }

    /// Pop back to the grammar in effect before the matching `begin_scope`.
    pub fn leave_scope(&mut self) -> CaapResult<()> {
        self.active = self
            .scope_stack
            .pop()
            .ok_or_else(|| CaapError::parse("end_scope without a matching begin_scope"))?;
        Ok(())
    }

    fn finish(&self) -> CaapResult<()> {
        if self.scope_stack.is_empty() {
            Ok(())
        } else {
            Err(CaapError::parse(
                "unbalanced begin_scope: missing end_scope",
            ))
        }
    }
}

/// A reader directive: a top-level form recognised at read time that mutates the
/// reader state instead of becoming program code. Implement this (plus a trigger
/// token) to add a directive without touching the read loop.
pub trait ReaderDirective {
    /// A token that must appear in the source for this directive to be possible.
    /// When no directive's token appears, the reader takes a whole-file fast path.
    fn trigger_token(&self) -> &'static str;

    /// Try to consume `form`: `Ok(true)` if it was this directive (state mutated),
    /// `Ok(false)` if `form` is not this directive, `Err` if it was but invalid.
    fn apply(&self, form: &ParsedForm, state: &mut ReaderState) -> CaapResult<bool>;
}

/// The built-in reader directives: `extend_syntax`, `define_grammar`,
/// `begin_scope`, `end_scope`.
pub fn default_reader_directives() -> Vec<Box<dyn ReaderDirective>> {
    vec![
        Box::new(ExtendSyntax),
        Box::new(DefineGrammar),
        Box::new(BeginScope),
        Box::new(EndScope),
    ]
}

/// Read `source` segmentally with `directives` and lower the collected forms to
/// one whole-program IR graph. The mechanism behind [`super::parse_segmental`].
pub fn read_segmental(
    source: &str,
    source_path: Option<&str>,
    directives: &[Box<dyn ReaderDirective>],
) -> CaapResult<IRGraph> {
    let mut state = ReaderState::new();
    let mut forms: Vec<ParsedForm> = Vec::new();
    let mut pos = 0usize;
    while let Some(step) = parse_next_form(&state.active, source, pos, source_path)? {
        let mut consumed = false;
        for directive in directives {
            if directive.apply(&step.form, &mut state)? {
                consumed = true;
                break;
            }
        }
        if !consumed {
            forms.push(step.form);
        }
        pos = step.next_pos;
    }
    state.finish()?;
    super::parsed_source_to_ir(&ParsedSource { forms })
}

/// Whether any directive's trigger token appears in `source` (the fast-path gate:
/// when none can occur, the caller parses whole-file).
pub fn any_directive_possible(source: &str, directives: &[Box<dyn ReaderDirective>]) -> bool {
    directives
        .iter()
        .any(|directive| source.contains(directive.trigger_token()))
}

// ── built-in directives ─────────────────────────────────────────────────────

struct ExtendSyntax;
impl ReaderDirective for ExtendSyntax {
    fn trigger_token(&self) -> &'static str {
        "extend_syntax"
    }
    fn apply(&self, form: &ParsedForm, state: &mut ReaderState) -> CaapResult<bool> {
        let Some([head, rule, source]) = list_items(form) else {
            return Ok(false);
        };
        let (Some("extend_syntax"), Some(rule), Some(source)) =
            (symbol(head), string(rule), string(source))
        else {
            return Ok(false);
        };
        state.extend_active_rule(rule, source)?;
        Ok(true)
    }
}

struct DefineGrammar;
impl ReaderDirective for DefineGrammar {
    fn trigger_token(&self) -> &'static str {
        "define_grammar"
    }
    fn apply(&self, form: &ParsedForm, state: &mut ReaderState) -> CaapResult<bool> {
        let Some([head, name, rule, source]) = list_items(form) else {
            return Ok(false);
        };
        let (Some("define_grammar"), Some(name), Some(rule), Some(source)) =
            (symbol(head), string(name), string(rule), string(source))
        else {
            return Ok(false);
        };
        state.define_named_grammar(name.to_string(), rule, source)?;
        Ok(true)
    }
}

struct BeginScope;
impl ReaderDirective for BeginScope {
    fn trigger_token(&self) -> &'static str {
        "begin_scope"
    }
    fn apply(&self, form: &ParsedForm, state: &mut ReaderState) -> CaapResult<bool> {
        let Some([head, name]) = list_items(form) else {
            return Ok(false);
        };
        let (Some("begin_scope"), Some(name)) = (symbol(head), string(name)) else {
            return Ok(false);
        };
        state.enter_scope(name)?;
        Ok(true)
    }
}

struct EndScope;
impl ReaderDirective for EndScope {
    fn trigger_token(&self) -> &'static str {
        "end_scope"
    }
    fn apply(&self, form: &ParsedForm, state: &mut ReaderState) -> CaapResult<bool> {
        let Some([head]) = list_items(form) else {
            return Ok(false);
        };
        if symbol(head) != Some("end_scope") {
            return Ok(false);
        }
        state.leave_scope()?;
        Ok(true)
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn base_reader_grammar() -> Grammar {
    let mut grammar = super::grammar::surface_grammar().clone();
    grammar.thaw(); // root grammar is sealed; allow rule replacement on our copy
    grammar
}

fn replace_grammar_rule(grammar: &mut Grammar, rule: &str, source: &str) -> CaapResult<()> {
    caap_peg::replace_rule(grammar, rule, source).map_err(|error| {
        CaapError::parse(format!("cannot replace grammar rule {rule:?}: {error:?}"))
    })
}

/// Borrow a list form's items as a fixed-size array `[ParsedForm; N]` (the
/// directive shape), or `None` if `form` is not a list of exactly `N` items.
fn list_items<const N: usize>(form: &ParsedForm) -> Option<&[ParsedForm; N]> {
    match form {
        ParsedForm::List { items, .. } => items.as_slice().try_into().ok(),
        _ => None,
    }
}

fn symbol(form: &ParsedForm) -> Option<&str> {
    match form {
        ParsedForm::Symbol { text, .. } => Some(text.as_str()),
        _ => None,
    }
}

fn string(form: &ParsedForm) -> Option<&str> {
    match form {
        ParsedForm::String { value, .. } => Some(value.as_str()),
        _ => None,
    }
}
