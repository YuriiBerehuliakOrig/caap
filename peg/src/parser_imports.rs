use std::collections::HashSet;

use crate::error::ParseError;
use crate::expr::{PegExpr, RuleTextParser};
use crate::grammar::Grammar;
use crate::registry::GrammarRegistry;

/// Return all grammar import aliases referenced by `ImportedRef` or `GrammarScope` expressions in `source`.
pub fn extract_import_aliases_from_source(source: &str) -> Vec<String> {
    match RuleTextParser::parse(source) {
        Ok(expr) => {
            let mut aliases = Vec::new();
            collect_import_aliases(&expr, &mut aliases);
            aliases.sort_unstable();
            aliases.dedup();
            aliases
        }
        Err(_) => Vec::new(),
    }
}

pub(crate) fn hydrate_imports_from_registry(
    grammar: &Grammar,
    registry: &GrammarRegistry,
) -> Result<Grammar, ParseError> {
    let mut in_progress = HashSet::new();
    hydrate_imports_from_registry_impl(grammar, registry, &mut in_progress)
}

fn hydrate_imports_from_registry_impl(
    grammar: &Grammar,
    registry: &GrammarRegistry,
    in_progress: &mut HashSet<String>,
) -> Result<Grammar, ParseError> {
    let mut hydrated = grammar.clone();

    let existing_aliases: Vec<String> = hydrated.imports.keys().cloned().collect();
    for alias in existing_aliases {
        let Some(imported) = hydrated.imports.get(&alias).cloned() else {
            continue;
        };
        let nested = hydrate_imports_from_registry_impl(&imported, registry, in_progress)?;
        hydrated.imports.insert(alias, Box::new(nested));
    }

    for (alias, target) in metadata_import_targets(&hydrated).into_iter().chain(
        import_aliases_from_grammar(&hydrated)
            .into_iter()
            .map(|alias| {
                let target = alias.clone();
                (alias, target)
            }),
    ) {
        if hydrated.imports.contains_key(&alias) {
            continue;
        }
        if !in_progress.insert(alias.clone()) {
            continue;
        }
        let imported = registry.resolve_grammar(&target).map_err(|err| {
            ParseError::new(
                format!("unknown grammar import '{alias}' targeting '{target}': {err}"),
                0,
                0,
            )
            .with_code("unknown_import")
        })?;
        let nested = hydrate_imports_from_registry_impl(&imported, registry, in_progress)?;
        in_progress.remove(&alias);
        hydrated.imports.insert(alias, Box::new(nested));
    }

    Ok(hydrated)
}

fn import_aliases_from_grammar(grammar: &Grammar) -> Vec<String> {
    let mut aliases = Vec::new();
    for rule in &grammar.rules {
        collect_import_aliases(rule.expr(), &mut aliases);
    }
    aliases.sort_unstable();
    aliases.dedup();
    aliases
}

fn metadata_import_targets(grammar: &Grammar) -> Vec<(String, String)> {
    let mut imports = Vec::new();
    let Some(gmeta) = grammar.metadata.get("__grammar__") else {
        return imports;
    };
    let Some(value) = gmeta.get("imports") else {
        return imports;
    };
    if let Some(map) = value.as_object() {
        for (alias, target) in map {
            if let Some(target) = target.as_str() {
                imports.push((alias.clone(), target.to_string()));
            }
        }
    }
    imports.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    imports.dedup();
    imports
}

fn collect_import_aliases(expr: &PegExpr, out: &mut Vec<String>) {
    match expr {
        PegExpr::ImportedRef { grammar_name, .. } => out.push(grammar_name.clone()),
        PegExpr::GrammarScope {
            grammar_name,
            expr: inner,
        } => {
            out.push(grammar_name.clone());
            collect_import_aliases(inner, out);
        }
        PegExpr::Sequence(items) | PegExpr::Choice(items) => {
            for item in items {
                collect_import_aliases(item, out);
            }
        }
        PegExpr::And(n)
        | PegExpr::Not(n)
        | PegExpr::Optional(n)
        | PegExpr::OneOrMore(n)
        | PegExpr::ZeroOrMore(n)
        | PegExpr::Eager(n)
        | PegExpr::NoTrivia(n) => collect_import_aliases(n, out),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::Behavior { expr: n, .. }
        | PegExpr::Capture { expr: n, .. } => collect_import_aliases(n, out),
        PegExpr::SepOneOrMore { element, separator } => {
            collect_import_aliases(element, out);
            collect_import_aliases(separator, out);
        }
        PegExpr::Call { args, .. } => {
            for arg in args {
                collect_import_aliases(arg, out);
            }
        }
        _ => {}
    }
}
