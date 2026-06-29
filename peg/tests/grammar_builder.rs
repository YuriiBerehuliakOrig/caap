//! Scenario: the programmatic **`GrammarBuilder`** DSL produces a real,
//! parseable grammar — terminals, choice/sequence, and recursive references
//! built from `PegExpr` helpers parse input end-to-end.

use caap_peg::builder::{char_class, choice, lit, plus, rule_ref, seq, GrammarBuilder};
use caap_peg::ParseValue;

#[test]
fn builder_constructs_and_parses() {
    let grammar = GrammarBuilder::new()
        .start("word")
        .rule("word", plus(char_class("a-z").unwrap()))
        .build();
    assert_eq!(grammar.start_rule, "word");
    assert_eq!(grammar.rule_count(), 1);
    // The built grammar must parse and return a non-nil value.
    let val = caap_peg::parse("hello", &grammar).unwrap();
    assert!(!matches!(val, ParseValue::Nil));
}

#[test]
fn builder_choice_and_seq() {
    let grammar = GrammarBuilder::new()
        .start("root")
        .rule(
            "root",
            choice(vec![
                seq(vec![lit("("), rule_ref("root"), lit(")")]),
                plus(char_class("0-9").unwrap()),
            ]),
        )
        .build();
    // Nested parens + digits should parse successfully.
    let val = caap_peg::parse("(42)", &grammar).unwrap();
    assert!(!matches!(val, ParseValue::Nil));
}
