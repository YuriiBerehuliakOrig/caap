use std::collections::HashMap;

/// Upper bound on the buffer a single caller-driven read may allocate up front.
///
/// Caller-supplied `max_bytes` values are clamped to this so that a hostile or
/// buggy request (e.g. `max_bytes` near `i64::MAX`) cannot trigger a multi-
/// terabyte zeroed allocation that aborts the host with OOM. Partial reads are
/// already expected by socket/UDP callers, so capping per-read is safe.
pub(crate) const MAX_READ_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum SysValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<SysValue>),
    Map(HashMap<String, SysValue>),
}

/// Category of a [`SysError`], letting a host react programmatically instead of
/// string-matching: distinguish a timeout from a real failure, a missing path
/// from a permission denial, an exhausted handle pool from a bad argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysErrorKind {
    /// Bad argument: wrong type, out of range, or malformed.
    InvalidArgument,
    /// A named resource (path, handle, host) does not exist.
    NotFound,
    /// The operation was refused by the OS or by host policy.
    PermissionDenied,
    /// A resource that must not already exist does.
    AlreadyExists,
    /// A non-blocking operation has no data ready yet.
    WouldBlock,
    /// A deadline elapsed before the operation completed.
    TimedOut,
    /// The operation or value is not supported on this platform/build.
    Unsupported,
    /// A bounded pool (handles, memory) is exhausted.
    ResourceExhausted,
    /// The operation was interrupted (e.g. by a signal) and may be retried.
    Interrupted,
    /// Any error without a more specific classification.
    Other,
}

impl SysErrorKind {
    /// Stable snake_case name for the kind, suitable as a category tag surfaced
    /// to host diagnostics and (eventually) to CAAP programs. Stable across
    /// versions so callers may match on it.
    pub fn as_str(&self) -> &'static str {
        match self {
            SysErrorKind::InvalidArgument => "invalid_argument",
            SysErrorKind::NotFound => "not_found",
            SysErrorKind::PermissionDenied => "permission_denied",
            SysErrorKind::AlreadyExists => "already_exists",
            SysErrorKind::WouldBlock => "would_block",
            SysErrorKind::TimedOut => "timed_out",
            SysErrorKind::Unsupported => "unsupported",
            SysErrorKind::ResourceExhausted => "resource_exhausted",
            SysErrorKind::Interrupted => "interrupted",
            SysErrorKind::Other => "other",
        }
    }
}

/// A runtime error carrying a programmatic [`SysErrorKind`] alongside its
/// human-readable message.
///
/// `Display` and `Deref<Target = str>` both expose the message verbatim, so the
/// type is a near-drop-in for the `String` errors it replaced: `format!`,
/// `.contains(..)`, and `Into<String>` (used by host adapters) all keep working,
/// while `kind()` now exposes the classification that string errors discarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysError {
    kind: SysErrorKind,
    message: String,
}

impl SysError {
    pub fn new(kind: SysErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> SysErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::new(SysErrorKind::Other, message)
    }
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(SysErrorKind::InvalidArgument, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(SysErrorKind::NotFound, message)
    }
    pub fn permission_denied(message: impl Into<String>) -> Self {
        Self::new(SysErrorKind::PermissionDenied, message)
    }
    pub fn already_exists(message: impl Into<String>) -> Self {
        Self::new(SysErrorKind::AlreadyExists, message)
    }
    pub fn would_block(message: impl Into<String>) -> Self {
        Self::new(SysErrorKind::WouldBlock, message)
    }
    pub fn timed_out(message: impl Into<String>) -> Self {
        Self::new(SysErrorKind::TimedOut, message)
    }
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(SysErrorKind::Unsupported, message)
    }
    pub fn resource_exhausted(message: impl Into<String>) -> Self {
        Self::new(SysErrorKind::ResourceExhausted, message)
    }
    pub fn interrupted(message: impl Into<String>) -> Self {
        Self::new(SysErrorKind::Interrupted, message)
    }

    /// Build from a `std::io::Error`, mapping its `ErrorKind` to a
    /// [`SysErrorKind`] and formatting the message as `"{context}: {error}"` —
    /// identical text to the string errors this replaced, now with the kind kept.
    pub fn from_io(context: &str, error: std::io::Error) -> Self {
        Self::new(io_error_kind(error.kind()), format!("{context}: {error}"))
    }
}

