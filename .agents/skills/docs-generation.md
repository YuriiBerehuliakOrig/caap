---
name: docs-generation
description: Generate or update detailed kernel and stdlib reference docs with explicit phase separation: compile-time CTFE vs runtime. Every entry must be checked against real code; missing features are marked explicitly rather than invented.
---

# Skill: docs-generation

Use this skill when reference documentation must be synchronized with code, such
as after adding a primitive, kit, helper, or mechanism.

Shared rules:

- [`conventions.md`](../conventions.md) section 2: ground claims in code.
- Section 5: state phase for every entry.

Primary artifacts:

- [`KERNEL_REFERENCE.md`](../../KERNEL_REFERENCE.md)
- [`docs/stdlib-reference.md`](../../docs/stdlib-reference.md)
- [`docs/mechanisms/`](../../docs/mechanisms/)

## Phase Rule

Every documented primitive or helper must explicitly state whether it is:

- compile-time (CTFE);
- runtime;
- dual-phase, including how residual runtime behavior works.

This is semantic, not cosmetic. Many primitives exist in only one phase.

## Procedure

1. Extract source data from code, not memory:
   - kernel/CTFE: `register(ev)` in [`caap/src/builtins/`](../../caap/src/builtins/)
     plus `KERNEL_PRIMITIVE_CLASSIFICATIONS`;
   - stdlib: registration calls in the relevant stdlib modules.
2. For each entry, record canonical name, arity, phase, effect/capability,
   short contract, and one example where useful.
3. Diff docs against code:
   - in code but missing from docs: add;
   - in docs but missing from code: remove or explicitly mark absent.
4. Mark absent features explicitly. An explicit absence is better than an
   invented primitive or shim.

## Output

Edit the Markdown reference files directly. Preserve the existing section
structure unless the task asks for a larger reorganization.

## Guardrails

- Do not document a primitive that does not correspond to real registration
  code.
- If docs and code disagree, code is the source of truth for docs.
- Do not paste implementation bodies; document the contract and link or cite the
  relevant code location when appropriate.
