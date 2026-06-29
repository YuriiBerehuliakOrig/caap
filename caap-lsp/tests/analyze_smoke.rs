//! End-to-end smoke tests for the LSP server's analysis layer.
//!
//! These exercise the same code path that runs on each `didOpen`/`didChange`
//! but without going through the LSP wire protocol. They guard against API
//! drift in `caap-core` and basic correctness of definition extraction and
//! semantic-token emission.

use std::path::PathBuf;
use std::str::FromStr;

use caap_core::lsp::BootstrapSession;
use caap_lsp::analyze::{Analysis, DefinitionKind};
use caap_lsp::{semantic_tokens, symbols};
use lsp_types::Position;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at the caap-lsp crate; the repo root is its parent.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("caap-lsp has a parent directory")
        .to_path_buf()
}

const SAMPLE: &str = r#"
; top_level function definition
(bind add (lambda (x y) (int_add x y)))

(bind ((counter 0)
       (label "hello"))
  (do
    (set! counter 1)
    counter))

(define_class registry "Dog" "Animal"
  (list_of "legs")
  (map_of "speak" (lambda (self) "woof")))
"#;

#[test]
fn analysis_extracts_top_level_bindings() {
    let analysis = Analysis::from_source("inmemory://test.caap", SAMPLE).expect("parse ok");

    let names: Vec<_> = analysis
        .definitions
        .iter()
        .map(|d| (d.name.as_str(), d.kind))
        .collect();

    assert!(
        names.contains(&("add", DefinitionKind::Function)),
        "{names:?}"
    );
    assert!(
        names.contains(&("counter", DefinitionKind::Variable)),
        "{names:?}"
    );
    assert!(
        names.contains(&("label", DefinitionKind::Variable)),
        "{names:?}"
    );
    assert!(names.contains(&("Dog", DefinitionKind::Class)), "{names:?}");
}

#[test]
fn semantic_tokens_emits_data_for_sample() {
    let analysis = Analysis::from_source("inmemory://test.caap", SAMPLE).expect("parse ok");
    let result = semantic_tokens::full_tokens(&analysis, SAMPLE);
    match result {
        lsp_types::SemanticTokensResult::Tokens(tokens) => {
            assert!(
                !tokens.data.is_empty(),
                "expected semantic tokens for sample"
            );
            let has_number = tokens.data.iter().any(|t| t.token_type == 4);
            let has_string = tokens.data.iter().any(|t| t.token_type == 5);
            assert!(has_number, "expected at least one NUMBER token");
            assert!(has_string, "expected at least one STRING token");
        }
        _ => panic!("expected SemanticTokensResult::Tokens"),
    }
}

/// The LSP renders the surface analyzer's token STREAM verbatim: each category
/// comes from `analysis.tokens` (parser/symbol-derived), never from a name. Here
/// three tokens — a `parameter`, a `property`, a `function` — round-trip to their
/// legend indices, proving the consumer is thin (no name logic in the LSP).
#[test]
fn semantic_tokens_render_the_surface_stream() {
    use caap_core::SourceSpan;
    use caap_lsp::analyze::SemToken;

    let mut analysis = Analysis::empty();
    // One line, 1-based columns: a param, a field, a call.
    let tok = |sc: usize, ec: usize, kind: &str, mods: Vec<String>| SemToken {
        span: SourceSpan::new(0, 0, 1, sc, 1, ec).unwrap(),
        kind: kind.to_string(),
        mods,
    };
    analysis.tokens = vec![
        tok(1, 2, "parameter", vec![]),
        tok(3, 6, "property", vec![]),
        tok(7, 14, "function", vec![]),
    ];

    let result = semantic_tokens::full_tokens(&analysis, "q cap ur_send");
    let lsp_types::SemanticTokensResult::Tokens(tokens) = result else {
        panic!("expected tokens");
    };
    // legend indices: function=1, parameter=8, property=10.
    let types: Vec<u32> = tokens.data.iter().map(|t| t.token_type).collect();
    assert_eq!(
        tokens.data.len(),
        3,
        "exactly the three stream tokens: {types:?}"
    );
    assert!(types.contains(&8), "parameter rendered: {types:?}");
    assert!(types.contains(&10), "property rendered: {types:?}");
    assert!(types.contains(&1), "function rendered: {types:?}");
}

