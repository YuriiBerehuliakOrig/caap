# Codegen / LLVM Interface

**Source:** native head/type vocabulary in
[stdlib/backend/native_meta.caap](../../stdlib/backend/native_meta.caap), stdlib native prep in
[stdlib/backend/prep.caap](../../stdlib/backend/prep.caap), LLVM lowering in
[stdlib/backend/emit/llvm.caap](../../stdlib/backend/emit/llvm.caap), build/link wrapper in
[stdlib/backend/driver.caap](../../stdlib/backend/driver.caap), and CLI tool programs
[tools/s2_emit.caap](../../tools/s2_emit.caap) /
[tools/s2_build.caap](../../tools/s2_build.caap).

## Pipeline

The active backend is stdlib-owned and does not use the removed v1
`codegen_kit` / `llvm` provider pipeline.

```text
source file
  -> ctfe_compiler_load_surface_file_template
  -> stdlib.backend.emit.llvm.emit_program / emit_freestanding
  -> LLVM IR text
  -> stdlib.backend.driver.compile_file / compile_ir
  -> clang + caap-sys-runtime-ffi when runtime symbols are needed
```

`stdlib/boot/native_emit.caap` lazily loads the backend codegen modules and registers:

| Emitter | Contract |
|---|---|
| `stdlib.llvm.emit` | hosted program; `main` result becomes process exit code |
| `stdlib.llvm.emit_freestanding` | kernel object; no `main` / exit-code contract |
| `stdlib.native.emit` | kernel-source prep artifact for compatibility experiments |

[stdlib/backend/driver.caap](../../stdlib/backend/driver.caap) additionally exposes
`compile_surface_freestanding` — the one-call surface-source -> bare-ELF entry that lowers the
clike modules, declares them, lowers the entry, looks up the target, runs `compile_freestanding`,
and cleans up.

## Native head / type vocabulary

The native head and type vocabulary is centralized in
[stdlib/backend/native_meta.caap](../../stdlib/backend/native_meta.caap) so the gate and the
emitters share one source of truth:

- `native_heads` maps each native head to the backends that realize it. For a head that only
  LLVM realizes, the entry also carries a `wasm_gap` string — the WAT emitter's "no realization"
  reason for that head.
- `native_types` maps each native type token to its width/signedness metadata.

`prep` derives its pre-codegen gate scope vocabulary from `head_names` / `type_names`; the WAT
(wasm) emitter derives its reject set from `wasm_gap_of`. Concretely, a wasm program that uses an
LLVM-only head — `asm`, the `volatile_*` MMIO pair, or the `atomic_*` family — now gets a precise
located diagnostic naming the head, instead of falling through to a generic "unsupported form" error.

## Strict native profile

`prep` carries an opt-in STRICT native profile (`set_strict!` / `clear_strict!` / `strict?`), OFF
by default. When pinned, the gate rejects a declared type token that is not a known native type
(`native_meta.native_type?`), a program struct/union, or a pointer.

## Supported Native Subset

The LLVM kit lowers the typed native subset produced by stdlib forms and prep:

- typed `defn` signatures drive ABI widths (`u8`, `i32`, `f64`, pointers, etc.);
- integer/float arithmetic, comparisons, bit ops, shifts, div/rem;
- `if`, `while`, `do`, local `bind`, `ref` / `deref` / `set_ref`;
- structs as named LLVM aggregates, constructors as `insertvalue`, field reads
  as `extractvalue`;
- strings through the runtime `{ptr, i64}` ABI where needed;
- globals, extern declarations, typed pointers, pointer arithmetic/read/write;
- fixed stack/global arrays and function pointers;
- MMIO volatile read/write, inline asm, and freestanding object emission.

Unsupported constructs must produce diagnostics, not silently wrong IR. Treat
[stdlib/backend/emit/llvm.caap](../../stdlib/backend/emit/llvm.caap) as the detailed contract.
