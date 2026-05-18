/// Compiler query graph/list CTFE builtins — Rust port of
/// `caap/builtins/compiler/compiler_graphs.py`.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use sha2::{Digest, Sha256};

use crate::artifacts::{ArtifactInvalidationRecord, ArtifactKey, ArtifactValue, SourceOrigin};
use crate::builtins::compiler_registry::{require_compiler_bridge, require_named_string};
use crate::compiler::{
    CompilerBridgeValue, QueryArtifactProjection, QueryArtifactSource, QueryExecutionOptions,
    QueryPlanStep, QueryProvider, QueryStageSpec, UnitBridgeValue,
};
use crate::diagnostics::{Diagnostic, DiagnosticFix};
use crate::eval::{eval_args, Evaluator};
use crate::semantic::{PhasePolicy, SemanticValue};
use crate::values::{eval_err, BuiltinInfo, EvalSignal, MapKey, RuntimeValue};

pub fn register(ev: &mut Evaluator) {
    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-list-stages".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-list-stages expects a compiler bridge",
            )?;
            Ok(tuple(
                bridge.list_stages().iter().map(stage_to_value).collect(),
            ))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-list-providers".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-list-providers expects a compiler bridge",
            )?;
            let filter = match args.get(1) {
                None | Some(RuntimeValue::Null) => None,
                Some(value) => Some(require_named_string(
                    value,
                    "ctfe-compiler-list-providers expects a valid stage or target alias when provided",
                )?),
            };
            let providers = bridge.list_providers(filter).map_err(eval_err)?;
            Ok(tuple(providers.iter().map(provider_to_value).collect()))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-graph-query".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(5),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-graph-query expects a compiler bridge",
            )?;
            let target = require_named_string(
                &args[1],
                "ctfe-compiler-graph-query expects a non-empty target name",
            )?;
            let source = query_artifact_source(
                &args[2],
                "ctfe-compiler-graph-query expects a unit handle or path-like source",
            )?;
            let (phase, options) = phase_and_initial_options(
                &args,
                3,
                4,
                "ctfe-compiler-graph-query expects a valid phase",
                "ctfe-compiler-graph-query expects an initial bindings map when provided",
            )?;
            compiler_graph_query(bridge, target, source, phase, options)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-graph-lineage".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 3,
        max_arity: Some(5),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let bridge = require_compiler_bridge(
                &args[0],
                "ctfe-compiler-graph-lineage expects a compiler bridge",
            )?;
            let target = require_named_string(
                &args[1],
                "ctfe-compiler-graph-lineage expects a non-empty target name",
            )?;
            let source = query_artifact_source(
                &args[2],
                "ctfe-compiler-graph-lineage expects a unit handle or path-like source",
            )?;
            let (phase, options) = phase_and_initial_options(
                &args,
                3,
                4,
                "ctfe-compiler-graph-lineage expects a valid phase",
                "ctfe-compiler-graph-lineage expects an initial bindings map when provided",
            )?;
            compiler_graph_lineage(bridge, target, source, phase, options)
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-graph-node-labels".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let graph = require_map(
                &args[0],
                "ctfe-compiler-graph-node-labels expects a graph map",
            )?;
            let kind = require_named_string(
                &args[1],
                "ctfe-compiler-graph-node-labels expects a non-empty node kind",
            )?;
            let nodes = graph_sequence(
                &graph.borrow(),
                "nodes",
                "ctfe-compiler-graph-node-labels expects graph nodes to be a sequence",
            )?;
            let mut labels = Vec::new();
            for raw_node in nodes {
                let node = require_map(
                    &raw_node,
                    "ctfe-compiler-graph-node-labels expects node maps",
                )?;
                let node = node.borrow();
                if string_field(&node, "kind").as_deref() == Some(kind.as_str()) {
                    labels.push(
                        node.get(&str_key("label"))
                            .cloned()
                            .unwrap_or(RuntimeValue::Null),
                    );
                }
            }
            Ok(tuple(labels))
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-graph-dependencies".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 2,
        max_arity: Some(2),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let graph = require_map(
                &args[0],
                "ctfe-compiler-graph-dependencies expects a graph map",
            )?;
            let edge_kind = require_named_string(
                &args[1],
                "ctfe-compiler-graph-dependencies expects a non-empty edge kind",
            )?;
            let result = graph_dependencies(&graph.borrow(), edge_kind.as_str());
            result
        }),
    });

    ev.register_builtin(BuiltinInfo {
        name: "ctfe-compiler-graph-changed-lineages".to_string(),
        metadata: crate::values::BuiltinMetadata::compile_time_pure(),
        min_arity: 1,
        max_arity: Some(1),
        eager_handler: None,
        handler: Box::new(|ev, call, env| {
            let args = eval_args(ev, call, env)?;
            let graph = require_map(
                &args[0],
                "ctfe-compiler-graph-changed-lineages expects a graph map",
            )?;
            let result = changed_lineages(&graph.borrow());
            result
        }),
    });
}

