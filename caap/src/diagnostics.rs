//! Structured diagnostics and rendering for CAAP.

use crate::error::{CaapError, CaapResult};
use crate::source::SourceSpan;
use crate::values::{EvaluationError, RuntimeCallFrame};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Note,
    Hint,
}

impl DiagnosticSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
            Self::Hint => "hint",
        }
    }
}

/// Typed, unique diagnostic codes.
///
/// Each variant maps to exactly one code string; the compiler rejects
/// duplicate variant names, so uniqueness is enforced at compile time.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum DiagnosticCode {
    /// A CAAP parse error produced by the PEG engine.
    Parse,
    /// A compiler-internal error (type checking, lowering, etc.).
    Compiler,
    /// A runtime evaluation error.
    Runtime,
    /// A host capability / sandbox policy violation.
    Capability,
    /// An artifact cache or fingerprinting error.
    Artifacts,
    /// An error in the diagnostics subsystem itself.
    Diagnostics,
    /// A provider graph / query-routing error.
    Graph,
    /// A host-function error (fs, net, proc).
    Host,
    /// An IR construction or validation error.
    Ir,
    /// A semantic analysis error (types, effects, policies).
    Semantic,
    /// A compilation unit error.
    Unit,
}

impl DiagnosticCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parse => "CAAP-PARSE-001",
            Self::Compiler => "CAAP-COMPILER-001",
            Self::Runtime => "CAAP-RUNTIME-001",
            Self::Capability => "CAAP-CAP-001",
            Self::Artifacts => "CAAP-ARTIFACTS-001",
            Self::Diagnostics => "CAAP-DIAGNOSTICS-001",
            Self::Graph => "CAAP-GRAPH-001",
            Self::Host => "CAAP-HOST-001",
            Self::Ir => "CAAP-IR-001",
            Self::Semantic => "CAAP-SEMANTIC-001",
            Self::Unit => "CAAP-UNIT-001",
        }
    }
}

