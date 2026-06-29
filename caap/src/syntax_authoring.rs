//! Syntax-authoring DSL support for CAAP surface grammars.
//!
//! This mirrors the supportable CAAP surface syntax-authoring
//! behavior at the UnitSyntaxState boundary: grammar-authoring source is
//! compiled into semantic grammar rule specs plus metadata, then applied to a
//! unit syntax state.

use crate::error::{CaapError, CaapResult};
use crate::semantic::SemanticValue;
use crate::unit::UnitSyntaxState;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq)]
pub struct AuthoringRuleOp {
    pub name: String,
    pub expr: SemanticValue,
    pub metadata: Option<SemanticValue>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AuthoringOp {
    AddRule(AuthoringRuleOp),
    ReplaceRule(AuthoringRuleOp),
    IncludeGrammar {
        source: String,
        prefix: Option<String>,
    },
    /// `set <key> = "<value>"` / `set <key> = none` — grammar-level metadata
    /// directives. The canonical consumer is `set comment = "//"`: the
    /// grammar's line-comment convention (trivia), with `none` disabling
    /// comments entirely (whitespace-only trivia). The kernel default (`;`
    /// comments) stays when the directive is absent.
    SetMetadata {
        key: String,
        value: Option<String>,
    },
}

pub fn compile_authoring_grammar_source(source: &str) -> CaapResult<Vec<AuthoringOp>> {
    let mut ops = Vec::new();
    for (line_index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        let mut parser = Parser::new(trimmed);
        let op = parser.parse_statement().map_err(|error| {
            CaapError::parse(format!("syntax authoring line {}: {error}", line_index + 1))
        })?;
        parser.expect_end().map_err(|error| {
            CaapError::parse(format!("syntax authoring line {}: {error}", line_index + 1))
        })?;
        ops.push(op);
    }
    Ok(ops)
}

pub fn apply_authoring_grammar_source(
    syntax: &mut UnitSyntaxState,
    source: &str,
) -> CaapResult<()> {
    for op in compile_authoring_grammar_source(source)? {
        apply_authoring_op(syntax, op)?;
    }
    Ok(())
}

pub fn define_authoring_syntax_rule(
    syntax: &mut UnitSyntaxState,
    source: &str,
    function_name: &str,
) -> CaapResult<()> {
    if function_name.is_empty() {
        return Err(CaapError::parse(
            "syntax rule function name must be non-empty",
        ));
    }
    if source.contains("->") {
        return Err(CaapError::parse(
            "syntax rule source must not include an inline semantic hook",
        ));
    }
    let authoring_source = format!("{} -> {function_name}", source.trim_end());
    let mut hook_refs = Vec::new();
    for op in compile_authoring_grammar_source(&authoring_source)? {
        if let Some(metadata) = authoring_rule_metadata(&op) {
            hook_refs.extend(semantic_hook_references(metadata)?);
        }
        apply_authoring_op(syntax, op)?;
    }
    if hook_refs.len() != 1 {
        return Err(CaapError::parse(
            "define-syntax-rule expects exactly one semantic hook in the authoring rule",
        ));
    }
    syntax_set_hook_function(syntax, &hook_refs[0], function_name)
}

pub fn define_authoring_syntax_rule_inline_source(
    syntax: &mut UnitSyntaxState,
    source: &str,
    implementation_source: &str,
) -> CaapResult<String> {
    if source.contains("->") {
        return Err(CaapError::parse(
            "syntax rule source must not include an inline semantic hook",
        ));
    }
    let hook_ref = inline_hook_ref(source, implementation_source);
    let authoring_source = format!("{} -> {hook_ref}", source.trim_end());
    let mut hook_refs = Vec::new();
    for op in compile_authoring_grammar_source(&authoring_source)? {
        if let Some(metadata) = authoring_rule_metadata(&op) {
            hook_refs.extend(semantic_hook_references(metadata)?);
        }
        apply_authoring_op(syntax, op)?;
    }
    if hook_refs != [hook_ref.as_str()] {
        return Err(CaapError::semantic(
            "define-syntax-rule inline hook produced unexpected semantic hook metadata",
        ));
    }
    set_inline_syntax_hook_source(syntax, &hook_ref, implementation_source)?;
    Ok(hook_ref)
}

pub fn extract_inline_lambda_source(source: &str) -> CaapResult<&str> {
    let trimmed = source.trim();
    if trimmed.starts_with("(lambda") && trimmed.ends_with(')') {
        return Ok(trimmed);
    }
    Err(CaapError::parse(
        "inline syntax implementation node must be a lambda form",
    ))
}

fn apply_authoring_op(syntax: &mut UnitSyntaxState, op: AuthoringOp) -> CaapResult<()> {
    match op {
        AuthoringOp::AddRule(op) | AuthoringOp::ReplaceRule(op) => {
            let name = op.name;
            if let Some(metadata) = op.metadata {
                syntax.set_grammar_metadata(name.clone(), metadata)?;
            }
            syntax.set_grammar_rule(name, op.expr)?;
            Ok(())
        }
        AuthoringOp::IncludeGrammar { .. } => Err(CaapError::parse(
            "syntax authoring include-grammar operations are not applied at unit scope",
        )),
        AuthoringOp::SetMetadata { key, value } => {
            // `none` is stored as the empty string: "directive present,
            // feature disabled" must stay distinguishable from "no directive".
            syntax.set_grammar_metadata(key, SemanticValue::Str(value.unwrap_or_default()))?;
            Ok(())
        }
    }
}

fn authoring_rule_metadata(op: &AuthoringOp) -> Option<&SemanticValue> {
    match op {
        AuthoringOp::AddRule(op) | AuthoringOp::ReplaceRule(op) => op.metadata.as_ref(),
        AuthoringOp::IncludeGrammar { .. } | AuthoringOp::SetMetadata { .. } => None,
    }
}

fn semantic_hook_references(metadata: &SemanticValue) -> CaapResult<Vec<String>> {
    let SemanticValue::Map(entries) = metadata else {
        return Ok(Vec::new());
    };
    let Some((_, hooks)) = entries.iter().find(|(key, _)| key == "semantic_hooks") else {
        return Ok(Vec::new());
    };
    let SemanticValue::List(hooks) = hooks else {
        return Err(CaapError::semantic(
            "semantic_hooks metadata must be a sequence",
        ));
    };
    let mut refs = Vec::with_capacity(hooks.len());
    for entry in hooks {
        let SemanticValue::List(pair) = entry else {
            return Err(CaapError::semantic("semantic_hooks entries must be pairs"));
        };
        let [_, SemanticValue::Str(hook_ref)] = pair.as_slice() else {
            return Err(CaapError::semantic(
                "semantic_hooks entries must contain a string hook reference",
            ));
        };
        refs.push(hook_ref.clone());
    }
    Ok(refs)
}

fn syntax_set_hook_function(
    syntax: &mut UnitSyntaxState,
    hook_ref: &str,
    function_name: &str,
) -> CaapResult<()> {
    let hooks = syntax
        .grammar_metadata
        .get("semantic_hook_functions")
        .cloned()
        .unwrap_or_else(|| SemanticValue::Map(Vec::new()));
    let mut entries = match hooks {
        SemanticValue::Map(entries) => entries,
        _ => Vec::new(),
    };
    entries.retain(|(key, _)| key != hook_ref);
    entries.push((
        hook_ref.to_string(),
        SemanticValue::Str(function_name.to_string()),
    ));
    syntax.set_grammar_metadata("semantic_hook_functions", SemanticValue::Map(entries))?;
    Ok(())
}

pub fn set_inline_syntax_hook_source(
    syntax: &mut UnitSyntaxState,
    hook_ref: &str,
    implementation_source: &str,
) -> CaapResult<()> {
    if hook_ref.is_empty() {
        return Err(CaapError::parse("inline syntax hook ref must be non-empty"));
    }
    let hooks = syntax
        .grammar_metadata
        .get("semantic_hook_inline_sources")
        .cloned()
        .unwrap_or_else(|| SemanticValue::Map(Vec::new()));
    let mut entries = match hooks {
        SemanticValue::Map(entries) => entries,
        _ => Vec::new(),
    };
    entries.retain(|(key, _)| key != hook_ref);
    entries.push((
        hook_ref.to_string(),
        SemanticValue::Str(implementation_source.to_string()),
    ));
    syntax.set_grammar_metadata("semantic_hook_inline_sources", SemanticValue::Map(entries))?;
    Ok(())
}

fn inline_hook_ref(source: &str, implementation_source: &str) -> String {
    let mut bytes = Vec::with_capacity(source.len() + implementation_source.len() + 1);
    bytes.extend_from_slice(source.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(implementation_source.as_bytes());
    let suffix = sha256_hex_prefix_32(&bytes);
    format!("inline.syntax.sha256.{suffix}")
}

fn sha256_hex_prefix_32(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn sv_tag(tag: &str, mut rest: Vec<SemanticValue>) -> SemanticValue {
    let mut items = Vec::with_capacity(rest.len() + 1);
    items.push(SemanticValue::Str(tag.to_string()));
    items.append(&mut rest);
    SemanticValue::List(items)
}

fn sv_str(value: impl Into<String>) -> SemanticValue {
    SemanticValue::Str(value.into())
}

fn rule_metadata(hooks: &[(String, Vec<String>)]) -> Result<Option<SemanticValue>, String> {
    if hooks.is_empty() {
        return Ok(None);
    }
    let hook_items = hooks
        .iter()
        .map(|(hook, _)| SemanticValue::List(vec![sv_str(hook), sv_str(hook)]))
        .collect();
    Ok(Some(
        SemanticValue::map([(
            "semantic_hooks".to_string(),
            SemanticValue::List(hook_items),
        )])
        .map_err(|error| error.to_string())?,
    ))
}

struct Parser<'a> {
    source: &'a str,
    pos: usize,
    semantic_hooks: Vec<(String, Vec<String>)>,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            semantic_hooks: Vec::new(),
        }
    }