#[derive(Default)]
struct CompilerGraphBuilder {
    nodes: Vec<RuntimeValue>,
    edges: Vec<RuntimeValue>,
    seen_nodes: HashSet<String>,
}

impl CompilerGraphBuilder {
    fn add_node(
        &mut self,
        kind: &str,
        label: impl AsRef<str>,
        data: RuntimeValue,
        node_id: Option<String>,
    ) -> String {
        let label = label.as_ref();
        let resolved_id = node_id.unwrap_or_else(|| format!("{kind}:{label}"));
        if self.seen_nodes.insert(resolved_id.clone()) {
            self.nodes.push(graph_node(&resolved_id, kind, label, data));
        }
        resolved_id
    }

    fn add_edge(
        &mut self,
        source: impl AsRef<str>,
        target: impl AsRef<str>,
        kind: &str,
        data: RuntimeValue,
    ) {
        self.edges
            .push(graph_edge(source.as_ref(), target.as_ref(), kind, data));
    }
}

fn compiler_graph_query(
    bridge: &CompilerBridgeValue,
    target: String,
    source: QueryArtifactSource,
    phase: PhasePolicy,
    options: QueryExecutionOptions,
) -> Result<RuntimeValue, EvalSignal> {
    let execution = bridge
        .query_execution_projection_with_options(target.clone(), source.clone(), phase, options)
        .map_err(eval_err)?;
    let mut graph = CompilerGraphBuilder::default();
    let mut previous_stage_id: Option<String> = None;

    for (index, step) in execution.plan.steps.iter().enumerate() {
        let stage_id = add_query_stage_node(&mut graph, step, index);
        if let Some(previous) = previous_stage_id.as_ref() {
            if previous != &stage_id {
                graph.add_edge(previous, &stage_id, "requires", map([]));
            }
        }
        previous_stage_id = Some(stage_id.clone());

        add_stage_provider_nodes(&mut graph, bridge, step, &stage_id)?;
        add_query_step_artifact_node(&mut graph, step, &stage_id);
    }

    let result_id = graph.add_node(
        "artifact",
        "result",
        query_artifact_to_value(&execution.artifact),
        Some(format!(
            "artifact:result:{}",
            graph_digest(&execution.artifact.key.to_string())
        )),
    );
    if let Some(previous) = previous_stage_id {
        graph.add_edge(previous, result_id, "produces", map([]));
    }

    Ok(graph_root(
        "query",
        map([
            ("target", string(target.as_str())),
            ("phase", string(phase.as_str())),
            ("origin", query_source_origin_to_value(&source)),
        ]),
        graph,
        map([
            ("result_stage", string(execution.artifact.stage.as_str())),
            ("result_key", artifact_key_to_value(&execution.artifact.key)),
            (
                "execution_required",
                RuntimeValue::Bool(!execution.plan.executed.is_empty()),
            ),
        ]),
    ))
}

