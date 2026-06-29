mod args;
pub mod arithmetic;
pub mod bytes;
mod compiler_node_match;
pub mod compiler_providers;
pub mod compiler_query;
mod compiler_query_helpers;
mod compiler_query_runtime;
pub mod compiler_registry;
pub mod compiler_units;
mod compiler_units_helpers;
mod compiler_units_runtime;
pub mod control_flow;
pub mod effects;
pub mod grammar;
pub mod grammar_builder;
pub mod host_services;
pub mod ir_builders;
pub mod mutable;
pub mod provider_context;
mod provider_context_helpers;
mod provider_context_runtime;
pub mod reflect;
mod semantic_projection;
pub mod sequences;
pub mod strings;
pub mod surface;
pub mod syntax;
mod value_compare;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KernelPrimitiveClass {
    BootstrapIntegration,
    CompilerMechanism,
    CompilerIntrospection,
    CompilerUnitMechanism,
    GrammarMechanism,
    IrConstructionMechanism,
    MetadataMechanism,
    SemanticResolutionMechanism,
    SurfaceMechanism,
    SyntaxTreeMechanism,
    ProviderMechanism,
}

pub(crate) fn compiler_units_helpers_span_to_value(
    span: &crate::source::SourceSpan,
) -> crate::values::RuntimeValue {
    compiler_units_helpers::source_span_to_value(span)
}

