//! Snapshot-safe source location data owned by CAAP core.

use crate::error::{CaapError, CaapResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize)]
pub struct SourcePoint {
    pub offset: usize,
    pub line: usize,
    pub column: usize,
}

impl SourcePoint {
    pub fn new(offset: usize, line: usize, column: usize) -> CaapResult<Self> {
        if line == 0 {
            return Err(CaapError::parse("source point line must be positive"));
        }
        if column == 0 {
            return Err(CaapError::parse("source point column must be positive"));
        }
        Ok(Self {
            offset,
            line,
            column,
        })
    }
}

impl<'de> Deserialize<'de> for SourcePoint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SourcePointData {
            offset: usize,
            line: usize,
            column: usize,
        }

        let data = SourcePointData::deserialize(deserializer)?;
        Self::new(data.offset, data.line, data.column).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize)]
pub struct SourceRange {
    pub start: SourcePoint,
    pub end: SourcePoint,
}

impl SourceRange {
    pub fn new(start: SourcePoint, end: SourcePoint) -> CaapResult<Self> {
        if end.offset < start.offset {
            return Err(CaapError::parse(
                "source range end offset must not precede start offset",
            ));
        }
        if (end.line, end.column) < (start.line, start.column) {
            return Err(CaapError::parse(
                "source range end location must not precede start location",
            ));
        }
        Ok(Self { start, end })
    }
}

impl<'de> Deserialize<'de> for SourceRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SourceRangeData {
            start: SourcePoint,
            end: SourcePoint,
        }

        let data = SourceRangeData::deserialize(deserializer)?;
        Self::new(data.start, data.end).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize)]
pub struct SourceSpanLocator {
    pub file_id: Option<u32>,
    pub path: Option<String>,
}

impl SourceSpanLocator {
    pub fn new(file_id: Option<u32>, path: Option<String>) -> CaapResult<Self> {
        if path.as_ref().is_some_and(|value| value.is_empty()) {
            return Err(CaapError::parse(
                "source span path must be non-empty when present",
            ));
        }
        Ok(Self { file_id, path })
    }
}

impl<'de> Deserialize<'de> for SourceSpanLocator {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SourceSpanLocatorData {
            file_id: Option<u32>,
            path: Option<String>,
        }

        let data = SourceSpanLocatorData::deserialize(deserializer)?;
        Self::new(data.file_id, data.path).map_err(serde::de::Error::custom)
    }
}

impl SourceSpan {
    pub fn new(
        start: usize,
        end: usize,
        start_line: usize,
        start_col: usize,
        end_line: usize,
        end_col: usize,
    ) -> CaapResult<Self> {
        Self::with_locator(None, start, end, start_line, start_col, end_line, end_col)
    }

    pub fn with_locator(
        locator: Option<SourceSpanLocator>,
        start: usize,
        end: usize,
        start_line: usize,
        start_col: usize,
        end_line: usize,
        end_col: usize,
    ) -> CaapResult<Self> {
        let locator = locator.unwrap_or(SourceSpanLocator::new(None, None)?);
        let locator = SourceSpanLocator::new(locator.file_id, locator.path)?;
        if end < start {
            return Err(CaapError::parse(
                "source span end offset must not precede start offset",
            ));
        }
        if start_line == 0 || start_col == 0 || end_line == 0 || end_col == 0 {
            return Err(CaapError::parse(
                "source span line/column values must be positive",
            ));
        }
        if (end_line, end_col) < (start_line, start_col) {
            return Err(CaapError::parse(
                "source span end location must not precede start location",
            ));
        }
        Ok(Self {
            file_id: locator.file_id,
            start,
            end,
            path: locator.path,
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

impl<'de> Deserialize<'de> for SourceSpan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SourceSpanData {
            file_id: Option<u32>,
            start: usize,
            end: usize,
            path: Option<String>,
            start_line: usize,
            start_col: usize,
            end_line: usize,
            end_col: usize,
        }

        let data = SourceSpanData::deserialize(deserializer)?;
        Self::with_locator(
            Some(SourceSpanLocator {
                file_id: data.file_id,
                path: data.path,
            }),
            data.start,
            data.end,
            data.start_line,
            data.start_col,
            data.end_line,
            data.end_col,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_span_range_roundtrips_points() {
        let span = SourceSpan::with_locator(
            Some(SourceSpanLocator {
                file_id: Some(7),
                path: Some("demo.caap".to_string()),
            }),
            3,
            9,
            1,
            4,
            1,
            10,
        )
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

    #[test]
    fn source_point_deserialize_rejects_zero_line() {
        let err =
            serde_json::from_str::<SourcePoint>(r#"{"offset":0,"line":0,"column":1}"#).unwrap_err();
        assert!(err.to_string().contains("line must be positive"));
    }

    #[test]
    fn source_range_deserialize_rejects_reversed_offsets() {
        let err = serde_json::from_str::<SourceRange>(
            r#"{"start":{"offset":2,"line":1,"column":3},"end":{"offset":1,"line":1,"column":2}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("end offset"));
    }

    #[test]
    fn source_span_deserialize_rejects_empty_path() {
        let err = serde_json::from_str::<SourceSpan>(
            r#"{"file_id":null,"start":0,"end":1,"path":"","start_line":1,"start_col":1,"end_line":1,"end_col":2}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("path must be non-empty"));
    }

    #[test]
    fn source_span_locator_deserialize_rejects_empty_path() {
        let err =
            serde_json::from_str::<SourceSpanLocator>(r#"{"file_id":null,"path":""}"#).unwrap_err();
        assert!(err.to_string().contains("path must be non-empty"));
    }
}
