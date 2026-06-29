#![warn(clippy::all)]
#![deny(missing_docs)]

//! `caap-peg` — CAAP's standalone grammar mechanism.
//!
//! The public API is layered:
//!
//! - [`prelude`] re-exports the curated, everyday surface — grammar building,
//!   parsing ([`ParseRequest`]), values, semantics, and analysis.
//!   Most users want `use caap_peg::prelude::*;`.
//! - The crate root additionally exposes niche capabilities that are not part of
//!   the everyday flow: grammar mutation/diffing, validation, the registry,
//!   incremental parsing/pipelines, diagnostics projections, and signatures.
//! - Optional heavyweight subsystems live behind default-off features:
//!   `recovery` (error-recovery parsing) and `transaction` (transactional
//!   grammar editing). Enable them in `Cargo.toml` when needed.

// ── Core modules ───────────────────────────────────────────────────────────

pub mod analysis;
pub mod analysis_rule;
pub mod ast;
pub mod ast_diff;
pub mod builder;
pub mod diagnostics;
pub mod driver;
pub mod editor;
pub mod error;
pub mod expr;
pub mod grammar;
pub mod incremental_edits;
pub mod mutation;
pub mod pipeline;
pub mod profile;
pub mod registry;
pub mod request;
pub mod scanner;
pub mod skip;
pub mod spec_compiler;
pub(crate) mod spec_compiler_exprs;
pub(crate) mod spec_compiler_helpers;
pub mod testing;
pub mod typed;
pub mod types;
pub mod validation;
pub mod values;

// ── Optional feature-gated subsystems ──────────────────────────────────────

#[cfg(feature = "recovery")]
pub mod recovery;
#[cfg(feature = "transaction")]
pub mod transaction;
#[cfg(feature = "transaction")]
pub mod transaction_stack;

// ── Internal modules (crate-private) ──────────────────────────────────────

pub(crate) mod cache_transforms;
pub(crate) mod diagnostics_utils;
pub(crate) mod graph_ops;
pub(crate) mod invalid_rules;
pub(crate) mod parser_analysis;
pub(crate) mod parser_compile;
pub(crate) mod parser_diagnostics;
pub(crate) mod parser_engine;
pub(crate) mod parser_engine_diag;
pub(crate) mod parser_engine_state;
pub(crate) mod parser_imports;
pub(crate) mod signature;

// ── Prelude: curated everyday surface ──────────────────────────────────────

/// The curated everyday surface. `use caap_peg::prelude::*;` for grammar
/// building, parsing, values, semantics, and analysis.
pub mod prelude {
    pub use crate::analysis::{analyze_and_store, analyze_grammar, GrammarAnalysis};
    pub use crate::ast::{parse_ast, parse_ast_tolerant, parse_ast_with_max_steps, AstNode};
    pub use crate::builder::{self, GrammarBuilder};
    pub use crate::driver::{
        BuiltDriver, Directive, DriverCheckpoint, GrammarContext, GrammarScalar, MemoFacet,
        ParseDriver, ParseDriverBuilder, ParseEffect, ParseView, ParserConfigContext,
        ParserStateContext, SubParse,
    };
    pub use crate::error::ParseError;
    pub use crate::expr::{Fixity, PegExpr, PrecLevel, RECOVER_TAG};
    pub use crate::grammar::{Grammar, GrammarRule};
    pub use crate::parse;
    pub use crate::parser_engine::PEGParser;
    pub use crate::request::ParseRequest;
    pub use crate::scanner::Scanner;
    pub use crate::spec_compiler::SpecCompiler;
    pub use crate::typed::{FromParseValue, FromParseValueError};
    pub use crate::types::{LexToken, MemoPolicy, ParseValue, ParserConfig};
    #[cfg(feature = "derive")]
    pub use caap_peg_derive::FromParseValue;
}

