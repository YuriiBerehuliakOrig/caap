/// Helper utilities and value-conversion functions for the compiler query CTFE builtins.
use indexmap::IndexMap;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use caap_peg::ParserConfig;

use crate::artifacts::{
    ArtifactFingerprint, ArtifactInvalidationRecord, ArtifactKey, ArtifactValue, SourceOrigin,
};
use crate::builtins::compiler_registry::require_named_string;
use crate::compiler::{
    CompilerBridgeValue, EvaluationCapture, QueryArtifactProjection, QueryArtifactSource,
    QueryExecutionOptions, QueryPlanStep, QueryProvider, QueryProviderExecutionRecord,
    QueryProviderSchedule, QueryStageSpec, SemanticPolicyRegistration, UnitBridgeValue,
};
use crate::diagnostics::{Diagnostic, DiagnosticFix, DiagnosticFrame};
use crate::error::{CaapError, CaapResult};
use crate::eval::Evaluator;
use crate::frontend::{parse, parse_with_source_path, parsed_source_to_ir};
use crate::graph::IRGraph;
use crate::ir::{IrLiteralData, Node, NodeId};
use crate::semantic::{PhasePolicy, SemanticValue};
use crate::surface_syntax::{
    compile_surface_grammar_from_syntax_state, parse_value_to_parsed_source, SurfaceBuiltinDriver,
};
use crate::unit::{Unit, UnitSyntaxState};
use crate::values::{eval_err, Environment, EvalSignal, MapKey, RuntimeValue};

use super::semantic_projection::{
    effect_policy_runtime_value, optional_runtime_phase_policy, semantic_value_to_plain_runtime,
};

// ---------------------------------------------------------------------------
// Projected types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DirEntryProjection {
    pub(super) name: String,
    pub(super) path: String,
    pub(super) kind: String,
    pub(super) is_file: bool,
    pub(super) is_dir: bool,
    pub(super) is_symlink: bool,
}

#[derive(Default)]
pub(super) struct SurfaceLoadOptions {
    pub(super) unit_id: Option<String>,
    pub(super) syntax_units: Vec<Unit>,
    pub(super) hooks: HashMap<String, RuntimeValue>,
    pub(super) leading_parenthesized_forms_only: bool,
    pub(super) leading_parenthesized_heads: BTreeSet<String>,
}

// ---------------------------------------------------------------------------
// Directory helpers
// ---------------------------------------------------------------------------

pub(super) fn list_dir(path: &Path) -> CaapResult<Vec<DirEntryProjection>> {
    let root = std::fs::canonicalize(path).map_err(|error| {
        CaapError::compiler(format!("directory path resolution failed: {error}"))
    })?;
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&root)
        .map_err(|error| CaapError::compiler(format!("directory read failed: {error}")))?
    {
        let entry = entry.map_err(|error| {
            CaapError::compiler(format!("directory entry read failed: {error}"))
        })?;
        entries.push(dir_entry_projection(entry.path())?);
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

fn dir_entry_projection(path: PathBuf) -> CaapResult<DirEntryProjection> {
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        CaapError::compiler(format!("directory entry metadata failed: {error}"))
    })?;
    let is_file = metadata.is_file();
    let is_dir = metadata.is_dir();
    let is_symlink = metadata.file_type().is_symlink();
    let name = path
        .file_name()
        .ok_or_else(|| CaapError::compiler("directory entry has no final path component"))?;
    Ok(DirEntryProjection {
        name: path_component_to_string(name, "directory entry name")?,
        path: path_to_string(&path, "directory entry path")?,
        kind: directory_entry_kind(is_file, is_dir, is_symlink).to_string(),
        is_file,
        is_dir,
        is_symlink,
    })
}

pub(super) fn directory_entry_kind(is_file: bool, is_dir: bool, is_symlink: bool) -> &'static str {
    if is_symlink {
        "symlink"
    } else if is_dir {
        "dir"
    } else if is_file {
        "file"
    } else {
        "other"
    }
}

fn path_component_to_string(component: &OsStr, ctx: &str) -> CaapResult<String> {
    component
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| CaapError::compiler(format!("{ctx} is not valid UTF-8")))
}

pub(super) fn path_to_string(path: &Path, ctx: &str) -> CaapResult<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| CaapError::compiler(format!("{ctx} is not valid UTF-8")))
}

pub(super) fn dir_entry_to_value(entry: &DirEntryProjection) -> RuntimeValue {
    map([
        ("name", string(entry.name.as_str())),
        ("path", string(entry.path.as_str())),
        ("kind", string(entry.kind.as_str())),
        ("is_file", RuntimeValue::Bool(entry.is_file)),
        ("is_dir", RuntimeValue::Bool(entry.is_dir)),
        ("is_symlink", RuntimeValue::Bool(entry.is_symlink)),
    ])
}

// ---------------------------------------------------------------------------
// Surface load options parsing
// ---------------------------------------------------------------------------

