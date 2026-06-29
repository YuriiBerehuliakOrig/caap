use serde_json::Value;

use crate::expr::{CompiledRegex, PegExpr, RuleTextParser};

use super::spec_compiler::SpecCompileError;
use super::spec_compiler_helpers::{
    expect_arr_at, expect_non_empty_str_at, expect_str_at, expect_val_at, optional_bool_at,
    optional_bool_field, optional_non_empty_str_field, optional_str_at, optional_str_field,
    quote_rule_string,
};

// ── Expression → source text ───────────────────────────────────────────────

/// Convert a spec expression to its PEG grammar source text.
///
/// Accepts both **list form** (`["lit", "hello"]`) and canonical
/// **mapping form** (`{"type": "literal", "text": "hello"}`).
pub fn expr_to_source(expr: &Value) -> Result<String, SpecCompileError> {
    // Mapping-style spec: { "type": "...", ... }
    if let Some(obj) = expr.as_object() {
        return mapping_expr_to_source(obj);
    }
    let arr = super::spec_compiler_helpers::expect_arr(expr, "expr")?;
    if arr.is_empty() {
        return Err(SpecCompileError::MissingField("expr tag".to_string()));
    }

    let tag = expect_str_at(arr, 0, "expr tag")?;

    match tag {
        // ── Terminals ──────────────────────────────────────────────────
        "lit" | "literal" => {
            let text = expect_str_at(arr, 1, "lit text")?;
            let escaped = text.replace('\\', "\\\\").replace('\'', "\\'");
            Ok(format!("'{escaped}'"))
        }
        "regex" => {
            let pattern = expect_str_at(arr, 1, "regex pattern")?;
            Ok(format!("/{pattern}/"))
        }
        "token" => {
            let pattern = expect_str_at(arr, 1, "token pattern")?;
            Ok(format!("token({pattern})"))
        }
        "tok" | "token_ref" => {
            let kind = expect_non_empty_str_at(arr, 1, "tok kind")?;
            if arr.len() > 2 {
                let text = expect_non_empty_str_at(arr, 2, "tok text")?;
                let escaped = text.replace('\'', "\\'");
                Ok(format!("tok({kind},'{escaped}')"))
            } else {
                Ok(format!("tok({kind})"))
            }
        }
        "soft_kw" | "soft_keyword" => {
            let text = expect_str_at(arr, 1, "soft_kw text")?;
            let escaped = text.replace('\'', "\\'");
            Ok(format!("'{escaped}'"))
        }
        "param" => {
            let name = expect_str_at(arr, 1, "param name")?;
            Ok(format!("${name}"))
        }
        "call" => {
            let rule = expect_str_at(arr, 1, "call rule")?;
            let args = arr[2..]
                .iter()
                .map(expr_to_source)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(format!("{rule}({})", args.join(", ")))
        }
        "newline" => Ok("newline".to_string()),
        "indent" => Ok("indent".to_string()),
        "dedent" => Ok("dedent".to_string()),

        // ── Composite ──────────────────────────────────────────────────
        "seq" => {
            let items = expect_arr_at(arr, 1, "seq items")?;
            let parts: Vec<_> = items.iter().map(expr_to_source).collect::<Result<_, _>>()?;
            Ok(match parts.as_slice() {
                [] => String::new(),
                [part] => part.clone(),
                _ => format!("({})", parts.join(" ")),
            })
        }
        "choice" => {
            let items = expect_arr_at(arr, 1, "choice items")?;
            let parts: Vec<_> = items.iter().map(expr_to_source).collect::<Result<_, _>>()?;
            Ok(match parts.as_slice() {
                [] => String::new(),
                [part] => part.clone(),
                _ => format!("({})", parts.join(" / ")),
            })
        }

        // ── Quantifiers ────────────────────────────────────────────────
        "star" | "*" | "many" => {
            let inner = expr_to_source(expect_val_at(arr, 1, "star expr")?)?;
            Ok(format!("{inner}*"))
        }
        "plus" | "+" | "one_or_more" => {
            let inner = expr_to_source(expect_val_at(arr, 1, "plus expr")?)?;
            Ok(format!("{inner}+"))
        }
        "opt" | "?" | "optional" => {
            let inner = expr_to_source(expect_val_at(arr, 1, "opt expr")?)?;
            Ok(format!("{inner}?"))
        }

        // ── References ─────────────────────────────────────────────────
        "ref" => {
            let name = expect_str_at(arr, 1, "ref name")?;
            Ok(name.to_string())
        }
        "imported_ref" | "import" => {
            let grammar = expect_str_at(arr, 1, "imported_ref grammar")?;
            let rule = expect_str_at(arr, 2, "imported_ref rule")?;
            Ok(format!("{grammar}::{rule}"))
        }
        "grammar_scope" | "scope" => {
            let grammar = expect_str_at(arr, 1, "grammar_scope grammar")?;
            let inner = expr_to_source(expect_val_at(arr, 2, "grammar_scope expr")?)?;
            Ok(format!("scope('{grammar}', {inner})"))
        }

        // ── Predicates ─────────────────────────────────────────────────
        "and" => {
            let inner = expr_to_source(expect_val_at(arr, 1, "and expr")?)?;
            Ok(format!("&{inner}"))
        }
        "not" => {
            let inner = expr_to_source(expect_val_at(arr, 1, "not expr")?)?;
            Ok(format!("!{inner}"))
        }

        // ── Named capture / binding ────────────────────────────────────
        "named" | "=" | "bind" => {
            let name = expect_str_at(arr, 1, "named label")?;
            let inner = expr_to_source(expect_val_at(arr, 2, "named expr")?)?;
            Ok(format!("{name}:{inner}"))
        }

        // ── Cut ────────────────────────────────────────────────────────
        "cut" | "~" => Ok("~".to_string()),

        // ── Eager ──────────────────────────────────────────────────────
        "eager" | "&&" => {
            let inner = expr_to_source(expect_val_at(arr, 1, "eager expr")?)?;
            Ok(format!("&&{inner}"))
        }

        // ── Tight / no_trivia ──────────────────────────────────────────
        "tight" | "no_trivia" => {
            let inner = expr_to_source(expect_val_at(arr, 1, "tight expr")?)?;
            Ok(format!("no_trivia({inner})"))
        }

        // ── Separator ──────────────────────────────────────────────────
        "sep_plus" | "gather" => {
            let sep = expr_to_source(expect_val_at(arr, 1, "sep_plus sep")?)?;
            let elem = expr_to_source(expect_val_at(arr, 2, "sep_plus element")?)?;
            Ok(format!("({elem} ({sep} {elem})*)"))
        }
        "interspersed" => {
            let elem = expr_to_source(expect_val_at(arr, 1, "interspersed element")?)?;
            let sep = expr_to_source(expect_val_at(arr, 2, "interspersed sep")?)?;
            Ok(format!("interspersed({elem}, {sep})"))
        }

        // ── Island / raw_block ─────────────────────────────────────────
        "island" => {
            let start = expect_str_at(arr, 1, "island start")?;
            let end = expect_str_at(arr, 2, "island end")?;
            let include_delims =
                optional_bool_at(arr, 3, "island include_delims")?.unwrap_or(false);
            if include_delims {
                Ok(format!(
                    "island({}, {}, true)",
                    quote_rule_string(start),
                    quote_rule_string(end)
                ))
            } else {
                Ok(format!(
                    "island({}, {})",
                    quote_rule_string(start),
                    quote_rule_string(end)
                ))
            }
        }
        "raw_block" => {
            let start = expect_str_at(arr, 1, "raw_block start")?;
            let end = expect_str_at(arr, 2, "raw_block end")?;
            if let Some(delim_kind) = optional_str_at(arr, 3, "raw_block delim_kind")? {
                Ok(format!(
                    "raw_block({}, {}, {})",
                    quote_rule_string(start),
                    quote_rule_string(end),
                    quote_rule_string(delim_kind)
                ))
            } else {
                Ok(format!(
                    "raw_block({}, {})",
                    quote_rule_string(start),
                    quote_rule_string(end)
                ))
            }
        }

        // ── Precedence hint (ignored, just compile inner) ──────────────
        "prec" => {
            // ["prec", ..., expr] — last element is the actual expression
            let last = arr
                .last()
                .ok_or_else(|| SpecCompileError::MissingField("prec inner expr".to_string()))?;
            expr_to_source(last)
        }

        // ── Expected (diagnostic label) ────────────────────────────────
        // The label is informational; the textual repr keeps the inner
        // expression (like `behavior`/`prec`), while expr_to_peg_expr builds
        // the real PegExpr::Expected that carries the message into diagnostics.
        "expected" => expr_to_source(expect_val_at(arr, 2, "expected expr")?),

        other => Err(SpecCompileError::UnknownTag(other.to_string())),
    }
}

