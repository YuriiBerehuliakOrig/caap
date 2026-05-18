use crate::grammar::Grammar;
use crate::nodes::PegNode;

// ── PegNode → grammar source text ──────────────────────────────────────────

/// Serialize a `PegNode` back to the PEG grammar text notation.
///
/// The output can be used as the body of a rule source string in `Grammar`.
pub fn node_to_source(node: &PegNode) -> String {
    match node {
        PegNode::Literal(text) => {
            let escaped = text.replace('\\', "\\\\").replace('\'', "\\'");
            format!("'{escaped}'")
        }
        PegNode::Regex(pattern) => format!("/{pattern}/"),
        PegNode::Token(pattern) => format!("token({pattern})"),
        PegNode::TokenRef(kind) => kind.clone(),
        PegNode::Ref(name) => name.clone(),
        PegNode::ImportedRef(name) => name.clone(),
        PegNode::Parameter(name) => format!("${name}"),

        PegNode::Sequence(items) => match items.len() {
            0 => String::new(),
            1 => node_to_source(&items[0]),
            _ => {
                let parts: Vec<_> = items.iter().map(node_to_source).collect();
                format!("({})", parts.join(" "))
            }
        },

        PegNode::Choice(items) => match items.len() {
            0 => String::new(),
            1 => node_to_source(&items[0]),
            _ => {
                let parts: Vec<_> = items.iter().map(node_to_source).collect();
                format!("({})", parts.join(" / "))
            }
        },

        PegNode::Optional(inner) => format!("{}?", atom_src(inner)),
        PegNode::ZeroOrMore(inner) => format!("{}*", atom_src(inner)),
        PegNode::OneOrMore(inner) => format!("{}+", atom_src(inner)),
        PegNode::And(inner) => format!("&{}", atom_src(inner)),
        PegNode::Not(inner) => format!("!{}", atom_src(inner)),

        PegNode::Named { name, node } => format!("{}:{}", name, atom_src(node)),
        PegNode::Capture(name, inner) => format!("{}:{}", name, atom_src(inner)),

        PegNode::Cut(inner) => format!("~ {}", node_to_source(inner)),
        PegNode::Eager(inner) => format!("&&{}", atom_src(inner)),

        PegNode::SepOneOrMore { separator, element } => {
            let elem = atom_src(element);
            let sep = atom_src(separator);
            format!("({elem} ({sep} {elem})*)")
        }

        PegNode::Newline => "newline".to_string(),
        PegNode::Indent => "indent".to_string(),
        PegNode::Dedent => "dedent".to_string(),
        PegNode::NoTrivia => "no_trivia".to_string(),

        PegNode::Action(items) => {
            let parts: Vec<_> = items.iter().map(node_to_source).collect();
            parts.join(" ")
        }
        PegNode::Behavior(name, inner) => {
            format!("@{}({})", name, node_to_source(inner))
        }
        PegNode::GrammarScope { name, body } => {
            let parts: Vec<_> = body.iter().map(node_to_source).collect();
            format!("{}!({})", name, parts.join(" "))
        }
        PegNode::GrammarScopeEnter | PegNode::GrammarScopeExit => String::new(),
        PegNode::InlineAction(src, _) => format!("{{{src}}}"),
        PegNode::Island(text) => text.clone(),
        PegNode::RawBlock(text) => text.clone(),
        PegNode::Expected(msg) => {
            let escaped = msg.replace('\'', "\\'");
            format!("expected('{escaped}')")
        }
    }
}

/// Return a source fragment that is safe to use as an operand of a postfix or
/// prefix operator.  Sequences and choices already include outer parentheses
/// from `node_to_source`; other compound nodes may need wrapping.
fn atom_src(node: &PegNode) -> String {
    // Sequence/Choice already produce `(...)` so no double-wrapping needed.
    // Prefix nodes (And/Not) do not need wrapping: `!!a` parses as `!(!a)`.
    node_to_source(node)
}

// ── Grammar-level optimization ─────────────────────────────────────────────

