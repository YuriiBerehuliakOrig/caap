//! Parse + index a CAAP document once per change. The result is consumed by
//! semantic-token, hover, definition, and outline providers.

use std::path::Path;

use caap_core::lsp::{runtime_value_to_json, BootstrapSession};
use caap_core::values::RuntimeValue;
use caap_core::{parse_forms_with_source_path, ParsedForm, ParsedSource, SourceSpan};
use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};
use serde_json::Value;

pub struct Analysis {
    pub parsed: ParsedSource,
    pub diagnostics: Vec<Diagnostic>,
    pub definitions: Vec<Definition>,
    /// Parser/symbol-derived semantic tokens for a grammar-extended (surface)
    /// file, produced by the surface analyzer's `analyze_program` (e.g. clike). When
    /// present, these ARE the highlighting — the name-based fallback is not used.
    /// Empty for plain s-expr files (their AST drives semantic tokens instead).
    pub tokens: Vec<SemToken>,
}

/// One parser-classified semantic token from the surface analyzer: a source
/// span, a category (`function`/`type`/`parameter`/`variable`/`property`/
/// `enumMember`/`keyword`/`operator`/`number`/`string`), and modifier names
/// (`readonly`/`declaration`). The category comes from the parse + symbol table,
/// never from the identifier's spelling.
#[derive(Clone, Debug)]
pub struct SemToken {
    pub span: SourceSpan,
    pub kind: String,
    pub mods: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Definition {
    pub name: String,
    pub kind: DefinitionKind,
    /// Span of the binding name itself.
    pub name_span: SourceSpan,
    /// Span of the entire defining form, used as the symbol selection range.
    pub form_span: SourceSpan,
    /// Free-form one-line description shown on hover.
    pub detail: String,
    /// Parameter names, when this definition is a function/lambda. Populated for
    /// grammar-extended files from the grammar-aware bootstrap analysis (the
    /// base-AST path resolves params separately via `symbols`).
    pub params: Vec<String>,
    /// Call sites inside this definition's body (grammar-extended files only),
    /// backing call hierarchy / inlay hints / signature help on surface DSLs.
    pub calls: Vec<CallRef>,
}

impl Definition {
    /// A definition with no params/calls (the base-AST path; those are derived
    /// elsewhere or only meaningful for grammar-extended bootstrap entries).
    pub fn leaf(
        name: String,
        kind: DefinitionKind,
        name_span: SourceSpan,
        form_span: SourceSpan,
        detail: String,
    ) -> Self {
        Self {
            name,
            kind,
            name_span,
            form_span,
            detail,
            params: Vec::new(),
            calls: Vec::new(),
        }
    }
}

/// A call site recorded inside a function body: the callee name, the span of the
/// callee token, and the spans of each argument (for inlay/signature help).
#[derive(Clone, Debug)]
pub struct CallRef {
    pub name: String,
    pub span: SourceSpan,
    pub arg_spans: Vec<SourceSpan>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefinitionKind {
    Function,
    Variable,
    Macro,
    Class,
    Interface,
    Module,
}

impl Analysis {
    /// Whether the grammar-aware bootstrap supplied surface structure (params or
    /// call sites) that the base s-expr AST cannot represent — i.e. this is a
    /// grammar-extended file, so structural features should read the definitions
    /// rather than the (header-only) AST.
    pub fn has_surface_structure(&self) -> bool {
        self.definitions
            .iter()
            .any(|d| !d.params.is_empty() || !d.calls.is_empty())
    }

    /// An empty analysis used when the base parser fails but bootstrap
    /// augmentation may still succeed (e.g., grammar-extended files).
    pub fn empty() -> Self {
        Self {
            parsed: ParsedSource { forms: Vec::new() },
            diagnostics: Vec::new(),
            definitions: Vec::new(),
            tokens: Vec::new(),
        }
    }

