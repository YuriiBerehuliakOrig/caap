//! Context-dependent parsing: the "name → meta → value" chain, where each stage
//! selects how the next stage is parsed.
//!
//! Three statement shapes share one dispatch:
//!   var int const = 5u            -- name `var`; metas pick the value grammar
//!   type Point struct { x u32 y u32 }  -- name `type`; `struct` picks the body
//!   a = b + c                     -- name (ident); `=` picks an expression
//!
//! The top-level dispatch (which statement) is plain ordered choice — the
//! *leading keyword* statically selects a sub-grammar. The genuinely
//! context-SENSITIVE part is the `var` declaration: the parsed type meta
//! (`int`/`str`/`bool`) chooses the value grammar at runtime, and qualifiers are
//! validated against the declaration. That contextual flow can't be expressed in
//! static PEG, so it rides the Parse Effects Protocol: a stateful `ParseDriver`
//! records the type/qualifiers and answers `@!set_type` / `@!qual_ok` guards and
//! `@?ty_is_*` predicates. `checkpoint`/`rollback` keep that state correct across
//! PEG backtracking; `memo_facet` keeps the packrat cache sound.
//!
//! Run with: `cargo run --example context_dependent`

use std::cell::RefCell;
use std::collections::HashMap;

use caap_peg::{
    Directive, DriverCheckpoint, Grammar, MemoFacet, ParseDriver, ParseEffect, ParseRequest,
    ParseView, ParserConfig,
};

fn grammar() -> Grammar {
    // `kw(...)` keywords match on word boundaries; `'='`/`'{'` are literals.
    // Whitespace trivia is enabled so tokens may be space/newline separated.
    // One rule per line (the grammar is line-based).
    let src = r#"
stmt       <- decl / typedef / assign
# 1) DECLARATION — name `var`, a type meta + qualifiers, then a type-selected value.
decl       <- kw("var") @!set_type(word) (@!qual_ok(word))* '=' value
value      <- @?ty_is_int int_lit / @?ty_is_bool bool_lit / @?ty_is_str str_lit
int_lit    <- /[0-9]+/ ('u' / 'i')?
bool_lit   <- kw("true") / kw("false")
str_lit    <- '"' /[^"]*/ '"'
# 2) TYPE DEFINITION — name `type`, a TypeName meta, a struct/enum meta that picks the body.
typedef    <- kw("type") typename structkind '{' field* '}'
structkind <- kw("struct") / kw("enum")
field      <- word word
typename   <- /[A-Z][A-Za-z0-9_]*/
# 3) ASSIGNMENT — name (ident), `=` meta, an arithmetic value via precedence.
assign     <- word '=' expr
expr       <- prec(operand, infixl("+", "-"), infixl("*", "/"))
operand    <- word / /[0-9]+/
word       <- /[a-z][a-z0-9_]*/
"#;
    Grammar::trusted_new(src)
        .with_start_rule("stmt")
        .with_metadata(
            "__grammar__",
            HashMap::from([("trivia".to_string(), serde_json::json!("whitespace"))]),
        )
}

/// The host context for a `var` declaration: the chosen type and qualifiers.
#[derive(Clone, Default)]
struct DeclCtx {
    ty: Option<String>,
    quals: Vec<String>,
}

#[derive(Default)]
struct DeclDriver {
    ctx: RefCell<DeclCtx>,
}

impl DeclDriver {
    fn known_type(t: &str) -> bool {
        matches!(t, "int" | "bool" | "str")
    }
    fn legal_qualifier(q: &str) -> bool {
        matches!(q, "const" | "mut")
    }
    fn digest(&self) -> u64 {
        // Cheap state fingerprint so memo can't replay a decision made under a
        // different (ty, quals).
        let c = self.ctx.borrow();
        let mut h: u64 = 1469598103934665603;
        for b in
            c.ty.as_deref()
                .unwrap_or("")
                .bytes()
                .chain(c.quals.join(",").bytes())
        {
            h = (h ^ b as u64).wrapping_mul(1099511628211);
        }
        h
    }
}

impl ParseDriver for DeclDriver {
    fn handle(&self, effect: &ParseEffect<'_>, _view: &ParseView<'_>) -> Directive {
        match effect {
            // The type meta: validate it's a known type, then record it (resetting
            // the qualifier list for this fresh declaration).
            ParseEffect::Guard {
                name: "set_type",
                value,
                ..
            } => match value.text() {
                Some(t) if Self::known_type(t) => {
                    *self.ctx.borrow_mut() = DeclCtx {
                        ty: Some(t.to_string()),
                        quals: Vec::new(),
                    };
                    Directive::Proceed
                }
                _ => Directive::Reject, // unknown type → the decl branch fails
            },
            // Each qualifier: legal for a `var`, and not a duplicate.
            ParseEffect::Guard {
                name: "qual_ok",
                value,
                ..
            } => match value.text() {
                Some(q)
                    if Self::legal_qualifier(q)
                        && !self.ctx.borrow().quals.iter().any(|x| x == q) =>
                {
                    self.ctx.borrow_mut().quals.push(q.to_string());
                    Directive::Proceed
                }
                _ => Directive::Reject, // illegal/duplicate qualifier → stop the list
            },
            // The value grammar is chosen by the recorded type meta.
            ParseEffect::SemanticPredicate { name, .. } => {
                let want = match *name {
                    "ty_is_int" => "int",
                    "ty_is_bool" => "bool",
                    "ty_is_str" => "str",
                    _ => return Directive::Proceed,
                };
                if self.ctx.borrow().ty.as_deref() == Some(want) {
                    Directive::Proceed
                } else {
                    Directive::Reject
                }
            }
            _ => Directive::Proceed,
        }
    }

    // Decisions in these rules depend on the declaration context, so memo entries
    // are keyed by its digest — a packrat hit can't replay a stale decision.
    fn memo_facet(&self, rule: &str) -> MemoFacet {
        match rule {
            "stmt" | "decl" | "value" => MemoFacet::Depends(self.digest()),
            _ => MemoFacet::Pure,
        }
    }

    // Keep the host state consistent across PEG backtracking.
    fn checkpoint(&self) -> DriverCheckpoint {
        DriverCheckpoint::of(self.ctx.borrow().clone())
    }
    fn rollback(&self, snapshot: DriverCheckpoint) {
        if let Some(ctx) = snapshot.take::<DeclCtx>() {
            *self.ctx.borrow_mut() = ctx;
        }
    }
}

fn try_parse(g: &Grammar, input: &str) {
    let driver = DeclDriver::default();
    let cfg = ParserConfig::default().with_max_steps(64 * 1024);
    match ParseRequest::new(g).config(cfg).driver(&driver).run(input) {
        Ok(_) => println!("  OK     {input}"),
        Err(e) => println!("  REJECT {input:<34} ({})", e.message),
    }
}

fn main() {
    let g = grammar();

    println!("Accepted — name → meta → value chains:");
    try_parse(&g, "var int const = 5u"); // type int → integer value; const qualifier
    try_parse(&g, "var str = \"hi\""); // type str → string value
    try_parse(&g, "var bool mut = true"); // type bool → boolean value
    try_parse(&g, "type Point struct { x u32 y u32 }"); // `struct` → field body
    try_parse(&g, "a = b + c"); // ident `=` → arithmetic expression

    println!("\nRejected — the context forbids it:");
    try_parse(&g, "var int = \"hi\""); // type int rejects a string value
    try_parse(&g, "var int str = 5"); // two types: `str` isn't a legal qualifier
    try_parse(&g, "var color = 5"); // `color` is not a known type
}
