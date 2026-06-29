/// CAAP surface-syntax frontend: parses source text into an `IRGraph`.
///
/// Uses the Rust PEG port (`caap_peg`) with the same grammar as
/// `caap/surface/grammar.py`:
///
///   form    = list | string | integer | boolean | null | symbol
///   list    = '(' form* ')'
///   string  = "(?:[^"\\]|\\.)*"
///   integer = -?(?:0|[1-9][0-9]*)
///   boolean = 'true' | 'false'
///   null    = 'null'
///   symbol  = [A-Za-z_+\-*/<>=!?$%&:.][A-Za-z0-9_+\-*/<>=!?$%&:.]*
///
/// Trivia (same as the surface grammar): whitespace, `;` line comments,
/// `#| … |#` and `/* … */` block comments.
use caap_peg::{parse_ast_with_max_steps, Grammar};

use crate::error::{CaapError, CaapResult};
use crate::graph::{GraphBuilder, IRGraph};
use crate::ir::NodeId;

mod eval;
mod grammar;
mod model;
mod reader;
mod validation;

pub use eval::{eval_source, evaluator_from_source};
pub use model::{ParsedForm, ParsedSource};
pub use reader::{
    any_directive_possible, default_reader_directives, read_segmental, ReaderDirective, ReaderState,
};
pub use validation::{
    ast_json, canonicalize_parsed_form, canonicalize_parsed_source, canonicalize_source,
    check_source,
};

// ── Public API ─────────────────────────────────────────────────────────────

/// Parse CAAP surface text into a typed surface-form model.
pub fn parse_forms(source: &str) -> CaapResult<ParsedSource> {
    parse_forms_with_optional_source_path(source, None)
}

/// Parse CAAP surface text into typed forms and attach a source path to spans.
pub fn parse_forms_with_source_path(
    source: &str,
    source_path: impl AsRef<str>,
) -> CaapResult<ParsedSource> {
    parse_forms_with_optional_source_path(source, Some(source_path.as_ref()))
}

fn parse_forms_with_optional_source_path(
    source: &str,
    source_path: Option<&str>,
) -> CaapResult<ParsedSource> {
    let grammar = grammar::surface_grammar();
    let (parse_source, source_offset) = trimmed_parse_source(source);
    guard_surface_nesting(parse_source, source_path)?;
    let ast =
        parse_ast_with_max_steps(grammar, parse_source, Some("forms"), None).map_err(|e| {
            CaapError::parse(surface_parse_error_message(
                "forms",
                parse_source,
                source_path,
                &e.message,
            ))
        })?;
    ast_to_parsed_source(&ast, parse_source, source, source_offset, source_path)
}

/// Reparse CAAP surface text with a specific base surface grammar rule.
///
/// This is the frontend equivalent of `ParserSession.reparse()` for the
/// grammar currently owned by this frontend. It returns the typed surface-form
/// model for rules that project to surface forms (`form`, atom rules, `list`,
/// and `forms`).
pub fn reparse_surface_rule(rule_name: &str, source: &str) -> CaapResult<ParsedSource> {
    let grammar = grammar::surface_grammar();
    let (parse_source, source_offset) = trimmed_parse_source(source);
    guard_surface_nesting(parse_source, None)?;
    let ast =
        parse_ast_with_max_steps(grammar, parse_source, Some(rule_name), None).map_err(|e| {
            CaapError::parse(surface_parse_error_message(
                rule_name,
                parse_source,
                None,
                &e.message,
            ))
        })?;
    ast_to_parsed_source(&ast, parse_source, source, source_offset, None)
}

/// One step of a segmental read: a single parsed top-level form and the byte
/// offset into the original source to resume from.
#[derive(Clone, Debug)]
pub struct ReadStep {
    /// The parsed form.
    pub form: ParsedForm,
    /// Absolute byte offset in the original source to continue reading from.
    pub next_pos: usize,
}

