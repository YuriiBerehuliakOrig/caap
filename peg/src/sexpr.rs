/// S-expression grammar loader.
///
/// Parses a compact S-expression DSL into a [`Grammar`]:
///
/// ```text
/// (grammar my-grammar
///   (start root)
///   (rule root (choice (seq (lit "(") (ref expr) (lit ")")) (ref number)))
///   (rule number (regex "[0-9]+"))
///   (trivia "whitespace"))
/// ```
use std::collections::HashMap;

use serde_json::{json, Value};

use crate::grammar::{Grammar, GrammarRule, GrammarState};
use crate::registry::RegistryError;
use crate::spec_compiler::expr_to_source;

// ── S-expression value ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum SexprValue {
    Atom(String),
    List(Vec<SexprValue>),
}

impl SexprValue {
    pub fn as_atom(&self) -> Option<&str> {
        if let Self::Atom(s) = self {
            Some(s.as_str())
        } else {
            None
        }
    }

    pub fn as_list(&self) -> Option<&[SexprValue]> {
        if let Self::List(v) = self {
            Some(v.as_slice())
        } else {
            None
        }
    }

    pub fn tag(&self) -> Option<&str> {
        self.as_list()?.first()?.as_atom()
    }
}

// ── Tokenizer ──────────────────────────────────────────────────────────────

fn parse_sexpr(text: &str) -> Result<SexprValue, String> {
    let mut chars = text.chars().peekable();
    let mut values = parse_all(&mut chars)?;
    if values.len() == 1 {
        Ok(values.remove(0))
    } else if values.is_empty() {
        Err("empty s-expression input".to_string())
    } else {
        Ok(SexprValue::List(values))
    }
}

