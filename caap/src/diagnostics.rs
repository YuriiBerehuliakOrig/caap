//! Structured diagnostics and rendering for Rust CAAP.

use std::collections::BTreeMap;

use crate::source::SourceSpan;
use crate::values::{EvaluationError, RuntimeCallFrame};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Note,
}

impl DiagnosticSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
        }
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
    pub fn new(name: impl Into<String>) -> Result<Self, String> {
        Self::with_location(name, None, None)
    }

    pub fn with_location(
        name: impl Into<String>,
        location: Option<String>,
        span: Option<SourceSpan>,
    ) -> Result<Self, String> {
        let name = name.into();
        if name.is_empty() {
            return Err("diagnostic frame name must be non-empty".to_string());
        }
        if location.as_ref().is_some_and(String::is_empty) {
            return Err("diagnostic frame location must be non-empty when present".to_string());
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
pub struct DiagnosticExplanation {
    pub code: String,
    pub title: String,
    pub body: String,
    pub help: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiagnosticExplanationRegistry {
    explanations: BTreeMap<String, DiagnosticExplanation>,
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
    pub fn error(message: impl Into<String>) -> Result<Self, String> {
        Self::new(DiagnosticSeverity::Error, message)
    }

    pub fn warning(message: impl Into<String>) -> Result<Self, String> {
        Self::new(DiagnosticSeverity::Warning, message)
    }

    pub fn note(message: impl Into<String>) -> Result<Self, String> {
        Self::new(DiagnosticSeverity::Note, message)
    }

    pub fn new(severity: DiagnosticSeverity, message: impl Into<String>) -> Result<Self, String> {
        let message = message.into();
        if message.is_empty() {
            return Err("diagnostic message must be non-empty".to_string());
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

    pub fn with_code(mut self, code: impl Into<String>) -> Result<Self, String> {
        let code = code.into();
        if code.is_empty() {
            return Err("diagnostic code must be non-empty".to_string());
        }
        self.code = Some(code);
        Ok(self)
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Result<Self, String> {
        let label = label.into();
        if label.is_empty() {
            return Err("diagnostic label must be non-empty".to_string());
        }
        self.label = Some(label);
        Ok(self)
    }

    pub fn with_span(mut self, span: SourceSpan) -> Self {
        self.location = Some(span_location(&span));
        self.span = Some(span);
        self
    }

    pub fn add_note(mut self, note: impl Into<String>) -> Result<Self, String> {
        let note = note.into();
        if note.is_empty() {
            return Err("diagnostic note must be non-empty".to_string());
        }
        self.notes.push(note);
        Ok(self)
    }

    pub fn add_help(mut self, help: impl Into<String>) -> Result<Self, String> {
        let help = help.into();
        if help.is_empty() {
            return Err("diagnostic help must be non-empty".to_string());
        }
        self.help.push(help);
        Ok(self)
    }

    pub fn add_fix(mut self, fix: DiagnosticFix) -> Result<Self, String> {
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
            code: Some("CAAP-RUNTIME-001".to_string()),
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
}

impl DiagnosticFix {
    pub fn new(label: impl Into<String>, kind: impl Into<String>) -> Result<Self, String> {
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
    ) -> Result<Self, String> {
        let mut metadata: Vec<(String, String)> = metadata.into_iter().collect();
        if metadata.iter().any(|(key, _)| key.is_empty()) {
            return Err("diagnostic fix metadata keys must be non-empty".to_string());
        }
        metadata.sort_by(|left, right| left.0.cmp(&right.0));
        self.metadata = metadata;
        Ok(self)
    }

    fn validate(&self) -> Result<(), String> {
        if self.label.is_empty() {
            return Err("diagnostic fix label must be non-empty".to_string());
        }
        if self.kind.is_empty() {
            return Err("diagnostic fix kind must be non-empty".to_string());
        }
        if self.metadata.iter().any(|(key, _)| key.is_empty()) {
            return Err("diagnostic fix metadata keys must be non-empty".to_string());
        }
        Ok(())
    }
}

impl DiagnosticExplanation {
    pub fn new(
        code: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Self, String> {
        let code = code.into();
        let title = title.into();
        let body = body.into();
        if code.is_empty() {
            return Err("diagnostic explanation code must be non-empty".to_string());
        }
        if title.is_empty() {
            return Err("diagnostic explanation title must be non-empty".to_string());
        }
        if body.is_empty() {
            return Err("diagnostic explanation body must be non-empty".to_string());
        }
        Ok(Self {
            code,
            title,
            body,
            help: Vec::new(),
        })
    }

    pub fn with_help(mut self, help: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let help: Vec<String> = help.into_iter().collect();
        if help.iter().any(String::is_empty) {
            return Err("diagnostic explanation help entries must be non-empty".to_string());
        }
        self.help = help;
        Ok(self)
    }
}

impl DiagnosticExplanationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, explanation: DiagnosticExplanation) {
        self.explanations
            .insert(explanation.code.clone(), explanation);
    }

    pub fn explain(&self, code: &str) -> Result<Option<&DiagnosticExplanation>, String> {
        if code.is_empty() {
            return Err("diagnostic explanation lookup code must be non-empty".to_string());
        }
        Ok(self.explanations.get(code))
    }

    pub fn codes(&self) -> Vec<&str> {
        self.explanations.keys().map(String::as_str).collect()
    }
}

impl CompilerEvent {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Result<Self, String> {
        Self::with_target(kind, None, message, [])
    }

    pub fn with_target(
        kind: impl Into<String>,
        target: Option<String>,
        message: impl Into<String>,
        metadata: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, String> {
        let kind = kind.into();
        let message = message.into();
        if kind.is_empty() {
            return Err("compiler event kind must be non-empty".to_string());
        }
        if message.is_empty() {
            return Err("compiler event message must be non-empty".to_string());
        }
        if target.as_ref().is_some_and(String::is_empty) {
            return Err("compiler event target must be non-empty when present".to_string());
        }
        let mut metadata: Vec<(String, String)> = metadata.into_iter().collect();
        if metadata
            .iter()
            .any(|(key, value)| key.is_empty() || value.is_empty())
        {
            return Err("compiler event metadata entries must be non-empty".to_string());
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

    pub fn by_kind(&self, kind: &str) -> Result<Vec<&CompilerEvent>, String> {
        if kind.is_empty() {
            return Err("compiler event kind lookup must be non-empty".to_string());
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
}
