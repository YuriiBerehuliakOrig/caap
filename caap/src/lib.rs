pub mod artifacts;
pub(crate) mod bind_args;
pub mod bridges;
pub mod builtins;
pub mod compiler;
pub mod debug;
pub mod diagnostics;
pub mod error;
pub mod eval;
pub mod frontend;
pub mod graph;
pub mod host;
pub mod ir;
pub mod language;
pub mod lsp;
pub mod runtime_loader;
pub mod semantic;
pub mod source;
pub mod surface_syntax;
pub mod syntax_authoring;
pub mod unit;
pub mod values;
pub mod workspace;

pub use artifacts::{
    changed_inputs_for_lineage, ArtifactCache, ArtifactCacheFile, ArtifactInvalidationRecord,
    ArtifactKey, ArtifactValue, SourceArtifact, SourceTemplateCache,
};
pub use compiler::{
    BootstrapImageFile, BootstrapImageTrustPolicy, BootstrapVirtualFileSystem, CapabilityName,
    Compiler, CompilerHost, QueryExecutionOptions, QueryProviderCacheScope,
    QueryProviderCallbackOutcome, QueryProviderRegistrationSpec, QueryStageSpec,
    QueryTransactionMode,
};
pub use diagnostics::{
    render_diagnostic, CompilerEvent, Diagnostic, DiagnosticCode, DiagnosticSeverity,
};
pub use error::{CaapError, CaapResult};
pub use eval::Evaluator;
pub use frontend::{
    ast_json, canonicalize_parsed_source, canonicalize_source, check_source, eval_source, parse,
    parse_forms_with_source_path, parse_with_source_path, ParsedForm, ParsedSource,
};
pub use graph::{GraphBuilder, IRGraph, IRGraphTemplate};
pub use host::{
    HostCapabilityPolicy, HostExportMetadata, HostExportParameter, HostExportSignature,
    HostFileSystemPolicy, HostOsEnvironmentPolicy, HostServiceLibrary, HostServiceRegistry,
    HostSystemPolicy,
};
pub use ir::{CallNode, ExprSpec, IrLiteralData, LiteralNode, NameNode, Node, NodeId};
pub use semantic::{
    node_subject_id, subject_id, ControlPolicy, EffectPolicy, EntrySource, EvalPolicy, PhasePolicy,
    ScopePolicy, SemanticEntry, SemanticRegistry, SemanticValue, SymbolEntry, SymbolKind,
    UnifiedSemanticGraph,
};
pub use source::SourceSpan;
pub use unit::{
    CrossUnitGraph, LinkBinding, Unit, UnitAssemblyPipeline, UnitLinkState, UnitSyntaxState,
    UnitTemplate,
};
pub use values::{
    require_int_strict, BuiltinVisibility, EnvRef, Environment, EvalSignal, EvaluationError,
    HostFunction, MapKey, RuntimeValue,
};
pub use workspace::WorkspaceLayout;