// ── Re-exports ─────────────────────────────────────────────────────────────
//
// Everything the prelude exposes is re-exported at the crate root too, so
// existing `caap_peg::X` paths keep working. The root additionally surfaces the
// niche, non-everyday items (mutation, validation, registry, pipeline,
// diagnostics projections, signatures, …).

pub use crate::analysis::{
    analyze_and_store, analyze_cached_grammar, analyze_grammar, compute_nullable_rules,
    GrammarAnalysis, GrammarAnalysisState,
};
pub use crate::analysis_rule::{RuleIssueSummary, RuleScanSummary};
pub use crate::ast::{
    parse_ast, parse_ast_tolerant, parse_ast_with_max_steps, walk as walk_ast, AstCapture, AstNode,
    AstSpan, Source, ERROR_RULE,
};
pub use crate::ast_diff::{changed_ranges, reparse_ast_incremental, AstEdit};
pub use crate::builder::GrammarBuilder;
pub use crate::diagnostics::{
    byte_to_utf16, interpret_parser_diagnostics, lsp_position, peg_error_to_diagnostic,
    peg_error_to_diagnostic_with_source, render_parse_error, render_parse_errors, Diagnostic,
    ParserDiagnosticsSnapshot, PegDiagnosticSource, RuleVisitStat, SourceLocator, SourcePoint,
    SourceRange, SourceSpan,
};
pub use crate::driver::{
    BuiltDriver, Directive, DriverCheckpoint, GrammarContext, GrammarScalar, MemoFacet,
    ParseDriver, ParseDriverBuilder, ParseEffect, ParseView, ParserConfigContext,
    ParserStateContext, SubParse, SubParseProvider,
};
pub use crate::editor::{
    document_symbols, folding_ranges, selection_ranges, semantic_tokens, FoldRange, RuleKinds,
    SemanticToken, Symbol, SymbolRule, SymbolRules,
};
pub use crate::error::{ParseError, ParseSpan};
pub use crate::expr::{Fixity, PegExpr, PrecLevel, RECOVER_TAG};
pub use crate::grammar::{CloneGrammar, Grammar, GrammarPatch, GrammarRule};
pub use crate::incremental_edits::IncrementalEditError;
pub use crate::mutation::{
    add_rule, apply, diff_grammars, remove_rule, replace_rule, set_start_rule, GrammarDiff,
    GrammarMutation, MutationError, MutationKind, MutationOutcome,
};
pub use crate::parser_engine::{PEGParser, ParseOutput};
pub use crate::pipeline::{
    compute_snapshot_edits, parse_pipeline, IncrementalPipeline, PipelineStage,
    PipelineStageResult, PipelineTextUpdate,
};
pub use crate::profile::{ParseProfile, RuleStats};
pub use crate::registry::{
    from_text, load_json_grammar, GrammarDataSource, GrammarId, GrammarRegistry,
    JsonGrammarPayload, RegistryEntry, RegistryError, ScopedGrammarRegistry,
};
pub use crate::request::ParseRequest;
pub use crate::scanner::Scanner;
pub use crate::signature::grammar_signature;
pub use crate::spec_compiler::{SpecCompileError, SpecCompiler};
pub use crate::typed::{FromParseValue, FromParseValueError};
pub use crate::types::{
    CachedResult, CompletedEdit, CompletedPrefixParse, IncrementalEdit, LexToken, MemoPolicy,
    ParseCache, ParseValue, ParserConfig, ParserOutputMode,
};
pub use crate::validation::{
    validate_grammar, validate_grammar_with_label, validate_grammar_with_options, Severity,
    ValidationIssue, ValidationOptions, ValidationReport,
};
pub use crate::values::{
    contains_spanned, extract_span, strip_spans, unwrap_spanned, SequenceValueBuilder,
};
/// `#[derive(FromParseValue)]` (enabled by the `derive` feature). Shares the
/// `FromParseValue` name with the trait, like `serde`'s `Serialize`.
#[cfg(feature = "derive")]
pub use caap_peg_derive::FromParseValue;

