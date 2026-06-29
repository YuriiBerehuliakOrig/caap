# Partial Evaluation And Dual-Phase Functions

> Status: historical design note with active mechanisms. The current public
> behavior is described in [`caap-spec.md`](caap-spec.md) and
> [`mechanisms/dual-phase-execution.md`](mechanisms/dual-phase-execution.md).
> The implemented stdlib transforms are `peval` and `pe` under
> `stdlib/semantics/passes/`. They are optional and are not enabled by the base
> bootstrap because fixed-point work has measurable load cost.

The specialization-budget decision is recorded in
[`adr-0001-specialization-budget.md`](adr-0001-specialization-budget.md).

## Thesis

Compile-time execution, runtime execution, and "dual phase" behavior are not
three separate features. They are endpoints of one mechanism: partial
evaluation.

```text
all inputs static        partial evaluation        all inputs dynamic
       |                         |                         |
residual is a literal       mixed residual          residual is original
    compile time            specialization              runtime
```

- Compile-time evaluation is PE where all inputs are static and the callee is
  foldable relative to runtime.
- Runtime execution is PE where no inputs are static.
- A dual-phase function is one definition whose residual is chosen per call
  site by binding-time analysis.

`PhasePolicy::Dual` is only a per-symbol permission that a callable may run in
both phases. It is a prerequisite for this model, not the full PE mechanism.

## Current Implementation

The active stdlib implementation is deliberately conservative:

- `peval` performs whole-module const propagation, constfold, simplify, and DCE
  to a fixed point.
- `pe` reads static-parameter annotations and specializes call sites by inlining
  static arguments and re-running `peval`.
- `static!(name, indices)` is the data API for binding-time metadata.
- `(static_params name idx...)` is the in-source form that expands to an inert
  marker read by `pe`.
- The base bootstrap does not enable PE. Compose `stdlib/boot/peval.caap` after
  `stdlib/bootstrap.caap` when the session wants it.

The implemented path is behavior-preserving: it folds constants and removes dead
code, but should not change program meaning.

## Alignment With Principles

| Principle | PE requirement |
| --- | --- |
| Minimal kernel / callee semantics | No new IR nodes. Binding-time facts attach to existing `Name`, `Literal`, and `Call` trees. |
| Policy-driven behavior | Foldability is a callee policy, not an evaluator name list. |
| Libraries over features | Analyses and specialization live in stdlib passes. |
| Determinism | Specialization keys use stable IDs and fingerprints. Budgets bound compile-time work. |
| No silent fallback | Inferred specialization may fall back; explicit annotations are requirements. |
| Observability | Folding, specialization, and budget events should be traceable. |

## Existing Substrate

PE builds on mechanisms CAAP already has:

- One evaluator can run in runtime or compile-time phase.
- CTFE can inspect and rewrite IR through explicit APIs.
- The semantic graph carries facts and stable IDs.
- Provider contexts expose effect-checked mutation.
- The stdlib type/effect layer can prove enough purity for safe folding.
- The backend already consumes residual runtime IR.

Gaps in the broader target model:

- A first-class `FoldPolicy` surface in the core policy model.
- Complete per-argument binding-time facts across all callable forms.
- A fully unified trace contract for all fold/specialization decisions.
- A user-facing annotation surface beyond current inert/source forms.

## Binding-Time Model

Binding time is a small lattice:

```text
static <= dynamic
```

A node is static when its inputs are static and its callee is foldable relative
to runtime behavior. Any dynamic input makes the result dynamic and taints
downstream computation.

Dynamic sources include:

- runtime-only callees;
- runtime effects;
- unknown external input;
- budget exhaustion;
- unsupported or unproven purity.

The default is dynamic. Unknown information must not produce unsafe folding.

## Foldability

The target policy model is:

- `Always`: fold when inputs are static.
- `Never`: do not fold.
- `RuntimePure`: fold when the call is pure relative to runtime, even if it has
  compile-time bookkeeping effects.

The fold condition is:

```text
fold policy permits
AND required inputs are static
AND budget remains
AND evaluation succeeds within phase/effect constraints
```

## Budgets And Specialization Control

Compile-time evaluation needs cheap fuel. Budget exhaustion must be a structured
diagnostic or traceable fallback, not an infinite loop or host crash.

Specialized residuals are keyed by:

```text
(stable function identity, fingerprint of static arguments)
```

The accepted decision is budgeted memoization:

- memoize residuals while the specialization budget remains;
- when the budget is exceeded, inferred binding-time decisions fall back to the
  unspecialized runtime residual;
- already-created residuals stay cached;
- explicit annotations turn budget exhaustion into a compile error.

## Implemented Stdlib Strategy

`peval` composes existing pure tree rewrites:

1. Substitute literal and simple name bindings when capture-safe.
2. Fold pure literal calls.
3. Simplify dead branches and beta-reducible calls.
4. Remove dead local bindings when the removed value is total and effect-free.
5. Iterate until `node_eq` stabilizes or the fixed-point cap is reached.

`pe` adds binding-time metadata:

1. Read static-parameter markers.
2. Find calls whose static arguments are literal.
3. Inline arguments into the callee body.
4. Remove static parameters/arguments from the residual call.
5. Run `peval` over the residual.

This gives useful partial specialization without introducing a separate
compile-time function mechanism.

## Annotation Semantics

Current and target annotation behavior follows one rule:

```text
explicit annotation = requirement
inferred binding time = opportunity
```

Practical consequences:

- If an inferred specialization fails, fallback to runtime is allowed.
- If an explicit "fold this" or "this parameter is static" annotation fails,
  compilation should report an error.
- A check pass should be read-only and diagnostic-producing; it should not repair
  failed specialization silently.

## Observability

Useful trace events:

- `binding_time_resolved`
- `fold_attempted`
- `fold_succeeded`
- `fold_failed`
- `specialization_created`
- `budget_exceeded`

Users need a way to answer: "was this folded, specialized, or left as runtime
code?"

## Phase Plan

| Phase | Work | Status |
| --- | --- | --- |
| 0 | Define the single-mechanism PE contract. | Done as design. |
| 1 | Binding-time facts and forward propagation. | Partially represented in stdlib pass metadata. |
| 2 | Compile-time budgets and trace events. | Partially available through evaluator/parse budget patterns. |
| 3 | All-static folding across function calls. | Available in conservative stdlib form. |
| 4 | Mixed static/dynamic residual specialization. | Available for annotated/static-argument cases. |
| 5 | Explicit annotations as hard requirements. | Available through current marker/check path where composed. |
| 6 | Remove duplicate folding paths. | Ongoing architectural cleanup target. |

## Open Questions

- Should the user-facing annotation syntax be a grammar extension, a form, or
  metadata API only?
- What is the exact deterministic fingerprint format for composite static
  values?
- Should "pure relative to runtime" remain stdlib policy or become a core
  predicate over effect policy?
- How much trace should be default vs opt-in?