    /// Best-effort parse of just the leading top-level parenthesized forms.
    /// Grammar-extended files (e.g. `app module = { ... }`) cannot be read by
    /// the s-expr parser as a whole, but their header — `(module ...)`,
    /// `(syntax-import ...)`, `(import-symbols ...)` — is ordinary s-expr.
    /// Parsing that prefix lets hover, semantic tokens, and import
    /// go-to-definition work on the header even when the body cannot be
    /// parsed by the base parser. Never errors; yields an empty analysis when
    /// there are no leading forms.
    pub fn from_leading_forms(uri: &str, text: &str) -> Self {
        let prefix_len = leading_forms_prefix_len(text);
        if prefix_len == 0 {
            return Self::empty();
        }
        match parse_forms_with_source_path(&text[..prefix_len], uri) {
            Ok(parsed) => {
                let mut definitions = Vec::new();
                for form in &parsed.forms {
                    collect_definitions(form, &mut definitions);
                }
                Analysis {
                    parsed,
                    diagnostics: Vec::new(),
                    definitions,
                    tokens: Vec::new(),
                }
            }
            Err(_) => Self::empty(),
        }
    }

    pub fn from_source(uri: &str, text: &str) -> Result<Self, Box<Diagnostic>> {
        let parsed = parse_forms_with_source_path(text, uri).map_err(|err| {
            // The strict parser fails fast at the first problem with only a
            // message; the error-tolerant parser locates the offending region,
            // so point the diagnostic there instead of at (0, 0).
            let (range, message) = caap_core::frontend::surface_syntax_errors(text, Some(uri))
                .into_iter()
                .next()
                .map(|e| (span_to_range(&e.span), e.message))
                .unwrap_or_else(|| {
                    (
                        Range {
                            start: Position::new(0, 0),
                            end: Position::new(0, 0),
                        },
                        err.message().to_string(),
                    )
                });
            Box::new(Diagnostic {
                range,
                severity: Some(DiagnosticSeverity::ERROR),
                source: Some("caap".to_string()),
                message,
                ..Default::default()
            })
        })?;

        let mut definitions = Vec::new();
        for form in &parsed.forms {
            collect_definitions(form, &mut definitions);
        }

        Ok(Analysis {
            parsed,
            diagnostics: Vec::new(),
            definitions,
            tokens: Vec::new(),
        })
    }

