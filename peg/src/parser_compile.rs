use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::analysis::analyze_grammar;
use crate::error::ParseError;
use crate::expr::PegExpr;
use crate::grammar::{validate_import_alias, Grammar};
use crate::invalid_rules::InvalidRulePolicy;
pub(crate) use crate::parser_analysis::ChoiceDispatch;
use crate::parser_analysis::{compute_dispatch_from_choice_expr, compute_lr_min_step_from_expr};
use crate::signature::StableHasher;
use crate::skip::{skip_strategy_from_config, BoxedSkipStrategy};
use crate::types::ParserConfig;

/// Compile `grammar`, memoising the result inside the grammar itself.
///
/// The compiled form is cached in `Grammar::compiled` (a per-grammar,
/// version-keyed `CompiledMemo`), so a caller that reuses a `Grammar` across
/// parses compiles it once. There is no process-global cache: the memo's
/// lifetime is the grammar's, and any mutation bumps `grammar.version`, which
/// invalidates the entry on the next lookup.
pub(crate) fn get_compiled(grammar: &Grammar) -> Result<Arc<CompiledGrammar>, ParseError> {
    if let Some(cached) = grammar.compiled.get(grammar.version) {
        if let Ok(compiled) = cached.downcast::<CompiledGrammar>() {
            return Ok(compiled);
        }
    }
    let compiled = Arc::new(CompiledGrammar::compile(grammar)?);
    let erased: Arc<dyn std::any::Any + Send + Sync> = compiled.clone();
    grammar.compiled.set(grammar.version, erased);
    Ok(compiled)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MemoRuntimeConfig {
    pub(crate) enabled: bool,
    pub(crate) rule_limit: Option<usize>,
}

pub(crate) fn runtime_signature(
    grammar: &Grammar,
    config: &ParserConfig,
) -> Result<u64, ParseError> {
    let policy = resolve_invalid_rule_policy(grammar, config)?;
    let memo = memo_runtime_config(config)?;
    let mut hasher = StableHasher::default();
    policy.include_invalid_rules.hash(&mut hasher);
    config.return_spans.hash(&mut hasher);
    memo.enabled.hash(&mut hasher);
    config.memo_policy.hash(&mut hasher);
    config.max_steps.hash(&mut hasher);
    for prefix in policy.prefixes {
        prefix.hash(&mut hasher);
    }
    Ok(hasher.finish())
}

pub(crate) fn memo_runtime_config(config: &ParserConfig) -> Result<MemoRuntimeConfig, ParseError> {
    let Some(policy) = config.memo_policy.as_ref() else {
        return Ok(MemoRuntimeConfig {
            enabled: config.memo,
            rule_limit: config
                .memo
                .then_some(default_memo_rule_limit(config.max_steps)),
        });
    };
    if !config.memo || policy.global_budget() == Some(0) {
        return Ok(MemoRuntimeConfig {
            enabled: false,
            rule_limit: None,
        });
    }
    Ok(MemoRuntimeConfig {
        enabled: true,
        rule_limit: policy.global_budget(),
    })
}

fn default_memo_rule_limit(max_steps: usize) -> usize {
    max_steps.saturating_mul(16).max(1)
}

pub(crate) fn ensure_memo_allowed_for_left_recursion(
    memo: &MemoRuntimeConfig,
    compiled: &CompiledGrammar,
    text_len: usize,
) -> Result<(), ParseError> {
    if memo.enabled || compiled.left_recursive_rules.is_empty() {
        return Ok(());
    }
    let mut rules: Vec<&str> = compiled
        .left_recursive_rules
        .iter()
        .map(String::as_str)
        .collect();
    rules.sort_unstable();
    Err(ParseError::new(
        format!("memoization cannot be disabled for left-recursive rules: {rules:?}"),
        0,
        text_len,
    ))
}

pub(crate) struct CompiledGrammar {
    pub(crate) start_rule: String,
    pub(crate) analysis_errors: Vec<String>,
    pub(crate) metadata_keys: Vec<String>,
    pub(crate) rules: HashMap<String, PegExpr>,
    /// Ordered parameter names for each parametric rule.
    pub(crate) rule_params: HashMap<String, Vec<String>>,
    /// Compiled imported grammars keyed by alias.
    pub(crate) imports: HashMap<String, CompiledGrammar>,
    /// Import aliases sorted once at compile time, so the semantic-context
    /// builder need not re-sort them on every hook invocation.
    pub(crate) import_aliases_sorted: Vec<String>,
    /// Rules that participate in left-recursive SCCs and need seed/grow parsing.
    pub(crate) left_recursive_rules: HashSet<String>,
    /// Minimum proven byte advance for one successful growth step per LR rule.
    pub(crate) lr_min_step: HashMap<String, usize>,
    /// Precomputed first-character dispatch tables for top-level Choice rules.
    pub(crate) rule_dispatch: HashMap<String, ChoiceDispatch>,
    /// Maps each left-recursive rule to its SCC index. Rules in the same
    /// left-recursive cycle (direct *or* indirect/mutual) share an index, so the
    /// evaluator can let only the SCC *head* drive seed-grow while the other
    /// involved rules reuse the head's seed.
    pub(crate) rule_scc: HashMap<String, usize>,
}

impl CompiledGrammar {
    fn compile(grammar: &Grammar) -> Result<Self, ParseError> {
        let analysis = match &grammar.state.analysis_state {
            Some(cached) => cached.analysis.clone(),
            None => analyze_grammar(grammar),
        };
        let analysis_errors = analysis.errors.clone();
        let left_recursive_rules: HashSet<String> = analysis
            .left_recursive_sccs
            .iter()
            .flatten()
            .cloned()
            .collect();
        let mut rule_scc: HashMap<String, usize> = HashMap::new();
        for (idx, scc) in analysis.left_recursive_sccs.iter().enumerate() {
            for rule_name in scc {
                rule_scc.insert(rule_name.clone(), idx);
            }
        }
        let mut lr_min_step = HashMap::new();
        for scc in &analysis.left_recursive_sccs {
            let scc_set: HashSet<&str> = scc.iter().map(String::as_str).collect();
            for rule_name in scc {
                if let Some(rule) = grammar.get_rule(rule_name) {
                    lr_min_step.insert(
                        rule_name.clone(),
                        compute_lr_min_step_from_expr(rule.expr(), &scc_set).max(1),
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
            validate_import_alias(alias.as_str())?;
            imports.insert(alias.clone(), CompiledGrammar::compile(imported)?);
        }
        let mut import_aliases_sorted: Vec<String> = imports.keys().cloned().collect();
        import_aliases_sorted.sort_unstable();

        let mut metadata_keys: Vec<String> = grammar.metadata.keys().cloned().collect();
        metadata_keys.sort_unstable();

        Ok(Self {
            start_rule: grammar.start_rule.clone(),
            analysis_errors,
            metadata_keys,
            rules,
            rule_params,
            imports,
            import_aliases_sorted,
            left_recursive_rules,
            lr_min_step,
            rule_dispatch,
            rule_scc,
        })
    }
}

/// Match CAAP surface defaults: grammars use the default trivia skipper unless metadata opts out.
pub(crate) fn resolve_trivia_skipper(
    grammar_meta: Option<&std::collections::HashMap<String, serde_json::Value>>,
) -> Result<Option<BoxedSkipStrategy>, ParseError> {
    let token = match grammar_meta.and_then(|m| m.get("trivia")) {
        Some(serde_json::Value::String(token)) => token.as_str(),
        Some(other) => {
            return Err(ParseError::new(
                format!(
                    "invalid trivia metadata: expected string, got {}",
                    json_type_name(other)
                ),
                0,
                0,
            ));
        }
        None => "default",
    };
    skip_strategy_from_config(Some(token)).map_err(|error| {
        ParseError::new(
            format!("invalid trivia regex in grammar metadata: {error}"),
            0,
            0,
        )
    })
}

pub(crate) fn resolve_indentation_enabled(
    grammar_meta: Option<&std::collections::HashMap<String, serde_json::Value>>,
) -> Result<bool, ParseError> {
    match grammar_meta.and_then(|m| m.get("indentation")) {
        Some(serde_json::Value::Bool(enabled)) => Ok(*enabled),
        Some(other) => Err(ParseError::new(
            format!(
                "invalid indentation metadata: expected bool, got {}",
                json_type_name(other)
            ),
            0,
            0,
        )),
        None => Ok(false),
    }
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

pub(crate) fn resolve_invalid_rule_policy(
    grammar: &Grammar,
    config: &ParserConfig,
) -> Result<InvalidRulePolicy, ParseError> {
    InvalidRulePolicy::resolve(
        grammar,
        config.include_invalid_rules,
        config.invalid_rule_prefixes.as_deref(),
    )
}

pub(crate) fn validate_lex_tokens(
    text: &str,
    tokens: &[crate::types::LexToken],
) -> Result<(), ParseError> {
    let mut previous_end = 0;
    for (index, token) in tokens.iter().enumerate() {
        if token.kind.is_empty() {
            return Err(ParseError::new(
                format!("lex token[{index}] kind must be non-empty"),
                token.start.min(text.len()),
                text.len(),
            ));
        }
        if token.start >= token.end {
            return Err(ParseError::new(
                format!("lex token[{index}] span must be non-empty and ordered"),
                token.start.min(text.len()),
                text.len(),
            ));
        }
        if token.end > text.len() {
            return Err(ParseError::new(
                format!("lex token[{index}] end exceeds input length"),
                token.start.min(text.len()),
                text.len(),
            ));
        }
        if token.start < previous_end {
            return Err(ParseError::new(
                format!("lex token[{index}] overlaps or is out of order"),
                token.start,
                text.len(),
            ));
        }
        if !text.is_char_boundary(token.start) || !text.is_char_boundary(token.end) {
            return Err(ParseError::new(
                format!("lex token[{index}] span is not on UTF-8 character boundaries"),
                token.start,
                text.len(),
            ));
        }
        if text[token.start..token.end] != token.text {
            return Err(ParseError::new(
                format!("lex token[{index}] text does not match input slice"),
                token.start,
                token.end,
            ));
        }
        previous_end = token.end;
    }
    Ok(())
}
