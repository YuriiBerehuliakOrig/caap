use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use crate::ast::AstNode;
use crate::diagnostics_utils::{compute_line_offsets, line_col_u32};
use crate::driver::ParserConfigContext;
use crate::error::ParseError;
use crate::expr::PegExpr;
use crate::grammar::{Grammar, GrammarPatch};
use crate::incremental_edits::IncrementalEditError;
use crate::parser_compile::{
    ensure_memo_allowed_for_left_recursion, get_compiled, memo_runtime_config,
    resolve_indentation_enabled, resolve_invalid_rule_policy, resolve_trivia_skipper,
    runtime_signature, validate_lex_tokens, ChoiceDispatch, CompiledGrammar, MemoRuntimeConfig,
};
use crate::parser_diagnostics::{parse_error_with_location, parse_error_with_precomputed_location};
use crate::parser_engine_diag::{DiagnosticsState, LayoutState};
use crate::parser_engine_state::{
    fnv_hash_64, outcome_end, EvalCfg, EvalState, ParseCtx, ParseOutcome, PegEvaluatorInit,
};
#[cfg(feature = "recovery")]
use crate::recovery::{recover_parse, try_recover_parse, RecoveredParse, RecoveryConfig};
use crate::signature::grammar_signature;
use crate::types::{
    CompletedEdit, CompletedPrefixParse, IncrementalEdit, ParseCache, ParseValue, ParserConfig,
};

mod matchers;

/// The PEG parser engine. A zero-sized handle; the ergonomic entry point is
/// [`crate::ParseRequest`].
#[derive(Default, Clone)]
pub struct PEGParser;

#[derive(Clone, Debug, Eq, PartialEq)]
/// The result of [`crate::ParseRequest::run_output`]: a value or an AST.
pub enum ParseOutput {
    /// A [`ParseValue`] tree.
    Value(ParseValue),
    /// An [`AstNode`] tree.
    Ast(AstNode),
}

impl PEGParser {
    /// Parse `text` against `grammar` with `config`, returning a [`ParseValue`].
    pub fn parse(
        &self,
        grammar: &Grammar,
        text: &str,
        config: &ParserConfig,
    ) -> Result<ParseValue, ParseError> {
        self.parse_with_driver(grammar, text, config, None)
    }

    pub(crate) fn parse_with_lex_tokens_and_driver(
        &self,
        grammar: &Grammar,
        text: &str,
        config: &ParserConfig,
        tokens: std::sync::Arc<Vec<crate::types::LexToken>>,
        driver: Option<&dyn crate::driver::ParseDriver>,
    ) -> Result<ParseValue, ParseError> {
        self.run_full_parse(grammar, text, config, driver, Some(tokens), false)
            .map(|(value, _)| value)
    }

    /// Parse, optionally attaching a [`crate::driver::ParseDriver`] — the Parse
    /// Effects Protocol control surface that backs every semantic hook
    /// (`@action`/`@?pred`/`@!guard`) and global control. This is the entry
    /// point [`crate::ParseRequest`] uses.
    pub fn parse_with_driver(
        &self,
        grammar: &Grammar,
        text: &str,
        config: &ParserConfig,
        driver: Option<&dyn crate::driver::ParseDriver>,
    ) -> Result<ParseValue, ParseError> {
        self.run_full_parse(grammar, text, config, driver, None, false)
            .map(|(value, _)| value)
    }

    /// The full-input parse shared by every entry point: the plain value parse,
    /// the token-stream parse (`tokens` set), and the profiling parse
    /// (`profile_enabled`). Returns the parsed value plus an optional profile.
    pub(crate) fn run_full_parse(
        &self,
        grammar: &Grammar,
        text: &str,
        config: &ParserConfig,
        driver: Option<&dyn crate::driver::ParseDriver>,
        tokens: Option<std::sync::Arc<Vec<crate::types::LexToken>>>,
        profile_enabled: bool,
    ) -> Result<(ParseValue, Option<crate::profile::ParseProfile>), ParseError> {
        if config.max_steps == 0 {
            return Err(ParseError::new("max_steps must be > 0", 0, text.len()));
        }
        if text.len() > config.max_steps {
            return Err(ParseError::new(
                format!("input exceeds configured max_steps: {}", config.max_steps),
                0,
                text.len(),
            ));
        }
        if let Some(tokens) = &tokens {
            validate_lex_tokens(text, tokens.as_ref())?;
        }
        let memo = memo_runtime_config(config)?;
        let (compiled, cfg, indentation_enabled) = resolve_evaluator_parts(grammar, config, memo)?;
        ensure_memo_allowed_for_left_recursion(&memo, &compiled, text.len())?;

        if memo.enabled && !compiled.analysis_errors.is_empty() {
            return Err(ParseError::new(
                format!("invalid grammar: {}", compiled.analysis_errors.join(", ")),
                0,
                text.len(),
            ));
        }

        let mut evaluator = PegEvaluator::new(PegEvaluatorInit {
            ctx: ParseCtx::from_compiled(&compiled, text),
            cfg,
            indentation_enabled,
        })
        .with_driver(driver)
        .with_profiling(profile_enabled);
        if let Some(tokens) = tokens {
            evaluator = evaluator.with_tokens(tokens);
        }
        let outcome = evaluator.parse_rule(&grammar.start_rule, 0)?;
        let (value, end) = match outcome {
            ParseOutcome::Success { pos, value, .. } => (value, pos),
            ParseOutcome::Failure { .. } => {
                let furthest = evaluator.state.diag.furthest;
                let expected = evaluator.state.diag.expected_at_furthest();
                let msg = evaluator.driver_failure_message(
                    furthest,
                    &expected,
                    fail_message(&expected, "parse failed"),
                );
                // Reuse cached line offsets if the evaluator already computed them.
                let offsets = evaluator
                    .state
                    .line_offsets
                    .get_or_insert_with(|| compute_line_offsets(text));
                return Err(parse_error_with_precomputed_location(
                    msg,
                    furthest,
                    text,
                    offsets,
                    "parse_failed",
                ));
            }
        };

        // Trailing trivia after the start rule is ignorable by definition; a
        // failed repetition iteration rewinds past it, so consume it here
        // before judging full consumption.
        let end = evaluator.skip_trivia(end)?;
        if end != text.len() {
            let expected = evaluator.state.diag.expected_at_furthest();
            let msg = evaluator.driver_failure_message(
                end,
                &expected,
                fail_message(&expected, "did not consume complete input"),
            );
            let offsets = evaluator
                .state
                .line_offsets
                .get_or_insert_with(|| compute_line_offsets(text));
            return Err(parse_error_with_precomputed_location(
                msg,
                end,
                text,
                offsets,
                "incomplete_input",
            ));
        }

        let profile = evaluator.state.profile.take().map(|collector| {
            collector.finish(
                evaluator.state.expr_steps as u64,
                evaluator.state.diag.furthest,
            )
        });
        Ok((
            PegParserHelpers::maybe_span(value, 0, end, config.return_spans),
            profile,
        ))
    }