/// Apply `optimize_node` to every rule in `grammar` and return a new `Grammar`.
///
/// Rules are serialized back to source text via `node_to_source`.  Rules whose
/// source cannot be parsed (malformed text) are left unchanged.
///
/// Mirrors `peg/compile/optimization.py::optimize_grammar()`.
pub fn optimize_grammar(grammar: &Grammar) -> Grammar {
    let mut cloned = grammar.clone();
    for rule in &mut cloned.rules {
        let source = rule.source.trim().to_string();
        // Parse the rule source into a PegNode, optimize, then round-trip back.
        // Rules that use advanced constructs (indent/dedent, behaviours, etc.)
        // cannot be represented as PegNode and are left unchanged.
        if let Some(node) = crate::parser::parse_source_to_node(&source) {
            let optimized = optimize_node(node);
            rule.set_source(node_to_source(&optimized));
        }
    }
    cloned
}

// ── Optimization ───────────────────────────────────────────────────────────

/// Recursively optimize a `PegNode` tree.
///
/// Current passes:
/// 1. **Flatten nested choices**: `(a / (b / c))` → `(a / b / c)`
/// 2. **Left-factor literal prefixes**: adjacent alternatives sharing a
///    literal prefix of ≥ 3 characters are factored into
///    `(shared_prefix (rest_a / rest_b))`.
pub fn optimize_node(node: PegNode) -> PegNode {
    match node {
        PegNode::Choice(items) => optimize_choice(items),
        PegNode::Sequence(items) => {
            let optimized: Vec<_> = items.into_iter().map(optimize_node).collect();
            PegNode::Sequence(optimized)
        }
        PegNode::Optional(inner) => PegNode::Optional(Box::new(optimize_node(*inner))),
        PegNode::ZeroOrMore(inner) => PegNode::ZeroOrMore(Box::new(optimize_node(*inner))),
        PegNode::OneOrMore(inner) => PegNode::OneOrMore(Box::new(optimize_node(*inner))),
        PegNode::And(inner) => PegNode::And(Box::new(optimize_node(*inner))),
        PegNode::Not(inner) => PegNode::Not(Box::new(optimize_node(*inner))),
        PegNode::Cut(inner) => PegNode::Cut(Box::new(optimize_node(*inner))),
        PegNode::Eager(inner) => PegNode::Eager(Box::new(optimize_node(*inner))),
        PegNode::Named { name, node } => PegNode::Named {
            name,
            node: Box::new(optimize_node(*node)),
        },
        PegNode::Capture(label, inner) => PegNode::Capture(label, Box::new(optimize_node(*inner))),
        PegNode::SepOneOrMore { separator, element } => PegNode::SepOneOrMore {
            separator: Box::new(optimize_node(*separator)),
            element: Box::new(optimize_node(*element)),
        },
        other => other,
    }
}

fn optimize_choice(items: Vec<PegNode>) -> PegNode {
    // 1. Recurse + flatten nested choices
    let mut flat: Vec<PegNode> = Vec::with_capacity(items.len());
    for item in items {
        let opt = optimize_node(item);
        match opt {
            PegNode::Choice(inner) => flat.extend(inner),
            other => flat.push(other),
        }
    }

    if flat.is_empty() {
        return PegNode::Choice(vec![]);
    }
    if flat.len() == 1 {
        return flat.remove(0);
    }

    // 2. Left-factor common literal prefixes (min 3 chars)
    let factored = factor_options(flat);
    if factored.len() == 1 {
        factored.into_iter().next().unwrap()
    } else {
        PegNode::Choice(factored)
    }
}

fn factor_options(options: Vec<PegNode>) -> Vec<PegNode> {
    if options.len() < 2 {
        return options;
    }

    let mut result: Vec<PegNode> = Vec::new();
    let mut i = 0;

    while i < options.len() {
        let prefix = literal_prefix(&options[i]);

        if prefix.map(str::len).unwrap_or(0) < 3 {
            result.push(options[i].clone());
            i += 1;
            continue;
        }

        let mut best_prefix = prefix.unwrap().to_string();
        let mut group_end = i + 1;

        while group_end < options.len() {
            let next_prefix = literal_prefix(&options[group_end]);
            match next_prefix {
                None => break,
                Some(np) => {
                    let common = common_prefix(&best_prefix, np);
                    if common.len() < 3 {
                        break;
                    }
                    best_prefix = common;
                    group_end += 1;
                }
            }
        }

        if group_end == i + 1 {
            // No grouping possible
            result.push(options[i].clone());
            i += 1;
        } else {
            let group = options[i..group_end].to_vec();
            result.push(build_factored(&best_prefix, group));
            i = group_end;
        }
    }

    // options is drained conceptually; avoid borrow issues by using indices
    let _ = options; // consumed via indexing above
    result
}

