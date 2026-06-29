//! Scenario: **grammar text round-trips** — for a generated `PegExpr`, printing
//! it (`peg_expr_to_source`) and parsing the result back yields the same tree.
//! `parse(print(e)) == e` is the parser↔printer consistency property; a failure
//! is a real bug in one of them (this is exactly the class the `a ('c')*`
//! call-vs-grouping bug belonged to).
//!
//! The strict form (not a fixpoint) is deliberate: a consistent-but-wrong
//! parse/print pair is a *stable* fixpoint, so only `== e` catches it.

use caap_peg::expr::peg_expr_to_source;
use caap_peg::{Grammar, PegExpr};
use proptest::prelude::*;

/// Parse a PEG expression by wrapping it in a throwaway rule and extracting the
/// compiled body. `None` if the printed text does not parse at all (itself a bug).
fn parse_expr(src: &str) -> Option<PegExpr> {
    Grammar::try_new(format!("zzz <- {src}"))
        .ok()
        .and_then(|g| g.get_rule("zzz").map(|r| r.expr().clone()))
}

/// Identifiers drawn from a safe pool — never a reserved builtin/keyword
/// (`prec`, `kw`, `tok`, `newline`, …) so a bare `Ref`/`Call` name can't be
/// reinterpreted as a builtin form on the way back.
fn ident() -> impl Strategy<Value = String> {
    prop::sample::select(vec!["a", "b", "c", "foo", "bar", "baz", "item", "x", "y"])
        .prop_map(String::from)
}

/// A bounded random `PegExpr` over the parser's structural core (the region the
/// round-trip must preserve): references, literals, keywords, named bindings,
/// the quantifiers/lookahead, sequence/choice grouping, and parametric calls.
fn arb_expr() -> impl Strategy<Value = PegExpr> {
    let leaf = prop_oneof![
        ident().prop_map(PegExpr::Ref),
        prop::sample::select(vec!["x", "ab", "kw1", "q"]).prop_map(|s| PegExpr::Literal(s.into())),
        ident().prop_map(PegExpr::HardKeyword),
        ident().prop_map(PegExpr::Backref),
        ident().prop_map(|name| PegExpr::Parameter { name }),
    ];
    leaf.prop_recursive(5, 48, 4, |inner| {
        prop_oneof![
            inner.clone().prop_map(|e| PegExpr::Optional(Box::new(e))),
            inner.clone().prop_map(|e| PegExpr::ZeroOrMore(Box::new(e))),
            inner.clone().prop_map(|e| PegExpr::OneOrMore(Box::new(e))),
            inner.clone().prop_map(|e| PegExpr::And(Box::new(e))),
            inner.clone().prop_map(|e| PegExpr::Not(Box::new(e))),
            (ident(), inner.clone()).prop_map(|(name, e)| PegExpr::Named {
                name,
                expr: Box::new(e)
            }),
            prop::collection::vec(inner.clone(), 2..4).prop_map(PegExpr::Sequence),
            prop::collection::vec(inner.clone(), 2..4).prop_map(PegExpr::Choice),
            (ident(), prop::collection::vec(inner, 1..3))
                .prop_map(|(rule, args)| PegExpr::Call { rule, args }),
        ]
    })
}

/// Deterministic round-trips for the call-vs-grouping shapes specifically, so the
/// regression is pinned regardless of which trees proptest happens to draw.
#[test]
fn ref_then_group_shapes_round_trip() {
    let r = |n: &str| PegExpr::Ref(n.into());
    let seq = |v: Vec<PegExpr>| PegExpr::Sequence(v);
    let cases = [
        // a (b c)*  — a leading ref then a repeated multi-element group.
        seq(vec![
            r("a"),
            PegExpr::ZeroOrMore(Box::new(seq(vec![r("b"), r("c")]))),
        ]),
        // a (b)?    — ref then optional group.
        seq(vec![r("a"), PegExpr::Optional(Box::new(r("b")))]),
        // a (b / c)+
        seq(vec![
            r("a"),
            PegExpr::OneOrMore(Box::new(PegExpr::Choice(vec![r("b"), r("c")]))),
        ]),
        // dup(x)    — a genuine tight parametric call must stay a Call.
        PegExpr::Call {
            rule: "dup".into(),
            args: vec![r("x")],
        },
    ];
    for expr in cases {
        let printed = peg_expr_to_source(&expr);
        assert_eq!(
            parse_expr(&printed).as_ref(),
            Some(&expr),
            "round-trip failed for {printed:?}"
        );
    }
}

proptest! {
    #[test]
    fn print_then_parse_is_identity(expr in arb_expr()) {
        let printed = peg_expr_to_source(&expr);
        let reparsed = parse_expr(&printed);
        prop_assert!(reparsed.is_some(), "printer produced unparseable text: {printed:?}");
        prop_assert_eq!(
            reparsed.as_ref(),
            Some(&expr),
            "round-trip changed the tree:\n  printed = {:?}\n  reparsed = {:?}",
            printed,
            reparsed
        );
    }
}
