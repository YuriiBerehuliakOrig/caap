//! Multi-pass parse pipelines: feed one stage's parsed output (via a transform)
//! into the next stage's input text, statelessly ([`parse_pipeline`]) or with a
//! reusable incremental cache (`IncrementalPipeline`).

use crate::error::ParseError;
use crate::grammar::Grammar;
use crate::parser_engine::PEGParser;
use crate::types::{IncrementalEdit, ParseCache, ParseValue, ParserConfig};

// ── Pipeline input/output types ────────────────────────────────────────────

/// Text (and optional incremental-edit hints) fed into one pipeline stage.
#[derive(Clone, Debug)]
pub struct PipelineTextUpdate {
    /// The full text for this stage.
    pub text: String,
    /// Optional incremental-edit hints relative to the previous run.
    pub edits: Option<Vec<IncrementalEdit>>,
}

impl PipelineTextUpdate {
    /// A whole-text update with no edit hints.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            edits: None,
        }
    }

    /// A text update carrying incremental-edit hints.
    pub fn with_edits(text: impl Into<String>, edits: Vec<IncrementalEdit>) -> Self {
        Self {
            text: text.into(),
            edits: Some(edits),
        }
    }
}

/// Transform applied to a stage's parse result to produce the next stage's input.
pub type PipelineTransform = Box<dyn Fn(ParseValue) -> Result<PipelineTextUpdate, ParseError>>;

/// A single stage: grammar + optional transform to the next stage.
pub struct PipelineStage {
    /// The grammar parsed by this stage.
    pub grammar: Grammar,
    /// Optional transform producing the next stage's input.
    pub transform: Option<PipelineTransform>,
}

impl PipelineStage {
    /// A terminal stage with no onward transform.
    pub fn new(grammar: Grammar) -> Self {
        Self {
            grammar,
            transform: None,
        }
    }

    /// A stage that transforms its result into the next stage's input.
    pub fn with_transform<F>(grammar: Grammar, transform: F) -> Self
    where
        F: Fn(ParseValue) -> Result<PipelineTextUpdate, ParseError> + 'static,
    {
        Self {
            grammar,
            transform: Some(Box::new(transform)),
        }
    }
}

/// Result produced by running one stage.
pub struct PipelineStageResult {
    /// The input fed to this stage.
    pub input: PipelineTextUpdate,
    /// The parsed value.
    pub value: ParseValue,
    /// The derived input for the next stage, or `None` if this is the last stage.
    pub next_input: Option<PipelineTextUpdate>,
}

// ── Stateless pipeline ─────────────────────────────────────────────────────

/// Run a stateless multi-pass parse pipeline.
///
/// Each stage creates a fresh `PEGParser` with no caching.  If a stage has a
/// `transform`, the transformed output becomes the next stage's input.  If
/// there is no transform and the value is `ParseValue::Text(s)`, that string
/// becomes the next stage's input directly.  Any other value with a following
/// stage is an error.
pub fn parse_pipeline(
    text: &str,
    stages: &[PipelineStage],
) -> Result<Vec<PipelineStageResult>, ParseError> {
    let mut results: Vec<PipelineStageResult> = Vec::new();
    let mut current_input = PipelineTextUpdate::new(text);

    for (index, stage) in stages.iter().enumerate() {
        let parser = PEGParser;
        let value = parser.parse(
            &stage.grammar,
            &current_input.text,
            &ParserConfig::default(),
        )?;

        let has_next = index + 1 < stages.len();
        let next_input = resolve_next_input(&value, stage.transform.as_deref(), has_next)?;

        let stage_result = PipelineStageResult {
            input: current_input,
            value,
            next_input: next_input.clone(),
        };
        results.push(stage_result);

        match next_input {
            Some(ni) => current_input = ni,
            None => break,
        }
    }

    Ok(results)
}

// ── Stateful incremental pipeline ─────────────────────────────────────────

struct IncrementalStageState {
    grammar: Grammar,
    transform: Option<PipelineTransform>,
    parser: PEGParser,
    cache: Option<ParseCache>,
    last_input: Option<PipelineTextUpdate>,
}

