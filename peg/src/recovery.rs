//! Batch error-recovery parsing (the `recovery` feature): split input at sync
//! points, parse each segment, and collect [`RecoveredParse`] results plus the
//! errors, instead of failing on the first error.

use std::collections::BTreeMap;

use regex::Regex;

use crate::diagnostics_utils::{compute_line_offsets, line_col_u32};
use crate::error::ParseError;
use crate::grammar::Grammar;
use crate::types::ParseValue;

// ── Public types ───────────────────────────────────────────────────────────

/// A text that could be inserted to recover from a parse error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryInsertCandidate {
    /// The candidate insertion text.
    pub text: String,
    /// A human-readable label for the candidate.
    pub label: String,
}

/// A token span that could be deleted to recover from a parse error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryDeleteCandidate {
    /// Inclusive start byte offset of the deletable span.
    pub start: usize,
    /// Exclusive end byte offset of the deletable span.
    pub end: usize,
    /// The text covered by the span.
    pub text: String,
}

/// Configuration for the batch recovery algorithm.
#[derive(Clone, Debug)]
pub struct RecoveryConfig {
    /// Literal strings that act as synchronization points.
    pub sync_tokens: Vec<String>,
    /// Optional regex pattern whose matches act as synchronization points.
    pub sync_regex: Option<String>,
    /// Maximum number of errors before recovery aborts.
    pub max_errors: usize,
    /// Whether to attempt local (single-token) insert/delete recovery.
    pub local_tolerance: bool,
    /// Maximum text length of an insertable candidate.
    pub local_insert_max_length: usize,
    /// Maximum number of parse attempts performed by one recovery run.
    pub max_recovery_attempts: usize,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            sync_tokens: Vec::new(),
            sync_regex: None,
            max_errors: 5,
            local_tolerance: true,
            local_insert_max_length: DEFAULT_LOCAL_INSERT_MAX_LENGTH,
            max_recovery_attempts: DEFAULT_MAX_RECOVERY_ATTEMPTS,
        }
    }
}

/// The result of a batch recovery parse: collected values and errors.
pub type RecoveredParse = (Vec<ParseValue>, Vec<ParseError>);

const DEFAULT_LOCAL_INSERT_MAX_LENGTH: usize = 8;
const DEFAULT_LOCAL_INSERT_MAX_CANDIDATES: usize = 4;
const DEFAULT_MAX_RECOVERY_ATTEMPTS: usize = 1024;

// ── Normalisation helpers ──────────────────────────────────────────────────

/// Validate and normalise a list of sync tokens.
pub fn normalize_sync_tokens(tokens: &[String]) -> Result<Vec<String>, ParseError> {
    for t in tokens {
        if t.is_empty() {
            return Err(recovery_config_error(
                "sync_tokens must contain only non-empty strings",
            ));
        }
    }
    Ok(tokens.to_vec())
}

/// Validate a sync regex string.
pub fn normalize_sync_regex(regex: &str) -> Result<String, ParseError> {
    if regex.trim().is_empty() {
        return Err(recovery_config_error(
            "sync_regex must be a non-empty string",
        ));
    }
    Regex::new(regex).map_err(|e| recovery_config_error(format!("invalid sync_regex: {e}")))?;
    Ok(regex.to_string())
}

/// Validate recovery sync configuration and return normalized values.
pub fn validate_recovery_config(
    config: &RecoveryConfig,
) -> Result<(Vec<String>, Option<String>), ParseError> {
    let sync_tokens = normalize_sync_tokens(&config.sync_tokens)?;
    let sync_regex = match config.sync_regex.as_deref() {
        Some(pattern) => Some(normalize_sync_regex(pattern)?),
        None => None,
    };

    if sync_tokens.is_empty() && sync_regex.is_none() {
        return Err(recovery_config_error(
            "Recovery requires explicit sync_tokens or sync_regex",
        ));
    }
    if config.max_errors == 0 {
        return Err(recovery_config_error(
            "max_errors must be greater than zero",
        ));
    }
    if config.max_recovery_attempts == 0 {
        return Err(recovery_config_error(
            "max_recovery_attempts must be greater than zero",
        ));
    }

    Ok((sync_tokens, sync_regex))
}