fn io_error_kind(kind: std::io::ErrorKind) -> SysErrorKind {
    use std::io::ErrorKind;
    match kind {
        ErrorKind::NotFound => SysErrorKind::NotFound,
        ErrorKind::PermissionDenied => SysErrorKind::PermissionDenied,
        ErrorKind::AlreadyExists => SysErrorKind::AlreadyExists,
        ErrorKind::WouldBlock => SysErrorKind::WouldBlock,
        ErrorKind::TimedOut => SysErrorKind::TimedOut,
        ErrorKind::Interrupted => SysErrorKind::Interrupted,
        ErrorKind::Unsupported => SysErrorKind::Unsupported,
        ErrorKind::OutOfMemory => SysErrorKind::ResourceExhausted,
        ErrorKind::InvalidInput | ErrorKind::InvalidData => SysErrorKind::InvalidArgument,
        _ => SysErrorKind::Other,
    }
}

impl std::fmt::Display for SysError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SysError {}

// Exposes the message as `&str` so the pervasive `error.contains(..)` checks (and
// other `str` reads) keep working unchanged after the migration from `String`.
impl std::ops::Deref for SysError {
    type Target = str;
    fn deref(&self) -> &str {
        &self.message
    }
}

// `?` on the many helpers that still return `Result<_, String>` converts their
// untyped errors into the `Other` kind.
impl From<String> for SysError {
    fn from(message: String) -> Self {
        Self::other(message)
    }
}
impl From<&str> for SysError {
    fn from(message: &str) -> Self {
        Self::other(message)
    }
}
// Lets host adapters keep using `Into<String>` (e.g. `map_err(eval_err)`).
impl From<SysError> for String {
    fn from(error: SysError) -> Self {
        error.message
    }
}

pub type SysResult = Result<SysValue, SysError>;

pub struct SysArgs(pub Vec<SysValue>);

pub struct SysMap(pub HashMap<String, SysValue>);

impl SysArgs {
    pub fn require_value(&self, idx: usize, ctx: &str) -> Result<&SysValue, SysError> {
        self.0
            .get(idx)
            .ok_or_else(|| SysError::invalid_argument(format!("{ctx}: missing arg {idx}")))
    }

    pub fn optional(&self, idx: usize) -> Option<&SysValue> {
        self.0.get(idx)
    }

    pub fn iter(&self) -> std::slice::Iter<'_, SysValue> {
        self.0.iter()
    }

    pub fn require_str(&self, idx: usize, ctx: &str) -> Result<String, SysError> {
        match self.0.get(idx) {
            Some(SysValue::Str(s)) => Ok(s.clone()),
            Some(other) => Err(SysError::invalid_argument(format!(
                "{ctx}: arg {idx} must be string, got {:?}",
                other
            ))),
            None => Err(SysError::invalid_argument(format!(
                "{ctx}: missing arg {idx}"
            ))),
        }
    }

    pub fn require_int(&self, idx: usize, ctx: &str) -> Result<i64, SysError> {
        match self.0.get(idx) {
            Some(SysValue::Int(n)) => Ok(*n),
            Some(other) => Err(SysError::invalid_argument(format!(
                "{ctx}: arg {idx} must be int, got {:?}",
                other
            ))),
            None => Err(SysError::invalid_argument(format!(
                "{ctx}: missing arg {idx}"
            ))),
        }
    }

    pub fn require_float(&self, idx: usize, ctx: &str) -> Result<f64, SysError> {
        match self.0.get(idx) {
            Some(SysValue::Float(n)) => Ok(*n),
            Some(SysValue::Int(n)) => {
                // i64 values with |n| > 2^53 cannot be exactly represented as f64.
                let f = *n as f64;
                if f as i64 != *n {
                    return Err(SysError::invalid_argument(format!(
                        "{ctx}: arg {idx}: integer {n} cannot be exactly represented as float"
                    )));
                }
                Ok(f)
            }
            Some(other) => Err(SysError::invalid_argument(format!(
                "{ctx}: arg {idx} must be float, got {:?}",
                other
            ))),
            None => Err(SysError::invalid_argument(format!(
                "{ctx}: missing arg {idx}"
            ))),
        }
    }

    pub fn require_map(&self, idx: usize, ctx: &str) -> Result<SysMap, SysError> {
        match self.0.get(idx) {
            Some(SysValue::Map(m)) => Ok(SysMap(m.clone())),
            Some(other) => Err(SysError::invalid_argument(format!(
                "{ctx}: arg {idx} must be map, got {:?}",
                other
            ))),
            None => Err(SysError::invalid_argument(format!(
                "{ctx}: missing arg {idx}"
            ))),
        }
    }

    pub fn require_list(&self, idx: usize, ctx: &str) -> Result<Vec<SysValue>, SysError> {
        match self.0.get(idx) {
            Some(SysValue::List(v)) => Ok(v.clone()),
            Some(other) => Err(SysError::invalid_argument(format!(
                "{ctx}: arg {idx} must be list, got {:?}",
                other
            ))),
            None => Err(SysError::invalid_argument(format!(
                "{ctx}: missing arg {idx}"
            ))),
        }
    }

    pub fn require_bytes(&self, idx: usize, ctx: &str) -> Result<Vec<u8>, SysError> {
        match self.0.get(idx) {
            Some(SysValue::Bytes(b)) => Ok(b.clone()),
            Some(other) => Err(SysError::invalid_argument(format!(
                "{ctx}: arg {idx} must be bytes, got {:?}",
                other
            ))),
            None => Err(SysError::invalid_argument(format!(
                "{ctx}: missing arg {idx}"
            ))),
        }
    }
}

