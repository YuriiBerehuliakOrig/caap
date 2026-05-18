#![allow(dead_code)]

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use crate::analysis::analyze_grammar;
use crate::ast::AstNode;
use crate::behaviors::BehaviorEntry;
use crate::diagnostics_utils::{compute_line_offsets, line_col};
use crate::error::ParseError;
use crate::expr::{PegExpr, RuleTextParser};
use crate::grammar::{Grammar, GrammarPatch};
use crate::invalid_rules::InvalidRulePolicy;
use crate::parser_diagnostics::parse_error_with_location;
use crate::parser_imports::hydrate_imports_from_registry;
use crate::recovery::{recover_parse, try_recover_parse, RecoveredParse, RecoveryConfig};
use crate::registry::GrammarRegistry;
use crate::semantic::{
    GrammarContext, ParserConfigContext, ParserStateContext, SemanticContext, SemanticRuntime,
};
use crate::signature::grammar_signature;
use crate::skip::{skip_strategy_from_config, BoxedSkipStrategy};
use crate::types::{
    CompletedEdit, CompletedPrefixParse, IncrementalEdit, ParseCache, ParseValue, ParserConfig,
    ParserOutputMode,
};
use regex::Regex as StdRegex;

#[derive(Default, Clone)]
pub struct PEGParser;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseOutput {
    Value(ParseValue),
    Ast(AstNode),
}

impl PEGParser {
    pub fn parse(
        &self,
        grammar: &Grammar,
        text: &str,
        config: &ParserConfig,
    ) -> Result<ParseValue, ParseError> {
        self.parse_with_semantic(grammar, text, config, None)
    }

    /// Parse with missing `ImportedRef`/`GrammarScope` aliases resolved from a registry.
    ///
    /// Inline imports already attached to the grammar take precedence.
    pub fn parse_with_registry(
        &self,
        grammar: &Grammar,
        text: &str,
        config: &ParserConfig,
        registry: &GrammarRegistry,
    ) -> Result<ParseValue, ParseError> {
        let hydrated = hydrate_imports_from_registry(grammar, registry)?;
        self.parse(&hydrated, text, config)
    }

    pub fn parse_output(
        &self,
        grammar: &Grammar,
        text: &str,
        config: &ParserConfig,
    ) -> Result<ParseOutput, ParseError> {
        match config.output_mode {
            ParserOutputMode::Value => self.parse(grammar, text, config).map(ParseOutput::Value),
            ParserOutputMode::Ast => {
                crate::ast::parse_ast_with_max_steps(grammar, text, None, Some(config.max_steps))
                    .map(ParseOutput::Ast)
            }
        }
    }

    pub fn parse_output_with_registry(
        &self,
        grammar: &Grammar,
        text: &str,
        config: &ParserConfig,
        registry: &GrammarRegistry,
    ) -> Result<ParseOutput, ParseError> {
        let hydrated = hydrate_imports_from_registry(grammar, registry)?;
        self.parse_output(&hydrated, text, config)
    }

    /// Parse using a pre-produced token list, enabling `tok()` expressions.
    ///
    /// Tokens must be non-overlapping, sorted by `start`, and each token's
    /// `text` must equal `&input[token.start..token.end]`.
    pub fn parse_with_lex_tokens(
        &self,
        grammar: &Grammar,
        text: &str,
        config: &ParserConfig,
        tokens: Vec<crate::types::LexToken>,
    ) -> Result<ParseValue, ParseError> {
        let arc = std::sync::Arc::new(tokens);
        self.parse_with_lex_tokens_and_semantic(grammar, text, config, arc, None)
    }

