use std::fmt;

use crate::diagnostics::Diagnostic;
use crate::values::{EvalSignal, EvaluationError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaapError {
    Artifacts(String),
    Compiler(String),
    Context {
        context: String,
        source: Box<CaapError>,
    },
    Diagnostic(Box<Diagnostic>),
    Diagnostics(String),
    Eval(EvaluationError),
    Graph(String),
    Host(String),
    Ir(String),
    Parse(String),
    Semantic(String),
    Unit(String),
}

pub type CaapResult<T> = Result<T, CaapError>;

impl CaapError {
    pub fn artifacts(message: impl Into<String>) -> Self {
        Self::Artifacts(message.into())
    }

    pub fn compiler(message: impl Into<String>) -> Self {
        Self::Compiler(message.into())
    }

    pub fn diagnostic(diagnostic: Diagnostic) -> Self {
        Self::Diagnostic(Box::new(diagnostic))
    }

    pub fn with_context(self, context: impl Into<String>) -> Self {
        let context = context.into();
        if context.is_empty() {
            return self;
        }
        Self::Context {
            context,
            source: Box::new(self),
        }
    }

    pub fn diagnostics(message: impl Into<String>) -> Self {
        Self::Diagnostics(message.into())
    }

    pub fn graph(message: impl Into<String>) -> Self {
        Self::Graph(message.into())
    }

    pub fn host(message: impl Into<String>) -> Self {
        Self::Host(message.into())
    }

    pub fn ir(message: impl Into<String>) -> Self {
        Self::Ir(message.into())
    }

    pub fn parse(message: impl Into<String>) -> Self {
        Self::Parse(message.into())
    }

    pub fn semantic(message: impl Into<String>) -> Self {
        Self::Semantic(message.into())
    }

    pub fn unit(message: impl Into<String>) -> Self {
        Self::Unit(message.into())
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Artifacts(message)
            | Self::Compiler(message)
            | Self::Diagnostics(message)
            | Self::Graph(message)
            | Self::Host(message)
            | Self::Ir(message)
            | Self::Parse(message)
            | Self::Semantic(message)
            | Self::Unit(message) => message,
            Self::Context { context, .. } => context,
            Self::Diagnostic(diagnostic) => &diagnostic.message,
            Self::Eval(error) => error.message(),
        }
    }

    pub fn as_diagnostic(&self) -> Option<&Diagnostic> {
        match self {
            Self::Diagnostic(diagnostic) => Some(diagnostic),
            Self::Context { source, .. } => source.as_diagnostic(),
            _ => None,
        }
    }

    pub fn domain(&self) -> &'static str {
        match self {
            Self::Artifacts(_) => "artifacts",
            Self::Compiler(_) => "compiler",
            Self::Context { source, .. } => source.domain(),
            Self::Diagnostic(_) => "diagnostic",
            Self::Diagnostics(_) => "diagnostics",
            Self::Eval(_) => "eval",
            Self::Graph(_) => "graph",
            Self::Host(_) => "host",
            Self::Ir(_) => "ir",
            Self::Parse(_) => "parse",
            Self::Semantic(_) => "semantic",
            Self::Unit(_) => "unit",
        }
    }
}

impl fmt::Display for CaapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Context { context, source } => {
                write!(
                    f,
                    "{} error: {context}: {}",
                    source.domain(),
                    source.message()
                )
            }
            Self::Diagnostic(diagnostic) => match &diagnostic.code {
                Some(code) => write!(f, "diagnostic error [{code}]: {}", diagnostic.message),
                None => write!(f, "diagnostic error: {}", diagnostic.message),
            },
            Self::Eval(error) => write!(f, "{error}"),
            _ => write!(f, "{} error: {}", self.domain(), self.message()),
        }
    }
}

impl std::error::Error for CaapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Context { source, .. } => Some(source.as_ref()),
            Self::Eval(error) => Some(error),
            _ => None,
        }
    }
}

impl From<EvaluationError> for CaapError {
    fn from(error: EvaluationError) -> Self {
        Self::Eval(error)
    }
}

impl From<EvalSignal> for CaapError {
    fn from(signal: EvalSignal) -> Self {
        match signal {
            EvalSignal::Error(error) => Self::Eval(error),
            EvalSignal::Leave(leave) => Self::Eval(EvaluationError::new(format!(
                "uncaught leave signal for block {}",
                leave.target_block_id
            ))),
            EvalSignal::Exception(val) => {
                Self::Eval(EvaluationError::new(format!("uncaught exception: {val}")))
            }
            EvalSignal::TailCall(_) => Self::Eval(EvaluationError::new(
                "internal tail-call signal escaped its closure",
            )),
        }
    }
}

impl From<CaapError> for EvalSignal {
    fn from(error: CaapError) -> Self {
        match error {
            CaapError::Eval(error) => EvalSignal::Error(error),
            other => EvalSignal::Error(EvaluationError::new(other.to_string())),
        }
    }
}

impl From<CaapError> for String {
    fn from(error: CaapError) -> Self {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::DiagnosticCode;

    #[test]
    fn caap_error_display_includes_domain_for_structural_errors() {
        let error = CaapError::semantic("duplicate symbol");
        assert_eq!(error.domain(), "semantic");
        assert_eq!(error.message(), "duplicate symbol");
        assert_eq!(error.to_string(), "semantic error: duplicate symbol");
    }

    #[test]
    fn caap_error_converts_to_eval_signal_without_losing_message() {
        let signal = EvalSignal::from(CaapError::graph("missing node"));
        let EvalSignal::Error(error) = signal else {
            panic!("expected error signal");
        };
        assert!(error.message().contains("graph error: missing node"));
    }

    #[test]
    fn caap_error_can_carry_structured_diagnostic() {
        let diagnostic = Diagnostic::error("capability denied")
            .and_then(|diagnostic| diagnostic.with_code(DiagnosticCode::Capability))
            .unwrap();
        let error = CaapError::diagnostic(diagnostic.clone());

        assert_eq!(error.domain(), "diagnostic");
        assert_eq!(error.message(), "capability denied");
        assert_eq!(error.as_diagnostic(), Some(&diagnostic));
        assert_eq!(
            error.to_string(),
            "diagnostic error [CAAP-CAP-001]: capability denied"
        );
    }

    #[test]
    fn caap_error_context_preserves_domain_and_source_chain() {
        let error = CaapError::compiler("provider failed").with_context("query stage lower");

        assert_eq!(error.domain(), "compiler");
        assert_eq!(error.message(), "query stage lower");
        assert_eq!(
            error.to_string(),
            "compiler error: query stage lower: provider failed"
        );
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn empty_caap_error_context_is_ignored() {
        let error = CaapError::unit("bad unit").with_context("");

        assert_eq!(error, CaapError::unit("bad unit"));
    }
}