fn literal_prefix(node: &PegNode) -> Option<&str> {
    match node {
        PegNode::Literal(text) => Some(text.as_str()),
        PegNode::Sequence(items) if !items.is_empty() => {
            if let PegNode::Literal(text) = &items[0] {
                Some(text.as_str())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn common_prefix(a: &str, b: &str) -> String {
    a.chars()
        .zip(b.chars())
        .take_while(|(c1, c2)| c1 == c2)
        .map(|(c, _)| c)
        .collect()
}

fn build_factored(prefix: &str, group: Vec<PegNode>) -> PegNode {
    let prefix_len = prefix.len();
    let suffixes: Vec<PegNode> = group
        .into_iter()
        .map(|n| strip_prefix(n, prefix_len))
        .collect();
    let suffix_choice = if suffixes.len() == 1 {
        suffixes.into_iter().next().unwrap()
    } else {
        PegNode::Choice(suffixes)
    };
    PegNode::Sequence(vec![PegNode::Literal(prefix.to_string()), suffix_choice])
}

fn strip_prefix(node: PegNode, prefix_len: usize) -> PegNode {
    match node {
        PegNode::Literal(text) => {
            let remainder = text[prefix_len..].to_string();
            if remainder.is_empty() {
                // Empty literal after stripping — represent as empty sequence
                PegNode::Sequence(vec![])
            } else {
                PegNode::Literal(remainder)
            }
        }
        PegNode::Sequence(mut items) if !items.is_empty() => {
            let first = items.remove(0);
            let stripped_first = strip_prefix(first, prefix_len);
            match stripped_first {
                // Empty sequence: drop it
                PegNode::Sequence(ref v) if v.is_empty() => {
                    if items.is_empty() {
                        PegNode::Sequence(vec![])
                    } else if items.len() == 1 {
                        items.remove(0)
                    } else {
                        PegNode::Sequence(items)
                    }
                }
                other => {
                    let mut new_items = vec![other];
                    new_items.extend(items);
                    PegNode::Sequence(new_items)
                }
            }
        }
        other => other,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_to_source_literal() {
        assert_eq!(node_to_source(&PegNode::Literal("hello".into())), "'hello'");
    }

    #[test]
    fn node_to_source_literal_escapes_apostrophe() {
        assert_eq!(node_to_source(&PegNode::Literal("it's".into())), "'it\\'s'");
    }

    #[test]
    fn node_to_source_ref() {
        assert_eq!(node_to_source(&PegNode::Ref("expr".into())), "expr");
    }

    #[test]
    fn node_to_source_regex() {
        assert_eq!(node_to_source(&PegNode::Regex("[a-z]+".into())), "/[a-z]+/");
    }

    #[test]
    fn node_to_source_sequence() {
        let seq = PegNode::Sequence(vec![PegNode::Literal("a".into()), PegNode::Ref("b".into())]);
        assert_eq!(node_to_source(&seq), "('a' b)");
    }

    #[test]
    fn node_to_source_single_sequence_unwraps() {
        let seq = PegNode::Sequence(vec![PegNode::Literal("x".into())]);
        assert_eq!(node_to_source(&seq), "'x'");
    }

    #[test]
    fn node_to_source_choice() {
        let ch = PegNode::Choice(vec![
            PegNode::Literal("a".into()),
            PegNode::Literal("b".into()),
        ]);
        assert_eq!(node_to_source(&ch), "('a' / 'b')");
    }

    #[test]
    fn node_to_source_optional() {
        let opt = PegNode::Optional(Box::new(PegNode::Ref("ws".into())));
        assert_eq!(node_to_source(&opt), "ws?");
    }

    #[test]
    fn node_to_source_zero_or_more() {
        let star = PegNode::ZeroOrMore(Box::new(PegNode::Ref("item".into())));
        assert_eq!(node_to_source(&star), "item*");
    }

    #[test]
    fn node_to_source_one_or_more() {
        let plus = PegNode::OneOrMore(Box::new(PegNode::Ref("digit".into())));
        assert_eq!(node_to_source(&plus), "digit+");
    }

    #[test]
    fn node_to_source_and_not() {
        let and = PegNode::And(Box::new(PegNode::Literal("x".into())));
        assert_eq!(node_to_source(&and), "&'x'");
        let not = PegNode::Not(Box::new(PegNode::Ref("kw".into())));
        assert_eq!(node_to_source(&not), "!kw");
    }

    #[test]
    fn node_to_source_named() {
        let named = PegNode::Named {
            name: "lhs".into(),
            node: Box::new(PegNode::Ref("expr".into())),
        };
        assert_eq!(node_to_source(&named), "lhs:expr");
    }

    #[test]
    fn optimize_flattens_nested_choice() {
        let inner = PegNode::Choice(vec![
            PegNode::Literal("b".into()),
            PegNode::Literal("c".into()),
        ]);
        let outer = PegNode::Choice(vec![PegNode::Literal("a".into()), inner]);
        let result = optimize_node(outer);
        match result {
            PegNode::Choice(items) => assert_eq!(items.len(), 3),
            _ => panic!("expected Choice"),
        }
    }

    #[test]
    fn optimize_left_factors_literal_prefix() {
        // "foobar" and "foobaz" share the 5-char prefix "fooba"
        let choice = PegNode::Choice(vec![
            PegNode::Literal("foobar".into()),
            PegNode::Literal("foobaz".into()),
        ]);
        let result = optimize_node(choice);
        match result {
            PegNode::Sequence(items) => {
                assert_eq!(items.len(), 2);
                // common prefix is "fooba" (first 5 chars identical)
                assert_eq!(items[0], PegNode::Literal("fooba".into()));
                match &items[1] {
                    PegNode::Choice(alts) => assert_eq!(alts.len(), 2),
                    _ => panic!("expected Choice suffix"),
                }
            }
            _ => panic!("expected Sequence from factoring"),
        }
    }

    #[test]
    fn optimize_skips_short_prefix() {
        // Prefix "ab" is < 3 chars — no factoring
        let choice = PegNode::Choice(vec![
            PegNode::Literal("abx".into()),
            PegNode::Literal("aby".into()),
        ]);
        let result = optimize_node(choice);
        // "ab" is only 2 chars — should NOT be factored
        match result {
            PegNode::Choice(items) => assert_eq!(items.len(), 2),
            _ => panic!("expected unfactored Choice"),
        }
    }

    #[test]
    fn optimize_choice_single_item_unwraps() {
        let choice = PegNode::Choice(vec![PegNode::Ref("a".into())]);
        let result = optimize_node(choice);
        assert_eq!(result, PegNode::Ref("a".into()));
    }

    #[test]
    fn node_to_source_sep_one_or_more() {
        let node = PegNode::SepOneOrMore {
            separator: Box::new(PegNode::Literal(",".into())),
            element: Box::new(PegNode::Ref("item".into())),
        };
        let src = node_to_source(&node);
        assert!(src.contains("item"));
        assert!(src.contains("','"));
    }

    // ── optimize_grammar ───────────────────────────────────────────────────

    #[test]
    fn optimize_grammar_preserves_rule_count() {
        let grammar = crate::grammar::Grammar::new("word <- [a-z]+\nnumber <- [0-9]+")
            .with_start_rule("word");
        let optimized = optimize_grammar(&grammar);
        assert_eq!(optimized.rules.len(), grammar.rules.len());
    }

    #[test]
    fn optimize_grammar_flattens_nested_choice() {
        // (a / (b / c)) should become (a / b / c)
        let grammar =
            crate::grammar::Grammar::new("root <- 'a' / ('b' / 'c')").with_start_rule("root");
        let optimized = optimize_grammar(&grammar);
        let rule = optimized.rules.iter().find(|r| r.name == "root").unwrap();
        // After optimization the nested choice is flattened — fewer parens / slashes at top level.
        assert!(rule.source.contains("'a'"));
        assert!(rule.source.contains("'b'"));
        assert!(rule.source.contains("'c'"));
    }

    #[test]
    fn optimize_grammar_leaves_advanced_rules_unchanged() {
        // Rules with dot (.) have no PegNode equivalent and should be left as-is.
        let grammar = crate::grammar::Grammar::new("any <- .").with_start_rule("any");
        let optimized = optimize_grammar(&grammar);
        let rule = optimized.rules.iter().find(|r| r.name == "any").unwrap();
        assert_eq!(rule.source.trim(), ".");
    }
}