#[cfg(feature = "recovery")]
pub use crate::recovery::{
    collect_delete_candidate, collect_insert_candidates, collect_sync_markers,
    normalize_sync_regex, normalize_sync_tokens, recover_parse, try_recover_parse,
    validate_recovery_config, DefaultRecoveryStrategy, RecoveredParse, RecoveryConfig,
    RecoveryDeleteCandidate, RecoveryInsertCandidate, StreamingFormRecoveryStrategy,
};
#[cfg(feature = "transaction")]
pub use crate::transaction::{GrammarTransaction, TransactionError, TransactionOp};
#[cfg(feature = "transaction")]
pub use crate::transaction_stack::GrammarTransactionStack;

// ── Top-level functions ───────────────────────────────────────────────────────
//
// `parse` is the one-liner; everything richer — spans, semantics, token streams,
// a scanner, a registry, prefix/incremental/profiled/AST output — goes through
// the single [`ParseRequest`] builder. The functions below are grammar/edit
// utilities, not parse entry points.

/// Parse `text` against `grammar` with default configuration.
///
/// For spans, a semantic runtime, a token stream/scanner, a registry, or
/// prefix/incremental/profiled/AST output, use [`ParseRequest`].
pub fn parse(text: &str, grammar: &Grammar) -> Result<ParseValue, ParseError> {
    PEGParser.parse(grammar, text, &ParserConfig::default())
}

/// Normalise and order a batch of edits into non-overlapping sequential edits.
pub fn snapshot_edits_to_sequential(
    base_text: &str,
    edits: &[IncrementalEdit],
) -> Result<Vec<CompletedEdit>, ParseError> {
    let parser = PEGParser;
    parser.snapshot_edits_to_sequential(base_text, edits)
}

/// Apply sequential [`CompletedEdit`]s to `base_text`, returning the new text.
pub fn apply_edits(base_text: &str, edits: &[CompletedEdit]) -> Result<String, ParseError> {
    PEGParser::apply_edits(base_text, edits)
}

/// Clone `grammar`, optionally applying a [`GrammarPatch`], resetting caches.
pub fn clone_grammar(
    grammar: &Grammar,
    patch: Option<GrammarPatch>,
) -> Result<Grammar, ParseError> {
    PEGParser::clone_grammar(grammar, patch)
}

/// Return the rule reference graph as `rule → set of referenced rules`.
pub fn rule_graph(grammar: &Grammar) -> std::collections::HashMap<String, Vec<String>> {
    use crate::analysis::analyze_grammar;
    let analysis = analyze_grammar(grammar);
    analysis.refs
}

// ── Module-level tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // End-to-end parse/driver behaviour is covered by the integration suites
    // (`tests/api.rs`, `tests/driver.rs`); these unit tests cover only the
    // crate-root grammar-utility wrappers in isolation.

    #[test]
    fn clone_and_mutate_grammar() {
        let mut base = Grammar::trusted_new("a <- [a]");
        let cloned = clone_grammar(
            &base,
            Some(GrammarPatch {
                source: "b <- [b]".to_string(),
                start_rule: Some("b".to_string()),
            }),
        )
        .expect("grammar patch should parse");
        assert_eq!(cloned.start_rule, "b");
        assert_eq!(cloned.version, base.version + 1);
        add_rule(&mut base, "c", "[c]").expect("rule added");
        assert!(base.get_rule("c").is_some());
    }

    #[test]
    fn clone_grammar_rejects_invalid_patch_source() {
        let base = Grammar::trusted_new("a <- [a]");
        let err = clone_grammar(
            &base,
            Some(GrammarPatch {
                source: "b <- [b".to_string(),
                start_rule: Some("b".to_string()),
            }),
        )
        .expect_err("invalid patch source should fail");
        assert!(err.message.contains("unterminated"));
    }
}
