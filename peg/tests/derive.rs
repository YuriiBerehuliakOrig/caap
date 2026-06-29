#![cfg(feature = "derive")]
//! Tests for `#[derive(FromParseValue)]` (the `derive` feature).

use caap_peg::{FromParseValue, ParseValue};
use std::sync::Arc;

fn named(name: &str, v: ParseValue) -> ParseValue {
    ParseValue::Named(name.into(), Arc::new(v))
}
fn node(tag: &str, children: Vec<ParseValue>) -> ParseValue {
    ParseValue::Node(tag.into(), Arc::new(children))
}
fn text(s: &str) -> ParseValue {
    ParseValue::Text(s.into())
}

#[derive(FromParseValue, Debug, PartialEq)]
struct Point {
    x: i64,
    #[peg(rename = "why")]
    y: i64,
    label: Option<String>,
}

#[test]
fn derives_struct_with_rename_and_option() {
    let v = node(
        "point",
        vec![
            named("x", text("3")),
            named("why", text("4")),
            named("label", text("origin")),
        ],
    );
    let p: Point = v.parse_as().unwrap();
    assert_eq!(
        p,
        Point {
            x: 3,
            y: 4,
            label: Some("origin".to_string())
        }
    );
}

#[test]
fn struct_missing_field_errors_with_path() {
    let v = node(
        "point",
        vec![named("x", text("3")), named("why", text("4"))],
    );
    // `label` is Option, so its *absence* is an error (the field binding is
    // required even though the type is Option) — documents the contract.
    let err = v.parse_as::<Point>().unwrap_err();
    assert!(err.to_string().contains("label"), "{err}");
}

#[derive(FromParseValue, Debug, PartialEq)]
struct Bin {
    op: String,
}

#[derive(FromParseValue, Debug, PartialEq)]
enum Expr {
    #[peg(tag = "bin")]
    Bin(Bin),
    #[peg(tag = "nil")]
    Nil,
}

#[derive(FromParseValue, Debug, PartialEq)]
struct Token {
    #[peg(text)]
    value: String,
}

#[derive(FromParseValue, Debug, PartialEq)]
struct Opts {
    name: String,
    #[peg(default)]
    count: i64,
    #[peg(default)]
    flag: bool,
}

#[test]
fn text_attr_maps_the_nodes_own_value() {
    let t: Token = text("hello").parse_as().unwrap();
    assert_eq!(t.value, "hello");
}

#[test]
fn default_attr_fills_absent_bindings() {
    // Only `name` present → count/flag take their Default.
    let v = node("opts", vec![named("name", text("x"))]);
    assert_eq!(
        v.parse_as::<Opts>().unwrap(),
        Opts {
            name: "x".into(),
            count: 0,
            flag: false
        }
    );
    // Present bindings are parsed normally.
    let v2 = node(
        "opts",
        vec![
            named("name", text("x")),
            named("count", text("7")),
            named("flag", text("true")),
        ],
    );
    assert_eq!(
        v2.parse_as::<Opts>().unwrap(),
        Opts {
            name: "x".into(),
            count: 7,
            flag: true
        }
    );
}

#[test]
fn derives_enum_by_node_tag() {
    let bin = node("bin", vec![named("op", text("+"))]);
    assert_eq!(
        bin.parse_as::<Expr>().unwrap(),
        Expr::Bin(Bin { op: "+".into() })
    );

    let nil = node("nil", vec![]);
    assert_eq!(nil.parse_as::<Expr>().unwrap(), Expr::Nil);

    let unknown = node("loop", vec![]);
    assert!(unknown.parse_as::<Expr>().is_err());
}