/// Parse the **next single** top-level form from `source` starting at byte
/// `pos`, using `grammar` (the root surface grammar, or a host-extended one).
///
/// This is the segmental-reader counterpart to [`parse_forms`]: instead of
/// parsing the whole source at once it reads one form and reports where to
/// resume, so a caller can evaluate the form (possibly extending the grammar)
/// before reading the next. Returns `Ok(None)` once only trivia / end-of-input
/// remains. Spans are absolute (relative to `source`).
///
/// `grammar` must produce the standard surface-form node kinds (`form`, `list`,
/// `symbol`, `string`, `integer`, `boolean`, `null`); the form is lowered with
/// the same projection as [`parse_forms`].
pub fn parse_next_form(
    grammar: &Grammar,
    source: &str,
    pos: usize,
    source_path: Option<&str>,
) -> CaapResult<Option<ReadStep>> {
    let remaining = source.get(pos..).unwrap_or("");
    // Skip leading trivia (whitespace + default comments) so we can tell
    // "only trivia / EOF remains" (→ None) apart from a genuine malformed form
    // (→ Err below). `parse_source` then begins exactly at the form.
    let parse_source = strip_leading_trivia(remaining);
    if parse_source.is_empty() {
        return Ok(None);
    }
    let leading = remaining.len() - parse_source.len();
    let abs_offset = pos + leading;
    guard_surface_nesting(parse_source, source_path)?;
    // `parse_ast` requires the rule to consume its whole input, so first take a
    // *prefix* match of one `form` to learn how many bytes it spans, then build
    // the AST from exactly that slice (`parse_source` begins at the form, so a
    // failure here is a genuinely malformed form, not trailing trivia).
    //
    // The step budget mirrors `parse_ast_with_max_steps(None)` (which sizes to
    // the input) so a large remaining tail is not rejected by the default cap.
    let steps = parse_source.len().saturating_add(65536).max(65536);
    let prefix = caap_peg::ParseRequest::new(grammar)
        .start_rule("form")
        .config(caap_peg::ParserConfig::default().with_max_steps(steps))
        .run_prefix(parse_source, 0);
    let consumed = match prefix.value {
        Some(_) => prefix.consumed,
        None => {
            return Err(CaapError::parse(surface_parse_error_message(
                "form",
                parse_source,
                source_path,
                &prefix.errors.join("; "),
            )))
        }
    };
    let form_source = &parse_source[..consumed];
    let ast = parse_ast_with_max_steps(grammar, form_source, Some("form"), None).map_err(|e| {
        CaapError::parse(surface_parse_error_message(
            "form",
            form_source,
            source_path,
            &e.message,
        ))
    })?;
    let parsed = ast_to_parsed_source(&ast, form_source, source, abs_offset, source_path)?;
    let form = parsed.forms.into_iter().next().ok_or_else(|| {
        CaapError::parse("parse_next_form: grammar matched but produced no form".to_string())
    })?;
    Ok(Some(ReadStep {
        form,
        next_pos: abs_offset + consumed,
    }))
}

/// Byte length of leading default trivia in `s`: whitespace, `;` line comments,
/// and `#| … |#` / `/* … */` block comments — the root surface grammar's default
/// skip set, and the one place that knowledge lives.
///
/// Returns `Err(marker)` (the `"#| |#"` / `"/* */"` pair) on an unterminated
/// block comment; lenient callers can treat that as "trivia to end of input".
pub(crate) fn leading_trivia_len(s: &str) -> Result<usize, &'static str> {
    let mut i = 0;
    loop {
        let rest = &s[i..];
        let ws = rest.len() - rest.trim_start().len();
        if ws > 0 {
            i += ws;
            continue;
        }
        if rest.starts_with(';') {
            match rest.find('\n') {
                Some(n) => i += n + 1,
                None => return Ok(s.len()),
            }
        } else if rest.starts_with("#|") {
            match rest.find("|#") {
                Some(n) => i += n + 2,
                None => return Err("#| |#"),
            }
        } else if rest.starts_with("/*") {
            match rest.find("*/") {
                Some(n) => i += n + 2,
                None => return Err("/* */"),
            }
        } else {
            return Ok(i);
        }
    }
}

/// Strip leading default trivia, lenient: an unterminated block comment counts as
/// trivia to end of input. `parse_next_form` uses this to recognise an all-trivia
/// / EOF remainder (so trailing comments read as end-of-input, not a bad form).
fn strip_leading_trivia(s: &str) -> &str {
    &s[leading_trivia_len(s).unwrap_or(s.len())..]
}

/// A surface syntax error located by the error-tolerant parser: a span into the
/// original (untrimmed) source plus a human-readable message. Unlike
/// [`parse_forms`], locating errors never fails — it is meant for tooling (the
/// LSP) that must point at the offending text even when the input does not parse.
#[derive(Clone, Debug)]
pub struct SurfaceSyntaxError {
    /// Region of the source the grammar could not match.
    pub span: crate::source::SourceSpan,
    /// What was rejected, for display in an editor diagnostic.
    pub message: String,
}

