#![allow(
    clippy::items_after_test_module,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

pub mod artifacts;
pub mod bridges;
pub mod builtins;
pub mod cli;
pub mod compiler;
pub mod diagnostics;
pub mod error;
pub mod eval;
pub mod frontend;
pub mod graph;
pub mod host;
pub mod ir;
pub mod semantic;
pub mod source;
pub mod surface_syntax;
pub mod syntax_authoring;
pub mod unit;
pub mod values;

pub use artifacts::{
    changed_inputs_for_lineage, parse_surface_inline_key, parse_surface_path_key, ArtifactCache,
    ArtifactCacheFile, ArtifactCacheSnapshot, ArtifactCacheStats, ArtifactFingerprint,
    ArtifactInvalidationRecord, ArtifactKey, ArtifactValue, ReusableArtifactCacheSnapshot,
    SourceArtifact, SourceOrigin, SourceTemplateArtifact, SourceTemplateCache,
};
pub use bridges::{HostCapabilityBridgeValue, SemanticEntryBridgeValue};
pub use compiler::{
    bootstrap_image_file_fingerprint, package_dependency_module_names, parse_package_declarations,
    parse_package_declarations_or_none, source_module_name, BootstrapCapabilityGraph,
    BootstrapImage, BootstrapImageFile, BootstrapImageStore, BootstrapImageTrustPolicy,
    BootstrapTraceEvent, BootstrapVirtualFileSystem, Compiler, CompilerBootstrapController,
    CompilerBridgeValue, CompilerCatalog, CompilerEvaluationService, CompilerHost,
    CompilerHostConfig, CompilerNameService, CompilerQueryService, CompilerRegistry,
    CompilerRegistrySnapshot, EvaluationCapture, FactSchemaEntry, FactSchemaRegistry,
    FactSchemaTypeBridge, FactSchemaTypeBridgeKind, ModuleCatalog, ModuleMaterialization,
    PackageDescriptor, PackageExport, PackageImport, PackageImportSymbol,
    ProviderContextBridgeValue, QueryArtifactSource, QueryExecutionOptions,
    QueryExecutionProjection, QueryPlan, QueryPlanStep, QueryProvider,
    QueryProviderCallbackOutcome, QueryProviderContext, QueryProviderExecutionRecord,
    QueryProviderRegistrationSpec, QueryProviderRegistry, QueryStageSpec, QueryTransactionMode,
    UnitBridgeValue,
};
pub use diagnostics::{
    render_diagnostic, span_location, CompilerEvent, CompilerEventLog, Diagnostic,
    DiagnosticExplanation, DiagnosticExplanationRegistry, DiagnosticFix, DiagnosticFrame,
    DiagnosticSeverity,
};
pub use error::{CaapError, CaapResult};
pub use eval::Evaluator;
pub use frontend::{
    ast_json, check_source, eval_source, evaluator_from_source, format_parsed_form,
    format_parsed_source, format_source, parse, parse_forms, parse_forms_with_source_path,
    parse_with_source_path, parsed_source_to_ir, ParsedForm, ParsedSource,
};
pub use graph::{GraphBuilder, IRGraph, IRGraphTemplate};
pub use host::{
    HostCapabilityPolicy, HostExportMetadata, HostExportParameter, HostExportSignature,
    HostFileSystemPolicy, HostIoPolicy, HostNetworkPolicy, HostOsEnvironmentPolicy,
    HostProcessPolicy, HostServiceExport, HostServiceLibrary, HostServiceRegistry,
    HostSystemPolicy,
};
pub use ir::{
    CallNode, CallSpec, ExprSpec, IrLiteralData, LiteralNode, LiteralSpec, NameNode, NameSpec,
    Node, NodeId,
};
pub use semantic::{
    node_subject_id, semantic_entity_id, subject_id, symbol_subject_id, ControlPolicy,
    EffectPolicy, EntrySource, EvalPolicy, PhasePolicy, ScopePolicy, SemanticEntry, SemanticGraph,
    SemanticGraphSnapshot, SemanticRegistry, SemanticRegistrySnapshot, SemanticSubjectId,
    SemanticValue, StableId, SymbolEntry, SymbolKind, UnifiedSemanticGraph,
    UnifiedSemanticGraphSnapshot, UnifiedSemanticTransaction,
};
pub use source::{SourcePoint, SourceRange, SourceSpan};
pub use surface_syntax::{
    compile_surface_grammar_from_syntax_state, named_parse_bindings_to_runtime_map,
    parse_value_to_parsed_source, parse_value_to_runtime_value,
    runtime_surface_form_to_parsed_form, runtime_value_to_parse_value, semantic_value_to_json,
    surface_grammar_spec_from_syntax_state, SurfaceBuiltinSemanticRuntime,
};
pub use unit::{
    CrossUnitGraph, LinkBinding, Unit, UnitAssemblyHook, UnitAssemblyPipeline,
    UnitAttributeSnapshot, UnitLifecycleEvent, UnitLinkState, UnitSnapshot, UnitSyntaxState,
    UnitTemplate, UnitTransaction,
};
pub use values::{
    is_truthy, require_int_strict, require_list, require_map, require_str,
    runtime_value_from_literal, BuiltinInfo, BuiltinMetadata, ClosureValue, EnvRef, Environment,
    EvalResult, EvalSignal, EvaluationError, HostFunction, HostObject, LeaveSignal, MapKey, RtList,
    RtMap, RuntimeCallFrame, RuntimeValue,
};
