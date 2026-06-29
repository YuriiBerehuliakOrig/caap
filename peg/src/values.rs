//! Helpers for building and reshaping [`ParseValue`]s: the
//! [`SequenceValueBuilder`] and span-stripping/unwrapping utilities.

use std::collections::HashMap;
use std::sync::Arc;

use crate::types::ParseValue;

/// Extract the span bounds `(start, end)` from the outermost span wrapper of a value.
///
/// Recursively merges spans from container nodes when no direct wrapper is present.
pub fn extract_span(value: &ParseValue) -> Option<(usize, usize)> {
    match value {
        ParseValue::SpannedValue { start, end, .. } => Some((*start, *end)),
        ParseValue::Node(_, items) => merge_spans(items.iter().filter_map(extract_span)),
        ParseValue::Named(_, inner) => extract_span(inner),
        _ => None,
    }
}

/// Remove all `SpannedValue` wrappers recursively, returning the bare value.
pub fn strip_spans(value: ParseValue) -> ParseValue {
    match value {
        ParseValue::SpannedValue { value, .. } => strip_spans(ParseValue::unwrap_arc(value)),
        ParseValue::Node(name, items) => ParseValue::Node(
            name,
            Arc::new(items.iter().map(|v| strip_spans(v.clone())).collect()),
        ),
        ParseValue::Named(name, inner) => {
            ParseValue::Named(name, Arc::new(strip_spans(ParseValue::unwrap_arc(inner))))
        }
        other => other,
    }
}

/// Unwrap a single top-level `SpannedValue`, returning the inner value and optional span.
pub fn unwrap_spanned(value: ParseValue) -> (ParseValue, Option<(usize, usize)>) {
    match value {
        ParseValue::SpannedValue { value, start, end } => {
            (ParseValue::unwrap_arc(value), Some((start, end)))
        }
        other => (other, None),
    }
}

/// Return `true` if the value contains any `SpannedValue` node at any depth.
pub fn contains_spanned(value: &ParseValue) -> bool {
    match value {
        ParseValue::SpannedValue { .. } => true,
        ParseValue::Node(_, items) => items.iter().any(contains_spanned),
        ParseValue::Named(_, inner) => contains_spanned(inner),
        _ => false,
    }
}

fn merge_spans(mut spans: impl Iterator<Item = (usize, usize)>) -> Option<(usize, usize)> {
    let (mut lo, mut hi) = spans.next()?;
    for (s, e) in spans {
        if s < lo {
            lo = s;
        }
        if e > hi {
            hi = e;
        }
    }
    Some((lo, hi))
}

/// Accumulates items from a sequence, tracking named bindings separately.
///
/// Ordered items plus a named-binding map for sequence parsing.
#[derive(Default)]
pub struct SequenceValueBuilder {
    items: Vec<ParseValue>,
    named: HashMap<String, ParseValue>,
}

impl SequenceValueBuilder {
    /// An empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a value.
    ///
    /// `ParseValue::Named(name, inner)` bindings are recorded in the named map
    /// **and** still appended to the item list so positional access works.
    pub fn add(&mut self, value: ParseValue) {
        if let ParseValue::Named(name, inner) = &value {
            self.named.insert(name.to_string(), inner.as_ref().clone());
        }
        self.items.push(value);
    }

    /// The recorded `name:` bindings.
    pub fn named_bindings(&self) -> &HashMap<String, ParseValue> {
        &self.named
    }

    /// Whether no items have been added.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Number of items added.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Consume the builder and return a `ParseValue`.
    ///
    /// - Empty → `Nil`
    /// - Single item → that item
    /// - Multiple items → `Node("sequence", items)`
    pub fn build(mut self) -> ParseValue {
        match self.items.len() {
            0 => ParseValue::Nil,
            1 => self.items.pop().unwrap_or(ParseValue::Nil),
            _ => ParseValue::Node("sequence".into(), Arc::new(self.items)),
        }
    }