/// Locate surface syntax errors without failing.
///
/// Runs the error-tolerant PEG parse (`parse_ast_tolerant`) and returns a span
/// for every region the grammar could not match, mapped back onto the original
/// (untrimmed) `source` so positions line up with editor coordinates. A clean
/// parse yields an empty vector. Pathologically nested input (which would
/// overflow the recursive-descent matcher) is reported as a single
/// whole-input error rather than risking a crash.
pub fn surface_syntax_errors(source: &str, source_path: Option<&str>) -> Vec<SurfaceSyntaxError> {
    let (parse_source, source_offset) = trimmed_parse_source(source);
    if parse_source.is_empty() {
        return Vec::new();
    }
    if guard_surface_nesting(parse_source, source_path).is_err() {
        return vec![whole_source_syntax_error(
            source,
            source_offset,
            parse_source.len(),
            source_path,
            "surface nesting too deep to parse",
        )];
    }
    let grammar = grammar::surface_grammar();
    let root = caap_peg::parse_ast_tolerant(grammar, parse_source, Some("forms"));
    if !root.error {
        return Vec::new();
    }
    let line_offsets = compute_line_offsets(source);
    let mut errors = Vec::new();
    collect_error_nodes(
        &root,
        parse_source,
        &line_offsets,
        source_offset,
        source_path,
        &mut errors,
    );
    if errors.is_empty() {
        // The root flagged an error but carried no explicit error node (e.g. a
        // zero-width tail); fall back to a single end-of-input marker.
        errors.push(whole_source_syntax_error(
            source,
            source_offset,
            parse_source.len(),
            source_path,
            "unexpected end of input",
        ));
    }
    errors
}

/// Walk the tolerant AST, recording one [`SurfaceSyntaxError`] per synthetic
/// error node ([`caap_peg::ast::ERROR_RULE`]). Error nodes are leaves, so once
/// one is found its subtree is not descended.
fn collect_error_nodes(
    node: &caap_peg::ast::AstNode,
    parse_source: &str,
    line_offsets: &[usize],
    source_offset: usize,
    source_path: Option<&str>,
    out: &mut Vec<SurfaceSyntaxError>,
) {
    if node.rule == caap_peg::ast::ERROR_RULE {
        let start = node.span.start.min(parse_source.len());
        let end = node.span.end.min(parse_source.len());
        if let Ok(span) =
            source_span_for_offsets(start, end, line_offsets, source_offset, source_path)
        {
            let snippet: String = parse_source[start..end].chars().take(40).collect();
            let message = if snippet.trim().is_empty() {
                "unexpected end of input".to_string()
            } else {
                format!("unexpected input: {:?}", snippet.trim())
            };
            out.push(SurfaceSyntaxError { span, message });
        }
        return;
    }
    for child in node.children.iter() {
        collect_error_nodes(
            child,
            parse_source,
            line_offsets,
            source_offset,
            source_path,
            out,
        );
    }
}

/// A single syntax error covering the whole parsed region — the fallback when a
/// precise error node is unavailable.
fn whole_source_syntax_error(
    source: &str,
    source_offset: usize,
    parse_len: usize,
    source_path: Option<&str>,
    message: &str,
) -> SurfaceSyntaxError {
    let line_offsets = compute_line_offsets(source);
    let span = source_span_for_offsets(0, parse_len, &line_offsets, source_offset, source_path)
        .unwrap_or_else(|_| {
            crate::source::SourceSpan::new(0, 0, 1, 1, 1, 1).expect("trivial span is valid")
        });
    SurfaceSyntaxError {
        span,
        message: message.to_string(),
    }
}

fn surface_parse_error_message(
    rule_name: &str,
    source: &str,
    source_path: Option<&str>,
    message: &str,
) -> String {
    if std::env::var_os("CAAP_SURFACE_PARSE_CONTEXT").is_none() {
        return message.to_string();
    }
    let path = source_path.unwrap_or("<inline>");
    let snippet: String = source.chars().take(240).collect();
    format!("surface parse failed rule={rule_name} source={path} snippet={snippet:?}: {message}")
}

fn trimmed_parse_source(source: &str) -> (&str, usize) {
    let trimmed = source.trim();
    let offset = if trimmed.is_empty() {
        0
    } else {
        source.len() - source.trim_start().len()
    };
    (trimmed, offset)
}

/// Maximum bracket-nesting depth the surface parser will attempt.
///
/// The PEG matcher is recursive-descent, so unbounded bracket nesting overflows
/// the native thread stack and aborts the process instead of producing a
/// diagnostic. The cap sits an order of magnitude above any real program (the
/// whole stdlib peaks at depth 26) yet well under the overflow threshold, so
/// only pathological input is rejected — with an error, not a crash.
const MAX_SURFACE_NESTING_DEPTH: usize = 256;

