use std::collections::{HashMap, HashSet};

use crate::expr::PegExpr;

/// Precomputed first-character dispatch table for a Choice expression.
///
/// `map[c]` = 1-based indices of alternatives whose first char *could* be `c`.
/// `default` = 1-based indices of alternatives that must always be tried.
pub(crate) type ChoiceDispatch = (HashMap<char, Vec<usize>>, Vec<usize>);

/// Return all parametric rule calls in `expr` as `(rule_name, arg_count)` pairs.
pub(crate) fn extract_calls_from_expr(expr: &PegExpr) -> Vec<(String, usize)> {
    let mut calls = Vec::new();
    collect_calls_impl(expr, &mut calls);
    calls
}

/// Return all `Parameter` names referenced in `expr`.
pub(crate) fn extract_params_used_from_expr(expr: &PegExpr) -> Vec<String> {
    let mut params = Vec::new();
    collect_params_impl(expr, &mut params);
    params.sort_unstable();
    params.dedup();
    params
}

/// Return `Some("cut")` or `Some("eager")` if `expr` contains a bare commit
/// (a `Cut`/`Eager` not inside any `Choice` alternative), else `None`.
pub(crate) fn has_bare_commit_in_expr(expr: &PegExpr) -> Option<&'static str> {
    bare_commit_kind(expr, false)
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
        | PegExpr::LookBehind { expr: n, .. }
        | PegExpr::Optional(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::Repeat { expr: n, .. }
        | PegExpr::Eager(n)
        | PegExpr::NoTrivia(n)
        | PegExpr::WithTrivia { expr: n, .. } => collect_calls_impl(n, out),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::SemanticGuard { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::GrammarScope { expr: n, .. } => collect_calls_impl(n, out),
        PegExpr::SepOneOrMore { element, separator }
        | PegExpr::Interspersed { element, separator } => {
            collect_calls_impl(element, out);
            collect_calls_impl(separator, out);
        }
        PegExpr::Precedence { operand, levels } => {
            collect_calls_impl(operand, out);
            for level in levels {
                for op in &level.operators {
                    collect_calls_impl(op, out);
                }
            }
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
        | PegExpr::LookBehind { expr: n, .. }
        | PegExpr::Optional(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::Repeat { expr: n, .. }
        | PegExpr::Eager(n)
        | PegExpr::NoTrivia(n)
        | PegExpr::WithTrivia { expr: n, .. } => collect_params_impl(n, out),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::SemanticGuard { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::GrammarScope { expr: n, .. } => collect_params_impl(n, out),
        PegExpr::Call { args, .. } => {
            for arg in args {
                collect_params_impl(arg, out);
            }
        }
        PegExpr::SepOneOrMore { element, separator }
        | PegExpr::Interspersed { element, separator } => {
            collect_params_impl(element, out);
            collect_params_impl(separator, out);
        }
        PegExpr::Precedence { operand, levels } => {
            collect_params_impl(operand, out);
            for level in levels {
                for op in &level.operators {
                    collect_params_impl(op, out);
                }
            }
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
        | PegExpr::SemanticGuard { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::GrammarScope { expr: n, .. } => bare_commit_kind(n, inside_choice),
        PegExpr::Optional(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::Repeat { expr: n, .. }
        | PegExpr::NoTrivia(n)
        | PegExpr::WithTrivia { expr: n, .. } => bare_commit_kind(n, inside_choice),
        // Lookaheads never trigger cut semantics outward.
        PegExpr::And(_) | PegExpr::Not(_) => None,
        PegExpr::SepOneOrMore { element, separator }
        | PegExpr::Interspersed { element, separator } => bare_commit_kind(element, inside_choice)
            .or_else(|| bare_commit_kind(separator, inside_choice)),
        _ => None,
    }
}

// ── Public analysis helpers ────────────────────────────────────────────────

/// Return every rule name referenced by `expr`.
pub(crate) fn extract_refs_from_expr(expr: &PegExpr) -> Vec<String> {
    let mut refs = Vec::new();
    collect_expr_refs(expr, &mut refs);
    refs.sort_unstable();
    refs.dedup();
    refs
}

/// Return `true` when `expr` contains any `TokenRef` (`tok(...)`) expression.
pub(crate) fn has_token_ref_in_expr(expr: &PegExpr) -> bool {
    expr_has_token_ref(expr)
}

fn expr_has_token_ref(expr: &PegExpr) -> bool {
    match expr {
        PegExpr::TokenRef { .. } => true,
        PegExpr::Sequence(items) | PegExpr::Choice(items) => items.iter().any(expr_has_token_ref),
        PegExpr::And(n)
        | PegExpr::Not(n)
        | PegExpr::LookBehind { expr: n, .. }
        | PegExpr::Optional(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::Repeat { expr: n, .. }
        | PegExpr::Eager(n)
        | PegExpr::NoTrivia(n)
        | PegExpr::WithTrivia { expr: n, .. } => expr_has_token_ref(n),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::SemanticGuard { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::GrammarScope { expr: n, .. } => expr_has_token_ref(n),
        PegExpr::SepOneOrMore { element, separator }
        | PegExpr::Interspersed { element, separator } => {
            expr_has_token_ref(element) || expr_has_token_ref(separator)
        }
        PegExpr::Call { args, .. } => args.iter().any(expr_has_token_ref),
        _ => false,
    }
}

/// Return `true` when `expr` contains any char-level terminal (Literal, Regex,
/// Dot, HardKeyword, SoftKeyword, Island, RawBlock, Newline, Indent, Dedent).
pub(crate) fn has_char_terminal_in_expr(expr: &PegExpr) -> bool {
    expr_has_char_terminal(expr)
}

fn expr_has_char_terminal(expr: &PegExpr) -> bool {
    match expr {
        PegExpr::Literal(_)
        | PegExpr::Regex(_)
        | PegExpr::CharClass(_)
        | PegExpr::Recover { .. }
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
        | PegExpr::LookBehind { expr: n, .. }
        | PegExpr::Optional(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::Repeat { expr: n, .. }
        | PegExpr::Eager(n)
        | PegExpr::NoTrivia(n)
        | PegExpr::WithTrivia { expr: n, .. } => expr_has_char_terminal(n),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::SemanticGuard { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::GrammarScope { expr: n, .. } => expr_has_char_terminal(n),
        PegExpr::SepOneOrMore { element, separator }
        | PegExpr::Interspersed { element, separator } => {
            expr_has_char_terminal(element) || expr_has_char_terminal(separator)
        }
        PegExpr::Call { args, .. } => args.iter().any(expr_has_char_terminal),
        _ => false,
    }
}

/// Return whether an expression is nullable without following `Ref` nodes.
pub(crate) fn is_nullable_in_expr(expr: &PegExpr) -> bool {
    match expr {
        PegExpr::Literal(_)
        | PegExpr::Regex(_)
        | PegExpr::CharClass(_)
        | PegExpr::Recover { .. }
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
        | PegExpr::Backref(_)
        | PegExpr::TokenRef { .. } => false,
        PegExpr::Optional(_) | PegExpr::ZeroOrMore(_) | PegExpr::Cut => true,
        // Lookbehind/lookahead/predicates are zero-width → nullable.
        PegExpr::Not(_) | PegExpr::SemanticPredicate { .. } | PegExpr::LookBehind { .. } => true,
        PegExpr::And(n)
        | PegExpr::NoTrivia(n)
        | PegExpr::WithTrivia { expr: n, .. }
        | PegExpr::Eager(n) => is_nullable_in_expr(n),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::SemanticGuard { expr: n, .. }
        | PegExpr::Capture { expr: n, .. } => is_nullable_in_expr(n),
        PegExpr::OneOrMore(n) => is_nullable_in_expr(n),
        PegExpr::Repeat { expr, min, .. } => *min == 0 || is_nullable_in_expr(expr),
        // A precedence expression always matches at least its operand.
        PegExpr::Precedence { operand, .. } => is_nullable_in_expr(operand),
        PegExpr::SepOneOrMore { element, .. } | PegExpr::Interspersed { element, .. } => {
            is_nullable_in_expr(element)
        }
        PegExpr::Sequence(items) => items.iter().all(is_nullable_in_expr),
        PegExpr::Choice(alts) => alts.iter().any(is_nullable_in_expr),
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
        | PegExpr::LookBehind { expr: n, .. }
        | PegExpr::Optional(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::Repeat { expr: n, .. }
        | PegExpr::NoTrivia(n)
        | PegExpr::WithTrivia { expr: n, .. }
        | PegExpr::Eager(n) => collect_expr_refs(n, out),
        PegExpr::Named { expr: n, .. } | PegExpr::Expected { expr: n, .. } => {
            collect_expr_refs(n, out);
        }
        PegExpr::SepOneOrMore { element, separator }
        | PegExpr::Interspersed { element, separator } => {
            collect_expr_refs(element, out);
            collect_expr_refs(separator, out);
        }
        PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::SemanticGuard { expr: n, .. }
        | PegExpr::Capture { expr: n, .. } => {
            collect_expr_refs(n, out);
        }
        PegExpr::Precedence { operand, levels } => {
            collect_expr_refs(operand, out);
            for level in levels {
                for op in &level.operators {
                    collect_expr_refs(op, out);
                }
            }
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
        | PegExpr::CharClass(_)
        | PegExpr::Recover { .. }
        | PegExpr::Dot
        | PegExpr::Cut
        | PegExpr::Backref(_)
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
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::SemanticGuard { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::NoTrivia(n)
        | PegExpr::WithTrivia { expr: n, .. } => expr_fixed_text_impl(n, fixed_rules),
        PegExpr::Cut => Some(String::new()),
        _ => None,
    }
}

/// Return the exact text that `expr` always matches given fixed-text map for other rules.
pub(crate) fn fixed_text_in_expr(
    expr: &PegExpr,
    fixed_rules: &HashMap<String, Option<String>>,
) -> Option<String> {
    expr_fixed_text_impl(expr, fixed_rules)
}

// ── Nullable-with-rules analysis ───────────────────────────────────────────

fn is_expr_nullable_with_rules_impl(expr: &PegExpr, nullable_rules: &HashSet<String>) -> bool {
    match expr {
        PegExpr::Optional(_) | PegExpr::ZeroOrMore(_) | PegExpr::Cut => true,
        PegExpr::Not(_) | PegExpr::SemanticPredicate { .. } => true,
        PegExpr::And(n)
        | PegExpr::NoTrivia(n)
        | PegExpr::WithTrivia { expr: n, .. }
        | PegExpr::Eager(n) => is_expr_nullable_with_rules_impl(n, nullable_rules),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::SemanticGuard { expr: n, .. }
        | PegExpr::Capture { expr: n, .. } => is_expr_nullable_with_rules_impl(n, nullable_rules),
        PegExpr::OneOrMore(n) => is_expr_nullable_with_rules_impl(n, nullable_rules),
        PegExpr::Repeat { expr, min, .. } => {
            *min == 0 || is_expr_nullable_with_rules_impl(expr, nullable_rules)
        }
        PegExpr::Precedence { operand, .. } => {
            is_expr_nullable_with_rules_impl(operand, nullable_rules)
        }
        PegExpr::SepOneOrMore { element, .. } | PegExpr::Interspersed { element, .. } => {
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

/// Return whether `expr` can match without consuming input using known nullable
/// refs (follows `Ref` nodes via the supplied nullable rule set).
pub(crate) fn is_nullable_with_rules_in_expr(
    expr: &PegExpr,
    nullable_rules: &HashSet<String>,
) -> bool {
    is_expr_nullable_with_rules_impl(expr, nullable_rules)
}

// ── Productive-with-rules analysis ─────────────────────────────────────────

fn is_expr_productive_impl(expr: &PegExpr, productive_rules: &HashMap<String, bool>) -> bool {
    match expr {
        PegExpr::Literal(_)
        | PegExpr::Regex(_)
        | PegExpr::CharClass(_)
        | PegExpr::Recover { .. }
        | PegExpr::Dot
        | PegExpr::HardKeyword(_)
        | PegExpr::SoftKeyword(_)
        | PegExpr::Island { .. }
        | PegExpr::RawBlock { .. }
        | PegExpr::Cut
        | PegExpr::Not(_)
        | PegExpr::LookBehind { .. }
        | PegExpr::Backref(_)
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
        PegExpr::Repeat { expr, min, .. } => {
            *min == 0 || is_expr_productive_impl(expr, productive_rules)
        }
        PegExpr::Precedence { operand, .. } => is_expr_productive_impl(operand, productive_rules),
        PegExpr::OneOrMore(n)
        | PegExpr::And(n)
        | PegExpr::Eager(n)
        | PegExpr::NoTrivia(n)
        | PegExpr::WithTrivia { expr: n, .. } => is_expr_productive_impl(n, productive_rules),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::SemanticGuard { expr: n, .. }
        | PegExpr::Capture { expr: n, .. } => is_expr_productive_impl(n, productive_rules),
        PegExpr::SepOneOrMore { element, .. } | PegExpr::Interspersed { element, .. } => {
            is_expr_productive_impl(element, productive_rules)
        }
        PegExpr::Call { args, .. } => args
            .iter()
            .all(|a| is_expr_productive_impl(a, productive_rules)),
        PegExpr::Invalid(_) => false,
    }
}

/// Return whether `expr` can match on at least some input.
pub(crate) fn is_productive_with_rules_in_expr(
    expr: &PegExpr,
    productive_rules: &HashMap<String, bool>,
) -> bool {
    is_expr_productive_impl(expr, productive_rules)
}

// ── Left-refs analysis ─────────────────────────────────────────────────────

/// Collect rule names reachable from the left edge of `expr`.
///
/// A ref is a "left ref" if it can be the first thing consumed — i.e., all
/// preceding parts in any enclosing Sequence are nullable.
fn collect_left_refs_impl(expr: &PegExpr, nullable: &HashSet<String>) -> HashSet<String> {
    match expr {
        PegExpr::Ref(name) => {
            let mut s = HashSet::new();
            s.insert(name.clone());
            s
        }
        PegExpr::Sequence(parts) => {
            let mut refs = HashSet::new();
            for part in parts {
                refs.extend(collect_left_refs_impl(part, nullable));
                if !is_expr_nullable_with_rules_impl(part, nullable) {
                    break;
                }
            }
            refs
        }
        PegExpr::Choice(alts) => {
            let mut refs = HashSet::new();
            for alt in alts {
                refs.extend(collect_left_refs_impl(alt, nullable));
            }
            refs
        }
        PegExpr::SepOneOrMore { element, .. } | PegExpr::Interspersed { element, .. } => {
            collect_left_refs_impl(element, nullable)
        }
        // Only the operand is at the left edge of a precedence expression.
        PegExpr::Precedence { operand, .. } => collect_left_refs_impl(operand, nullable),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::SemanticGuard { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::And(n)
        | PegExpr::Not(n)
        | PegExpr::LookBehind { expr: n, .. }
        | PegExpr::Optional(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::Repeat { expr: n, .. }
        | PegExpr::NoTrivia(n)
        | PegExpr::WithTrivia { expr: n, .. }
        | PegExpr::Eager(n) => collect_left_refs_impl(n, nullable),
        _ => HashSet::new(),
    }
}

/// Return the sorted list of rule names reachable from the left edge of `expr`.
///
/// Only refs in `nullable_rules` may be "transparent" (not stopping the
/// left-edge walk) when they appear in a Sequence.
pub(crate) fn extract_left_refs_from_expr(
    expr: &PegExpr,
    nullable_rules: &HashSet<String>,
) -> Vec<String> {
    let mut refs: Vec<String> = collect_left_refs_impl(expr, nullable_rules)
        .into_iter()
        .collect();
    refs.sort_unstable();
    refs
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

/// Return the minimum proven character advance per LR-growth iteration for `expr`.
///
/// Walks the rule's body looking for `Sequence([Ref(lr_rule), Literal(sep), ...])`
/// patterns. Returns 1 (conservative) when no provable minimum can be found.
pub(crate) fn compute_lr_min_step_from_expr(expr: &PegExpr, scc_set: &HashSet<&str>) -> usize {
    rule_lr_min_step_impl(expr, scc_set)
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
        PegExpr::Repeat { expr, max, .. } => {
            // An unbounded repeat over a nullable expr is the same trap.
            if max.is_none() && is_expr_nullable_with_rules_impl(expr, nullable_rules) {
                out.push("Repeat(expr nullable)".to_string());
            }
            collect_nullable_repetitions_impl(expr, nullable_rules, out);
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
        PegExpr::Interspersed { element, separator } => {
            if is_expr_nullable_with_rules_impl(separator, nullable_rules) {
                out.push("Interspersed(sep nullable)".to_string());
            }
            if is_expr_nullable_with_rules_impl(element, nullable_rules) {
                out.push("Interspersed(expr nullable)".to_string());
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
        | PegExpr::SemanticGuard { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::Optional(n)
        | PegExpr::And(n)
        | PegExpr::Not(n)
        | PegExpr::LookBehind { expr: n, .. }
        | PegExpr::Eager(n)
        | PegExpr::NoTrivia(n)
        | PegExpr::WithTrivia { expr: n, .. }
        | PegExpr::GrammarScope { expr: n, .. } => {
            collect_nullable_repetitions_impl(n, nullable_rules, out);
        }
        _ => {}
    }
}

/// Return kind strings for each nullable repetition trap in `expr`.
pub(crate) fn collect_nullable_repetitions_from_expr(
    expr: &PegExpr,
    nullable_rules: &HashSet<String>,
) -> Vec<String> {
    let mut out = Vec::new();
    collect_nullable_repetitions_impl(expr, nullable_rules, &mut out);
    out
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

/// Return `(dead_index, live_index)` pairs (1-based) for dead choice alternatives in `expr`.
pub(crate) fn collect_dead_choice_alts_from_expr(
    expr: &PegExpr,
    fixed_rules: &HashMap<String, Option<String>>,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    collect_dead_choice_alts_impl(expr, fixed_rules, &mut out);
    out
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

/// Return `(dead_index, live_index, prefix)` for prefix-shadowed alternatives in `expr`.
pub(crate) fn collect_prefix_shadowed_alts_from_expr(
    expr: &PegExpr,
    fixed_rules: &HashMap<String, Option<String>>,
) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    collect_prefix_shadowed_alts_impl(expr, fixed_rules, &mut out);
    out
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

/// Return `(alt1, alt2, common_prefix)` for alternatives sharing ≥3 common chars in `expr`.
pub(crate) fn collect_overlapping_prefixes_from_expr(
    expr: &PegExpr,
    fixed_rules: &HashMap<String, Option<String>>,
) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    collect_overlapping_prefixes_impl(expr, fixed_rules, &mut out);
    out
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
        | PegExpr::SemanticGuard { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::Optional(n)
        | PegExpr::And(n)
        | PegExpr::Not(n)
        | PegExpr::LookBehind { expr: n, .. }
        | PegExpr::Eager(n)
        | PegExpr::NoTrivia(n)
        | PegExpr::WithTrivia { expr: n, .. }
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::Repeat { expr: n, .. }
        | PegExpr::GrammarScope { expr: n, .. } => f(n),
        PegExpr::SepOneOrMore { element, separator }
        | PegExpr::Interspersed { element, separator } => {
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
        PegExpr::CharClass(class) => {
            // Negated classes are an open set → no useful dispatch key. For a
            // positive class, expand its ranges (capped, so a very wide class
            // degrades to "unknown" rather than emitting thousands of keys).
            if class.negated {
                return None;
            }
            let mut chars: Vec<char> = Vec::new();
            for &(lo, hi) in &class.ranges {
                for cp in (lo as u32)..=(hi as u32) {
                    if chars.len() >= 256 {
                        return None;
                    }
                    if let Some(c) = char::from_u32(cp) {
                        chars.push(c);
                    }
                }
            }
            Some(chars)
        }
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
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::SemanticGuard { expr: n, .. }
        | PegExpr::Capture { expr: n, .. }
        | PegExpr::And(n)
        | PegExpr::Eager(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::Repeat { expr: n, .. }
        | PegExpr::Optional(n) => expr_first_chars_impl(n, fixed_rules, nullable_rules),
        PegExpr::SepOneOrMore { element, .. } | PegExpr::Interspersed { element, .. } => {
            expr_first_chars_impl(element, fixed_rules, nullable_rules)
        }
        _ => None,
    }
}

/// Build a first-char dispatch table for a top-level Choice expression's
/// alternatives.
///
/// Returns `(dispatch: HashMap<char, Vec<usize>>, default: Vec<usize>)` where
/// indices are 1-based alternative positions. Returns `None` when no dispatch
/// table can be built (every alternative is nullable or has an unbounded
/// first-char set).
pub(crate) fn compute_dispatch_from_choice_expr(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::RuleTextParser;

    fn parse(src: &str) -> PegExpr {
        RuleTextParser::parse(src).expect("test source must parse")
    }

    #[test]
    fn extract_calls_finds_parametric_calls() {
        let calls = extract_calls_from_expr(&parse("wrap('a') combine(item, 'x')"));
        assert!(
            calls.iter().any(|(r, n)| r == "wrap" && *n == 1),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(|(r, n)| r == "combine" && *n == 2),
            "{calls:?}"
        );
    }

    #[test]
    fn extract_params_used_dedups_and_sorts() {
        let params = extract_params_used_from_expr(&parse("$x $y $x"));
        assert_eq!(params, vec!["x".to_string(), "y".to_string()]);
    }

    #[test]
    fn has_bare_commit_detects_cut_in_sequence() {
        assert_eq!(has_bare_commit_in_expr(&parse("'a' ~ 'b'")), Some("cut"));
    }

    #[test]
    fn has_bare_commit_none_for_cut_in_choice() {
        assert_eq!(has_bare_commit_in_expr(&parse("('a' ~ 'b') / 'c'")), None);
    }

    #[test]
    fn has_bare_commit_none_for_no_commit() {
        assert_eq!(has_bare_commit_in_expr(&parse("'a' 'b'")), None);
    }

    #[test]
    fn extract_refs_finds_rule_names() {
        let refs = extract_refs_from_expr(&parse("a b c"));
        assert!(refs.contains(&"a".to_string()));
        assert!(refs.contains(&"b".to_string()));
        assert!(refs.contains(&"c".to_string()));
    }

    #[test]
    fn extract_refs_empty_for_literals() {
        assert!(extract_refs_from_expr(&parse("'hello'")).is_empty());
    }

    #[test]
    fn expr_is_nullable_for_optional_and_star() {
        assert!(is_nullable_in_expr(&parse("'x'?")));
        assert!(is_nullable_in_expr(&parse("'x'*")));
        assert!(!is_nullable_in_expr(&parse("'x'")));
        assert!(!is_nullable_in_expr(&parse("'x'+")));
    }
}
