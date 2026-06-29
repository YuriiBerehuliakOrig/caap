//! Semantic tokens. Two sources, never spelling heuristics:
//!
//! - **Grammar-extended (surface) files** — the surface analyzer (e.g. clike's
//!   `analyze_program`) already classified every token from the PARSE + a SYMBOL
//!   TABLE (functions, user types, enum members, params; `name(` is a call, an
//!   identifier after `->` is a field, …), plus string-aware `//` comment tokens
//!   the lexer discards. Those tokens arrive in `analysis.tokens` and ARE the
//!   highlighting — rendered directly here.
//! - **Plain s-expr files** — classified from the parsed kernel AST (`emit_form`),
//!   using the kernel vocabulary (`crate::vocab`), not a list maintained here.
//!
//! The TextMate grammar stays a lexical-only fallback (strings/numbers/operators;
//! surface-file comments now come from the analyzer); this module owns every
//! SEMANTIC category.

use caap_core::{ParsedForm, SourceSpan};
use lsp_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens, SemanticTokensLegend,
    SemanticTokensResult,
};

use crate::analyze::Analysis;

const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::KEYWORD,     // 0
    SemanticTokenType::FUNCTION,    // 1
    SemanticTokenType::MACRO,       // 2
    SemanticTokenType::VARIABLE,    // 3
    SemanticTokenType::NUMBER,      // 4
    SemanticTokenType::STRING,      // 5
    SemanticTokenType::OPERATOR,    // 6
    SemanticTokenType::NAMESPACE,   // 7
    SemanticTokenType::PARAMETER,   // 8
    SemanticTokenType::TYPE,        // 9
    SemanticTokenType::PROPERTY,    // 10
    SemanticTokenType::ENUM_MEMBER, // 11
    SemanticTokenType::COMMENT,     // 12
];

const TOKEN_MODIFIERS: &[SemanticTokenModifier] = &[
    SemanticTokenModifier::DECLARATION, // bit 0
    SemanticTokenModifier::READONLY,    // bit 1
];

const KW_KEYWORD: u32 = 0;
const KW_FUNCTION: u32 = 1;
const KW_MACRO: u32 = 2;
const KW_VARIABLE: u32 = 3;
const KW_NUMBER: u32 = 4;
const KW_STRING: u32 = 5;
const KW_OPERATOR: u32 = 6;
const KW_NAMESPACE: u32 = 7;
const KW_PARAMETER: u32 = 8;
const KW_TYPE: u32 = 9;
const KW_PROPERTY: u32 = 10;
const KW_ENUM_MEMBER: u32 = 11;
const KW_COMMENT: u32 = 12;

const MOD_DECLARATION: u32 = 1 << 0;
const MOD_READONLY: u32 = 1 << 1;

pub fn token_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TOKEN_TYPES.to_vec(),
        token_modifiers: TOKEN_MODIFIERS.to_vec(),
    }
}

pub fn full_tokens(analysis: &Analysis, _source: &str) -> SemanticTokensResult {
    let mut absolute = Vec::new();
    if analysis.tokens.is_empty() {
        // Plain s-expr file: classify leaves from the parsed kernel AST.
        for form in &analysis.parsed.forms {
            emit_form(form, /* head_position = */ false, &mut absolute);
        }
    } else {
        // Grammar-extended (surface) file: the surface analyzer already
        // classified each token from the parse + symbol table. Render it as-is.
        for tok in &analysis.tokens {
            push_span_token(
                &tok.span,
                category_index(&tok.kind),
                modifier_bits(&tok.mods),
                &mut absolute,
            );
        }
    }
    absolute.sort_by_key(|tok: &AbsToken| (tok.line, tok.start_char));
    // Safety net against double-emitting at one position (stable sort keeps the
    // first), so tokens never overlap.
    absolute.dedup_by_key(|tok| (tok.line, tok.start_char));
    SemanticTokensResult::Tokens(SemanticTokens {
        result_id: None,
        data: delta_encode(absolute),
    })
}

/// Map a surface-analyzer category string to its legend index. An unknown
/// category degrades to `variable` (never panics, never drops the token).
fn category_index(kind: &str) -> u32 {
    match kind {
        "keyword" => KW_KEYWORD,
        "function" => KW_FUNCTION,
        "macro" => KW_MACRO,
        "variable" => KW_VARIABLE,
        "number" => KW_NUMBER,
        "string" => KW_STRING,
        "operator" => KW_OPERATOR,
        "namespace" => KW_NAMESPACE,
        "parameter" => KW_PARAMETER,
        "type" => KW_TYPE,
        "property" => KW_PROPERTY,
        "enumMember" => KW_ENUM_MEMBER,
        "comment" => KW_COMMENT,
        _ => KW_VARIABLE,
    }
}

