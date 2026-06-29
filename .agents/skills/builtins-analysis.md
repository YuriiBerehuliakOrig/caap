---
name: builtins-analysis
description: Evaluate core builtins and CTFE primitives for necessity, correctness, effect metadata, classification, ergonomics, and minimal surface area. Recommends keep, merge, move to stdlib, or metadata/classification fixes.
---

# Skill: builtins-analysis

Use this skill when auditing existing builtins/CTFE primitives or deciding
whether a new primitive belongs in the kernel.

Shared rules:

- [`conventions.md`](../conventions.md) section 2: verify against code and
  references.
- Section 4: keep the core surface minimal.
- Section 5: distinguish CTFE from runtime.

Inventory:

- [`caap/src/builtins/`](../../caap/src/builtins/)
- `KERNEL_PRIMITIVE_CLASSIFICATIONS` in
  [`caap/src/builtins/mod.rs`](../../caap/src/builtins/mod.rs)
- [`docs/builtins.md`](../../docs/builtins.md)

Adding primitives:

- [`add-builtin`](../playbooks/add-builtin.md)
- [`add-ctfe-primitive`](../playbooks/add-ctfe-primitive.md)

## Evaluation Axes

1. Necessity. Is this true kernel substrate, or can it be expressed in `.caap`
   or a stdlib kit?
2. Correctness. Are phase, arity, handler shape, effect tags, and CTFE
   classification accurate?
3. Ergonomics. Is the name stable and canonical? Is argument behavior
   predictable? Are bad arguments diagnosed rather than silently repaired?

## Procedure

1. List primitives by builtins module and registration site.
2. Fill the three evaluation axes for each primitive.
3. Compare the code surface to `KERNEL_REFERENCE.md` runtime and CTFE sections.
4. Report a table: primitive, verdict, code location, and rationale.

Possible verdicts:

- `keep`
- `merge`
- `move-to-stdlib`
- `fix-metadata`
- `fix-classification`

## Verification

Run:

```bash
cargo test -p caap-core
scripts/strict-gate.sh
```

## Guardrails

- Do not recommend removal before checking consumers in `stdlib/` and
  `examples/`.
- A `move-to-stdlib` recommendation should name the stdlib mechanism that would
  replace the primitive.