    /// When a workspace-level `BootstrapSession` is configured, ask the stdlib
    /// `analyze_source` session command to re-walk the file through the actual
    /// CAAP frontend (including grammar extensions). On
    /// success, replace the locally-extracted definitions with the richer data
    /// set returned by the bootstrap. Any diagnostics the compiler emits while
    /// analyzing the file (real semantic errors/warnings, with source spans)
    /// are converted and appended to `self.diagnostics`.
    ///
    /// Returns `true` when the file was analyzed (so the caller can suppress
    /// the raw base-parse error for grammar-extended files), `false` only when
    /// analysis hard-failed without producing any precise diagnostic.
    pub fn augment_from_bootstrap(
        &mut self,
        session: &BootstrapSession,
        absolute_path: &Path,
    ) -> bool {
        let Some(path_str) = absolute_path.to_str() else {
            return false;
        };
        // Mirror `caap run --root <parent>` semantics so module-root discovery
        // finds sibling `(module ...)` files (e.g., a syntax-import target
        // next to the demo file under analysis).
        let module_root = absolute_path.parent().and_then(Path::to_str);
        let extra_roots = absolute_path
            .parent()
            .map(|p| vec![p.to_path_buf()])
            .unwrap_or_default();
        // Capability keys into `caap.session.commands`. With a known module root
        // use the 2-ary `analyze_source_with_root` (reserves cross-module
        // resolution context); otherwise the 1-ary `analyze_source`. Arity must
        // match the chosen key.
        let command_name = if module_root.is_some() {
            "analyze_source_with_root"
        } else {
            "analyze_source"
        };
        // Degrade quietly when the booted stdlib ships no command map: the base
        // parse / leading-header analysis still stands, and no per-file
        // undefined-call error is surfaced.
        if !session.supports_command(command_name) {
            return false;
        }
        let result = session.invoke_named_command(command_name, path_str, module_root, extra_roots);

        // Convert the compiler's emitted diagnostics (semantic errors/warnings)
        // that pertain to this file, keeping their precise spans.
        let semantic = session.drain_diagnostics();
        let before = self.diagnostics.len();
        for diag in &semantic {
            if let Some(lsp) = bootstrap_diagnostic_to_lsp(diag, absolute_path) {
                self.diagnostics.push(lsp);
            }
        }
        let added_precise = self.diagnostics.len() > before;

        match result {
            Ok(value) => {
                let json = runtime_value_to_json(&value);
                let parsed = parse_bootstrap_definitions(&json);
                if !parsed.is_empty() {
                    self.definitions = parsed;
                }
                // The surface analyzer's parser/symbol-derived semantic tokens
                // (present only for grammar-extended files); these drive
                // highlighting directly in `semantic_tokens::full_tokens`.
                self.tokens = parse_bootstrap_tokens(&json);
                // stdlib returns its semantic findings as DATA under
                // `diagnostics`: located strings `"path:line:col: message"`
                // (the checker/type pass write to their own sink, not the
                // kernel DiagnosticSink). Convert and surface them.
                if let Some(items) = json.get("diagnostics").and_then(Value::as_array) {
                    for item in items {
                        if let Some(text) = item.as_str() {
                            self.diagnostics
                                .push(located_string_to_lsp(text, absolute_path));
                        }
                    }
                }
                true
            }
            Err(error) => {
                // A hard failure: surface it at the most precise location we can
                // recover from the error's call frames (falling back to 0,0).
                if !added_precise {
                    self.diagnostics
                        .push(error_to_lsp_diagnostic(&error, absolute_path));
                }
                false
            }
        }
    }

    pub fn definition_for(&self, name: &str) -> Option<&Definition> {
        self.definitions.iter().find(|def| def.name == name)
    }