pub(super) fn surface_load_options(
    value: Option<&RuntimeValue>,
) -> Result<SurfaceLoadOptions, EvalSignal> {
    let Some(value) = value else {
        return Ok(SurfaceLoadOptions::default());
    };
    if matches!(value, RuntimeValue::Null) {
        return Ok(SurfaceLoadOptions::default());
    }
    let RuntimeValue::Map(fields) = value else {
        return Err(eval_err(
            "ctfe-compiler-load-surface-file-template expects an options map or null",
        ));
    };
    let mut options = SurfaceLoadOptions::default();
    for (key, value) in fields.borrow().iter() {
        let MapKey::Str(key) = key else {
            return Err(eval_err(
                "ctfe-compiler-load-surface-file-template option keys must be strings",
            ));
        };
        match key.as_ref() {
            "unit_id" => {
                options.unit_id = optional_named_string(
                    Some(value),
                    "ctfe-compiler-load-surface-file-template option unit_id must be a non-empty string or null",
                )?;
            }
            "syntax_units" => {
                if !matches!(value, RuntimeValue::Null) {
                    options.syntax_units = syntax_unit_sequence(
                        value,
                        "ctfe-compiler-load-surface-file-template option syntax_units must be unit handles",
                    )?;
                }
            }
            "hooks" => {
                if !matches!(value, RuntimeValue::Null) {
                    options.hooks = hook_value_map(
                        value,
                        "ctfe-compiler-load-surface-file-template option hooks must be a string-keyed map",
                    )?;
                }
            }
            "leading_parenthesized_forms_only" => {
                options.leading_parenthesized_forms_only = match value {
                    RuntimeValue::Bool(value) => *value,
                    RuntimeValue::Null => false,
                    _ => {
                        return Err(eval_err(
                            "ctfe-compiler-load-surface-file-template option leading_parenthesized_forms_only must be a boolean",
                        ));
                    }
                };
            }
            "leading_parenthesized_heads" => {
                options.leading_parenthesized_heads = string_sequence(
                    value,
                    "ctfe-compiler-load-surface-file-template option leading_parenthesized_heads must be a sequence",
                    "ctfe-compiler-load-surface-file-template option leading_parenthesized_heads entries must be non-empty strings",
                )?
                .into_iter()
                .collect();
            }
            other => {
                return Err(eval_err(format!(
                    "ctfe-compiler-load-surface-file-template unknown option {other:?}"
                )));
            }
        }
    }
    Ok(options)
}

// ---------------------------------------------------------------------------
// Stage / provider value converters
// ---------------------------------------------------------------------------

pub(super) fn stage_to_value(stage: &QueryStageSpec) -> RuntimeValue {
    map([
        ("name", string(stage.name.clone())),
        ("requires", string_tuple(stage.requires.iter())),
        ("phase_policy", string(stage.phase_policy.as_str())),
        ("input_kinds", string_tuple(stage.input_kinds.iter())),
        ("family", optional_string(stage.family_label.as_deref())),
        (
            "terminal_target_aliases",
            string_tuple(stage.aliases.iter()),
        ),
        (
            "restart_stage",
            optional_string(stage.restart_stage.as_deref()),
        ),
    ])
}

/// Parse `text` under the merged grammar of `syntax_units` (with their inline
/// lower hooks) — the textual core shared by the file-template loader and
/// `ctfe_grammar_parse_forms`. A grammar/hook/setup problem is a hard `Err`;
/// a PARSE failure of `text` itself is the recoverable `Ok(Err(message))`
/// arm so one-call API consumers can surface it as data.
pub(super) fn parse_dynamic_surface_text(
    bridge: &CompilerBridgeValue,
    text: &str,
    source_label: &str,
    syntax_units: Vec<Unit>,
    mut hooks: HashMap<String, RuntimeValue>,
    start_rule: Option<&str>,
) -> Result<Result<caap_peg::ParseValue, String>, EvalSignal> {
    emit_dynamic_source_event(
        bridge,
        "start",
        source_label,
        [("bytes".to_string(), text.len().to_string())],
    )?;
    let mut parser_syntax = UnitSyntaxState::new("caap.dynamic").map_err(eval_err)?;

    for syntax_unit in syntax_units {
        let syntax_unit_id = syntax_unit.unit_id().to_string();
        emit_dynamic_source_event(
            bridge,
            "syntax_import",
            source_label,
            [("unit".to_string(), syntax_unit_id.clone())],
        )?;
        merge_syntax_state(&mut parser_syntax, syntax_unit.syntax_state()).map_err(eval_err)?;
        collect_inline_syntax_hooks(bridge, &syntax_unit, &mut hooks).map_err(eval_err)?;
    }

    let mut grammar =
        compile_surface_grammar_from_syntax_state(&parser_syntax).map_err(eval_err)?;
    if let Some(start) = start_rule {
        // peg defers start-rule validation to parse time; a missing start is a
        // SETUP mistake, so surface it as a hard error here, not as parse data.
        if grammar.get_rule(start).is_none() {
            return Err(eval_err(format!(
                "dynamic grammar start rule {start:?} is not defined by the merged syntax units"
            )));
        }
        grammar = grammar
            .try_with_start_rule(start)
            .map_err(|error| eval_err(format!("dynamic grammar start rule {start:?}: {error}")))?;
    }
    emit_dynamic_source_event(
        bridge,
        "grammar",
        source_label,
        [
            (
                "rules".to_string(),
                parser_syntax.grammar_rules.len().to_string(),
            ),
            ("hooks".to_string(), hooks.len().to_string()),
        ],
    )?;
    let (parse_text, source_offset) = trimmed_source_with_offset(text);
    let runtime = SurfaceBuiltinDriver::new(text, Some(source_label.to_string()))
        .with_source_offset(source_offset)
        .with_trivia(crate::surface_syntax::driver_trivia_from_syntax_state(
            &parser_syntax,
        ))
        .with_hooks(hooks);
    let parser_config =
        ParserConfig::default().with_max_steps(text.len().saturating_add(65_536).max(65_536));
    emit_dynamic_source_event(bridge, "parse_start", source_label, std::iter::empty())?;
    let parse_value = match caap_peg::ParseRequest::new(&grammar)
        .config(parser_config)
        .driver(&runtime)
        .run(parse_text)
    {
        Ok(value) => value,
        Err(error) => {
            return Ok(Err(format!(
                "parse failed at {}..{} found {:?} stack {:?}: {}",
                error.span.start, error.span.end, error.found, error.rule_stack, error.message
            )))
        }
    };
    emit_dynamic_source_event(bridge, "parse_finish", source_label, std::iter::empty())?;
    if let Some(error) = runtime.error() {
        return Ok(Err(error));
    }
    Ok(Ok(parse_value))
}

