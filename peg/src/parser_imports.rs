use crate::error::ParseError;
use crate::expr::PegExpr;
use crate::grammar::Grammar;
use crate::registry::GrammarRegistry;

/// Return all grammar import aliases referenced by `ImportedRef` or `GrammarScope` expressions in `expr`.
pub fn extract_import_aliases_from_expr(expr: &PegExpr) -> Vec<String> {
    let mut aliases = Vec::new();
    collect_import_aliases(expr, &mut aliases);
    aliases.sort_unstable();
    aliases.dedup();
    aliases
}

pub(crate) fn hydrate_imports_from_registry(
    grammar: &Grammar,
    registry: &GrammarRegistry,
) -> Result<Grammar, ParseError> {
    let mut active_targets = Vec::new();
    hydrate_imports_from_registry_impl(grammar, registry, &mut active_targets)
}

fn hydrate_imports_from_registry_impl(
    grammar: &Grammar,
    registry: &GrammarRegistry,
    active_targets: &mut Vec<String>,
) -> Result<Grammar, ParseError> {
    let mut hydrated = grammar.clone();

    let existing_aliases: Vec<String> = hydrated.imports.keys().cloned().collect();
    for alias in existing_aliases {
        let Some(imported) = hydrated.imports.get(&alias).cloned() else {
            continue;
        };
        let nested = hydrate_imports_from_registry_impl(&imported, registry, active_targets)?;
        hydrated.imports.insert(alias, Box::new(nested));
    }

    for (alias, target) in metadata_import_targets(&hydrated)?.into_iter().chain(
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
        push_import_target(active_targets, &alias, &target)?;
        let imported = registry.resolve_grammar(&target).map_err(|err| {
            ParseError::new(
                format!("unknown grammar import '{alias}' targeting '{target}': {err}"),
                0,
                0,
            )
            .with_code("unknown_import")
        })?;
        let nested = hydrate_imports_from_registry_impl(&imported, registry, active_targets);
        active_targets.pop();
        let nested = nested?;
        hydrated.imports.insert(alias, Box::new(nested));
    }

    Ok(hydrated)
}

fn push_import_target(
    active_targets: &mut Vec<String>,
    alias: &str,
    target: &str,
) -> Result<(), ParseError> {
    if let Some(index) = active_targets.iter().position(|active| active == target) {
        let mut cycle = active_targets[index..].to_vec();
        cycle.push(target.to_string());
        return Err(ParseError::new(
            format!(
                "cyclic grammar import through alias '{alias}' targeting '{target}': {}",
                cycle.join(" -> ")
            ),
            0,
            0,
        )
        .with_code("import_cycle"));
    }
    active_targets.push(target.to_string());
    Ok(())
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

pub(crate) fn metadata_import_targets(
    grammar: &Grammar,
) -> Result<Vec<(String, String)>, ParseError> {
    let mut imports = Vec::new();
    let Some(gmeta) = grammar.metadata.get("__grammar__") else {
        return Ok(imports);
    };
    let Some(value) = gmeta.get("imports") else {
        return Ok(imports);
    };
    let Some(map) = value.as_object() else {
        return Err(ParseError::new(
            "__grammar__.imports must be an object mapping aliases to grammar names",
            0,
            0,
        )
        .with_code("invalid_import_metadata"));
    };
    for (alias, target) in map {
        if alias.is_empty() {
            return Err(
                ParseError::new("__grammar__.imports alias must be non-empty", 0, 0)
                    .with_code("invalid_import_metadata"),
            );
        }
        let Some(target) = target.as_str() else {
            return Err(ParseError::new(
                format!("__grammar__.imports.{alias} must be a string grammar name"),
                0,
                0,
            )
            .with_code("invalid_import_metadata"));
        };
        if target.is_empty() {
            return Err(ParseError::new(
                format!("__grammar__.imports.{alias} must be non-empty"),
                0,
                0,
            )
            .with_code("invalid_import_metadata"));
        }
        imports.push((alias.clone(), target.to_string()));
    }
    imports.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    imports.dedup();
    Ok(imports)
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
        | PegExpr::NoTrivia(n)
        | PegExpr::WithTrivia { expr: n, .. } => collect_import_aliases(n, out),
        PegExpr::Named { expr: n, .. }
        | PegExpr::Expected { expr: n, .. }
        | PegExpr::SemanticAction { expr: n, .. }
        | PegExpr::Capture { expr: n, .. } => collect_import_aliases(n, out),
        PegExpr::SepOneOrMore { element, separator }
        | PegExpr::Interspersed { element, separator } => {
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