    /// Tighten each definition's `name_span` to the exact name token in
    /// `source`. Grammar-extended lowerings hand back the whole defining-rule
    /// span (often starting in the leading whitespace of the previous form),
    /// which makes go-to-definition land a line or two early. Searching the
    /// span's own line range for the name token recovers a precise location.
    pub fn refine_definition_spans(&mut self, source: &str) {
        for def in &mut self.definitions {
            if let Some(refined) = refine_name_span(source, &def.name, &def.name_span) {
                def.name_span = refined;
            }
        }
    }
}

fn refine_name_span(source: &str, name: &str, span: &SourceSpan) -> Option<SourceSpan> {
    if name.is_empty() {
        return None;
    }
    let from = span.start_line.saturating_sub(1);
    let to = span.end_line.saturating_sub(1);
    let target: Vec<char> = name.chars().collect();
    // What chars belong to a symbol comes from the kernel grammar, never a list
    // maintained here (see `caap_core::language::is_symbol_char`).
    let is_ident = caap_core::language::is_symbol_char;

    for (line_idx, line) in source.lines().enumerate() {
        if line_idx < from {
            continue;
        }
        if line_idx > to {
            break;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        let mut in_string = false;
        while i + target.len() <= chars.len() {
            // Never match the name inside a string literal or a `;` line comment
            // — a name echoed in a doc comment (e.g. `; Round — …`) must not win
            // over the real declaration.
            let c = chars[i];
            if in_string {
                if c == '\\' {
                    i += 2;
                    continue;
                }
                if c == '"' {
                    in_string = false;
                }
                i += 1;
                continue;
            }
            if c == '"' {
                in_string = true;
                i += 1;
                continue;
            }
            if c == ';' {
                break;
            }
            if chars[i..i + target.len()] == target[..] {
                let before_ok = i == 0 || !is_ident(chars[i - 1]);
                let after = i + target.len();
                let after_ok = after >= chars.len() || !is_ident(chars[after]);
                if before_ok && after_ok {
                    let line1 = line_idx + 1;
                    let col1 = i + 1;
                    let end_col1 = after + 1;
                    return SourceSpan::new(0, name.len(), line1, col1, line1, end_col1).ok();
                }
            }
            i += 1;
        }
    }
    None
}

/// Turn a failed analysis error into an LSP diagnostic, placing it at the
/// innermost call frame that points into the file under analysis (so the
/// squiggle lands on the offending form rather than at line 0).
fn error_to_lsp_diagnostic(error: &caap_core::error::CaapError, file: &Path) -> Diagnostic {
    use caap_core::error::CaapError;
    let span = match error {
        CaapError::Eval(eval) => eval
            .frames()
            .iter()
            .find(|frame| {
                frame
                    .span
                    .as_ref()
                    .and_then(|s| s.path.as_ref())
                    .is_some_and(|p| same_file(p, file))
            })
            .and_then(|frame| frame.span.clone()),
        CaapError::Diagnostic(diag) => diag.span.clone(),
        _ => None,
    };
    let (range, severity) = match span {
        Some(span) => (span_to_range(&span), DiagnosticSeverity::ERROR),
        None => (
            Range {
                start: Position::new(0, 0),
                end: Position::new(0, 0),
            },
            DiagnosticSeverity::WARNING,
        ),
    };
    Diagnostic {
        range,
        severity: Some(severity),
        source: Some("caap".to_string()),
        message: error.message().to_string(),
        ..Default::default()
    }
}

/// Parse one of stdlib's located diagnostic strings into an LSP diagnostic.
/// The shapes are `"path:line:col: message"` (a precise finding), `"path: …"`
/// (span-less), and `"stdlib.analyze: …"` (an unlocated internal error). The
/// leading file path is stripped by exact match so a message containing colons
/// is not mis-split; an unrecognized prefix yields a file-level diagnostic.
fn located_string_to_lsp(text: &str, file: &Path) -> Diagnostic {
    let zero = Range {
        start: Position::new(0, 0),
        end: Position::new(0, 0),
    };
    let make = |range: Range, message: &str| Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("caap".to_string()),
        message: message.to_string(),
        ..Default::default()
    };
    let file_str = file.to_string_lossy();
    if let Some(rest) = text
        .strip_prefix(file_str.as_ref())
        .and_then(|r| r.strip_prefix(':'))
    {
        // `rest` is `"line:col: message"` or `" message"` (span-less).
        let mut parts = rest.splitn(3, ':');
        if let (Some(l), Some(c), Some(msg)) = (parts.next(), parts.next(), parts.next()) {
            if let (Ok(line), Ok(col)) = (l.trim().parse::<u32>(), c.trim().parse::<u32>()) {
                // stdlib lines/cols are 1-based; LSP is 0-based.
                let line0 = line.saturating_sub(1);
                let range = Range {
                    start: Position::new(line0, col.saturating_sub(1)),
                    end: Position::new(line0, col),
                };
                return make(range, msg.trim_start());
            }
        }
        return make(zero, rest.trim_start());
    }
    make(zero, text)
}

/// Convert a `caap-core` compiler diagnostic into an LSP diagnostic, keeping
/// only those that pertain to the file under analysis (or carry no path).
fn bootstrap_diagnostic_to_lsp(
    diag: &caap_core::diagnostics::Diagnostic,
    file: &Path,
) -> Option<Diagnostic> {
    use caap_core::diagnostics::DiagnosticSeverity as CoreSeverity;
    // Drop diagnostics that belong to other files (e.g. stdlib internals).
    if let Some(span) = &diag.span {
        if let Some(path) = &span.path {
            if !same_file(path, file) {
                return None;
            }
        }
    }
    let range = diag
        .span
        .as_ref()
        .map(span_to_range)
        .unwrap_or_else(|| Range {
            start: Position::new(0, 0),
            end: Position::new(0, 0),
        });
    let severity = match diag.severity {
        // Real problems underline the code.
        CoreSeverity::Error => DiagnosticSeverity::ERROR,
        CoreSeverity::Warning => DiagnosticSeverity::WARNING,
        // Notes and hints are informational, not problems: map both to the LSP
        // HINT level, the only severity editors do not render as a wavy
        // underline (and which stays out of the Problems panel). An advisory
        // note (e.g. a `mutable_vars` lint hint) must not mark the code as if
        // something is wrong.
        CoreSeverity::Note | CoreSeverity::Hint => DiagnosticSeverity::HINT,
    };
    Some(Diagnostic {
        range,
        severity: Some(severity),
        code: diag.code.clone().map(lsp_types::NumberOrString::String),
        source: Some("caap".to_string()),
        message: diag.message.clone(),
        ..Default::default()
    })
}

fn same_file(path: &str, file: &Path) -> bool {
    let a = std::fs::canonicalize(path).ok();
    let b = std::fs::canonicalize(file).ok();
    match (a, b) {
        (Some(a), Some(b)) => a == b,
        _ => Path::new(path) == file,
    }
}

fn parse_bootstrap_definitions(value: &Value) -> Vec<Definition> {
    let Some(defs) = value.get("definitions").and_then(Value::as_array) else {
        return Vec::new();
    };
    // Grammar-extended files surface each function twice: the `caap.codegen.*`
    // marker (precise name span, kind=function, emitted first) and the canonical
    // `bind` node (kind=bind, now carrying the lambda's form_span/params/calls).
    // Merge by name so one fully-populated definition results: the marker keeps
    // the precise name span + kind, the bind contributes the structural fields.
    let mut order: Vec<String> = Vec::new();
    let mut by_name: std::collections::HashMap<String, Definition> =
        std::collections::HashMap::new();
    for def in defs.iter().filter_map(parse_one_definition) {
        match by_name.get_mut(&def.name) {
            Some(existing) => merge_definition(existing, def),
            None => {
                order.push(def.name.clone());
                by_name.insert(def.name.clone(), def);
            }
        }
    }
    order
        .into_iter()
        .filter_map(|n| by_name.remove(&n))
        .collect()
}

/// Fold a duplicate same-name entry into the kept one: adopt a more specific
/// kind (markers classify as Function/Class/…; the bind entry is the generic
/// Variable) with its precise name span, and pull in the structural fields
/// (real body `form_span`, `params`, `calls`) from whichever entry has them.
fn merge_definition(existing: &mut Definition, other: Definition) {
    if existing.kind == DefinitionKind::Variable && other.kind != DefinitionKind::Variable {
        existing.kind = other.kind;
        existing.name_span = other.name_span.clone();
        existing.detail = other.detail.clone();
    }
    if existing.params.is_empty() && !other.params.is_empty() {
        existing.params = other.params;
    }
    if existing.calls.is_empty() && !other.calls.is_empty() {
        existing.calls = other.calls;
    }
    // Adopt a real (wider-than-name) body span when we still only have the name.
    if existing.form_span == existing.name_span && other.form_span != other.name_span {
        existing.form_span = other.form_span;
    }
}

/// Parse the surface analyzer's `tokens` array into semantic tokens. Each entry
/// is `{span, type, mods}` (the categories are parser/symbol-derived — see clike
/// `analyze_program`); a malformed entry is skipped, never fatal.
fn parse_bootstrap_tokens(value: &Value) -> Vec<SemToken> {
    value
        .get("tokens")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(parse_one_token).collect())
        .unwrap_or_default()
}

fn parse_one_token(entry: &Value) -> Option<SemToken> {
    let span = parse_bootstrap_span(entry.get("span")?)?;
    let kind = entry.get("type").and_then(Value::as_str)?.to_string();
    let mods = entry
        .get("mods")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(SemToken { span, kind, mods })
}

fn parse_one_definition(entry: &Value) -> Option<Definition> {
    let name = entry.get("name").and_then(Value::as_str)?.to_string();
    let span = parse_bootstrap_span(entry.get("span")?)?;
    let kind = entry
        .get("kind")
        .and_then(Value::as_str)
        .map(classify_bootstrap_kind)
        .unwrap_or(DefinitionKind::Variable);
    // Grammar-aware extras (present only for lambda-valued functions): the body
    // span, parameter names, and call sites.
    let form_span = entry
        .get("form_span")
        .and_then(parse_bootstrap_span)
        .unwrap_or_else(|| span.clone());
    let params = entry
        .get("params")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let calls = entry
        .get("calls")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(parse_call_ref).collect())
        .unwrap_or_default();
    Some(Definition {
        name: name.clone(),
        kind,
        name_span: span,
        form_span,
        detail: format!("**{name}**"),
        params,
        calls,
    })
}