fn parse_all(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Result<Vec<SexprValue>, String> {
    let mut result = Vec::new();
    loop {
        skip_whitespace_and_comments(chars);
        match chars.peek() {
            None | Some(')') => break,
            _ => result.push(parse_one(chars)?),
        }
    }
    Ok(result)
}

fn skip_whitespace_and_comments(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    loop {
        match chars.peek() {
            Some(&c) if c.is_whitespace() => {
                chars.next();
            }
            Some(&';') => while chars.next().is_some_and(|c| c != '\n') {},
            _ => break,
        }
    }
}

fn parse_one(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<SexprValue, String> {
    skip_whitespace_and_comments(chars);
    match chars.peek() {
        Some(&'(') => {
            chars.next();
            let children = parse_all(chars)?;
            match chars.next() {
                Some(')') => Ok(SexprValue::List(children)),
                _ => Err("expected closing ')'".to_string()),
            }
        }
        Some(&'"') => {
            chars.next();
            let mut s = String::new();
            loop {
                match chars.next() {
                    None => return Err("unterminated string literal".to_string()),
                    Some('"') => break,
                    Some('\\') => match chars.next() {
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some('r') => s.push('\r'),
                        Some(c) => s.push(c),
                        None => return Err("unterminated string escape".to_string()),
                    },
                    Some(c) => s.push(c),
                }
            }
            Ok(SexprValue::Atom(format!("\"{s}\"")))
        }
        Some(_) => {
            let mut s = String::new();
            loop {
                match chars.peek() {
                    Some(&c)
                        if !c.is_whitespace() && c != '(' && c != ')' && c != '"' && c != ';' =>
                    {
                        s.push(c);
                        chars.next();
                    }
                    _ => break,
                }
            }
            if s.is_empty() {
                Err("unexpected character".to_string())
            } else {
                Ok(SexprValue::Atom(s))
            }
        }
        None => Err("unexpected end of input".to_string()),
    }
}

// ── Spec-expression converter ──────────────────────────────────────────────

/// Convert an S-expression value (rule body) to a [`serde_json::Value`] in the
/// spec-compiler format, then call `expr_to_source` to get PEG text.
pub fn sexpr_to_peg_source(v: &SexprValue) -> Result<String, String> {
    let json = sexpr_to_spec(v)?;
    expr_to_source(&json).map_err(|e| e.to_string())
}

fn sexpr_to_spec(v: &SexprValue) -> Result<Value, String> {
    match v {
        SexprValue::Atom(s) => {
            if s == "~" || s == "cut" {
                return Ok(json!("~"));
            }
            if s == "." {
                return Ok(json!(["dot"]));
            }
            // Quoted string → literal.
            if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
                let inner = &s[1..s.len() - 1];
                return Ok(json!(["lit", inner]));
            }
            // Bare identifier → rule reference.
            Ok(json!(["ref", s.as_str()]))
        }
        SexprValue::List(items) => {
            let tag = items
                .first()
                .and_then(|v| v.as_atom())
                .ok_or("list must start with a tag atom")?;
            sexpr_list_to_spec(tag, &items[1..])
        }
    }
}

fn sexpr_list_to_spec(tag: &str, args: &[SexprValue]) -> Result<Value, String> {
    match tag {
        "lit" | "literal" => {
            let text = expect_string_arg(tag, args, 0)?;
            Ok(json!(["lit", text]))
        }
        "regex" => {
            let pat = expect_string_arg(tag, args, 0)?;
            Ok(json!(["regex", pat]))
        }
        "class" | "char-class" | "char_class" => {
            let chars = expect_string_arg(tag, args, 0)?;
            Ok(json!(["class", chars]))
        }
        "ref" => {
            let name = expect_atom_arg(tag, args, 0)?;
            Ok(json!(["ref", name]))
        }
        "dot" => Ok(json!(["dot"])),
        "cut" => Ok(json!("~")),
        "seq" | "sequence" => {
            let children = args
                .iter()
                .map(sexpr_to_spec)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!(["seq", children]))
        }
        "choice" | "ordered-choice" | "ordered_choice" => {
            let children = args
                .iter()
                .map(sexpr_to_spec)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!(["choice", children]))
        }
        "opt" | "optional" | "?" => {
            let child = expect_single_child(tag, args)?;
            Ok(json!(["opt", child]))
        }
        "star" | "*" | "zero-or-more" | "zero_or_more" => {
            let child = expect_single_child(tag, args)?;
            Ok(json!(["star", child]))
        }
        "plus" | "+" | "one-or-more" | "one_or_more" => {
            let child = expect_single_child(tag, args)?;
            Ok(json!(["plus", child]))
        }
        "and" | "lookahead" => {
            let child = expect_single_child(tag, args)?;
            Ok(json!(["and", child]))
        }
        "not" | "negative-lookahead" | "negative_lookahead" => {
            let child = expect_single_child(tag, args)?;
            Ok(json!(["not", child]))
        }
        "named" | "bind" | "=" => {
            let name = expect_atom_arg(tag, args, 0)?;
            let child = sexpr_to_spec(
                args.get(1)
                    .ok_or_else(|| format!("({tag}) requires 2 args"))?,
            )?;
            Ok(json!(["named", name, child]))
        }
        "expected" => {
            let msg = expect_string_arg(tag, args, 0)?;
            let child = sexpr_to_spec(
                args.get(1)
                    .ok_or_else(|| format!("({tag}) requires 2 args"))?,
            )?;
            Ok(json!(["expected", msg, child]))
        }
        "no-trivia" | "no_trivia" | "tight" => {
            let child = expect_single_child(tag, args)?;
            Ok(json!(["tight", child]))
        }
        "sep-plus" | "sep_plus" | "gather" | "sep" => {
            let sep = sexpr_to_spec(
                args.first()
                    .ok_or_else(|| format!("({tag}) requires separator arg"))?,
            )?;
            let elem = sexpr_to_spec(
                args.get(1)
                    .ok_or_else(|| format!("({tag}) requires element arg"))?,
            )?;
            Ok(json!(["sep_plus", sep, elem]))
        }
        "token" => {
            // (token kind value) — pass through to spec compiler
            if args.len() < 2 {
                return Err("(token) requires at least 2 args".to_string());
            }
            let kind = expect_atom_arg(tag, args, 0)?;
            let value = expect_atom_or_string(tag, args, 1)?;
            Ok(json!(["token", kind, value]))
        }
        other => Err(format!("unknown SEXPR expression tag '{other}'")),
    }
}