fn compiler_graph_lineage(
    bridge: &CompilerBridgeValue,
    target: String,
    source: QueryArtifactSource,
    phase: PhasePolicy,
    options: QueryExecutionOptions,
) -> Result<RuntimeValue, EvalSignal> {
    let execution = bridge
        .query_execution_projection_with_options(target.clone(), source.clone(), phase, options)
        .map_err(eval_err)?;
    let mut graph = CompilerGraphBuilder::default();

    let origin_lineage_id = format!("origin:{}", graph_digest(&format!("{source:?}:{phase:?}")));
    let mut previous_lineage_id = graph.add_node(
        "lineage",
        "origin",
        map([
            ("lineage_id", string(origin_lineage_id.as_str())),
            ("stage", string("origin")),
            ("cached", RuntimeValue::Bool(false)),
            ("invalidation", RuntimeValue::Null),
        ]),
        Some(format!("lineage:{origin_lineage_id}")),
    );
    let origin_artifact_id = graph.add_node(
        "artifact",
        "origin",
        query_source_origin_to_value(&source),
        Some(format!("artifact:{origin_lineage_id}")),
    );
    graph.add_edge(
        &previous_lineage_id,
        &origin_artifact_id,
        "produces",
        map([]),
    );
    let mut previous_artifact_id = Some(origin_artifact_id);

    for (step, invalidation) in execution
        .plan
        .steps
        .iter()
        .zip(execution.invalidations.iter())
    {
        let lineage_id = step
            .artifact_key
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("stage:{}", step.stage));
        let lineage_node_id = graph.add_node(
            "lineage",
            step.stage.as_str(),
            map([
                ("lineage_id", step_artifact_key_value(step)),
                ("stage", string(step.stage.as_str())),
                ("cached", RuntimeValue::Bool(step.cached)),
                (
                    "invalidation",
                    invalidation
                        .as_ref()
                        .map(invalidation_record_to_value)
                        .unwrap_or(RuntimeValue::Null),
                ),
            ]),
            Some(format!("lineage:{}", graph_digest(&lineage_id))),
        );
        graph.add_edge(
            &previous_lineage_id,
            &lineage_node_id,
            "depends_on",
            map([]),
        );

        let next_previous_artifact_id = if let Some(key) = step.artifact_key.as_ref() {
            let artifact_id = graph.add_node(
                "artifact",
                step.stage.as_str(),
                map([
                    ("key", artifact_key_to_value(key)),
                    ("stage", string(step.stage.as_str())),
                    ("cached", RuntimeValue::Bool(step.cached)),
                ]),
                Some(format!("artifact:{}", graph_digest(&key.to_string()))),
            );
            graph.add_edge(&lineage_node_id, &artifact_id, "produces", map([]));
            Some(artifact_id)
        } else {
            previous_artifact_id.clone()
        };
        add_lineage_invalidation_nodes(
            &mut graph,
            &lineage_node_id,
            invalidation.as_ref(),
            previous_artifact_id.as_deref(),
        );
        previous_artifact_id = next_previous_artifact_id;
        previous_lineage_id = lineage_node_id;
    }

    Ok(graph_root(
        "lineage",
        map([
            ("target", string(target.as_str())),
            ("phase", string(phase.as_str())),
            ("origin_lineage_id", string(origin_lineage_id.as_str())),
        ]),
        graph,
        map([
            ("result_stage", string(execution.artifact.stage.as_str())),
            ("result_key", artifact_key_to_value(&execution.artifact.key)),
            (
                "step_count",
                RuntimeValue::Int(execution.plan.steps.len() as i64),
            ),
        ]),
    ))
}