fn recovery_config_error(message: impl Into<String>) -> ParseError {
    ParseError::new(message, 0, 0)
}

// ── Sync-marker collection ─────────────────────────────────────────────────

/// Find all synchronisation points in `text`.
///
/// Returns `(start, end)` pairs sorted by start position, where `end` is the
/// position just after the sync token/regex match.
pub fn collect_sync_markers(
    text: &str,
    sync_tokens: &[String],
    sync_regex: Option<&str>,
) -> Vec<(usize, usize)> {
    // Use BTreeMap so we merge overlapping markers and sort automatically.
    let mut markers: BTreeMap<usize, usize> = BTreeMap::new();

    for token in sync_tokens {
        let token_bytes = token.as_bytes();
        let text_bytes = text.as_bytes();
        let mut start = 0usize;
        while start + token_bytes.len() <= text_bytes.len() {
            if text_bytes[start..].starts_with(token_bytes) {
                let end = start + token_bytes.len();
                let entry = markers.entry(start).or_insert(end);
                if end > *entry {
                    *entry = end;
                }
                start += 1;
            } else {
                start += 1;
            }
        }
    }

    if let Some(pattern) = sync_regex {
        if let Ok(re) = Regex::new(pattern) {
            for m in re.find_iter(text) {
                let idx = m.start();
                let end = m.end().max(idx + 1);
                let entry = markers.entry(idx).or_insert(end);
                if end > *entry {
                    *entry = end;
                }
            }
        }
    }

    markers.into_iter().collect()
}

// ── Insert/delete candidate collection ────────────────────────────────────

/// Build a ranked list of text candidates that might be inserted to recover.
///
/// Candidates are derived from `expected` token labels (same format as
/// `ParseError::expected`).
pub fn collect_insert_candidates(
    expected: &[String],
    max_length: usize,
    max_candidates: usize,
) -> Vec<RecoveryInsertCandidate> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut candidates: Vec<RecoveryInsertCandidate> = Vec::new();
    for label in expected {
        if let Some(text) = insertable_text_from_label(label) {
            if text.len() <= max_length && seen.insert(text.clone()) {
                candidates.push(RecoveryInsertCandidate {
                    text,
                    label: label.clone(),
                });
            }
        }
    }
    // Prefer shorter, then lexicographic
    candidates.sort_by(|a, b| {
        a.text
            .len()
            .cmp(&b.text.len())
            .then(a.text.cmp(&b.text))
            .then(a.label.cmp(&b.label))
    });
    candidates.truncate(max_candidates);
    candidates
}

/// Find a word/token at `pos` in `text` that could be deleted to recover.
pub fn collect_delete_candidate(text: &str, pos: usize) -> Option<RecoveryDeleteCandidate> {
    if pos >= text.len() {
        return None;
    }
    let (start, end) = token_bounds(text, pos);
    if start >= end {
        return None;
    }
    Some(RecoveryDeleteCandidate {
        start,
        end,
        text: text[start..end].to_string(),
    })
}

// ── Batch recovery ────────────────────────────────────────────────────────

/// Batch error-recovery parse using sync markers.
///
/// Splits `text` at sync points and attempts to parse each segment.  On
/// failure, tries local single-token insert/delete recovery before advancing
/// past the error.
pub fn recover_parse<F>(
    text: &str,
    grammar: &Grammar,
    parse_segment: F,
    config: &RecoveryConfig,
) -> RecoveredParse
where
    F: Fn(&str, &Grammar) -> Result<ParseValue, ParseError>,
{
    match try_recover_parse(text, grammar, parse_segment, config) {
        Ok(recovered) => recovered,
        Err(error) => (Vec::new(), vec![error]),
    }
}