fn expect_string_arg(tag: &str, args: &[SexprValue], idx: usize) -> Result<String, String> {
    let v = args
        .get(idx)
        .ok_or_else(|| format!("({tag}) requires argument {idx}"))?;
    let s = v
        .as_atom()
        .ok_or_else(|| format!("({tag}) argument {idx} must be a string"))?;
    // Strip surrounding quotes if present.
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        Ok(s[1..s.len() - 1].to_string())
    } else {
        Ok(s.to_string())
    }
}

fn expect_atom_arg(tag: &str, args: &[SexprValue], idx: usize) -> Result<String, String> {
    let v = args
        .get(idx)
        .ok_or_else(|| format!("({tag}) requires argument {idx}"))?;
    let s = v
        .as_atom()
        .ok_or_else(|| format!("({tag}) argument {idx} must be an atom"))?;
    Ok(s.to_string())
}

fn expect_atom_or_string(tag: &str, args: &[SexprValue], idx: usize) -> Result<String, String> {
    let s = expect_string_arg(tag, args, idx)?;
    Ok(s)
}

fn expect_single_child(tag: &str, args: &[SexprValue]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "({tag}) takes exactly one argument, got {}",
            args.len()
        ));
    }
    sexpr_to_spec(&args[0])
}

// ── Grammar loader ─────────────────────────────────────────────────────────

struct SexprGrammarLoader {
    start: Option<String>,
    rules: Vec<GrammarRule>,
    seen_names: std::collections::HashSet<String>,
    trivia: Option<String>,
    hard_keywords: Vec<String>,
    soft_keywords: Vec<String>,
    metadata: HashMap<String, HashMap<String, serde_json::Value>>,
}