impl From<DiagnosticCode> for String {
    fn from(code: DiagnosticCode) -> String {
        code.as_str().to_string()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticFix {
    pub label: String,
    pub kind: String,
    pub metadata: Vec<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticFrame {
    pub name: String,
    pub location: Option<String>,
    pub span: Option<SourceSpan>,
}

impl DiagnosticFrame {
    pub fn new(name: impl Into<String>) -> CaapResult<Self> {
        Self::with_location(name, None, None)
    }

    pub fn with_location(
        name: impl Into<String>,
        location: Option<String>,
        span: Option<SourceSpan>,
    ) -> CaapResult<Self> {
        let name = name.into();
        if name.is_empty() {
            return Err(CaapError::diagnostics(
                "diagnostic frame name must be non-empty",
            ));
        }
        if location.as_ref().is_some_and(String::is_empty) {
            return Err(CaapError::diagnostics(
                "diagnostic frame location must be non-empty when present",
            ));
        }
        Ok(Self {
            name,
            location,
            span,
        })
    }

    pub fn from_runtime_frame(frame: &RuntimeCallFrame) -> Self {
        let name = frame.name.clone().unwrap_or_else(|| "runtime".to_string());
        let location = frame.span.as_ref().map(span_location);
        Self {
            name,
            location,
            span: frame.span.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub code: Option<String>,
    pub label: Option<String>,
    pub span: Option<SourceSpan>,
    pub location: Option<String>,
    pub notes: Vec<String>,
    pub help: Vec<String>,
    pub context: Vec<String>,
    pub fixes: Vec<DiagnosticFix>,
    pub stack_trace: Vec<DiagnosticFrame>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompilerEvent {
    pub kind: String,
    pub target: Option<String>,
    pub message: String,
    pub metadata: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompilerEventLog {
    events: Vec<CompilerEvent>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>) -> CaapResult<Self> {
        Self::new(DiagnosticSeverity::Error, message)
    }

    pub fn warning(message: impl Into<String>) -> CaapResult<Self> {
        Self::new(DiagnosticSeverity::Warning, message)
    }

    pub fn note(message: impl Into<String>) -> CaapResult<Self> {
        Self::new(DiagnosticSeverity::Note, message)
    }

    pub fn new(severity: DiagnosticSeverity, message: impl Into<String>) -> CaapResult<Self> {
        let message = message.into();
        if message.is_empty() {
            return Err(CaapError::diagnostics(
                "diagnostic message must be non-empty",
            ));
        }
        Ok(Self {
            severity,
            message,
            code: None,
            label: None,
            span: None,
            location: None,
            notes: Vec::new(),
            help: Vec::new(),
            context: Vec::new(),
            fixes: Vec::new(),
            stack_trace: Vec::new(),
        })
    }

    pub fn with_code(mut self, code: impl Into<String>) -> CaapResult<Self> {
        let code = code.into();
        if code.is_empty() {
            return Err(CaapError::diagnostics("diagnostic code must be non-empty"));
        }
        self.code = Some(code);
        Ok(self)
    }

    pub fn with_label(mut self, label: impl Into<String>) -> CaapResult<Self> {
        let label = label.into();
        if label.is_empty() {
            return Err(CaapError::diagnostics("diagnostic label must be non-empty"));
        }
        self.label = Some(label);
        Ok(self)
    }

    pub fn with_span(mut self, span: SourceSpan) -> Self {
        self.location = Some(span_location(&span));
        self.span = Some(span);
        self
    }

    pub fn add_note(mut self, note: impl Into<String>) -> CaapResult<Self> {
        let note = note.into();
        if note.is_empty() {
            return Err(CaapError::diagnostics("diagnostic note must be non-empty"));
        }
        self.notes.push(note);
        Ok(self)
    }

    pub fn add_help(mut self, help: impl Into<String>) -> CaapResult<Self> {
        let help = help.into();
        if help.is_empty() {
            return Err(CaapError::diagnostics("diagnostic help must be non-empty"));
        }
        self.help.push(help);
        Ok(self)
    }

    pub fn add_fix(mut self, fix: DiagnosticFix) -> CaapResult<Self> {
        fix.validate()?;
        self.fixes.push(fix);
        Ok(self)
    }

    pub fn from_evaluation_error(error: &EvaluationError) -> Self {
        let stack_trace: Vec<DiagnosticFrame> = error
            .frames()
            .iter()
            .rev()
            .map(DiagnosticFrame::from_runtime_frame)
            .collect();
        let span = error.frames().first().and_then(|frame| frame.span.clone());
        let location = span.as_ref().map(span_location);
        Self {
            severity: DiagnosticSeverity::Error,
            message: error.message().to_string(),
            code: Some(DiagnosticCode::Runtime.as_str().to_string()),
            label: None,
            span,
            location,
            notes: Vec::new(),
            help: Vec::new(),
            context: Vec::new(),
            fixes: Vec::new(),
            stack_trace,
        }
    }

    pub fn from_caap_error(error: &CaapError, fallback_location: Option<&str>) -> Option<Self> {
        let contexts = caap_error_contexts(error);
        let leaf = caap_error_leaf(error);
        let mut diagnostic = match leaf {
            CaapError::Diagnostic(diagnostic) => diagnostic.as_ref().clone(),
            CaapError::Eval(error) => Self::from_evaluation_error(error),
            _ => {
                let code = diagnostic_code_for_caap_domain(leaf.domain())?;
                Self::error(leaf.message().to_string())
                    .and_then(|diagnostic| diagnostic.with_code(code))
                    .ok()?
            }
        };
        if diagnostic.location.is_none() {
            diagnostic.location = fallback_location.map(str::to_string);
        }
        if diagnostic.context.is_empty() {
            diagnostic.context.push(leaf.domain().to_string());
        }
        diagnostic.context.extend(contexts);
        Some(diagnostic)
    }
}

fn diagnostic_code_for_caap_domain(domain: &str) -> Option<DiagnosticCode> {
    match domain {
        "artifacts" => Some(DiagnosticCode::Artifacts),
        "parse" => Some(DiagnosticCode::Parse),
        "compiler" => Some(DiagnosticCode::Compiler),
        "diagnostics" => Some(DiagnosticCode::Diagnostics),
        "graph" => Some(DiagnosticCode::Graph),
        "host" => Some(DiagnosticCode::Host),
        "ir" => Some(DiagnosticCode::Ir),
        "semantic" => Some(DiagnosticCode::Semantic),
        "unit" => Some(DiagnosticCode::Unit),
        _ => None,
    }
}

fn caap_error_leaf(mut error: &CaapError) -> &CaapError {
    while let CaapError::Context { source, .. } = error {
        error = source;
    }
    error
}

fn caap_error_contexts(error: &CaapError) -> Vec<String> {
    let mut contexts = Vec::new();
    let mut current = error;
    while let CaapError::Context { context, source } = current {
        contexts.push(context.clone());
        current = source;
    }
    contexts.reverse();
    contexts
}

impl DiagnosticFix {
    pub fn new(label: impl Into<String>, kind: impl Into<String>) -> CaapResult<Self> {
        let fix = Self {
            label: label.into(),
            kind: kind.into(),
            metadata: Vec::new(),
        };
        fix.validate()?;
        Ok(fix)
    }

    pub fn with_metadata(
        mut self,
        metadata: impl IntoIterator<Item = (String, String)>,
    ) -> CaapResult<Self> {
        let mut metadata: Vec<(String, String)> = metadata.into_iter().collect();
        if metadata.iter().any(|(key, _)| key.is_empty()) {
            return Err(CaapError::diagnostics(
                "diagnostic fix metadata keys must be non-empty",
            ));
        }
        metadata.sort_by(|left, right| left.0.cmp(&right.0));
        self.metadata = metadata;
        Ok(self)
    }

    fn validate(&self) -> CaapResult<()> {
        if self.label.is_empty() {
            return Err(CaapError::diagnostics(
                "diagnostic fix label must be non-empty",
            ));
        }
        if self.kind.is_empty() {
            return Err(CaapError::diagnostics(
                "diagnostic fix kind must be non-empty",
            ));
        }
        if self.metadata.iter().any(|(key, _)| key.is_empty()) {
            return Err(CaapError::diagnostics(
                "diagnostic fix metadata keys must be non-empty",
            ));
        }
        Ok(())
    }
}

impl CompilerEvent {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> CaapResult<Self> {
        Self::with_target(kind, None, message, [])
    }

    pub fn with_target(
        kind: impl Into<String>,
        target: Option<String>,
        message: impl Into<String>,
        metadata: impl IntoIterator<Item = (String, String)>,
    ) -> CaapResult<Self> {
        let kind = kind.into();
        let message = message.into();
        if kind.is_empty() {
            return Err(CaapError::diagnostics(
                "compiler event kind must be non-empty",
            ));
        }
        if message.is_empty() {
            return Err(CaapError::diagnostics(
                "compiler event message must be non-empty",
            ));
        }
        if target.as_ref().is_some_and(String::is_empty) {
            return Err(CaapError::diagnostics(
                "compiler event target must be non-empty when present",
            ));
        }
        let mut metadata: Vec<(String, String)> = metadata.into_iter().collect();
        if metadata
            .iter()
            .any(|(key, value)| key.is_empty() || value.is_empty())
        {
            return Err(CaapError::diagnostics(
                "compiler event metadata entries must be non-empty",
            ));
        }
        metadata.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
        Ok(Self {
            kind,
            target,
            message,
            metadata,
        })
    }
}

impl CompilerEventLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn emit(&mut self, event: CompilerEvent) {
        self.events.push(event);
    }

    pub fn events(&self) -> &[CompilerEvent] {
        &self.events
    }

    pub fn by_kind(&self, kind: &str) -> CaapResult<Vec<&CompilerEvent>> {
        if kind.is_empty() {
            return Err(CaapError::diagnostics(
                "compiler event kind lookup must be non-empty",
            ));
        }
        Ok(self
            .events
            .iter()
            .filter(|event| event.kind == kind)
            .collect())
    }
}

pub fn render_diagnostic(diagnostic: &Diagnostic, source_text: Option<&str>) -> String {
    let mut lines = Vec::new();
    let code = diagnostic
        .code
        .as_ref()
        .map(|code| format!("[{code}]"))
        .unwrap_or_default();
    lines.push(format!(
        "{}{}: {}",
        diagnostic.severity.as_str(),
        code,
        diagnostic.message
    ));
    if let Some(span) = &diagnostic.span {
        lines.push(format!("  --> {}", span_location(span)));
        if let Some(source_line) = source_line(source_text, span.start_line) {
            let line_no = span.start_line.to_string();
            let gutter = " ".repeat(line_no.len());
            lines.push(format!("{gutter} |"));
            lines.push(format!("{line_no} | {source_line}"));
            lines.push(format!(
                "{gutter} | {}",
                caret_line(span, diagnostic.label.as_deref())
            ));
            lines.push(format!("{gutter} |"));
        }
    } else if let Some(location) = &diagnostic.location {
        lines.push(format!("  --> {location}"));
    }
    for item in &diagnostic.context {
        lines.push(format!("context: {item}"));
    }
    for item in &diagnostic.notes {
        lines.push(format!("note: {item}"));
    }
    for item in &diagnostic.help {
        lines.push(format!("help: {item}"));
    }
    if !diagnostic.stack_trace.is_empty() {
        lines.push("stack trace:".to_string());
        for frame in &diagnostic.stack_trace {
            let location = frame
                .location
                .clone()
                .or_else(|| frame.span.as_ref().map(span_location))
                .unwrap_or_default();
            lines.push(format!("  at {} ({location})", frame.name));
        }
    }
    lines.join("\n")
}

pub fn span_location(span: &SourceSpan) -> String {
    format!(
        "{}:{}:{}",
        span.path.as_deref().unwrap_or("<input>"),
        span.start_line,
        span.start_col
    )
}

fn source_line(source_text: Option<&str>, line: usize) -> Option<&str> {
    let text = source_text?;
    text.lines().nth(line.saturating_sub(1))
}

fn caret_line(span: &SourceSpan, label: Option<&str>) -> String {
    let start = span.start_col.max(1);
    let end = span.end_col.max(start + 1);
    let width = (end - start).max(1);
    let mut caret = format!("{}{}", " ".repeat(start - 1), "^".repeat(width));
    if let Some(label) = label {
        caret.push(' ');
        caret.push_str(label);
    }
    caret
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_builder_rejects_empty_message_and_sets_location() {
        assert!(Diagnostic::error("").is_err());
        let span = SourceSpan::new(0, 4, 1, 1, 1, 5).unwrap();
        let diagnostic = Diagnostic::error("boom")
            .unwrap()
            .with_code("CAAP-TEST")
            .unwrap()
            .with_span(span);
        assert_eq!(diagnostic.severity, DiagnosticSeverity::Error);
        assert_eq!(diagnostic.code.as_deref(), Some("CAAP-TEST"));
        assert_eq!(diagnostic.location.as_deref(), Some("<input>:1:1"));
    }

    #[test]
    fn compiler_event_log_filters_by_kind() {
        let mut log = CompilerEventLog::new();
        log.emit(CompilerEvent::new("query", "planned").unwrap());
        log.emit(CompilerEvent::new("eval", "started").unwrap());
        assert_eq!(log.by_kind("query").unwrap().len(), 1);
        assert!(log.by_kind("").is_err());
    }

    #[test]
    fn caap_error_projects_to_structured_diagnostic() {
        let diagnostic = Diagnostic::from_caap_error(
            &CaapError::semantic("duplicate symbol"),
            Some("demo.caap"),
        )
        .expect("semantic errors should project to diagnostics");

        assert_eq!(diagnostic.code.as_deref(), Some("CAAP-SEMANTIC-001"));
        assert_eq!(diagnostic.message, "duplicate symbol");
        assert_eq!(diagnostic.location.as_deref(), Some("demo.caap"));
        assert_eq!(diagnostic.context, vec!["semantic"]);
    }

    #[test]
    fn caap_error_diagnostic_projection_preserves_context_chain() {
        let error = CaapError::compiler("provider failed")
            .with_context("query stage analyze")
            .with_context("bootstrap demo");
        let diagnostic = Diagnostic::from_caap_error(&error, Some("demo.caap"))
            .expect("contextual compiler errors should project to diagnostics");

        assert_eq!(diagnostic.code.as_deref(), Some("CAAP-COMPILER-001"));
        assert_eq!(diagnostic.message, "provider failed");
        assert_eq!(diagnostic.location.as_deref(), Some("demo.caap"));
        assert_eq!(
            diagnostic.context,
            vec!["compiler", "query stage analyze", "bootstrap demo"]
        );
    }

    #[test]
    fn caap_error_diagnostic_projection_preserves_embedded_diagnostic() {
        let embedded = Diagnostic::error("capability denied")
            .and_then(|diagnostic| diagnostic.with_code(DiagnosticCode::Capability))
            .unwrap();
        let diagnostic =
            Diagnostic::from_caap_error(&CaapError::diagnostic(embedded), Some("demo.caap"))
                .expect("embedded diagnostics should project");

        assert_eq!(diagnostic.code.as_deref(), Some("CAAP-CAP-001"));
        assert_eq!(diagnostic.message, "capability denied");
        assert_eq!(diagnostic.location.as_deref(), Some("demo.caap"));
        assert_eq!(diagnostic.context, vec!["diagnostic"]);
    }
}
