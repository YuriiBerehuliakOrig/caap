---
name: readability-refactor
description: Reduce duplication and boilerplate for readability in grammar/lowering, examples, and repeated Rust or CAAP patterns. Behavior-preserving refactor with golden lowered-output checks; no contract changes.
---

# Skill: readability-refactor

Use this when code works but has repeated or verbose patterns: duplicated demo
preambles, grammar rules, lowering hooks, register blocks, or similar boilerplate.

Shared rules:

- [`conventions.md`](../conventions.md) section 3: preserve behavior.
- Section 4: prefer one mechanism.
- Section 6: use golden lowered output.
- Section 1: edit existing `.caap` files through the script.

## Scope

- Grammar/lowering: repeated surface forms or desugar hooks can become shared
  helpers when that improves clarity. Remember that lower hooks are isolated and
  should not rely on shared mutable state.
- Examples: repeated module/import/pass wiring can be factored while keeping each
  example readable and runnable.
- Rust patterns: repeated builtin registration or match arms may become a table
  or helper when local clarity remains good.

## Procedure

1. Identify duplication. Three or more copies of the same pattern is a strong
   candidate.
2. Capture golden lowered output before editing, using the relevant tool program
   under the flagless launcher.
3. Refactor. Use `caap-refactor` for `.caap`; edit Rust normally.
4. Compare golden output after the change. Any unplanned difference is a
   behavior change.

## Verification

Run `python3 scripts/caap_refactor.py check <file>` on changed `.caap` files,
compare goldens, and run `scripts/strict-gate.sh` when the change is ready.

## Guardrails

- Do not change the contract.
- Do not sacrifice local readability only to remove duplication.
- Do not optimize isolated lower hooks through shared mutable state.
## Guardrails

- Do not recommend removal before checking consumers in `stdlib/` and
  `examples/`.
- A `move-to-stdlib` recommendation should name the stdlib mechanism that would
  replace the primitive.
