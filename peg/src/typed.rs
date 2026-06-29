//! Typed extraction over [`ParseValue`] — map a dynamic parse result into Rust
//! types without hand-walking `Node`/`Named`/`Text`.
//!
//! This is a consumer-side layer; it never changes the parser. The accessors
//! ([`ParseValue::text`], [`ParseValue::node`], [`ParseValue::field`]) unwrap
//! `SpannedValue` wrappers automatically, and [`ParseValue::parse_as`] converts
//! via the [`FromParseValue`] trait, accumulating a field path for clear errors.
//!
//! ```
//! use caap_peg::{ParseValue, FromParseValue};
//! use std::sync::Arc;
//!
//! // A `Node("point", [Named("x", Text "3"), Named("y", Text "4")])`.
//! let v = ParseValue::Node(
//!     "point".into(),
//!     Arc::new(vec![
//!         ParseValue::Named("x".into(), Arc::new(ParseValue::Text("3".into()))),
//!         ParseValue::Named("y".into(), Arc::new(ParseValue::Text("4".into()))),
//!     ]),
//! );
//! let x: i64 = v.field("x").unwrap().parse_as().unwrap();
//! let y: i64 = v.field("y").unwrap().parse_as().unwrap();
//! assert_eq!((x, y), (3, 4));
//! ```

use crate::types::ParseValue;

/// Error from a failed [`FromParseValue`] conversion, with a breadcrumb `path`
/// from the root value down to where the mismatch occurred.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FromParseValueError {
    /// Field/index segments from the root to the failure site (outermost first).
    /// Field/index segments from the root to the failure site (outermost first).
    pub path: Vec<String>,
    /// The failure message.
    pub message: String,
}

impl FromParseValueError {
    /// Build an error with `message` and an empty path.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            path: Vec::new(),
            message: message.into(),
        }
    }

    /// Prepend a path segment (a field name or index) as the error bubbles up.
    pub fn at(mut self, segment: impl Into<String>) -> Self {
        self.path.insert(0, segment.into());
        self
    }
}

impl std::fmt::Display for FromParseValueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "at `{}`: {}", self.path.join("."), self.message)
        }
    }
}

impl std::error::Error for FromParseValueError {}

/// Convert a [`ParseValue`] into a typed value.
pub trait FromParseValue: Sized {
    /// Convert `value` into `Self`, or return a path-annotated error.
    fn from_parse_value(value: &ParseValue) -> Result<Self, FromParseValueError>;
}

impl ParseValue {
    /// The text of a `Text` value (unwrapping any `SpannedValue`), else `None`.
    pub fn text(&self) -> Option<&str> {
        match self.inner() {
            ParseValue::Text(s) => Some(s),
            _ => None,
        }
    }

    /// `(tag, children)` of a `Node` (unwrapping any `SpannedValue`), else `None`.
    pub fn node(&self) -> Option<(&str, &[ParseValue])> {
        match self.inner() {
            ParseValue::Node(tag, children) => Some((tag, children)),
            _ => None,
        }
    }

    /// The value bound to the first `Named("name", _)` among this value's
    /// children (or this value itself when it *is* that binding). Spans are
    /// unwrapped on both the container and the bindings.
    pub fn field(&self, name: &str) -> Option<&ParseValue> {
        match self.inner() {
            ParseValue::Named(n, v) if n.as_ref() == name => Some(v),
            ParseValue::Node(_, children) => {
                children.iter().find_map(|child| match child.inner() {
                    ParseValue::Named(n, v) if n.as_ref() == name => Some(v.as_ref()),
                    _ => None,
                })
            }
            _ => None,
        }
    }

    /// Require a `field(name)`, erroring (with the name in the path) if absent.
    pub fn require(&self, name: &str) -> Result<&ParseValue, FromParseValueError> {
        self.field(name)
            .ok_or_else(|| FromParseValueError::new("missing field").at(name))
    }

    /// Convert this value into `T` via [`FromParseValue`].
    pub fn parse_as<T: FromParseValue>(&self) -> Result<T, FromParseValueError> {
        T::from_parse_value(self)
    }

    /// Convert the named field `name` into `T`, tagging any error with `name`.
    pub fn parse_field<T: FromParseValue>(&self, name: &str) -> Result<T, FromParseValueError> {
        self.require(name)?.parse_as::<T>().map_err(|e| e.at(name))
    }
}

// ── Built-in conversions ───────────────────────────────────────────────────

impl FromParseValue for ParseValue {
    fn from_parse_value(value: &ParseValue) -> Result<Self, FromParseValueError> {
        Ok(value.clone())
    }
}