fn add_query_stage_node(
    graph: &mut CompilerGraphBuilder,
    step: &QueryPlanStep,
    index: usize,
) -> String {
    graph.add_node(
        "stage",
        step.stage.as_str(),
        map([
            ("name", string(step.stage.as_str())),
            ("family", string(step.stage.as_str())),
            ("cached", RuntimeValue::Bool(step.cached)),
            ("index", RuntimeValue::Int(index as i64)),
            ("providers", string_tuple(step.provider_names.iter())),
            ("effect_tags", string_tuple(step.effect_tags.iter())),
            ("restarted", RuntimeValue::Bool(step.restarted)),
            (
                "restart_target",
                optional_string(step.restart_target.as_deref()),
            ),
            ("artifact_key", step_artifact_key_value(step)),
        ]),
        Some(format!("stage:{}", step.stage)),
    )
}

fn add_stage_provider_nodes(
    graph: &mut CompilerGraphBuilder,
    bridge: &CompilerBridgeValue,
    step: &QueryPlanStep,
    stage_id: &str,
) -> Result<(), EvalSignal> {
    let providers = bridge
        .list_providers(Some(step.stage.clone()))
        .map_err(eval_err)?;
    for provider in providers {
        let provider_id = graph.add_node(
            "provider",
            provider.name.as_str(),
            provider_to_value(&provider),
            Some(format!("provider:{}", provider.name)),
        );
        graph.add_edge(stage_id, &provider_id, "executes", map([]));
        for requirement in &provider.requires {
            graph.add_edge(
                format!("provider:{requirement}"),
                &provider_id,
                "requires",
                map([]),
            );
        }
    }
    Ok(())
}

fn add_query_step_artifact_node(
    graph: &mut CompilerGraphBuilder,
    step: &QueryPlanStep,
    stage_id: &str,
) {
    let Some(key) = step.artifact_key.as_ref() else {
        return;
    };
    let artifact_id = graph.add_node(
        "artifact",
        step.stage.as_str(),
        map([
            ("key", artifact_key_to_value(key)),
            ("stage", string(step.stage.as_str())),
            ("cached", RuntimeValue::Bool(step.cached)),
        ]),
        Some(format!("artifact:{}", graph_digest(&key.to_string()))),
    );
    graph.add_edge(stage_id, artifact_id, "produces", map([]));
}

fn add_lineage_invalidation_nodes(
    graph: &mut CompilerGraphBuilder,
    lineage_node_id: &str,
    record: Option<&ArtifactInvalidationRecord>,
    previous_artifact_id: Option<&str>,
) {
    let Some(record) = record else {
        return;
    };
    if let Some(replacement_key) = record.replacement_key.as_ref() {
        let replacement_id = graph.add_node(
            "artifact",
            "replacement",
            map([("key", artifact_key_to_value(replacement_key))]),
            Some(format!(
                "artifact:replacement:{}",
                graph_digest(&replacement_key.to_string())
            )),
        );
        graph.add_edge(lineage_node_id, replacement_id, "invalidates", map([]));
    }
    if let (Some(_previous), invalidated_key) = (previous_artifact_id, &record.invalidated_key) {
        let invalidated_id = graph.add_node(
            "artifact",
            "invalidated",
            map([("key", artifact_key_to_value(invalidated_key))]),
            Some(format!(
                "artifact:invalidated:{}",
                graph_digest(&invalidated_key.to_string())
            )),
        );
        graph.add_edge(lineage_node_id, invalidated_id, "invalidates", map([]));
    }
}

fn stage_to_value(stage: &QueryStageSpec) -> RuntimeValue {
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

fn provider_to_value(provider: &QueryProvider) -> RuntimeValue {
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
                ("emits", string_tuple(provider.effect_tags.iter())),
                ("uses", tuple(Vec::new())),
            ]),
        ),
        ("requires", string_tuple(provider.requires.iter())),
        ("requires_data", string_tuple(provider.requires_data.iter())),
        ("provides_data", string_tuple(provider.provides_data.iter())),
        (
            "input_schema",
            optional_string(provider.input_schema.as_deref()),
        ),
        ("reads", string_tuple(provider.reads.iter())),
        ("writes", string_tuple(provider.writes.iter())),
        ("cache_scope", string(provider.cache_scope.clone())),
        ("resume_policy", string(provider.resume_policy.clone())),
    ])
}