/// Fallible batch recovery parse.
///
/// This is the strict API equivalent to the strict recovery parser: callers must
/// provide explicit `sync_tokens` or `sync_regex`, and invalid recovery
/// configuration is reported as an error instead of falling back silently.
pub fn try_recover_parse<F>(
    text: &str,
    grammar: &Grammar,
    parse_segment: F,
    config: &RecoveryConfig,
) -> Result<RecoveredParse, ParseError>
where
    F: Fn(&str, &Grammar) -> Result<ParseValue, ParseError>,
{
    let (sync_tokens, sync_regex) = validate_recovery_config(config)?;
    Ok(recover_parse_impl(
        text,
        grammar,
        parse_segment,
        config,
        &sync_tokens,
        sync_regex.as_deref(),
    ))
}

fn recover_parse_impl<F>(
    text: &str,
    grammar: &Grammar,
    parse_segment: F,
    config: &RecoveryConfig,
    sync_tokens: &[String],
    sync_regex: Option<&str>,
) -> RecoveredParse
where
    F: Fn(&str, &Grammar) -> Result<ParseValue, ParseError>,
{
    let mut results: Vec<ParseValue> = Vec::new();
    let mut errors: Vec<ParseError> = Vec::new();

    let markers = collect_sync_markers(text, sync_tokens, sync_regex);
    let marker_starts: Vec<usize> = markers.iter().map(|(s, _)| *s).collect();
    let line_offsets = compute_line_offsets(text);
    let mut budget = RecoveryBudget::new(config.max_recovery_attempts);
    let mut cursor = 0usize;

    while cursor < text.len() {
        let (segment_end, cursor_after) =
            segment_bounds(cursor, &markers, &marker_starts, text.len());

        if segment_end <= cursor {
            cursor = cursor_after;
            continue;
        }

        let chunk = &text[cursor..segment_end];

        if let Err(error) = budget.consume(cursor, &line_offsets) {
            errors.push(error);
            break;
        }

        match parse_segment(chunk, grammar) {
            Ok(v) => {
                results.push(v);
                cursor = cursor_after;
                continue;
            }
            Err(error) => {
                let recovered = if config.local_tolerance {
                    let mut local_context = LocalRecoveryContext {
                        chunk,
                        grammar,
                        parse_segment: &parse_segment,
                        budget: &mut budget,
                        absolute_offset: cursor,
                        line_offsets: &line_offsets,
                    };
                    recover_segment_locally(
                        &error,
                        config.local_insert_max_length,
                        &mut local_context,
                    )
                } else {
                    Ok(None)
                };

                let recovered = match recovered {
                    Ok(recovered) => recovered,
                    Err(error) => {
                        errors.push(error);
                        break;
                    }
                };

                if let Some((value, local_msg)) = recovered {
                    let abs_pos = cursor.saturating_add(error.span.start).min(text.len());
                    results.push(value);
                    let (line, col) = line_col_u32(&line_offsets, abs_pos);
                    errors.push(
                        ParseError::with_context(
                            local_msg,
                            abs_pos,
                            abs_pos,
                            error.expected.to_vec(),
                            error.found.as_deref().map(str::to_string),
                        )
                        .at_absolute_pos(abs_pos, abs_pos)
                        .with_location(line, col),
                    );
                    if errors.len() >= config.max_errors {
                        break;
                    }
                    cursor = cursor_after;
                    continue;
                }

                let abs_pos = cursor.saturating_add(error.span.start).min(text.len());
                let (line, col) = line_col_u32(&line_offsets, abs_pos);
                errors.push(
                    error
                        .at_absolute_pos(abs_pos, abs_pos)
                        .with_location(line, col),
                );
                if errors.len() >= config.max_errors {
                    break;
                }

                match cursor_after_error(abs_pos, &markers, &marker_starts) {
                    Some(next) => cursor = next,
                    None => break,
                }
            }
        }
    }

    (results, errors)
}

// ── DefaultRecoveryStrategy ────────────────────────────────────────────────

