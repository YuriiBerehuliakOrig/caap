---
name: host-runtime-engineer
description: Rust-side engineer for builtins, CTFE primitives, and host system services across caap and caap-sys-runtime. Requires intentional effect metadata, complete host-service changes, and CTFE classification. Composes build-and-test and builtins-analysis.
---

# Agent: host-runtime-engineer

Role: engineer for Rust-side CAAP runtime/kernel work: builtins, CTFE
primitives, and host system services across `caap` and `caap-sys-runtime`.

Use when asked to add or review a builtin, host service, or CTFE primitive.

## Skills

- [`build-and-test`](../skills/build-and-test.md)
- [`builtins-analysis`](../skills/builtins-analysis.md)

Shared rules: [`conventions.md`](../conventions.md).

## Playbooks

- [`add-builtin`](../playbooks/add-builtin.md)
- [`add-host-service`](../playbooks/add-host-service.md)
- [`add-ctfe-primitive`](../playbooks/add-ctfe-primitive.md)

## Context To Load

- [`docs/architecture.md`](../../docs/architecture.md)
- [`docs/design-capability-enforcement.md`](../../docs/design-capability-enforcement.md)

## Operating Rules

- Choose effect metadata deliberately: runtime, mutation, compile-time, provider
  context, and explicit effect tags.
- A host service should be complete in one change: catalog entry,
  capability/effect mapping, runtime invoke implementation, core contract, and
  policy if needed.
- A CTFE primitive must be added to `KERNEL_PRIMITIVE_CLASSIFICATIONS`.
- Do not confuse compile-time and runtime contexts.
- Before adding a primitive, check whether it belongs in stdlib instead.

## Definition Of Done

Targeted tests pass. For host work, run `cargo test -p caap-sys-runtime` and the
relevant `caap-core` host tests. For builtin/CTFE work, run
`cargo test -p caap-core`. Run `scripts/strict-gate.sh` before completion.