pub(super) fn load_dynamic_surface_file_template(
    bridge: &CompilerBridgeValue,
    path: &Path,
    unit_id: &str,
    syntax_units: Vec<Unit>,
    hooks: HashMap<String, RuntimeValue>,
) -> Result<UnitBridgeValue, EvalSignal> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| eval_err(format!("dynamic source read failed: {error}")))?;
    let source_path = path_to_string(path, "dynamic source path").map_err(eval_err)?;
    let parse_value =
        parse_dynamic_surface_text(bridge, &text, &source_path, syntax_units, hooks, None)?
            .map_err(|message| eval_err(format!("dynamic source {message}")))?;
    emit_dynamic_source_event(bridge, "decode_start", &source_path, std::iter::empty())?;
    let parsed = parse_value_to_parsed_source(&parse_value).map_err(eval_err)?;
    emit_dynamic_source_event(
        bridge,
        "decode_finish",
        &source_path,
        [("forms".to_string(), parsed.forms.len().to_string())],
    )?;
    emit_dynamic_source_event(bridge, "lower_start", &source_path, std::iter::empty())?;
    let graph = parsed_source_to_ir(&parsed).map_err(eval_err)?;
    emit_dynamic_source_event(
        bridge,
        "lower_finish",
        &source_path,
        [("nodes".to_string(), graph.node_count().to_string())],
    )?;
    let mut unit = Unit::from_graph(unit_id.to_string(), graph).map_err(eval_err)?;
    let syntax = UnitSyntaxState::new("caap")
        .map_err(eval_err)?
        .with_source(
            source_path,
            ArtifactFingerprint::sha256(text.as_bytes()).to_string(),
        )
        .map_err(eval_err)?;
    unit.set_syntax_state(syntax).map_err(eval_err)?;
    Ok(UnitBridgeValue::from_unit_snapshot(unit))
}

pub(super) fn load_leading_parenthesized_surface_file_template(
    path: &Path,
    unit_id: Option<String>,
    heads: &BTreeSet<String>,
) -> Result<UnitBridgeValue, EvalSignal> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| eval_err(format!("surface declaration source read failed: {error}")))?;
    let source_path = path_to_string(path, "surface declaration source path").map_err(eval_err)?;
    let prefix = leading_parenthesized_forms_prefix(&text, heads)?;
    let graph = parse_with_source_path(prefix, &source_path).map_err(eval_err)?;
    let unit_id = unit_id.unwrap_or_else(|| format!("{source_path}#leading-parenthesized-forms"));
    let unit = Unit::from_graph(unit_id, graph).map_err(eval_err)?;
    Ok(UnitBridgeValue::from_unit_snapshot(unit))
}

fn leading_parenthesized_forms_prefix<'a>(
    source: &'a str,
    heads: &BTreeSet<String>,
) -> Result<&'a str, EvalSignal> {
    let mut index = 0;
    let mut end = 0;
    loop {
        index = skip_surface_trivia(source, index)?;
        if source[index..].starts_with('(') {
            let form_start = index;
            let form_end = consume_parenthesized_form(source, index)?;
            if !heads.is_empty() {
                let Some(head) = parenthesized_form_head(source, form_start, form_end)? else {
                    break;
                };
                if !heads.contains(head) {
                    break;
                }
            }
            index = form_end;
            end = form_end;
            continue;
        }
        break;
    }
    Ok(&source[..end])
}

fn skip_surface_trivia(source: &str, index: usize) -> Result<usize, EvalSignal> {
    // Shares the default-trivia marker knowledge with the frontend reader; here
    // an unterminated block comment in a source declaration is an error.
    crate::frontend::leading_trivia_len(&source[index..])
        .map(|len| index + len)
        .map_err(|marker| {
            eval_err(format!(
                "unterminated {marker} block comment in source declarations"
            ))
        })
}

fn consume_parenthesized_form(source: &str, mut index: usize) -> Result<usize, EvalSignal> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    while index < source.len() {
        let rest = &source[index..];
        if in_string {
            let ch = rest
                .chars()
                .next()
                .ok_or_else(|| eval_err("unterminated string in source declaration"))?;
            index += ch.len_utf8();
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if rest.starts_with(';') {
            index = rest
                .find('\n')
                .map(|offset| index + offset + 1)
                .unwrap_or(source.len());
            continue;
        }
        if rest.starts_with("#|") {
            let Some(offset) = rest.find("|#") else {
                return Err(eval_err(
                    "unterminated #| |# block comment in source declaration",
                ));
            };
            index += offset + 2;
            continue;
        }
        if rest.starts_with("/*") {
            let Some(offset) = rest.find("*/") else {
                return Err(eval_err(
                    "unterminated /* */ block comment in source declaration",
                ));
            };
            index += offset + 2;
            continue;
        }
        let ch = rest
            .chars()
            .next()
            .ok_or_else(|| eval_err("unterminated source declaration"))?;
        index += ch.len_utf8();
        match ch {
            '"' => in_string = true,
            '(' => depth += 1,
            ')' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| eval_err("unbalanced ')' in source declaration"))?;
                if depth == 0 {
                    return Ok(index);
                }
            }
            _ => {}
        }
    }
    Err(eval_err("unterminated parenthesized source declaration"))
}

fn parenthesized_form_head(
    source: &str,
    form_start: usize,
    form_end: usize,
) -> Result<Option<&str>, EvalSignal> {
    let mut index = form_start + 1;
    index = skip_surface_trivia(&source[..form_end], index)?;
    if index >= form_end {
        return Ok(None);
    }
    let Some(first) = source[index..form_end].chars().next() else {
        return Ok(None);
    };
    if !is_surface_symbol_start(first) {
        return Ok(None);
    }
    let head_start = index;
    index += first.len_utf8();
    while index < form_end {
        let Some(ch) = source[index..form_end].chars().next() else {
            break;
        };
        if !is_surface_symbol_continue(ch) {
            break;
        }
        index += ch.len_utf8();
    }
    Ok(Some(&source[head_start..index]))
}

fn is_surface_symbol_start(ch: char) -> bool {
    ch.is_ascii_alphabetic()
        || matches!(
            ch,
            '_' | '+' | '-' | '*' | '/' | '<' | '>' | '=' | '!' | '?' | '$' | '%' | '&' | ':' | '.'
        )
}

fn is_surface_symbol_continue(ch: char) -> bool {
    is_surface_symbol_start(ch) || ch.is_ascii_digit()
}

