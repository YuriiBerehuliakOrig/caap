use std::fmt;

use crate::values::{EvalSignal, EvaluationError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaapError {
    Artifacts(String),
    Compiler(String),
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
            | Self::Graph(message)
            | Self::Host(message)
            | Self::Ir(message)
            | Self::Parse(message)
            | Self::Semantic(message)
            | Self::Unit(message) => message,
            Self::Eval(error) => error.message(),
        }
    }

    pub fn domain(&self) -> &'static str {
        match self {
            Self::Artifacts(_) => "artifacts",
            Self::Compiler(_) => "compiler",
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
            Self::Eval(error) => write!(f, "{error}"),
            _ => write!(f, "{} error: {}", self.domain(), self.message()),
        }
    }
}

impl std::error::Error for CaapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
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
}
