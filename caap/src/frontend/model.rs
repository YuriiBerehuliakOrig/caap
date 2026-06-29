use serde::{Deserialize, Serialize};

use crate::source::SourceSpan;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParsedSource {
    pub forms: Vec<ParsedForm>,
}

impl ParsedSource {
    pub fn is_empty(&self) -> bool {
        self.forms.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ParsedForm {
    List {
        items: Vec<ParsedForm>,
        span: SourceSpan,
    },
    Symbol {
        text: String,
        span: SourceSpan,
    },
    String {
        value: String,
        raw: String,
        span: SourceSpan,
    },
    Integer {
        value: i64,
        raw: String,
        span: SourceSpan,
    },
    Float {
        value: f64,
        raw: String,
        span: SourceSpan,
    },
    Boolean {
        value: bool,
        span: SourceSpan,
    },
    Null {
        span: SourceSpan,
    },
}

impl ParsedForm {
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::List { span, .. }
            | Self::Symbol { span, .. }
            | Self::String { span, .. }
            | Self::Integer { span, .. }
            | Self::Float { span, .. }
            | Self::Boolean { span, .. }
            | Self::Null { span } => span,
        }
    }

    pub fn head_symbol(&self) -> Option<&str> {
        let Self::List { items, .. } = self else {
            return None;
        };
        match items.first() {
            Some(Self::Symbol { text, .. }) => Some(text),
            _ => None,
        }
    }
}
