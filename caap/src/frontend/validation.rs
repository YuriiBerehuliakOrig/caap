use crate::error::{CaapError, CaapResult};

use super::{parse_forms, ParsedForm, ParsedSource};

/// Validate CAAP surface text without lowering it to IR.
pub fn check_source(source: &str) -> CaapResult<()> {
    parse_forms(source).map(|_| ())
}

/// Canonicalize CAAP surface text into a stable one-line representation.
pub fn canonicalize_source(source: &str) -> CaapResult<String> {
    let parsed = parse_forms(source)?;
    Ok(canonicalize_parsed_source(&parsed))
}

/// Parse CAAP surface text and project the typed surface model as JSON.
pub fn ast_json(source: &str) -> CaapResult<String> {
    let parsed = parse_forms(source)?;
    serde_json::to_string_pretty(&parsed).map_err(|error| {
        CaapError::parse(format!("failed to serialize parsed surface forms: {error}"))
    })
}

pub fn canonicalize_parsed_source(parsed: &ParsedSource) -> String {
    parsed
        .forms
        .iter()
        .map(canonicalize_parsed_form)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn canonicalize_parsed_form(form: &ParsedForm) -> String {
    match form {
        ParsedForm::List { items, .. } => {
            if items.is_empty() {
                "()".to_string()
            } else {
                format!(
                    "({})",
                    items
                        .iter()
                        .map(canonicalize_parsed_form)
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            }
        }
        ParsedForm::Symbol { text, .. } => text.clone(),
        ParsedForm::String { value, .. } => format!("\"{}\"", super::escape_string_literal(value)),
        ParsedForm::Integer { value, .. } => value.to_string(),
        ParsedForm::Float { raw, .. } => raw.clone(),
        ParsedForm::Boolean { value, .. } => value.to_string(),
        ParsedForm::Null { .. } => "null".to_string(),
    }
}