    fn parse_statement(&mut self) -> Result<AuthoringOp, String> {
        self.skip_spaces();
        if self.consume_keyword("add") {
            self.parse_rule_op(true)
        } else if self.consume_keyword("replace") {
            self.parse_rule_op(false)
        } else if self.consume_keyword("include_grammar") {
            let source = self.parse_string_literal()?;
            let prefix = if self.peek_nonspace().is_some() {
                Some(self.parse_string_literal()?)
            } else {
                None
            };
            Ok(AuthoringOp::IncludeGrammar { source, prefix })
        } else if self.consume_keyword("set") {
            let key = self.parse_identifier()?;
            self.expect_char('=')?;
            self.skip_spaces();
            let value = if self.consume_keyword("none") {
                None
            } else {
                Some(self.parse_string_literal()?)
            };
            Ok(AuthoringOp::SetMetadata { key, value })
        } else {
            Err("expected add, replace, set, or include-grammar".to_string())
        }
    }

    fn parse_rule_op(&mut self, add: bool) -> Result<AuthoringOp, String> {
        self.expect_keyword("rule")?;
        let name = self.parse_identifier()?;
        self.expect_char('=')?;
        let expr = self.parse_choice()?;
        let metadata = rule_metadata(&self.semantic_hooks)?;
        let op = AuthoringRuleOp {
            name,
            expr,
            metadata,
        };
        if add {
            Ok(AuthoringOp::AddRule(op))
        } else {
            Ok(AuthoringOp::ReplaceRule(op))
        }
    }