fn parse_call_ref(entry: &Value) -> Option<CallRef> {
    let name = entry.get("name").and_then(Value::as_str)?.to_string();
    let span = parse_bootstrap_span(entry.get("span")?)?;
    // Keep argument spans index-aligned with params; placeholder spans (a coarse
    // grammar sometimes attaches the module's `1:1` to a synthesized argument)
    // are skipped at use sites, not dropped here, to preserve the index→param map.
    let arg_spans = entry
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(parse_bootstrap_span_or_empty).collect())
        .unwrap_or_default();
    Some(CallRef {
        name,
        span,
        arg_spans,
    })
}

/// Parse a span, or a sentinel `(0,0)` (offset 0) span when absent/degenerate so
/// the argument keeps its positional slot; use sites treat offset-0 as "skip".
fn parse_bootstrap_span_or_empty(value: &Value) -> SourceSpan {
    parse_bootstrap_span(value).unwrap_or_else(empty_bootstrap_span)
}

fn empty_bootstrap_span() -> SourceSpan {
    SourceSpan {
        file_id: None,
        start: 0,
        end: 0,
        path: None,
        start_line: 1,
        start_col: 1,
        end_line: 1,
        end_col: 1,
    }
}

fn parse_bootstrap_span(value: &Value) -> Option<SourceSpan> {
    let object = value.as_object()?;
    let start = object.get("start")?.as_u64()? as usize;
    let end = object.get("end")?.as_u64()? as usize;
    let start_line = object.get("start_line")?.as_u64()? as usize;
    let start_col = object.get("start_col")?.as_u64()? as usize;
    let end_line = object.get("end_line")?.as_u64()? as usize;
    let end_col = object.get("end_col")?.as_u64()? as usize;
    SourceSpan::new(start, end, start_line, start_col, end_line, end_col).ok()
}