/// General-purpose recovery strategy.
/// General-purpose recovery strategy.
pub struct DefaultRecoveryStrategy {
    /// Maximum errors to recover before giving up.
    pub max_errors: usize,
    /// Whether to attempt local insert/delete recovery.
    pub local_tolerance: bool,
    /// Cap on locally-inserted recovery text length.
    pub local_insert_max_length: usize,
    /// Cap on recovery attempts per error site.
    pub max_recovery_attempts: usize,
}

impl Default for DefaultRecoveryStrategy {
    fn default() -> Self {
        Self {
            max_errors: 5,
            local_tolerance: true,
            local_insert_max_length: DEFAULT_LOCAL_INSERT_MAX_LENGTH,
            max_recovery_attempts: DEFAULT_MAX_RECOVERY_ATTEMPTS,
        }
    }
}

impl DefaultRecoveryStrategy {
    /// Recover-parse `text`, splitting at the given sync tokens/regex.
    pub fn recover<F>(
        &self,
        text: &str,
        grammar: &Grammar,
        parse_segment: F,
        sync_tokens: &[String],
        sync_regex: Option<&str>,
    ) -> RecoveredParse
    where
        F: Fn(&str, &Grammar) -> Result<ParseValue, ParseError>,
    {
        let config = RecoveryConfig {
            sync_tokens: sync_tokens.to_vec(),
            sync_regex: sync_regex.map(str::to_string),
            max_errors: self.max_errors,
            local_tolerance: self.local_tolerance,
            local_insert_max_length: self.local_insert_max_length,
            max_recovery_attempts: self.max_recovery_attempts,
        };
        recover_parse(text, grammar, parse_segment, &config)
    }
}

/// Recovery strategy for streaming/top-level forms (uses `)` as default sync).
/// Recovery strategy for streaming/top-level forms (uses `)` as default sync).
pub struct StreamingFormRecoveryStrategy {
    /// Maximum errors to recover before giving up.
    pub max_errors: usize,
    /// Whether to attempt local insert/delete recovery.
    pub local_tolerance: bool,
    /// Cap on locally-inserted recovery text length.
    pub local_insert_max_length: usize,
    /// Cap on recovery attempts per error site.
    pub max_recovery_attempts: usize,
}

impl Default for StreamingFormRecoveryStrategy {
    fn default() -> Self {
        Self {
            max_errors: 20,
            local_tolerance: true,
            local_insert_max_length: DEFAULT_LOCAL_INSERT_MAX_LENGTH,
            max_recovery_attempts: DEFAULT_MAX_RECOVERY_ATTEMPTS,
        }
    }
}

impl StreamingFormRecoveryStrategy {
    /// Recover-parse `text` for streaming forms, defaulting sync to `)`.
    pub fn recover<F>(
        &self,
        text: &str,
        grammar: &Grammar,
        parse_segment: F,
        sync_tokens: Option<&[String]>,
        sync_regex: Option<&str>,
    ) -> RecoveredParse
    where
        F: Fn(&str, &Grammar) -> Result<ParseValue, ParseError>,
    {
        let default_paren = vec![")".to_string()];
        let effective_tokens: &[String] = match sync_tokens {
            Some(t) if !t.is_empty() || sync_regex.is_some() => t,
            _ => &default_paren,
        };
        let config = RecoveryConfig {
            sync_tokens: effective_tokens.to_vec(),
            sync_regex: sync_regex.map(str::to_string),
            max_errors: self.max_errors,
            local_tolerance: self.local_tolerance,
            local_insert_max_length: self.local_insert_max_length,
            max_recovery_attempts: self.max_recovery_attempts,
        };
        recover_parse(text, grammar, parse_segment, &config)
    }
}

// ── Internal helpers ───────────────────────────────────────────────────────

struct LocalRecoveryContext<'a, F> {
    chunk: &'a str,
    grammar: &'a Grammar,
    parse_segment: &'a F,
    budget: &'a mut RecoveryBudget,
    absolute_offset: usize,
    line_offsets: &'a [usize],
}