    fn parse_choice(&mut self) -> Result<SemanticValue, String> {
        let mut alts = vec![self.parse_sequence()?];
        loop {
            self.skip_spaces();
            if !self.consume_char('|') {
                break;
            }
            alts.push(self.parse_sequence()?);
        }
        if alts.len() == 1 {
            Ok(alts.remove(0))
        } else {
            Ok(sv_tag("choice", alts))
        }
    }

    fn parse_sequence(&mut self) -> Result<SemanticValue, String> {
        let mut items = Vec::new();
        loop {
            self.skip_spaces();
            if self.remaining().starts_with("->") {
                break;
            }
            match self.peek_char() {
                None | Some(')') | Some('|') => break,
                _ => items.push(self.parse_transformable()?),
            }
        }
        if items.is_empty() {
            return Err("expected expression".to_string());
        }
        let expr = if items.len() == 1 {
            items.remove(0)
        } else {
            sv_tag("seq", items)
        };
        self.parse_transform_suffix(expr)
    }

    fn parse_transformable(&mut self) -> Result<SemanticValue, String> {
        let left = self.parse_postfix()?;
        self.skip_spaces();
        if self.consume_char('.') {
            let right = self.parse_primary()?;
            self.skip_spaces();
            if !self.consume_char('+') {
                return Err("separator expressions require '+' after the item expression".into());
            }
            return Ok(sv_tag("sep_plus", vec![left, right]));
        }
        Ok(left)
    }