fn trimmed_source_with_offset(source: &str) -> (&str, usize) {
    let trimmed = source.trim();
    let offset = if trimmed.is_empty() {
        0
    } else {
        source.len() - source.trim_start().len()
    };
    (trimmed, offset)
}

fn emit_dynamic_source_event(
    bridge: &CompilerBridgeValue,
    action: &str,
    source_path: &str,
    metadata: impl IntoIterator<Item = (String, String)>,
) -> Result<(), EvalSignal> {
    bridge
        .emit_event(
            "dynamic_source",
            action,
            "dynamic source unit loading",
            std::iter::once(("source".to_string(), source_path.to_string())).chain(metadata),
        )
        .map_err(eval_err)
}

fn merge_syntax_state(target: &mut UnitSyntaxState, source: &UnitSyntaxState) -> CaapResult<()> {
    for (name, rule) in &source.grammar_rules {
        target.set_grammar_rule(name.clone(), rule.clone())?;
    }
    for (key, value) in &source.grammar_metadata {
        target.set_grammar_metadata(key.clone(), value.clone())?;
    }
    for (name, params) in &source.grammar_rule_params {
        target.set_grammar_rule_params(name.clone(), params.clone())?;
    }
    Ok(())
}

fn collect_inline_syntax_hooks(
    bridge: &CompilerBridgeValue,
    syntax_unit: &Unit,
    hooks: &mut HashMap<String, RuntimeValue>,
) -> CaapResult<()> {
    let syntax = syntax_unit.syntax_state();
    let Some(value) = syntax.grammar_metadata("semantic_hook_inline_sources") else {
        return Ok(());
    };
    let SemanticValue::Map(entries) = value else {
        return Err(CaapError::semantic(
            "semantic_hook_inline_sources metadata must be a map",
        ));
    };
    let initial = syntax_unit_import_bindings(bridge, syntax_unit)?;
    for (hook_ref, source) in entries {
        let SemanticValue::Str(source) = source else {
            return Err(CaapError::semantic(
                "semantic_hook_inline_sources values must be strings",
            ));
        };
        hooks.insert(hook_ref.clone(), eval_inline_hook_source(source, &initial)?);
    }
    Ok(())
}

/// Resolve the names a grammar unit imports so they are in scope inside its
/// inline `lower` hooks.
///
/// Inline hooks are evaluated in an isolated environment (see
/// [`eval_inline_hook_source`]); the module's normal lexical import scope is not
/// available there. Link bindings are not populated on a syntax unit, so we
/// resolve imports the same way the named-hook path does (the loader's
/// import-symbol resolution): walk the unit's top-level `import-symbols` /
/// `import-namespace` forms and look each module up in the compiler value
/// registry (keyed by module name).
fn syntax_unit_import_bindings(
    bridge: &CompilerBridgeValue,
    syntax_unit: &Unit,
) -> CaapResult<Vec<(String, RuntimeValue)>> {
    let mut bindings = Vec::new();
    let graph = syntax_unit.ir();
    for &form_id in syntax_unit.top_level_form_ids() {
        let Some(Node::Call(call)) = graph.node(form_id) else {
            continue;
        };
        let Some(head) = call_head_name(graph, call.callee) else {
            continue;
        };
        let Some(module_name) = call.args.first().and_then(|&a| node_str_literal(graph, a)) else {
            continue;
        };
        let Some(module_value) = bridge.lookup_registered_value(module_name)? else {
            continue;
        };
        match head {
            // (import-symbols "module" "a" "b" …) → bind each exported member by name.
            "import_symbols" => {
                for &arg in &call.args[1..] {
                    let Some(name) = node_str_literal(graph, arg) else {
                        continue;
                    };
                    let value = if name == module_name {
                        module_value.clone()
                    } else {
                        match map_member(&module_value, name) {
                            Some(member) => member,
                            None => continue,
                        }
                    };
                    bindings.push((name.to_string(), value));
                }
            }
            // (import-namespace "module" "alias") → bind the whole exports map.
            "import_namespace" => {
                if let Some(alias) = call.args.get(1).and_then(|&a| node_str_literal(graph, a)) {
                    bindings.push((alias.to_string(), module_value));
                }
            }
            _ => {}
        }
    }
    Ok(bindings)
}

/// The identifier a call node is calling, if its callee is a plain name.
fn call_head_name(graph: &IRGraph, callee: NodeId) -> Option<&str> {
    match graph.node(callee) {
        Some(Node::Name(name)) => Some(name.identifier.as_ref()),
        _ => None,
    }
}

/// The string payload of a literal node, if `id` is a string literal.
fn node_str_literal(graph: &IRGraph, id: NodeId) -> Option<&str> {
    match graph.node(id) {
        Some(Node::Literal(literal)) => match &literal.value {
            IrLiteralData::Str(value) => Some(value.as_str()),
            _ => None,
        },
        _ => None,
    }
}

/// Clone the value stored under string `key` in a `RuntimeValue::Map`, if present.
fn map_member(value: &RuntimeValue, key: &str) -> Option<RuntimeValue> {
    let RuntimeValue::Map(fields) = value else {
        return None;
    };
    fields
        .borrow()
        .iter()
        .find_map(|(map_key, member)| match map_key {
            MapKey::Str(name) if name.as_ref() == key => Some(member.clone()),
            _ => None,
        })
}

fn eval_inline_hook_source(
    source: &str,
    initial: &[(String, RuntimeValue)],
) -> Result<RuntimeValue, EvalSignal> {
    let graph = parse(source).map_err(eval_err)?;
    let mut evaluator = Evaluator::new(graph);
    let env = evaluator.make_env();
    for (name, value) in initial {
        Environment::define(&env, name.clone(), value.clone());
    }
    let forms = evaluator.graph().top_level_form_ids().to_vec();
    evaluator.eval_top_level_sequence(&forms, &env)
}

pub(super) fn syntax_unit_sequence(
    value: &RuntimeValue,
    message: &str,
) -> Result<Vec<Unit>, EvalSignal> {
    match value {
        RuntimeValue::Tuple(items) => items
            .iter()
            .map(|item| {
                require_unit_bridge(item, message).map(UnitBridgeValue::clone_unit_snapshot)
            })
            .collect(),
        RuntimeValue::List(items) => items
            .borrow()
            .iter()
            .map(|item| {
                require_unit_bridge(item, message).map(UnitBridgeValue::clone_unit_snapshot)
            })
            .collect(),
        _ => Err(eval_err(message)),
    }
}