    pub(crate) fn parse_prefix(
        &self,
        grammar: &Grammar,
        text: &str,
        start_pos: usize,
        start_rule: Option<&str>,
        config: &ParserConfig,
    ) -> CompletedPrefixParse {
        if start_pos > text.len() {
            return prefix_error("start_pos is past input end");
        }

        if config.max_steps == 0 {
            return prefix_error("max_steps must be > 0");
        }

        if text.len() - start_pos > config.max_steps {
            return prefix_error(format!(
                "input exceeds configured max_steps: {}",
                config.max_steps
            ));
        }

        let effective_rule = start_rule.unwrap_or(&grammar.start_rule);

        let parts = memo_runtime_config(config)
            .and_then(|memo| resolve_evaluator_parts(grammar, config, memo));
        let (compiled, cfg, indentation_enabled) = match parts {
            Ok(parts) => parts,
            Err(err) => return prefix_error(err.message.to_string()),
        };
        let mut evaluator = PegEvaluator::new(PegEvaluatorInit {
            ctx: ParseCtx::from_compiled(&compiled, &text[start_pos..]),
            cfg,
            indentation_enabled,
        });
        match evaluator.parse_rule(effective_rule, 0) {
            Err(err) => prefix_error(err.message.to_string()),
            Ok(ParseOutcome::Failure { .. }) => prefix_error("failed to parse prefix"),
            Ok(ParseOutcome::Success { pos, value, .. }) => CompletedPrefixParse {
                value: Some(PegParserHelpers::maybe_span(
                    value,
                    start_pos,
                    start_pos + pos,
                    config.return_spans,
                )),
                consumed: pos,
                eof: start_pos + pos >= text.len(),
                errors: Vec::new(),
            },
        }
    }

    pub(crate) fn parse_incremental_many(
        &self,
        grammar: &Grammar,
        text: &str,
        config: &ParserConfig,
        cache: &mut ParseCache,
    ) -> Result<std::sync::Arc<ParseValue>, ParseError> {
        let memo = memo_runtime_config(config)?;
        if memo.enabled {
            let hash = fnv_hash_64(text.as_bytes());
            let gram_sig = grammar_signature(grammar);
            let run_sig = runtime_signature(grammar, config)?;

            // Fast path: exact same text + grammar + runtime already cached.
            if let Some(found) = cache
                .entries
                .iter()
                .find(|entry| {
                    entry.text_hash == hash
                        && entry.grammar_signature == gram_sig
                        && entry.runtime_signature == run_sig
                })
                .map(|entry| entry.output.clone())
            {
                return Ok(found);
            }

            // Prepare a position-level seed from the previous run. The cache
            // transplant layer owns compatibility checks, edit detection, and
            // embedded span projection so memo values cannot carry stale spans.
            let cache_tables = crate::cache_transforms::build_parse_cache_seed(
                cache.pos_cache.as_ref(),
                text,
                &[],
                None,
                true,
                gram_sig,
                run_sig,
            )
            .map_err(|error| incremental_edit_error_to_parse_error(error, text.len()))?;
            let seeded_pos_cache = if cache_tables.memo_data.is_empty() {
                None
            } else {
                Some(crate::types::PositionCache {
                    text: text.to_string(),
                    grammar_hash: gram_sig,
                    runtime_signature: run_sig,
                    memo: cache_tables.memo_data,
                })
            };

            // Run the parse with optional position seed.
            let (compiled, cfg, indentation_enabled) =
                resolve_evaluator_parts(grammar, config, memo)?;
            let mut evaluator = PegEvaluator::new(PegEvaluatorInit {
                ctx: ParseCtx::from_compiled(&compiled, text),
                cfg,
                indentation_enabled,
            })
            .with_read_extent_tracking();
            if let Some(ref seed) = seeded_pos_cache {
                evaluator = evaluator.with_pos_seed(seed);
            }

            let outcome = evaluator.parse_rule(&grammar.start_rule, 0)?;
            let (value, end) = match outcome {
                ParseOutcome::Success { pos, value, .. } => (value, pos),
                ParseOutcome::Failure { .. } => {
                    let expected = evaluator.state.diag.expected_at_furthest();
                    let msg = fail_message(&expected, "parse failed");
                    return Err(ParseError::new(
                        msg,
                        evaluator.state.diag.furthest,
                        text.len(),
                    ));
                }
            };

            let end = evaluator.skip_trivia(end)?;
            if end != text.len() {
                let expected = evaluator.state.diag.expected_at_furthest();
                let msg = fail_message(&expected, "did not consume complete input");
                return Err(parse_error_with_location(
                    msg,
                    end,
                    text,
                    "incomplete_input",
                ));
            }

            // Export memo entries to the persistent position cache.
            let exported = evaluator.export_memo();
            let mut new_pc = seeded_pos_cache
                .unwrap_or_else(|| crate::types::PositionCache::new(text, gram_sig, run_sig));
            new_pc.grammar_hash = gram_sig;
            new_pc.runtime_signature = run_sig;
            new_pc.text = text.to_string();
            new_pc.absorb_with_limit(exported, memo.rule_limit);
            cache.pos_cache = Some(new_pc);

            // Record this whole-input result for exact-text reuse; the position
            // cache handles inter-edit speedup separately.
            let wrapped = if config.return_spans {
                PegParserHelpers::maybe_span(value, 0, end, true)
            } else {
                value
            };
            let output = std::sync::Arc::new(wrapped);
            cache.insert_exact_result(crate::types::CachedResult {
                text_hash: hash,
                grammar_signature: gram_sig,
                runtime_signature: run_sig,
                output: output.clone(),
            });
            return Ok(output);
        }

        self.parse(grammar, text, config).map(std::sync::Arc::new)
    }

    /// Normalise and order a batch of edits into non-overlapping sequential edits.
    pub fn snapshot_edits_to_sequential(
        &self,
        base_text: &str,
        edits: &[IncrementalEdit],
    ) -> Result<Vec<CompletedEdit>, ParseError> {
        crate::incremental_edits::snapshot_edits_to_sequential(base_text, edits)
            .map(|edits| {
                edits
                    .into_iter()
                    .map(|edit| {
                        let span = (edit.start(), edit.old_end());
                        CompletedEdit {
                            text: edit.into_replacement(),
                            span,
                        }
                    })
                    .collect()
            })
            .map_err(|error| incremental_edit_error_to_parse_error(error, base_text.len()))
    }

    /// Apply sequential [`CompletedEdit`]s to `base_text`, returning the result.
    pub fn apply_edits(base_text: &str, edits: &[CompletedEdit]) -> Result<String, ParseError> {
        if edits.is_empty() {
            return Ok(base_text.to_string());
        }

        let mut out = String::new();
        let mut cursor = 0usize;
        for edit in edits {
            let (start, end) = edit.span;
            if start > end || end > base_text.len() || start < cursor {
                return Err(ParseError::new(
                    format!(
                        "invalid sequential edit span: start={start} end={end} cursor={cursor} len={}",
                        base_text.len()
                    ),
                    cursor.min(base_text.len()),
                    base_text.len(),
                ));
            }
            if !base_text.is_char_boundary(start) || !base_text.is_char_boundary(end) {
                return Err(ParseError::new(
                    format!("edit span is not on UTF-8 character boundaries: [{start},{end})"),
                    start.min(base_text.len()),
                    end.min(base_text.len()),
                ));
            }
            out.push_str(&base_text[cursor..edit.span.0]);
            out.push_str(&edit.text);
            cursor = edit.span.1;
        }
        out.push_str(&base_text[cursor..]);
        Ok(out)
    }

    /// Batch error-recovery parse using sync markers (parity with the recovery parser contract).
    #[cfg(feature = "recovery")]
    pub fn recover_parse(
        &self,
        grammar: &Grammar,
        text: &str,
        config: RecoveryConfig,
        parse_config: &ParserConfig,
    ) -> RecoveredParse {
        recover_parse(
            text,
            grammar,
            |chunk, g| self.parse(g, chunk, parse_config),
            &config,
        )
    }