fn classify_bootstrap_kind(kind: &str) -> DefinitionKind {
    match kind {
        "bind" => DefinitionKind::Variable,
        "function" | "syntax_rule" | "syntax_rule_params" => DefinitionKind::Function,
        "class" | "define_class" | "register_class" => DefinitionKind::Class,
        "define_interface" | "register_interface" => DefinitionKind::Interface,
        "defmacro" | "define_macro" => DefinitionKind::Macro,
        "register_module" | "register_module_with_semantics" | "declare" | "module" => {
            DefinitionKind::Module
        }
        _ => DefinitionKind::Variable,
    }
}

/// Suppress unused-warning until we wire RuntimeValue inspection for diagnostics.
#[allow(dead_code)]
fn _runtime_value_doc(_value: &RuntimeValue) {}

fn collect_definitions(form: &ParsedForm, out: &mut Vec<Definition>) {
    let ParsedForm::List {
        items,
        span: form_span,
    } = form
    else {
        return;
    };
    let head = match items.first() {
        Some(ParsedForm::Symbol { text, .. }) => text.as_str(),
        _ => return,
    };
    // Kernel name-binders (`bind`/`lambda`): the kernel owns their surface shape,
    // so ask `caap_core::language` instead of re-encoding "bind binds its pairs"
    // here. Only `bind` locals surface as outline definitions; lambda parameters
    // belong to scope/param analysis, not the document outline.
    if let Some(names) = caap_core::language::introduced_names(form) {
        for name in names {
            if name.role == caap_core::language::NameRole::Local {
                out.push(Definition::leaf(
                    name.text.to_string(),
                    classify_value(name.value),
                    name.span.clone(),
                    name.form_span.clone(),
                    detail_for(name.text, name.value),
                ));
            }
        }
        return;
    }
    // Stdlib / grammar definer forms — a fast syntactic approximation. The
    // authoritative source for these is the bootstrap analysis, which asks the
    // compiler; the kernel cannot describe stdlib-defined forms.
    match head {
        // `(define-class registry "Name" parent ...)` — name is the 3rd item.
        "define_class" | "register_class" => {
            if let Some(name_form) = items.get(2) {
                if let Some((text, span)) = symbol_or_string(name_form) {
                    out.push(Definition::leaf(
                        text.clone(),
                        DefinitionKind::Class,
                        span.clone(),
                        form_span.clone(),
                        format!("class **{text}**"),
                    ));
                }
            }
        }
        "define_interface" | "register_interface" => {
            if let Some(name_form) = items.get(2) {
                if let Some((text, span)) = symbol_or_string(name_form) {
                    out.push(Definition::leaf(
                        text.clone(),
                        DefinitionKind::Interface,
                        span.clone(),
                        form_span.clone(),
                        format!("interface **{text}**"),
                    ));
                }
            }
        }
        "register_module" | "register_module_with_semantics" | "declare" => {
            if let Some(name_form) = items.get(1) {
                if let Some((text, span)) = symbol_or_string(name_form) {
                    out.push(Definition::leaf(
                        text.clone(),
                        DefinitionKind::Module,
                        span.clone(),
                        form_span.clone(),
                        format!("module **{text}**"),
                    ));
                }
            }
        }
        "defmacro" | "define_macro" => {
            if let Some(ParsedForm::Symbol { text, span }) = items.get(1) {
                out.push(Definition::leaf(
                    text.clone(),
                    DefinitionKind::Macro,
                    span.clone(),
                    form_span.clone(),
                    format!("macro **{text}**"),
                ));
            }
        }
        // `(syntax_rule "name" ...)` / `(syntax_rule_params "name" ...)` — a
        // grammar rule definition; the rule name is the (quoted-string) 2nd item.
        "syntax_rule" | "syntax_rule_params" => {
            if let Some(name_form) = items.get(1) {
                if let Some((text, span)) = symbol_or_string(name_form) {
                    out.push(Definition::leaf(
                        text.clone(),
                        DefinitionKind::Function,
                        span.clone(),
                        form_span.clone(),
                        format!("syntax rule **{text}**"),
                    ));
                }
            }
        }
        // `(register_exports "module" (bind ((name value) ...) ...))` — the
        // exported helpers live in the nested `bind`; recurse into every child
        // so the grouped bindings surface as definitions.
        "register_exports" => {
            for child in &items[1..] {
                collect_definitions(child, out);
            }
        }
        // `(do ...)` — recurse to collect nested top-level binds.
        "do" => {
            for child in &items[1..] {
                collect_definitions(child, out);
            }
        }
        _ => {}
    }
}