/// Document symbols nest by span containment: definitions whose defining form
/// lies inside another's (e.g. functions declared inside a `module`) appear as
/// children, so the outline mirrors the file's structure.
#[test]
fn document_symbols_nest_by_span_containment() {
    use caap_core::SourceSpan;
    use caap_lsp::analyze::{Definition, DefinitionKind};

    // `app` module spans bytes 0..100 and encloses two functions; `helper`
    // (200..260) is a sibling outside the module.
    let module = SourceSpan::new(0, 100, 1, 1, 10, 1).unwrap();
    let inner_a = SourceSpan::new(10, 40, 2, 3, 4, 3).unwrap();
    let inner_b = SourceSpan::new(50, 90, 5, 3, 8, 3).unwrap();
    let sibling = SourceSpan::new(200, 260, 12, 1, 15, 1).unwrap();

    let def = |name: &str, kind, span: &SourceSpan| {
        Definition::leaf(
            name.to_string(),
            kind,
            span.clone(),
            span.clone(),
            String::new(),
        )
    };

    let mut analysis = Analysis::empty();
    analysis.definitions = vec![
        def("app", DefinitionKind::Module, &module),
        def("greet", DefinitionKind::Function, &inner_a),
        def("farewell", DefinitionKind::Function, &inner_b),
        def("helper", DefinitionKind::Function, &sibling),
    ];

    let symbols = symbols::document_symbols(&analysis);
    assert_eq!(
        symbols.len(),
        2,
        "module + sibling at the root: {symbols:?}"
    );

    let module_sym = symbols
        .iter()
        .find(|s| s.name == "app")
        .expect("app module");
    let children = module_sym.children.as_ref().expect("module has children");
    let child_names: Vec<_> = children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        child_names,
        vec!["greet", "farewell"],
        "nested in source order"
    );

    let helper = symbols.iter().find(|s| s.name == "helper").expect("helper");
    assert!(helper.children.is_none(), "sibling has no children");
}

#[test]
fn folding_ranges_cover_multiline_forms() {
    use caap_lsp::structure;
    let analysis = Analysis::from_source("inmemory://test.caap", SAMPLE).expect("parse ok");
    let folds = structure::folding_ranges(&analysis, SAMPLE);
    // The grouped `bind` and the `define-class` both span several lines.
    let regions = folds
        .iter()
        .filter(|f| f.kind == Some(lsp_types::FoldingRangeKind::Region))
        .count();
    assert!(regions >= 2, "expected multi-line forms to fold: {folds:?}");
    // Every fold spans more than one line.
    assert!(folds.iter().all(|f| f.end_line > f.start_line), "{folds:?}");
}

#[test]
fn function_infos_capture_body_calls() {
    use caap_lsp::structure;
    let analysis = Analysis::from_source("inmemory://test.caap", SAMPLE).expect("parse ok");
    let funcs = structure::function_infos(&analysis.parsed);
    let add = funcs
        .iter()
        .find(|f| f.name == "add")
        .expect("add function");
    let callees: Vec<_> = add.calls.iter().map(|c| c.callee.as_str()).collect();
    assert!(
        callees.contains(&"int_add"),
        "add calls int-add: {callees:?}"
    );
}