/// Reject input whose bracket nesting would overflow the recursive-descent
/// parser. Brackets inside string literals and `;` line comments are ignored.
fn guard_surface_nesting(source: &str, source_path: Option<&str>) -> CaapResult<()> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut in_comment = false;
    for ch in source.chars() {
        if in_comment {
            if ch == '\n' {
                in_comment = false;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            ';' => in_comment = true,
            '(' | '[' | '{' => {
                depth += 1;
                if depth > MAX_SURFACE_NESTING_DEPTH {
                    let path = source_path.unwrap_or("<inline>");
                    return Err(CaapError::parse(format!(
                        "surface nesting exceeds the maximum depth of \
                         {MAX_SURFACE_NESTING_DEPTH} in {path}: refusing to parse to \
                         avoid a parser stack overflow"
                    )));
                }
            }
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// Parse CAAP surface text and return an `IRGraph`.
///
/// All top-level forms are registered in the graph's `top_level_forms` list.
pub fn parse(source: &str) -> CaapResult<IRGraph> {
    parse_with_optional_source_path(source, None)
}

/// Parse CAAP surface text and attach a source path to every lowered span.
pub fn parse_with_source_path(source: &str, source_path: impl AsRef<str>) -> CaapResult<IRGraph> {
    parse_with_optional_source_path(source, Some(source_path.as_ref()))
}

/// Parse CAAP source **segmentally** into one IR graph. Top-level reader
/// directives (consumed; never part of the program) shape how the *following*
/// forms are read:
///
/// - `(extend_syntax "rule" "peg-source")` — replace a rule in the live grammar.
/// - `(define_grammar "name" "rule" "peg-source")` — register/extend a *named*
///   grammar (a base clone with rule overrides; call repeatedly to add rules).
/// - `(begin_scope "name")` … `(end_scope)` — read the forms between them with
///   the named grammar, restoring the previous grammar afterwards. Scopes nest
///   (a grammar stack), and an `extend_syntax` inside a scope reverts with it.
///
/// Forms are read one at a time so directives take effect mid-source, but the
/// assembled graph is whole-program (hoisted, forward-reference-capable eval is
/// unchanged). For source with no directive the graph is identical to [`parse`].
pub fn parse_segmental(source: &str) -> CaapResult<IRGraph> {
    parse_segmental_with_optional_source_path(source, None)
}

/// [`parse_segmental`] with a source path attached to spans.
pub fn parse_segmental_with_source_path(
    source: &str,
    source_path: impl AsRef<str>,
) -> CaapResult<IRGraph> {
    parse_segmental_with_optional_source_path(source, Some(source_path.as_ref()))
}

fn parse_segmental_with_optional_source_path(
    source: &str,
    source_path: Option<&str>,
) -> CaapResult<IRGraph> {
    // The read loop and directive set are the segmental-reader mechanism; see
    // `reader`. Fast path: no directive can occur unless its trigger token
    // appears, so ordinary source parses whole-file — byte-identical to `parse`.
    let directives = reader::default_reader_directives();
    if !reader::any_directive_possible(source, &directives) {
        return parse_with_grammar(grammar::surface_grammar(), source, source_path);
    }
    reader::read_segmental(source, source_path, &directives)
}

/// Lower an already typed surface model into IR.
///
/// Dynamic syntax parsing produces the same surface-form model as the base
/// parser after semantic hooks have run, so it enters the compiler through
/// this lowering path instead of being reparsed as text.
pub fn parsed_source_to_ir(parsed: &ParsedSource) -> CaapResult<IRGraph> {
    let mut builder = GraphBuilder::new();
    let mut labels: std::collections::HashMap<String, NodeId> = std::collections::HashMap::new();
    for form in &parsed.forms {
        let id = lower_parsed_form(form, &mut builder, &mut labels)?;
        builder.graph.add_top_level_form(id)?;
    }
    Ok(builder.graph)
}

fn parse_with_optional_source_path(source: &str, source_path: Option<&str>) -> CaapResult<IRGraph> {
    parse_with_grammar(grammar::surface_grammar(), source, source_path)
}

/// Whole-source AST→IR lowering against a specific grammar (the complete kernel
/// lowering used by [`parse`]). Shared by the static and segmental front ends.
fn parse_with_grammar(
    grammar: &Grammar,
    source: &str,
    source_path: Option<&str>,
) -> CaapResult<IRGraph> {
    let (parse_source, source_offset) = trimmed_parse_source(source);
    guard_surface_nesting(parse_source, source_path)?;
    let ast =
        parse_ast_with_max_steps(grammar, parse_source, Some("forms"), None).map_err(|e| {
            CaapError::parse(surface_parse_error_message(
                "forms",
                parse_source,
                source_path,
                &e.message,
            ))
        })?;
    ast_to_ir(&ast, parse_source, source, source_offset, source_path)
}

mod lower;
use lower::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::Evaluator;
    use crate::source::SourceSpan;
    use crate::values::RuntimeValue;

    #[test]
    fn parse_next_form_reads_forms_one_at_a_time() {
        let grammar = grammar::surface_grammar();
        let src = "(int_add 1 2)\nhello";
        let step1 = parse_next_form(grammar, src, 0, None)
            .unwrap()
            .expect("first form");
        assert!(matches!(step1.form, ParsedForm::List { .. }));
        let step2 = parse_next_form(grammar, src, step1.next_pos, None)
            .unwrap()
            .expect("second form");
        assert!(matches!(step2.form, ParsedForm::Symbol { .. }));
        assert!(parse_next_form(grammar, src, step2.next_pos, None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn parse_next_form_treats_trailing_trivia_as_eof() {
        let grammar = grammar::surface_grammar();
        // whitespace, line comment, and block comment after the form are EOF.
        let src = "alpha\n  ; trailing comment\n  #| block |#  ";
        let step = parse_next_form(grammar, src, 0, None)
            .unwrap()
            .expect("the form");
        assert!(parse_next_form(grammar, src, step.next_pos, None)
            .unwrap()
            .is_none());
        // pure trivia / empty also yields None.
        assert!(parse_next_form(grammar, "  \n ; only a comment\n", 0, None)
            .unwrap()
            .is_none());
        assert!(parse_next_form(grammar, "", 0, None).unwrap().is_none());
    }

    #[test]
    fn parse_next_form_errors_on_malformed_form() {
        let grammar = grammar::surface_grammar();
        assert!(parse_next_form(grammar, "(unclosed", 0, None).is_err());
    }

    #[test]
    fn parse_next_form_spans_are_absolute_to_source() {
        let grammar = grammar::surface_grammar();
        let src = "alpha (beta)"; // second form starts at byte 6
        let step1 = parse_next_form(grammar, src, 0, None).unwrap().unwrap();
        let step2 = parse_next_form(grammar, src, step1.next_pos, None)
            .unwrap()
            .unwrap();
        let ParsedForm::List { span, .. } = &step2.form else {
            panic!("expected list form");
        };
        assert_eq!(span.start, 6);
    }

    fn run(src: &str) -> RuntimeValue {
        eval_source(src).expect("eval_source failed")
    }

    #[test]
    fn trimmed_parse_source_computes_offset_without_search_fallback() {
        let source = " \n\talpha alpha\t\n";

        let (trimmed, offset) = trimmed_parse_source(source);

        assert_eq!(trimmed, "alpha alpha");
        assert_eq!(offset, " \n\t".len());
    }

    #[test]
    fn pathological_nesting_errors_instead_of_overflowing_the_stack() {
        // Far past the recursive-descent overflow threshold: must be a parse
        // diagnostic, never a panic/abort.
        let src = "(".repeat(5000);
        let err = parse(&src).expect_err("deep nesting should be rejected");
        assert!(
            format!("{err:?}").contains("nesting"),
            "expected a nesting-depth diagnostic, got {err:?}"
        );
    }

    #[test]
    fn nesting_just_below_the_cap_still_parses() {
        // A valid, deeply-but-legally nested expression (well above any real
        // program, comfortably below the cap) must still parse.
        let depth = MAX_SURFACE_NESTING_DEPTH - 8;
        let src = format!("{}1{}", "(int_add ".repeat(depth), ")".repeat(depth));
        assert!(
            parse_forms(&src).is_ok(),
            "nesting below the cap should parse"
        );
    }

    #[test]
    fn brackets_in_strings_do_not_count_toward_nesting() {
        // A string literal full of open brackets is not nesting.
        let src = format!("(print \"{}\")", "(".repeat(MAX_SURFACE_NESTING_DEPTH + 50));
        assert!(
            guard_surface_nesting(&src, None).is_ok(),
            "brackets inside a string must not trip the guard"
        );
    }

    // ── Atoms ──────────────────────────────────────────────────────────────

    #[test]
    fn parse_null() {
        assert_eq!(run("null"), RuntimeValue::Null);
    }

    #[test]
    fn parse_bool_true() {
        assert_eq!(run("true"), RuntimeValue::Bool(true));
    }

    #[test]
    fn parse_bool_false() {
        assert_eq!(run("false"), RuntimeValue::Bool(false));
    }

    #[test]
    fn parse_positive_int() {
        assert_eq!(run("42"), RuntimeValue::Int(42));
    }

    #[test]
    fn parse_negative_int() {
        assert_eq!(run("-7"), RuntimeValue::Int(-7));
    }

    #[test]
    fn parse_zero() {
        assert_eq!(run("0"), RuntimeValue::Int(0));
    }

    #[test]
    fn parse_string() {
        assert_eq!(run(r#""hello""#), RuntimeValue::Str("hello".into()));
    }

    #[test]
    fn parse_string_escape_newline() {
        assert_eq!(run(r#""a\nb""#), RuntimeValue::Str("a\nb".into()));
    }

    #[test]
    fn parse_empty_list_is_null() {
        assert_eq!(run("()"), RuntimeValue::Null);
    }

    // ── Calls ──────────────────────────────────────────────────────────────

    #[test]
    fn parse_add() {
        assert_eq!(run("(int_add 1 2)"), RuntimeValue::Int(3));
    }

    #[test]
    fn parse_nested() {
        assert_eq!(run("(int_add (int_mul 2 3) 4)"), RuntimeValue::Int(10));
    }

    #[test]
    fn parse_forms_exposes_typed_surface_model() {
        let parsed = parse_forms(r#"(int_add 1 "a\n") null"#).unwrap();
        assert_eq!(parsed.forms.len(), 2);

        assert!(
            matches!(&parsed.forms[0], ParsedForm::List { .. }),
            "expected top-level list form"
        );
        let ParsedForm::List { items, span } = &parsed.forms[0] else {
            return;
        };
        assert_eq!(span.start, 0);
        assert_eq!(span.start_line, 1);
        assert_eq!(parsed.forms[0].head_symbol(), Some("int_add"));
        assert_eq!(
            items[0],
            ParsedForm::Symbol {
                text: "int_add".to_string(),
                span: SourceSpan::new(1, 8, 1, 2, 1, 9).unwrap(),
            }
        );
        assert_eq!(
            items[1],
            ParsedForm::Integer {
                value: 1,
                raw: "1".to_string(),
                span: SourceSpan::new(9, 10, 1, 10, 1, 11).unwrap(),
            }
        );
        assert_eq!(
            items[2],
            ParsedForm::String {
                value: "a\n".to_string(),
                raw: r#""a\n""#.to_string(),
                span: SourceSpan::new(11, 16, 1, 12, 1, 17).unwrap(),
            }
        );

        assert_eq!(
            parsed.forms[1],
            ParsedForm::Null {
                span: SourceSpan::new(18, 22, 1, 19, 1, 23).unwrap(),
            }
        );
    }

    #[test]
    fn parse_forms_parses_float_literals() {
        // Regression: the surface ParsedForm path used to only handle i64 and
        // rejected float literals with "invalid integer", while the IR path
        // accepted them. A '.', 'e', or 'E' now yields ParsedForm::Float.
        let parsed = parse_forms("12.5 -1.0 1e3 42").unwrap();
        assert!(matches!(
            &parsed.forms[0],
            ParsedForm::Float { value, .. } if (*value - 12.5).abs() < 1e-12
        ));
        assert!(matches!(&parsed.forms[1], ParsedForm::Float { value, .. } if *value == -1.0));
        assert!(matches!(&parsed.forms[2], ParsedForm::Float { value, .. } if *value == 1000.0));
        assert!(matches!(
            &parsed.forms[3],
            ParsedForm::Integer { value: 42, .. }
        ));
    }

    #[test]
    fn parse_forms_keeps_dotted_symbols_whole() {
        let parsed = parse_forms("(stdlib.pass_kit.register_provider)").unwrap();
        assert!(
            matches!(&parsed.forms[0], ParsedForm::List { .. }),
            "expected top-level list form"
        );
        let ParsedForm::List { items, .. } = &parsed.forms[0] else {
            return;
        };
        assert_eq!(items.len(), 1);
        assert_eq!(
            parsed.forms[0].head_symbol(),
            Some("stdlib.pass_kit.register_provider")
        );
        assert_eq!(
            items[0],
            ParsedForm::Symbol {
                text: "stdlib.pass_kit.register_provider".to_string(),
                span: SourceSpan::new(1, 34, 1, 2, 1, 35).unwrap(),
            }
        );
    }

    #[test]
    fn eval_resolves_qualified_name_through_map_prefix() {
        assert_eq!(
            run(r#"(bind ((stdlib.pass_kit (map_of "answer" 42))) stdlib.pass_kit.answer)"#),
            RuntimeValue::Int(42)
        );
    }

    #[test]
    fn check_and_canonicalize_source_use_typed_surface_forms() {
        check_source(" ; ignored\n( int_add 1 (int_mul 2 3) )").unwrap();
        assert!(check_source("(int_add 1 @bad)").is_err());
        assert_eq!(
            canonicalize_source(
                r#"
                  ( int_add 1
                    (string_size "a\nb\"c") )
                  null
                "#
            )
            .unwrap(),
            "(int_add 1 (string_size \"a\\nb\\\"c\"))\nnull"
        );
    }

    #[test]
    fn ast_json_projects_typed_surface_forms() {
        let json = ast_json("(int_add 1)").unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["forms"][0]["List"]["span"]["start"], 0);
        assert_eq!(
            value["forms"][0]["List"]["items"][0]["Symbol"]["text"],
            "int_add"
        );
        assert_eq!(value["forms"][0]["List"]["items"][1]["Integer"]["value"], 1);
    }

    #[test]
    fn parse_multiple_top_forms_returns_last() {
        assert_eq!(run("1 2 3"), RuntimeValue::Int(3));
    }

    #[test]
    fn lower_bind_with_empty_binding_list_to_body() {
        assert_eq!(run("(bind () 42)"), RuntimeValue::Int(42));
        assert_eq!(run("(bind () 1 2)"), RuntimeValue::Int(2));
    }

    #[test]
    fn parsed_source_to_ir_lowers_special_forms() {
        let parsed = ParsedSource {
            forms: vec![ParsedForm::List {
                items: vec![
                    ParsedForm::Symbol {
                        text: "bind".to_string(),
                        span: SourceSpan::new(1, 5, 1, 2, 1, 6).unwrap(),
                    },
                    ParsedForm::List {
                        items: vec![ParsedForm::List {
                            items: vec![
                                ParsedForm::Symbol {
                                    text: "x".to_string(),
                                    span: SourceSpan::new(8, 9, 1, 9, 1, 10).unwrap(),
                                },
                                ParsedForm::Integer {
                                    value: 41,
                                    raw: "41".to_string(),
                                    span: SourceSpan::new(10, 12, 1, 11, 1, 13).unwrap(),
                                },
                            ],
                            span: SourceSpan::new(7, 13, 1, 8, 1, 14).unwrap(),
                        }],
                        span: SourceSpan::new(6, 14, 1, 7, 1, 15).unwrap(),
                    },
                    ParsedForm::List {
                        items: vec![
                            ParsedForm::Symbol {
                                text: "int_add".to_string(),
                                span: SourceSpan::new(16, 23, 1, 17, 1, 24).unwrap(),
                            },
                            ParsedForm::Symbol {
                                text: "x".to_string(),
                                span: SourceSpan::new(24, 25, 1, 25, 1, 26).unwrap(),
                            },
                            ParsedForm::Integer {
                                value: 1,
                                raw: "1".to_string(),
                                span: SourceSpan::new(26, 27, 1, 27, 1, 28).unwrap(),
                            },
                        ],
                        span: SourceSpan::new(15, 28, 1, 16, 1, 29).unwrap(),
                    },
                ],
                span: SourceSpan::new(0, 29, 1, 1, 1, 30).unwrap(),
            }],
        };
        let graph = parsed_source_to_ir(&parsed).unwrap();
        let mut ev = Evaluator::new(graph);
        assert_eq!(ev.run().unwrap(), RuntimeValue::Int(42));
    }

    // ── Trivia ─────────────────────────────────────────────────────────────

    #[test]
    fn line_comment_skipped() {
        assert_eq!(run("; ignored\n42"), RuntimeValue::Int(42));
    }

    #[test]
    fn block_comment_hash_pipe() {
        assert_eq!(run("#| ignored |# 99"), RuntimeValue::Int(99));
    }

    #[test]
    fn block_comment_c_style() {
        assert_eq!(run("/* ignored */ 77"), RuntimeValue::Int(77));
    }

    // ── Real programs ──────────────────────────────────────────────────────

    #[test]
    fn eval_factorial() {
        let src = r#"
            (do
              (bind fact
                (lambda (n)
                  (if (eq n 0)
                    1
                    (int_mul n (fact (int_add n -1))))))
              (fact 5))
        "#;
        assert_eq!(run(src), RuntimeValue::Int(120));
    }

    #[test]
    fn eval_fibonacci() {
        let src = r#"
            (do
              (bind fib
                (lambda (n)
                  (if (lt n 2)
                    n
                    (int_add (fib (int_add n -1)) (fib (int_add n -2))))))
              (fib 8))
        "#;
        assert_eq!(run(src), RuntimeValue::Int(21));
    }

    #[test]
    fn eval_sequence_map() {
        let src = r#"
            (sequence_map (list_of 1 2 3 4 5) (lambda (x) (int_mul x x)))
        "#;
        let result = run(src);
        assert!(matches!(&result, RuntimeValue::List(_)), "expected list");
        let RuntimeValue::List(l) = result else {
            return;
        };
        let v = l.borrow().clone();
        assert_eq!(
            v,
            vec![
                RuntimeValue::Int(1),
                RuntimeValue::Int(4),
                RuntimeValue::Int(9),
                RuntimeValue::Int(16),
                RuntimeValue::Int(25),
            ]
        );
    }

    #[test]
    fn eval_string_concat() {
        assert_eq!(
            run(r#"(string_concat_many "hello" " " "world")"#),
            RuntimeValue::Str("hello world".into())
        );
    }

    #[test]
    fn eval_do_bind() {
        let src = "(do (bind x 10) (bind y 20) (int_add x y))";
        assert_eq!(run(src), RuntimeValue::Int(30));
    }

    #[test]
    fn eval_canonicalized_multi_bind() {
        let src = "(bind ((x 1) (y 2)) (int_add x y))";
        assert_eq!(run(src), RuntimeValue::Int(3));
    }

    #[test]
    fn eval_if_true() {
        assert_eq!(run("(if true 1 2)"), RuntimeValue::Int(1));
    }

    #[test]
    fn eval_if_false() {
        assert_eq!(run("(if false 1 2)"), RuntimeValue::Int(2));
    }

    #[test]
    fn eval_block_leave() {
        assert_eq!(run("(block b (leave b 42) 99)"), RuntimeValue::Int(42));
    }

    #[test]
    fn eval_canonical_block_null_label() {
        assert_eq!(run("(block null 7)"), RuntimeValue::Int(7));
    }

    #[test]
    fn eval_canonical_block_string_label_leave() {
        assert_eq!(
            run(r#"(block "exit" (leave "exit" 42) 99)"#),
            RuntimeValue::Int(42)
        );
    }

    #[test]
    fn eval_while() {
        let src = r#"
            (do
              (bind n 0)
              (while (lt n 5) (set! n (int_add n 1)))
              n)
        "#;
        assert_eq!(run(src), RuntimeValue::Int(5));
    }

    #[test]
    fn set_lowers_to_static_lexical_assignment_target() {
        let graph = parse("(do (bind n 0) (set! n 1))").unwrap();
        let assign_call = graph
            .node_ids()
            .into_iter()
            .find_map(|id| {
                let crate::ir::Node::Call(call) = graph.node(id)? else {
                    return None;
                };
                let crate::ir::Node::Name(callee) = graph.node(call.callee)? else {
                    return None;
                };
                (callee.identifier.as_ref() == "assign_lexical").then_some(call)
            })
            .expect("set! should lower to assign-lexical");

        assert!(graph.is_internal_node(assign_call.callee));
        let crate::ir::Node::Name(target) = graph.node(assign_call.args[0]).unwrap() else {
            panic!("assign-lexical target must be a name node");
        };
        assert_eq!(target.identifier.as_ref(), "n");
        assert!(graph.source_span(target.id).is_some());
    }

    #[test]
    fn set_is_collection_mutation_not_assignment() {
        assert_eq!(
            run(r#"(do (bind m (map_of)) (set m "answer" 42) (get m "answer" null))"#),
            RuntimeValue::Int(42)
        );

        let err = eval_source("(do (bind n 0) (set n 1))").expect_err("legacy set must fail");
        assert!(
            format!("{err}").contains("variable assignment uses set!, not set"),
            "unexpected error: {err}"
        );

        let err = eval_source(r#"(assign_lexical n 1)"#).expect_err("internal target must fail");
        assert!(
            format!("{err}").contains("internal builtin assign_lexical is not directly callable"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn eval_negative_int_arg() {
        assert_eq!(run("(int_add -3 4)"), RuntimeValue::Int(1));
    }

    #[test]
    fn eval_hyphen_in_builtin_name() {
        // '-' in a symbol is valid; 'string-concat-many' is a single symbol
        assert_eq!(
            run(r#"(string_concat_many "a" "b")"#),
            RuntimeValue::Str("ab".into())
        );
    }

    #[test]
    fn eval_sequence_filter() {
        let src = "(sequence_filter (list_of 1 2 3 4 5) (lambda (x) (gt x 2)))";
        let result = run(src);
        assert!(matches!(&result, RuntimeValue::List(_)), "expected list");
        let RuntimeValue::List(l) = result else {
            return;
        };
        let v = l.borrow().clone();
        assert_eq!(
            v,
            vec![
                RuntimeValue::Int(3),
                RuntimeValue::Int(4),
                RuntimeValue::Int(5)
            ]
        );
    }

    #[test]
    fn eval_map_of() {
        let src = r#"(map_of "a" 1 "b" 2)"#;
        let result = run(src);
        assert!(matches!(&result, RuntimeValue::Map(_)), "expected map");
        let RuntimeValue::Map(m) = result else {
            return;
        };
        use crate::values::MapKey;
        let map = m.borrow();
        assert_eq!(
            map.get(&MapKey::Str("a".into())),
            Some(&RuntimeValue::Int(1))
        );
        assert_eq!(
            map.get(&MapKey::Str("b".into())),
            Some(&RuntimeValue::Int(2))
        );
    }

    #[test]
    fn eval_gensym_is_unique() {
        let src = "(eq (gensym) (gensym))";
        assert_eq!(run(src), RuntimeValue::Bool(false));
    }

    #[test]
    fn parse_error_on_bad_input() {
        let error = parse("(add 1 @bad)").expect_err("expected error");
        assert_eq!(error.domain(), "parse");
        assert!(!error.message().is_empty());
    }
}
