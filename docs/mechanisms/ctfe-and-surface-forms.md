# CTFE & Surface Forms

**Source:** node/unit/meta primitives in
[compiler_units_runtime.rs](../../caap/src/builtins/compiler_units_runtime.rs);
surface-form primitives in [surface.rs](../../caap/src/builtins/surface.rs);
provider-context surface in
[provider_context_runtime.rs](../../caap/src/builtins/provider_context_runtime.rs);
IR construction in [ir_builders.rs](../../caap/src/builtins/ir_builders.rs).

Compile-time evaluation (CTFE) is how stdlib and user passes **inspect and
rewrite** programs while compiling. The primitives fall into four layers.

## 1. IR node inspection (`ctfe_node_*`)

Read-only accessors over the three IR node kinds (`Name` / `Literal` / `Call`).
All `compile_time_pure`:

| Builtin | Returns |
|---|---|
| `ctfe_node_kind` | `"name"` / `"literal"` / `"call"` |
| `ctfe_node_is_name` / `ctfe_node_is_literal` / `ctfe_node_is_call` | kind predicates |
| `ctfe_node_id` | the node id |
| `ctfe_node_name_identifier` | a `Name`'s identifier string |
| `ctfe_node_literal_value` | a `Literal`'s value |
| `ctfe_node_call_callee` / `ctfe_node_call_args` | a `Call`'s callee / argument nodes |
| `ctfe_node_children` | child node ids |
| `ctfe_node_parent` / `ctfe_node_ancestor?` | parent / ancestor test |
| `ctfe_node_live?` | is the node still in the graph |
| `ctfe_node_to_spec` | convert a live node to a reusable construction **spec** |
| `ctfe_node_resolved_name_entry` | the `caap.fact.resolved_name` semantic entry |
| `ctfe_node_resolved_block` | the `caap.fact.resolved_block` fact |
| `ctfe_node_call_semantics` | the call's `caap.fact.call_semantics` |
| `ctfe_node_match` | structural match against a pattern |

## 2. Unit inspection & mutation (`ctfe_unit_*`)

A **unit** is one compilation unit (a module). Queries (`compile_time_pure`)
include `ctfe_unit_id`, `ctfe_unit_root`, `ctfe_unit_top_level_forms`,
`ctfe_unit_top_level_symbols`, `ctfe_unit_symbols`, `ctfe_unit_exposed_names`,
`ctfe_unit_facts`, `ctfe_unit_node_location`, `ctfe_unit_version`. Mutations
(`!`-suffixed, impure) include `ctfe_unit_append_top_level!`,
`ctfe_unit_set_top_level_forms!`, `ctfe_unit_set_root!`,
`ctfe_unit_declare_symbol!`, `ctfe_unit_set_symbol_semantics!`,
`ctfe_unit_add_exposed_name!`, `ctfe_unit_add_dependency_binding!`,
`ctfe_unit_erase_detached!`. Templates: `ctfe_unit_to_template` /
`ctfe_unit_template_instantiate`. Syntax-rule mutation:
`ctfe_unit_syntax_rule_define!`, `ctfe_unit_syntax_metadata_set!`,
`ctfe_unit_syntax_authoring_source_apply!`, etc.

> **Synthesis gotcha:** when synthesizing a top-level binding, the new
> `SymbolEntry` must carry `node_id = None` (like an ordinary top-level bind).
> A `Some(id)` makes `ctfe_unit_top_level_symbols` expose a raw int that later
> trips node-only builtins. The convenience primitive
> `ctfe_provider_synthesize_internal_definition!` does this correctly.

## 3. Facts & annotations (`ctfe_meta_*`)

- **Facts** are analysis results keyed by predicate:
  `ctfe_meta_fact_get_by_key` / `ctfe_meta_fact_set_by_key` /
  `ctfe_meta_fact_has_by_key`.
- **Annotations** are user-supplied node metadata:
  `ctfe_meta_annotation_get` / `ctfe_meta_annotation_set`.

Both store under the same backing store: an annotation `key` is stored as a fact
under `annotation_tracking_predicate(key)` on the node subject, so the
provider-context readers (`ctfe_provider_annotation_get`) and the meta writers
(`ctfe_meta_annotation_set`) share one keyspace. The provider-context variants
(`ctfe_provider_fact_*`, `ctfe_provider_annotation_*`) additionally enforce the
provider's declared `read_attributes` / `write_attributes` effects.

## 4. Surface forms (`ctfe_surface_*`)

Surface forms are the parsed-but-not-yet-lowered representation. The
[surface.rs](../../caap/src/builtins/surface.rs) primitives (all
`compile_time_pure`) build, unwrap and match them:

| Builtin | Purpose |
|---|---|
| `ctfe_surface_parse_form` | parse text → surface form |
| `ctfe_surface_reparse_text` | re-parse a region |
| `ctfe_surface_unwrap` | unwrap a surface form to its neutral value |
| `ctfe_surface_match` | match a surface form against a pattern |
| `ctfe_surface_form_symbol` / `_string` / `_integer` / `_null` / `_list` | construct surface forms |
| `ctfe_surface_form_list_prepend` | prepend into a surface list form |
| `ctfe_surface_binding_get` / `ctfe_surface_binding_group_collect` | read binding forms |

## IR construction & spec evaluation

New IR is built from **specs** (declarative node descriptions), not by mutating
live nodes directly:

- `ctfe_ir_name` / `ctfe_ir_literal` / `ctfe_ir_call` `(payload [metadata])` build node specs
  ([ir_builders.rs](../../caap/src/builtins/ir_builders.rs)) — e.g. kind
  `"name"` with `"identifier"`, `"literal"` with `"value"`, `"call"` with
  `"callee"` + `"args"`.
- `ctfe_node_to_spec` turns an existing live node back into a spec (for copying
  / substitution).
- In a provider, `ctfe_provider_node_replace`, `ctfe_provider_node_rewrite`,
  `ctfe_provider_node_erase` apply specs to the live graph; the fold engine uses
  `ctfe_provider_evaluate_call!` / `ctfe_provider_fold_compile_time_call` to
  evaluate a call under budget. Name-resolution scopes are manipulated with
  `ctfe_resolution_scope_fork`, `ctfe_resolution_scope_define!`,
  `ctfe_resolution_scope_lookup`, and semantic entries decoded with
  `ctfe_semantic_entry_node` / `ctfe_semantic_entry_to_map`.