#[test]
fn definition_function_infos_from_bootstrap_calls() {
    use caap_core::SourceSpan;
    use caap_lsp::analyze::{CallRef, Definition, DefinitionKind};
    use caap_lsp::structure;

    // A grammar-extended analysis: empty AST, definitions carrying call data.
    let span = SourceSpan::new(0, 10, 1, 1, 1, 10).unwrap();
    let call_span = SourceSpan::new(20, 25, 2, 3, 2, 8).unwrap();
    let mut analysis = Analysis::empty();
    let mut greet = Definition::leaf(
        "greet".to_string(),
        DefinitionKind::Function,
        span.clone(),
        span.clone(),
        String::new(),
    );
    greet.params = vec!["who".to_string()];
    greet.calls = vec![CallRef {
        name: "shout".to_string(),
        span: call_span.clone(),
        arg_spans: vec![],
    }];
    analysis.definitions = vec![greet];

    assert!(analysis.has_surface_structure());
    let infos = structure::analysis_function_infos(&analysis);
    let g = infos
        .iter()
        .find(|f| f.name == "greet")
        .expect("greet info");
    assert_eq!(g.calls.len(), 1);
    assert_eq!(g.calls[0].callee, "shout");
}

#[test]
fn selection_ranges_nest_outward_from_cursor() {
    use caap_lsp::structure;
    let analysis = Analysis::from_source("inmemory://test.caap", SAMPLE).expect("parse ok");
    // Inside `int-add` on the `add` definition line (0-based line 2).
    let pos = Position::new(2, 27);
    let ranges = structure::selection_ranges(&analysis, SAMPLE, &[pos]);
    assert_eq!(ranges.len(), 1, "one selection range per position");
    let innermost = &ranges[0];
    assert!(
        innermost.parent.is_some(),
        "cursor inside nested forms should have an enclosing parent"
    );
}

#[test]
fn hover_lookup_finds_local_binding() {
    let analysis = Analysis::from_source("inmemory://test.caap", SAMPLE).expect("parse ok");
    let hover = symbols::hover_at(&analysis, SAMPLE, Position::new(2, 7));
    let hover = hover.unwrap_or_else(|| "<none>".to_string());
    assert!(hover.contains("add"), "hover should mention `add`: {hover}");
}

/// The s-expr header of a grammar-extended file (module / import forms) must
/// still be parsed even though the body cannot be, so hover and import
/// go-to-definition work on it.
#[test]
fn leading_forms_recover_header_of_grammar_extended_file() {
    let source = "(module \"demo\")\n\n(import_symbols \"sys.io\" \"println\")\n\napp module = {\n  x i32 = 1\n}\n";
    // Full parse fails on `app module = { ... }`.
    assert!(Analysis::from_source("inmemory://x.caap", source).is_err());

    let analysis = Analysis::from_leading_forms("inmemory://x.caap", source);
    // Cursor on the "sys.io" module string in the import form.
    let target = symbols::import_target_at(&analysis, Position::new(2, 18))
        .expect("cursor on import string resolves a target");
    assert_eq!(target.module, "sys.io");
    // Cursor on the "println" symbol string.
    let target = symbols::import_target_at(&analysis, Position::new(2, 27))
        .expect("cursor on imported symbol resolves a target");
    assert_eq!(target.module, "sys.io");
    assert_eq!(target.symbol.as_deref(), Some("println"));
}

#[test]
fn identifier_at_extracts_token_under_cursor() {
    let src = "  alice Person = make_person(\"Alice\")\n";
    // Cursor inside `make-person`.
    assert_eq!(
        symbols::identifier_at(src, Position::new(0, 20)).as_deref(),
        Some("make_person")
    );
    // Cursor inside `Person`.
    assert_eq!(
        symbols::identifier_at(src, Position::new(0, 10)).as_deref(),
        Some("Person")
    );
    // Cursor in leading whitespace (no adjacent identifier) yields nothing.
    assert_eq!(symbols::identifier_at(src, Position::new(0, 1)), None);
}

