//! Minimal compiler host/session substrate for CAAP.
//!
//! This mirrors the compiler boundary where `CompilerHost` owns long-lived host
//! resources and `Compiler` owns mutable per-session state. A fresh session is
//! intentionally bare: no stdlib, stages, providers, or system services are
//! bootstrapped implicitly.

mod fact_schema;
pub use fact_schema::{
    FactSchemaEntry, FactSchemaRegistry, FactSchemaTypeBridge, FactSchemaTypeBridgeKind,
};

mod bridges;
pub use bridges::{
    ProviderContextBridgeValue, QueryArtifactProjection, QueryArtifactSource,
    QueryExecutionProjection, SemanticPolicyRegistration, UnitBridgeValue,
};

mod host;
pub use host::{
    CompilerCatalog, CompilerHost, CompilerHostConfig, CompilerNameService, DiagnosticSink,
};

mod bootstrap;
pub use crate::semantic::{CapabilityName, EffectSet, EffectTag};
pub use bootstrap::{
    BootstrapCapabilityGraph, BootstrapImage, BootstrapImageFile, BootstrapImageStore,
    BootstrapImageTrustPolicy, BootstrapTraceEvent, BootstrapVirtualFileSystem,
    CompilerBootstrapController, EvaluationCapture,
};

mod query_provider;
mod query_provider_registry;
mod query_provider_types;
pub(crate) use query_provider::{annotation_tracking_predicate, ANNOTATION_PREDICATE_PREFIX};
pub use query_provider::{
    NativeProviderContext, ProviderCacheEntry, QueryExecutionOptions, QueryPlan, QueryPlanStep,
    QueryProvider, QueryProviderCacheScope, QueryProviderCallback, QueryProviderCallbackOutcome,
    QueryProviderContext, QueryProviderContractSpec, QueryProviderExecutionRecord,
    QueryProviderRegistrationSpec, QueryProviderRegistry, QueryProviderResumePolicy,
    QueryProviderSchedule, QueryStageSpec, QueryTransactionMode,
};

mod session;
pub use session::{
    bootstrap_image_file_fingerprint, Compiler, CompilerRegistry, CompilerRegistrySnapshot,
};

mod bridge;
pub use bridge::CompilerBridgeValue;

mod eval_service;
pub use eval_service::CompilerEvaluationService;

mod query_service;
pub use query_service::CompilerQueryService;
