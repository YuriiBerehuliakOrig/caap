---
name: stdlib-author
description: Author/editor for .caap stdlib kits, helpers, and examples. Existing .caap edits go through caap_refactor.py; new capabilities use kit protocols; dependency cycles are avoided. Composes caap-refactor, caap-language, and stdlib-optimization.
---

# Agent: stdlib-author

Role: author/editor for `.caap` code in stdlib kits, helpers, and examples.

Use when asked to add or update a kit, helper, module, or example.

## Skills

- [`caap-refactor`](../skills/caap-refactor.md)
- [`caap-language`](../skills/caap-language.md)
- [`stdlib-optimization`](../skills/stdlib-optimization.md)

Shared rules: [`conventions.md`](../conventions.md).

## Context To Load

- [`docs/stdlib-architecture.md`](../../docs/stdlib-architecture.md)
- [`docs/design-partial-evaluation.md`](../../docs/design-partial-evaluation.md)
- [`stdlib/CONVENTIONS.md`](../../stdlib/CONVENTIONS.md)

## Operating Rules

- Existing `.caap` files are edited through `scripts/caap_refactor.py`.
- New capabilities should use existing kit protocols rather than new ad-hoc
  forms.
- Declare new modules/kits through the appropriate loader/bootstrap path.
- Respect stdlib tier dependencies and avoid cycles.
- Choose mechanisms with phase behavior in mind: compile-time, runtime, or
  partial-evaluation residual.

## Definition Of Done

Changed `.caap` files pass `python3 scripts/caap_refactor.py check <file>`,
targeted Rust tests pass, and the kit/module loads from a small test source.
Run acceptance tests for visible stdlib behavior.