fn graph_root(
    graph_kind: &str,
    subject: RuntimeValue,
    graph: CompilerGraphBuilder,
    metadata: RuntimeValue,
) -> RuntimeValue {
    map([
        ("graph_kind", string(graph_kind)),
        ("subject", subject),
        ("nodes", tuple(graph.nodes)),
        ("edges", tuple(graph.edges)),
        ("metadata", metadata),
    ])
}

fn graph_node(node_id: &str, kind: &str, label: &str, data: RuntimeValue) -> RuntimeValue {
    map([
        ("id", string(node_id)),
        ("kind", string(kind)),
        ("label", string(label)),
        ("data", data),
    ])
}

fn graph_edge(source: &str, target: &str, kind: &str, data: RuntimeValue) -> RuntimeValue {
    map([
        ("from", string(source)),
        ("to", string(target)),
        ("kind", string(kind)),
        ("data", data),
    ])
}

fn graph_digest(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    format!("{:x}", digest.finalize())
        .chars()
        .take(12)
        .collect()
}

fn query_artifact_source(
    value: &RuntimeValue,
    message: &str,
) -> Result<QueryArtifactSource, EvalSignal> {
    if let RuntimeValue::Str(path) = value {
        return Ok(QueryArtifactSource::Path(path.to_string()));
    }
    let RuntimeValue::HostObject(object) = value else {
        return Err(eval_err(message));
    };
    let unit = object
        .as_any()
        .downcast_ref::<UnitBridgeValue>()
        .ok_or_else(|| eval_err(message))?;
    Ok(QueryArtifactSource::Unit(Box::new(unit.clone_unit())))
}

fn phase_and_initial_options(
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

fn phase_arg(value: &RuntimeValue, message: &str) -> Result<PhasePolicy, EvalSignal> {
    match value {
        RuntimeValue::Null => Ok(PhasePolicy::CompileTime),
        RuntimeValue::Str(value) => match value.as_ref() {
            "runtime" => Ok(PhasePolicy::Runtime),
            "compile_time" | "compile-time" => Ok(PhasePolicy::CompileTime),
            "dual" => Ok(PhasePolicy::Dual),
            _ => Err(eval_err(message)),
        },
        _ => Err(eval_err(message)),
    }
}

fn initial_bindings(
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

fn query_source_origin_to_value(source: &QueryArtifactSource) -> RuntimeValue {
    match source {
        QueryArtifactSource::Unit(unit) => {
            map([("kind", string("unit")), ("id", string(unit.unit_id()))])
        }
        QueryArtifactSource::Path(path) => {
            map([("kind", string("path")), ("path", string(path.as_str()))])
        }
        QueryArtifactSource::Text(text) => map([
            ("kind", string("text")),
            ("digest", string(graph_digest(text))),
        ]),
    }
}

fn step_artifact_key_value(step: &QueryPlanStep) -> RuntimeValue {
    step.artifact_key
        .as_ref()
        .map(artifact_key_to_value)
        .unwrap_or(RuntimeValue::Null)
}

fn query_artifact_to_value(artifact: &QueryArtifactProjection) -> RuntimeValue {
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
        ("value", artifact_value_to_value(&artifact.value)),
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
        ArtifactValue::Semantic(value) => map([
            ("kind", string("semantic")),
            ("value", semantic_value_to_runtime(value)),
        ]),
    }
}

fn semantic_value_to_runtime(value: &SemanticValue) -> RuntimeValue {
    match value {
        SemanticValue::Null => RuntimeValue::Null,
        SemanticValue::Bool(value) => RuntimeValue::Bool(*value),
        SemanticValue::Int(value) => RuntimeValue::Int(*value),
        SemanticValue::Float(value) => RuntimeValue::Float(*value),
        SemanticValue::Str(value) => string(value),
        SemanticValue::Node(node_id) => RuntimeValue::Int(*node_id as i64),
        SemanticValue::List(items) => RuntimeValue::List(Rc::new(RefCell::new(
            items.iter().map(semantic_value_to_runtime).collect(),
        ))),
        SemanticValue::Map(entries) => {
            let mut map = HashMap::new();
            for (key, value) in entries {
                map.insert(
                    MapKey::Str(key.as_str().into()),
                    semantic_value_to_runtime(value),
                );
            }
            RuntimeValue::Map(Rc::new(RefCell::new(map)))
        }
    }
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

fn diagnostic_to_value(diagnostic: &Diagnostic) -> RuntimeValue {
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
    ])
}

