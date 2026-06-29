# ADR-0001: Specialization Budget For Partial Evaluation

Status: accepted design.

Context: [`design-partial-evaluation.md`](design-partial-evaluation.md).

## Context

Partial evaluation can generate one residual function for each unique
combination of static arguments. Without a limit, the number of residual copies
can grow combinatorially. That threatens memory use, compile time, and compiler
termination when compile-time evaluation itself does not terminate.

Two options were considered:

1. Unlimited memoization. Cache every residual forever. This gives the best
   runtime result in the happy path, but has no resource bound and conflicts
   with deterministic, bounded compilation.
2. Budgeted specialization with runtime fallback. Limit the number of
   specializations; after the limit, leave new combinations unspecialized.

## Decision

Use budgeted specialization with memoization inside the budget.

This combines the two options: unlimited memoization is the same mechanism with
an infinite budget.

Rules:

- Residuals are memoized by `(stable function id, fingerprint of static args)`
  while the specialization budget remains.
- When the budget is exceeded, new inferred combinations fall back to the
  unspecialized runtime residual.
- Already-created residuals remain cached.
- Fallback is silent only for inferred binding-time decisions.
- For explicit annotations, budget exhaustion is a compile error. An explicit
  annotation is a requirement, not a hint.

## Consequences

Positive:

- Compile-time resources are bounded.
- Termination is controlled by a visible budget.
- Memoization still helps common repeated static combinations.
- The trace can report `budget_exceeded`.

Negative:

- Static-argument fingerprints must be deterministic, including composite/map
  values.
- Behavior depends on the budget value, so the budget must be explicit and
  documented rather than hidden.
