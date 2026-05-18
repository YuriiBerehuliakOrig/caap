#![warn(clippy::all)]
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

pub mod analysis;
pub mod ast;
pub mod behaviors;
pub mod cache_runtime;
pub mod cache_transforms;
pub mod compile;
pub mod diagnostics;
pub mod diagnostics_utils;
pub mod error;
pub mod expr;
pub mod grammar;
pub mod graph_ops;
pub mod hash;
pub mod incremental_edits;
pub mod incremental_refresh;
pub mod invalid_rules;
pub mod lru_dict;
pub mod mutation;
pub mod nodes;
pub mod optimize;
pub mod parse_limits;
pub mod parser;
pub mod parser_diagnostics;
pub mod parser_imports;
pub mod pipeline;
pub mod recovery;
pub mod registry;
pub mod semantic;
pub mod sexpr;
pub mod signature;
pub mod skip;
pub mod spec_compiler;
pub mod transaction;
pub mod transaction_stack;
pub mod types;
pub mod validation;
pub mod values;

// ── Re-exports ─────────────────────────────────────────────────────────────

pub use crate::analysis::{
    analyze_and_store, analyze_cached_grammar, analyze_grammar, compute_nullable_rules,
    grammar_to_dot, GrammarAnalysis, GrammarAnalysisState, ParamArityMismatch, RuleIssueSummary,
    RuleScanSummary,
};
pub use crate::ast::{
    parse_ast, parse_ast_with_max_steps, walk as walk_ast, AstCapture, AstNode, AstSpan, Source,
};
pub use crate::behaviors::{
    BehaviorEntry, DiagnosticBehavior, GrammarScalar, PredicateBehavior, TraceBehavior,
    TraceBehaviorKind, TransformBehavior,
};
pub use crate::cache_runtime::{
    inline_elem_memo_get, inline_elem_memo_put, BoundedMap, CacheRuntimeOps, MemoEntry,
    NodeMemoKey, NodeMemoOps, NodeMemoState, OverlayMemo, RuleMemoKey, RuleMemoState,
    NODE_MEMO_AUTODISABLE_HIT_RATIO_DENOM, NODE_MEMO_AUTODISABLE_MISS_THRESHOLD, SKIP_MEMO_LIMIT,
};
pub use crate::cache_transforms::{
    build_parse_cache_seed, ensure_cache_compatible, persist_position_cache,
    resolve_incremental_cache_tables, validate_cache_provenance_metadata, CacheCompatError,
    CacheTables, FilenameUpdate, PayloadProjection, UnmappablePayloadError,
};
pub use crate::compile::{compile_grammar, project_value, GrammarCompilation, NodeSpec};
pub use crate::diagnostics::{
    interpret_parser_diagnostics, peg_error_to_diagnostic, peg_error_to_diagnostic_with_source,
    Diagnostic, ParserDiagnosticsSnapshot, RuleVisitStat, SourceLocator, SourcePoint, SourceRange,
    SourceSpan,
};
pub use crate::diagnostics_utils::{
    compute_line_offsets, describe_found, expected_label, format_context, line_col,
    summarize_expected,
};
pub use crate::error::{ParseError, ParseSpan};
pub use crate::expr::{peg_expr_to_source, CompiledRegex, PegExpr};
pub use crate::grammar::{CloneGrammar, Grammar, GrammarPatch, GrammarRule};
pub use crate::graph_ops::{
    build_reverse_graph, collect_bidirectional_region, collect_reachable_rules,
    collect_transitive_dependents, find_left_recursive_sccs, merge_graphs,
    refresh_left_recursive_sccs, strongly_connected_components,
};
pub use crate::hash::compiled_grammar_hash;
pub use crate::incremental_edits::{
    apply_incremental_steps_to_entry, apply_incremental_steps_to_interval,
    boundary_transplant_plan, cache_interval_zone, compile_incremental_edit_steps,
    normalize_incremental_edits, snapshot_edits_to_sequential as snapshot_edits_to_sequential_v2,
    transplant_cached_entry_with_boundary, transplant_interval_with_boundary, BoundaryTransplant,
    IncrementalEditError, IncrementalEditStep, SpanShift,
};
pub use crate::incremental_refresh::{
    collect_rule_issues_summary, collect_structural_changes, compute_lr_min_step,
    refresh_grammar_analysis_state, scan_rule_summary, solve_rule_property_incrementally,
};
pub use crate::invalid_rules::{
    normalize_invalid_rule_prefixes, normalize_metadata_invalid_rule_prefixes, InvalidRulePolicy,
    DEFAULT_INVALID_RULE_PREFIXES,
};
pub use crate::lru_dict::LruDict;
pub use crate::mutation::{
    add_rule, apply, diff_grammars, remove_rule, replace_rule, set_start_rule, GrammarDiff,
    GrammarMutation, MutationError, MutationKind, MutationOutcome,
};
pub use crate::nodes::{NodeKind, PegGrammarTree, PegNode};
pub use crate::optimize::{node_to_source, optimize_grammar, optimize_node};
pub use crate::parse_limits::resolve_memo_limits;
pub use crate::parser::{
    collect_dead_choice_alts_from_source, collect_nullable_repetitions_from_source,
    collect_overlapping_prefixes_from_source, collect_prefix_shadowed_alts_from_source,
    compute_choice_dispatch_from_source, compute_lr_min_step_from_source,
    extract_calls_from_source, extract_left_refs_from_source, extract_params_used_from_source,
    extract_refs_from_source, has_bare_commit_from_source, has_char_terminal_from_source,
    has_token_ref_from_source, is_source_nullable, is_source_nullable_with_rules,
    is_source_productive_with_rules, parse_source_to_node, source_fixed_text, PEGParser,
    ParseOutput,
};
pub use crate::parser_imports::extract_import_aliases_from_source;
pub use crate::pipeline::{
    compute_snapshot_edits, parse_pipeline, IncrementalPipeline, PipelineStage,
    PipelineStageResult, PipelineTextUpdate,
};
pub use crate::recovery::{
    collect_delete_candidate, collect_insert_candidates, collect_sync_markers,
    normalize_sync_regex, normalize_sync_tokens, recover_parse, try_recover_parse,
    validate_recovery_config, DefaultRecoveryStrategy, RecoveredParse, RecoveryConfig,
    RecoveryDeleteCandidate, RecoveryInsertCandidate, StreamingFormRecoveryStrategy,
};
pub use crate::registry::{
    from_text, load_json_grammar, GrammarDataSource, GrammarId, GrammarRegistry,
    JsonGrammarPayload, RegistryEntry, RegistryError, ScopedGrammarRegistry,
};
pub use crate::semantic::{
    ClosureSemanticRuntime, ContextualSemanticRuntime, GrammarContext, NullSemanticRuntime,
    ParserConfigContext, ParserStateContext, SemanticContext, SemanticRuntime,
};
pub use crate::sexpr::load_grammar_from_sexpr;
pub use crate::signature::{grammar_signature, node_signature, nodes_structurally_equal};
pub use crate::skip::{
    make_skipper, make_skipper_with_patterns, skip_strategy_from_config, DefaultSkipStrategy,
    LineCommentSkipStrategy, NoSkipStrategy, RegexSkipStrategy, SkipError, SkipStrategy,
    StatefulSkipStrategy, WhitespaceSkipStrategy, DEFAULT_BLOCK_COMMENTS, DEFAULT_LINE_COMMENTS,
    DEFAULT_WHITESPACE, NO_SKIPPER,
};
pub use crate::spec_compiler::{expr_to_source, SpecCompileError, SpecCompiler};
pub use crate::transaction::{GrammarTransaction, TransactionError, TransactionOp};
pub use crate::transaction_stack::GrammarTransactionStack;
pub use crate::types::{
    CachedResult, CompletedEdit, CompletedPrefixParse, IncrementalEdit, LexToken, MemoPolicy,
    ParseCache, ParseEvent, ParseValue, ParserConfig, ParserOutputMode, TraceCallback,
    TraceCallbackHolder,
};
pub use crate::validation::{
    validate_grammar, validate_grammar_with_label, validate_grammar_with_options, Severity,
    ValidationIssue, ValidationOptions, ValidationReport,
};
pub use crate::values::{
    contains_spanned, extract_span, strip_spans, unwrap_spanned, SequenceValueBuilder,
};

