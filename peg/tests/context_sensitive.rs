//! End-to-end demonstration of the Parse Effects Protocol on a real
//! context-sensitive parsing problem: the classic "is this identifier a type
//! name?" ambiguity (the C "lexer hack").
//!
//! The grammar is genuinely ambiguous on its own — `foo bar ;` could be a
//! declaration ("type `foo`, variable `bar`") or nonsense. Resolution depends on
//! a *symbol table built while parsing*: `typedef foo ;` registers `foo` as a
//! type, after which `foo bar ;` parses as a declaration; without it, the same
//! text fails. This exercises the whole machine at once:
//!
//! * a stateful driver (the type table),
//! * a `@!guard` whose verdict depends on that state (semantic match),
//! * `RuleExit` mutating state (registering a type),
//! * transactional `checkpoint`/`rollback` so a rejected branch leaves no trace,
//! * facet-keyed memo so packrat stays sound under the mutating table.

use std::cell::RefCell;
use std::collections::HashSet;

use caap_peg::{
    Directive, DriverCheckpoint, Grammar, MemoFacet, ParseDriver, ParseEffect, ParseRequest,
    ParseValue, ParseView,
};

// `.` terminates a statement (the default trivia skipper treats `;` as a
// line-comment, so it would be swallowed; `.` is a clean token here).
const GRAMMAR: &str = "\
program      <- stmt+
stmt         <- typedef_stmt / decl_stmt / expr_stmt
typedef_stmt <- 'typedef' name:ident '.'
decl_stmt    <- @!is_type(ident) ident '.'
expr_stmt    <- ident '.'
ident        <- /[a-zA-Z_][a-zA-Z0-9_]*/";

/// FNV-1a digest of the type table — a faithful memo facet (changes iff the set
/// of registered type names changes).
fn types_digest(types: &HashSet<String>) -> u64 {
    let mut names: Vec<&str> = types.iter().map(String::as_str).collect();
    names.sort_unstable();
    let mut hash: u64 = 0xcbf29ce484222325;
    for name in names {
        for byte in name.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0xff; // separator
    }
    hash
}

#[derive(Default)]
struct CTypeResolver {
    types: RefCell<HashSet<String>>,
    decls_recognized: RefCell<usize>,
}

impl CTypeResolver {
    /// Snapshot payload: the whole mutable state.
    fn state(&self) -> (HashSet<String>, usize) {
        (self.types.borrow().clone(), *self.decls_recognized.borrow())
    }
}

impl ParseDriver for CTypeResolver {
    fn handle(&self, effect: &ParseEffect<'_>, view: &ParseView<'_>) -> Directive {
        match effect {
            // After a `typedef foo ;` matches, register `foo` as a type name.
            ParseEffect::RuleExit {
                rule: "typedef_stmt",
                value,
                ..
            } => {
                if let Some(name) = view_named_text(value) {
                    self.types.borrow_mut().insert(name);
                }
                Directive::Proceed
            }
            // The semantic match: a `decl_stmt` only accepts its leading ident if
            // that ident is a registered type. Otherwise reject → the `stmt`
            // choice falls through to `expr_stmt`.
            ParseEffect::Guard {
                name: "is_type",
                pos,
                end,
                ..
            } => {
                let ident = view.slice(*pos, *end).trim();
                if self.types.borrow().contains(ident) {
                    *self.decls_recognized.borrow_mut() += 1;
                    Directive::Proceed
                } else {
                    Directive::Reject
                }
            }
            _ => Directive::Proceed,
        }
    }

    // Rules whose outcome depends on the type table are memo-keyed by its digest,
    // so a packrat hit can never replay a decision made under a different table.
    fn memo_facet(&self, rule: &str) -> MemoFacet {
        match rule {
            "decl_stmt" | "stmt" | "program" => {
                MemoFacet::Depends(types_digest(&self.types.borrow()))
            }
            _ => MemoFacet::Pure,
        }
    }

    fn checkpoint(&self) -> DriverCheckpoint {
        DriverCheckpoint::of(self.state())
    }

    fn rollback(&self, snapshot: DriverCheckpoint) {
        if let Some((types, decls)) = snapshot.take::<(HashSet<String>, usize)>() {
            *self.types.borrow_mut() = types;
            *self.decls_recognized.borrow_mut() = decls;
        }
    }
}

/// Pull the `name:ident` binding's text out of a rule's matched value.
fn view_named_text(value: &ParseValue) -> Option<String> {
    value
        .named_bindings()
        .get("name")
        .and_then(|bound| match bound {
            ParseValue::Text(text) => Some(text.to_string()),
            _ => None,
        })
}

#[test]
fn typedef_makes_a_later_statement_parse_as_a_declaration() {
    let grammar = Grammar::trusted_new(GRAMMAR).with_start_rule("program");
    let driver = CTypeResolver::default();

    // `typedef foo ;` registers foo; `foo bar ;` is then a declaration; `x ;` is
    // a bare expression.
    let result = ParseRequest::new(&grammar)
        .driver(&driver)
        .run("typedef foo . foo bar . x .");

    assert!(result.is_ok(), "program should parse, got {result:?}");
    assert!(driver.types.borrow().contains("foo"));
    assert_eq!(
        *driver.decls_recognized.borrow(),
        1,
        "exactly one statement (`foo bar ;`) should resolve as a declaration"
    );
}

#[test]
fn same_text_fails_without_the_typedef() {
    let grammar = Grammar::trusted_new(GRAMMAR).with_start_rule("program");
    let driver = CTypeResolver::default();

    // Without a prior `typedef foo ;`, `foo bar ;` is neither a declaration
    // (foo is not a type) nor a valid expression statement (two idents) — so the
    // identical text now fails. That is the context sensitivity.
    let result = ParseRequest::new(&grammar)
        .driver(&driver)
        .run("foo bar . x .");

    assert!(
        result.is_err(),
        "should fail without the typedef, got {result:?}"
    );
    assert_eq!(*driver.decls_recognized.borrow(), 0);
}

#[test]
fn rejected_declaration_branch_leaves_no_state() {
    let grammar = Grammar::trusted_new(GRAMMAR).with_start_rule("program");
    let driver = CTypeResolver::default();

    // `x ;` is a bare expression. The `decl_stmt` branch is attempted first and
    // its guard rejects `x` (not a type); the speculative decl count bump made
    // inside that branch must be rolled back.
    let result = ParseRequest::new(&grammar).driver(&driver).run("x .");

    assert!(result.is_ok());
    assert_eq!(
        *driver.decls_recognized.borrow(),
        0,
        "the rejected decl branch must not leave a recognized-declaration count"
    );
}