fn recover_segment_locally<F>(
    error: &ParseError,
    insert_max_length: usize,
    context: &mut LocalRecoveryContext<'_, F>,
) -> Result<Option<(ParseValue, String)>, ParseError>
where
    F: Fn(&str, &Grammar) -> Result<ParseValue, ParseError>,
{
    let rel_pos = error.span.start.min(context.chunk.len());

    let insert_candidates = collect_insert_candidates(
        &error.expected,
        insert_max_length.max(1),
        DEFAULT_LOCAL_INSERT_MAX_CANDIDATES,
    );

    if let Some(result) = try_insertions(context, rel_pos, &insert_candidates)? {
        return Ok(Some(result));
    }

    let Some(deletion) = collect_delete_candidate(context.chunk, rel_pos) else {
        return Ok(None);
    };
    try_deletion(context, &deletion)
}

fn try_insertions<F>(
    context: &mut LocalRecoveryContext<'_, F>,
    pos: usize,
    candidates: &[RecoveryInsertCandidate],
) -> Result<Option<(ParseValue, String)>, ParseError>
where
    F: Fn(&str, &Grammar) -> Result<ParseValue, ParseError>,
{
    for candidate in candidates {
        context
            .budget
            .consume(context.absolute_offset + pos, context.line_offsets)?;
        let corrected = format!(
            "{}{}{}",
            &context.chunk[..pos],
            candidate.text,
            &context.chunk[pos..]
        );
        if let Ok(value) = (context.parse_segment)(&corrected, context.grammar) {
            let msg = format!("Recovered by inserting missing {}", candidate.label);
            return Ok(Some((value, msg)));
        }
    }
    Ok(None)
}

fn try_deletion<F>(
    context: &mut LocalRecoveryContext<'_, F>,
    candidate: &RecoveryDeleteCandidate,
) -> Result<Option<(ParseValue, String)>, ParseError>
where
    F: Fn(&str, &Grammar) -> Result<ParseValue, ParseError>,
{
    context.budget.consume(
        context.absolute_offset + candidate.start,
        context.line_offsets,
    )?;
    let corrected = format!(
        "{}{}",
        &context.chunk[..candidate.start],
        &context.chunk[candidate.end..]
    );
    if let Ok(value) = (context.parse_segment)(&corrected, context.grammar) {
        let msg = format!(
            "Recovered by deleting unexpected token {:?}",
            candidate.text
        );
        return Ok(Some((value, msg)));
    }
    Ok(None)
}

struct RecoveryBudget {
    used: usize,
    max: usize,
}

impl RecoveryBudget {
    fn new(max: usize) -> Self {
        Self { used: 0, max }
    }

    fn consume(&mut self, abs_pos: usize, line_offsets: &[usize]) -> Result<(), ParseError> {
        if self.used >= self.max {
            let (line, col) = line_col_u32(line_offsets, abs_pos);
            return Err(ParseError::new(
                format!("Recovery aborted after {} parse attempts", self.max),
                abs_pos,
                abs_pos,
            )
            .with_code("recovery_budget_exhausted")
            .with_location(line, col));
        }
        self.used += 1;
        Ok(())
    }
}

/// `(segment_end, cursor_after_success)` — the next segment to parse and
/// where the cursor moves after a successful parse.
fn segment_bounds(
    cursor: usize,
    markers: &[(usize, usize)],
    marker_starts: &[usize],
    text_len: usize,
) -> (usize, usize) {
    let idx = marker_starts.partition_point(|&s| s < cursor);
    match markers.get(idx) {
        None => (text_len, text_len),
        Some(&(start, end)) => (start, end),
    }
}

fn cursor_after_error(
    abs_pos: usize,
    markers: &[(usize, usize)],
    marker_starts: &[usize],
) -> Option<usize> {
    let next_idx = marker_starts.partition_point(|&s| s <= abs_pos);
    markers.get(next_idx).map(|&(_, end)| end)
}

