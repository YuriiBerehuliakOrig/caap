# How to add a native operation head

A *native head* is a CTFE call head the native backends lower directly to machine
code but the kernel evaluator does not implement — `ptr_read`, `ptr_add`, `cast`,
`atomic_load`, `volatile_write`, `asm`, … (see [KERNEL_REFERENCE.md](../../KERNEL_REFERENCE.md)
and [builtins.md](../builtins.md) for the full set). Adding one touches **one
declarative table plus the backend(s) that realize it** — the vocabulary is
centralized so the layers can't drift.

## The single source of truth

[stdlib/backend/native_meta.caap](../../stdlib/backend/native_meta.caap)
(`stdlib.backend.native_meta`) holds the vocabulary ONCE:

- `native_heads` — `head -> {backends: {…}, wasm_gap?}`. The pre-codegen gate's
  scope seed (`head_names`), the WAT emitter's reject set (`wasm_gap_of`), and the
  strict profile all derive from this table.
- `native_types` — `name -> {bits?, signed?, float?, …}` (the width/signedness the
  strict profile validates against).

Because prep, the LLVM emitter, and the WAT emitter all derive from this table,
adding a head is a deliberate, checked edit rather than three by-convention edits.

## A new head touches at most four places

1. **The table** — add a row to `native_heads` in `native_meta.caap`. Tag the
   `backends` it realizes (`{"llvm" true "wasm" true}` for a portable op; an
   LLVM-only op lists only `"llvm"` and carries a `wasm_gap` string — the WAT
   emitter rejects it with that note automatically). If the op introduces a new
   type token, add it to `native_types` too.
2. **LLVM** — add an emit arm to the head `cond` in `lower_dispatch`
   ([emit/llvm.caap:945](../../stdlib/backend/emit/llvm.caap#L945); see
   `((eq head "atomic_load") …)` at L1411 or `((eq head "ptr_add") …)` at L1688 as
   templates). It returns a `{type, ct, text}` value record.
3. **WAT** — if wasm realizes it, add the mirror arm to `lower_call`
   ([emit/wasm.caap:454](../../stdlib/backend/emit/wasm.caap#L454)). If it does
   NOT (it is `"llvm"`-only with a `wasm_gap`), do nothing: the table-driven reject
   arm at [emit/wasm.caap:753](../../stdlib/backend/emit/wasm.caap#L753) already
   produces a precise located diagnostic.
4. **The pin test** — update the canonical set in
   [caap/tests/native_meta_tests.rs](../../caap/tests/native_meta_tests.rs)
   (`native_meta_head_and_type_sets_are_pinned`). The pin is deliberate: a head
   added without updating it FAILS, forcing the change to be intentional.

A pointer-creating or specially-typed op may also need an arm in the alias
analysis ([semantics/passes/alias.caap](../../stdlib/semantics/passes/alias.caap))
or type inference — only if it participates in those analyses.

## The rule that will bite you: eval ≠ native

A native head is, by definition, NOT a kernel builtin — it only exists for the
backends. A program using it can be cross-compiled but **cannot be run on the bare
evaluator**. If the op should also work under eval, it is a kernel builtin, not a
native head. Keep the two backends honest: if `backends` claims `"wasm"`, you owe
`lower_call` a real arm; if it claims only `"llvm"`, give it a `wasm_gap` so the
WAT side rejects it precisely instead of falling through to "unsupported form".

## Verify (run these, in order)

`cargo build -p caap-cli` first (the tools call `./target/debug/caap`).

```bash
# 1. the table + edited backends still parse
python3 scripts/caap_refactor.py check stdlib/backend/native_meta.caap \
    stdlib/backend/emit/llvm.caap stdlib/backend/emit/wasm.caap

# 2. the pinned vocabulary + the wasm-reject guard (one test binary)
cargo test -p caap-core --test native_meta_tests

# 3. the backend invariants + the full native codegen suite (one binary each)
cargo test -p caap-core --test codegen_invariants_tests
cargo test -p caap-core --test stdlib_codegen_tests        # non-ignored

# 4. eyeball the LLVM IR your arm emits (write a tiny native program using the head).
#    s2_emit's own backend loads on demand, so the bare bootstrap suffices.
cargo run -p caap-cli -- stdlib/bootstrap.caap tools/s2_emit.caap your_prog.caap > out.ll
```

If a wasm program using a head you marked `"llvm"`-only must show a precise
message (not "unsupported form"), assert it the way
`wasm_rejects_atomic_load_with_precise_diagnostic` does in `native_meta_tests.rs`.
