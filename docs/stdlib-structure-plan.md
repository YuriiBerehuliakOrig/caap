# Stdlib Structure Plan

Status: active restructuring plan for keeping `stdlib/` readable and
extendable without changing public module names.

## Goal

`stdlib/` should be easy to enter from the top, easy to extend in one domain, and
hard to accidentally couple across tiers. The current top-level layout is the
right shape; the main maintenance cost is large implementation files and uneven
local documentation.

## Target Shape

```text
stdlib/
  bootstrap.caap       boot order and root declarations
  boot/                expander, forms, loader, commands, opt-in boot profiles
  lib/                 reusable libraries with no frontend/backend policy
  syntax/              AST/IR helpers and rendering
  semantics/           types, effects, passes, SSA/dataflow
  frontend/            opt-in source surfaces and lowering
  backend/             native prep, emitters, and build drivers
  sys/                 host capability facades
  storage/             storage/binary DSL
  bare/                native-only bare-metal wrappers
```

Do not rename these top-level domains unless an architectural boundary changes.

## Facade Rule

Public imports should target stable facade modules. Internal leaves may move or
split as implementation detail.

Examples:

- `stdlib.frontend.clike` remains the public C-like surface entrypoint.
- `stdlib.semantics.types.infer` remains the loader type-pass entrypoint.
- `stdlib.backend.emit.llvm.lowering` remains the LLVM lowering entrypoint.

Leaf modules under those namespaces can own focused implementation areas, but
they are not a compatibility promise unless their README explicitly says so.

## File Size Guidance

New implementation files should normally stay under 450 lines. A file over 700
lines is a restructuring candidate unless it is a generated catalog, a test
corpus, or a deliberately flat table.

Preferred split order:

1. Extract pure helper leaves.
2. Extract domain dispatch leaves behind a service map.
3. Keep the existing facade exports unchanged.
4. Run targeted tests after each split.

## Current Hotspots

The first behavior-preserving splits are:

- `frontend/clike.caap`: keep the facade; move expression, type, declaration,
  and module-frame helpers into `frontend/clike/*`.
- `semantics/types/infer.caap`: keep the bootstrap-facing facade; move result
  helpers, signature store, and call/form walking into leaves loaded before the
  facade.
- `backend/emit/llvm/lowering.caap`: keep `lower` and `lower_dispatch` as the
  public import surface; move control, memory, call, and aggregate lowering into
  internal leaves.

`backend/prep.caap`, `semantics/ssa.caap`, and `boot/forms.caap` are intentionally
deferred until the first three splits are stable.

## Verification

For each phase:

```bash
python3 scripts/caap_refactor.py check <changed.caap>
git diff --check
```

Then run the relevant targeted Rust tests:

```bash
cargo test -p caap-core stdlib_governance_tests
cargo test -p caap-core stdlib_types_tests
cargo test -p caap-core stdlib_codegen_tests
```