fn hook_value_map(
    value: &RuntimeValue,
    message: &str,
) -> Result<HashMap<String, RuntimeValue>, EvalSignal> {
    let RuntimeValue::Map(fields) = value else {
        return Err(eval_err(message));
    };
    let mut hooks = HashMap::new();
    for (key, value) in fields.borrow().iter() {
        let MapKey::Str(key) = key else {
            return Err(eval_err(message));
        };
        hooks.insert(key.to_string(), value.clone());
    }
    Ok(hooks)
}

// ---------------------------------------------------------------------------
// Argument parsing helpers
// ---------------------------------------------------------------------------

pub(super) fn optional_named_string(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Option<String>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(RuntimeValue::Str(value)) if !value.is_empty() => Ok(Some(value.to_string())),
        Some(RuntimeValue::Str(_)) => Err(eval_err(message)),
        Some(_) => Err(eval_err(message)),
    }
}

pub(super) fn string_sequence(
    value: &RuntimeValue,
    sequence_message: &str,
    item_message: &str,
) -> Result<Vec<String>, EvalSignal> {
    match value {
        RuntimeValue::Tuple(items) => items
            .iter()
            .map(|item| require_named_string(item, item_message))
            .collect(),
        RuntimeValue::List(items) => items
            .borrow()
            .iter()
            .map(|item| require_named_string(item, item_message))
            .collect(),
        _ => Err(eval_err(sequence_message)),
    }
}

pub(super) fn optional_string_sequence(
    value: Option<&RuntimeValue>,
    sequence_message: &str,
    item_message: &str,
) -> Result<Vec<String>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(Vec::new()),
        Some(value) => string_sequence(value, sequence_message, item_message),
    }
}

pub(super) fn phase_arg(value: &RuntimeValue, message: &str) -> Result<PhasePolicy, EvalSignal> {
    optional_runtime_phase_policy(Some(value), message)
        .map(|phase| phase.unwrap_or(PhasePolicy::CompileTime))
}

pub(super) fn phase_and_initial_options(
    args: &[RuntimeValue],
    phase_index: usize,
    initial_index: usize,
    phase_message: &str,
    initial_message: &str,
) -> Result<(PhasePolicy, QueryExecutionOptions), EvalSignal> {
    let phase = args
        .get(phase_index)
        .map(|value| phase_arg(value, phase_message))
        .transpose()?
        .unwrap_or(PhasePolicy::CompileTime);
    let initial = initial_bindings(args.get(initial_index), initial_message)?;
    Ok((
        phase,
        QueryExecutionOptions::new().with_initial_bindings(initial),
    ))
}

pub(super) fn initial_bindings(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<Vec<(String, RuntimeValue)>, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(Vec::new()),
        Some(RuntimeValue::Map(map)) => map
            .borrow()
            .iter()
            .map(|(key, value)| match key {
                MapKey::Str(name) if !name.is_empty() => Ok((name.to_string(), value.clone())),
                _ => Err(eval_err(message)),
            })
            .collect(),
        Some(_) => Err(eval_err(message)),
    }
}

pub(super) fn optional_nonnegative_usize(
    value: Option<&RuntimeValue>,
    message: &str,
) -> Result<usize, EvalSignal> {
    match value {
        None | Some(RuntimeValue::Null) => Ok(0),
        Some(RuntimeValue::Int(value)) if *value >= 0 => {
            usize::try_from(*value).map_err(|_| eval_err(message))
        }
        Some(_) => Err(eval_err(message)),
    }
}

pub(super) fn require_unit_bridge<'a>(
    value: &'a RuntimeValue,
    message: &str,
) -> Result<&'a UnitBridgeValue, EvalSignal> {
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    object
        .as_any()
        .downcast_ref::<UnitBridgeValue>()
        .ok_or_else(|| eval_err(message))
}

pub(super) fn query_artifact_source(
    value: &RuntimeValue,
) -> Result<QueryArtifactSource, EvalSignal> {
    query_artifact_source_with_message(
        value,
        "ctfe-compiler-query-execution expects a unit handle or path-like source",
    )
}

fn query_artifact_source_with_message(
    value: &RuntimeValue,
    message: &str,
) -> Result<QueryArtifactSource, EvalSignal> {
    if let RuntimeValue::Str(path) = value {
        return Ok(QueryArtifactSource::Path(path.to_string()));
    }
    let unit = require_unit_bridge(value, message)?;
    Ok(QueryArtifactSource::Unit(Box::new(
        unit.clone_unit_snapshot(),
    )))
}

pub(super) fn query_source_origin_to_value(source: &QueryArtifactSource) -> RuntimeValue {
    match source {
        QueryArtifactSource::Unit(unit) => {
            map([("kind", string("unit")), ("id", string(unit.unit_id()))])
        }
        QueryArtifactSource::Path(path) => {
            map([("kind", string("path")), ("path", string(path.as_str()))])
        }
        QueryArtifactSource::Text(text) => map([
            ("kind", string("text")),
            ("digest", string(short_source_digest(text))),
        ]),
    }
}

fn short_source_digest(text: &str) -> String {
    ArtifactFingerprint::sha256(text.as_bytes())
        .to_string()
        .chars()
        .take(12)
        .collect()
}

pub(super) fn evaluation_capture_to_value(capture: &EvaluationCapture) -> RuntimeValue {
    map([
        (
            "result",
            capture.value.clone().unwrap_or(RuntimeValue::Null),
        ),
        ("unit_id", string(capture.unit_id.as_str())),
        ("phase", string(capture.phase.as_str())),
        (
            "diagnostics",
            tuple(
                capture
                    .diagnostics
                    .iter()
                    .map(diagnostic_to_value)
                    .collect(),
            ),
        ),
        (
            "bindings",
            map_from_bindings(capture.bindings.iter().map(|(name, value)| (name, value))),
        ),
        (
            "skipped_forms",
            RuntimeValue::Int(capture.skipped_forms as i64),
        ),
    ])
}

