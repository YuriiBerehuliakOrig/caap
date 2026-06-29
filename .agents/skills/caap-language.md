---
name: caap-language
description: Understand CAAP syntax, semantics, primitives, and phase boundaries: name-first expression-only surface, kernel S-expressions, CTFE vs runtime, and KERNEL_REFERENCE navigation.
---

# Skill: caap-language

Use this skill to understand CAAP syntax, semantics, primitives, or phase
boundaries.

Shared rules:

- [`conventions.md`](../conventions.md) section 2: ground claims in code.
- [`conventions.md`](../conventions.md) section 5: distinguish CTFE from runtime.

Source of truth: [`KERNEL_REFERENCE.md`](../../KERNEL_REFERENCE.md). This skill
is navigation and caution, not a replacement for the reference.

## Surface Syntax

See [`docs/name-first-expression-only-grammar.md`](../../docs/name-first-expression-only-grammar.md).

Key points:

- Everything is an expression.
- Named declarations start with the name.
- Function shape: `name (params...) returnType = { ... }`.
- Type shape: `Name type = { fieldName fieldType (= default)? }`.
- Module shape: `name module = { ... }`.

Underneath, the kernel syntax is S-expressions. The "Surface Grammar" section
of `KERNEL_REFERENCE.md` describes atoms, lists, and desugaring head symbols.

## KERNEL_REFERENCE Map

- Sections 0-1: surface forms and kernel special forms.
- Section 2: arithmetic, numeric conversions, equality, ordering.
- Sections 3-4: mutable collections and map/sequence access.
- Sections 5-6: strings, reflection, dispatch.
- Section 6a: runtime syntax values and macros.
- Section 7: stdlib module layer.
- Sections 8-15: CTFE and compile-time compiler access.

## Run An Example

```bash
./target/debug/caap stdlib/bootstrap.caap tools/run.caap <file>.caap
```

Examples live under [`examples/`](../../examples/).

## Cautions

- Verify primitive names before using them.
- Do not confuse compile-time and runtime primitives.
- Native codegen supports a deliberate subset and rejects unsupported heads.