#[test]
fn document_highlights_and_references_find_all_occurrences() {
    // `counter` appears three times; `0`/`1` are not identifiers we match.
    let src = "(bind ((counter 0))\n  (do (set! counter 1) counter))";
    let analysis = Analysis::from_source("inmemory://r.caap", src).expect("parse ok");
    // Cursor inside the first `counter` (line 0, cols 8..15).
    let highlights = symbols::document_highlights(&analysis, src, Position::new(0, 10))
        .expect("highlights for `counter`");
    assert_eq!(
        highlights.len(),
        3,
        "three occurrences of counter: {highlights:?}"
    );

    let uri = lsp_types::Uri::from_str("file:///r.caap").unwrap();
    let refs = symbols::references(&analysis, src, &uri, Position::new(0, 10))
        .expect("references for `counter`");
    assert_eq!(refs.len(), 3);
    assert!(refs.iter().all(|loc| loc.uri == uri));
}

#[test]
fn formatting_canonicalizes_and_skips_when_unsafe() {
    use caap_lsp::format::format_document;
    // Messy spacing canonicalizes to a single edit.
    let edits = format_document("(int_add   1    2)").expect("should format");
    assert_eq!(edits.len(), 1);
    assert!(
        edits[0].new_text.contains("(int_add 1 2)"),
        "{:?}",
        edits[0].new_text
    );

    // Comments are not preserved by canonicalize → refuse to format.
    assert!(
        format_document("; a comment\n(int_add 1 2)").is_none(),
        "must not format sources with comments"
    );
    // Grammar-extended surface syntax does not parse as s-expr → no edit.
    assert!(format_document("app module = { x i32 = 1 }").is_none());
    // Canonical form is stable: formatting it again is a no-op.
    let once = edits[0].new_text.clone();
    assert!(
        format_document(&once).is_none(),
        "canonical output should be idempotent"
    );
}

#[test]
fn completion_offers_definitions_and_keywords() {
    let analysis = Analysis::from_source("inmemory://c.caap", SAMPLE).expect("parse ok");
    let items = symbols::completions(&analysis);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"add"),
        "definition `add` offered: {labels:?}"
    );
    assert!(labels.contains(&"Dog"), "class `Dog` offered");
    assert!(labels.contains(&"lambda"), "keyword `lambda` offered");
    assert!(labels.contains(&"if"), "keyword `if` offered");
}

#[test]
#[allow(clippy::mutable_key_type)] // lsp_types::Uri map key is immutable in practice
fn rename_edits_all_occurrences_in_document() {
    let src = "(bind ((counter 0))\n  (do (set! counter 1) counter))";
    let analysis = Analysis::from_source("inmemory://rn.caap", src).expect("parse ok");
    let uri = lsp_types::Uri::from_str("file:///rn.caap").unwrap();

    let range = symbols::prepare_rename(&analysis, src, Position::new(0, 10))
        .expect("renameable identifier under cursor");
    assert_eq!(range.start.line, 0);

    let edit = symbols::rename(&analysis, src, &uri, Position::new(0, 10), "total")
        .expect("rename produces an edit");
    let edits = edit.changes.unwrap();
    let doc_edits = &edits[&uri];
    assert_eq!(
        doc_edits.len(),
        3,
        "all three `counter` occurrences renamed"
    );
    assert!(doc_edits.iter().all(|e| e.new_text == "total"));
}

#[test]
fn signature_help_shows_params_and_active_arg() {
    let src = "(bind ((f (lambda (alpha beta) (int_add alpha beta))))\n  (f 1 2))";
    let analysis = Analysis::from_source("inmemory://sig.caap", src).expect("parse ok");
    // Cursor on the second argument of `(f 1 2)` (line 1).
    let help = symbols::signature_help(&analysis, Position::new(1, 7))
        .expect("signature help inside the call");
    assert_eq!(help.signatures.len(), 1);
    let sig = &help.signatures[0];
    assert!(sig.label.contains("f alpha beta"), "label: {}", sig.label);
    assert_eq!(sig.parameters.as_ref().unwrap().len(), 2);
}