// ── Top-level convenience functions ───────────────────────────────────────

pub fn parse(
    text: &str,
    grammar: &Grammar,
    config: Option<ParserConfig>,
    return_spans: bool,
) -> Result<ParseValue, ParseError> {
    let effective_config = config
        .unwrap_or_default()
        .with_updates(return_spans, None, None);
    let parser = PEGParser;
    parser.parse(grammar, text, &effective_config)
}

pub fn parse_with_spans(
    text: &str,
    grammar: &Grammar,
    config: Option<ParserConfig>,
) -> Result<ParseValue, ParseError> {
    parse(text, grammar, config, true)
}

pub fn parse_with_registry(
    text: &str,
    grammar: &Grammar,
    registry: &GrammarRegistry,
    config: Option<ParserConfig>,
    return_spans: bool,
) -> Result<ParseValue, ParseError> {
    let effective_config = config
        .unwrap_or_default()
        .with_updates(return_spans, None, None);
    let parser = PEGParser;
    parser.parse_with_registry(grammar, text, &effective_config, registry)
}

pub fn parse_output(
    text: &str,
    grammar: &Grammar,
    config: Option<ParserConfig>,
) -> Result<ParseOutput, ParseError> {
    let effective_config = config.unwrap_or_default();
    let parser = PEGParser;
    parser.parse_output(grammar, text, &effective_config)
}

pub fn parse_with_lex_tokens(
    text: &str,
    grammar: &Grammar,
    tokens: Vec<LexToken>,
    config: Option<ParserConfig>,
    return_spans: bool,
) -> Result<ParseValue, ParseError> {
    let effective_config = config
        .unwrap_or_default()
        .with_updates(return_spans, None, None);
    let parser = PEGParser;
    parser.parse_with_lex_tokens(grammar, text, &effective_config, tokens)
}