    fn parse_postfix(&mut self) -> Result<SemanticValue, String> {
        let mut expr = self.parse_primary()?;
        loop {
            self.skip_spaces();
            if self.consume_char('?') {
                expr = sv_tag("optional", vec![expr]);
            } else if self.consume_char('*') {
                expr = sv_tag("many", vec![expr]);
            } else if self.consume_char('+') {
                expr = sv_tag("plus", vec![expr]);
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<SemanticValue, String> {
        self.skip_spaces();
        if self.consume_char('(') {
            let expr = self.parse_choice()?;
            self.expect_char(')')?;
            return Ok(expr);
        }
        if self.consume_char('~') {
            return Ok(sv_tag("cut", Vec::new()));
        }
        if self.peek_char() == Some('"') {
            return Ok(sv_tag(
                "literal",
                vec![sv_str(self.parse_string_literal()?)],
            ));
        }
        if self.peek_char() == Some('/') {
            return Ok(sv_tag("regex", vec![sv_str(self.parse_regex_literal()?)]));
        }
        let ident = self.parse_identifier()?;
        self.skip_spaces();
        if self.consume_char(':') {
            let expr = self.parse_postfix()?;
            return Ok(sv_tag("named", vec![sv_str(ident), expr]));
        }
        Ok(sv_tag("ref", vec![sv_str(ident)]))
    }

    fn parse_transform_suffix(&mut self, expr: SemanticValue) -> Result<SemanticValue, String> {
        self.skip_spaces();
        if !self.consume_str("->") {
            return Ok(expr);
        }
        let hook = self.parse_identifier()?;
        let mut action = vec![sv_str("transform"), sv_str(&hook)];
        let mut args = Vec::new();
        self.skip_spaces();
        if self.consume_char('(') {
            loop {
                self.skip_spaces();
                if self.consume_char(')') {
                    break;
                }
                args.push(self.parse_string_literal()?);
                self.skip_spaces();
                if self.consume_char(',') {
                    continue;
                }
                self.expect_char(')')?;
                break;
            }
        }
        action.extend(args.iter().cloned().map(sv_str));
        self.semantic_hooks.push((hook, args));
        Ok(sv_tag(
            "behavior",
            vec![SemanticValue::List(vec![SemanticValue::List(action)]), expr],
        ))
    }

    fn parse_identifier(&mut self) -> Result<String, String> {
        self.skip_spaces();
        let start = self.pos;
        let mut chars = self.remaining().char_indices();
        let Some((_, first)) = chars.next() else {
            return Err("expected identifier".to_string());
        };
        if !is_ident_start(first) {
            return Err(format!("expected identifier at byte {}", self.pos));
        }
        self.pos += first.len_utf8();
        while let Some(ch) = self.peek_char() {
            if !is_ident_continue(ch) {
                break;
            }
            self.pos += ch.len_utf8();
        }
        Ok(self.source[start..self.pos].to_string())
    }

    fn parse_string_literal(&mut self) -> Result<String, String> {
        self.skip_spaces();
        self.expect_char('"')?;
        let mut out = String::new();
        while let Some(ch) = self.next_char() {
            match ch {
                '"' => return Ok(out),
                '\\' => {
                    let escaped = self
                        .next_char()
                        .ok_or_else(|| "unterminated string escape".to_string())?;
                    match escaped {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
        Err("unterminated string literal".to_string())
    }

    fn parse_regex_literal(&mut self) -> Result<String, String> {
        self.skip_spaces();
        self.expect_char('/')?;
        let mut out = String::new();
        let mut escaped = false;
        while let Some(ch) = self.next_char() {
            if escaped {
                out.push('\\');
                out.push(ch);
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '/' {
                return Ok(out);
            }
            out.push(ch);
        }
        Err("unterminated regex literal".to_string())
    }

    fn expect_keyword(&mut self, keyword: &str) -> Result<(), String> {
        if self.consume_keyword(keyword) {
            Ok(())
        } else {
            Err(format!("expected {keyword}"))
        }
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        self.skip_spaces();
        let rest = self.remaining();
        if !rest.starts_with(keyword) {
            return false;
        }
        let next_pos = self.pos + keyword.len();
        if self
            .source
            .get(next_pos..)
            .and_then(|rest| rest.chars().next())
            .is_some_and(is_ident_continue)
        {
            return false;
        }
        self.pos = next_pos;
        true
    }

    fn expect_char(&mut self, ch: char) -> Result<(), String> {
        self.skip_spaces();
        if self.consume_char(ch) {
            Ok(())
        } else {
            Err(format!("expected {ch:?} at byte {}", self.pos))
        }
    }

    fn consume_char(&mut self, ch: char) -> bool {
        if self.peek_char() == Some(ch) {
            self.pos += ch.len_utf8();
            true
        } else {
            false
        }
    }

    fn consume_str(&mut self, text: &str) -> bool {
        self.skip_spaces();
        if self.remaining().starts_with(text) {
            self.pos += text.len();
            true
        } else {
            false
        }
    }

    fn expect_end(&mut self) -> Result<(), String> {
        self.skip_spaces();
        if self.pos == self.source.len() {
            Ok(())
        } else {
            Err(format!("unexpected trailing input at byte {}", self.pos))
        }
    }

    fn skip_spaces(&mut self) {
        while let Some(ch) = self.peek_char() {
            if ch.is_whitespace() {
                self.pos += ch.len_utf8();
            } else {
                break;
            }
        }
    }

    fn peek_nonspace(&mut self) -> Option<char> {
        self.skip_spaces();
        self.peek_char()
    }

    fn peek_char(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    fn next_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn remaining(&self) -> &'a str {
        &self.source[self.pos..]
    }
}

fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_ident_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_transform_rule_like_reference_authoring() {
        let ops = compile_authoring_grammar_source(
            r#"add rule demo = "x" -> surface.keyword_string("X")"#,
        )
        .unwrap();
        let AuthoringOp::AddRule(op) = &ops[0] else {
            panic!("expected add rule op");
        };
        assert_eq!(op.name, "demo");
        assert_eq!(
            op.expr,
            sv_tag(
                "behavior",
                vec![
                    SemanticValue::List(vec![SemanticValue::List(vec![
                        sv_str("transform"),
                        sv_str("surface.keyword_string"),
                        sv_str("X")
                    ])]),
                    sv_tag("literal", vec![sv_str("x")])
                ]
            )
        );
        assert_eq!(
            op.metadata,
            Some(
                SemanticValue::map([(
                    "semantic_hooks".to_string(),
                    SemanticValue::List(vec![SemanticValue::List(vec![
                        sv_str("surface.keyword_string"),
                        sv_str("surface.keyword_string")
                    ])])
                )])
                .unwrap()
            )
        );
    }

    #[test]
    fn compiles_choice_named_many_and_separator_plus() {
        let ops = compile_authoring_grammar_source(
            r#"add rule demo = name:symbol* | ("x" symbol) -> surface.list
add rule args = ",".symbol+"#,
        )
        .unwrap();
        let AuthoringOp::AddRule(first) = &ops[0] else {
            panic!("expected first add rule op");
        };
        assert_eq!(
            first.expr,
            sv_tag(
                "choice",
                vec![
                    sv_tag(
                        "named",
                        vec![
                            sv_str("name"),
                            sv_tag("many", vec![sv_tag("ref", vec![sv_str("symbol")])])
                        ]
                    ),
                    sv_tag(
                        "behavior",
                        vec![
                            SemanticValue::List(vec![SemanticValue::List(vec![
                                sv_str("transform"),
                                sv_str("surface.list")
                            ])]),
                            sv_tag(
                                "seq",
                                vec![
                                    sv_tag("literal", vec![sv_str("x")]),
                                    sv_tag("ref", vec![sv_str("symbol")])
                                ]
                            )
                        ]
                    )
                ]
            )
        );
        let AuthoringOp::AddRule(second) = &ops[1] else {
            panic!("expected second add rule op");
        };
        assert_eq!(
            second.expr,
            sv_tag(
                "sep_plus",
                vec![
                    sv_tag("literal", vec![sv_str(",")]),
                    sv_tag("ref", vec![sv_str("symbol")])
                ]
            )
        );
    }

    #[test]
    fn inline_hook_ref_uses_sha256_namespace() {
        assert_eq!(
            sha256_hex_prefix_32(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223"
        );
        assert!(inline_hook_ref("add rule demo = symbol", "(lambda (x) x)")
            .starts_with("inline.syntax.sha256."));
    }

    #[test]
    fn define_rule_trims_trailing_source_and_registers_metadata_hook_ref() {
        let mut syntax = UnitSyntaxState::new("caap").unwrap();

        define_authoring_syntax_rule(&mut syntax, "add rule demo = symbol\n", "lower_demo")
            .unwrap();

        assert!(syntax.grammar_rules.contains_key("demo"));
        assert_eq!(
            syntax.grammar_metadata.get("semantic_hook_functions"),
            Some(&SemanticValue::Map(vec![(
                "lower_demo".to_string(),
                SemanticValue::Str("lower_demo".to_string())
            )]))
        );
    }

    #[test]
    fn semantic_hook_references_rejects_malformed_metadata() {
        let metadata = SemanticValue::map([(
            "semantic_hooks".to_string(),
            SemanticValue::List(vec![SemanticValue::Str("not_a_pair".to_string())]),
        )])
        .unwrap();

        assert!(semantic_hook_references(&metadata)
            .unwrap_err()
            .message()
            .contains("entries must be pairs"));
    }
}