#[test]
fn inlay_hints_label_call_arguments() {
    let src = "(bind ((f (lambda (alpha beta) (int_add alpha beta))))\n  (f 1 2))";
    let analysis = Analysis::from_source("inmemory://ih.caap", src).expect("parse ok");
    let range = lsp_types::Range {
        start: Position::new(0, 0),
        end: Position::new(100, 0),
    };
    let hints = symbols::inlay_hints(&analysis, range);
    let labels: Vec<String> = hints
        .iter()
        .filter_map(|h| match &h.label {
            lsp_types::InlayHintLabel::String(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(labels.contains(&"alpha:".to_string()), "hints: {labels:?}");
    assert!(labels.contains(&"beta:".to_string()), "hints: {labels:?}");
}

#[test]
fn analysis_reports_parse_error_with_diagnostic() {
    let result = Analysis::from_source("inmemory://broken.caap", "(int_add 1 @bad)");
    let Err(err) = result else {
        panic!("expected parse failure on `@bad` input");
    };
    assert!(!err.message.is_empty());
    assert_eq!(err.source.as_deref(), Some("caap"));
}

/// Go-to-definition resolves function parameters and local `bind`s to their
/// declaration within the enclosing definition (grammar-extended file: no base
/// AST, definitions from bootstrap). Callees/builtins are not mis-resolved.
#[test]
fn definition_resolves_parameters_and_locals_in_grammar_extended_body() {
    use caap_core::SourceSpan;
    use caap_lsp::analyze::{CallRef, Definition, DefinitionKind};
    use caap_lsp::symbols::definition_at;
    use lsp_types::{Position, Uri};
    use std::str::FromStr;

    // line 0: hint (secret i32, guess i32) i32 = {
    // line 1:   state i32 = secret
    // line 2:   if secret > guess { state } else { hint(secret, guess) }
    // line 3: }
    let source = "hint (secret i32, guess i32) i32 = {\n  state i32 = secret\n  if secret > guess { state } else { hint(secret, guess) }\n}\n";
    let dummy = SourceSpan::new(0, 0, 1, 1, 1, 1).unwrap();
    let mut analysis = Analysis::empty();
    analysis.definitions = vec![Definition {
        name: "hint".to_string(),
        kind: DefinitionKind::Function,
        name_span: SourceSpan::new(0, 4, 1, 1, 1, 5).unwrap(),
        form_span: SourceSpan::new(0, source.len(), 1, 1, 4, 2).unwrap(),
        detail: String::new(),
        params: vec!["secret".to_string(), "guess".to_string()],
        calls: vec![
            CallRef {
                name: "if".to_string(),
                span: dummy.clone(),
                arg_spans: vec![],
            },
            CallRef {
                name: "hint".to_string(),
                span: dummy.clone(),
                arg_spans: vec![],
            },
        ],
    }];
    let uri = Uri::from_str("inmemory://t.caap").unwrap();

    // `secret` used in the body (line 2) -> its parameter declaration (line 0, col 6).
    let loc = definition_at(&analysis, source, &uri, Position::new(2, 6))
        .expect("parameter `secret` resolves");
    assert_eq!((loc.range.start.line, loc.range.start.character), (0, 6));

    // local `state` used in the body (line 2) -> its `bind` declaration (line 1, col 2).
    let loc = definition_at(&analysis, source, &uri, Position::new(2, 24))
        .expect("local `state` resolves");
    assert_eq!((loc.range.start.line, loc.range.start.character), (1, 2));

    // `if` is a callee/builtin -> must not jump to a stray occurrence.
    assert!(definition_at(&analysis, source, &uri, Position::new(2, 2)).is_none());
}

#[test]
fn occurrence_scope_confines_locals_but_not_globals() {
    // `x` is a parameter local to each function; `add`/`total` are top-level.
    let src = "(bind add (lambda (x y) (int_add x y)))\n(bind total (lambda (x) x))";
    let analysis = Analysis::from_source("inmemory://s.caap", src).expect("parse ok");

    // `x` inside `add` (line 0, col 19) is local: confined to `add`'s form.
    let (word, scope) =
        symbols::occurrence_scope(&analysis, src, Position::new(0, 19)).expect("scope for local x");
    assert_eq!(word, "x");
    match scope {
        symbols::OccurrenceScope::Local { form_span } => {
            let confined = symbols::occurrences_in_span(src, &word, &form_span);
            assert_eq!(
                confined.len(),
                2,
                "local x confined to its own function, not `total`: {confined:?}"
            );
            assert!(confined.iter().all(|r| r.start.line == 0));
        }
        symbols::OccurrenceScope::Global => panic!("local x must not be classified global"),
    }

    // A top-level definition name is global (cursor on `add`, col 7).
    let (word, scope) =
        symbols::occurrence_scope(&analysis, src, Position::new(0, 7)).expect("scope for add");
    assert_eq!(word, "add");
    assert!(matches!(scope, symbols::OccurrenceScope::Global));
}

/// A genuine s-expr syntax error must be reported at the offending location,
/// not at (0, 0): the tolerant parser locates the unmatched region.
#[test]
fn syntax_error_diagnostic_points_at_offending_form() {
    // Two complete forms on lines 0-1, then an unterminated list on line 2.
    let source = "(ok 1)\n(ok 2)\n(broken (\n";
    let err = match Analysis::from_source("inmemory://e.caap", source) {
        Ok(_) => panic!("unterminated list must fail to parse"),
        Err(diag) => diag,
    };
    // The error must land after the valid forms, not at the file head.
    assert!(
        err.range.start.line >= 1,
        "diagnostic should point past the valid forms (line {}), not the file head",
        err.range.start.line
    );
    assert!(
        !(err.range.start.line == 0 && err.range.start.character == 0),
        "diagnostic must not collapse to (0, 0)"
    );
}

#[test]
fn completion_offers_special_form_snippets() {
    let analysis = Analysis::from_source("inmemory://c.caap", SAMPLE).expect("parse ok");
    let items = symbols::completions(&analysis);
    let lambda = items
        .iter()
        .find(|i| i.label == "lambda")
        .expect("lambda offered");
    assert_eq!(lambda.kind, Some(lsp_types::CompletionItemKind::SNIPPET));
    assert_eq!(
        lambda.insert_text_format,
        Some(lsp_types::InsertTextFormat::SNIPPET)
    );
    assert!(
        lambda
            .insert_text
            .as_deref()
            .unwrap_or_default()
            .contains("${1:"),
        "snippet carries tabstops"
    );
}

/// stdlib returns its checker findings as DATA in the analyze response; the LSP
/// converts the located strings into precise diagnostics.
#[test]
fn stdlib_analyze_surfaces_semantic_diagnostics() {
    let root = workspace_root();
    let bootstrap = root.join("stdlib").join("bootstrap.caap");
    if !bootstrap.exists() {
        return;
    }
    let file = root.join("tests").join("typo.caap");
    let session = BootstrapSession::new(vec![bootstrap], vec![root.clone()])
        .expect("stdlib session constructs");

    let mut analysis = Analysis::empty();
    analysis.augment_from_bootstrap(&session, &file);

    let diag = analysis
        .diagnostics
        .iter()
        .find(|d| d.message.contains("sequence_fold_lett"))
        .expect("misspelled name is reported as a semantic diagnostic");
    // The finding is located on the offending line (typo.caap line 4, 0-based 3).
    assert_eq!(
        diag.range.start.line, 3,
        "diagnostic lands on the bind line"
    );
}

/// End-to-end PROOF that surface highlighting is parser/symbol-derived, not
/// spelling-derived: the booted clike analyzer classifies `tests/clike_highlight.caap`
/// and the SAME spelling `px` comes back as a `property` (the `p->px` field access)
/// AND a `variable` (the struct-field declaration) — impossible under any
/// name/regex heuristic. Functions, params, user types and enum members are also
/// classified from the parse + symbol table, never from case or an `ur_`/`UR_` list.
#[test]
fn surface_semantic_tokens_are_parser_derived() {
    let root = workspace_root();
    let bootstrap = root.join("stdlib").join("bootstrap.caap");
    if !bootstrap.exists() {
        return;
    }
    let file = root.join("tests").join("clike_highlight.caap");
    let session = BootstrapSession::new(vec![bootstrap], vec![root.clone()])
        .expect("stdlib session constructs");

    let mut analysis = Analysis::empty();
    analysis.augment_from_bootstrap(&session, &file);
    assert!(
        !analysis.tokens.is_empty(),
        "clike analyze_program emits parser-derived semantic tokens"
    );

    // Reconstruct each token's source text from its (1-based) span so the
    // assertions read in terms of the program, not opaque offsets.
    let text = std::fs::read_to_string(&file).expect("read fixture");
    let lines: Vec<&str> = text.lines().collect();
    let cats: Vec<(String, String)> = analysis
        .tokens
        .iter()
        .map(|t| {
            let s = &t.span;
            let txt: String = lines
                .get(s.start_line.saturating_sub(1))
                .map(|ln| {
                    ln.chars()
                        .skip(s.start_col.saturating_sub(1))
                        .take(s.end_col.saturating_sub(s.start_col))
                        .collect()
                })
                .unwrap_or_default();
            (txt, t.kind.clone())
        })
        .collect();
    let has = |text: &str, kind: &str| cats.iter().any(|(s, k)| s == text && k == kind);

    // Classified from the parse + symbol table:
    assert!(
        has("shade", "function"),
        "function from a decl/call: {cats:?}"
    );
    assert!(
        has("p", "parameter"),
        "parameter from the signature: {cats:?}"
    );
    assert!(
        has("Point", "type"),
        "user struct type (not uppercase regex): {cats:?}"
    );
    assert!(
        has("GREEN", "enumMember"),
        "enum member from the enum body: {cats:?}"
    );
    // THE PROOF — same spelling, two categories, decided by POSITION:
    assert!(
        has("px", "property"),
        "`px` after `->` is a field/property: {cats:?}"
    );
    assert!(
        has("px", "variable"),
        "`px` in the struct decl is not a property: {cats:?}"
    );
}

/// Per-file symbol state must not leak across files (clike's reset_file_state!).
/// Analyze A — which declares a struct type `Leaky` — then analyze B in the SAME
/// session. B never declares `Leaky`, so a bare use of it must classify as a
/// plain `variable`; if the reset failed, A's struct would leak and it would be
/// coloured `type`. This pins the reset invariant so a future per-file registry
/// added without being cleared fails here.
#[test]
fn per_file_state_does_not_leak_across_files() {
    let root = workspace_root();
    let bootstrap = root.join("stdlib").join("bootstrap.caap");
    if !bootstrap.exists() {
        return;
    }
    let session = BootstrapSession::new(vec![bootstrap], vec![root.clone()])
        .expect("stdlib session constructs");

    // A registers the struct type `Leaky` in the per-file table.
    let file_a = root.join("tests").join("clike_leak_a.caap");
    let mut a = Analysis::empty();
    a.augment_from_bootstrap(&session, &file_a);

    // B, analyzed next in the same session, must start from a clean slate.
    let file_b = root.join("tests").join("clike_leak_b.caap");
    let mut b = Analysis::empty();
    b.augment_from_bootstrap(&session, &file_b);

    let text = std::fs::read_to_string(&file_b).expect("read fixture B");
    let lines: Vec<&str> = text.lines().collect();
    let kind_of = |name: &str| -> Option<String> {
        b.tokens.iter().find_map(|t| {
            let s = &t.span;
            let txt: String = lines
                .get(s.start_line.saturating_sub(1))
                .map(|ln| {
                    ln.chars()
                        .skip(s.start_col.saturating_sub(1))
                        .take(s.end_col.saturating_sub(s.start_col))
                        .collect()
                })
                .unwrap_or_default();
            if txt == name {
                Some(t.kind.clone())
            } else {
                None
            }
        })
    };

    assert_eq!(
        kind_of("Leaky").as_deref(),
        Some("variable"),
        "file A's struct type `Leaky` leaked into file B (reset_file_state! \
         did not clear the per-file registry); B tokens: {:?}",
        b.tokens
    );
}