pub(super) fn query_artifact_to_value(artifact: &QueryArtifactProjection) -> RuntimeValue {
    map([
        ("artifact_kind", string(artifact.artifact_kind.as_str())),
        ("stage", string(artifact.stage.as_str())),
        ("family", string(artifact.family.as_str())),
        ("phase", string(artifact.phase.as_str())),
        ("key", artifact_key_to_value(&artifact.key)),
        (
            "origin_key",
            artifact
                .origin_key
                .as_ref()
                .map(artifact_key_to_value)
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "dependencies",
            tuple(
                artifact
                    .dependencies
                    .iter()
                    .map(artifact_key_to_value)
                    .collect(),
            ),
        ),
        (
            "diagnostics",
            tuple(
                artifact
                    .diagnostics
                    .iter()
                    .map(diagnostic_to_value)
                    .collect(),
            ),
        ),
        (
            "execution_diagnostics",
            tuple(
                artifact
                    .execution_diagnostics
                    .iter()
                    .map(diagnostic_to_value)
                    .collect(),
            ),
        ),
        ("iterations", RuntimeValue::Int(artifact.iterations as i64)),
        (
            "execution_summary",
            tuple(
                artifact
                    .execution_summary
                    .iter()
                    .map(provider_execution_record_to_value)
                    .collect(),
            ),
        ),
        (
            "reads_subjects",
            tuple(artifact.reads_subjects.iter().map(string).collect()),
        ),
        (
            "writes_subjects",
            tuple(artifact.writes_subjects.iter().map(string).collect()),
        ),
        (
            "read_cells",
            tuple(artifact.read_cells.iter().map(string).collect()),
        ),
        (
            "write_cells",
            tuple(artifact.write_cells.iter().map(string).collect()),
        ),
        (
            "reads_files",
            tuple(artifact.reads_files.iter().map(string).collect()),
        ),
        (
            "writes_files",
            tuple(artifact.writes_files.iter().map(string).collect()),
        ),
        ("value", artifact_value_to_value(&artifact.value)),
    ])
}

pub(super) fn invalidation_plan_step_to_value(
    step: &QueryPlanStep,
    invalidation: Option<&ArtifactInvalidationRecord>,
) -> RuntimeValue {
    map([
        ("stage", string(step.stage.as_str())),
        ("family", RuntimeValue::Null),
        (
            "key",
            step.artifact_key
                .as_ref()
                .map(artifact_key_to_value)
                .unwrap_or(RuntimeValue::Null),
        ),
        ("cached", RuntimeValue::Bool(step.cached)),
        (
            "invalidation",
            invalidation
                .map(invalidation_record_to_value)
                .unwrap_or(RuntimeValue::Null),
        ),
    ])
}

fn invalidation_record_to_value(record: &ArtifactInvalidationRecord) -> RuntimeValue {
    map([
        ("reason_kind", string(record.reason_kind.as_str())),
        (
            "lineage_kind",
            optional_string(record.lineage_kind.as_deref()),
        ),
        (
            "invalidated_key",
            artifact_key_to_value(&record.invalidated_key),
        ),
        (
            "replacement_key",
            record
                .replacement_key
                .as_ref()
                .map(artifact_key_to_value)
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "upstream_key",
            record
                .upstream_key
                .as_ref()
                .map(artifact_key_to_value)
                .unwrap_or(RuntimeValue::Null),
        ),
        (
            "changed_inputs",
            tuple(record.changed_inputs.iter().map(string).collect()),
        ),
    ])
}