impl FromParseValue for String {
    fn from_parse_value(value: &ParseValue) -> Result<Self, FromParseValueError> {
        match value.inner() {
            ParseValue::Text(s) => Ok(s.to_string()),
            ParseValue::Number(n) => Ok(n.to_string()),
            other => Err(FromParseValueError::new(format!(
                "expected text, got {}",
                value_kind(other)
            ))),
        }
    }
}

impl FromParseValue for i64 {
    fn from_parse_value(value: &ParseValue) -> Result<Self, FromParseValueError> {
        match value.inner() {
            ParseValue::Number(n) => Ok(*n),
            ParseValue::Text(s) => s
                .parse::<i64>()
                .map_err(|e| FromParseValueError::new(format!("invalid integer {s:?}: {e}"))),
            other => Err(FromParseValueError::new(format!(
                "expected integer, got {}",
                value_kind(other)
            ))),
        }
    }
}

impl FromParseValue for bool {
    fn from_parse_value(value: &ParseValue) -> Result<Self, FromParseValueError> {
        match value.text() {
            Some("true") => Ok(true),
            Some("false") => Ok(false),
            _ => Err(FromParseValueError::new("expected \"true\" or \"false\"")),
        }
    }
}

impl<T: FromParseValue> FromParseValue for Option<T> {
    fn from_parse_value(value: &ParseValue) -> Result<Self, FromParseValueError> {
        if matches!(value.inner(), ParseValue::Nil) {
            Ok(None)
        } else {
            T::from_parse_value(value).map(Some)
        }
    }
}

impl<T: FromParseValue> FromParseValue for Vec<T> {
    /// Maps the children of a `Node` (each child → `T`), tagging errors with the
    /// child index. A `Nil` is an empty list.
    fn from_parse_value(value: &ParseValue) -> Result<Self, FromParseValueError> {
        match value.inner() {
            ParseValue::Nil => Ok(Vec::new()),
            ParseValue::Node(_, children) => children
                .iter()
                .enumerate()
                .map(|(i, child)| T::from_parse_value(child).map_err(|e| e.at(format!("[{i}]"))))
                .collect(),
            other => Err(FromParseValueError::new(format!(
                "expected a node of items, got {}",
                value_kind(other)
            ))),
        }
    }
}

fn value_kind(value: &ParseValue) -> &'static str {
    match value {
        ParseValue::Nil => "nil",
        ParseValue::Text(_) => "text",
        ParseValue::Number(_) => "number",
        ParseValue::Node(..) => "node",
        ParseValue::Named(..) => "named",
        ParseValue::SpannedValue { .. } => "spanned",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn scalar_conversions() {
        assert_eq!(text("hi").parse_as::<String>().unwrap(), "hi");
        assert_eq!(text("42").parse_as::<i64>().unwrap(), 42);
        assert_eq!(ParseValue::Number(7).parse_as::<i64>().unwrap(), 7);
        assert!(text("nope").parse_as::<i64>().is_err());
        assert!(text("true").parse_as::<bool>().unwrap());
    }

    #[test]
    fn accessors_unwrap_spans_and_find_fields() {
        let v = node("p", vec![named("x", text("3")), named("y", text("4"))]).spanned(0, 5);
        assert_eq!(v.node().unwrap().0, "p");
        assert_eq!(v.field("x").unwrap().text(), Some("3"));
        assert_eq!(v.parse_field::<i64>("y").unwrap(), 4);
        assert!(v.field("z").is_none());
    }

    #[test]
    fn option_and_vec_and_error_paths() {
        assert_eq!(ParseValue::Nil.parse_as::<Option<i64>>().unwrap(), None);
        assert_eq!(text("9").parse_as::<Option<i64>>().unwrap(), Some(9));

        let list = node("items", vec![text("1"), text("2"), text("3")]);
        assert_eq!(list.parse_as::<Vec<i64>>().unwrap(), vec![1, 2, 3]);

        // Error path points at the bad child and the field that contained it.
        let bad = node("row", vec![named("nums", node("l", vec![text("ok")]))]);
        let err = bad.parse_field::<Vec<i64>>("nums").unwrap_err();
        assert_eq!(err.path, vec!["nums", "[0]"]);
        assert!(err.to_string().contains("nums.[0]"));
    }

    #[test]
    fn require_reports_missing_field() {
        let err = node("p", vec![]).require("x").unwrap_err();
        assert_eq!(err.path, vec!["x"]);
    }
}
