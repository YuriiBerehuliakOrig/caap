# Provider & Pass Pipeline

> Status: historical v1 mechanism note. The v1 `stdlib/kits/compiler_kit`
> provider pipeline was removed with the v1 stdlib. For the active stdlib load
> pipeline, read [../caap-spec.md](../caap-spec.md) and
> [../stdlib-reference.md](../stdlib-reference.md).

**Source:** registration primitives in
[compiler_providers.rs](../../caap/src/builtins/compiler_providers.rs). The
removed v1 toolchain lived under `stdlib/kits/compiler_kit/`, starting at
`toolchain_foundation.caap`; those paths are intentionally no longer present in
the active stdlib.

The CAAP compiler is itself a CAAP program. Compiling a unit means running a
**DAG of stages**, each containing ordered **providers** that read and rewrite
the unit's IR and attach **facts**. The whole toolchain is registered from
stdlib at bootstrap — there is no hard-coded pass list in the kernel.

## Stages

A stage is a named pipeline phase with declared dependencies. Stages are
registered with `ctfe_compiler_stage_register`
([compiler_providers.rs](../../caap/src/builtins/compiler_providers.rs),
arity 2..7). The canonical v1 pipeline was:

```
unit_input
  → parse_surface
  → normalize_before_resolve
  → resolve_names
  → normalize_after_resolve
  → fold_calls          (partial evaluation: fold + specialize)
  → validate_graph
  → compile_unit
```

Stages carry a **family** (e.g. `surface`, `semantic_normalization`,
`resolve_names`, `validate_graph`, `compile_unit`) used to group related work.

`ctfe_compiler_list_stages` ([compiler_query_runtime.rs](../../caap/src/builtins/compiler_query_runtime.rs))
returns the live stage list.

## Providers

A provider is a function `(lambda (ctx root) …)` registered into a stage with
`ctfe_compiler_provider_register`
([compiler_providers.rs](../../caap/src/builtins/compiler_providers.rs),
arity 4..7):

```
(ctfe_compiler_provider_register
  compiler
  "<provider-name>"
  "<stage>"
  <provider-fn>
  (list_of "<required-provider>" …)         ; intra-stage ordering deps
  (list_of "read_ir" "write_ir" …)          ; declared effects (capabilities)
  (assoc (map_of) "family" … "reads" … "writes" … "cache_scope" … "resume_policy" …))
```

Key points, all enforced by the kernel:

- **Effects are capabilities.** A provider may only use a primitive whose effect
  it declared. E.g. `ctfe_provider_annotation_get` requires `read_attributes`;
  `ctfe_provider_diagnostics_error` requires `emit_diagnostics`. Declaring the
  wrong effects makes the call fail at compile time. (Effect tags surfaced in
  registrations include `read_ir`, `write_ir`, `read_facts`, `write_facts`,
  `read_symbols`, `write_symbols`, `read_attributes`, `write_attributes`,
  `emit_diagnostics`, `use_host_services`.)
- **Ordering within a stage** is by `requires` dependency edges, then
  registration order.
- The provider's `(ctx root)` arguments give a **provider context** (the handle
  through which all `ctfe_provider_*` primitives operate) and the unit's IR
  **root** node.

`ctfe_compiler_list_providers` returns the live provider list.

## The provider context

Inside a provider, the `ctx` value is the gate to the unit. The richest surface
of `ctfe_provider_*` primitives lives in
[provider_context_runtime.rs](../../caap/src/builtins/provider_context_runtime.rs)
and [provider_context.rs](../../caap/src/builtins/provider_context.rs). Common
operations:

- `ctfe_provider_unit ctx` → the unit handle (for `ctfe_unit_*` queries).
- `ctfe_provider_traversal_walk ctx root fn [opts]` → walk nodes (optionally
  filtered by `"kind"`).
- `ctfe_provider_node_replace ctx node spec` → rewrite a node (needs `write_ir`).
- `ctfe_provider_fact_get/set`, `ctfe_provider_annotation_get/set`.
- `ctfe_provider_diagnostics_error/warning/note/hint ctx node message code`.
- `ctfe_provider_synthesize_internal_definition! ctx name value` → add a
  top-level binding (needs `write_ir` + `write_symbols`).

## Return value, change reporting & the restart mechanism

A provider returns a boolean: `true` = "I changed the graph", `false` = "no
change". Reporting a change can trigger the pipeline to **restart** from an
earlier stage so downstream analyses re-run on the new graph — controlled by the
provider's `resume_policy` (`"safe"` allows restart; `"never"` forbids it).

This fixpoint loop is budgeted: a provider that keeps reporting change forever
exhausts the **query restart budget** (the compiler error
`query restart budget exhausted while restarting from <stage>`). Two practical
consequences:

- A rewrite pass that emits new name-referencing nodes should re-run name
  resolution (the pass-kit `reresolve` helper) so the new nodes are well-formed
  regardless of provider ordering.
- A provider that only writes **metadata** (an annotation/fact) but does not
  change graph *structure* should report `false` (and may use
  `resume_policy "never"`): a metadata write that requests a restart can recreate
  node ids each pass and never reach a fixpoint.

## Facts & fact schemas

Providers attach **facts** to nodes — typed key/value analysis results, distinct
from user **annotations** (facts are produced by analysis; annotations are
user-supplied; both ultimately live in the unit's semantics). Fact predicates
are registered via `ctfe_compiler_fact_schema_register` /
`ctfe_compiler_fact_schema_type_bridge_register`
([compiler_providers.rs](../../caap/src/builtins/compiler_providers.rs)). Core
predicates seen in the toolchain include `caap.fact.resolved_name`,
`caap.fact.resolved_block`, and `caap.fact.call_semantics`.

## Inspecting the pipeline

`ctfe_compiler_query_execution`
([compiler_query_runtime.rs](../../caap/src/builtins/compiler_query_runtime.rs))
runs the pipeline (or a sub-range) and returns the resulting unit + facts for
inspection — the basis of the toolchain tests.