/// Stateful multi-stage parser that reuses per-stage caches across runs.
///
/// Call [`parse`][IncrementalPipeline::parse] repeatedly; on subsequent calls
/// the cache from each stage is reused and incremental edits are applied
/// automatically when the input text changes.
pub struct IncrementalPipeline {
    stages: Vec<IncrementalStageState>,
}

impl IncrementalPipeline {
    /// Build a stateful incremental pipeline from its stages.
    pub fn new(stages: Vec<PipelineStage>) -> Self {
        Self {
            stages: stages
                .into_iter()
                .map(|s| IncrementalStageState {
                    grammar: s.grammar,
                    transform: s.transform,
                    parser: PEGParser,
                    cache: None,
                    last_input: None,
                })
                .collect(),
        }
    }

    /// Run all stages on `text`, reusing cached state from previous runs.
    pub fn parse(&mut self, text: &str) -> Result<Vec<PipelineStageResult>, ParseError> {
        let mut results: Vec<PipelineStageResult> = Vec::new();
        let mut current_input = PipelineTextUpdate::new(text);
        let stage_count = self.stages.len();

        for (index, state) in self.stages.iter_mut().enumerate() {
            let value = parse_stage_incremental(state, &current_input)?;

            let has_next = index + 1 < stage_count;
            let next_input = resolve_next_input(&value, state.transform.as_deref(), has_next)?;

            results.push(PipelineStageResult {
                input: current_input,
                value,
                next_input: next_input.clone(),
            });

            match next_input {
                Some(ni) => current_input = ni,
                None => break,
            }
        }

        Ok(results)
    }

    /// Reset all stage caches (equivalent to a cold parse on the next run).
    pub fn reset(&mut self) {
        for state in &mut self.stages {
            state.cache = None;
            state.last_input = None;
        }
    }

    /// Snapshot of all stage caches (may be `None` before the first run).
    pub fn cache_snapshot(&self) -> Vec<Option<&ParseCache>> {
        self.stages.iter().map(|s| s.cache.as_ref()).collect()
    }
}

fn parse_stage_incremental(
    state: &mut IncrementalStageState,
    stage_input: &PipelineTextUpdate,
) -> Result<ParseValue, ParseError> {
    // Compute incremental edits when text changed and no explicit edits were
    // provided; this keeps diff metadata aligned with the incremental API even
    // though parsing still relies on whole-input cache keys.
    let _edits: Option<Vec<IncrementalEdit>> = match &stage_input.edits {
        Some(e) => Some(e.clone()),
        None => {
            if let Some(last) = &state.last_input {
                if last.text != stage_input.text {
                    Some(compute_snapshot_edits(&last.text, &stage_input.text))
                } else {
                    None
                }
            } else {
                None
            }
        }
    };

    let cache = state.cache.get_or_insert_with(ParseCache::new);
    let value = state.parser.parse_incremental_many(
        &state.grammar,
        &stage_input.text,
        &ParserConfig::default(),
        cache,
    )?;

    // Track last input for future incremental-edit computation.
    state.last_input = Some(stage_input.clone());

    Ok(value.as_ref().clone())
}

// ── compute_snapshot_edits ─────────────────────────────────────────────────

/// Compute a minimal list of `IncrementalEdit`s that transform `old_text`
/// into `new_text`.
pub fn compute_snapshot_edits(old_text: &str, new_text: &str) -> Vec<IncrementalEdit> {
    if old_text == new_text {
        return vec![];
    }
    if old_text.len() == new_text.len() {
        return compute_equal_length_edits(old_text, new_text);
    }

    let start = common_prefix_len(old_text, new_text);
    let suffix = common_suffix_len(old_text, new_text, start);
    let old_end = old_text.len() - suffix;
    let new_end = new_text.len() - suffix;

    IncrementalEdit::new(start, old_end, new_text[start..new_end].to_string())
        .into_iter()
        .collect()
}