impl SexprGrammarLoader {
    fn new() -> Self {
        Self {
            start: None,
            rules: Vec::new(),
            seen_names: std::collections::HashSet::new(),
            trivia: None,
            hard_keywords: Vec::new(),
            soft_keywords: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    fn load(mut self, root: &SexprValue) -> Result<Grammar, RegistryError> {
        let items = root.as_list().ok_or_else(|| {
            RegistryError::InvalidGrammar("SEXPR grammar must be a list".to_string())
        })?;

        if items.is_empty() {
            return Err(RegistryError::InvalidGrammar(
                "SEXPR grammar list is empty".to_string(),
            ));
        }

        let first_tag = items[0].as_atom().unwrap_or("");
        if first_tag != "grammar" {
            return Err(RegistryError::InvalidGrammar(format!(
                "SEXPR grammar must start with 'grammar' tag, got '{first_tag}'"
            )));
        }

        // items[1] (optional): grammar name (ignored — name is from registry)
        let entries_start = if items.get(1).is_some_and(|v| v.as_atom().is_some()) {
            2
        } else {
            1
        };

        for entry in &items[entries_start..] {
            self.apply_entry(entry)?;
        }

        self.build()
    }

    fn apply_entry(&mut self, entry: &SexprValue) -> Result<(), RegistryError> {
        let items = match entry.as_list() {
            Some(v) => v,
            None => return Ok(()), // skip bare atoms at top level
        };
        if items.is_empty() {
            return Ok(());
        }
        let tag = items[0].as_atom().unwrap_or("");
        let args = &items[1..];

        match tag {
            "start" => {
                let name = args.first().and_then(|v| v.as_atom()).ok_or_else(|| {
                    RegistryError::InvalidGrammar("(start) requires a rule name".to_string())
                })?;
                self.start = Some(name.to_string());
            }
            "rule" => self.apply_rule_entry(args)?,
            "trivia" => {
                let value = args.first().and_then(|v| v.as_atom()).ok_or_else(|| {
                    RegistryError::InvalidGrammar("(trivia) requires a value".to_string())
                })?;
                self.trivia = Some(strip_quotes(value).to_string());
            }
            "hard-keywords" | "hard_keywords" => {
                for kw in args {
                    if let Some(s) = kw.as_atom() {
                        self.hard_keywords.push(strip_quotes(s).to_string());
                    }
                }
            }
            "soft-keywords" | "soft_keywords" => {
                for kw in args {
                    if let Some(s) = kw.as_atom() {
                        self.soft_keywords.push(strip_quotes(s).to_string());
                    }
                }
            }
            "metadata" => self.apply_metadata_entry(args)?,
            // Skip entries we don't support yet.
            "imports" | "indentation" | "semantic-hooks" | "semantic_hooks" | "rule-memo"
            | "rule_memo" | "strict-actions" | "strict_actions" | "recovery" | "recover-sync"
            | "recover_sync" => {}
            other => {
                return Err(RegistryError::InvalidGrammar(format!(
                    "unsupported SEXPR grammar entry '{other}'"
                )));
            }
        }
        Ok(())
    }

    fn apply_rule_entry(&mut self, args: &[SexprValue]) -> Result<(), RegistryError> {
        if args.is_empty() {
            return Err(RegistryError::InvalidGrammar(
                "(rule) requires at least a name".to_string(),
            ));
        }

        // Header: either a bare atom `name` or a list `(name param1 param2 ...)`
        let (name, params) = parse_rule_header(&args[0])?;

        if !self.seen_names.insert(name.clone()) {
            return Err(RegistryError::InvalidGrammar(format!(
                "duplicate rule '{name}' in SEXPR grammar"
            )));
        }

        let body = args
            .get(1)
            .ok_or_else(|| RegistryError::InvalidGrammar(format!("rule '{name}' has no body")))?;

        let source = sexpr_to_peg_source(body)
            .map_err(|e| RegistryError::InvalidGrammar(format!("rule '{name}' body: {e}")))?;

        self.rules
            .push(GrammarRule::from_source(name, source, params));
        Ok(())
    }

    fn apply_metadata_entry(&mut self, args: &[SexprValue]) -> Result<(), RegistryError> {
        if args.is_empty() {
            return Ok(());
        }
        // (metadata section (key value) ...)
        let section = args[0].as_atom().ok_or_else(|| {
            RegistryError::InvalidGrammar("(metadata) requires a section name".to_string())
        })?;
        let section = strip_quotes(section).to_string();
        let table = self.metadata.entry(section).or_default();

        for kv in &args[1..] {
            if let Some(list) = kv.as_list() {
                if list.len() == 2 {
                    let key = list[0].as_atom().map(|s| strip_quotes(s).to_string());
                    let val = &list[1];
                    if let (Some(key), Some(s)) = (key, val.as_atom()) {
                        let json_val: serde_json::Value = if s.starts_with('"') {
                            json!(strip_quotes(s))
                        } else if s == "true" {
                            json!(true)
                        } else if s == "false" {
                            json!(false)
                        } else if let Ok(n) = s.parse::<i64>() {
                            json!(n)
                        } else {
                            json!(s)
                        };
                        table.insert(key, json_val);
                    }
                }
            }
        }
        Ok(())
    }

    fn build(self) -> Result<Grammar, RegistryError> {
        let start = self.start.ok_or_else(|| {
            RegistryError::InvalidGrammar("SEXPR grammar must declare (start <rule>)".to_string())
        })?;

        let text = self
            .rules
            .iter()
            .map(|r| {
                if r.params.is_empty() {
                    format!("{} <- {}", r.name, r.source)
                } else {
                    format!("{}({}) <- {}", r.name, r.params.join(", "), r.source)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Build grammar metadata.
        let mut metadata: HashMap<String, HashMap<String, serde_json::Value>> = self.metadata;
        if self.trivia.is_some() || !self.hard_keywords.is_empty() || !self.soft_keywords.is_empty()
        {
            let grammar_meta = metadata.entry("__grammar__".to_string()).or_default();
            if let Some(trivia) = self.trivia {
                grammar_meta.insert("trivia".to_string(), json!(trivia));
            }
            if !self.hard_keywords.is_empty() {
                grammar_meta.insert("hard_keywords".to_string(), json!(self.hard_keywords));
            }
            if !self.soft_keywords.is_empty() {
                grammar_meta.insert("soft_keywords".to_string(), json!(self.soft_keywords));
            }
        }

        Ok(Grammar {
            start_rule: start,
            text,
            rules: self.rules,
            metadata,
            imports: HashMap::new(),
            version: 1,
            state: GrammarState {
                sealed: false,
                analysis_state: None,
                version: 0,
            },
        })
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn parse_rule_header(v: &SexprValue) -> Result<(String, Vec<String>), RegistryError> {
    match v {
        SexprValue::Atom(s) => Ok((s.clone(), Vec::new())),
        SexprValue::List(items) => {
            let name = items
                .first()
                .and_then(|v| v.as_atom())
                .ok_or_else(|| {
                    RegistryError::InvalidGrammar("rule name must be an atom".to_string())
                })?
                .to_string();
            let params = items[1..]
                .iter()
                .map(|v| {
                    v.as_atom().map(|s| s.to_string()).ok_or_else(|| {
                        RegistryError::InvalidGrammar("rule params must be atoms".to_string())
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((name, params))
        }
    }
}

fn strip_quotes(s: &str) -> &str {
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

// ── Public API ─────────────────────────────────────────────────────────────

/// Load a grammar from an S-expression text definition.
///
/// The top-level form must be `(grammar <name> <entry>*)` where entries
/// include `(start <rule>)` and `(rule <name> <body>)`.
///
/// Rule bodies use the NodeSpec S-expression format:
/// `(seq ...)`, `(choice ...)`, `(lit "...")`, `(regex "...")`, `(ref name)`, etc.
pub fn load_grammar_from_sexpr(text: &str) -> Result<Grammar, RegistryError> {
    let root = parse_sexpr(text)
        .map_err(|e| RegistryError::InvalidGrammar(format!("SEXPR parse error: {e}")))?;
    SexprGrammarLoader::new().load(&root)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_atom() {
        let v = parse_sexpr("hello").unwrap();
        assert_eq!(v, SexprValue::Atom("hello".to_string()));
    }

    #[test]
    fn parse_quoted_string_atom() {
        let v = parse_sexpr(r#""hello world""#).unwrap();
        assert_eq!(v, SexprValue::Atom("\"hello world\"".to_string()));
    }

    #[test]
    fn parse_simple_list() {
        let v = parse_sexpr("(seq a b c)").unwrap();
        assert_eq!(
            v,
            SexprValue::List(vec![
                SexprValue::Atom("seq".into()),
                SexprValue::Atom("a".into()),
                SexprValue::Atom("b".into()),
                SexprValue::Atom("c".into()),
            ])
        );
    }

    #[test]
    fn parse_nested_list() {
        let v = parse_sexpr("(choice (lit \"a\") (ref b))").unwrap();
        assert!(matches!(v, SexprValue::List(_)));
        let items = v.as_list().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].as_atom(), Some("choice"));
    }

    #[test]
    fn parse_comment_skipped() {
        let v = parse_sexpr("; a comment\n(lit \"x\")").unwrap();
        assert_eq!(v.tag(), Some("lit"));
    }

    #[test]
    fn sexpr_to_peg_literal() {
        let v = SexprValue::List(vec![
            SexprValue::Atom("lit".into()),
            SexprValue::Atom("\"hello\"".into()),
        ]);
        let src = sexpr_to_peg_source(&v).unwrap();
        assert_eq!(src, "'hello'");
    }

    #[test]
    fn sexpr_to_peg_seq() {
        let v = SexprValue::List(vec![
            SexprValue::Atom("seq".into()),
            SexprValue::List(vec![
                SexprValue::Atom("lit".into()),
                SexprValue::Atom("\"a\"".into()),
            ]),
            SexprValue::List(vec![
                SexprValue::Atom("ref".into()),
                SexprValue::Atom("b".into()),
            ]),
        ]);
        let src = sexpr_to_peg_source(&v).unwrap();
        assert!(src.contains("'a'"), "src={src}");
        assert!(src.contains("b"), "src={src}");
    }

    #[test]
    fn sexpr_to_peg_choice() {
        let v = SexprValue::List(vec![
            SexprValue::Atom("choice".into()),
            SexprValue::List(vec![
                SexprValue::Atom("lit".into()),
                SexprValue::Atom("\"x\"".into()),
            ]),
            SexprValue::List(vec![
                SexprValue::Atom("lit".into()),
                SexprValue::Atom("\"y\"".into()),
            ]),
        ]);
        let src = sexpr_to_peg_source(&v).unwrap();
        assert!(src.contains('/'), "expected / in choice: {src}");
    }

    #[test]
    fn sexpr_bare_atom_is_ref() {
        let v = SexprValue::Atom("myRule".into());
        let src = sexpr_to_peg_source(&v).unwrap();
        assert_eq!(src, "myRule");
    }

    #[test]
    fn sexpr_bare_quoted_atom_is_literal() {
        let v = SexprValue::Atom("\"hello\"".into());
        let src = sexpr_to_peg_source(&v).unwrap();
        assert_eq!(src, "'hello'");
    }

    #[test]
    fn load_grammar_from_sexpr_basic() {
        let text = r#"
        (grammar my-grammar
          (start root)
          (rule root (choice (lit "hello") (lit "world")))
        )
        "#;
        let grammar = load_grammar_from_sexpr(text).expect("sexpr grammar loads");
        assert_eq!(grammar.start_rule, "root");
        assert_eq!(grammar.rules.len(), 1);
        let src = &grammar.rules[0].source;
        assert!(src.contains("hello"), "source: {src}");
        assert!(src.contains("world"), "source: {src}");
    }

    #[test]
    fn load_grammar_from_sexpr_with_ref() {
        let text = r#"
        (grammar calc
          (start expr)
          (rule expr (choice (ref paren) (ref number)))
          (rule paren (seq (lit "(") (ref expr) (lit ")")))
          (rule number (regex "[0-9]+"))
        )
        "#;
        let grammar = load_grammar_from_sexpr(text).expect("sexpr grammar loads");
        assert_eq!(grammar.start_rule, "expr");
        assert_eq!(grammar.rules.len(), 3);
    }

    #[test]
    fn load_grammar_from_sexpr_with_trivia() {
        let text = r#"
        (grammar ws-grammar
          (start root)
          (rule root (lit "x"))
          (trivia "whitespace")
        )
        "#;
        let grammar = load_grammar_from_sexpr(text).expect("sexpr grammar loads");
        let meta = grammar.metadata.get("__grammar__");
        assert!(meta.is_some());
        let trivia = meta.unwrap().get("trivia").and_then(|v| v.as_str());
        assert_eq!(trivia, Some("whitespace"));
    }

    #[test]
    fn load_grammar_from_sexpr_rejects_missing_start() {
        let text = "(grammar no-start (rule root (lit \"x\")))";
        assert!(load_grammar_from_sexpr(text).is_err());
    }

    #[test]
    fn load_grammar_from_sexpr_rejects_duplicate_rule() {
        let text = r#"
        (grammar dup
          (start root)
          (rule root (lit "a"))
          (rule root (lit "b"))
        )
        "#;
        assert!(load_grammar_from_sexpr(text).is_err());
    }

    #[test]
    fn load_grammar_from_sexpr_opt_star_plus() {
        let text = r#"
        (grammar quants
          (start root)
          (rule root (seq (opt (lit "a")) (star (lit "b")) (plus (lit "c"))))
        )
        "#;
        let grammar = load_grammar_from_sexpr(text).expect("quantifiers load");
        let src = &grammar.rules[0].source;
        assert!(src.contains('?'), "opt: {src}");
        assert!(src.contains('*'), "star: {src}");
        assert!(src.contains('+'), "plus: {src}");
    }
}
