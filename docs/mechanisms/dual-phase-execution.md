# Dual-Phase Execution

**Source:** [eval.rs](../../caap/src/eval.rs), [semantic/policy.rs](../../caap/src/semantic/policy.rs),
[values.rs](../../caap/src/values.rs). See also the deeper design note
[design-partial-evaluation.md](../design-partial-evaluation.md).

## The thesis: one evaluator, two phases

CAAP does **not** have a separate "macro language" and "runtime language". There
is a single tree-walking evaluator (`Evaluator`, [eval.rs](../../caap/src/eval.rs))
that interprets the three IR node kinds (`Name` / `Literal` / `Call`) in one of
two **phases**:

- **compile-time (CTFE)** — runs while the compiler is building a unit;
- **runtime** — runs when the compiled program executes.

The evaluator carries a `phase: PhasePolicy` field
([eval.rs:29](../../caap/src/eval.rs#L29)); the phase gate is
[`require_phase`](../../caap/src/eval.rs#L790). "comptime" and "runtime" are the
two **degenerate ends** of one mechanism (partial evaluation), not two features.

## `PhasePolicy`

**Source:** [semantic/policy.rs:10](../../caap/src/semantic/policy.rs#L10)

```rust
pub enum PhasePolicy { Runtime, CompileTime, Dual }
```

Every symbol/binding carries a phase policy that says where it is *allowed* to
evaluate:

| Variant | Meaning |
|---|---|
| `Runtime` | runtime-only — using it at compile time is a phase error. |
| `CompileTime` | compile-time-only — e.g. the `ctfe_*` compiler primitives. |
| `Dual` | usable in both phases — most pure stdlib functions. |

`Dual` is a *permission* ("this definition may run in either phase"), distinct
from the partial-evaluation notion of a value's binding-time (see below).

## Binding-time vs phase policy

- **PhasePolicy** is a static, per-symbol *permission*.
- **Binding-time** is a per-*expression*, per-call property: is every argument
  known (static) at compile time, or does some input only exist at runtime
  (dynamic)?

A `Dual` function called with all-static arguments can be **folded** at compile
time; the same function with a dynamic argument runs at runtime. This is
partial evaluation, and it is the unifying frame for "constant folding",
"compile-time functions", and "macros" — they are all the same fold mechanism at
different binding-time points.

## Folding (compile-time evaluation of calls)

When a call's arguments are all literals, the `evaluate_calls` provider asks the
kernel to evaluate it under a step + depth budget and replaces the call with the
resulting literal. See [CTFE & surface forms](ctfe-and-surface-forms.md) and the
partial-evaluation design note. Key facts:

- Folding runs in a dedicated **`fold_calls`** stage, *after* the
  normalize fixpoint, so it captures final (post-rewrite) bodies.
- The kernel reconstructs the unit's top-level bindings into a module
  environment, so a fully-static call folds even across sibling functions and
  recursion.
- Only **scalar** results (int / bool / null) are materialized as literals; a
  structured result leaves the call to run at runtime
  ([provider_context_helpers.rs](../../caap/src/builtins/provider_context_helpers.rs)).
- A fold that exceeds its step/depth budget or hits a runtime-only operation
  fails *conservatively* and the call is left to run at runtime.

### Explicit control (annotations)

Inference is opportunistic with a silent runtime fallback. A user can **require**
folding with explicit annotations enforced by the `pe_check` provider:
`caap.pe.comptime` (a function whose every call must fold), `caap.pe.fold` (this
call must fold), `caap.pe.static_param` (named params must be static). An unmet
explicit requirement is a **hard compile error**, not a silent fallback. See
[design-partial-evaluation.md §6](../design-partial-evaluation.md).

## Budgets

Compile-time evaluation is bounded so a non-terminating fold cannot hang or
overflow the compiler:

- a **step budget** bounds total evaluation work;
- a **depth cap** (`DEFAULT_CTFE_FOLD_DEPTH_BUDGET`,
  [eval.rs](../../caap/src/eval.rs)) bounds native recursion depth so deep
  recursion fails the fold instead of aborting the compiler.

## `runtime_error`

`runtime_error` ([control_flow.rs](../../caap/src/builtins/control_flow.rs))
raises an error. At runtime it surfaces as a `CAAP-RUNTIME-001` diagnostic; in a
fold it makes the fold fail (so the call falls back to runtime) — a single
primitive that respects the phase it runs in.