impl SysMap {
    pub fn require_str(&self, key: &str, ctx: &str) -> Result<String, SysError> {
        match self.0.get(key) {
            Some(SysValue::Str(s)) => Ok(s.clone()),
            Some(other) => Err(SysError::invalid_argument(format!(
                "{ctx}: field '{key}' must be string, got {:?}",
                other
            ))),
            None => Err(SysError::invalid_argument(format!(
                "{ctx}: missing field '{key}'"
            ))),
        }
    }

    pub fn require_int(&self, key: &str, ctx: &str) -> Result<i64, SysError> {
        match self.0.get(key) {
            Some(SysValue::Int(n)) => Ok(*n),
            Some(other) => Err(SysError::invalid_argument(format!(
                "{ctx}: field '{key}' must be int, got {:?}",
                other
            ))),
            None => Err(SysError::invalid_argument(format!(
                "{ctx}: missing field '{key}'"
            ))),
        }
    }

    pub fn require_float(&self, key: &str, ctx: &str) -> Result<f64, SysError> {
        match self.0.get(key) {
            Some(SysValue::Float(n)) => Ok(*n),
            Some(SysValue::Int(n)) => {
                let f = *n as f64;
                if f as i64 != *n {
                    return Err(SysError::invalid_argument(format!(
                        "{ctx}: field '{key}': integer {n} cannot be exactly represented as float"
                    )));
                }
                Ok(f)
            }
            Some(other) => Err(SysError::invalid_argument(format!(
                "{ctx}: field '{key}' must be float, got {:?}",
                other
            ))),
            None => Err(SysError::invalid_argument(format!(
                "{ctx}: missing field '{key}'"
            ))),
        }
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.0.get(key) {
            Some(SysValue::Str(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn get_int(&self, key: &str) -> Option<i64> {
        match self.0.get(key) {
            Some(SysValue::Int(n)) => Some(*n),
            _ => None,
        }
    }

    pub fn get_float(&self, key: &str) -> Option<f64> {
        match self.0.get(key) {
            Some(SysValue::Float(n)) => Some(*n),
            Some(SysValue::Int(n)) => Some(*n as f64),
            _ => None,
        }
    }

    pub fn bool(&self, key: &str) -> bool {
        matches!(self.0.get(key), Some(SysValue::Bool(true)))
    }

    pub fn get_list(&self, key: &str) -> Option<&Vec<SysValue>> {
        match self.0.get(key) {
            Some(SysValue::List(v)) => Some(v),
            _ => None,
        }
    }

    pub fn get_map(&self, key: &str) -> Option<SysMap> {
        match self.0.get(key) {
            Some(SysValue::Map(m)) => Some(SysMap(m.clone())),
            _ => None,
        }
    }
}

pub fn val_to_display_string(v: &SysValue) -> String {
    match v {
        SysValue::Null => "null".to_string(),
        SysValue::Bool(b) => b.to_string(),
        SysValue::Int(n) => n.to_string(),
        SysValue::Float(n) => n.to_string(),
        SysValue::Str(s) => s.clone(),
        SysValue::Bytes(b) => format!("#bytes({})", b.len()),
        SysValue::List(items) => {
            let inner: Vec<String> = items.iter().map(val_to_display_string).collect();
            format!("[{}]", inner.join(", "))
        }
        SysValue::Map(m) => {
            let mut pairs: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{k}: {}", val_to_display_string(v)))
                .collect();
            pairs.sort();
            format!("{{{}}}", pairs.join(", "))
        }
    }
}