    /// Like [`recover_parse`](Self::recover_parse) but returns an error instead
    /// of an empty result when recovery cannot proceed.
    #[cfg(feature = "recovery")]
    pub fn try_recover_parse(
        &self,
        grammar: &Grammar,
        text: &str,
        config: RecoveryConfig,
        parse_config: &ParserConfig,
    ) -> Result<RecoveredParse, ParseError> {
        try_recover_parse(
            text,
            grammar,
            |chunk, g| self.parse(g, chunk, parse_config),
            &config,
        )
    }

    /// Clone `grammar`, optionally applying a [`GrammarPatch`], resetting caches.
    pub fn clone_grammar(
        grammar: &Grammar,
        patch: Option<GrammarPatch>,
    ) -> Result<Grammar, ParseError> {
        let mut copied = grammar.clone();
        copied.state.analysis_state = None;
        if let Some(patch) = patch {
            copied.text = patch.source;
            if let Some(rule) = patch.start_rule {
                copied.start_rule = rule;
            }
            copied.rules = crate::grammar::try_parse_rules_from_text(&copied.text)?;
        }
        copied.bump_version()?;
        Ok(copied)
    }
}

/// Build a failed [`CompletedPrefixParse`] carrying a single error message.
/// Centralises the `value: None, consumed: 0, eof: false` shape that every
/// early-return in `parse_prefix` would otherwise repeat.
fn prefix_error(message: impl Into<String>) -> CompletedPrefixParse {
    CompletedPrefixParse::failed(message)
}

/// Resolve the per-parse evaluator inputs shared by every entry point: the
/// invalid-rule policy, the compiled grammar, the trivia skipper, indentation
/// mode, and a borrow-free [`EvalCfg`]. Each caller builds its own
/// `PegEvaluator` from these (the evaluator borrows the returned `compiled`, so
/// it must outlive the evaluator in the caller) and attaches its own extras —
/// driver, tokens, profiling, read-extent tracking, or a position seed.
fn resolve_evaluator_parts<'a>(
    grammar: &Grammar,
    config: &ParserConfig,
    memo: MemoRuntimeConfig,
) -> Result<(Arc<CompiledGrammar>, EvalCfg<'a>, bool), ParseError> {
    let invalid_rule_policy = resolve_invalid_rule_policy(grammar, config)?;
    let compiled = get_compiled(grammar)?;
    let grammar_meta = grammar.metadata.get("__grammar__");
    let trivia = resolve_trivia_skipper(grammar_meta)?;
    let indentation_enabled = resolve_indentation_enabled(grammar_meta)?;
    let cfg = EvalCfg::new(
        invalid_rule_policy,
        trivia,
        memo,
        ParserConfigContext::from_config(config),
    );
    Ok((compiled, cfg, indentation_enabled))
}

/// Build a root-failure message from the furthest expected-token set, falling
/// back to `empty` when nothing was recorded. Shared by every root parse entry.
fn fail_message(expected: &[String], empty: &str) -> String {
    if expected.is_empty() {
        empty.to_string()
    } else {
        format!("expected: {}", expected.join(", "))
    }
}

fn incremental_edit_error_to_parse_error(
    error: IncrementalEditError,
    base_len: usize,
) -> ParseError {
    match error {
        IncrementalEditError::InvalidRange {
            index,
            start,
            old_end,
            len,
        } => {
            let span_start = start.min(len);
            let span_end = old_end.min(len).max(span_start);
            ParseError::new(
                format!(
                    "invalid edit range at edit[{index}]: start={start} old_end={old_end} len={len}"
                ),
                span_start,
                span_end,
            )
            .with_code("invalid_incremental_edit_range")
        }
        IncrementalEditError::OverlappingEdits {
            cur_index,
            cur_start,
            cur_end,
            prev_index,
            prev_start,
            prev_end,
        } => {
            let span_start = cur_start.min(prev_start).min(base_len);
            let span_end = cur_end.max(prev_end).min(base_len).max(span_start);
            ParseError::new(
                format!(
                    "overlapping snapshot edits: edit[{cur_index}] [{cur_start},{cur_end}) overlaps \
                     edit[{prev_index}] [{prev_start},{prev_end})"
                ),
                span_start,
                span_end,
            )
            .with_code("overlapping_incremental_edits")
        }
        IncrementalEditError::DeltaOverflow {
            index,
            inserted_len,
            removed_len,
        } => ParseError::new(
            format!(
                "incremental edit[{index}] length delta is too large: inserted={inserted_len} removed={removed_len}"
            ),
            0,
            base_len.min(1),
        )
        .with_code("incremental_edit_delta_overflow"),
        IncrementalEditError::OffsetOverflow {
            index,
            offset,
            delta,
        } => {
            let span_start = offset.min(base_len);
            ParseError::new(
                format!(
                    "incremental edit[{index}] shifted offset overflows: offset={offset} delta={delta}"
                ),
                span_start,
                span_start,
            )
            .with_code("incremental_edit_offset_overflow")
        }
    }
}

// ── Evaluator ──────────────────────────────────────────────────────────────

struct PegEvaluator<'a> {
    ctx: ParseCtx<'a>,
    cfg: EvalCfg<'a>,
    state: EvalState<'a>,
}

impl<'a> PegEvaluator<'a> {
    fn new(init: PegEvaluatorInit<'a>) -> Self {
        let use_memo = init.cfg.use_memo;
        let profile_enabled = init.cfg.profile_enabled;
        Self {
            ctx: init.ctx,
            cfg: init.cfg,
            state: EvalState {
                rule_memo: if use_memo {
                    HashMap::with_capacity(64)
                } else {
                    HashMap::new()
                },
                diag: DiagnosticsState::new(),
                layout: LayoutState::new(init.indentation_enabled),
                line_offsets: None,
                params: Vec::new(),
                rule_stack: Vec::new(),
                trivia_on: true,
                expr_steps: 0,
                expr_depth: 0,
                captures: HashMap::new(),
                read_stack: Vec::new(),
                last_read_extent: crate::parser_engine_state::ReadExtent::empty(0),
                read_memo: if use_memo {
                    HashMap::with_capacity(64)
                } else {
                    HashMap::new()
                },
                profile: profile_enabled.then(crate::profile::ProfileCollector::new),
                lr_growing: HashSet::new(),
                trivia_cache: HashMap::new(),
                trivia_override_stack: Vec::new(),
            },
        }
    }

    fn with_tokens(mut self, tokens: std::sync::Arc<Vec<crate::types::LexToken>>) -> Self {
        self.cfg.lex_tokens = Some(tokens);
        self
    }

    fn with_pos_seed(mut self, seed: &'a crate::types::PositionCache) -> Self {
        self.cfg.pos_seed = Some(seed);
        self
    }

    /// Enable per-rule read-extent tracking (only the incremental parse path
    /// needs it, to export sound examined intervals to the position cache).
    fn with_read_extent_tracking(mut self) -> Self {
        self.cfg.track_read_extent = true;
        self
    }

    /// Enable per-rule profiling. The collector must be (re)created here because
    /// `cfg.profile_enabled` is consulted by `PegEvaluator::new` before this
    /// builder runs only when set via the init config; setting it after needs
    /// the collector installed explicitly.
    fn with_profiling(mut self, enabled: bool) -> Self {
        self.cfg.profile_enabled = enabled;
        if enabled && self.state.profile.is_none() {
            self.state.profile = Some(crate::profile::ProfileCollector::new());
        }
        self
    }

    fn with_driver(mut self, driver: Option<&'a dyn crate::driver::ParseDriver>) -> Self {
        self.cfg.driver = driver;
        self
    }

    // ── Driver protocol (Parse Effects Protocol) ───────────────────────────

