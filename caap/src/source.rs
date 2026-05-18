//! Snapshot-safe source location data owned by CAAP core.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct SourcePoint {
    pub offset: usize,
    pub line: usize,
    pub column: usize,
}

impl SourcePoint {
    pub fn new(offset: usize, line: usize, column: usize) -> Result<Self, String> {
        if line == 0 {
            return Err("source point line must be positive".to_string());
        }
        if column == 0 {
            return Err("source point column must be positive".to_string());
        }
        Ok(Self {
            offset,
            line,
            column,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct SourceRange {
    pub start: SourcePoint,
    pub end: SourcePoint,
}

impl SourceRange {
    pub fn new(start: SourcePoint, end: SourcePoint) -> Result<Self, String> {
        if end.offset < start.offset {
            return Err("source range end offset must not precede start offset".to_string());
        }
        if (end.line, end.column) < (start.line, start.column) {
            return Err("source range end location must not precede start location".to_string());
        }
        Ok(Self { start, end })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct SourceSpan {
    pub file_id: Option<u32>,
    pub start: usize,
    pub end: usize,
    pub path: Option<String>,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

impl SourceSpan {
    pub fn new(
        start: usize,
        end: usize,
        start_line: usize,
        start_col: usize,
        end_line: usize,
        end_col: usize,
    ) -> Result<Self, String> {
        Self::with_locator(
            None, start, end, None, start_line, start_col, end_line, end_col,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_locator(
        file_id: Option<u32>,
        start: usize,
        end: usize,
        path: Option<String>,
        start_line: usize,
        start_col: usize,
        end_line: usize,
        end_col: usize,
    ) -> Result<Self, String> {
        if end < start {
            return Err("source span end offset must not precede start offset".to_string());
        }
        if start_line == 0 || start_col == 0 || end_line == 0 || end_col == 0 {
            return Err("source span line/column values must be positive".to_string());
        }
        if (end_line, end_col) < (start_line, start_col) {
            return Err("source span end location must not precede start location".to_string());
        }
        if path.as_ref().is_some_and(|p| p.is_empty()) {
            return Err("source span path must be non-empty when present".to_string());
        }
        Ok(Self {
            file_id,
            start,
            end,
            path,
            start_line,
            start_col,
            end_line,
            end_col,
        })
    }

    pub fn range(&self) -> SourceRange {
        SourceRange {
            start: SourcePoint {
                offset: self.start,
                line: self.start_line,
                column: self.start_col,
            },
            end: SourcePoint {
                offset: self.end,
                line: self.end_line,
                column: self.end_col,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_span_range_roundtrips_points() {
        let span =
            SourceSpan::with_locator(Some(7), 3, 9, Some("demo.caap".to_string()), 1, 4, 1, 10)
                .unwrap();

        let range = span.range();
        assert_eq!(range.start, SourcePoint::new(3, 1, 4).unwrap());
        assert_eq!(range.end, SourcePoint::new(9, 1, 10).unwrap());
        assert_eq!(span.file_id, Some(7));
    }

    #[test]
    fn source_span_rejects_reversed_offsets() {
        assert!(SourceSpan::new(10, 9, 1, 1, 1, 2).is_err());
    }
}
