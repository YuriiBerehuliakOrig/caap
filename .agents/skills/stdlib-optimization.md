---
name: stdlib-optimization
description: Find better stdlib mechanisms: reduce duplicate kits/helpers to one mechanism, choose the correct phase (compile-time, runtime, partial evaluation residual), and remove unnecessary dependencies while preserving behavior.
---

# Skill: stdlib-optimization

Use this skill when stdlib has duplicated mechanisms, wrong-phase helpers, or
avoidable dependencies and the goal is behavior-preserving improvement.

Shared rules:

- [`conventions.md`](../conventions.md) section 1: edit `.caap` through the
  script.
- Section 4: prefer one mechanism.
- Section 5: reason about phases and partial evaluation.
- Section 6: use golden lowered output.

Useful maps:

- [`docs/stdlib-architecture.md`](../../docs/stdlib-architecture.md)
- [`stdlib/CONVENTIONS.md`](../../stdlib/CONVENTIONS.md)
- [`docs/design-partial-evaluation.md`](../../docs/design-partial-evaluation.md)

Use [`caap-refactor`](caap-refactor.md) for `.caap` edits.

## Two Optimization Axes

1. Mechanism. Look for repeated helper bodies, repeated registration
   boilerplate, parallel capability forms, and unnecessary module dependencies.
   Prefer one parameterized mechanism over several local copies.
2. Phase. Ask whether inputs are static, dynamic, or mixed. Static pure work can
   fold at compile time; mixed work may leave a residual; dynamic work belongs
   in runtime/native lowering.

## Procedure

1. Inventory the candidate helpers/kits/modules and their dependencies.
2. Capture golden lowered output for affected examples/modules before editing.
3. Refactor with the span-based workflow.
4. Compare golden output after the change. Any unplanned difference means the
   change altered behavior.

## Verification

Run `python3 scripts/caap_refactor.py check <file>` on changed `.caap` files,
targeted Rust tests, and acceptance when stdlib behavior is visible.

## Guardrails

- Optimization is behavior-preserving by default.
- Do not introduce dependency cycles.
- Do not hide a semantic change inside a cleanup.