fn modifier_bits(mods: &[String]) -> u32 {
    let mut bits = 0;
    for m in mods {
        match m.as_str() {
            "declaration" => bits |= MOD_DECLARATION,
            "readonly" => bits |= MOD_READONLY,
            _ => {}
        }
    }
    bits
}

struct AbsToken {
    line: u32,
    start_char: u32,
    length: u32,
    ty: u32,
    mods: u32,
}

fn emit_form(form: &ParsedForm, head_position: bool, out: &mut Vec<AbsToken>) {
    match form {
        ParsedForm::List { items, .. } => {
            for (i, item) in items.iter().enumerate() {
                emit_form(item, i == 0, out);
            }
        }
        ParsedForm::Symbol { text, span } => {
            let ty = classify_symbol(text, head_position);
            push_span_token(span, ty, 0, out);
        }
        ParsedForm::String { span, .. } => push_span_token(span, KW_STRING, 0, out),
        ParsedForm::Integer { span, .. } => push_span_token(span, KW_NUMBER, 0, out),
        ParsedForm::Float { span, .. } => push_span_token(span, KW_NUMBER, 0, out),
        ParsedForm::Boolean { span, .. } => push_span_token(span, KW_KEYWORD, 0, out),
        ParsedForm::Null { span } => push_span_token(span, KW_KEYWORD, 0, out),
    }
}

/// Convert a 1-based single-line CAAP span into a 0-based LSP token. Multi-line
/// spans and degenerate (zero-length / origin) spans are skipped.
fn push_span_token(span: &SourceSpan, ty: u32, mods: u32, out: &mut Vec<AbsToken>) {
    if span.start_line == 0 {
        return;
    }
    // CAAP atoms never span lines; a multi-line span is a list (no own token).
    if span.start_line != span.end_line {
        return;
    }
    let line = (span.start_line.saturating_sub(1)) as u32;
    let start_char = (span.start_col.saturating_sub(1)) as u32;
    let length = (span.end_col.saturating_sub(span.start_col)) as u32;
    if length == 0 {
        return;
    }
    out.push(AbsToken {
        line,
        start_char,
        length,
        ty,
        mods,
    });
}

fn classify_symbol(text: &str, head_position: bool) -> u32 {
    // Reserved words come from the kernel (see `crate::vocab`), never a list
    // maintained here — so highlighting tracks the real language vocabulary.
    if crate::vocab::is_special_form(text) || crate::vocab::is_literal(text) {
        return KW_KEYWORD;
    }
    if is_operator_symbol(text) {
        return KW_OPERATOR;
    }
    if text.contains('.') {
        // Dotted identifier likely refers to a namespaced member.
        return KW_NAMESPACE;
    }
    if head_position {
        KW_FUNCTION
    } else if text.starts_with('&') {
        KW_PARAMETER
    } else {
        KW_VARIABLE
    }
}

fn is_operator_symbol(text: &str) -> bool {
    // An operator token is a non-empty symbol made ENTIRELY of kernel
    // symbol-punctuation (`caap_core::language`), excluding the identifier-ish `_`
    // and the namespace separator `.` (dotted names are classified as namespace
    // above). Derived from the kernel symbol grammar, never a parallel char list.
    !text.is_empty()
        && text.chars().all(|c| {
            c != '_'
                && c != '.'
                && !c.is_ascii_alphanumeric()
                && caap_core::language::is_symbol_char(c)
        })
}

fn delta_encode(absolute: Vec<AbsToken>) -> Vec<SemanticToken> {
    let mut out = Vec::with_capacity(absolute.len());
    let mut prev_line: u32 = 0;
    let mut prev_start: u32 = 0;
    for tok in absolute {
        let delta_line = tok.line - prev_line;
        let delta_start = if delta_line == 0 {
            tok.start_char.saturating_sub(prev_start)
        } else {
            tok.start_char
        };
        out.push(SemanticToken {
            delta_line,
            delta_start,
            length: tok.length,
            token_type: tok.ty,
            token_modifiers_bitset: tok.mods,
        });
        prev_line = tok.line;
        prev_start = tok.start_char;
    }
    out
}