pub const KERNEL_PRIMITIVE_CLASSIFICATIONS: &[(&str, KernelPrimitiveClass)] = &[
    (
        "ctfe_call_semantics_from_entry",
        KernelPrimitiveClass::SemanticResolutionMechanism,
    ),
    (
        "ctfe_eval_node",
        KernelPrimitiveClass::IrConstructionMechanism,
    ),
    (
        "ctfe_spec_span",
        KernelPrimitiveClass::IrConstructionMechanism,
    ),
    (
        "ctfe_spec_with_span",
        KernelPrimitiveClass::IrConstructionMechanism,
    ),
    // Read-only evaluator introspection: the callable vocabulary as data, for
    // in-language compile-time tooling (load-time checkers/linters).
    (
        "ctfe_kernel_vocabulary",
        KernelPrimitiveClass::CompilerIntrospection,
    ),
    // Diagnostics-class ONLY: the live closure-call stack for REPL/tracing.
    (
        "ctfe_debug_frames",
        KernelPrimitiveClass::CompilerIntrospection,
    ),
    // One-call text→forms parse under merged grammar units (mechanism over the
    // same dynamic-surface machinery as the file-template loader).
    (
        "ctfe_grammar_parse_forms",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_ir_name",
        KernelPrimitiveClass::IrConstructionMechanism,
    ),
    (
        "ctfe_ir_literal",
        KernelPrimitiveClass::IrConstructionMechanism,
    ),
    (
        "ctfe_ir_call",
        KernelPrimitiveClass::IrConstructionMechanism,
    ),
    (
        "ctfe_meta_annotation_delete",
        KernelPrimitiveClass::MetadataMechanism,
    ),
    (
        "ctfe_meta_annotation_get",
        KernelPrimitiveClass::MetadataMechanism,
    ),
    (
        "ctfe_meta_fact_delete",
        KernelPrimitiveClass::MetadataMechanism,
    ),
    (
        "ctfe_meta_annotation_set",
        KernelPrimitiveClass::MetadataMechanism,
    ),
    (
        "ctfe_meta_fact_get_by_key",
        KernelPrimitiveClass::MetadataMechanism,
    ),
    (
        "ctfe_meta_fact_has_by_key",
        KernelPrimitiveClass::MetadataMechanism,
    ),
    (
        "ctfe_meta_fact_set_by_key",
        KernelPrimitiveClass::MetadataMechanism,
    ),
    (
        "ctfe_node_ancestor?",
        KernelPrimitiveClass::SyntaxTreeMechanism,
    ),
    (
        "ctfe_node_call_args",
        KernelPrimitiveClass::SyntaxTreeMechanism,
    ),
    (
        "ctfe_node_call_callee",
        KernelPrimitiveClass::SyntaxTreeMechanism,
    ),
    (
        "ctfe_node_call_semantics",
        KernelPrimitiveClass::SyntaxTreeMechanism,
    ),
    (
        "ctfe_node_children",
        KernelPrimitiveClass::SyntaxTreeMechanism,
    ),
    ("ctfe_node_id", KernelPrimitiveClass::SyntaxTreeMechanism),
    (
        "ctfe_node_is_call",
        KernelPrimitiveClass::SyntaxTreeMechanism,
    ),
    (
        "ctfe_node_is_literal",
        KernelPrimitiveClass::SyntaxTreeMechanism,
    ),
    (
        "ctfe_node_is_name",
        KernelPrimitiveClass::SyntaxTreeMechanism,
    ),
    ("ctfe_node_kind", KernelPrimitiveClass::SyntaxTreeMechanism),
    (
        "ctfe_node_literal_value",
        KernelPrimitiveClass::SyntaxTreeMechanism,
    ),
    ("ctfe_node_live?", KernelPrimitiveClass::SyntaxTreeMechanism),
    ("ctfe_node_match", KernelPrimitiveClass::SyntaxTreeMechanism),
    (
        "ctfe_node_name_identifier",
        KernelPrimitiveClass::SyntaxTreeMechanism,
    ),
    (
        "ctfe_node_parent",
        KernelPrimitiveClass::SyntaxTreeMechanism,
    ),
    (
        "ctfe_node_resolved_block",
        KernelPrimitiveClass::SemanticResolutionMechanism,
    ),
    (
        "ctfe_node_resolved_name_entry",
        KernelPrimitiveClass::SemanticResolutionMechanism,
    ),
    (
        "ctfe_node_to_spec",
        KernelPrimitiveClass::SyntaxTreeMechanism,
    ),
    (
        "ctfe_resolution_scope_define!",
        KernelPrimitiveClass::SemanticResolutionMechanism,
    ),
    (
        "ctfe_resolution_scope_fork",
        KernelPrimitiveClass::SemanticResolutionMechanism,
    ),
    (
        "ctfe_resolution_scope_lookup",
        KernelPrimitiveClass::SemanticResolutionMechanism,
    ),
    (
        "ctfe_semantic_entry_node",
        KernelPrimitiveClass::SemanticResolutionMechanism,
    ),
    (
        "ctfe_semantic_entry_to_map",
        KernelPrimitiveClass::SemanticResolutionMechanism,
    ),
    (
        "ctfe_source_ast_json",
        KernelPrimitiveClass::SurfaceMechanism,
    ),
    (
        "ctfe_source_canonicalize",
        KernelPrimitiveClass::SurfaceMechanism,
    ),
    (
        "ctfe_surface_binding_get",
        KernelPrimitiveClass::SurfaceMechanism,
    ),
    (
        "ctfe_surface_binding_group_collect",
        KernelPrimitiveClass::SurfaceMechanism,
    ),
    (
        "ctfe_surface_form_bool",
        KernelPrimitiveClass::SurfaceMechanism,
    ),
    (
        "ctfe_surface_form_float",
        KernelPrimitiveClass::SurfaceMechanism,
    ),
    (
        "ctfe_surface_form_integer",
        KernelPrimitiveClass::SurfaceMechanism,
    ),
    (
        "ctfe_surface_form_list",
        KernelPrimitiveClass::SurfaceMechanism,
    ),
    (
        "ctfe_surface_form_list_prepend",
        KernelPrimitiveClass::SurfaceMechanism,
    ),
    (
        "ctfe_surface_form_null",
        KernelPrimitiveClass::SurfaceMechanism,
    ),
    (
        "ctfe_surface_form_string",
        KernelPrimitiveClass::SurfaceMechanism,
    ),
    (
        "ctfe_surface_form_symbol",
        KernelPrimitiveClass::SurfaceMechanism,
    ),
    (
        "ctfe_surface_parse_form",
        KernelPrimitiveClass::SurfaceMechanism,
    ),
    (
        "ctfe_surface_reparse_text",
        KernelPrimitiveClass::SurfaceMechanism,
    ),
    (
        "ctfe_surface_unwrap",
        KernelPrimitiveClass::SurfaceMechanism,
    ),
    (
        "ctfe_unit_add_dependency_binding!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_add_exposed_name!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_append_top_level!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_declare_symbol!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_dependency_bindings",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_erase_detached!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_exposed_names",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_facts",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    ("ctfe_unit_id", KernelPrimitiveClass::CompilerUnitMechanism),
    (
        "ctfe_unit_node_location",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_node_span",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_rewrite_report",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_root",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_set_id!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_set_root!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_set_symbol_semantics!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_set_top_level_forms!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_symbols",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_syntax_authoring_source_apply!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_syntax_hook_set_inline_node!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_syntax_metadata_get",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_syntax_metadata_set!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_syntax_rule_define!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_syntax_rule_define_inline_node!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_syntax_rule_set!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_syntax_rule_params_set!",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_template_instantiate",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_to_template",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_top_level_forms",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_top_level_symbols",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_unit_version",
        KernelPrimitiveClass::CompilerUnitMechanism,
    ),
    (
        "ctfe_grammar_analyze",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_conflicts",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_describe",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_extend",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_ast_changed_ranges",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_ast_reparse_incremental",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    ("ctfe_ast_to_map", KernelPrimitiveClass::GrammarMechanism),
    (
        "ctfe_grammar_apply_edits",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    ("ctfe_grammar_diff", KernelPrimitiveClass::GrammarMechanism),
    (
        "ctfe_grammar_nullable_rules",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_remove_rule",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_replace_rule",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_rule_graph",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_set_metadata",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_signature",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_validate",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    ("ctfe_grammar_new", KernelPrimitiveClass::GrammarMechanism),
    (
        "ctfe_grammar_parse_ast",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_parse_ast_tolerant",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    ("ctfe_grammar_parse", KernelPrimitiveClass::GrammarMechanism),
    (
        "ctfe_grammar_parse_cache",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_parse_incremental",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_parse_prefix",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_parse_recover",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_parse_profiled",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_parse_tokens",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_parse_with_registry",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_registry",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_registry_list",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_registry_register",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_rule_get",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_grammar_set_start",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    ("ctfe_lex_token", KernelPrimitiveClass::GrammarMechanism),
    (
        "ctfe_lexer_tokenize",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    ("ctfe_peg_action", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_builder", KernelPrimitiveClass::GrammarMechanism),
    (
        "ctfe_peg_builder_build",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_peg_builder_import",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_peg_builder_parametric_rule",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_peg_builder_rule",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    ("ctfe_peg_call", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_capture", KernelPrimitiveClass::GrammarMechanism),
    (
        "ctfe_peg_char_class",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    ("ctfe_peg_choice", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_cut", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_dedent", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_dot", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_eager", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_expected", KernelPrimitiveClass::GrammarMechanism),
    (
        "ctfe_peg_grammar_scope",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    (
        "ctfe_peg_imported_ref",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    ("ctfe_peg_indent", KernelPrimitiveClass::GrammarMechanism),
    (
        "ctfe_peg_interspersed",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    ("ctfe_peg_island", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_keyword", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_lit", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_named", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_newline", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_no_trivia", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_param", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_plus", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_raw_block", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_ref", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_regex", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_seq", KernelPrimitiveClass::GrammarMechanism),
    ("ctfe_peg_sep_plus", KernelPrimitiveClass::GrammarMechanism),
    (
        "ctfe_peg_soft_keyword",
        KernelPrimitiveClass::GrammarMechanism,
    ),
    ("ctfe_peg_token_ref", KernelPrimitiveClass::GrammarMechanism),
    (
        "ctfe_compiler_current_bootstrap_context",
        KernelPrimitiveClass::BootstrapIntegration,
    ),
    (
        "ctfe_compiler_emit_event",
        KernelPrimitiveClass::CompilerMechanism,
    ),
    (
        "ctfe_compiler_builtin_semantic_entries",
        KernelPrimitiveClass::CompilerIntrospection,
    ),
    (
        "ctfe_compiler_evaluate_bootstrap_file",
        KernelPrimitiveClass::BootstrapIntegration,
    ),
    (
        "ctfe_compiler_evaluate_capture",
        KernelPrimitiveClass::CompilerMechanism,
    ),
    (
        "ctfe_compiler_execute_bootstrap_file",
        KernelPrimitiveClass::BootstrapIntegration,
    ),
    (
        "ctfe_compiler_fact_schema_register",
        KernelPrimitiveClass::CompilerMechanism,
    ),
    (
        "ctfe_compiler_fact_schema_type_bridge_register",
        KernelPrimitiveClass::CompilerMechanism,
    ),
    (
        "ctfe_compiler_is_file",
        KernelPrimitiveClass::BootstrapIntegration,
    ),
    (
        "ctfe_compiler_list_dir",
        KernelPrimitiveClass::BootstrapIntegration,
    ),
    (
        "ctfe_compiler_list_providers",
        KernelPrimitiveClass::CompilerIntrospection,
    ),
    (
        "ctfe_compiler_list_semantic_policies",
        KernelPrimitiveClass::CompilerIntrospection,
    ),
    (
        "ctfe_compiler_list_stages",
        KernelPrimitiveClass::CompilerIntrospection,
    ),
    (
        "ctfe_compiler_load_surface_file_template",
        KernelPrimitiveClass::BootstrapIntegration,
    ),
    (
        "ctfe_compiler_lookup_unit",
        KernelPrimitiveClass::CompilerMechanism,
    ),
    (
        "ctfe_compiler_lookup_value",
        KernelPrimitiveClass::CompilerMechanism,
    ),
    (
        "ctfe_compiler_provider_register",
        KernelPrimitiveClass::CompilerMechanism,
    ),
    (
        "ctfe_compiler_provider_schedule",
        KernelPrimitiveClass::CompilerIntrospection,
    ),
    (
        "ctfe_compiler_query_execution",
        KernelPrimitiveClass::CompilerMechanism,
    ),
    (
        "ctfe_compiler_register_base_semantic_entries",
        KernelPrimitiveClass::CompilerMechanism,
    ),
    (
        "ctfe_compiler_register_semantic_policy",
        KernelPrimitiveClass::CompilerMechanism,
    ),
    (
        "ctfe_compiler_register_unit",
        KernelPrimitiveClass::CompilerMechanism,
    ),
    (
        "ctfe_compiler_register_value",
        KernelPrimitiveClass::CompilerMechanism,
    ),
    (
        "ctfe_compiler_stage_register",
        KernelPrimitiveClass::CompilerMechanism,
    ),
    (
        "ctfe_provider_annotation_get",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_annotation_set",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_base_resolution_scope",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_diagnostics_error",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_diagnostics_hint",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_diagnostics_note",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_diagnostics_warning",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_fact_get",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_fact_set",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_fold_compile_time_call",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_evaluate_call!",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_synthesize_internal_definition!",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_invoke_callback",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_node_erase",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_node_replace",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_node_rewrite",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_require_effect",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_traversal_walk",
        KernelPrimitiveClass::ProviderMechanism,
    ),
    (
        "ctfe_provider_unit",
        KernelPrimitiveClass::ProviderMechanism,
    ),
];

pub fn register_all(ev: &mut crate::eval::Evaluator) {
    arithmetic::register(ev);
    bytes::register(ev);
    control_flow::register(ev);
    effects::register(ev);
    mutable::register(ev);
    strings::register(ev);
    sequences::register(ev);
    reflect::register(ev);
    host_services::register(ev);
    grammar::register(ev);
    grammar_builder::register(ev);
    compiler_registry::register(ev);
    compiler_providers::register(ev);
    compiler_query::register(ev);
    compiler_units::register(ev);
    ir_builders::register(ev);
    provider_context::register(ev);
    surface::register(ev);
    syntax::register(ev);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::KERNEL_PRIMITIVE_CLASSIFICATIONS;
    use crate::eval::Evaluator;
    use crate::graph::IRGraph;

    // Substrate ↔ policy boundary guard (docs/builtins.md): every compile-time
    // `ctfe_*` primitive must be a classified *mechanism*, and every
    // classification must map to a registered builtin. A new builtin therefore
    // has to declare which mechanism it is — the moment to ask whether it belongs
    // in core (mechanism) or stdlib (policy). There is no `Policy` class.
    #[test]
    fn compiler_and_provider_kernel_primitives_are_explicitly_classified() {
        let evaluator = Evaluator::new(IRGraph::new());
        let actual: BTreeSet<_> = evaluator
            .builtin_names()
            .into_iter()
            .filter(|name| name.starts_with("ctfe_"))
            .collect();
        let classified: BTreeSet<_> = KERNEL_PRIMITIVE_CLASSIFICATIONS
            .iter()
            .map(|(name, _)| *name)
            .collect();

        let missing: Vec<_> = actual.difference(&classified).copied().collect();
        assert!(
            missing.is_empty(),
            "unclassified CTFE kernel primitives: {missing:?}"
        );

        let stale: Vec<_> = classified.difference(&actual).copied().collect();
        assert!(
            stale.is_empty(),
            "classified CTFE kernel primitives are not registered: {stale:?}"
        );
    }

    #[test]
    fn kernel_does_not_expose_ambient_random_builtins() {
        let evaluator = Evaluator::new(IRGraph::new());
        let names: BTreeSet<_> = evaluator.builtin_names().into_iter().collect();
        assert!(!names.contains("random"));
        assert!(!names.contains("random_int"));
    }
}