fn symbol_or_string(form: &ParsedForm) -> Option<(&String, &SourceSpan)> {
    match form {
        ParsedForm::Symbol { text, span }
        | ParsedForm::String {
            value: text, span, ..
        } => Some((text, span)),
        _ => None,
    }
}

fn classify_value(value: Option<&ParsedForm>) -> DefinitionKind {
    // A binding whose value is a lambda is a function — the kernel recognizes
    // lambda; the LSP does not redefine what one looks like.
    match value {
        Some(form) if caap_core::language::lambda_body(form).is_some() => DefinitionKind::Function,
        _ => DefinitionKind::Variable,
    }
}

fn detail_for(name: &str, value: Option<&ParsedForm>) -> String {
    match value {
        // Lambda value: render its parameter list (from the kernel) in the hover.
        Some(form) if caap_core::language::lambda_body(form).is_some() => {
            let params = caap_core::language::introduced_names(form)
                .map(|names| names.iter().map(|n| n.text).collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            format!("(lambda ({params}) ...) — **{name}**")
        }
        Some(ParsedForm::List { items, .. }) => match items.first() {
            Some(ParsedForm::Symbol { text, .. }) => format!("**{name}** = {text} …"),
            _ => format!("**{name}**"),
        },
        Some(ParsedForm::String { raw, .. }) => format!("**{name}** = {raw}"),
        Some(ParsedForm::Integer { raw, .. }) => format!("**{name}** = {raw}"),
        Some(ParsedForm::Boolean { value, .. }) => format!("**{name}** = {value}"),
        Some(ParsedForm::Null { .. }) => format!("**{name}** = null"),
        _ => format!("**{name}**"),
    }
}

/// Convert a CAAP 1-based line/col `SourceSpan` to an LSP 0-based `Range`.
///
/// LSP positions are formally UTF-16 code units; we treat columns as character
/// offsets which is exact for ASCII and a close approximation for most CAAP
/// identifiers (which avoid non-BMP characters in practice).
pub fn span_to_range(span: &SourceSpan) -> Range {
    Range {
        start: Position::new(
            span.start_line.saturating_sub(1) as u32,
            span.start_col.saturating_sub(1) as u32,
        ),
        end: Position::new(
            span.end_line.saturating_sub(1) as u32,
            span.end_col.saturating_sub(1) as u32,
        ),
    }
}

/// Byte length of the leading run of top-level parenthesized forms in `text`
/// (separated by whitespace/comments), stopping at the first non-`(` content.
/// All `(` / `)` / `"` / `;` markers are ASCII, so the returned offset always
/// lands on a char boundary.
fn leading_forms_prefix_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    let mut last_end = 0;
    loop {
        i = skip_trivia(bytes, i);
        if i >= n || bytes[i] != b'(' {
            break;
        }
        match matching_paren_end(bytes, i) {
            Some(end) => {
                last_end = end + 1;
                i = end + 1;
            }
            None => break,
        }
    }
    last_end
}

fn skip_trivia(bytes: &[u8], mut i: usize) -> usize {
    let n = bytes.len();
    loop {
        while i < n && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
            i += 1;
        }
        if i < n && bytes[i] == b';' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i + 1 < n && bytes[i] == b'#' && bytes[i + 1] == b'|' {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'|' && bytes[i + 1] == b'#') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        break;
    }
    i
}

/// Index of the `)` that closes the `(` at `start`, skipping string literals
/// and line comments. Returns `None` if unbalanced.
fn matching_paren_end(bytes: &[u8], start: usize) -> Option<usize> {
    let n = bytes.len();
    let mut depth = 0i32;
    let mut i = start;
    let mut in_string = false;
    while i < n {
        let c = bytes[i];
        if in_string {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b';' => {
                while i < n && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

pub fn position_in_span(pos: Position, span: &SourceSpan) -> bool {
    let range = span_to_range(span);
    let after_start = (pos.line, pos.character) >= (range.start.line, range.start.character);
    let before_end = (pos.line, pos.character) <= (range.end.line, range.end.character);
    after_start && before_end
}