fn diagnostic_fix_to_value(fix: &DiagnosticFix) -> RuntimeValue {
    map([
        ("label", string(fix.label.as_str())),
        ("kind", string(fix.kind.as_str())),
        ("metadata", map_from_string_pairs(&fix.metadata)),
    ])
}

fn graph_dependencies(
    graph: &HashMap<MapKey, RuntimeValue>,
    edge_kind: &str,
) -> Result<RuntimeValue, EvalSignal> {
    let nodes = graph_sequence(
        graph,
        "nodes",
        "ctfe-compiler-graph-dependencies expects graph nodes to be a sequence",
    )?;
    let mut label_by_id = HashMap::new();
    for raw_node in nodes {
        let node = require_map(
            &raw_node,
            "ctfe-compiler-graph-dependencies expects node maps",
        )?;
        let node = node.borrow();
        if let Some(id) = node.get(&str_key("id")) {
            if let Ok(key) = MapKey::try_from(id) {
                label_by_id.insert(
                    key,
                    node.get(&str_key("label"))
                        .cloned()
                        .unwrap_or_else(|| id.clone()),
                );
            }
        }
    }

    let edges = graph_sequence(
        graph,
        "edges",
        "ctfe-compiler-graph-dependencies expects graph edges to be a sequence",
    )?;
    let mut items = Vec::new();
    for raw_edge in edges {
        let edge = require_map(
            &raw_edge,
            "ctfe-compiler-graph-dependencies expects edge maps",
        )?;
        let edge = edge.borrow();
        if string_field(&edge, "kind").as_deref() != Some(edge_kind) {
            continue;
        }
        let from = edge_endpoint_label(&edge, "from", &label_by_id);
        let to = edge_endpoint_label(&edge, "to", &label_by_id);
        items.push(map([
            ("from", from),
            ("to", to),
            ("kind", string(edge_kind)),
            (
                "data",
                edge.get(&str_key("data"))
                    .cloned()
                    .unwrap_or_else(|| map([])),
            ),
        ]));
    }
    Ok(tuple(items))
}

fn changed_lineages(graph: &HashMap<MapKey, RuntimeValue>) -> Result<RuntimeValue, EvalSignal> {
    let nodes = graph_sequence(
        graph,
        "nodes",
        "ctfe-compiler-graph-changed-lineages expects graph nodes to be a sequence",
    )?;
    let mut items = Vec::new();
    for raw_node in nodes {
        let node = require_map(
            &raw_node,
            "ctfe-compiler-graph-changed-lineages expects node maps",
        )?;
        let node = node.borrow();
        if string_field(&node, "kind").as_deref() != Some("lineage") {
            continue;
        }
        let Some(RuntimeValue::Map(data)) = node.get(&str_key("data")) else {
            continue;
        };
        let data = data.borrow();
        let Some(RuntimeValue::Map(invalidation)) = data.get(&str_key("invalidation")) else {
            continue;
        };
        let invalidation = invalidation.borrow();
        let Some(reason_kind) = invalidation.get(&str_key("reason_kind")).cloned() else {
            continue;
        };
        if matches!(reason_kind, RuntimeValue::Null) {
            continue;
        }
        items.push(map([
            (
                "id",
                node.get(&str_key("id"))
                    .cloned()
                    .unwrap_or(RuntimeValue::Null),
            ),
            (
                "label",
                node.get(&str_key("label"))
                    .cloned()
                    .unwrap_or(RuntimeValue::Null),
            ),
            (
                "lineage_id",
                data.get(&str_key("lineage_id"))
                    .cloned()
                    .unwrap_or(RuntimeValue::Null),
            ),
            (
                "stage",
                data.get(&str_key("stage"))
                    .cloned()
                    .unwrap_or(RuntimeValue::Null),
            ),
            ("reason_kind", reason_kind),
            (
                "changed_inputs",
                invalidation
                    .get(&str_key("changed_inputs"))
                    .cloned()
                    .unwrap_or_else(|| tuple(Vec::new())),
            ),
        ]));
    }
    Ok(tuple(items))
}