    /// Raise a [`crate::driver::ParseEffect`] to the attached driver and return
    /// its [`crate::driver::Directive`]. Returns `Proceed` when no driver is
    /// attached, so callers can treat "no driver" and "driver said proceed"
    /// uniformly. Builds a fresh `ParseView` (and a lazily-used sub-parse
    /// capability) from the live evaluator state.
    fn raise_effect(&self, effect: crate::driver::ParseEffect<'_>) -> crate::driver::Directive {
        let Some(driver) = self.cfg.driver else {
            return crate::driver::Directive::Proceed;
        };
        let runner = SubParseRunner {
            ctx: &self.ctx,
            cfg: &self.cfg,
            indentation_enabled: self.state.layout.indentation_enabled,
        };
        let (value, span) = effect_value_span(&effect);
        let matched_text = span
            .and_then(|(start, end)| self.ctx.text.get(start..end))
            .unwrap_or("");
        // Cheap to build: only borrows. The rich projections (named/items/
        // grammar/config/state) are computed on demand inside `ParseView`.
        let view = crate::driver::ParseView {
            source: self.ctx.text,
            pos: effect_pos(&effect),
            span,
            matched_text,
            args: effect_args(&effect),
            rule_stack: &self.state.rule_stack,
            start_rule: self.ctx.grammar_start,
            value,
            import_aliases: self.ctx.import_aliases,
            metadata_keys: self.ctx.metadata_keys,
            rule_count: self.ctx.rules.len(),
            config_context: &self.cfg.config_context,
            trivia_on: self.state.trivia_on,
            param_depth: self.state.params.len(),
            memo_entries: self.state.rule_memo.len(),
            indentation_enabled: self.state.layout.indentation_enabled,
            bracket_depth: self.state.layout.bracket_depth,
            sub: Some(&runner),
        };
        // Named host hooks run arbitrary user logic by name, so a panic there is
        // converted to a hard parse failure (parity with the former semantic
        // hooks). Pure lifecycle/control effects are the driver's own logic; a
        // panic there propagates as a normal bug rather than paying catch_unwind
        // on every rule.
        if effect_is_value_hook(&effect) {
            match catch_unwind(AssertUnwindSafe(|| driver.handle(&effect, &view))) {
                Ok(directive) => directive,
                Err(panic) => {
                    let detail = panic
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "non-string panic payload".to_string());
                    crate::driver::Directive::Fail(format!("driver hook panicked: {detail}"))
                }
            }
        } else {
            driver.handle(&effect, &view)
        }
    }

    /// Apply a host verdict to a matched sub-result — the single
    /// `Accept`/`Reject`/`Commit`/`Fail`/`Proceed` → outcome translation shared
    /// by every value hook. `snapshot` guards host state the hook's inner
    /// expression mutated: committed on accept, rolled back on reject/fail.
    fn apply_verdict(
        &mut self,
        directive: crate::driver::Directive,
        snapshot: Speculation,
        matched: MatchedHook,
        reject_label: &str,
    ) -> Result<ParseOutcome, ParseError> {
        use crate::driver::Directive;
        let MatchedHook {
            pos,
            end,
            value,
            cut,
        } = matched;
        match directive {
            Directive::Proceed | Directive::Restrict(_) => {
                self.spec_commit(snapshot);
                Ok(ParseOutcome::Success {
                    pos: end,
                    value,
                    cut,
                })
            }
            Directive::Accept(new_value) => {
                self.spec_commit(snapshot);
                Ok(ParseOutcome::Success {
                    pos: end,
                    value: new_value,
                    cut,
                })
            }
            Directive::Commit => {
                self.spec_commit(snapshot);
                Ok(ParseOutcome::Success {
                    pos: end,
                    value,
                    cut: true,
                })
            }
            Directive::Reject => {
                self.spec_rollback(snapshot);
                self.state
                    .diag
                    .record_expected(end, reject_label.to_string());
                Ok(ParseOutcome::failure(end))
            }
            Directive::Fail(message) => {
                self.spec_rollback(snapshot);
                Err(ParseError::new(message, pos, self.ctx.text.len()).with_code("driver_fail"))
            }
        }
    }

    /// Snapshot host state before a speculative branch (no-op without a driver).
    fn driver_checkpoint(&self) -> crate::driver::DriverCheckpoint {
        match self.cfg.driver {
            Some(driver) => driver.checkpoint(),
            None => crate::driver::DriverCheckpoint::none(),
        }
    }

    /// Restore host state after a speculative branch failed.
    fn driver_rollback(&self, snapshot: crate::driver::DriverCheckpoint) {
        if let Some(driver) = self.cfg.driver {
            driver.rollback(snapshot);
        }
    }

    /// Discard a snapshot once the branch it guarded is kept.
    fn driver_commit(&self, snapshot: crate::driver::DriverCheckpoint) {
        if let Some(driver) = self.cfg.driver {
            driver.commit(snapshot);
        }
    }

    // ── Speculation checkpoints (driver state + capture store) ─────────────

    /// Snapshot everything that must be rolled back if a speculative branch
    /// fails: the host driver's state *and* the engine's `backref` capture store.
    /// Cheap when unused (no driver → empty driver snapshot; no captures → `None`).
    fn spec_checkpoint(&self) -> Speculation {
        Speculation {
            driver: self.driver_checkpoint(),
            captures: if self.state.captures.is_empty() {
                None
            } else {
                Some(self.state.captures.clone())
            },
        }
    }

    /// Restore both driver state and the capture store from a checkpoint.
    fn spec_rollback(&mut self, snapshot: Speculation) {
        self.driver_rollback(snapshot.driver);
        match snapshot.captures {
            Some(captures) => self.state.captures = captures,
            // The store was empty at checkpoint; discard anything the branch added.
            None => self.state.captures.clear(),
        }
    }

    /// Keep the branch: discard the driver snapshot, keep any new captures.
    fn spec_commit(&self, snapshot: Speculation) {
        self.driver_commit(snapshot.driver);
    }

    /// A no-op checkpoint for zero-width hooks (nothing to roll back).
    fn spec_none(&self) -> Speculation {
        Speculation {
            driver: crate::driver::DriverCheckpoint::none(),
            captures: None,
        }
    }

    /// The host driver's memo facet for `rule`, as a packrat key component.
    /// `0` means state-independent (pure): the fast path with no driver, and the
    /// only facet whose results may be reused across runs (the position cache).
    /// A `Depends(h)` digest of `0` is remapped to `1` so it never aliases pure.
    fn memo_facet_hash(&self, rule: &str) -> u64 {
        match self.cfg.driver {
            Some(driver) => match driver.memo_facet(rule) {
                crate::driver::MemoFacet::Pure => 0,
                crate::driver::MemoFacet::Depends(0) => 1,
                crate::driver::MemoFacet::Depends(hash) => hash,
            },
            None => 0,
        }
    }

    /// Raise a `Failed` effect so the driver can rewrite the diagnostic message;
    /// returns the host's `Fail(msg)` override or `default` otherwise.
    fn driver_failure_message(
        &self,
        furthest: usize,
        expected: &[String],
        default: String,
    ) -> String {
        if self.cfg.driver.is_none() {
            return default;
        }
        match self.raise_effect(crate::driver::ParseEffect::Failed { furthest, expected }) {
            crate::driver::Directive::Fail(custom) => custom,
            _ => default,
        }
    }

    // ── Read-extent tracking (incremental subtree reuse) ───────────────────

    /// Record that the parse examined the half-open byte interval `[lo, hi)`,
    /// widening the innermost active rule frame. A no-op outside any rule.
    #[inline]
    fn note_read(&mut self, lo: usize, hi: usize) {
        if let Some(top) = self.state.read_stack.last_mut() {
            top.note(lo, hi);
        }
    }

    /// One byte position past `end` extended by the full UTF-8 char at `end`
    /// (the greedy "stop byte" a `+`/`*`/`?` terminal peeks to decide it cannot
    /// extend). Returns `end` at end-of-input. Used to record a sound, tight read
    /// extent for read-bounded terminals.
    #[inline]
    fn examined_with_lookahead(&self, end: usize) -> usize {
        match self.ctx.text[end..].chars().next() {
            Some(c) => end + c.len_utf8(),
            None => end,
        }
    }

    /// Fold an already-computed extent (e.g. from a memo / seed hit) into the
    /// innermost active rule frame.
    #[inline]
    fn merge_read(&mut self, extent: crate::parser_engine_state::ReadExtent) {
        if let Some(top) = self.state.read_stack.last_mut() {
            top.merge(extent);
        }
    }

    /// Push a fresh read-extent frame for a rule body starting at `pos`. A no-op
    /// when tracking is off, so plain parses never touch the frame stack and
    /// `note_read`/`merge_read` (which only widen the top frame) stay inert.
    #[inline]
    fn push_read_frame(&mut self, pos: usize) {
        if !self.cfg.track_read_extent {
            return;
        }
        self.state
            .read_stack
            .push(crate::parser_engine_state::ReadExtent::empty(pos));
    }

    /// Pop the innermost read-extent frame, merge it into its parent, and stash
    /// it in `last_read_extent` for the caller to memoize.
    #[inline]
    fn pop_read_frame(&mut self) {
        if !self.cfg.track_read_extent {
            return;
        }
        if let Some(frame) = self.state.read_stack.pop() {
            self.state.last_read_extent = frame;
            if let Some(parent) = self.state.read_stack.last_mut() {
                parent.merge(frame);
            }
        }
    }

    fn memo_insert(
        &mut self,
        key: (&'a str, usize, u64),
        outcome: ParseOutcome,
    ) -> Result<(), ParseError> {
        if !self.cfg.use_memo {
            return Ok(());
        }
        let can_insert = self.cfg.memo_rule_limit.is_none_or(|limit| {
            self.state.rule_memo.contains_key(&key) || self.state.rule_memo.len() < limit
        });
        if can_insert {
            self.state.rule_memo.insert(key, outcome);
            Ok(())
        } else {
            Err(ParseError::new(
                format!(
                    "parser memo budget exceeded while caching rule {:?} at byte {}",
                    key.0, key.1
                ),
                key.1,
                self.ctx.text.len(),
            ))
        }
    }

    /// Export successful memo entries as `(rule_name, start, end, value)` tuples.
    ///
    /// Only pure entries (`facet == 0`) are exported to the cross-run position
    /// cache: state-dependent results are sound only within the run whose state
    /// produced their digest, so replaying them in a later run would be unsound.
    fn export_memo(&self) -> Vec<crate::types::ExportedMemoEntry> {
        self.state
            .rule_memo
            .iter()
            .filter_map(|(key @ (name, start, facet), outcome)| {
                if *facet != 0 {
                    return None;
                }
                if let ParseOutcome::Success {
                    pos: end,
                    value,
                    cut,
                } = outcome
                {
                    // Carry the examined interval so the position cache can
                    // invalidate this entry soundly across edits. Fall back to
                    // the matched span if no extent was recorded.
                    let (read_lo, read_hi) = self
                        .state
                        .read_memo
                        .get(key)
                        .map(|e| (e.lo.min(*start), e.hi.max(*end)))
                        .unwrap_or((*start, *end));
                    Some(crate::types::ExportedMemoEntry {
                        rule: name.to_string(),
                        start: *start,
                        end: *end,
                        cut: *cut,
                        read_lo,
                        read_hi,
                        value: value.clone(),
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    // ── Trivia helpers ─────────────────────────────────────────────────────

    /// Advance `pos` past trivia when trivia skipping is active. A `with_trivia`
    /// region overrides the grammar's default skipper for its scope.
    fn skip_trivia(&mut self, pos: usize) -> Result<usize, ParseError> {
        if !self.state.trivia_on {
            return Ok(pos);
        }
        let skipper: Option<&dyn crate::skip::SkipStrategy> =
            if let Some(spec) = self.state.trivia_override_stack.last() {
                self.state.trivia_cache.get(spec).and_then(|s| s.as_deref())
            } else {
                self.cfg.trivia.as_deref()
            };
        let Some(skipper) = skipper else {
            return Ok(pos);
        };
        match skipper.try_skip(self.ctx.text, pos) {
            Ok(p) => {
                // Skipped trivia was examined; the boundary byte at `p` is
                // covered by the matcher that resumes there.
                self.note_read(pos, p);
                Ok(p)
            }
            Err(err) => {
                let offsets = self
                    .state
                    .line_offsets
                    .get_or_insert_with(|| compute_line_offsets(self.ctx.text));
                let (line, col) = line_col_u32(offsets, err.pos);
                Err(ParseError::with_context(
                    err.message,
                    err.pos,
                    err.pos,
                    Vec::new(),
                    Some("<eof>".to_string()),
                )
                .with_location(line, col)
                .with_rule_stack(vec!["skip".to_string()]))
            }
        }
    }

    // ── Rule dispatch ──────────────────────────────────────────────────────

    fn parse_rule(&mut self, rule: &str, pos: usize) -> Result<ParseOutcome, ParseError> {
        self.parse_rule_inner(rule, pos, &mut HashSet::new())
    }

    fn parse_rule_inner(
        &mut self,
        rule: &str,
        pos: usize,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        if self.cfg.invalid_rule_policy.excludes(rule) {
            return Ok(ParseOutcome::failure(pos));
        }

        // Check parameter bindings first (top of stack wins).
        if let Some(expr) = self.state.params.last().and_then(|frame| frame.get(rule)) {
            let expr = expr.clone();
            return self.parse_expr_owned(expr, pos, active);
        }

        // Copy the &'a reference out of self — lifetime 'a is independent of &mut self.
        let rules = self.ctx.rules;
        let Some((canonical_rule, expr)) = rules.get_key_value(rule) else {
            return Err(ParseError::new(format!("unknown rule '{rule}'"), pos, pos));
        };
        let canonical_rule: &'a str = canonical_rule.as_str();
        if let Some(p) = self.state.profile.as_mut() {
            p.record_call(canonical_rule);
        }

        // Keyed packrat memo: the host driver's state digest (`facet`) is part of
        // the memo key, so a state-dependent rule is cached *per state* — reused
        // when the digest matches, recomputed when it differs — instead of being
        // skipped. Pure rules (and the no-driver fast path) have `facet == 0`.
        let is_left_recursive = self.ctx.left_recursive_rules.contains(canonical_rule);
        let facet = self.memo_facet_hash(canonical_rule);
        let key = (canonical_rule, pos, facet);
        // Cycle/left-recursion bookkeeping is positional, independent of facet.
        let active_key = (canonical_rule, pos);
        if self.cfg.use_memo {
            if let Some(cached) = self.state.rule_memo.get(&key) {
                let cached = cached.clone();
                if let Some(p) = self.state.profile.as_mut() {
                    p.record_memo_hit(canonical_rule);
                }
                // The caller's outcome depends on what this subtree examined, so
                // bubble the memoized extent into the caller's frame even though
                // the body is not re-run.
                if let Some(extent) = self.state.read_memo.get(&key).copied() {
                    self.merge_read(extent);
                }
                return Ok(cached);
            }
            // The cross-run position cache seed is only sound for pure results.
            if facet == 0 {
                if let Some(seed) = self.cfg.pos_seed {
                    if let Some(entry) = seed.get(canonical_rule, pos) {
                        let outcome = ParseOutcome::Success {
                            pos: entry.end,
                            value: (*entry.value).clone(),
                            cut: entry.cut,
                        };
                        if let Some(p) = self.state.profile.as_mut() {
                            p.record_seed_hit(canonical_rule);
                        }
                        let (lo, hi) = entry.examined(pos);
                        let extent = crate::parser_engine_state::ReadExtent { lo, hi };
                        self.merge_read(extent);
                        self.state.read_memo.insert(key, extent);
                        self.memo_insert(key, outcome.clone())?;
                        return Ok(outcome);
                    }
                }
            }
            // A left-recursive rule drives seed-grow only when it is the *head*
            // of its SCC at this position — i.e. no peer in the same cycle is
            // already growing here. Involved (non-head) rules fall through to a
            // single body evaluation and reuse the head's seed via the memo,
            // which is what makes indirect / mutual left recursion work.
            if is_left_recursive && !active.contains(&active_key) {
                let scc = self.ctx.rule_scc.get(canonical_rule).copied();
                let head = scc.is_none_or(|s| !self.state.lr_growing.contains(&(s, pos)));
                if head {
                    return self.run_left_recursive_growth(canonical_rule, expr, pos, active);
                }
            }
            if active.contains(&active_key) {
                let failure = ParseOutcome::failure(pos);
                self.memo_insert(key, failure.clone())?;
                return Ok(failure);
            }
        } else {
            if is_left_recursive {
                return Err(ParseError::new(
                    format!(
                        "memoization cannot be disabled for left-recursive rule '{canonical_rule}'"
                    ),
                    pos,
                    self.ctx.text.len(),
                ));
            }
            if active.contains(&active_key) {
                return Ok(ParseOutcome::failure(pos));
            }
        }

        let result = self.parse_rule_body(canonical_rule, expr, pos, active)?;
        if let Some(p) = self.state.profile.as_mut() {
            p.record_body_run(canonical_rule, !result.is_success());
        }
        if self.cfg.use_memo {
            if self.cfg.track_read_extent {
                // `parse_rule_body` already merged this body's extent into the
                // caller's frame and published it as `last_read_extent`; record
                // it for replay on a later memo hit.
                self.state
                    .read_memo
                    .insert(key, self.state.last_read_extent);
            }
            self.memo_insert(key, result.clone())?;
        }
        Ok(result)
    }

    fn parse_rule_body(
        &mut self,
        canonical_rule: &'a str,
        expr: &'a PegExpr,
        pos: usize,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let key = (canonical_rule, pos);
        active.insert(key);
        self.push_read_frame(pos);
        let track_rule_stack = self.cfg.driver.is_some();
        if track_rule_stack {
            self.state.rule_stack.push(canonical_rule);
        }
        if self.cfg.driver.is_some() {
            let directive = self.raise_effect(crate::driver::ParseEffect::RuleEnter {
                rule: canonical_rule,
                pos,
            });
            if let crate::driver::Directive::Fail(message) = directive {
                active.remove(&key);
                self.pop_read_frame();
                if track_rule_stack {
                    self.state.rule_stack.pop();
                }
                return Err(
                    ParseError::new(message, pos, self.ctx.text.len()).with_code("driver_fail")
                );
            }
        }

        let rule_dispatch: &'a HashMap<String, ChoiceDispatch> = self.ctx.rule_dispatch;
        let mut result = if let PegExpr::Choice(alts) = expr {
            match rule_dispatch.get(canonical_rule) {
                Some(dispatch) => self.match_choice_dispatched(pos, alts, dispatch, active)?,
                None => self.match_choice(pos, alts, active)?,
            }
        } else {
            self.parse_expr(expr, pos, active)?
        };

        // RuleExit: let the driver transform / reject / commit / fail a rule
        // that matched syntactically.
        if self.cfg.driver.is_some() {
            if let ParseOutcome::Success {
                pos: end,
                value,
                cut,
            } = &result
            {
                let directive = self.raise_effect(crate::driver::ParseEffect::RuleExit {
                    rule: canonical_rule,
                    pos,
                    end: *end,
                    value,
                });
                match directive {
                    crate::driver::Directive::Reject => result = ParseOutcome::failure(*end),
                    crate::driver::Directive::Accept(new_value) => {
                        result = ParseOutcome::Success {
                            pos: *end,
                            value: new_value,
                            cut: *cut,
                        }
                    }
                    crate::driver::Directive::Commit => {
                        result = ParseOutcome::Success {
                            pos: *end,
                            value: value.clone(),
                            cut: true,
                        }
                    }
                    crate::driver::Directive::Fail(message) => {
                        active.remove(&key);
                        self.pop_read_frame();
                        if track_rule_stack {
                            self.state.rule_stack.pop();
                        }
                        return Err(ParseError::new(message, pos, self.ctx.text.len())
                            .with_code("driver_fail"));
                    }
                    crate::driver::Directive::Proceed | crate::driver::Directive::Restrict(_) => {}
                }
            }
        }

        active.remove(&key);
        self.pop_read_frame();
        if track_rule_stack {
            self.state.rule_stack.pop();
        }
        // A failed rule body emits a `RuleFail` observation (the AST builder
        // folds enter/exit/fail into its tree).
        if self.cfg.driver.is_some() && !result.is_success() {
            self.raise_effect(crate::driver::ParseEffect::RuleFail {
                rule: canonical_rule,
                pos,
            });
        }
        Ok(result)
    }

    /// Seed-grow the SCC head at `pos`, marking the SCC as "growing" so that
    /// peers in the same cycle (indirect/mutual LR) reuse the head's seed
    /// instead of starting their own growth. Cleanup runs on every exit path
    /// (including errors), so the marker can never leak.
    fn run_left_recursive_growth(
        &mut self,
        canonical_rule: &'a str,
        expr: &'a PegExpr,
        pos: usize,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let scc = self.ctx.rule_scc.get(canonical_rule).copied();
        if let Some(s) = scc {
            self.state.lr_growing.insert((s, pos));
        }
        let result = self.grow_left_recursive(canonical_rule, expr, pos, active);
        if let Some(s) = scc {
            self.state.lr_growing.remove(&(s, pos));
        }
        result
    }

    fn grow_left_recursive(
        &mut self,
        canonical_rule: &'a str,
        expr: &'a PegExpr,
        pos: usize,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let facet = self.memo_facet_hash(canonical_rule);
        let key = (canonical_rule, pos, facet);
        let mut best = ParseOutcome::failure(pos);
        self.memo_insert(key, best.clone())?;

        let min_step = self
            .ctx
            .lr_min_step
            .get(canonical_rule)
            .copied()
            .unwrap_or(1)
            .max(1);
        let max_growth = self
            .ctx
            .text
            .len()
            .saturating_sub(pos)
            .checked_div(min_step)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| {
                ParseError::new(
                    format!(
                        "internal parser error: rule '{canonical_rule}' left-recursive growth bound overflowed"
                    ),
                    pos,
                    self.ctx.text.len(),
                )
                .with_code("left_recursive_growth_bound_overflow")
            })?;
        let mut union = crate::parser_engine_state::ReadExtent::empty(pos);
        for _ in 0..=max_growth {
            self.prune_left_recursive_peer_memos(canonical_rule, pos);
            let result = self.parse_rule_body(canonical_rule, expr, pos, active)?;
            if let Some(p) = self.state.profile.as_mut() {
                p.record_body_run(canonical_rule, !result.is_success());
            }
            // Each seed-grow iteration examines its own bytes; the reusable
            // extent of the grown result is the union over all iterations.
            union.merge(self.state.last_read_extent);
            if outcome_end(&result) <= outcome_end(&best) || !result.is_success() {
                if self.cfg.track_read_extent {
                    self.state.read_memo.insert(key, union);
                }
                self.memo_insert(key, best.clone())?;
                return Ok(best);
            }
            best = result;
            self.memo_insert(key, best.clone())?;
        }

        Err(ParseError::new(
            format!(
                "internal parser error: rule '{canonical_rule}' left-recursive growth exceeded input bounds"
            ),
            pos,
            self.ctx.text.len(),
        ))
    }

    fn prune_left_recursive_peer_memos(&mut self, canonical_rule: &'a str, pos: usize) {
        let left_recursive = self.ctx.left_recursive_rules;
        if self.cfg.driver.is_none() {
            // No driver ⇒ every facet is 0, so the key is exact: targeted removes
            // cost O(|left_recursive|) instead of scanning the whole memo table.
            for rule in left_recursive {
                let rule_str = rule.as_str();
                if rule_str != canonical_rule {
                    self.state.rule_memo.remove(&(rule_str, pos, 0));
                }
            }
        } else {
            // A driver may report varying facets per evaluation, so a peer's
            // stored facet is not predictable. Drop every peer entry at `pos`
            // regardless of facet to keep left-recursive growth sound.
            self.state.rule_memo.retain(|(rule, entry_pos, _), _| {
                !(*entry_pos == pos && *rule != canonical_rule && left_recursive.contains(*rule))
            });
        }
    }

    /// Evaluate an owned PegExpr (from parameter bindings).
    fn parse_expr_owned(
        &mut self,
        expr: PegExpr,
        pos: usize,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        // Box to get a stable address, then evaluate.  Parameter expressions
        // are not in the grammar HashMap so we can't use &'a lifetime here.
        self.parse_expr(&expr, pos, active)
    }

    // ── Expression dispatch ────────────────────────────────────────────────

    /// Recursion-guarded entry to expression evaluation. Bounds the live
    /// recursive-descent depth so deeply-nested input fails with a
    /// `recursion_limit` error instead of overflowing the stack.
    fn parse_expr(
        &mut self,
        expr: &PegExpr,
        pos: usize,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        self.state.expr_depth += 1;
        if self.state.expr_depth > self.cfg.config_context.max_depth {
            self.state.expr_depth -= 1;
            return Err(ParseError::new(
                format!(
                    "parser recursion depth limit exceeded ({})",
                    self.cfg.config_context.max_depth
                ),
                pos,
                self.ctx.text.len(),
            )
            .with_code("recursion_limit"));
        }
        // Grow the native stack on demand (rustc's ensure_sufficient_stack
        // approach): depth stays bounded by the max_depth policy above, while
        // the stack is no longer a hidden second limit — callers need no
        // RUST_MIN_STACK tuning for deeply nested input.
        let result = stacker::maybe_grow(100 * 1024, 1024 * 1024, || {
            self.parse_expr_dispatch(expr, pos, active)
        });
        self.state.expr_depth -= 1;
        result
    }

    fn parse_expr_dispatch(
        &mut self,
        expr: &PegExpr,
        pos: usize,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        self.consume_expr_step(pos)?;
        match expr {
            PegExpr::Literal(value) => self.match_literal(pos, value),
            PegExpr::Dot => self.match_dot(pos),
            PegExpr::Regex(cr) => self.match_regex(pos, cr),
            PegExpr::CharClass(class) => self.match_char_class(pos, class),
            PegExpr::Recover { syncs } => self.match_recover(pos, syncs),
            PegExpr::Invalid(msg) => Err(ParseError::new(msg.clone(), pos, self.ctx.text.len())),
            PegExpr::And(node) => self.match_and(pos, node, active),
            PegExpr::Not(node) => self.match_not(pos, node, active),
            PegExpr::Cut => self.match_cut(pos),
            PegExpr::Ref(name) => self.parse_rule_inner(name, pos, active),
            PegExpr::Sequence(items) => self.match_sequence(pos, items, active),
            PegExpr::Choice(alternatives) => self.match_choice(pos, alternatives, active),
            PegExpr::Optional(node) => self.match_optional(pos, node, active),
            PegExpr::OneOrMore(node) => self.match_one_or_more(pos, node, active),
            PegExpr::ZeroOrMore(node) => self.match_zero_or_more(pos, node, active),
            PegExpr::Repeat { expr, min, max } => self.match_repeat(pos, expr, *min, *max, active),
            PegExpr::Precedence { operand, levels } => {
                self.match_precedence(pos, operand, levels, active)
            }
            PegExpr::SepOneOrMore { element, separator } => {
                self.match_sep_one_or_more(pos, element, separator, active)
            }
            PegExpr::Interspersed { element, separator } => {
                self.match_interspersed(pos, element, separator, active)
            }
            PegExpr::Named { name, expr: inner } => self.match_named(pos, name, inner, active),
            PegExpr::Expected {
                message,
                expr: inner,
            } => self.match_expected(pos, message, inner, active),
            PegExpr::NoTrivia(inner) => self.match_no_trivia(pos, inner, active),
            PegExpr::WithTrivia { spec, expr: inner } => {
                self.match_with_trivia(pos, spec, inner, active)
            }
            PegExpr::Newline => self.match_newline(pos),
            PegExpr::Indent => self.match_indent(pos),
            PegExpr::Dedent => self.match_dedent(pos),
            PegExpr::SemanticAction { name, expr: inner } => {
                self.match_semantic_action(pos, name, inner, active)
            }
            PegExpr::SemanticPredicate { name } => self.match_semantic_predicate(pos, name),
            PegExpr::SemanticGuard { name, expr: inner } => {
                self.match_semantic_guard(pos, name, inner, active)
            }
            PegExpr::Capture { label, expr: inner } => {
                self.match_capture(pos, label, inner, active)
            }
            PegExpr::Island {
                start,
                end,
                include_delims,
            } => self.match_island(pos, start, end, *include_delims),
            PegExpr::RawBlock {
                start,
                end,
                delim_kind,
            } => self.match_raw_block(pos, start, end, delim_kind),
            PegExpr::Eager(inner) => self.match_eager(pos, inner, active),
            PegExpr::LookBehind { expr, negative } => {
                self.match_look_behind(pos, expr, *negative, active)
            }
            PegExpr::Backref(name) => self.match_backref(pos, name),
            PegExpr::ImportedRef {
                grammar_name,
                rule_name,
            } => self.match_imported_ref(pos, grammar_name, rule_name),
            PegExpr::Parameter { name } => self.match_parameter(pos, name, active),
            PegExpr::Call { rule, args } => self.match_call(pos, rule, args, active),
            PegExpr::HardKeyword(kw) => self.match_hard_keyword(pos, kw),
            PegExpr::SoftKeyword(kw) => self.match_soft_keyword(pos, kw),
            PegExpr::GrammarScope {
                grammar_name,
                expr: inner,
            } => self.match_grammar_scope(pos, grammar_name, inner, active),
            PegExpr::TokenRef { kind, text } => self.match_token_ref(pos, kind, text),
        }
    }

    fn consume_expr_step(&mut self, pos: usize) -> Result<(), ParseError> {
        self.state.expr_steps = self.state.expr_steps.checked_add(1).ok_or_else(|| {
            ParseError::new(
                "parser expression step counter overflowed",
                pos,
                self.ctx.text.len(),
            )
            .with_code("step_counter_overflow")
        })?;
        let step = self.state.expr_steps;
        if step > self.cfg.config_context.max_steps {
            return Err(ParseError::new(
                format!(
                    "parser step budget exceeded after {} expression steps",
                    self.cfg.config_context.max_steps
                ),
                pos,
                self.ctx.text.len(),
            )
            .with_code("step_budget_exhausted"));
        }
        Ok(())
    }
}

// ── Driver sub-parse capability ────────────────────────────────────────────

/// The byte position an effect is anchored at, for `ParseView::pos`.
fn effect_pos(effect: &crate::driver::ParseEffect<'_>) -> usize {
    use crate::driver::ParseEffect::*;
    match effect {
        RuleEnter { pos, .. }
        | RuleExit { pos, .. }
        | RuleFail { pos, .. }
        | ChoiceEnter { pos, .. }
        | AltMatched { pos, .. }
        | Guard { pos, .. }
        | SemanticAction { pos, .. }
        | SemanticPredicate { pos, .. } => *pos,
        Failed { furthest, .. } => *furthest,
    }
}

/// The matched value and span an effect carries, for building the rich
/// `ParseView` (named bindings, items, matched text).
fn effect_value_span<'e>(
    effect: &'e crate::driver::ParseEffect<'_>,
) -> (Option<&'e ParseValue>, Option<(usize, usize)>) {
    use crate::driver::ParseEffect::*;
    match effect {
        RuleExit {
            value, pos, end, ..
        }
        | AltMatched {
            value, pos, end, ..
        }
        | Guard {
            value, pos, end, ..
        }
        | SemanticAction {
            value, pos, end, ..
        }
        | SemanticPredicate {
            value, pos, end, ..
        } => (Some(value), Some((*pos, *end))),
        _ => (None, None),
    }
}

/// The behavior/hook scalar arguments an effect carries.
fn effect_args<'e>(
    effect: &'e crate::driver::ParseEffect<'_>,
) -> &'e [crate::driver::GrammarScalar] {
    use crate::driver::ParseEffect::*;
    match effect {
        SemanticAction { args, .. } | SemanticPredicate { args, .. } => args,
        _ => &[],
    }
}

/// Whether an effect invokes a named host hook (arbitrary user logic, so a panic
/// is caught), as opposed to a pure lifecycle/control effect.
fn effect_is_value_hook(effect: &crate::driver::ParseEffect<'_>) -> bool {
    use crate::driver::ParseEffect::*;
    matches!(
        effect,
        Guard { .. } | SemanticAction { .. } | SemanticPredicate { .. }
    )
}

/// A combined backtracking checkpoint: host driver state plus the `backref`
/// capture store, so both are rolled back together when a branch fails.
struct Speculation {
    driver: crate::driver::DriverCheckpoint,
    captures: Option<std::collections::HashMap<String, String>>,
}

/// The matched sub-result a value hook produced, fed to `apply_verdict`.
struct MatchedHook {
    pos: usize,
    end: usize,
    value: ParseValue,
    cut: bool,
}

/// Runs an isolated sub-parse against the same compiled grammar. Borrows the
/// outer evaluator's immutable context and config; builds a *fresh* evaluator
/// (own memo, no driver) so it never aliases outer mutable state nor recurses
/// into the driver.
struct SubParseRunner<'a, 'c> {
    ctx: &'c ParseCtx<'a>,
    cfg: &'c EvalCfg<'a>,
    indentation_enabled: bool,
}

impl<'a, 'c> crate::driver::SubParseProvider for SubParseRunner<'a, 'c> {
    fn run_sub_parse(&self, rule: &str, pos: usize) -> crate::driver::SubParse {
        if pos > self.ctx.text.len() || !self.ctx.text.is_char_boundary(pos) {
            return crate::driver::SubParse {
                ok: false,
                consumed: 0,
                value: None,
            };
        }
        let mut evaluator = PegEvaluator::new(PegEvaluatorInit {
            ctx: self.ctx.reborrow(),
            cfg: self.cfg.fork_isolated(),
            indentation_enabled: self.indentation_enabled,
        });
        match evaluator.parse_rule(rule, pos) {
            Ok(ParseOutcome::Success {
                pos: end, value, ..
            }) => crate::driver::SubParse {
                ok: true,
                consumed: end.saturating_sub(pos),
                value: Some(value),
            },
            _ => crate::driver::SubParse {
                ok: false,
                consumed: 0,
                value: None,
            },
        }
    }
}

// ── Value-building helpers ─────────────────────────────────────────────────

/// Build a sequence result preserving position-only nodes (``Nil``).
fn sequence_value(pos: usize, mut values: Vec<ParseValue>) -> ParseOutcome {
    match values.len() {
        0 => ParseOutcome::success(pos, ParseValue::Nil),
        1 => match values.pop() {
            Some(value) => ParseOutcome::success(pos, value),
            None => ParseOutcome::success(pos, ParseValue::Nil),
        },
        _ => ParseOutcome::success(pos, ParseValue::Node("sequence".into(), Arc::new(values))),
    }
}

/// Build a repetition result.
fn repeat_value(pos: usize, values: Vec<ParseValue>, tag: &str) -> ParseOutcome {
    ParseOutcome::success(pos, ParseValue::Node(Arc::from(tag), Arc::new(values)))
}

#[derive(Debug)]
struct PegParserHelpers;

impl PegParserHelpers {
    fn maybe_span(value: ParseValue, start: usize, end: usize, as_spanned: bool) -> ParseValue {
        if as_spanned {
            ParseValue::SpannedValue {
                value: Arc::new(value),
                start,
                end,
            }
        } else {
            value
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::ParserConfigContext;

    #[test]
    fn expression_step_counter_overflow_is_diagnostic() {
        let grammar = Grammar::try_new("root <- 'x'").unwrap();
        let compiled = get_compiled(&grammar).unwrap();
        let config = ParserConfig::default().with_max_steps(usize::MAX);
        let mut evaluator = PegEvaluator::new(PegEvaluatorInit {
            ctx: ParseCtx::from_compiled(compiled.as_ref(), "x"),
            cfg: EvalCfg::new(
                resolve_invalid_rule_policy(&grammar, &config).unwrap(),
                resolve_trivia_skipper(None).unwrap(),
                memo_runtime_config(&config).unwrap(),
                ParserConfigContext::from_config(&config),
            ),
            indentation_enabled: resolve_indentation_enabled(None).unwrap(),
        });
        evaluator.state.expr_steps = usize::MAX;

        let error = evaluator.consume_expr_step(0).unwrap_err();

        assert_eq!(error.code.as_deref(), Some("step_counter_overflow"));
        assert!(error.message.contains("step counter overflowed"));
        assert_eq!(evaluator.state.expr_steps, usize::MAX);
    }

    #[test]
    fn parametric_self_recursive_call_resolves_caller_arg() {
        use crate::builder::{call, char_class, choice, lit, param, rule_ref, seq, GrammarBuilder};
        // wrap($p) <- '!' wrap($p) / $p ;  start <- wrap(atom) ;  atom <- [a-z]
        let grammar = GrammarBuilder::new()
            .start("start")
            .rule("start", call("wrap", vec![rule_ref("atom")]))
            .parametric(
                "wrap",
                vec!["p".to_string()],
                choice(vec![
                    seq(vec![lit("!"), call("wrap", vec![param("p")])]),
                    param("p"),
                ]),
            )
            .rule("atom", char_class("a-z").unwrap())
            .build();
        let config = ParserConfig::default();
        // Before resolve_arg_param the self-call bound `wrap`'s own `$p` to
        // itself and recursed forever; now `$p` carries the caller's `atom`.
        let out = PEGParser.parse(&grammar, "!!a", &config);
        assert!(
            out.is_ok(),
            "parametric self-recursion should parse, got {out:?}"
        );
    }
}
