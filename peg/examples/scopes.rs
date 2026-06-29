//! Lexical scoping / name visibility — a grammar can't express "this name is
//! only visible inside the block that declared it" on its own (PEG has no notion
//! of an environment), so the rule of visibility rides the Parse Effects
//! Protocol: a stateful `ParseDriver` keeps a stack of scopes and answers the
//! `@!declare` / `@!reference` guards as the parse walks the text.
//!
//! The toy language:
//!   let x          -- declare `x` in the current scope
//!   use x          -- reference `x`; rejected unless `x` is visible here
//!   { … }          -- a nested scope: declarations inside vanish at `}`
//!
//! Visibility = "declared in the current scope or any enclosing one". So an
//! outer name is visible inside a block, a block-local name is *not* visible
//! after the block closes, and two sibling blocks can't see each other's names.
//!
//! The two load-bearing protocol pieces:
//!   * `checkpoint`/`rollback` — keep the scope stack consistent across PEG
//!     backtracking (a half-entered block that fails must un-push its scope).
//!   * `memo_facet::Depends` — key the packrat cache by the scope state so a
//!     memoised "`use x` succeeds" can't be replayed in a scope where it doesn't.
//!
//! Run with: `cargo run --example scopes`

use std::cell::RefCell;
use std::collections::HashMap;

use caap_peg::{
    Directive, DriverCheckpoint, Grammar, MemoFacet, ParseDriver, ParseEffect, ParseRequest,
    ParseView, ParserConfig,
};

fn grammar() -> Grammar {
    // Line-based grammar; whitespace trivia so tokens may be space/newline
    // separated. Guards fire *after* their inner expression matches: `@!enter`
    // pushes a scope once `{` is consumed, `@!leave` pops it once `}` is.
    let src = r#"
prog  <- item*
item  <- decl / use_ / block
decl  <- kw("let") @!declare(name)
use_  <- kw("use") @!reference(name)
block <- @!enter('{') item* @!leave('}')
name  <- /[a-z][a-z0-9_]*/
"#;
    Grammar::trusted_new(src)
        .with_start_rule("prog")
        .with_metadata(
            "__grammar__",
            HashMap::from([("trivia".to_string(), serde_json::json!("whitespace"))]),
        )
}

/// A stack of lexical scopes; the last entry is the innermost (current) scope.
#[derive(Clone)]
struct ScopeStack {
    scopes: Vec<Vec<String>>,
}

impl Default for ScopeStack {
    fn default() -> Self {
        // One always-open global scope.
        Self {
            scopes: vec![Vec::new()],
        }
    }
}

impl ScopeStack {
    /// Visible = declared in the current scope or any enclosing one.
    fn visible(&self, name: &str) -> bool {
        self.scopes
            .iter()
            .rev()
            .any(|s| s.iter().any(|n| n == name))
    }
    /// Already declared in the *current* scope (shadowing an outer one is fine).
    fn declared_here(&self, name: &str) -> bool {
        self.scopes
            .last()
            .is_some_and(|s| s.iter().any(|n| n == name))
    }
}

#[derive(Default)]
struct ScopeDriver {
    stack: RefCell<ScopeStack>,
}

impl ScopeDriver {
    /// A fingerprint of the scope state so memo entries are scope-specific.
    fn digest(&self) -> u64 {
        let stack = self.stack.borrow();
        let mut h: u64 = 1469598103934665603;
        for scope in &stack.scopes {
            for name in scope {
                for b in name.bytes() {
                    h = (h ^ b as u64).wrapping_mul(1099511628211);
                }
                h = (h ^ 0xff).wrapping_mul(1099511628211); // name separator
            }
            h = (h ^ 0xee).wrapping_mul(1099511628211); // scope separator
        }
        h
    }
}

impl ParseDriver for ScopeDriver {
    fn handle(&self, effect: &ParseEffect<'_>, _view: &ParseView<'_>) -> Directive {
        match effect {
            // `let name`: declare in the current scope; reject a redeclaration of a
            // name already bound *in this same scope*.
            ParseEffect::Guard {
                name: "declare",
                value,
                ..
            } => {
                let Some(n) = value.text() else {
                    return Directive::Reject;
                };
                let mut stack = self.stack.borrow_mut();
                if stack.declared_here(n) {
                    return Directive::Reject; // duplicate in this scope
                }
                stack
                    .scopes
                    .last_mut()
                    .expect("a scope is open")
                    .push(n.to_string());
                Directive::Proceed
            }
            // `use name`: accept only if the name is visible from here.
            ParseEffect::Guard {
                name: "reference",
                value,
                ..
            } => match value.text() {
                Some(n) if self.stack.borrow().visible(n) => Directive::Proceed,
                _ => Directive::Reject, // undeclared / out-of-scope name
            },
            // `{` opens a fresh inner scope.
            ParseEffect::Guard { name: "enter", .. } => {
                self.stack.borrow_mut().scopes.push(Vec::new());
                Directive::Proceed
            }
            // `}` discards it — block-local names go out of scope.
            ParseEffect::Guard { name: "leave", .. } => {
                self.stack.borrow_mut().scopes.pop();
                Directive::Proceed
            }
            _ => Directive::Proceed,
        }
    }

    // Every rule whose success depends on what's visible must be memo-keyed by the
    // scope state; `name` is a pure lexical token, so it stays `Pure`.
    fn memo_facet(&self, rule: &str) -> MemoFacet {
        match rule {
            "name" => MemoFacet::Pure,
            _ => MemoFacet::Depends(self.digest()),
        }
    }

    // Keep the scope stack consistent across PEG backtracking.
    fn checkpoint(&self) -> DriverCheckpoint {
        DriverCheckpoint::of(self.stack.borrow().clone())
    }
    fn rollback(&self, snapshot: DriverCheckpoint) {
        if let Some(stack) = snapshot.take::<ScopeStack>() {
            *self.stack.borrow_mut() = stack;
        }
    }
}

fn try_parse(g: &Grammar, input: &str) {
    let driver = ScopeDriver::default();
    let cfg = ParserConfig::default().with_max_steps(64 * 1024);
    match ParseRequest::new(g).config(cfg).driver(&driver).run(input) {
        Ok(_) => println!("  OK     {input}"),
        Err(e) => println!("  REJECT {input:<24} ({})", e.message),
    }
}

fn main() {
    let g = grammar();

    println!("Accepted — every `use` names something visible here:");
    try_parse(&g, "let x use x"); // declared then used in the same scope
    try_parse(&g, "let x { use x }"); // outer name visible inside a block
    try_parse(&g, "{ let z use z }"); // block-local declare + use, same block
    try_parse(&g, "let a { let a use a }"); // inner `a` shadows the outer — fine

    println!("\nRejected — the name isn't visible at the point of use:");
    try_parse(&g, "use x"); // never declared
    try_parse(&g, "{ let y } use y"); // `y` died with its block
    try_parse(&g, "{ let p } { use p }"); // sibling blocks can't see each other
    try_parse(&g, "let q let q"); // redeclared in the same scope
}