pub(super) fn provider_schedule_to_value(
    bridge: &CompilerBridgeValue,
    stage: &str,
    groups: &QueryProviderSchedule,
) -> RuntimeValue {
    map([
        ("stage", string(stage)),
        (
            "groups",
            tuple(
                groups
                    .groups
                    .iter()
                    .enumerate()
                    .map(|(index, providers)| {
                        map([
                            ("index", RuntimeValue::Int(index as i64)),
                            (
                                "providers",
                                tuple(
                                    providers
                                        .iter()
                                        .map(|provider| {
                                            schedule_provider_to_value(
                                                provider,
                                                bridge
                                                    .provider_dynamic_requires_for(&provider.name),
                                            )
                                        })
                                        .collect(),
                                ),
                            ),
                            (
                                "barrier_after",
                                provider_schedule_barrier_to_value(
                                    index,
                                    groups.barriers.get(index).and_then(Option::as_ref),
                                ),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

pub(super) fn require_string_set(
    value: &RuntimeValue,
    message: &str,
) -> Result<BTreeSet<String>, EvalSignal> {
    let values: Vec<RuntimeValue> = match value {
        RuntimeValue::Null => return Ok(BTreeSet::new()),
        RuntimeValue::List(items) => items.borrow().iter().cloned().collect(),
        RuntimeValue::Tuple(items) => items.iter().cloned().collect(),
        _ => return Err(eval_err(message)),
    };
    values
        .into_iter()
        .map(|value| match value {
            RuntimeValue::Str(name) if !name.is_empty() => Ok(name.to_string()),
            _ => Err(eval_err(message)),
        })
        .collect()
}

fn provider_schedule_barrier_to_value(index: usize, reasons: Option<&Vec<String>>) -> RuntimeValue {
    match reasons {
        Some(reasons) => map([
            ("next_group_index", RuntimeValue::Int(index as i64 + 1)),
            ("reasons", string_tuple(reasons.iter())),
        ]),
        None => RuntimeValue::Null,
    }
}

pub(super) fn schedule_provider_to_value(
    provider: &QueryProvider,
    dynamic_requires: Vec<String>,
) -> RuntimeValue {
    map([
        ("name", string(provider.name.clone())),
        ("stage", string(provider.stage.clone())),
        ("family", optional_string(provider.family.as_deref())),
        ("phase_policy", string(provider.phase_policy.as_str())),
        ("internal", RuntimeValue::Bool(false)),
        (
            "effects",
            map([
                ("reads", string_tuple(provider.reads.iter())),
                ("writes", string_tuple(provider.writes.iter())),
                ("emits", string_tuple(provider.effect_tags.iter_strs())),
                ("uses", tuple(Vec::new())),
            ]),
        ),
        ("requires", string_tuple(provider.requires.iter())),
        ("dynamic_requires", string_tuple(dynamic_requires.iter())),
        ("requires_data", string_tuple(provider.requires_data.iter())),
        ("provides_data", string_tuple(provider.provides_data.iter())),
        (
            "input_schema",
            optional_string(provider.input_schema.as_deref()),
        ),
        ("reads", string_tuple(provider.reads.iter())),
        ("writes", string_tuple(provider.writes.iter())),
        ("cache_scope", string(provider.cache_scope.as_str())),
        ("resume_policy", string(provider.resume_policy.as_str())),
    ])
}

fn provider_execution_record_to_value(record: &QueryProviderExecutionRecord) -> RuntimeValue {
    map([
        (
            "recorded_at_unix_ns",
            RuntimeValue::Int(record.recorded_at_unix_ns),
        ),
        ("provider_name", string(record.provider_name.as_str())),
        ("stage", string(record.stage.as_str())),
        ("family", optional_string(record.family.as_deref())),
        (
            "provider_contract",
            provider_execution_contract_to_value(record),
        ),
        ("iteration", RuntimeValue::Int(record.iteration as i64)),
        ("changed", RuntimeValue::Bool(record.changed)),
        (
            "diagnostics_emitted",
            RuntimeValue::Int(record.diagnostics_emitted as i64),
        ),
        ("rolled_back", RuntimeValue::Bool(record.rolled_back)),
        (
            "stopped_by_error",
            RuntimeValue::Bool(record.stopped_by_error),
        ),
        ("outcome_kind", string(record.outcome_kind.as_str())),
        (
            "diagnostic_codes",
            tuple(record.diagnostic_codes.iter().map(string).collect()),
        ),
        (
            "artifact_dependencies",
            tuple(
                record
                    .artifact_dependencies
                    .iter()
                    .map(artifact_key_to_value)
                    .collect(),
            ),
        ),
        (
            "rewrite_count",
            RuntimeValue::Int(record.rewrite_count as i64),
        ),
        (
            "erased_count",
            RuntimeValue::Int(record.erased_count as i64),
        ),
        (
            "touched_node_kinds",
            tuple(record.touched_node_kinds.iter().map(string).collect()),
        ),
        (
            "reads_subjects",
            tuple(record.reads_subjects.iter().map(string).collect()),
        ),
        (
            "writes_subjects",
            tuple(record.writes_subjects.iter().map(string).collect()),
        ),
        (
            "read_cells",
            tuple(record.read_cells.iter().map(string).collect()),
        ),
        (
            "write_cells",
            tuple(record.write_cells.iter().map(string).collect()),
        ),
        (
            "reads_files",
            tuple(record.reads_files.iter().map(string).collect()),
        ),
        (
            "writes_files",
            tuple(record.writes_files.iter().map(string).collect()),
        ),
        (
            "change_domains",
            tuple(record.change_domains.iter().map(string).collect()),
        ),
        (
            "restart_requested",
            RuntimeValue::Bool(record.restart_requested),
        ),
        (
            "restart_stage",
            optional_string(record.restart_stage.as_deref()),
        ),
        (
            "outcome_summary",
            map_from_string_pairs(record.outcome_summary.iter()),
        ),
    ])
}

fn provider_execution_contract_to_value(record: &QueryProviderExecutionRecord) -> RuntimeValue {
    map([
        ("phase_policy", string(record.phase_policy.as_str())),
        ("internal", RuntimeValue::Bool(false)),
        (
            "effects",
            map([
                ("reads", string_tuple(record.reads.iter())),
                ("writes", string_tuple(record.writes.iter())),
                ("emits", string_tuple(record.effect_tags.iter_strs())),
                ("uses", tuple(Vec::new())),
            ]),
        ),
        ("requires", string_tuple(record.requires.iter())),
        ("requires_data", string_tuple(record.requires_data.iter())),
        ("provides_data", string_tuple(record.provides_data.iter())),
        ("reads", string_tuple(record.reads.iter())),
        ("writes", string_tuple(record.writes.iter())),
        ("cache_scope", string(record.cache_scope.as_str())),
        ("resume_policy", string(record.resume_policy.as_str())),
    ])
}

fn artifact_key_to_value(key: &ArtifactKey) -> RuntimeValue {
    tuple(key.parts().iter().map(string).collect())
}

fn artifact_value_to_value(value: &ArtifactValue) -> RuntimeValue {
    match value {
        ArtifactValue::Text(text) => map([("kind", string("text")), ("value", string(text))]),
        ArtifactValue::Bytes(bytes) => map([
            ("kind", string("bytes")),
            (
                "value",
                tuple(
                    bytes
                        .iter()
                        .map(|byte| RuntimeValue::Int(*byte as i64))
                        .collect(),
                ),
            ),
        ]),
        ArtifactValue::Source(source) => {
            let (origin_kind, origin_value) = match &source.origin {
                SourceOrigin::Inline { label } => ("inline", label.as_str()),
                SourceOrigin::Path { path, .. } => ("path", path.as_str()),
            };
            map([
                ("kind", string("source")),
                ("origin_kind", string(origin_kind)),
                ("origin", string(origin_value)),
                ("fingerprint", string(source.fingerprint.as_str())),
                ("text", string(source.text.as_str())),
            ])
        }
        ArtifactValue::SourceTemplate(cached) => {
            let (origin_kind, origin_value) = match &cached.source.origin {
                SourceOrigin::Inline { label } => ("inline", label.as_str()),
                SourceOrigin::Path { path, .. } => ("path", path.as_str()),
            };
            map([
                ("kind", string("source_template")),
                ("origin_kind", string(origin_kind)),
                ("origin", string(origin_value)),
                ("fingerprint", string(cached.source.fingerprint.as_str())),
                ("text", string(cached.source.text.as_str())),
                ("unit_id", string(cached.template.unit_id.as_str())),
            ])
        }
        ArtifactValue::QueryStage(cached) => map([
            ("kind", string("semantic")),
            ("value", semantic_value_to_plain_runtime(&cached.summary)),
        ]),
        ArtifactValue::Semantic(value) => map([
            ("kind", string("semantic")),
            ("value", semantic_value_to_plain_runtime(value)),
        ]),
    }
}

pub(super) fn semantic_policy_to_value(policy: &SemanticPolicyRegistration) -> RuntimeValue {
    map([
        ("name", string(policy.name.as_str())),
        ("source", string("registered")),
        ("phase_policy", string(policy.phase_policy.as_str())),
        (
            "effect_policy",
            effect_policy_runtime_value(&policy.effect_policy),
        ),
        ("eval_policy", string(policy.eval_policy.as_str())),
        ("control_policy", string(policy.control_policy.as_str())),
        ("scope_policy", string(policy.scope_policy.as_str())),
        ("fold_policy", string(policy.fold_policy.as_str())),
        ("form_policy", string(policy.form_policy.as_str())),
        ("has_normalizer", RuntimeValue::Bool(true)),
        ("unit_id", optional_string(policy.unit_id.as_deref())),
        ("stable_id", optional_string(policy.stable_id.as_deref())),
    ])
}

pub(super) fn diagnostic_to_value(diagnostic: &Diagnostic) -> RuntimeValue {
    map([
        ("severity", string(diagnostic.severity.as_str())),
        ("message", string(diagnostic.message.as_str())),
        ("code", optional_string(diagnostic.code.as_deref())),
        ("label", optional_string(diagnostic.label.as_deref())),
        ("location", optional_string(diagnostic.location.as_deref())),
        (
            "notes",
            tuple(diagnostic.notes.iter().map(string).collect()),
        ),
        ("help", tuple(diagnostic.help.iter().map(string).collect())),
        (
            "context",
            tuple(diagnostic.context.iter().map(string).collect()),
        ),
        (
            "fixes",
            tuple(
                diagnostic
                    .fixes
                    .iter()
                    .map(diagnostic_fix_to_value)
                    .collect(),
            ),
        ),
        (
            "stack_trace",
            tuple(
                diagnostic
                    .stack_trace
                    .iter()
                    .map(diagnostic_frame_to_value)
                    .collect(),
            ),
        ),
    ])
}

fn diagnostic_fix_to_value(fix: &DiagnosticFix) -> RuntimeValue {
    map([
        ("label", string(fix.label.as_str())),
        ("kind", string(fix.kind.as_str())),
        ("metadata", map_from_string_pairs(&fix.metadata)),
    ])
}

fn diagnostic_frame_to_value(frame: &DiagnosticFrame) -> RuntimeValue {
    map([
        ("name", string(frame.name.as_str())),
        ("location", optional_string(frame.location.as_deref())),
    ])
}

// ---------------------------------------------------------------------------
// Value construction primitives
// ---------------------------------------------------------------------------

pub(super) fn map<const N: usize>(entries: [(&str, RuntimeValue); N]) -> RuntimeValue {
    let mut map = IndexMap::new();
    for (key, value) in entries {
        map.insert(MapKey::Str(key.into()), value);
    }
    RuntimeValue::Map(Rc::new(RefCell::new(map)))
}

fn map_from_string_pairs<'a>(
    entries: impl IntoIterator<Item = &'a (String, String)>,
) -> RuntimeValue {
    let mut map = IndexMap::new();
    for (key, value) in entries {
        map.insert(MapKey::Str(key.clone().into()), string(value));
    }
    RuntimeValue::Map(Rc::new(RefCell::new(map)))
}

fn map_from_bindings<'a>(
    entries: impl IntoIterator<Item = (&'a String, &'a RuntimeValue)>,
) -> RuntimeValue {
    let mut map = IndexMap::new();
    for (key, value) in entries {
        map.insert(MapKey::Str(key.clone().into()), value.clone());
    }
    RuntimeValue::Map(Rc::new(RefCell::new(map)))
}

pub(super) use super::args::{string, tuple};
// Optional-string *encoder* (Option<&str> → value); canonical name is
// `optional_string_value`, kept under the local name for call sites.
pub(super) use super::args::optional_string_value as optional_string;

pub(super) fn string_tuple<S: AsRef<str>>(items: impl IntoIterator<Item = S>) -> RuntimeValue {
    tuple(items.into_iter().map(string).collect())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn compiler_directory_projection_rejects_non_utf8_entry_names() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "caap-query-dir-non-utf8-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let invalid_name = PathBuf::from(OsString::from_vec(b"bad-\xFF".to_vec()));
        std::fs::write(root.join(invalid_name), b"x").unwrap();

        let error = list_dir(&root).unwrap_err().to_string();

        std::fs::remove_dir_all(root).unwrap();
        assert!(error.contains("directory entry name is not valid UTF-8"));
    }

    #[test]
    fn compiler_directory_projection_classifies_symlink_and_other_entries() {
        assert_eq!(directory_entry_kind(true, false, false), "file");
        assert_eq!(directory_entry_kind(false, true, false), "dir");
        assert_eq!(directory_entry_kind(false, false, true), "symlink");
        assert_eq!(directory_entry_kind(false, false, false), "other");
    }

    #[test]
    fn optional_nonnegative_usize_rejects_negative_values() {
        let err = optional_nonnegative_usize(
            Some(&RuntimeValue::Int(-1)),
            "expected non-negative integer",
        )
        .unwrap_err();

        assert!(err.to_string().contains("expected non-negative integer"));
    }

    #[test]
    fn trimmed_source_with_offset_uses_leading_trivia_length() {
        let source = " \n\t(module \"demo\")\n(module \"demo\")\t";

        let (trimmed, offset) = trimmed_source_with_offset(source);

        assert_eq!(trimmed, "(module \"demo\")\n(module \"demo\")");
        assert_eq!(offset, " \n\t".len());
    }
}