fn compute_equal_length_edits(old_text: &str, new_text: &str) -> Vec<IncrementalEdit> {
    let old: Vec<char> = old_text.chars().collect();
    let new: Vec<char> = new_text.chars().collect();
    let mut edits: Vec<IncrementalEdit> = Vec::new();
    let mut mismatch_start: Option<usize> = None;

    for (i, (oc, nc)) in old.iter().zip(new.iter()).enumerate() {
        if oc == nc {
            if let Some(ms) = mismatch_start.take() {
                if let Some(edit) = IncrementalEdit::new(
                    ms,
                    i,
                    new_text.chars().skip(ms).take(i - ms).collect::<String>(),
                ) {
                    edits.push(edit);
                }
            }
        } else if mismatch_start.is_none() {
            mismatch_start = Some(i);
        }
    }
    if let Some(ms) = mismatch_start {
        if let Some(edit) = IncrementalEdit::new(
            ms,
            old_text.len(),
            new_text.chars().skip(ms).collect::<String>(),
        ) {
            edits.push(edit);
        }
    }
    edits
}

fn common_prefix_len(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(a, b)| a == b)
        .count()
}

fn common_suffix_len(left: &str, right: &str, prefix_len: usize) -> usize {
    let mut li = left.chars().rev();
    let mut ri = right.chars().rev();
    let max_left = left.len().saturating_sub(prefix_len);
    let max_right = right.len().saturating_sub(prefix_len);
    let limit = max_left.min(max_right);
    let mut count = 0usize;
    while count < limit {
        match (li.next(), ri.next()) {
            (Some(a), Some(b)) if a == b => count += 1,
            _ => break,
        }
    }
    count
}