    /// Consume the builder and return a record `ParseValue::Node("record", …)` that also
    /// embeds named bindings as explicit `ParseValue::Named` entries.
    pub fn build_record(self) -> ParseValue {
        if self.named.is_empty() {
            return self.build();
        }
        let mut record_items = vec![ParseValue::Node(
            "sequence".into(),
            Arc::new(self.items.clone()),
        )];
        for (key, value) in &self.named {
            record_items.push(ParseValue::Named(
                Arc::from(key.as_str()),
                Arc::new(value.clone()),
            ));
        }
        ParseValue::Node("record".into(), Arc::new(record_items))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ParseValue;

    #[test]
    fn extract_span_from_spanned_value() {
        let v = ParseValue::SpannedValue {
            value: Arc::new(ParseValue::Text("hi".into())),
            start: 3,
            end: 5,
        };
        assert_eq!(extract_span(&v), Some((3, 5)));
    }

    #[test]
    fn extract_span_merges_node_children() {
        let v = ParseValue::Node(
            "seq".into(),
            Arc::new(vec![
                ParseValue::SpannedValue {
                    value: Arc::new(ParseValue::Nil),
                    start: 1,
                    end: 3,
                },
                ParseValue::SpannedValue {
                    value: Arc::new(ParseValue::Nil),
                    start: 5,
                    end: 9,
                },
            ]),
        );
        assert_eq!(extract_span(&v), Some((1, 9)));
    }

    #[test]
    fn extract_span_returns_none_for_bare_value() {
        assert_eq!(extract_span(&ParseValue::Text("x".into())), None);
        assert_eq!(extract_span(&ParseValue::Nil), None);
    }

    #[test]
    fn strip_spans_removes_wrapper() {
        let v = ParseValue::SpannedValue {
            value: Arc::new(ParseValue::Text("abc".into())),
            start: 0,
            end: 3,
        };
        assert_eq!(strip_spans(v), ParseValue::Text("abc".into()));
    }

    #[test]
    fn strip_spans_recursive_in_node() {
        let inner = ParseValue::SpannedValue {
            value: Arc::new(ParseValue::Text("x".into())),
            start: 0,
            end: 1,
        };
        let v = ParseValue::Node("n".into(), Arc::new(vec![inner]));
        let stripped = strip_spans(v);
        assert_eq!(
            stripped,
            ParseValue::Node("n".into(), Arc::new(vec![ParseValue::Text("x".into())]))
        );
    }

    #[test]
    fn unwrap_spanned_extracts_span() {
        let v = ParseValue::SpannedValue {
            value: Arc::new(ParseValue::Nil),
            start: 2,
            end: 7,
        };
        let (inner, span) = unwrap_spanned(v);
        assert_eq!(inner, ParseValue::Nil);
        assert_eq!(span, Some((2, 7)));
    }

    #[test]
    fn unwrap_spanned_passthrough_for_bare() {
        let (inner, span) = unwrap_spanned(ParseValue::Text("a".into()));
        assert_eq!(inner, ParseValue::Text("a".into()));
        assert!(span.is_none());
    }

    #[test]
    fn contains_spanned_detects_nested() {
        let v = ParseValue::Node(
            "n".into(),
            Arc::new(vec![ParseValue::SpannedValue {
                value: Arc::new(ParseValue::Nil),
                start: 0,
                end: 1,
            }]),
        );
        assert!(contains_spanned(&v));
        assert!(!contains_spanned(&ParseValue::Text("x".into())));
    }

    #[test]
    fn sequence_builder_empty_gives_nil() {
        let b = SequenceValueBuilder::new();
        assert_eq!(b.build(), ParseValue::Nil);
    }

    #[test]
    fn sequence_builder_single_passthrough() {
        let mut b = SequenceValueBuilder::new();
        b.add(ParseValue::Text("hello".into()));
        assert_eq!(b.build(), ParseValue::Text("hello".into()));
    }

    #[test]
    fn sequence_builder_multiple_items_wrapped_as_node() {
        let mut b = SequenceValueBuilder::new();
        b.add(ParseValue::Text("a".into()));
        b.add(ParseValue::Text("b".into()));
        assert_eq!(b.len(), 2);
        match b.build() {
            ParseValue::Node(name, items) => {
                assert_eq!(&*name, "sequence");
                assert_eq!(items.len(), 2);
            }
            other => panic!("expected Node, got {:?}", other),
        }
    }

    #[test]
    fn sequence_builder_tracks_named_bindings() {
        let mut b = SequenceValueBuilder::new();
        b.add(ParseValue::Named(
            "foo".into(),
            Arc::new(ParseValue::Text("bar".into())),
        ));
        assert!(b.named_bindings().contains_key("foo"));
    }

    #[test]
    fn sequence_builder_ignores_magic_named_node_convention() {
        let mut b = SequenceValueBuilder::new();
        b.add(ParseValue::Node(
            "__named__:foo".into(),
            Arc::new(vec![ParseValue::Text("bar".into())]),
        ));
        assert!(b.named_bindings().is_empty());
    }

    #[test]
    fn sequence_builder_record_uses_named_variant() {
        let mut b = SequenceValueBuilder::new();
        b.add(ParseValue::Named(
            "foo".into(),
            Arc::new(ParseValue::Text("bar".into())),
        ));
        let ParseValue::Node(kind, items) = b.build_record() else {
            panic!("expected record node");
        };
        assert_eq!(&*kind, "record");
        assert!(matches!(items.get(1), Some(ParseValue::Named(name, _)) if &**name == "foo"));
    }
}