pub(super) fn expr_to_peg_expr(expr: &Value) -> Result<PegExpr, SpecCompileError> {
    if let Some(obj) = expr.as_object() {
        return mapping_expr_to_peg_expr(obj);
    }

    if let Some(arr) = expr.as_array() {
        return list_expr_to_peg_expr(arr);
    }

    let source = expr_to_source(expr)?;
    RuleTextParser::parse(&source)
        .map_err(|err| SpecCompileError::InvalidFormat(err.message.into()))
}

fn list_expr_to_peg_expr(arr: &[Value]) -> Result<PegExpr, SpecCompileError> {
    let tag = expect_str_at(arr, 0, "expr tag")?;
    match tag {
        "lit" | "literal" => Ok(PegExpr::Literal(
            expect_str_at(arr, 1, "lit text")?.to_string(),
        )),
        "regex" => {
            let pattern = expect_str_at(arr, 1, "regex pattern")?;
            Ok(PegExpr::Regex(
                CompiledRegex::new(pattern, 0, pattern.len())
                    .map_err(|err| SpecCompileError::InvalidFormat(err.message.into()))?,
            ))
        }
        "tok" | "token_ref" => {
            let kind = Some(expect_non_empty_str_at(arr, 1, "tok kind")?.to_string());
            let text = if arr.len() > 2 {
                Some(expect_non_empty_str_at(arr, 2, "tok text")?.to_string())
            } else {
                None
            };
            Ok(PegExpr::TokenRef { kind, text })
        }
        "soft_kw" | "soft_keyword" => Ok(PegExpr::SoftKeyword(
            expect_str_at(arr, 1, "soft keyword text")?.to_string(),
        )),
        "param" => Ok(PegExpr::Parameter {
            name: expect_str_at(arr, 1, "param name")?.to_string(),
        }),
        "call" => Ok(PegExpr::Call {
            rule: expect_str_at(arr, 1, "call rule")?.to_string(),
            args: arr[2..]
                .iter()
                .map(expr_to_peg_expr)
                .collect::<Result<Vec<_>, _>>()?,
        }),
        "newline" => Ok(PegExpr::Newline),
        "indent" => Ok(PegExpr::Indent),
        "dedent" => Ok(PegExpr::Dedent),
        "seq" => Ok(PegExpr::Sequence(
            expect_arr_at(arr, 1, "seq items")?
                .iter()
                .map(expr_to_peg_expr)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        "choice" => Ok(PegExpr::Choice(
            expect_arr_at(arr, 1, "choice items")?
                .iter()
                .map(expr_to_peg_expr)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        "star" | "*" | "many" => Ok(PegExpr::ZeroOrMore(Box::new(expr_to_peg_expr(
            expect_val_at(arr, 1, "star expr")?,
        )?))),
        "plus" | "+" | "one_or_more" => Ok(PegExpr::OneOrMore(Box::new(expr_to_peg_expr(
            expect_val_at(arr, 1, "plus expr")?,
        )?))),
        "opt" | "?" | "optional" => Ok(PegExpr::Optional(Box::new(expr_to_peg_expr(
            expect_val_at(arr, 1, "opt expr")?,
        )?))),
        "ref" => Ok(PegExpr::Ref(expect_str_at(arr, 1, "ref name")?.to_string())),
        "imported_ref" | "import" => Ok(PegExpr::ImportedRef {
            grammar_name: expect_str_at(arr, 1, "imported_ref grammar")?.to_string(),
            rule_name: expect_str_at(arr, 2, "imported_ref rule")?.to_string(),
        }),
        "grammar_scope" | "scope" => Ok(PegExpr::GrammarScope {
            grammar_name: expect_str_at(arr, 1, "grammar_scope grammar")?.to_string(),
            expr: Box::new(expr_to_peg_expr(expect_val_at(
                arr,
                2,
                "grammar_scope expr",
            )?)?),
        }),
        "and" => Ok(PegExpr::And(Box::new(expr_to_peg_expr(expect_val_at(
            arr, 1, "and expr",
        )?)?))),
        "not" => Ok(PegExpr::Not(Box::new(expr_to_peg_expr(expect_val_at(
            arr, 1, "not expr",
        )?)?))),
        "named" | "=" | "bind" => Ok(PegExpr::Named {
            name: expect_str_at(arr, 1, "named label")?.to_string(),
            expr: Box::new(expr_to_peg_expr(expect_val_at(arr, 2, "named expr")?)?),
        }),
        "cut" | "~" => Ok(PegExpr::Cut),
        "eager" | "&&" => Ok(PegExpr::Eager(Box::new(expr_to_peg_expr(expect_val_at(
            arr,
            1,
            "eager expr",
        )?)?))),
        "tight" | "no_trivia" => Ok(PegExpr::NoTrivia(Box::new(expr_to_peg_expr(
            expect_val_at(arr, 1, "tight expr")?,
        )?))),
        "sep_plus" | "gather" => Ok(PegExpr::SepOneOrMore {
            separator: Box::new(expr_to_peg_expr(expect_val_at(arr, 1, "sep_plus sep")?)?),
            element: Box::new(expr_to_peg_expr(expect_val_at(
                arr,
                2,
                "sep_plus element",
            )?)?),
        }),
        "interspersed" => Ok(PegExpr::Interspersed {
            element: Box::new(expr_to_peg_expr(expect_val_at(
                arr,
                1,
                "interspersed element",
            )?)?),
            separator: Box::new(expr_to_peg_expr(expect_val_at(
                arr,
                2,
                "interspersed sep",
            )?)?),
        }),
        "island" => Ok(PegExpr::Island {
            start: expect_str_at(arr, 1, "island start")?.to_string(),
            end: expect_str_at(arr, 2, "island end")?.to_string(),
            include_delims: optional_bool_at(arr, 3, "island include_delims")?.unwrap_or(false),
        }),
        "raw_block" => Ok(PegExpr::RawBlock {
            start: expect_str_at(arr, 1, "raw_block start")?.to_string(),
            end: expect_str_at(arr, 2, "raw_block end")?.to_string(),
            delim_kind: optional_str_at(arr, 3, "raw_block delim_kind")?
                .unwrap_or("block")
                .to_string(),
        }),
        "prec" => {
            let last = arr
                .last()
                .ok_or_else(|| SpecCompileError::MissingField("prec inner expr".to_string()))?;
            expr_to_peg_expr(last)
        }
        "expected" => Ok(PegExpr::Expected {
            message: expect_str_at(arr, 1, "expected message")?.to_string(),
            expr: Box::new(expr_to_peg_expr(expect_val_at(arr, 2, "expected expr")?)?),
        }),
        _ => {
            let source = expr_to_source(&Value::Array(arr.to_vec()))?;
            RuleTextParser::parse(&source)
                .map_err(|err| SpecCompileError::InvalidFormat(err.message.into()))
        }
    }
}

// ── Mapping-style expression compiler ────────────────────────────────────

/// Handle `{"type": "...", ...}` mapping-style spec expressions.
///
/// Mirrors `peg/compile/mapping.py::build_mapping_node()`.
fn mapping_expr_to_source(
    obj: &serde_json::Map<String, Value>,
) -> Result<String, SpecCompileError> {
    let kind = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SpecCompileError::MissingField("type".to_string()))?;

    let child_src = |key: &str| -> Result<String, SpecCompileError> {
        let v = obj
            .get(key)
            .ok_or_else(|| SpecCompileError::MissingField(key.to_string()))?;
        expr_to_source(v)
    };
    let parts_src = |key: &str| -> Result<Vec<String>, SpecCompileError> {
        let arr = obj
            .get(key)
            .and_then(|v| v.as_array())
            .ok_or_else(|| SpecCompileError::MissingField(key.to_string()))?;
        arr.iter().map(expr_to_source).collect()
    };

    match kind {
        "literal" | "lit" => {
            let text = obj
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("text".to_string()))?;
            let escaped = text.replace('\\', "\\\\").replace('\'', "\\'");
            Ok(format!("'{escaped}'"))
        }
        "regex" => {
            let pattern = obj
                .get("pattern")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("pattern".to_string()))?;
            Ok(format!("/{pattern}/"))
        }
        "token" => {
            let pattern = obj
                .get("pattern")
                .or_else(|| obj.get("text"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("pattern".to_string()))?;
            Ok(format!("token({pattern})"))
        }
        "token_ref" | "tok" => {
            let kind_val = optional_non_empty_str_field(obj, "kind", "tok kind")?;
            let text_val = optional_non_empty_str_field(obj, "text", "tok text")?;
            match (kind_val, text_val) {
                (Some(kind), Some(text)) => {
                    let escaped = text.replace('\'', "\\'");
                    Ok(format!("tok({kind},'{escaped}')"))
                }
                (Some(kind), None) => Ok(format!("tok({kind})")),
                (None, Some(text)) => {
                    let escaped = text.replace('\\', "\\\\").replace('\'', "\\'");
                    Ok(format!("tok('{escaped}')"))
                }
                (None, None) => Ok("tok()".to_string()),
            }
        }
        "soft_keyword" | "soft_kw" => {
            let text = obj
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("text".to_string()))?;
            let escaped = text.replace('\'', "\\'");
            Ok(format!("'{escaped}'"))
        }
        "param" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("name".to_string()))?;
            Ok(format!("${name}"))
        }
        "call" => {
            let rule = obj
                .get("rule")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("rule".to_string()))?;
            let args = parts_src("args")?;
            Ok(format!("{rule}({})", args.join(", ")))
        }
        "newline" => Ok("newline".to_string()),
        "indent" => Ok("indent".to_string()),
        "dedent" => Ok("dedent".to_string()),
        "cut" | "~" => Ok("~".to_string()),
        "seq" | "sequence" => {
            let children = parts_src("parts")?;
            Ok(children.join(" "))
        }
        "choice" => {
            let children = parts_src("options")?;
            Ok(children.join(" / "))
        }
        "star" | "many" | "zero_or_more" => Ok(format!("({})*", child_src("expr")?)),
        "plus" | "one_or_more" => Ok(format!("({})+", child_src("expr")?)),
        "opt" | "optional" => Ok(format!("({})?", child_src("expr")?)),
        "and" => Ok(format!("&({})", child_src("expr")?)),
        "not" => Ok(format!("!({})", child_src("expr")?)),
        "eager" | "and_eager" => Ok(format!("!!({})", child_src("expr")?)),
        "named" | "bind" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("name".to_string()))?;
            Ok(format!("{name}:({})", child_src("expr")?))
        }
        "expected" => child_src("expr"),
        "ref" => {
            let name = obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("name".to_string()))?;
            Ok(name.to_string())
        }
        "imported_ref" | "import" => {
            let grammar = obj
                .get("grammar_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("grammar_name".to_string()))?;
            let rule = obj
                .get("rule_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("rule_name".to_string()))?;
            Ok(format!("{grammar}::{rule}"))
        }
        "grammar_scope" | "scope" => {
            let grammar = obj
                .get("grammar_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("grammar_name".to_string()))?;
            Ok(format!("scope('{grammar}', {})", child_src("expr")?))
        }
        "no_trivia" | "tight" => Ok(format!("tight({})", child_src("expr")?)),
        "island" => {
            let start = obj
                .get("start")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("island start".to_string()))?;
            let end = obj
                .get("end")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("island end".to_string()))?;
            let include_delims =
                optional_bool_field(obj, "include_delims", "island include_delims")?
                    .unwrap_or(false);
            if include_delims {
                Ok(format!(
                    "island({}, {}, true)",
                    quote_rule_string(start),
                    quote_rule_string(end)
                ))
            } else {
                Ok(format!(
                    "island({}, {})",
                    quote_rule_string(start),
                    quote_rule_string(end)
                ))
            }
        }
        "sep_plus" | "sep_one_or_more" | "gather" => {
            let sep = child_src("sep")?;
            let body = child_src("expr")?;
            Ok(format!("({body}) ++ ({sep})"))
        }
        "interspersed" => {
            let elem = child_src("expr")?;
            let sep = child_src("sep")?;
            Ok(format!("interspersed({elem}, {sep})"))
        }
        "raw_block" => {
            let start = obj
                .get("start")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("raw_block start".to_string()))?;
            let end = obj
                .get("end")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("raw_block end".to_string()))?;
            if let Some(delim_kind) = optional_str_field(obj, "delim_kind", "raw_block delim_kind")?
            {
                Ok(format!(
                    "raw_block({}, {}, {})",
                    quote_rule_string(start),
                    quote_rule_string(end),
                    quote_rule_string(delim_kind)
                ))
            } else {
                Ok(format!(
                    "raw_block({}, {})",
                    quote_rule_string(start),
                    quote_rule_string(end)
                ))
            }
        }
        other => Err(SpecCompileError::UnknownTag(other.to_string())),
    }
}

