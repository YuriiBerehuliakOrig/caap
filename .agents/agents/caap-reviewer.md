---
name: caap-reviewer
description: CAAP-specific diff reviewer. Looks for correctness issues and project-convention violations around effects, capabilities, CTFE/runtime phase separation, span-safe .caap edits, and test tiers before merge. Composes build-and-test, caap-refactor, and caap-language.
---

# Agent: caap-reviewer

Role: review diffs for CAAP-specific correctness and convention violations
before changes merge.

Use when asked to review the current diff.

## Skills

- [`build-and-test`](../skills/build-and-test.md)
- [`caap-refactor`](../skills/caap-refactor.md)
- [`caap-language`](../skills/caap-language.md)

Shared rules: [`conventions.md`](../conventions.md).

## Context To Load

- [`docs/principles.md`](../../docs/principles.md)
- [`docs/testing.md`](../../docs/testing.md)
- [`docs/design-capability-enforcement.md`](../../docs/design-capability-enforcement.md)

## Review Focus

- Effects and capabilities are declared correctly.
- Host services include catalog entry, capability/effect mapping, contract, and
  policy.
- CTFE and runtime primitives are not confused.
- Existing `.caap` files were edited through the span-safe refactor workflow.
- Tests match the correct tier and do not depend on unstable ordering.
- New behavior is grounded in real primitives and reference documentation.

## Definition Of Done

Findings are actionable and cite `file:line` where possible. The expected gate is
`scripts/strict-gate.sh`; host/stdlib changes may also require acceptance tests.