    fn parse_with_lex_tokens_and_semantic(
        &self,
        grammar: &Grammar,
        text: &str,
        config: &ParserConfig,
        tokens: std::sync::Arc<Vec<crate::types::LexToken>>,
        semantic: Option<&dyn SemanticRuntime>,
    ) -> Result<ParseValue, ParseError> {
        if config.max_steps == 0 {
            return Err(ParseError::new("max_steps must be > 0", 0, text.len()));
        }
        let invalid_rule_policy = resolve_invalid_rule_policy(grammar, config)?;
        let compiled = CompiledGrammar::compile(grammar)?;
        let grammar_meta = grammar.metadata.get("__grammar__");
        let trivia = resolve_trivia_skipper(grammar_meta);
        let indentation_enabled = grammar_meta
            .and_then(|m| m.get("indentation"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let trace_cb = config.trace.as_ref().map(|h| h.0.clone());
        let mut evaluator = PegEvaluator::new(PegEvaluatorInit {
            ctx: ParseCtx::from_compiled(&compiled, text),
            cfg: EvalCfg::new(
                invalid_rule_policy,
                trivia,
                config.memo,
                semantic,
                ParserConfigContext::from_config(config),
            ),
            indentation_enabled,
        })
        .with_tokens(tokens)
        .with_trace(trace_cb);
        let outcome = evaluator.parse_rule(&grammar.start_rule, 0)?;
        let (value, end) = match outcome {
            ParseOutcome::Success { pos, value, .. } => (value, pos),
            ParseOutcome::Failure { .. } => {
                let expected = evaluator.state.diag.expected_at_furthest();
                let msg = if expected.is_empty() {
                    "parse failed".to_string()
                } else {
                    format!("expected: {}", expected.join(", "))
                };
                return Err(parse_error_with_location(
                    msg,
                    evaluator.state.diag.furthest,
                    text,
                    "parse_failed",
                ));
            }
        };
        if end != text.len() {
            let expected = evaluator.state.diag.expected_at_furthest();
            let msg = if expected.is_empty() {
                "did not consume complete input".to_string()
            } else {
                format!("expected: {}", expected.join(", "))
            };
            return Err(parse_error_with_location(
                msg,
                end,
                text,
                "incomplete_input",
            ));
        }
        Ok(PegParserHelpers::maybe_span(
            value,
            0,
            end,
            config.return_spans,
        ))
    }

    pub fn parse_with_semantic(
        &self,
        grammar: &Grammar,
        text: &str,
        config: &ParserConfig,
        semantic: Option<&dyn SemanticRuntime>,
    ) -> Result<ParseValue, ParseError> {
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

        if config.memo {
            // Prefer the cached analysis to avoid re-running full static analysis
            // on every parse call when the grammar hasn't changed.
            let errors: Vec<String> = match &grammar.state.analysis_state {
                Some(cached) => cached.analysis.errors.clone(),
                None => analyze_grammar(grammar).errors,
            };
            if !errors.is_empty() {
                return Err(ParseError::new(
                    format!("invalid grammar: {}", errors.join(", ")),
                    0,
                    text.len(),
                ));
            }
        }

        let invalid_rule_policy = resolve_invalid_rule_policy(grammar, config)?;
        let compiled = CompiledGrammar::compile(grammar)?;

        let grammar_meta = grammar.metadata.get("__grammar__");
        let trivia = resolve_trivia_skipper(grammar_meta);
        let indentation_enabled = grammar_meta
            .and_then(|m| m.get("indentation"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let trace_cb = config.trace.as_ref().map(|h| h.0.clone());
        let mut evaluator = PegEvaluator::new(PegEvaluatorInit {
            ctx: ParseCtx::from_compiled(&compiled, text),
            cfg: EvalCfg::new(
                invalid_rule_policy,
                trivia,
                config.memo,
                semantic,
                ParserConfigContext::from_config(config),
            ),
            indentation_enabled,
        })
        .with_trace(trace_cb);
        let outcome = evaluator.parse_rule(&grammar.start_rule, 0)?;
        let (value, end) = match outcome {
            ParseOutcome::Success { pos, value, .. } => (value, pos),
            ParseOutcome::Failure { .. } => {
                let expected = evaluator.state.diag.expected_at_furthest();
                let msg = if expected.is_empty() {
                    "parse failed".to_string()
                } else {
                    format!("expected: {}", expected.join(", "))
                };
                return Err(parse_error_with_location(
                    msg,
                    evaluator.state.diag.furthest,
                    text,
                    "parse_failed",
                ));
            }
        };

        if end != text.len() {
            let expected = evaluator.state.diag.expected_at_furthest();
            let msg = if expected.is_empty() {
                "did not consume complete input".to_string()
            } else {
                format!("expected: {}", expected.join(", "))
            };
            return Err(parse_error_with_location(
                msg,
                end,
                text,
                "incomplete_input",
            ));
        }

        Ok(PegParserHelpers::maybe_span(
            value,
            0,
            end,
            config.return_spans,
        ))
    }

    pub fn parse_prefix(
        &self,
        grammar: &Grammar,
        text: &str,
        start_pos: usize,
        start_rule: Option<&str>,
        config: &ParserConfig,
    ) -> CompletedPrefixParse {
        if start_pos > text.len() {
            return CompletedPrefixParse {
                value: None,
                consumed: 0,
                eof: false,
                errors: vec!["start_pos is past input end".to_string()],
            };
        }

        if config.max_steps == 0 {
            return CompletedPrefixParse {
                value: None,
                consumed: 0,
                eof: false,
                errors: vec!["max_steps must be > 0".to_string()],
            };
        }

        if text.len() - start_pos > config.max_steps {
            return CompletedPrefixParse {
                value: None,
                consumed: 0,
                eof: false,
                errors: vec![format!(
                    "input exceeds configured max_steps: {}",
                    config.max_steps
                )],
            };
        }

        let effective_rule = start_rule.unwrap_or(&grammar.start_rule);

        let invalid_rule_policy = match resolve_invalid_rule_policy(grammar, config) {
            Ok(policy) => policy,
            Err(err) => {
                return CompletedPrefixParse {
                    value: None,
                    consumed: 0,
                    eof: false,
                    errors: vec![err.message.to_string()],
                };
            }
        };
        let compiled = match CompiledGrammar::compile(grammar) {
            Ok(compiled) => compiled,
            Err(err) => {
                return CompletedPrefixParse {
                    value: None,
                    consumed: 0,
                    eof: false,
                    errors: vec![err.message.to_string()],
                };
            }
        };

        let grammar_meta = grammar.metadata.get("__grammar__");
        let trivia = resolve_trivia_skipper(grammar_meta);
        let indentation_enabled = grammar_meta
            .and_then(|m| m.get("indentation"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let trace_cb = config.trace.as_ref().map(|h| h.0.clone());
        let mut evaluator = PegEvaluator::new(PegEvaluatorInit {
            ctx: ParseCtx::from_compiled(&compiled, &text[start_pos..]),
            cfg: EvalCfg::new(
                invalid_rule_policy,
                trivia,
                config.memo,
                None,
                ParserConfigContext::from_config(config),
            ),
            indentation_enabled,
        })
        .with_trace(trace_cb);
        match evaluator.parse_rule(effective_rule, 0) {
            Err(err) => CompletedPrefixParse {
                value: None,
                consumed: 0,
                eof: false,
                errors: vec![err.message.to_string()],
            },
            Ok(ParseOutcome::Failure { .. }) => CompletedPrefixParse {
                value: None,
                consumed: 0,
                eof: false,
                errors: vec!["failed to parse prefix".to_string()],
            },
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

    pub fn parse_incremental_many(
        &self,
        grammar: &Grammar,
        text: &str,
        config: &ParserConfig,
        cache: &mut ParseCache,
    ) -> ParseValue {
        self.parse_incremental_many_with_result(grammar, text, config, cache)
            .unwrap_or(ParseValue::Nil)
    }

    pub(crate) fn parse_incremental_many_with_result(
        &self,
        grammar: &Grammar,
        text: &str,
        config: &ParserConfig,
        cache: &mut ParseCache,
    ) -> Result<ParseValue, ParseError> {
        if config.memo {
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

            // Prepare position-level seed from the previous run.
            // If grammar/runtime changed we discard the position cache.
            let pos_cache_valid = cache
                .pos_cache
                .as_ref()
                .is_some_and(|pc| pc.grammar_hash == gram_sig && pc.runtime_signature == run_sig);
            let seeded_pos_cache: Option<crate::types::PositionCache> = if pos_cache_valid {
                let mut pc = cache.pos_cache.take().unwrap();
                if pc.text != text {
                    // Text changed — compute edit steps and shift positions.
                    let edits = compute_boundary_edits(pc.text.as_str(), text);
                    pc.apply_edits(&edits);
                    pc.text = text.to_string();
                }
                Some(pc)
            } else {
                cache.pos_cache = None;
                None
            };

            // Run the parse with optional position seed.
            let invalid_rule_policy = resolve_invalid_rule_policy(grammar, config)?;
            let compiled = CompiledGrammar::compile(grammar)?;
            let grammar_meta = grammar.metadata.get("__grammar__");
            let trivia = resolve_trivia_skipper(grammar_meta);
            let indentation_enabled = grammar_meta
                .and_then(|m| m.get("indentation"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let trace_cb = config.trace.as_ref().map(|h| h.0.clone());
            let mut evaluator = PegEvaluator::new(PegEvaluatorInit {
                ctx: ParseCtx::from_compiled(&compiled, text),
                cfg: EvalCfg::new(
                    invalid_rule_policy,
                    trivia,
                    true,
                    None,
                    ParserConfigContext::from_config(config),
                ),
                indentation_enabled,
            })
            .with_trace(trace_cb);
            if let Some(ref seed) = seeded_pos_cache {
                evaluator = evaluator.with_pos_seed(seed);
            }

            let outcome = evaluator.parse_rule(&grammar.start_rule, 0)?;
            let (value, end) = match outcome {
                ParseOutcome::Success { pos, value, .. } => (value, pos),
                ParseOutcome::Failure { .. } => {
                    let expected = evaluator.state.diag.expected_at_furthest();
                    let msg = if expected.is_empty() {
                        "parse failed".to_string()
                    } else {
                        format!("expected: {}", expected.join(", "))
                    };
                    return Err(ParseError::new(
                        msg,
                        evaluator.state.diag.furthest,
                        text.len(),
                    ));
                }
            };

            // Check for unconsumed input (full parse only).
            let parsed = if end != text.len() {
                // Partial match: return what we got (same as parse()).
                value
            } else {
                value
            };

            // Export memo entries to the persistent position cache.
            let exported = evaluator.export_memo();
            let mut new_pc = seeded_pos_cache
                .unwrap_or_else(|| crate::types::PositionCache::new(text, gram_sig, run_sig));
            new_pc.grammar_hash = gram_sig;
            new_pc.runtime_signature = run_sig;
            new_pc.text = text.to_string();
            new_pc.absorb(exported);
            cache.pos_cache = Some(new_pc);

            // Append this result to the entries vec (one entry per unique text
            // version; the position cache handles inter-edit speedup separately).
            let wrapped = if config.return_spans {
                crate::parser::PegParserHelpers::maybe_span(parsed.clone(), 0, end, true)
            } else {
                parsed.clone()
            };
            cache.entries.push(crate::types::CachedResult {
                text_hash: hash,
                grammar_signature: gram_sig,
                runtime_signature: run_sig,
                output: wrapped.clone(),
            });
            return Ok(wrapped);
        }

        self.parse(grammar, text, config)
    }

    pub fn snapshot_edits_to_sequential(
        &self,
        base_text: &str,
        edits: &[IncrementalEdit],
    ) -> Vec<CompletedEdit> {
        if edits.is_empty() {
            return vec![];
        }

        let mut sorted_edits = edits.to_vec();
        sorted_edits.sort_by_key(|edit| (edit.start, edit.old_end));
        let mut result = Vec::with_capacity(sorted_edits.len());

        let base_len = base_text.len();
        let mut last_snapshot_end = 0usize;
        let mut offset_delta: isize = 0;

        for edit in sorted_edits {
            if edit.start > base_len || edit.old_end > base_len || edit.start > edit.old_end {
                panic!("invalid snapshot edit range");
            }
            if edit.start < last_snapshot_end {
                panic!("Overlapping snapshot edits");
            }

            let start = edit.start as isize + offset_delta;
            let end = edit.old_end as isize + offset_delta;
            if start < 0 || end < 0 {
                panic!("invalid snapshot edit range");
            }

            result.push(CompletedEdit {
                text: edit.replacement.clone(),
                span: (start as usize, end as usize),
            });

            let old_len = edit.old_end as isize - edit.start as isize;
            let new_len = edit.replacement.len() as isize;
            offset_delta += new_len - old_len;
            last_snapshot_end = edit.old_end;
        }

        result
    }

    pub fn apply_edits(base_text: &str, edits: &[CompletedEdit]) -> String {
        if edits.is_empty() {
            return base_text.to_string();
        }

        let mut out = String::new();
        let mut cursor = 0usize;
        for edit in edits {
            out.push_str(&base_text[cursor..edit.span.0]);
            out.push_str(&edit.text);
            cursor = edit.span.1;
        }
        out.push_str(&base_text[cursor..]);
        out
    }

    /// Batch error-recovery parse using sync markers (parity with Python `PEGParser.recover_parse`).
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

    pub fn clone_grammar(grammar: &Grammar, patch: Option<GrammarPatch>) -> Grammar {
        let mut copied = grammar.clone();
        copied.state.analysis_state = None;
        if let Some(patch) = patch {
            copied.text = patch.source;
            if let Some(rule) = patch.start_rule {
                copied.start_rule = rule;
            }
            copied.rules = crate::grammar::parse_rules_from_text(&copied.text);
        }
        copied.version = copied.version.saturating_add(1);
        copied
    }
}

fn runtime_signature(grammar: &Grammar, config: &ParserConfig) -> Result<u64, ParseError> {
    let policy = resolve_invalid_rule_policy(grammar, config)?;
    let mut hasher = DefaultHasher::new();
    policy.include_invalid_rules.hash(&mut hasher);
    config.return_spans.hash(&mut hasher);
    config.memo.hash(&mut hasher);
    config.max_steps.hash(&mut hasher);
    for prefix in policy.prefixes {
        prefix.hash(&mut hasher);
    }
    Ok(hasher.finish())
}

/// Precomputed first-character dispatch table for a Choice expression.
///
/// `map[c]` = 1-based indices of alternatives whose first char *could* be `c`.
/// `default` = 1-based indices of alternatives that must always be tried.
type ChoiceDispatch = (HashMap<char, Vec<usize>>, Vec<usize>);

struct CompiledGrammar {
    start_rule: String,
    metadata_keys: Vec<String>,
    rules: HashMap<String, PegExpr>,
    /// Ordered parameter names for each parametric rule.
    rule_params: HashMap<String, Vec<String>>,
    /// Compiled imported grammars keyed by alias.
    imports: HashMap<String, CompiledGrammar>,
    /// Rules that participate in left-recursive SCCs and need seed/grow parsing.
    left_recursive_rules: HashSet<String>,
    /// Minimum proven byte advance for one successful growth step per LR rule.
    lr_min_step: HashMap<String, usize>,
    /// Precomputed first-character dispatch tables for top-level Choice rules.
    /// Only populated for rules whose top-level expression is a Choice and where
    /// at least one alternative can be distinguished by its first character.
    rule_dispatch: HashMap<String, ChoiceDispatch>,
}

impl CompiledGrammar {
    fn compile(grammar: &Grammar) -> Result<Self, ParseError> {
        let analysis = match &grammar.state.analysis_state {
            Some(cached) => cached.analysis.clone(),
            None => analyze_grammar(grammar),
        };
        let left_recursive_rules: HashSet<String> = analysis
            .left_recursive_sccs
            .iter()
            .flatten()
            .cloned()
            .collect();
        let mut lr_min_step = HashMap::new();
        for scc in &analysis.left_recursive_sccs {
            let scc_set: HashSet<&str> = scc.iter().map(String::as_str).collect();
            for rule_name in scc {
                if let Some(rule) = grammar.get_rule(rule_name) {
                    lr_min_step.insert(
                        rule_name.clone(),
                        compute_lr_min_step_from_source(&rule.source, &scc_set).max(1),
                    );
                }
            }
        }

        let mut rules = HashMap::new();
        let mut rule_params = HashMap::new();
        let mut seen = HashSet::new();

        for rule in &grammar.rules {
            if !seen.insert(rule.name.clone()) {
                return Err(ParseError::new(
                    format!("duplicate grammar rule '{}'", rule.name),
                    0,
                    grammar.text.len(),
                ));
            }
            // Surface Invalid-placeholder errors at compile time.
            if let PegExpr::Invalid(msg) = &rule.expr {
                return Err(ParseError::new(msg.clone(), 0, grammar.text.len()));
            }
            rules.insert(rule.name.clone(), rule.expr.clone());
            if !rule.params.is_empty() {
                rule_params.insert(rule.name.clone(), rule.params.clone());
            }
        }

        if !rules.contains_key(&grammar.start_rule) {
            return Err(ParseError::new(
                format!("missing start rule '{}'", grammar.start_rule),
                0,
                grammar.text.len(),
            ));
        }

        // Build first-character dispatch tables for top-level Choice rules.
        // We use empty fixed/nullable maps here: this gives conservative (correct)
        // dispatch for literal/regex alternatives and falls back to linear scan
        // for Ref-based alternatives whose first chars aren't yet known.
        let empty_fixed: HashMap<String, Option<String>> = HashMap::new();
        let empty_nullable: HashSet<String> = HashSet::new();
        let mut rule_dispatch: HashMap<String, ChoiceDispatch> = HashMap::new();
        for (name, expr) in &rules {
            if let PegExpr::Choice(alts) = expr {
                if let Some(dispatch) =
                    compute_dispatch_from_choice_expr(alts, &empty_fixed, &empty_nullable)
                {
                    rule_dispatch.insert(name.clone(), dispatch);
                }
            }
        }

        let mut imports = HashMap::new();
        for (alias, imported) in &grammar.imports {
            imports.insert(alias.clone(), CompiledGrammar::compile(imported)?);
        }

        let mut metadata_keys: Vec<String> = grammar.metadata.keys().cloned().collect();
        metadata_keys.sort_unstable();

        Ok(Self {
            start_rule: grammar.start_rule.clone(),
            metadata_keys,
            rules,
            rule_params,
            imports,
            left_recursive_rules,
            lr_min_step,
            rule_dispatch,
        })
    }
}

/// Match Python: grammars use the default trivia skipper unless metadata opts out.
fn resolve_trivia_skipper(
    grammar_meta: Option<&std::collections::HashMap<String, serde_json::Value>>,
) -> Option<BoxedSkipStrategy> {
    let token = grammar_meta
        .and_then(|m| m.get("trivia"))
        .and_then(|v| v.as_str());
    skip_strategy_from_config(Some(token.unwrap_or("default")))
}

fn resolve_invalid_rule_policy(
    grammar: &Grammar,
    config: &ParserConfig,
) -> Result<InvalidRulePolicy, ParseError> {
    InvalidRulePolicy::resolve(
        grammar,
        config.include_invalid_rules,
        config.invalid_rule_prefixes.as_deref(),
    )
}

fn peg_live_trace_from_env() -> Option<PegLiveTrace> {
    std::env::var_os("CAAP_RUST_LIVE_TRACE")?;
    if let Ok(filter) = std::env::var("CAAP_RUST_LIVE_TRACE_FILTER") {
        let enabled = filter
            .split(',')
            .map(str::trim)
            .any(|needle| matches!(needle, "peg" | "parser"));
        if !enabled {
            return None;
        }
    }
    let interval = std::env::var("CAAP_RUST_PEG_TRACE_INTERVAL")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(100_000);
    Some(PegLiveTrace { interval })
}

fn peg_expr_kind(expr: &PegExpr) -> &'static str {
    match expr {
        PegExpr::Literal(_) => "literal",
        PegExpr::Dot => "dot",
        PegExpr::Regex(_) => "regex",
        PegExpr::Invalid(_) => "invalid",
        PegExpr::And(_) => "and",
        PegExpr::Not(_) => "not",
        PegExpr::Cut => "cut",
        PegExpr::Ref(_) => "ref",
        PegExpr::Sequence(_) => "sequence",
        PegExpr::Choice(_) => "choice",
        PegExpr::Optional(_) => "optional",
        PegExpr::OneOrMore(_) => "one-or-more",
        PegExpr::ZeroOrMore(_) => "zero-or-more",
        PegExpr::SepOneOrMore { .. } => "sep-one-or-more",
        PegExpr::Named { .. } => "named",
        PegExpr::Expected { .. } => "expected",
        PegExpr::NoTrivia(_) => "no-trivia",
        PegExpr::Newline => "newline",
        PegExpr::Indent => "indent",
        PegExpr::Dedent => "dedent",
        PegExpr::SemanticAction { .. } => "semantic-action",
        PegExpr::SemanticPredicate { .. } => "semantic-predicate",
        PegExpr::Behavior { .. } => "behavior",
        PegExpr::Capture { .. } => "capture",
        PegExpr::Island { .. } => "island",
        PegExpr::RawBlock { .. } => "raw-block",
        PegExpr::Eager(_) => "eager",
        PegExpr::ImportedRef { .. } => "imported-ref",
        PegExpr::Parameter { .. } => "parameter",
        PegExpr::Call { .. } => "call",
        PegExpr::HardKeyword(_) => "hard-keyword",
        PegExpr::SoftKeyword(_) => "soft-keyword",
        PegExpr::GrammarScope { .. } => "grammar-scope",
        PegExpr::TokenRef { .. } => "token-ref",
    }
}

#[derive(Debug, Clone)]
enum ParseOutcome {
    Success {
        pos: usize,
        value: ParseValue,
        cut: bool,
    },
    Failure {
        pos: usize,
        cut: bool,
    },
}

impl ParseOutcome {
    fn success(pos: usize, value: ParseValue) -> Self {
        Self::Success {
            pos,
            value,
            cut: false,
        }
    }

    fn success_with_cut(pos: usize) -> Self {
        Self::Success {
            pos,
            value: ParseValue::Nil,
            cut: true,
        }
    }

    fn failure(pos: usize) -> Self {
        Self::Failure { pos, cut: false }
    }

    fn failure_with_cut(pos: usize) -> Self {
        Self::Failure { pos, cut: true }
    }

    fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }
}

fn outcome_end(outcome: &ParseOutcome) -> usize {
    match outcome {
        ParseOutcome::Success { pos, .. } | ParseOutcome::Failure { pos, .. } => *pos,
    }
}

// ── Diagnostics tracking ───────────────────────────────────────────────────

struct DiagnosticsState {
    /// Furthest position reached during parsing (best error position).
    furthest: usize,
    /// Expected token labels at each position >= furthest.
    expected: HashMap<usize, HashSet<String>>,
}

impl DiagnosticsState {
    fn new() -> Self {
        Self {
            furthest: 0,
            expected: HashMap::new(),
        }
    }

    fn record_expected(&mut self, pos: usize, label: impl Into<String>) {
        if pos >= self.furthest {
            if pos > self.furthest {
                self.furthest = pos;
            }
            self.expected.entry(pos).or_default().insert(label.into());
        }
    }

    fn expected_at_furthest(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .expected
            .get(&self.furthest)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        v.sort_unstable();
        v
    }
}

fn format_tok_label(kind: &Option<String>, text: &Option<String>) -> String {
    match (kind, text) {
        (Some(k), Some(t)) => format!("tok({k},{t:?})"),
        (Some(k), None) => format!("tok({k})"),
        (None, Some(t)) => format!("tok({t:?})"),
        (None, None) => "tok(<any>)".to_string(),
    }
}

// ── Layout state ───────────────────────────────────────────────────────────

struct LayoutState {
    indent_stack: Vec<usize>,
    /// Number of open brackets (paren/bracket/brace); when > 0, newlines are
    /// treated as regular whitespace.
    bracket_depth: usize,
    at_line_start: bool,
    indentation_enabled: bool,
}

impl LayoutState {
    fn new(indentation_enabled: bool) -> Self {
        Self {
            indent_stack: vec![0],
            bracket_depth: 0,
            at_line_start: true,
            indentation_enabled,
        }
    }
}

// ── Evaluator sub-types ────────────────────────────────────────────────────

/// Immutable references to compiled grammar data for the duration of one parse.
struct ParseCtx<'a> {
    rules: &'a HashMap<String, PegExpr>,
    rule_params: &'a HashMap<String, Vec<String>>,
    text: &'a str,
    grammar_start: &'a str,
    metadata_keys: &'a [String],
    imports: &'a HashMap<String, CompiledGrammar>,
    left_recursive_rules: &'a HashSet<String>,
    lr_min_step: &'a HashMap<String, usize>,
    rule_dispatch: &'a HashMap<String, ChoiceDispatch>,
}

impl<'a> ParseCtx<'a> {
    fn from_compiled(compiled: &'a CompiledGrammar, text: &'a str) -> Self {
        Self {
            rules: &compiled.rules,
            rule_params: &compiled.rule_params,
            text,
            grammar_start: &compiled.start_rule,
            metadata_keys: &compiled.metadata_keys,
            imports: &compiled.imports,
            left_recursive_rules: &compiled.left_recursive_rules,
            lr_min_step: &compiled.lr_min_step,
            rule_dispatch: &compiled.rule_dispatch,
        }
    }
}

/// Parse configuration that is fixed at call time and never mutated during evaluation.
struct EvalCfg<'a> {
    use_memo: bool,
    invalid_rule_policy: InvalidRulePolicy,
    trivia: Option<BoxedSkipStrategy>,
    semantic: Option<&'a dyn SemanticRuntime>,
    pos_seed: Option<&'a crate::types::PositionCache>,
    lex_tokens: Option<std::sync::Arc<Vec<crate::types::LexToken>>>,
    trace: Option<crate::types::TraceCallback>,
    config_context: ParserConfigContext,
    live_trace: Option<PegLiveTrace>,
}

impl<'a> EvalCfg<'a> {
    fn new(
        invalid_rule_policy: InvalidRulePolicy,
        trivia: Option<BoxedSkipStrategy>,
        use_memo: bool,
        semantic: Option<&'a dyn SemanticRuntime>,
        config_context: ParserConfigContext,
    ) -> Self {
        Self {
            use_memo,
            invalid_rule_policy,
            trivia,
            semantic,
            pos_seed: None,
            lex_tokens: None,
            trace: None,
            config_context,
            live_trace: peg_live_trace_from_env(),
        }
    }
}

struct PegEvaluatorInit<'a> {
    ctx: ParseCtx<'a>,
    cfg: EvalCfg<'a>,
    indentation_enabled: bool,
}

/// Mutable state accumulated during a single parse run.
struct EvalState<'a> {
    /// Memo cache keyed by (&'a str, pos) — avoids String allocations per lookup.
    rule_memo: HashMap<(&'a str, usize), ParseOutcome>,
    diag: DiagnosticsState,
    layout: LayoutState,
    /// Lazily computed line offsets (only when an error needs line/col info).
    line_offsets: Option<Vec<usize>>,
    /// Stack of parameter bindings for parametric rule calls.
    params: Vec<HashMap<String, PegExpr>>,
    rule_stack: Vec<String>,
    trivia_on: bool,
    expr_steps: usize,
}

#[derive(Clone, Debug)]
struct PegLiveTrace {
    interval: usize,
}

// ── Evaluator ──────────────────────────────────────────────────────────────

struct PegEvaluator<'a> {
    ctx: ParseCtx<'a>,
    cfg: EvalCfg<'a>,
    state: EvalState<'a>,
}

impl<'a> PegEvaluator<'a> {
    fn new(init: PegEvaluatorInit<'a>) -> Self {
        Self {
            ctx: init.ctx,
            cfg: init.cfg,
            state: EvalState {
                rule_memo: HashMap::new(),
                diag: DiagnosticsState::new(),
                layout: LayoutState::new(init.indentation_enabled),
                line_offsets: None,
                params: Vec::new(),
                rule_stack: Vec::new(),
                trivia_on: true,
                expr_steps: 0,
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

    fn with_trace(mut self, trace: Option<crate::types::TraceCallback>) -> Self {
        self.cfg.trace = trace;
        self
    }

    /// Export successful memo entries as `(rule_name, start, end, value)` tuples.
    fn export_memo(&self) -> Vec<(String, usize, usize, ParseValue)> {
        self.state
            .rule_memo
            .iter()
            .filter_map(|((name, start), outcome)| {
                if let ParseOutcome::Success {
                    pos: end, value, ..
                } = outcome
                {
                    Some((name.to_string(), *start, *end, value.clone()))
                } else {
                    None
                }
            })
            .collect()
    }

    // ── Trivia helpers ─────────────────────────────────────────────────────

    /// Advance `pos` past trivia when trivia skipping is active.
    fn skip_trivia(&mut self, pos: usize) -> Result<usize, ParseError> {
        if !self.state.trivia_on {
            return Ok(pos);
        }
        match &self.cfg.trivia {
            Some(skipper) => match skipper.try_skip(self.ctx.text, pos) {
                Ok(p) => Ok(p),
                Err(err) => {
                    let offsets = self
                        .state
                        .line_offsets
                        .get_or_insert_with(|| compute_line_offsets(self.ctx.text));
                    let (line, col) = line_col(offsets, err.pos);
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
            },
            None => Ok(pos),
        }
    }

    fn semantic_context(
        &self,
        span: Option<(usize, usize)>,
        value: &ParseValue,
        args: Vec<crate::behaviors::GrammarScalar>,
        pos: usize,
    ) -> SemanticContext<'a> {
        let mut import_aliases: Vec<String> = self.ctx.imports.keys().cloned().collect();
        import_aliases.sort_unstable();
        let state = ParserStateContext {
            trivia_on: self.state.trivia_on,
            rule_stack: self.state.rule_stack.clone(),
            param_depth: self.state.params.len(),
            memo_entries: self.state.rule_memo.len(),
            indentation_enabled: self.state.layout.indentation_enabled,
            bracket_depth: self.state.layout.bracket_depth,
        };
        SemanticContext::new(
            self.ctx.text,
            span,
            value,
            args,
            self.ctx.grammar_start,
            GrammarContext::new(
                self.ctx.grammar_start,
                self.ctx.rules.len(),
                import_aliases,
                self.ctx.metadata_keys.to_vec(),
            ),
            self.cfg.config_context.clone(),
            state,
            self.state.rule_stack.clone(),
            pos,
        )
    }

    // ── Rule dispatch ──────────────────────────────────────────────────────

    fn parse_rule(&mut self, rule: &str, pos: usize) -> Result<ParseOutcome, ParseError> {
        self.parse_rule_unsafe(rule, pos, &mut HashSet::new())
    }

    fn parse_rule_unsafe(
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

        let key = (canonical_rule, pos);
        if self.cfg.use_memo {
            if let Some(cached) = self.state.rule_memo.get(&key) {
                return Ok(cached.clone());
            }
            // Check the persistent position cache seed (from a previous run).
            if let Some(seed) = self.cfg.pos_seed {
                if let Some(entry) = seed.get(canonical_rule, pos) {
                    let outcome = ParseOutcome::Success {
                        pos: entry.end,
                        value: entry.value.clone(),
                        cut: false,
                    };
                    self.state.rule_memo.insert(key, outcome.clone());
                    return Ok(outcome);
                }
            }
            if self.ctx.left_recursive_rules.contains(canonical_rule) && !active.contains(&key) {
                return self.run_left_recursive_growth(canonical_rule, expr, pos, active);
            }
            if active.contains(&key) {
                let failure = ParseOutcome::failure(pos);
                self.state.rule_memo.insert(key, failure.clone());
                return Ok(failure);
            }
        } else {
            if self.ctx.left_recursive_rules.contains(canonical_rule) {
                return Err(ParseError::new(
                    format!(
                        "memoization cannot be disabled for left-recursive rule '{canonical_rule}'"
                    ),
                    pos,
                    self.ctx.text.len(),
                ));
            }
            if active.contains(&key) {
                return Ok(ParseOutcome::failure(pos));
            }
        }

        let result = self.parse_rule_body(canonical_rule, expr, pos, active)?;
        if self.cfg.use_memo {
            self.state.rule_memo.insert(key, result.clone());
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
        self.state.rule_stack.push(canonical_rule.to_string());
        if let Some(cb) = &self.cfg.trace {
            cb(&crate::types::ParseEvent {
                kind: "enter",
                rule: canonical_rule.to_string(),
                pos,
            });
        }

        let rule_dispatch: &'a HashMap<String, ChoiceDispatch> = self.ctx.rule_dispatch;
        let result = if let PegExpr::Choice(alts) = expr {
            match rule_dispatch.get(canonical_rule) {
                Some(dispatch) => self.match_choice_dispatched(pos, alts, dispatch, active)?,
                None => self.match_choice(pos, alts, active)?,
            }
        } else {
            self.parse_expr(expr, pos, active)?
        };

        active.remove(&key);
        self.state.rule_stack.pop();
        if let Some(cb) = &self.cfg.trace {
            match &result {
                ParseOutcome::Success { pos: end_pos, .. } => {
                    cb(&crate::types::ParseEvent {
                        kind: "exit",
                        rule: canonical_rule.to_string(),
                        pos: *end_pos,
                    });
                }
                ParseOutcome::Failure { .. } => {
                    cb(&crate::types::ParseEvent {
                        kind: "fail",
                        rule: canonical_rule.to_string(),
                        pos,
                    });
                }
            }
        }
        Ok(result)
    }

    fn run_left_recursive_growth(
        &mut self,
        canonical_rule: &'a str,
        expr: &'a PegExpr,
        pos: usize,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let key = (canonical_rule, pos);
        let mut best = ParseOutcome::failure(pos);
        self.state.rule_memo.insert(key, best.clone());

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
            .saturating_add(1);
        for _ in 0..=max_growth {
            self.prune_left_recursive_peer_memos(canonical_rule, pos);
            let result = self.parse_rule_body(canonical_rule, expr, pos, active)?;
            if outcome_end(&result) <= outcome_end(&best) || !result.is_success() {
                self.state.rule_memo.insert(key, best.clone());
                return Ok(best);
            }
            best = result;
            self.state.rule_memo.insert(key, best.clone());
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
        self.state.rule_memo.retain(|(rule, start), _| {
            *start != pos || *rule == canonical_rule || !left_recursive.contains(*rule)
        });
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

    fn parse_expr(
        &mut self,
        expr: &PegExpr,
        pos: usize,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        self.trace_expr_step(expr, pos);
        match expr {
            PegExpr::Literal(value) => self.match_literal(pos, value),
            PegExpr::Dot => self.match_dot(pos),
            PegExpr::Regex(cr) => self.match_regex(pos, &cr.inner),
            PegExpr::Invalid(msg) => Err(ParseError::new(msg.clone(), pos, self.ctx.text.len())),
            PegExpr::And(node) => self.match_and(pos, node, active),
            PegExpr::Not(node) => self.match_not(pos, node, active),
            PegExpr::Cut => self.match_cut(pos),
            PegExpr::Ref(name) => self.parse_rule_unsafe(name, pos, active),
            PegExpr::Sequence(items) => self.match_sequence(pos, items, active),
            PegExpr::Choice(alternatives) => self.match_choice(pos, alternatives, active),
            PegExpr::Optional(node) => self.match_optional(pos, node, active),
            PegExpr::OneOrMore(node) => self.match_one_or_more(pos, node, active),
            PegExpr::ZeroOrMore(node) => self.match_zero_or_more(pos, node, active),
            PegExpr::SepOneOrMore { element, separator } => {
                self.match_sep_one_or_more(pos, element, separator, active)
            }
            PegExpr::Named { name, expr: inner } => self.match_named(pos, name, inner, active),
            PegExpr::Expected {
                message,
                expr: inner,
            } => self.match_expected(pos, message, inner, active),
            PegExpr::NoTrivia(inner) => self.match_no_trivia(pos, inner, active),
            PegExpr::Newline => self.match_newline(pos),
            PegExpr::Indent => self.match_indent(pos),
            PegExpr::Dedent => self.match_dedent(pos),
            PegExpr::SemanticAction { name, expr: inner } => {
                self.match_semantic_action(pos, name, inner, active)
            }
            PegExpr::SemanticPredicate { name } => self.match_semantic_predicate(pos, name),
            PegExpr::Behavior {
                entries,
                expr: inner,
            } => self.match_behavior(pos, entries, inner, active),
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

    fn trace_expr_step(&mut self, expr: &PegExpr, pos: usize) {
        let Some(trace) = &self.cfg.live_trace else {
            return;
        };
        self.state.expr_steps = self.state.expr_steps.saturating_add(1);
        let step = self.state.expr_steps;
        if step.is_multiple_of(trace.interval) {
            let rule = self
                .state
                .rule_stack
                .last()
                .map(String::as_str)
                .unwrap_or("-");
            eprintln!(
                "[caap-trace] peg.expr step={step} pos={pos} rule={rule} expr={}",
                peg_expr_kind(expr)
            );
        }
    }

    // ── Terminal matchers ──────────────────────────────────────────────────

    fn match_literal(&mut self, pos: usize, literal: &str) -> Result<ParseOutcome, ParseError> {
        let pos = self.skip_trivia(pos)?;
        if self.ctx.text[pos..].starts_with(literal) {
            let end = pos + literal.len();
            Ok(ParseOutcome::success(
                end,
                ParseValue::Text(literal.to_string()),
            ))
        } else {
            self.state
                .diag
                .record_expected(pos, format!("literal '{literal}'"));
            Ok(ParseOutcome::failure(pos))
        }
    }

    fn match_dot(&mut self, pos: usize) -> Result<ParseOutcome, ParseError> {
        let pos = self.skip_trivia(pos)?;
        if let Some(ch) = self.ctx.text[pos..].chars().next() {
            Ok(ParseOutcome::success(
                pos + ch.len_utf8(),
                ParseValue::Text(ch.to_string()),
            ))
        } else {
            self.state.diag.record_expected(pos, "any character");
            Ok(ParseOutcome::failure(pos))
        }
    }

    fn match_regex(&mut self, pos: usize, regex: &StdRegex) -> Result<ParseOutcome, ParseError> {
        let pos = self.skip_trivia(pos)?;
        let suffix = &self.ctx.text[pos..];
        match regex.find(suffix) {
            Some(m) if m.start() == 0 => {
                let end = pos + m.end();
                Ok(ParseOutcome::success(
                    end,
                    ParseValue::Text(suffix[..m.end()].to_string()),
                ))
            }
            _ => {
                self.state
                    .diag
                    .record_expected(pos, format!("/{}/", regex.as_str()));
                Ok(ParseOutcome::failure(pos))
            }
        }
    }

    // ── Predicate matchers ─────────────────────────────────────────────────

    fn match_and(
        &mut self,
        pos: usize,
        child: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        match self.parse_expr(child, pos, active)? {
            ParseOutcome::Success { .. } => Ok(ParseOutcome::success(pos, ParseValue::Nil)),
            ParseOutcome::Failure { .. } => Ok(ParseOutcome::failure(pos)),
        }
    }

    fn match_not(
        &mut self,
        pos: usize,
        child: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        match self.parse_expr(child, pos, active)? {
            ParseOutcome::Success { .. } => Ok(ParseOutcome::failure(pos)),
            ParseOutcome::Failure { .. } => Ok(ParseOutcome::success(pos, ParseValue::Nil)),
        }
    }

    fn match_cut(&self, pos: usize) -> Result<ParseOutcome, ParseError> {
        Ok(ParseOutcome::success_with_cut(pos))
    }

    // ── Repetition matchers ────────────────────────────────────────────────

    fn match_sequence(
        &mut self,
        pos: usize,
        items: &[PegExpr],
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let mut current_pos = pos;
        let mut values = Vec::new();
        let mut cut_seen = false;

        for item in items {
            match self.parse_expr(item, current_pos, active)? {
                ParseOutcome::Success {
                    pos: next,
                    value,
                    cut,
                } => {
                    current_pos = next;
                    if cut {
                        cut_seen = true;
                        continue;
                    }
                    values.push(value);
                }
                ParseOutcome::Failure { pos: next, cut: _ } => {
                    if cut_seen {
                        return Ok(ParseOutcome::failure_with_cut(next));
                    }
                    return Ok(ParseOutcome::failure(next));
                }
            }
        }

        Ok(sequence_value(current_pos, values))
    }

    fn match_choice(
        &mut self,
        pos: usize,
        alternatives: &[PegExpr],
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let mut furthest = pos;
        for alt in alternatives {
            let result = self.parse_expr(alt, pos, active)?;
            if result.is_success() {
                return Ok(result);
            }
            if let ParseOutcome::Failure { pos: fp, cut: true } = result {
                return Ok(ParseOutcome::failure_with_cut(fp));
            }
            if let ParseOutcome::Failure { pos: fp, .. } = result {
                if fp > furthest {
                    furthest = fp;
                }
            }
        }
        Ok(ParseOutcome::failure(furthest))
    }

    /// Choice evaluation using a precomputed first-character dispatch table.
    ///
    /// Peeks at the current character (after trivia) and limits the candidates
    /// to the alternatives registered for that character, unioned with the
    /// defaults (alternatives that are nullable or have unknown first chars).
    /// Falls back to `match_choice` when no dispatch entry exists for the char.
    fn match_choice_dispatched(
        &mut self,
        pos: usize,
        alternatives: &[PegExpr],
        dispatch: &ChoiceDispatch,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let (char_map, defaults) = dispatch;

        // Peek at the first character after trivia without consuming it.
        let peek_pos = self.skip_trivia(pos)?;
        let first_char = self.ctx.text[peek_pos..].chars().next();

        let indices: Vec<usize> = match first_char {
            None => {
                // At EOF — only defaults apply.
                defaults.clone()
            }
            Some(c) => match char_map.get(&c) {
                None => {
                    // No dispatch entry for this char → fall back to full linear scan.
                    return self.match_choice(pos, alternatives, active);
                }
                Some(char_indices) => {
                    // Union of char-specific and default indices, preserving order.
                    let mut merged = char_indices.clone();
                    for &d in defaults {
                        if !merged.contains(&d) {
                            merged.push(d);
                        }
                    }
                    merged.sort_unstable();
                    merged
                }
            },
        };

        let mut furthest = pos;
        for idx in indices {
            if idx == 0 || idx > alternatives.len() {
                continue;
            }
            let result = self.parse_expr(&alternatives[idx - 1], pos, active)?;
            if result.is_success() {
                return Ok(result);
            }
            if let ParseOutcome::Failure { pos: fp, cut: true } = result {
                return Ok(ParseOutcome::failure_with_cut(fp));
            }
            if let ParseOutcome::Failure { pos: fp, .. } = result {
                if fp > furthest {
                    furthest = fp;
                }
            }
        }
        Ok(ParseOutcome::failure(furthest))
    }

    fn match_optional(
        &mut self,
        pos: usize,
        child: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        match self.parse_expr(child, pos, active)? {
            ParseOutcome::Success { pos, value, .. } => Ok(ParseOutcome::success(pos, value)),
            ParseOutcome::Failure { .. } => Ok(ParseOutcome::success(pos, ParseValue::Nil)),
        }
    }

    fn match_one_or_more(
        &mut self,
        pos: usize,
        child: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let first = self.parse_expr(child, pos, active)?;
        let (mut cur, first_val) = match first {
            ParseOutcome::Failure { .. } => return Ok(first),
            ParseOutcome::Success { pos, value, .. } => (pos, value),
        };
        let mut values = vec![first_val];
        loop {
            match self.parse_expr(child, cur, active)? {
                ParseOutcome::Success { pos, value, .. } if pos > cur => {
                    values.push(value);
                    cur = pos;
                }
                _ => break,
            }
        }
        Ok(repeat_value(cur, values, "one_or_more"))
    }

    fn match_zero_or_more(
        &mut self,
        pos: usize,
        child: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let mut cur = pos;
        let mut values = Vec::new();
        loop {
            match self.parse_expr(child, cur, active)? {
                ParseOutcome::Success { pos, value, .. } if pos > cur => {
                    values.push(value);
                    cur = pos;
                }
                _ => break,
            }
        }
        if values.is_empty() {
            return Ok(ParseOutcome::success(cur, ParseValue::Nil));
        }
        Ok(repeat_value(cur, values, "zero_or_more"))
    }

    /// `element (separator element)*`
    fn match_sep_one_or_more(
        &mut self,
        pos: usize,
        element: &PegExpr,
        separator: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let first = self.parse_expr(element, pos, active)?;
        let (mut cur, first_val) = match first {
            ParseOutcome::Failure { .. } => return Ok(first),
            ParseOutcome::Success { pos, value, .. } => (pos, value),
        };
        let mut values = vec![first_val];
        loop {
            let sep_result = self.parse_expr(separator, cur, active)?;
            let sep_pos = match sep_result {
                ParseOutcome::Failure { .. } => break,
                ParseOutcome::Success { pos, .. } => pos,
            };
            match self.parse_expr(element, sep_pos, active)? {
                ParseOutcome::Failure { .. } => break,
                ParseOutcome::Success { pos, value, .. } => {
                    values.push(value);
                    cur = pos;
                }
            }
        }
        Ok(repeat_value(cur, values, "sep_one_or_more"))
    }

    // ── Value-binding matcher ──────────────────────────────────────────────

    fn match_named(
        &mut self,
        pos: usize,
        name: &str,
        expr: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let result = self.parse_expr(expr, pos, active)?;
        match result {
            ParseOutcome::Success { pos, value, cut } => Ok(ParseOutcome::Success {
                pos,
                value: ParseValue::Named(name.to_string(), Box::new(value)),
                cut,
            }),
            other => Ok(other),
        }
    }

    // ── Error-label override ───────────────────────────────────────────────

    fn match_expected(
        &mut self,
        pos: usize,
        message: &str,
        expr: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let before = self.state.diag.furthest;
        let result = self.parse_expr(expr, pos, active)?;
        if result.is_success() {
            return Ok(result);
        }
        let fail_pos = if self.state.diag.furthest > before {
            self.state.diag.furthest
        } else {
            pos
        };
        // Replace whatever was recorded with our label.
        self.state.diag.expected.remove(&fail_pos);
        self.state.diag.record_expected(fail_pos, message);
        Ok(ParseOutcome::failure(fail_pos))
    }

    // ── Trivia control ─────────────────────────────────────────────────────

    fn match_no_trivia(
        &mut self,
        pos: usize,
        inner: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let saved = self.state.trivia_on;
        self.state.trivia_on = false;
        let result = self.parse_expr(inner, pos, active);
        self.state.trivia_on = saved;
        result
    }

    // ── Layout-sensitive terminals ─────────────────────────────────────────

    fn match_newline(&mut self, pos: usize) -> Result<ParseOutcome, ParseError> {
        // When inside brackets or indentation is disabled, consume optional
        // surrounding whitespace and match \r?\n or \r.
        let cur = if self.state.layout.indentation_enabled && self.state.layout.bracket_depth == 0 {
            // Indent-aware: skip only horizontal whitespace before the newline.
            let mut c = pos;
            while c < self.ctx.text.len() && matches!(self.ctx.text.as_bytes()[c], b' ' | b'\t') {
                c += 1;
            }
            c
        } else {
            self.skip_trivia(pos)?
        };

        let end = match_newline_at(self.ctx.text, cur);
        if let Some(end) = end {
            self.state.layout.at_line_start = true;
            Ok(ParseOutcome::success(
                end,
                ParseValue::Text("\n".to_string()),
            ))
        } else {
            self.state.diag.record_expected(cur, "newline");
            Ok(ParseOutcome::failure(cur))
        }
    }

    fn match_indent(&mut self, pos: usize) -> Result<ParseOutcome, ParseError> {
        if !self.state.layout.indentation_enabled
            || self.state.layout.bracket_depth > 0
            || !self.state.layout.at_line_start
        {
            self.state.diag.record_expected(pos, "indent");
            return Ok(ParseOutcome::failure(pos));
        }
        let (width, end) = measure_indent(self.ctx.text, pos);
        let current = *self.state.layout.indent_stack.last().unwrap_or(&0);
        if width <= current {
            self.state.diag.record_expected(pos, "indent");
            return Ok(ParseOutcome::failure(pos));
        }
        self.state.layout.indent_stack.push(width);
        self.state.layout.at_line_start = false;
        Ok(ParseOutcome::success(end, ParseValue::Nil))
    }

    fn match_dedent(&mut self, pos: usize) -> Result<ParseOutcome, ParseError> {
        if !self.state.layout.indentation_enabled
            || self.state.layout.bracket_depth > 0
            || !self.state.layout.at_line_start
        {
            self.state.diag.record_expected(pos, "dedent");
            return Ok(ParseOutcome::failure(pos));
        }
        let (width, end) = measure_indent(self.ctx.text, pos);
        if self.state.layout.indent_stack.len() <= 1 {
            self.state.diag.record_expected(pos, "dedent");
            return Ok(ParseOutcome::failure(pos));
        }
        let target = self.state.layout.indent_stack[self.state.layout.indent_stack.len() - 2];
        if width != target {
            self.state.diag.record_expected(pos, "dedent");
            return Ok(ParseOutcome::failure(pos));
        }
        self.state.layout.indent_stack.pop();
        self.state.layout.at_line_start = false;
        Ok(ParseOutcome::success(end, ParseValue::Nil))
    }

    // ── Semantic action / predicate ────────────────────────────────────────

    fn match_semantic_action(
        &mut self,
        pos: usize,
        name: &str,
        expr: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let result = self.parse_expr(expr, pos, active)?;
        let ParseOutcome::Success {
            pos: end,
            value,
            cut,
        } = result
        else {
            return Ok(result);
        };
        let Some(runtime) = self.cfg.semantic else {
            return Err(ParseError::new(
                format!("semantic action '{name}' requires a semantic runtime"),
                pos,
                end,
            ));
        };
        let context = self.semantic_context(Some((pos, end)), &value, Vec::new(), pos);
        let new_value = runtime.invoke_action_with_context(name, value, &context);
        Ok(ParseOutcome::Success {
            pos: end,
            value: new_value,
            cut,
        })
    }

    fn match_semantic_predicate(
        &mut self,
        pos: usize,
        name: &str,
    ) -> Result<ParseOutcome, ParseError> {
        let Some(runtime) = self.cfg.semantic else {
            return Err(ParseError::new(
                format!("semantic predicate '{name}' requires a semantic runtime"),
                pos,
                pos,
            ));
        };
        let value = ParseValue::Nil;
        let context = self.semantic_context(None, &value, Vec::new(), pos);
        if runtime.invoke_predicate_with_context(name, &value, &context) {
            Ok(ParseOutcome::success(pos, ParseValue::Nil))
        } else {
            self.state
                .diag
                .record_expected(pos, format!("predicate '{name}'"));
            Ok(ParseOutcome::failure(pos))
        }
    }

    // ── Span capture ───────────────────────────────────────────────────────

    fn match_capture(
        &mut self,
        pos: usize,
        _label: &str,
        expr: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let result = self.parse_expr(expr, pos, active)?;
        if let ParseOutcome::Success {
            pos: end,
            value,
            cut,
        } = result
        {
            Ok(ParseOutcome::Success {
                pos: end,
                value: ParseValue::SpannedValue {
                    value: Box::new(value),
                    start: pos,
                    end,
                },
                cut,
            })
        } else {
            Ok(result)
        }
    }

    // ── Island ─────────────────────────────────────────────────────────────

    /// Match all text between `start` and `end` delimiter strings.
    /// Does NOT recurse (no nesting) — use `raw_block` for that.
    fn match_island(
        &mut self,
        pos: usize,
        start: &str,
        end: &str,
        include_delims: bool,
    ) -> Result<ParseOutcome, ParseError> {
        let cur = self.skip_trivia(pos)?;
        if !self.ctx.text[cur..].starts_with(start) {
            self.state.diag.record_expected(cur, format!("'{start}'"));
            return Ok(ParseOutcome::failure(cur));
        }
        let content_start = cur + start.len();
        let Some(end_offset) = self.ctx.text[content_start..].find(end) else {
            self.state
                .diag
                .record_expected(content_start, format!("closing '{end}'"));
            return Ok(ParseOutcome::failure(content_start));
        };
        let content_end = content_start + end_offset;
        let full_end = content_end + end.len();
        let value = if include_delims {
            ParseValue::Text(self.ctx.text[cur..full_end].to_string())
        } else {
            ParseValue::Text(self.ctx.text[content_start..content_end].to_string())
        };
        Ok(ParseOutcome::success(full_end, value))
    }

    // ── RawBlock ───────────────────────────────────────────────────────────

    /// Match nested balanced delimiters, yielding the inner content.
    /// Returns a `ParseValue::Text` of the content between the outermost delimiters.
    fn match_raw_block(
        &mut self,
        pos: usize,
        start: &str,
        end: &str,
        delim_kind: &str,
    ) -> Result<ParseOutcome, ParseError> {
        let cur = self.skip_trivia(pos)?;
        if !self.ctx.text[cur..].starts_with(start) {
            self.state.diag.record_expected(cur, format!("'{start}'"));
            return Ok(ParseOutcome::failure(cur));
        }
        let content_start = cur + start.len();
        let mut depth: usize = 1;
        let mut scan = content_start;
        let text = self.ctx.text;
        loop {
            let next_open = text[scan..].find(start).map(|o| scan + o);
            let next_close = text[scan..].find(end).map(|o| scan + o);
            match (next_open, next_close) {
                (_, None) => {
                    return Err(ParseError::new(
                        format!("unterminated raw block (kind={delim_kind})"),
                        cur,
                        text.len(),
                    ));
                }
                (Some(open), Some(close)) if open < close => {
                    depth += 1;
                    scan = open + start.len();
                }
                (_, Some(close)) => {
                    depth -= 1;
                    if depth == 0 {
                        let content = text[content_start..close].to_string();
                        let full_end = close + end.len();
                        return Ok(ParseOutcome::success(full_end, ParseValue::Text(content)));
                    }
                    scan = close + end.len();
                }
            }
        }
    }

    // ── Behavior ───────────────────────────────────────────────────────────

    fn match_behavior(
        &mut self,
        pos: usize,
        entries: &[BehaviorEntry],
        expr: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        use crate::behaviors::{
            trace_rule_name, DiagnosticBehavior, PredicateBehavior, TraceBehavior,
            TransformBehavior,
        };

        // Split behavior entries by kind.
        let mut diagnostics: Vec<&DiagnosticBehavior> = Vec::new();
        let mut predicates: Vec<&PredicateBehavior> = Vec::new();
        let mut traces: Vec<(usize, &TraceBehavior)> = Vec::new();
        let mut transforms: Vec<&TransformBehavior> = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            match entry {
                BehaviorEntry::Diagnostic(d) => diagnostics.push(d),
                BehaviorEntry::Predicate(p) => predicates.push(p),
                BehaviorEntry::Transform(t) => transforms.push(t),
                BehaviorEntry::Trace(t) => traces.push((index, t)),
            }
        }

        let result = self.parse_expr(expr, pos, active)?;
        match result {
            ParseOutcome::Failure { pos: fail_pos, cut } => {
                // On failure, record diagnostic labels as expected tokens.
                for diag in &diagnostics {
                    self.state
                        .diag
                        .record_expected(fail_pos, diag.label.clone());
                }
                Ok(ParseOutcome::Failure { pos: fail_pos, cut })
            }
            ParseOutcome::Success {
                pos: end,
                mut value,
                cut,
            } => {
                let span = Some((pos, end));
                if let Some(cb) = &self.cfg.trace {
                    for (index, trace) in &traces {
                        let rule = trace_rule_name(pos as u64, *index, trace);
                        cb(&crate::types::ParseEvent {
                            kind: "enter",
                            rule: rule.clone(),
                            pos,
                        });
                        cb(&crate::types::ParseEvent {
                            kind: "exit",
                            rule,
                            pos: end,
                        });
                    }
                }

                let needs_runtime = !predicates.is_empty() || !transforms.is_empty();
                if needs_runtime && self.cfg.semantic.is_none() {
                    return Err(ParseError::new(
                        "behavior predicates/transforms require a semantic runtime",
                        pos,
                        end,
                    ));
                }

                if let Some(runtime) = self.cfg.semantic {
                    for pred in &predicates {
                        let context = self.semantic_context(span, &value, pred.args.clone(), pos);
                        if !runtime.invoke_predicate_with_context(&pred.name, &value, &context) {
                            self.state.diag.record_expected(pos, pred.name.clone());
                            return Ok(ParseOutcome::failure(pos));
                        }
                    }
                    for transform in &transforms {
                        let context =
                            self.semantic_context(span, &value, transform.args.clone(), pos);
                        value =
                            runtime.invoke_action_with_context(&transform.name, value, &context);
                    }
                }

                Ok(ParseOutcome::Success {
                    pos: end,
                    value,
                    cut,
                })
            }
        }
    }

    // ── Eager ──────────────────────────────────────────────────────────────

    /// Like a regular expression, but on failure escalates to a fatal `ParseError`
    /// instead of returning a soft `ParseOutcome::Failure`.
    fn match_eager(
        &mut self,
        pos: usize,
        expr: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        match self.parse_expr(expr, pos, active)? {
            ok @ ParseOutcome::Success { .. } => Ok(ok),
            ParseOutcome::Failure { pos: fail_pos, .. } => {
                let expected = self.state.diag.expected_at_furthest();
                let msg = if expected.is_empty() {
                    "eager parse failure".to_string()
                } else {
                    format!("expected: {}", expected.join(", "))
                };
                Err(ParseError::new(msg, fail_pos, self.ctx.text.len()))
            }
        }
    }

    // ── ImportedRef ────────────────────────────────────────────────────────

    fn match_imported_ref(
        &mut self,
        pos: usize,
        grammar_name: &str,
        rule_name: &str,
    ) -> Result<ParseOutcome, ParseError> {
        let imported = match self.ctx.imports.get(grammar_name) {
            Some(c) => c,
            None => {
                return Err(ParseError::new(
                    format!("unknown grammar import '{grammar_name}' — add it to the grammar registry via Grammar::add_import()"),
                    pos,
                    self.ctx.text.len(),
                ));
            }
        };

        if !imported.rules.contains_key(rule_name) {
            return Err(ParseError::new(
                format!("rule '{rule_name}' not found in imported grammar '{grammar_name}'"),
                pos,
                self.ctx.text.len(),
            ));
        }

        // Run the target rule inside the imported grammar's compiled context.
        // We share text, trivia config, and indentation state but give the
        // sub-evaluator its own memo cache.
        let indentation_enabled = self.state.layout.indentation_enabled;
        let trivia_clone = self.cfg.trivia.clone();
        let lex_tokens = self.cfg.lex_tokens.clone();
        let mut sub = PegEvaluator::new(PegEvaluatorInit {
            ctx: ParseCtx::from_compiled(imported, self.ctx.text),
            cfg: EvalCfg::new(
                self.cfg.invalid_rule_policy.clone(),
                trivia_clone,
                self.cfg.use_memo,
                self.cfg.semantic,
                self.cfg.config_context.clone(),
            ),
            indentation_enabled,
        });
        sub.cfg.lex_tokens = lex_tokens;
        // Mirror trivia state from parent.
        sub.state.trivia_on = self.state.trivia_on;

        let mut active = HashSet::new();
        let outcome = sub.parse_rule_unsafe(rule_name, pos, &mut active)?;
        // Propagate furthest-failure diagnostics back to the parent.
        if sub.state.diag.furthest >= self.state.diag.furthest {
            self.state.diag.furthest = sub.state.diag.furthest;
            self.state.diag.expected = sub.state.diag.expected.clone();
        }
        Ok(outcome)
    }

    // ── Parameters ────────────────────────────────────────────────────────

    fn match_parameter(
        &mut self,
        pos: usize,
        name: &str,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        // Walk the params stack from top to bottom.
        for frame in self.state.params.iter().rev() {
            if let Some(expr) = frame.get(name) {
                let expr = expr.clone();
                return self.parse_expr_owned(expr, pos, active);
            }
        }
        Err(ParseError::new(
            format!("unbound parameter '${name}'"),
            pos,
            pos,
        ))
    }

    fn match_call(
        &mut self,
        pos: usize,
        rule: &str,
        args: &[PegExpr],
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        // Look up parameter names for this rule.
        let rule_params = self.ctx.rule_params;
        let param_names: Vec<String> = rule_params.get(rule).cloned().unwrap_or_default();

        // Build a parameter frame: name → owned arg PegExpr.
        let frame: HashMap<String, PegExpr> = param_names
            .iter()
            .zip(args.iter())
            .map(|(name, arg)| (name.clone(), arg.clone()))
            .collect();

        self.state.params.push(frame);
        let result = self.parse_rule_unsafe(rule, pos, active);
        self.state.params.pop();
        result
    }

    // ── Keyword terminals ─────────────────────────────────────────────────

    fn match_hard_keyword(&mut self, pos: usize, kw: &str) -> Result<ParseOutcome, ParseError> {
        let pos = self.skip_trivia(pos)?;
        let suffix = &self.ctx.text[pos..];
        if !suffix.starts_with(kw) {
            self.state
                .diag
                .record_expected(pos, format!("keyword '{kw}'"));
            return Ok(ParseOutcome::failure(pos));
        }
        let end = pos + kw.len();
        // Hard keyword: must not be followed by an identifier character.
        let next = self.ctx.text[end..].chars().next();
        if next
            .map(|c| c.is_alphanumeric() || c == '_')
            .unwrap_or(false)
        {
            self.state
                .diag
                .record_expected(pos, format!("keyword '{kw}'"));
            return Ok(ParseOutcome::failure(pos));
        }
        Ok(ParseOutcome::success(end, ParseValue::Text(kw.to_string())))
    }

    fn match_soft_keyword(&mut self, pos: usize, kw: &str) -> Result<ParseOutcome, ParseError> {
        // Soft keywords behave like HardKeyword (word-boundary check) since
        // their contextual nature is enforced by grammar structure, not a
        // runtime keyword list.  This is consistent with how most PEG parsers
        // handle contextual keywords.
        self.match_hard_keyword(pos, kw)
    }

    // ── Token-stream matching ──────────────────────────────────────────────

    fn match_token_ref(
        &mut self,
        pos: usize,
        kind: &Option<String>,
        text: &Option<String>,
    ) -> Result<ParseOutcome, ParseError> {
        let pos = self.skip_trivia(pos)?;
        let tokens = match &self.cfg.lex_tokens {
            Some(t) => t.clone(),
            None => {
                return Err(ParseError::new(
                    "tok() expression requires a pre-produced token list; use parse_with_lex_tokens()",
                    pos,
                    self.ctx.text.len(),
                ));
            }
        };
        let label = format_tok_label(kind, text);
        // Find the leftmost token whose span covers pos (token.start <= pos < token.end)
        // or starts exactly at pos.
        let idx = tokens.partition_point(|t| t.start < pos);
        // idx is the first token with start >= pos.  Check if the previous token covers pos.
        let tok = if idx > 0 && tokens[idx - 1].end > pos {
            &tokens[idx - 1]
        } else if idx < tokens.len() && tokens[idx].start == pos {
            &tokens[idx]
        } else {
            self.state.diag.record_expected(pos, label);
            return Ok(ParseOutcome::failure(pos));
        };
        if tok.start > pos {
            self.state.diag.record_expected(pos, label);
            return Ok(ParseOutcome::failure(pos));
        }
        if let Some(k) = kind {
            if &tok.kind != k {
                self.state.diag.record_expected(pos, label);
                return Ok(ParseOutcome::failure(pos));
            }
        }
        if let Some(t) = text {
            if &tok.text != t {
                self.state.diag.record_expected(pos, label);
                return Ok(ParseOutcome::failure(pos));
            }
        }
        Ok(ParseOutcome::success(
            tok.end,
            ParseValue::Text(tok.text.clone()),
        ))
    }

    // ── Grammar scope ──────────────────────────────────────────────────────

    fn match_grammar_scope(
        &mut self,
        pos: usize,
        grammar_name: &str,
        expr: &PegExpr,
        active: &mut HashSet<(&'a str, usize)>,
    ) -> Result<ParseOutcome, ParseError> {
        let imported = match self.ctx.imports.get(grammar_name) {
            Some(c) => c,
            None => {
                return Err(ParseError::new(
                    format!("unknown grammar import '{grammar_name}' — add it to the grammar registry via Grammar::add_import()"),
                    pos,
                    self.ctx.text.len(),
                ));
            }
        };

        // Temporarily swap in the imported grammar's rule table and evaluate
        // the inner expression.  We keep a reference to the imported rules
        // for the duration of the scope via a sub-evaluator.
        let indentation_enabled = self.state.layout.indentation_enabled;
        let trivia_clone = self.cfg.trivia.clone();
        let lex_tokens = self.cfg.lex_tokens.clone();
        let mut sub = PegEvaluator::new(PegEvaluatorInit {
            ctx: ParseCtx::from_compiled(imported, self.ctx.text),
            cfg: EvalCfg::new(
                self.cfg.invalid_rule_policy.clone(),
                trivia_clone,
                self.cfg.use_memo,
                self.cfg.semantic,
                self.cfg.config_context.clone(),
            ),
            indentation_enabled,
        });
        sub.state.trivia_on = self.state.trivia_on;
        sub.cfg.lex_tokens = lex_tokens;

        let mut sub_active = HashSet::new();
        let outcome = sub.parse_expr(expr, pos, &mut sub_active)?;
        if sub.state.diag.furthest >= self.state.diag.furthest {
            self.state.diag.furthest = sub.state.diag.furthest;
            self.state.diag.expected = sub.state.diag.expected.clone();
        }
        let _ = active; // parent active set unchanged — scope is its own call frame
        Ok(outcome)
    }
}

// ── Layout helpers ─────────────────────────────────────────────────────────

/// Match `\r\n`, `\r`, or `\n` at `pos`.  Returns the position after the match.
fn match_newline_at(text: &str, pos: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    if pos >= bytes.len() {
        return None;
    }
    match bytes[pos] {
        b'\r' => {
            if pos + 1 < bytes.len() && bytes[pos + 1] == b'\n' {
                Some(pos + 2)
            } else {
                Some(pos + 1)
            }
        }
        b'\n' => Some(pos + 1),
        _ => None,
    }
}

/// Return `(indent_width, end_pos)` where `indent_width` is the column of the
/// first non-whitespace character (tabs count as 4 spaces).
fn measure_indent(text: &str, pos: usize) -> (usize, usize) {
    let mut width = 0usize;
    let mut cur = pos;
    let bytes = text.as_bytes();
    while cur < bytes.len() {
        match bytes[cur] {
            b' ' => {
                width += 1;
                cur += 1;
            }
            b'\t' => {
                width += 4;
                cur += 1;
            }
            _ => break,
        }
    }
    (width, cur)
}

// ── Value-building helpers ─────────────────────────────────────────────────

/// Build a sequence result preserving position-only nodes (``Nil``).
fn sequence_value(pos: usize, values: Vec<ParseValue>) -> ParseOutcome {
    match values.len() {
        0 => ParseOutcome::success(pos, ParseValue::Nil),
        1 => ParseOutcome::success(pos, values.into_iter().next().unwrap()),
        _ => ParseOutcome::success(pos, ParseValue::Node("sequence".to_string(), values)),
    }
}

/// Build a repetition result.
fn repeat_value(pos: usize, values: Vec<ParseValue>, tag: &str) -> ParseOutcome {
    match values.len() {
        0 => ParseOutcome::success(pos, ParseValue::Nil),
        _ => ParseOutcome::success(pos, ParseValue::Node(tag.to_string(), values)),
    }
}

#[derive(Debug)]
struct PegParserHelpers;

impl PegParserHelpers {
    fn maybe_span(value: ParseValue, start: usize, end: usize, as_spanned: bool) -> ParseValue {
        if as_spanned {
            ParseValue::SpannedValue {
                value: Box::new(value),
                start,
                end,
            }
        } else {
            value
        }
    }
}

/// Return all parametric rule calls in `source` as `(rule_name, arg_count)` pairs.
pub fn extract_calls_from_source(source: &str) -> Vec<(String, usize)> {
    match RuleTextParser::parse(source) {
        Ok(expr) => {
            let mut calls = Vec::new();
            collect_calls_impl(&expr, &mut calls);
            calls
        }
        Err(_) => Vec::new(),
    }
}

/// Return all `Parameter` names referenced in `source`.
pub fn extract_params_used_from_source(source: &str) -> Vec<String> {
    match RuleTextParser::parse(source) {
        Ok(expr) => {
            let mut params = Vec::new();
            collect_params_impl(&expr, &mut params);
            params.sort_unstable();
            params.dedup();
            params
        }
        Err(_) => Vec::new(),
    }
}

/// Return `Some("cut")` or `Some("eager")` if `source` contains a `Cut`/`Eager`
/// that is not inside any `Choice` alternative, else `None`.
pub fn has_bare_commit_from_source(source: &str) -> Option<&'static str> {
    match RuleTextParser::parse(source) {
        Ok(expr) => bare_commit_kind(&expr, false),
        Err(_) => None,
    }
}

fn collect_calls_impl(expr: &PegExpr, out: &mut Vec<(String, usize)>) {
    match expr {
        PegExpr::Call { rule, args } => {
            out.push((rule.clone(), args.len()));
            for arg in args {
                collect_calls_impl(arg, out);
            }
        }
        PegExpr::Sequence(items) | PegExpr::Choice(items) => {
            for item in items {
                collect_calls_impl(item, out);
            }
        }
        PegExpr::And(n)
        | PegExpr::Not(n)
        | PegExpr::Optional(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::Eager(n)
        | PegExpr::NoTrivia(n) => collect_calls_impl(n, out),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::Behavior { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::GrammarScope { expr: n, .. } => collect_calls_impl(n, out),
        PegExpr::SepOneOrMore { element, separator } => {
            collect_calls_impl(element, out);
            collect_calls_impl(separator, out);
        }
        _ => {}
    }
}

fn collect_params_impl(expr: &PegExpr, out: &mut Vec<String>) {
    match expr {
        PegExpr::Parameter { name } => out.push(name.clone()),
        PegExpr::Sequence(items) | PegExpr::Choice(items) => {
            for item in items {
                collect_params_impl(item, out);
            }
        }
        PegExpr::And(n)
        | PegExpr::Not(n)
        | PegExpr::Optional(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::Eager(n)
        | PegExpr::NoTrivia(n) => collect_params_impl(n, out),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::Behavior { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::GrammarScope { expr: n, .. } => collect_params_impl(n, out),
        PegExpr::Call { args, .. } => {
            for arg in args {
                collect_params_impl(arg, out);
            }
        }
        PegExpr::SepOneOrMore { element, separator } => {
            collect_params_impl(element, out);
            collect_params_impl(separator, out);
        }
        _ => {}
    }
}

/// Walk `expr` and return the kind of the first Cut/Eager that is not inside
/// any Choice alternative.  `inside_choice` tracks whether we are currently
/// evaluating a branch of a Choice.
fn bare_commit_kind(expr: &PegExpr, inside_choice: bool) -> Option<&'static str> {
    match expr {
        PegExpr::Cut => {
            if !inside_choice {
                Some("cut")
            } else {
                None
            }
        }
        PegExpr::Eager(_) => {
            if !inside_choice {
                Some("eager")
            } else {
                None
            }
        }
        PegExpr::Choice(alts) => {
            // Cuts inside choice alternatives are fine.
            alts.iter().find_map(|a| bare_commit_kind(a, true))
        }
        PegExpr::Sequence(items) => items
            .iter()
            .find_map(|i| bare_commit_kind(i, inside_choice)),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::Behavior { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::GrammarScope { expr: n, .. } => bare_commit_kind(n, inside_choice),
        PegExpr::Optional(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::NoTrivia(n) => bare_commit_kind(n, inside_choice),
        // Lookaheads never trigger cut semantics outward.
        PegExpr::And(_) | PegExpr::Not(_) => None,
        PegExpr::SepOneOrMore { element, separator } => bare_commit_kind(element, inside_choice)
            .or_else(|| bare_commit_kind(separator, inside_choice)),
        _ => None,
    }
}

fn fnv_hash_64(input: &[u8]) -> u64 {
    const BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = BASIS;
    for byte in input {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Compute a minimal boundary edit list `(start, old_end, new_len)` from two
/// text versions using common-prefix / common-suffix detection.
///
/// Returns at most one edit covering the changed interior region.  If the texts
/// are identical, returns an empty vec.
fn compute_boundary_edits(old: &str, new: &str) -> Vec<(usize, usize, usize)> {
    if old == new {
        return vec![];
    }
    let old_b = old.as_bytes();
    let new_b = new.as_bytes();

    // Longest common prefix.
    let prefix = old_b
        .iter()
        .zip(new_b.iter())
        .take_while(|(a, b)| a == b)
        .count();

    // Longest common suffix (from the ends, not overlapping the prefix).
    let max_suffix = std::cmp::min(old_b.len() - prefix, new_b.len() - prefix);
    let suffix = old_b[old_b.len() - max_suffix..]
        .iter()
        .rev()
        .zip(new_b[new_b.len() - max_suffix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();

    let old_end = old_b.len() - suffix;
    let new_end = new_b.len() - suffix;
    vec![(prefix, old_end, new_end - prefix)]
}

// ── Public analysis helpers ────────────────────────────────────────────────

/// Parse a rule source string and return every rule name it references.
///
/// Returns an empty list when the source cannot be parsed.
pub fn extract_refs_from_source(source: &str) -> Vec<String> {
    match RuleTextParser::parse(source) {
        Ok(expr) => {
            let mut refs = Vec::new();
            collect_expr_refs(&expr, &mut refs);
            refs.sort_unstable();
            refs.dedup();
            refs
        }
        Err(_) => Vec::new(),
    }
}

/// Return `true` when `source` contains any `TokenRef` (`tok(...)`) expression.
pub fn has_token_ref_from_source(source: &str) -> bool {
    match RuleTextParser::parse(source) {
        Ok(expr) => expr_has_token_ref(&expr),
        Err(_) => false,
    }
}

fn expr_has_token_ref(expr: &PegExpr) -> bool {
    match expr {
        PegExpr::TokenRef { .. } => true,
        PegExpr::Sequence(items) | PegExpr::Choice(items) => items.iter().any(expr_has_token_ref),
        PegExpr::And(n)
        | PegExpr::Not(n)
        | PegExpr::Optional(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::Eager(n)
        | PegExpr::NoTrivia(n) => expr_has_token_ref(n),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::Behavior { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::GrammarScope { expr: n, .. } => expr_has_token_ref(n),
        PegExpr::SepOneOrMore { element, separator } => {
            expr_has_token_ref(element) || expr_has_token_ref(separator)
        }
        PegExpr::Call { args, .. } => args.iter().any(expr_has_token_ref),
        _ => false,
    }
}

/// Return `true` when `source` contains any char-level terminal (Literal, Regex, Dot, HardKeyword, SoftKeyword, Island, RawBlock).
pub fn has_char_terminal_from_source(source: &str) -> bool {
    match RuleTextParser::parse(source) {
        Ok(expr) => expr_has_char_terminal(&expr),
        Err(_) => false,
    }
}

fn expr_has_char_terminal(expr: &PegExpr) -> bool {
    match expr {
        PegExpr::Literal(_)
        | PegExpr::Regex(_)
        | PegExpr::Dot
        | PegExpr::HardKeyword(_)
        | PegExpr::SoftKeyword(_)
        | PegExpr::Island { .. }
        | PegExpr::RawBlock { .. }
        | PegExpr::Newline
        | PegExpr::Indent
        | PegExpr::Dedent => true,
        PegExpr::Sequence(items) | PegExpr::Choice(items) => {
            items.iter().any(expr_has_char_terminal)
        }
        PegExpr::And(n)
        | PegExpr::Not(n)
        | PegExpr::Optional(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::Eager(n)
        | PegExpr::NoTrivia(n) => expr_has_char_terminal(n),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::Behavior { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::GrammarScope { expr: n, .. } => expr_has_char_terminal(n),
        PegExpr::SepOneOrMore { element, separator } => {
            expr_has_char_terminal(element) || expr_has_char_terminal(separator)
        }
        PegExpr::Call { args, .. } => args.iter().any(expr_has_char_terminal),
        _ => false,
    }
}

/// Return `true` when the expression described by `source` can match without
/// consuming any input (structurally nullable, without following rule refs).
pub fn is_source_nullable(source: &str) -> bool {
    match RuleTextParser::parse(source) {
        Ok(expr) => is_expr_nullable(&expr),
        Err(_) => false,
    }
}

/// Return whether an expression is nullable without following `Ref` nodes.
fn is_expr_nullable(expr: &PegExpr) -> bool {
    match expr {
        PegExpr::Literal(_)
        | PegExpr::Regex(_)
        | PegExpr::Dot
        | PegExpr::Newline
        | PegExpr::Indent
        | PegExpr::Dedent
        | PegExpr::HardKeyword(_)
        | PegExpr::SoftKeyword(_)
        | PegExpr::Island { .. }
        | PegExpr::RawBlock { .. }
        | PegExpr::ImportedRef { .. }
        | PegExpr::GrammarScope { .. }
        | PegExpr::TokenRef { .. } => false,
        PegExpr::Optional(_) | PegExpr::ZeroOrMore(_) | PegExpr::Cut => true,
        PegExpr::Not(_) | PegExpr::SemanticPredicate { .. } => true,
        PegExpr::And(n) | PegExpr::NoTrivia(n) | PegExpr::Eager(n) => is_expr_nullable(n),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::Behavior { expr: n, .. }
        | PegExpr::Capture { expr: n, .. } => is_expr_nullable(n),
        PegExpr::OneOrMore(n) => is_expr_nullable(n),
        PegExpr::SepOneOrMore { element, .. } => is_expr_nullable(element),
        PegExpr::Sequence(items) => items.iter().all(is_expr_nullable),
        PegExpr::Choice(alts) => alts.iter().any(is_expr_nullable),
        PegExpr::Ref(_) | PegExpr::Parameter { .. } | PegExpr::Call { .. } => false,
        PegExpr::Invalid(_) => false,
    }
}

fn collect_expr_refs(expr: &PegExpr, out: &mut Vec<String>) {
    match expr {
        PegExpr::Ref(name) => out.push(name.clone()),
        PegExpr::Call { rule, args } => {
            out.push(rule.clone());
            for arg in args {
                collect_expr_refs(arg, out);
            }
        }
        PegExpr::Sequence(items) | PegExpr::Choice(items) => {
            for item in items {
                collect_expr_refs(item, out);
            }
        }
        PegExpr::And(n)
        | PegExpr::Not(n)
        | PegExpr::Optional(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::NoTrivia(n)
        | PegExpr::Eager(n) => collect_expr_refs(n, out),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::Behavior { expr: n, .. } => {
            collect_expr_refs(n, out);
        }
        PegExpr::SepOneOrMore { element, separator } => {
            collect_expr_refs(element, out);
            collect_expr_refs(separator, out);
        }
        PegExpr::SemanticAction { expr: n, .. } | PegExpr::Capture { expr: n, .. } => {
            collect_expr_refs(n, out);
        }
        PegExpr::SemanticPredicate { .. }
        | PegExpr::Parameter { .. }
        | PegExpr::ImportedRef { .. }
        | PegExpr::Island { .. }
        | PegExpr::RawBlock { .. }
        | PegExpr::HardKeyword(_)
        | PegExpr::SoftKeyword(_)
        | PegExpr::Literal(_)
        | PegExpr::Regex(_)
        | PegExpr::Dot
        | PegExpr::Cut
        | PegExpr::Newline
        | PegExpr::Indent
        | PegExpr::Dedent
        | PegExpr::TokenRef { .. } => {}
        PegExpr::GrammarScope { .. } => {}
        PegExpr::Invalid(_) => {}
    }
}

// ── Fixed-text analysis ────────────────────────────────────────────────────

fn expr_fixed_text_impl(
    expr: &PegExpr,
    fixed_rules: &HashMap<String, Option<String>>,
) -> Option<String> {
    match expr {
        PegExpr::Literal(s) => Some(s.clone()),
        PegExpr::HardKeyword(s) | PegExpr::SoftKeyword(s) => Some(s.clone()),
        PegExpr::Ref(name) => fixed_rules.get(name).and_then(|v| v.clone()),
        PegExpr::Sequence(items) => {
            let mut result = String::new();
            for item in items {
                result.push_str(&expr_fixed_text_impl(item, fixed_rules)?);
            }
            Some(result)
        }
        PegExpr::Choice(alts) if !alts.is_empty() => {
            let first = expr_fixed_text_impl(&alts[0], fixed_rules)?;
            for alt in &alts[1..] {
                if expr_fixed_text_impl(alt, fixed_rules)? != first {
                    return None;
                }
            }
            Some(first)
        }
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::Behavior { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::NoTrivia(n) => expr_fixed_text_impl(n, fixed_rules),
        PegExpr::Cut => Some(String::new()),
        _ => None,
    }
}

/// Return the exact text that `source` always matches given fixed-text map for other rules.
pub fn source_fixed_text(
    source: &str,
    fixed_rules: &HashMap<String, Option<String>>,
) -> Option<String> {
    RuleTextParser::parse(source)
        .ok()
        .and_then(|expr| expr_fixed_text_impl(&expr, fixed_rules))
}

// ── Nullable-with-rules analysis ───────────────────────────────────────────

fn is_expr_nullable_with_rules_impl(expr: &PegExpr, nullable_rules: &HashSet<String>) -> bool {
    match expr {
        PegExpr::Optional(_) | PegExpr::ZeroOrMore(_) | PegExpr::Cut => true,
        PegExpr::Not(_) | PegExpr::SemanticPredicate { .. } => true,
        PegExpr::And(n) | PegExpr::NoTrivia(n) | PegExpr::Eager(n) => {
            is_expr_nullable_with_rules_impl(n, nullable_rules)
        }
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::Behavior { expr: n, .. }
        | PegExpr::Capture { expr: n, .. } => is_expr_nullable_with_rules_impl(n, nullable_rules),
        PegExpr::OneOrMore(n) => is_expr_nullable_with_rules_impl(n, nullable_rules),
        PegExpr::SepOneOrMore { element, .. } => {
            is_expr_nullable_with_rules_impl(element, nullable_rules)
        }
        PegExpr::Sequence(items) => items
            .iter()
            .all(|i| is_expr_nullable_with_rules_impl(i, nullable_rules)),
        PegExpr::Choice(alts) => alts
            .iter()
            .any(|a| is_expr_nullable_with_rules_impl(a, nullable_rules)),
        PegExpr::Ref(name) => nullable_rules.contains(name),
        PegExpr::Literal(s) | PegExpr::HardKeyword(s) | PegExpr::SoftKeyword(s) => s.is_empty(),
        PegExpr::Regex(r) => r.inner.is_match(""),
        _ => false,
    }
}

/// Like `is_source_nullable` but follows `Ref` nodes via the supplied nullable rule set.
pub fn is_source_nullable_with_rules(source: &str, nullable_rules: &HashSet<String>) -> bool {
    match RuleTextParser::parse(source) {
        Ok(expr) => is_expr_nullable_with_rules_impl(&expr, nullable_rules),
        Err(_) => false,
    }
}

// ── Productive-with-rules analysis ─────────────────────────────────────────

fn is_expr_productive_impl(expr: &PegExpr, productive_rules: &HashMap<String, bool>) -> bool {
    match expr {
        PegExpr::Literal(_)
        | PegExpr::Regex(_)
        | PegExpr::Dot
        | PegExpr::HardKeyword(_)
        | PegExpr::SoftKeyword(_)
        | PegExpr::Island { .. }
        | PegExpr::RawBlock { .. }
        | PegExpr::Cut
        | PegExpr::Not(_)
        | PegExpr::Newline
        | PegExpr::Indent
        | PegExpr::Dedent
        | PegExpr::ImportedRef { .. }
        | PegExpr::GrammarScope { .. }
        | PegExpr::Parameter { .. }
        | PegExpr::SemanticPredicate { .. }
        | PegExpr::TokenRef { .. } => true,
        PegExpr::Ref(name) => productive_rules.get(name).copied().unwrap_or(false),
        PegExpr::Sequence(items) => items
            .iter()
            .all(|i| is_expr_productive_impl(i, productive_rules)),
        PegExpr::Choice(alts) => alts
            .iter()
            .any(|a| is_expr_productive_impl(a, productive_rules)),
        PegExpr::Optional(_) | PegExpr::ZeroOrMore(_) => true,
        PegExpr::OneOrMore(n) | PegExpr::And(n) | PegExpr::Eager(n) | PegExpr::NoTrivia(n) => {
            is_expr_productive_impl(n, productive_rules)
        }
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::Behavior { expr: n, .. }
        | PegExpr::Capture { expr: n, .. } => is_expr_productive_impl(n, productive_rules),
        PegExpr::SepOneOrMore { element, .. } => is_expr_productive_impl(element, productive_rules),
        PegExpr::Call { args, .. } => args
            .iter()
            .all(|a| is_expr_productive_impl(a, productive_rules)),
        PegExpr::Invalid(_) => false,
    }
}

/// Return whether the rule body in `source` can match on at least some input.
pub fn is_source_productive_with_rules(
    source: &str,
    productive_rules: &HashMap<String, bool>,
) -> bool {
    match RuleTextParser::parse(source) {
        Ok(expr) => is_expr_productive_impl(&expr, productive_rules),
        // Unparseable source: optimistically assume productive; the real
        // compile error will be reported when the grammar is compiled.
        Err(_) => true,
    }
}

// ── Left-refs analysis ─────────────────────────────────────────────────────

/// Collect rule names reachable from the left edge of `expr`.
///
/// A ref is a "left ref" if it can be the first thing consumed — i.e., all
/// preceding parts in any enclosing Sequence are nullable.
fn collect_left_refs_from_expr(expr: &PegExpr, nullable: &HashSet<String>) -> HashSet<String> {
    match expr {
        PegExpr::Ref(name) => {
            let mut s = HashSet::new();
            s.insert(name.clone());
            s
        }
        PegExpr::Sequence(parts) => {
            let mut refs = HashSet::new();
            for part in parts {
                refs.extend(collect_left_refs_from_expr(part, nullable));
                if !is_expr_nullable_with_rules_impl(part, nullable) {
                    break;
                }
            }
            refs
        }
        PegExpr::Choice(alts) => {
            let mut refs = HashSet::new();
            for alt in alts {
                refs.extend(collect_left_refs_from_expr(alt, nullable));
            }
            refs
        }
        PegExpr::SepOneOrMore { element, .. } => collect_left_refs_from_expr(element, nullable),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::Behavior { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::And(n)
        | PegExpr::Not(n)
        | PegExpr::Optional(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::NoTrivia(n)
        | PegExpr::Eager(n) => collect_left_refs_from_expr(n, nullable),
        _ => HashSet::new(),
    }
}

/// Return the sorted list of rule names reachable from the left edge of `source`.
///
/// Only refs in `nullable_rules` may be "transparent" (not stopping the left-edge
/// walk) when they appear in a Sequence.
pub fn extract_left_refs_from_source(
    source: &str,
    nullable_rules: &HashSet<String>,
) -> Vec<String> {
    match RuleTextParser::parse(source) {
        Ok(expr) => {
            let mut refs: Vec<String> = collect_left_refs_from_expr(&expr, nullable_rules)
                .into_iter()
                .collect();
            refs.sort_unstable();
            refs
        }
        Err(_) => vec![],
    }
}

// ── LR min-step analysis ───────────────────────────────────────────────────

/// Walk `expr` looking for `Sequence([Ref(scc_member), Literal(sep), ...])` patterns.
/// Returns the maximum proven separator length across all such patterns (minimum 1).
fn rule_lr_min_step_impl(expr: &PegExpr, scc_set: &HashSet<&str>) -> usize {
    let mut min_step: usize = 1;
    let mut stack: Vec<&PegExpr> = vec![expr];
    while let Some(cur) = stack.pop() {
        match cur {
            PegExpr::Choice(alts) => {
                for alt in alts {
                    stack.push(alt);
                }
            }
            PegExpr::Sequence(parts) => {
                if let Some(step) = sequence_lr_min_step_impl(parts, scc_set) {
                    if step > min_step {
                        min_step = step;
                    }
                }
            }
            _ => {}
        }
    }
    min_step
}

fn sequence_lr_min_step_impl(parts: &[PegExpr], scc_set: &HashSet<&str>) -> Option<usize> {
    let PegExpr::Ref(name) = parts.first()? else {
        return None;
    };
    if !scc_set.contains(name.as_str()) {
        return None;
    }
    match parts.get(1)? {
        PegExpr::Literal(text) => Some(text.len().max(1)),
        _ => None,
    }
}

/// Return the minimum proven character advance per LR-growth iteration for rules in `scc_set`.
///
/// Walks the rule's body looking for `Sequence([Ref(lr_rule), Literal(sep), ...])` patterns.
/// Returns 1 (conservative) when no provable minimum can be found.
pub fn compute_lr_min_step_from_source(source: &str, scc_set: &HashSet<&str>) -> usize {
    match RuleTextParser::parse(source) {
        Ok(expr) => rule_lr_min_step_impl(&expr, scc_set),
        Err(_) => 1,
    }
}

// ── Nullable repetition trap analysis ──────────────────────────────────────

fn collect_nullable_repetitions_impl(
    expr: &PegExpr,
    nullable_rules: &HashSet<String>,
    out: &mut Vec<String>,
) {
    match expr {
        PegExpr::ZeroOrMore(n) => {
            if is_expr_nullable_with_rules_impl(n, nullable_rules) {
                out.push("ZeroOrMore(expr nullable)".to_string());
            }
            collect_nullable_repetitions_impl(n, nullable_rules, out);
        }
        PegExpr::OneOrMore(n) => {
            if is_expr_nullable_with_rules_impl(n, nullable_rules) {
                out.push("OneOrMore(expr nullable)".to_string());
            }
            collect_nullable_repetitions_impl(n, nullable_rules, out);
        }
        PegExpr::SepOneOrMore { element, separator } => {
            if is_expr_nullable_with_rules_impl(separator, nullable_rules) {
                out.push("SepOneOrMore(sep nullable)".to_string());
            }
            if is_expr_nullable_with_rules_impl(element, nullable_rules) {
                out.push("SepOneOrMore(expr nullable)".to_string());
            }
            collect_nullable_repetitions_impl(element, nullable_rules, out);
            collect_nullable_repetitions_impl(separator, nullable_rules, out);
        }
        PegExpr::Sequence(items) | PegExpr::Choice(items) => {
            for item in items {
                collect_nullable_repetitions_impl(item, nullable_rules, out);
            }
        }
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::Behavior { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::Optional(n)
        | PegExpr::And(n)
        | PegExpr::Not(n)
        | PegExpr::Eager(n)
        | PegExpr::NoTrivia(n)
        | PegExpr::GrammarScope { expr: n, .. } => {
            collect_nullable_repetitions_impl(n, nullable_rules, out);
        }
        _ => {}
    }
}

/// Return kind strings for each nullable repetition trap in `source`.
pub fn collect_nullable_repetitions_from_source(
    source: &str,
    nullable_rules: &HashSet<String>,
) -> Vec<String> {
    match RuleTextParser::parse(source) {
        Ok(expr) => {
            let mut out = Vec::new();
            collect_nullable_repetitions_impl(&expr, nullable_rules, &mut out);
            out
        }
        Err(_) => Vec::new(),
    }
}

// ── Dead choice alternative analysis ──────────────────────────────────────

fn choice_fixed_texts(
    alts: &[PegExpr],
    fixed_rules: &HashMap<String, Option<String>>,
) -> Vec<Option<String>> {
    alts.iter()
        .map(|a| expr_fixed_text_impl(a, fixed_rules))
        .collect()
}

fn collect_dead_choice_alts_impl(
    expr: &PegExpr,
    fixed_rules: &HashMap<String, Option<String>>,
    out: &mut Vec<(usize, usize)>,
) {
    if let PegExpr::Choice(alts) = expr {
        let fixed = choice_fixed_texts(alts, fixed_rules);
        let mut prior: HashMap<String, usize> = HashMap::new();
        for (idx, ft) in fixed.iter().enumerate() {
            if let Some(text) = ft {
                if let Some(&live_idx) = prior.get(text) {
                    out.push((idx + 1, live_idx + 1));
                } else {
                    prior.insert(text.clone(), idx);
                }
            }
        }
        for alt in alts {
            collect_dead_choice_alts_impl(alt, fixed_rules, out);
        }
        return;
    }
    visit_children_for_choice_analysis(expr, |child| {
        collect_dead_choice_alts_impl(child, fixed_rules, out)
    });
}

/// Return `(dead_index, live_index)` pairs (1-based) for dead choice alternatives.
pub fn collect_dead_choice_alts_from_source(
    source: &str,
    fixed_rules: &HashMap<String, Option<String>>,
) -> Vec<(usize, usize)> {
    match RuleTextParser::parse(source) {
        Ok(expr) => {
            let mut out = Vec::new();
            collect_dead_choice_alts_impl(&expr, fixed_rules, &mut out);
            out
        }
        Err(_) => Vec::new(),
    }
}

// ── Prefix-shadowed choice alternative analysis ────────────────────────────

fn collect_prefix_shadowed_alts_impl(
    expr: &PegExpr,
    fixed_rules: &HashMap<String, Option<String>>,
    out: &mut Vec<(usize, usize, String)>,
) {
    if let PegExpr::Choice(alts) = expr {
        let fixed = choice_fixed_texts(alts, fixed_rules);
        for (dead_idx, dead) in fixed.iter().enumerate() {
            let Some(dead_text) = dead else {
                continue;
            };
            for (live_idx, live) in fixed.iter().enumerate().take(dead_idx) {
                let Some(live_text) = live else {
                    continue;
                };
                if !live_text.is_empty()
                    && live_text.len() < dead_text.len()
                    && dead_text.starts_with(live_text.as_str())
                {
                    out.push((dead_idx + 1, live_idx + 1, live_text.clone()));
                    break;
                }
            }
        }
        for alt in alts {
            collect_prefix_shadowed_alts_impl(alt, fixed_rules, out);
        }
        return;
    }
    visit_children_for_choice_analysis(expr, |child| {
        collect_prefix_shadowed_alts_impl(child, fixed_rules, out)
    });
}

/// Return `(dead_index, live_index, prefix)` for prefix-shadowed choice alternatives.
pub fn collect_prefix_shadowed_alts_from_source(
    source: &str,
    fixed_rules: &HashMap<String, Option<String>>,
) -> Vec<(usize, usize, String)> {
    match RuleTextParser::parse(source) {
        Ok(expr) => {
            let mut out = Vec::new();
            collect_prefix_shadowed_alts_impl(&expr, fixed_rules, &mut out);
            out
        }
        Err(_) => Vec::new(),
    }
}

// ── Overlapping prefix analysis ────────────────────────────────────────────

fn collect_overlapping_prefixes_impl(
    expr: &PegExpr,
    fixed_rules: &HashMap<String, Option<String>>,
    out: &mut Vec<(usize, usize, String)>,
) {
    if let PegExpr::Choice(alts) = expr {
        let fixed = choice_fixed_texts(alts, fixed_rules);
        for (i, first) in fixed.iter().enumerate() {
            let Some(text1) = first else { continue };
            if text1.chars().count() < 2 {
                continue;
            }
            for (j, second) in fixed.iter().enumerate().skip(i + 1) {
                let Some(text2) = second else { continue };
                if text2.chars().count() < 2 {
                    continue;
                }
                let common: String = text1
                    .chars()
                    .zip(text2.chars())
                    .take_while(|(a, b)| a == b)
                    .map(|(a, _)| a)
                    .collect();
                if common.chars().count() >= 3 {
                    out.push((i + 1, j + 1, common));
                }
            }
        }
        for alt in alts {
            collect_overlapping_prefixes_impl(alt, fixed_rules, out);
        }
        return;
    }
    visit_children_for_choice_analysis(expr, |child| {
        collect_overlapping_prefixes_impl(child, fixed_rules, out)
    });
}

/// Return `(alt1, alt2, common_prefix)` for alternatives sharing ≥3 common chars.
pub fn collect_overlapping_prefixes_from_source(
    source: &str,
    fixed_rules: &HashMap<String, Option<String>>,
) -> Vec<(usize, usize, String)> {
    match RuleTextParser::parse(source) {
        Ok(expr) => {
            let mut out = Vec::new();
            collect_overlapping_prefixes_impl(&expr, fixed_rules, &mut out);
            out
        }
        Err(_) => Vec::new(),
    }
}

/// Shared recursive descent for choice-analysis walkers.
fn visit_children_for_choice_analysis(expr: &PegExpr, mut f: impl FnMut(&PegExpr)) {
    match expr {
        PegExpr::Sequence(items) => {
            for item in items {
                f(item);
            }
        }
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::Behavior { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::Optional(n)
        | PegExpr::And(n)
        | PegExpr::Not(n)
        | PegExpr::Eager(n)
        | PegExpr::NoTrivia(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::GrammarScope { expr: n, .. } => f(n),
        PegExpr::SepOneOrMore { element, separator } => {
            f(element);
            f(separator);
        }
        _ => {}
    }
}

// ── First-char dispatch analysis ───────────────────────────────────────────

/// Compute the set of chars that `expr` can start with.
/// Returns `None` when the set cannot be statically bounded (open set — e.g. Dot, Regex with complex syntax).
fn expr_first_chars_impl(
    expr: &PegExpr,
    fixed_rules: &HashMap<String, Option<String>>,
    nullable_rules: &HashSet<String>,
) -> Option<Vec<char>> {
    match expr {
        PegExpr::Literal(s) | PegExpr::HardKeyword(s) | PegExpr::SoftKeyword(s) => {
            if s.is_empty() {
                Some(vec![])
            } else {
                s.chars().next().map(|c| vec![c])
            }
        }
        PegExpr::Regex(r) => parse_regex_first_chars(&r.pattern),
        PegExpr::Ref(name) => {
            if let Some(Some(fixed)) = fixed_rules.get(name) {
                if fixed.is_empty() {
                    Some(vec![])
                } else {
                    fixed.chars().next().map(|c| vec![c])
                }
            } else {
                None
            }
        }
        PegExpr::Sequence(items) => {
            let mut chars: Vec<char> = Vec::new();
            for item in items {
                let item_chars = expr_first_chars_impl(item, fixed_rules, nullable_rules)?;
                chars.extend(item_chars);
                if !is_expr_nullable_with_rules_impl(item, nullable_rules) {
                    break;
                }
            }
            Some(chars)
        }
        PegExpr::Choice(alts) => {
            let mut chars: Vec<char> = Vec::new();
            for alt in alts {
                chars.extend(expr_first_chars_impl(alt, fixed_rules, nullable_rules)?);
            }
            Some(chars)
        }
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::Behavior { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::And(n)
        | PegExpr::Eager(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::Optional(n) => expr_first_chars_impl(n, fixed_rules, nullable_rules),
        PegExpr::SepOneOrMore { element, .. } => {
            expr_first_chars_impl(element, fixed_rules, nullable_rules)
        }
        _ => None,
    }
}

/// Build a first-char dispatch table for a top-level Choice expression in `source`.
///
/// Returns `(dispatch: HashMap<char, Vec<usize>>, default: Vec<usize>)` where
/// indices are 1-based alternative positions. Returns `None` if the source is not
/// a simple Choice or if no dispatch table can be built.
pub fn compute_choice_dispatch_from_source(
    source: &str,
    fixed_rules: &HashMap<String, Option<String>>,
    nullable_rules: &HashSet<String>,
) -> Option<(HashMap<char, Vec<usize>>, Vec<usize>)> {
    let expr = RuleTextParser::parse(source).ok()?;
    let alts = match &expr {
        PegExpr::Choice(alts) => alts,
        _ => return None,
    };

    let mut dispatch: HashMap<char, Vec<usize>> = HashMap::new();
    let mut default: Vec<usize> = Vec::new();

    for (idx, alt) in alts.iter().enumerate() {
        if is_expr_nullable_with_rules_impl(alt, nullable_rules) {
            default.push(idx + 1);
            continue;
        }
        match expr_first_chars_impl(alt, fixed_rules, nullable_rules) {
            None => default.push(idx + 1),
            Some(ref v) if v.is_empty() => default.push(idx + 1),
            Some(chars) => {
                for c in chars {
                    dispatch.entry(c).or_default().push(idx + 1);
                }
            }
        }
    }

    if dispatch.is_empty() {
        None
    } else {
        Some((dispatch, default))
    }
}

/// Same logic as `compute_choice_dispatch_from_source` but works directly on a
/// pre-parsed `PegExpr` slice, avoiding a redundant source re-parse.
fn compute_dispatch_from_choice_expr(
    alts: &[PegExpr],
    fixed_rules: &HashMap<String, Option<String>>,
    nullable_rules: &HashSet<String>,
) -> Option<ChoiceDispatch> {
    let mut dispatch: HashMap<char, Vec<usize>> = HashMap::new();
    let mut default: Vec<usize> = Vec::new();

    for (idx, alt) in alts.iter().enumerate() {
        if is_expr_nullable_with_rules_impl(alt, nullable_rules) {
            default.push(idx + 1);
            continue;
        }
        match expr_first_chars_impl(alt, fixed_rules, nullable_rules) {
            None => default.push(idx + 1),
            Some(ref v) if v.is_empty() => default.push(idx + 1),
            Some(chars) => {
                for c in chars {
                    dispatch.entry(c).or_default().push(idx + 1);
                }
            }
        }
    }

    if dispatch.is_empty() {
        None
    } else {
        Some((dispatch, default))
    }
}

// Regex first-char heuristic (conservative — returns None for complex patterns).
fn parse_regex_first_chars(pattern: &str) -> Option<Vec<char>> {
    if pattern.is_empty() {
        return Some(vec![]);
    }
    if let Some(rest) = pattern.strip_prefix("-?") {
        let rest_chars = parse_regex_first_chars(rest)?;
        let mut chars = vec!['-'];
        chars.extend(rest_chars);
        return Some(chars);
    }
    if pattern.starts_with('[') {
        return parse_char_class_first_chars(pattern);
    }
    if pattern.starts_with("(?:") || pattern.starts_with("(?P") {
        return None;
    }
    if pattern.starts_with('\\') && pattern.len() >= 2 {
        let c = match pattern.chars().nth(1)? {
            't' => '\t',
            'n' => '\n',
            'r' => '\r',
            c => c,
        };
        return Some(vec![c]);
    }
    let first = pattern.chars().next()?;
    if "^$.()|*+?{".contains(first) {
        return None;
    }
    Some(vec![first])
}

fn parse_char_class_first_chars(pattern: &str) -> Option<Vec<char>> {
    // Find the closing ] accounting for escape sequences.
    let inner = pattern.strip_prefix('[')?;
    // Handle negated classes — open set.
    if inner.starts_with('^') {
        return None;
    }
    // Collect individual chars / ranges.
    let mut chars: Vec<char> = Vec::new();
    let mut it = inner.chars().peekable();
    while let Some(c) = it.next() {
        if c == ']' {
            break;
        }
        let decoded = if c == '\\' {
            match it.next()? {
                't' => '\t',
                'n' => '\n',
                'r' => '\r',
                c => c,
            }
        } else {
            c
        };
        // Check for range `a-z`
        if it.peek() == Some(&'-') {
            it.next(); // consume '-'
            if let Some(end_c) = it.next() {
                if end_c == ']' {
                    // Dash at end — treat as literal
                    chars.push(decoded);
                    chars.push('-');
                    break;
                }
                let end_decoded = if end_c == '\\' {
                    match it.next()? {
                        't' => '\t',
                        'n' => '\n',
                        'r' => '\r',
                        c => c,
                    }
                } else {
                    end_c
                };
                for code in (decoded as u32)..=(end_decoded as u32) {
                    if let Some(ch) = char::from_u32(code) {
                        chars.push(ch);
                    }
                }
            }
        } else {
            chars.push(decoded);
        }
    }
    if chars.is_empty() {
        None
    } else {
        Some(chars)
    }
}

// ── PegExpr → PegNode conversion ─────────────────────────────────────────

/// Convert a `PegExpr` to a `PegNode` for the common structural subset.
///
/// Returns `None` for expression kinds that have no `PegNode` equivalent
/// (e.g. `Indent`, `Dedent`, `Behavior`, `SemanticAction`, `HardKeyword`).
pub(crate) fn peg_expr_to_node(expr: &PegExpr) -> Option<crate::nodes::PegNode> {
    use crate::nodes::PegNode;
    match expr {
        PegExpr::Literal(s) | PegExpr::HardKeyword(s) | PegExpr::SoftKeyword(s) => {
            Some(PegNode::Literal(s.clone()))
        }
        PegExpr::Dot => None, // PegNode has no Dot variant
        PegExpr::Regex(cr) => Some(PegNode::Regex(cr.pattern.clone())),
        PegExpr::Ref(name) => Some(PegNode::Ref(name.clone())),
        PegExpr::Sequence(items) => {
            let nodes: Option<Vec<_>> = items.iter().map(peg_expr_to_node).collect();
            Some(PegNode::Sequence(nodes?))
        }
        PegExpr::Choice(alts) => {
            let nodes: Option<Vec<_>> = alts.iter().map(peg_expr_to_node).collect();
            Some(PegNode::Choice(nodes?))
        }
        PegExpr::Optional(inner) => Some(PegNode::Optional(Box::new(peg_expr_to_node(inner)?))),
        PegExpr::ZeroOrMore(inner) => Some(PegNode::ZeroOrMore(Box::new(peg_expr_to_node(inner)?))),
        PegExpr::OneOrMore(inner) => Some(PegNode::OneOrMore(Box::new(peg_expr_to_node(inner)?))),
        PegExpr::And(inner) => Some(PegNode::And(Box::new(peg_expr_to_node(inner)?))),
        PegExpr::Not(inner) => Some(PegNode::Not(Box::new(peg_expr_to_node(inner)?))),
        // PegExpr::Cut is a unit variant with no subexpression;
        // PegNode::Cut wraps an inner expr — no direct equivalent.
        PegExpr::Cut => None,
        PegExpr::Eager(inner) => Some(PegNode::Eager(Box::new(peg_expr_to_node(inner)?))),
        PegExpr::Named { name, expr: inner } => Some(PegNode::Named {
            name: name.clone(),
            node: Box::new(peg_expr_to_node(inner)?),
        }),
        PegExpr::Capture { label, expr: inner } => Some(PegNode::Capture(
            label.clone(),
            Box::new(peg_expr_to_node(inner)?),
        )),
        PegExpr::SepOneOrMore { element, separator } => Some(PegNode::SepOneOrMore {
            separator: Box::new(peg_expr_to_node(separator)?),
            element: Box::new(peg_expr_to_node(element)?),
        }),
        _ => None,
    }
}

/// Parse a PEG rule source string into a `PegNode` tree.
///
/// Returns `None` when the source is syntactically invalid or contains
/// expression kinds that cannot be represented as `PegNode` (e.g. behaviours,
/// indent/dedent, semantic actions).
pub fn parse_source_to_node(source: &str) -> Option<crate::nodes::PegNode> {
    let expr = RuleTextParser::parse(source).ok()?;
    peg_expr_to_node(&expr)
}