pub fn parse_with_semantic(
    text: &str,
    grammar: &Grammar,
    config: Option<ParserConfig>,
    return_spans: bool,
    semantic: Option<&dyn SemanticRuntime>,
) -> Result<ParseValue, ParseError> {
    let effective_config = config
        .unwrap_or_default()
        .with_updates(return_spans, None, None);
    let parser = PEGParser;
    parser.parse_with_semantic(grammar, text, &effective_config, semantic)
}

pub fn parse_prefix(
    text: &str,
    grammar: &Grammar,
    start_rule: Option<&str>,
    start_pos: usize,
    config: Option<ParserConfig>,
    return_spans: bool,
) -> CompletedPrefixParse {
    let effective_config = config
        .unwrap_or_default()
        .with_updates(return_spans, None, None);
    let parser = PEGParser;
    parser.parse_prefix(grammar, text, start_pos, start_rule, &effective_config)
}

pub fn parse_prefix_with_spans(
    text: &str,
    grammar: &Grammar,
    start_rule: Option<&str>,
    start_pos: usize,
    config: Option<ParserConfig>,
) -> CompletedPrefixParse {
    parse_prefix(text, grammar, start_rule, start_pos, config, true)
}

pub fn parse_incremental_many(
    text: &str,
    grammar: &Grammar,
    config: ParserConfig,
    cache: &mut ParseCache,
) -> ParseValue {
    let parser = PEGParser;
    parser.parse_incremental_many(grammar, text, &config, cache)
}

pub fn parse_incremental_many_with_spans(
    text: &str,
    grammar: &Grammar,
    config: ParserConfig,
    cache: &mut ParseCache,
) -> ParseValue {
    parse_incremental_many(text, grammar, config.with_updates(true, None, None), cache)
}

pub fn snapshot_edits_to_sequential(
    base_text: &str,
    edits: &[IncrementalEdit],
) -> Vec<CompletedEdit> {
    let parser = PEGParser;
    parser.snapshot_edits_to_sequential(base_text, edits)
}

pub fn apply_edits(base_text: &str, edits: &[CompletedEdit]) -> String {
    PEGParser::apply_edits(base_text, edits)
}

pub fn clone_grammar(grammar: &Grammar, patch: Option<GrammarPatch>) -> Grammar {
    PEGParser::clone_grammar(grammar, patch)
}

/// Run a parse and return all trace events (rule enter / exit / fail).
///
/// Mirrors `peg/debug.py::trace_parse()`. Useful for debugging grammars and
/// building AST trees without writing a custom trace callback.
pub fn trace_parse(text: &str, grammar: &Grammar, config: Option<ParserConfig>) -> Vec<ParseEvent> {
    use std::sync::{Arc, Mutex};
    let events: Arc<Mutex<Vec<ParseEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let ev = events.clone();
    let trace_config = config
        .unwrap_or_default()
        .with_trace(move |e| ev.lock().unwrap().push(e.clone()));
    let _ = PEGParser.parse(grammar, text, &trace_config);
    Arc::try_unwrap(events)
        .ok()
        .and_then(|m| m.into_inner().ok())
        .unwrap_or_default()
}

/// Return the rule reference graph as `rule → set of referenced rules`.
///
/// Mirrors `peg/debug.py::rule_graph()`.
pub fn rule_graph(grammar: &Grammar) -> std::collections::HashMap<String, Vec<String>> {
    use crate::analysis::analyze_grammar;
    let analysis = analyze_grammar(grammar);
    analysis.refs
}

// ── Module-level tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_span_roundtrip() {
        let grammar = Grammar::new("raw <- .").with_start_rule("raw");
        let parsed = parse_with_spans("a", &grammar, None).expect("parse works");
        match parsed {
            ParseValue::SpannedValue { value, .. } => {
                assert!(matches!(*value, ParseValue::Text(_)))
            }
            _ => panic!("expected spanned value"),
        }
    }

    #[test]
    fn clone_and_mutate_grammar() {
        let mut base = Grammar::new("a <- [a]");
        let cloned = clone_grammar(
            &base,
            Some(GrammarPatch {
                source: "b <- [b]".to_string(),
                start_rule: Some("b".to_string()),
            }),
        );
        assert_eq!(cloned.start_rule, "b");
        assert_eq!(cloned.version, base.version + 1);
        add_rule(&mut base, "c", "[c]").expect("rule added");
        assert!(base.get_rule("c").is_some());
    }

    #[test]
    fn trace_callback_fires_on_rule_enter_and_exit() {
        use std::sync::{Arc, Mutex};
        let grammar = Grammar::new("word <- [a-z]+").with_start_rule("word");
        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        let config = ParserConfig::default().with_trace(move |e| {
            ev.lock().unwrap().push(format!("{}:{}", e.kind, e.rule));
        });
        parse("abc", &grammar, Some(config), false).expect("parse succeeds");
        let log = events.lock().unwrap().clone();
        assert!(
            log.iter().any(|e| e == "enter:word"),
            "expected enter event"
        );
        assert!(log.iter().any(|e| e == "exit:word"), "expected exit event");
    }
}