fn edge_endpoint_label(
    edge: &HashMap<MapKey, RuntimeValue>,
    key: &str,
    label_by_id: &HashMap<MapKey, RuntimeValue>,
) -> RuntimeValue {
    let Some(id) = edge.get(&str_key(key)) else {
        return RuntimeValue::Null;
    };
    MapKey::try_from(id)
        .ok()
        .and_then(|key| label_by_id.get(&key).cloned())
        .unwrap_or_else(|| id.clone())
}

fn graph_sequence(
    graph: &HashMap<MapKey, RuntimeValue>,
    key: &str,
    message: &str,
) -> Result<Vec<RuntimeValue>, EvalSignal> {
    match graph.get(&str_key(key)) {
        None | Some(RuntimeValue::Null) => Ok(Vec::new()),
        Some(value) => sequence(value, message),
    }
}

fn sequence(value: &RuntimeValue, message: &str) -> Result<Vec<RuntimeValue>, EvalSignal> {
    match value {
        RuntimeValue::Tuple(items) => Ok(items.iter().cloned().collect()),
        RuntimeValue::List(items) => Ok(items.borrow().iter().cloned().collect()),
        _ => Err(eval_err(message)),
    }
}

fn require_map(
    value: &RuntimeValue,
    message: &str,
) -> Result<Rc<RefCell<HashMap<MapKey, RuntimeValue>>>, EvalSignal> {
    match value {
        RuntimeValue::Map(map) => Ok(Rc::clone(map)),
        _ => Err(eval_err(message)),
    }
}

fn string_field(map: &HashMap<MapKey, RuntimeValue>, key: &str) -> Option<String> {
    match map.get(&str_key(key)) {
        Some(RuntimeValue::Str(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn map<const N: usize>(entries: [(&str, RuntimeValue); N]) -> RuntimeValue {
    let mut map = HashMap::new();
    for (key, value) in entries {
        map.insert(MapKey::Str(key.into()), value);
    }
    RuntimeValue::Map(Rc::new(RefCell::new(map)))
}

fn str_key(key: &str) -> MapKey {
    MapKey::Str(key.into())
}

fn tuple(items: Vec<RuntimeValue>) -> RuntimeValue {
    RuntimeValue::Tuple(items.into())
}

fn string(value: impl AsRef<str>) -> RuntimeValue {
    RuntimeValue::Str(value.as_ref().into())
}

fn optional_string(value: Option<&str>) -> RuntimeValue {
    value.map(string).unwrap_or(RuntimeValue::Null)
}

fn map_from_string_pairs<'a>(
    entries: impl IntoIterator<Item = &'a (String, String)>,
) -> RuntimeValue {
    let mut map = HashMap::new();
    for (key, value) in entries {
        map.insert(MapKey::Str(key.clone().into()), string(value));
    }
    RuntimeValue::Map(Rc::new(RefCell::new(map)))
}

fn string_tuple<'a>(items: impl IntoIterator<Item = &'a String>) -> RuntimeValue {
    tuple(items.into_iter().map(string).collect())
}