fn mapping_expr_to_peg_expr(
    obj: &serde_json::Map<String, Value>,
) -> Result<PegExpr, SpecCompileError> {
    let kind = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SpecCompileError::MissingField("type".to_string()))?;
    let child = |key: &str| -> Result<PegExpr, SpecCompileError> {
        expr_to_peg_expr(
            obj.get(key)
                .ok_or_else(|| SpecCompileError::MissingField(key.to_string()))?,
        )
    };
    let children = |key: &str| -> Result<Vec<PegExpr>, SpecCompileError> {
        let arr = obj
            .get(key)
            .and_then(|v| v.as_array())
            .ok_or_else(|| SpecCompileError::MissingField(key.to_string()))?;
        arr.iter().map(expr_to_peg_expr).collect()
    };

    match kind {
        "literal" | "lit" => Ok(PegExpr::Literal(
            obj.get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("text".to_string()))?
                .to_string(),
        )),
        "regex" => {
            let pattern = obj
                .get("pattern")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("pattern".to_string()))?;
            Ok(PegExpr::Regex(
                CompiledRegex::new(pattern, 0, pattern.len())
                    .map_err(|err| SpecCompileError::InvalidFormat(err.message.into()))?,
            ))
        }
        "token_ref" | "tok" => {
            let kind = optional_non_empty_str_field(obj, "kind", "tok kind")?.map(str::to_string);
            let text = optional_non_empty_str_field(obj, "text", "tok text")?.map(str::to_string);
            Ok(PegExpr::TokenRef { kind, text })
        }
        "soft_keyword" | "soft_kw" => Ok(PegExpr::SoftKeyword(
            obj.get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("text".to_string()))?
                .to_string(),
        )),
        "param" => Ok(PegExpr::Parameter {
            name: obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("name".to_string()))?
                .to_string(),
        }),
        "call" => Ok(PegExpr::Call {
            rule: obj
                .get("rule")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("rule".to_string()))?
                .to_string(),
            args: children("args")?,
        }),
        "newline" => Ok(PegExpr::Newline),
        "indent" => Ok(PegExpr::Indent),
        "dedent" => Ok(PegExpr::Dedent),
        "cut" | "~" => Ok(PegExpr::Cut),
        "seq" | "sequence" => children("parts").map(PegExpr::Sequence),
        "choice" => children("options").map(PegExpr::Choice),
        "star" | "many" | "zero_or_more" => Ok(PegExpr::ZeroOrMore(Box::new(child("expr")?))),
        "plus" | "one_or_more" => Ok(PegExpr::OneOrMore(Box::new(child("expr")?))),
        "opt" | "optional" => Ok(PegExpr::Optional(Box::new(child("expr")?))),
        "and" => Ok(PegExpr::And(Box::new(child("expr")?))),
        "not" => Ok(PegExpr::Not(Box::new(child("expr")?))),
        "eager" | "and_eager" => Ok(PegExpr::Eager(Box::new(child("expr")?))),
        "named" | "bind" => Ok(PegExpr::Named {
            name: obj
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("name".to_string()))?
                .to_string(),
            expr: Box::new(child("expr")?),
        }),
        "expected" => Ok(PegExpr::Expected {
            message: obj
                .get("message")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("message".to_string()))?
                .to_string(),
            expr: Box::new(child("expr")?),
        }),
        "ref" => Ok(PegExpr::Ref(
            obj.get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("name".to_string()))?
                .to_string(),
        )),
        "imported_ref" | "import" => Ok(PegExpr::ImportedRef {
            grammar_name: obj
                .get("grammar_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("grammar_name".to_string()))?
                .to_string(),
            rule_name: obj
                .get("rule_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("rule_name".to_string()))?
                .to_string(),
        }),
        "grammar_scope" | "scope" => Ok(PegExpr::GrammarScope {
            grammar_name: obj
                .get("grammar_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("grammar_name".to_string()))?
                .to_string(),
            expr: Box::new(child("expr")?),
        }),
        "no_trivia" | "tight" => Ok(PegExpr::NoTrivia(Box::new(child("expr")?))),
        "sep_plus" | "sep_one_or_more" | "gather" => Ok(PegExpr::SepOneOrMore {
            separator: Box::new(child("sep")?),
            element: Box::new(child("expr")?),
        }),
        "interspersed" => Ok(PegExpr::Interspersed {
            element: Box::new(child("expr")?),
            separator: Box::new(child("sep")?),
        }),
        "island" => Ok(PegExpr::Island {
            start: obj
                .get("start")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("island start".to_string()))?
                .to_string(),
            end: obj
                .get("end")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("island end".to_string()))?
                .to_string(),
            include_delims: optional_bool_field(obj, "include_delims", "island include_delims")?
                .unwrap_or(false),
        }),
        "raw_block" => Ok(PegExpr::RawBlock {
            start: obj
                .get("start")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("raw_block start".to_string()))?
                .to_string(),
            end: obj
                .get("end")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SpecCompileError::MissingField("raw_block end".to_string()))?
                .to_string(),
            delim_kind: optional_str_field(obj, "delim_kind", "raw_block delim_kind")?
                .unwrap_or("block")
                .to_string(),
        }),
        _ => {
            let source = mapping_expr_to_source(obj)?;
            RuleTextParser::parse(&source)
                .map_err(|err| SpecCompileError::InvalidFormat(err.message.into()))
        }
    }
}