fn insertable_text_from_label(label: &str) -> Option<String> {
    if let Some(payload) = label.strip_prefix("literal ") {
        return unquote_literal(payload);
    }
    if let Some(text) = label.strip_prefix("soft keyword ") {
        return if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        };
    }
    if let Some(payload) = label.strip_prefix("token ") {
        return payload.split_once('=').and_then(|(_, text)| {
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        });
    }
    None
}

/// Simple unquoter for single-quoted PEG literals: `'hello'` → `hello`.
fn unquote_literal(payload: &str) -> Option<String> {
    let s = payload.trim();
    if s.len() < 2 {
        return None;
    }
    // Single-quoted: 'text'
    if s.starts_with('\'') && s.ends_with('\'') {
        let inner = &s[1..s.len() - 1];
        return Some(inner.replace("\\'", "'").replace("\\\\", "\\"));
    }
    // Double-quoted: "text"
    if s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        return Some(inner.replace("\\\"", "\"").replace("\\\\", "\\"));
    }
    None
}

fn token_bounds(text: &str, pos: usize) -> (usize, usize) {
    let ch = text[pos..].chars().next().unwrap_or('\0');
    if ch.is_alphanumeric() || ch == '_' {
        // Walk start backwards over word chars
        let mut start = pos;
        for (byte_pos, c) in text[..pos].char_indices().rev() {
            if c.is_alphanumeric() || c == '_' {
                start = byte_pos;
            } else {
                break;
            }
        }
        // Walk end forwards over word chars
        let mut end = pos;
        for (byte_pos, c) in text[pos..].char_indices() {
            if c.is_alphanumeric() || c == '_' {
                end = pos + byte_pos + c.len_utf8();
            } else {
                break;
            }
        }
        (start, end)
    } else {
        (pos, pos + ch.len_utf8().max(1))
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── normalize helpers ────────────────────────────────────────────────

    #[test]
    fn normalize_sync_tokens_rejects_empty_string() {
        let tokens = vec!["ok".to_string(), "".to_string()];
        assert!(normalize_sync_tokens(&tokens).is_err());
    }

    #[test]
    fn normalize_sync_tokens_accepts_valid() {
        let tokens = vec![";".to_string(), ")".to_string()];
        assert!(normalize_sync_tokens(&tokens).is_ok());
    }

    #[test]
    fn normalize_sync_regex_rejects_blank() {
        assert!(normalize_sync_regex("   ").is_err());
    }

    #[test]
    fn normalize_sync_regex_accepts_valid() {
        assert!(normalize_sync_regex(r"\n").is_ok());
    }

    #[test]
    fn validate_recovery_config_requires_explicit_sync() {
        let err = validate_recovery_config(&RecoveryConfig::default())
            .expect_err("missing sync config should fail");
        assert!(err.message.contains("sync_tokens or sync_regex"));
    }

    #[test]
    fn validate_recovery_config_rejects_zero_max_errors() {
        let config = RecoveryConfig {
            sync_tokens: vec![";".to_string()],
            max_errors: 0,
            ..Default::default()
        };
        let err = validate_recovery_config(&config).expect_err("zero max_errors should fail");
        assert!(err.message.contains("max_errors"));
    }

    #[test]
    fn validate_recovery_config_rejects_zero_recovery_attempts() {
        let config = RecoveryConfig {
            sync_tokens: vec![";".to_string()],
            max_recovery_attempts: 0,
            ..Default::default()
        };
        let err = validate_recovery_config(&config).expect_err("zero recovery budget should fail");
        assert!(err.message.contains("max_recovery_attempts"));
    }

    // ── collect_sync_markers ─────────────────────────────────────────────

    #[test]
    fn collect_sync_markers_finds_literal_tokens() {
        let text = "a;b;c";
        let markers = collect_sync_markers(text, &[";".to_string()], None);
        assert_eq!(markers, vec![(1, 2), (3, 4)]);
    }

    #[test]
    fn collect_sync_markers_finds_regex_matches() {
        let text = "abc\ndef\nghi";
        let markers = collect_sync_markers(text, &[], Some(r"\n"));
        // newlines at positions 3 and 7
        assert!(markers.iter().any(|&(s, _)| s == 3));
        assert!(markers.iter().any(|&(s, _)| s == 7));
    }

    #[test]
    fn collect_sync_markers_empty_when_no_match() {
        let markers = collect_sync_markers("hello", &["xyz".to_string()], None);
        assert!(markers.is_empty());
    }

    // ── collect_insert_candidates ────────────────────────────────────────

    #[test]
    fn insert_candidates_from_literal_labels() {
        let expected = vec!["literal 'fn'".to_string(), "literal 'class'".to_string()];
        let cands = collect_insert_candidates(&expected, 10, 4);
        let texts: Vec<&str> = cands.iter().map(|c| c.text.as_str()).collect();
        assert!(texts.contains(&"fn"));
        assert!(texts.contains(&"class"));
    }

    #[test]
    fn insert_candidates_filtered_by_max_length() {
        let expected = vec!["literal 'toolong'".to_string()];
        let cands = collect_insert_candidates(&expected, 4, 4);
        assert!(cands.is_empty());
    }

    #[test]
    fn insert_candidates_deduplicates() {
        let expected = vec!["literal 'fn'".to_string(), "literal 'fn'".to_string()];
        let cands = collect_insert_candidates(&expected, 10, 4);
        assert_eq!(cands.len(), 1);
    }

    #[test]
    fn insert_candidates_sorted_by_length() {
        let expected = vec!["literal 'class'".to_string(), "literal 'fn'".to_string()];
        let cands = collect_insert_candidates(&expected, 10, 4);
        assert_eq!(cands[0].text, "fn");
        assert_eq!(cands[1].text, "class");
    }

    // ── collect_delete_candidate ─────────────────────────────────────────

    #[test]
    fn delete_candidate_for_word() {
        let cand = collect_delete_candidate("hello world", 6).unwrap();
        assert_eq!(cand.text, "world");
        assert_eq!(cand.start, 6);
        assert_eq!(cand.end, 11);
    }

    #[test]
    fn delete_candidate_for_operator() {
        let cand = collect_delete_candidate("a + b", 2).unwrap();
        assert_eq!(cand.text, "+");
    }

    #[test]
    fn delete_candidate_out_of_bounds() {
        assert!(collect_delete_candidate("abc", 10).is_none());
    }

    // ── segment_bounds ───────────────────────────────────────────────────

    #[test]
    fn segment_bounds_picks_first_marker_after_cursor() {
        let markers = vec![(5, 6), (10, 11)];
        let starts: Vec<usize> = markers.iter().map(|(s, _)| *s).collect();
        // cursor at 0 → segment [0, 5), cursor moves to 6
        let (seg_end, cursor_after) = segment_bounds(0, &markers, &starts, 20);
        assert_eq!(seg_end, 5);
        assert_eq!(cursor_after, 6);
    }

    #[test]
    fn segment_bounds_at_end_returns_text_len() {
        let markers: Vec<(usize, usize)> = vec![];
        let starts: Vec<usize> = vec![];
        let (seg_end, cursor_after) = segment_bounds(0, &markers, &starts, 10);
        assert_eq!(seg_end, 10);
        assert_eq!(cursor_after, 10);
    }

    // ── insertable_text_from_label ───────────────────────────────────────

    #[test]
    fn insertable_text_literal_single_quoted() {
        assert_eq!(
            insertable_text_from_label("literal 'hello'"),
            Some("hello".to_string())
        );
    }

    #[test]
    fn insertable_text_soft_keyword() {
        assert_eq!(
            insertable_text_from_label("soft keyword async"),
            Some("async".to_string())
        );
    }

    #[test]
    fn insertable_text_token_with_value() {
        assert_eq!(
            insertable_text_from_label("token SEMI=;"),
            Some(";".to_string())
        );
    }

    #[test]
    fn insertable_text_unknown_label() {
        assert_eq!(insertable_text_from_label("regex [a-z]+"), None);
    }

    // ── recover_parse ────────────────────────────────────────────────────

    fn ok_grammar() -> Grammar {
        Grammar::trusted_new("word <- /[a-z]+/").with_start_rule("word")
    }

    fn always_ok(_text: &str, _grammar: &Grammar) -> Result<ParseValue, ParseError> {
        Ok(ParseValue::Text("ok".into()))
    }

    fn always_err(_text: &str, _grammar: &Grammar) -> Result<ParseValue, ParseError> {
        Err(ParseError::new("fail", 0, 0))
    }

    #[test]
    fn recover_parse_no_errors_collects_all_segments() {
        let config = RecoveryConfig {
            sync_tokens: vec![";".to_string()],
            ..Default::default()
        };
        let (values, errors) = recover_parse("a;b;c", &ok_grammar(), always_ok, &config);
        assert_eq!(values.len(), 3);
        assert!(errors.is_empty());
    }

    #[test]
    fn try_recover_parse_requires_sync_configuration() {
        let err = try_recover_parse("abc", &ok_grammar(), always_ok, &RecoveryConfig::default())
            .expect_err("missing sync config should fail");
        assert!(err.message.contains("sync_tokens or sync_regex"));
    }

    #[test]
    fn recover_parse_reports_config_errors_in_error_list() {
        let (values, errors) =
            recover_parse("abc", &ok_grammar(), always_ok, &RecoveryConfig::default());
        assert!(values.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("sync_tokens or sync_regex"));
    }

    #[test]
    fn recover_parse_collects_errors_and_advances() {
        let config = RecoveryConfig {
            sync_tokens: vec![";".to_string()],
            local_tolerance: false,
            max_errors: 10,
            ..Default::default()
        };
        let (values, errors) = recover_parse("a;b;c", &ok_grammar(), always_err, &config);
        // All 3 segments fail, 3 errors collected
        assert_eq!(errors.len(), 3);
        assert!(values.is_empty());
    }

    #[test]
    fn recover_parse_adds_absolute_line_col_to_errors() {
        fn err_at_b(text: &str, _grammar: &Grammar) -> Result<ParseValue, ParseError> {
            Err(ParseError::new("fail", text.find('b').unwrap_or(0), 0))
        }

        let config = RecoveryConfig {
            sync_tokens: vec![";".to_string()],
            local_tolerance: false,
            ..Default::default()
        };
        let (_values, errors) = recover_parse("ok;\nbad", &ok_grammar(), err_at_b, &config);
        assert_eq!(errors[1].line, Some(2));
        assert_eq!(errors[1].col, Some(1));
    }

    #[test]
    fn recover_parse_stops_at_max_errors() {
        let config = RecoveryConfig {
            sync_tokens: vec![";".to_string()],
            local_tolerance: false,
            max_errors: 2,
            ..Default::default()
        };
        let (_values, errors) = recover_parse("a;b;c", &ok_grammar(), always_err, &config);
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn recover_parse_stops_at_recovery_attempt_budget() {
        let config = RecoveryConfig {
            sync_tokens: vec![";".to_string()],
            local_tolerance: false,
            max_errors: 10,
            max_recovery_attempts: 2,
            ..Default::default()
        };
        let (_values, errors) = recover_parse("a;b;c", &ok_grammar(), always_err, &config);

        assert_eq!(errors.len(), 3);
        assert_eq!(
            errors.last().and_then(|error| error.code.as_deref()),
            Some("recovery_budget_exhausted")
        );
    }

    #[test]
    fn default_recovery_strategy_uses_defaults() {
        let strategy = DefaultRecoveryStrategy::default();
        let (values, errors) =
            strategy.recover("a;b", &ok_grammar(), always_ok, &[";".to_string()], None);
        assert_eq!(values.len(), 2);
        assert!(errors.is_empty());
    }

    #[test]
    fn streaming_form_strategy_defaults_to_paren_sync() {
        let strategy = StreamingFormRecoveryStrategy::default();
        let (values, errors) = strategy.recover("a)b", &ok_grammar(), always_ok, None, None);
        assert_eq!(values.len(), 2);
        assert!(errors.is_empty());
    }
}