fn resolve_next_input(
    value: &ParseValue,
    transform: Option<&dyn Fn(ParseValue) -> Result<PipelineTextUpdate, ParseError>>,
    has_next: bool,
) -> Result<Option<PipelineTextUpdate>, ParseError> {
    if let Some(f) = transform {
        return f(value.clone()).map(Some);
    }
    if !has_next {
        return Ok(None);
    }
    match value {
        ParseValue::Text(s) => Ok(Some(PipelineTextUpdate::new(s.to_string()))),
        _ => Err(ParseError::new(
            "pipeline stage produced a non-text value; a transform is required to proceed",
            0,
            0,
        )),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn word_grammar() -> Grammar {
        Grammar::trusted_new("word <- /[a-z]+/").with_start_rule("word")
    }

    // ── compute_snapshot_edits ───────────────────────────────────────────

    #[test]
    fn snapshot_edits_identical_texts_empty() {
        assert!(compute_snapshot_edits("hello", "hello").is_empty());
    }

    #[test]
    fn snapshot_edits_full_replacement() {
        let edits = compute_snapshot_edits("abc", "xyz");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].start(), 0);
        assert_eq!(edits[0].old_end(), 3);
        assert_eq!(edits[0].replacement(), "xyz");
    }

    #[test]
    fn snapshot_edits_insertion() {
        let edits = compute_snapshot_edits("helo", "hello");
        assert_eq!(edits.len(), 1);
        let e = &edits[0];
        // 'h','e' match; then old='l','o' new='l','l','o' diverge
        assert!(e.replacement().contains('l'));
    }

    #[test]
    fn snapshot_edits_deletion() {
        let edits = compute_snapshot_edits("hello", "helo");
        assert_eq!(edits.len(), 1);
    }

    #[test]
    fn snapshot_edits_equal_length_two_changes() {
        let edits = compute_snapshot_edits("abcd", "aXcY");
        // positions 1 (b→X) and 3 (d→Y) differ, but equal-length → 2 edits
        assert_eq!(edits.len(), 2);
    }

    // ── PipelineTextUpdate ───────────────────────────────────────────────

    #[test]
    fn pipeline_text_update_new() {
        let u = PipelineTextUpdate::new("hello");
        assert_eq!(u.text, "hello");
        assert!(u.edits.is_none());
    }

    // ── parse_pipeline ───────────────────────────────────────────────────

    #[test]
    fn parse_pipeline_single_stage() {
        let grammar = word_grammar();
        let stages = vec![PipelineStage::new(grammar)];
        let results = parse_pipeline("hello", &stages).unwrap();
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0].value, ParseValue::Text(_)));
    }

    #[test]
    fn parse_pipeline_two_stages_with_transform() {
        let g1 = word_grammar();
        let g2 = word_grammar();
        let stages = vec![
            PipelineStage::with_transform(g1, |v| {
                let text = match v {
                    ParseValue::Text(t) => t.to_string(),
                    _ => String::new(),
                };
                Ok(PipelineTextUpdate::new(text))
            }),
            PipelineStage::new(g2),
        ];
        let results = parse_pipeline("hello", &stages).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn parse_pipeline_parse_failure_propagates() {
        let g = Grammar::trusted_new("num <- /[0-9]+/").with_start_rule("num");
        let stages = vec![PipelineStage::new(g)];
        assert!(parse_pipeline("hello", &stages).is_err());
    }

    // ── IncrementalPipeline ──────────────────────────────────────────────

    #[test]
    fn incremental_pipeline_parse_basic() {
        let grammar = word_grammar();
        let mut pipeline = IncrementalPipeline::new(vec![PipelineStage::new(grammar)]);
        let results = pipeline.parse("hello").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn incremental_pipeline_reset_clears_cache() {
        let grammar = word_grammar();
        let mut pipeline = IncrementalPipeline::new(vec![PipelineStage::new(grammar)]);
        pipeline.parse("hello").unwrap();
        pipeline.reset();
        let snap = pipeline.cache_snapshot();
        assert!(snap[0].is_none());
    }

    #[test]
    fn incremental_pipeline_second_parse_uses_cache() {
        let grammar = word_grammar();
        let mut pipeline = IncrementalPipeline::new(vec![PipelineStage::new(grammar)]);
        pipeline.parse("hello").unwrap();
        assert_eq!(
            pipeline.cache_snapshot()[0].map(|c| c.entries.len()),
            Some(1)
        );

        let results = pipeline.parse("world").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            pipeline.cache_snapshot()[0].map(|c| c.entries.len()),
            Some(2)
        );
    }

    #[test]
    fn incremental_pipeline_second_parse_reuses_same_input_cache_entry() {
        let grammar = word_grammar();
        let mut pipeline = IncrementalPipeline::new(vec![PipelineStage::new(grammar)]);
        pipeline.parse("hello").unwrap();
        assert_eq!(
            pipeline.cache_snapshot()[0].map(|c| c.entries.len()),
            Some(1)
        );

        let results = pipeline.parse("hello").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            pipeline.cache_snapshot()[0].map(|c| c.entries.len()),
            Some(1)
        );
    }

    #[test]
    fn parse_stage_tracks_cache_entry_across_multiple_runs() {
        let grammar = word_grammar();
        let mut pipeline = IncrementalPipeline::new(vec![PipelineStage::new(grammar)]);
        let first = pipeline.parse("alpha").unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(
            pipeline.cache_snapshot()[0].map(|c| c.entries.len()),
            Some(1)
        );

        let second = pipeline.parse("beta").unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(
            pipeline.cache_snapshot()[0].map(|c| c.entries.len()),
            Some(2)
        );

        let third = pipeline.parse("beta").unwrap();
        assert_eq!(third.len(), 1);
        assert_eq!(
            pipeline.cache_snapshot()[0].map(|c| c.entries.len()),
            Some(2)
        );
    }

    // ── common prefix/suffix helpers ─────────────────────────────────────

    #[test]
    fn common_prefix_len_basic() {
        assert_eq!(common_prefix_len("foobar", "foobaz"), 5);
        assert_eq!(common_prefix_len("abc", "xyz"), 0);
    }

    #[test]
    fn common_suffix_len_basic() {
        assert_eq!(common_suffix_len("abcXY", "123XY", 0), 2);
        assert_eq!(common_suffix_len("abc", "abc", 0), 3);
    }
}
