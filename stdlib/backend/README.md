# stdlib/backend — the codegen pipeline

Rank-5 codegen. These modules turn a stdlib program into native LLVM IR,
WebAssembly, or a kernel-source artifact, and drive the toolchain that links a
runnable binary. They are loaded ON DEMAND (`boot/native_emit.caap`, or
`load_module` by name) — an ordinary session that only evaluates code never pays
for them.

The whole tower is pure kernel + stdlib, with **zero stdlib-v1**.

## The files

| File | Module | Role |
|---|---|---|
| `native_meta.caap` | `stdlib.backend.native_meta` | the SINGLE declarative source of the native head/type vocabulary |
| `prep.caap` | `stdlib.backend.prep` | the shared FRONT-END: program → structured codegen tables (+ the pre-codegen gate) |
| `codegen_common.caap` | `stdlib.backend.codegen_common` | target-neutral classification shared by both emitters (signedness, op tables, comparisons) |
| `emit/llvm.caap` | `stdlib.backend.emit.llvm` | the LLVM-IR emitter |
| `emit/wasm.caap` | `stdlib.backend.emit.wasm` | the WAT (WebAssembly text) emitter, sibling of `llvm` |
| `driver.caap` | `stdlib.backend.driver` | the clang/runtime build driver (IR → object → link) |

The C-like and custom-grammar **surfaces** that feed this tower live one tier up
in [`stdlib/frontend/`](../frontend/) (`surface.caap`, `clike.caap`), not here.

## The flow

```
                       (use …) deps inlined, expanded, gated
  loaded unit ──► prep.caap ───► prep_units ──► structured program
                  (FRONT-END)        │           (the codegen tables)
       native_meta ─┘ (vocab)        │
                         ┌───────────┼────────────┬───────────────┐
                         ▼           ▼            ▼                ▼
                    prep.caap    llvm.caap    wasm.caap      llvm.caap
                    (assemble)   emit_program emit_program   emit_freestanding
                         │           │            │                │
                  kernel-source   LLVM IR        WAT          LLVM IR (no main)
                    (text)          │            │                │
                                    ▼            ▼                ▼
                              driver.caap    wat2wasm/      driver.caap
                              link_ir!       wasmtime       link_bare!
                                    │                              │
                               hosted binary                  bare ELF
                               (+ caap-sys-runtime)        (-ffreestanding)
```

## native_meta.caap — the single vocabulary source

The native backends understand heads the kernel evaluator does NOT (`ptr_read`,
`ptr_add`, `cast`, `volatile_write`, `atomic_load`, `asm`, …) and a set of native
type tokens (`i32`/`u32`/…/`ptr`). This module holds that vocabulary ONCE:

- `native_heads` — `head → {backends, wasm_gap?}`: each head tags the backends
  that realize it; an LLVM-only head also carries the WAT emitter's "no
  realization" reason.
- `native_types` — `name → {bits?, signed?, float?, aggregate?, element?, …}`: the
  width/signedness the strict profile validates against; an aggregate row also names
  its buffer `element` scalar type (e.g. string → `i8`).
- derived: `head_names` / `type_names` (the gate's scope seed), `backend_supports?`,
  `wasm_gap_of`, `native_type?`.

`prep` derives its gate vocabulary from `head_names`/`type_names`; the WAT emitter
derives its reject set from `wasm_gap_of`. So adding a native operation is one
deliberate edit to the table (+ the backend arm that lowers it), not three
hand-kept lists that silently drift — see
[docs/how-to/add-native-head.md](../../docs/how-to/add-native-head.md).

## prep.caap — the shared FRONT-END

The one module every backend depends on. It runs the SAME front-end the loader
runs — expand → `run_transforms` → gate (`check` + type pass + `run_passes`) —
but instead of evaluating it **flattens** the program into one translation unit
and hands the backend a STRUCTURED description.

- Inlines the root's `(use …)` dependencies through the loader's name→path
  machinery: each module once, into one flat namespace, with a single collision
  guard.
- `prep_units unit` → the codegen tables (`pairs`, `structs`, `unions`, `consts`,
  `externs`, `globals`, `global_arrays`, `finals`, `diagnostics`). **This map is
  the cross-backend contract** — `llvm.caap` and `wasm.caap` both read these exact
  field shapes; its schema (documented at the top of `prep.caap`) must stay stable,
  or both emitters break in lockstep.
- `prep_program unit` → `{text, diagnostics}`: the structured program rendered back
  to pure-kernel source (the compatibility / debug / inspection artifact).
  `extern`/`global` have no kernel-text spelling, so such programs are routed to
  the LLVM backend with a diagnostic.

The signatures in the tables come from the program's own `(defn …)` markers, so
the **stdlib type system drives the native ABI** — `(defn f ((a u8)) u8 …)` really
becomes `define i8 @f(i8 %a)`. The gate means a native build rejects the same
unknown-name / arity / type / lint findings the loader does, each located.

### The strict native profile (opt-in)

By default the gate is permissive: an unknown DECLARED type (an extern result, a
struct field, a global) that resolves to no native type lowers to a null IR type
silently. `prep` exposes an opt-in **strict** profile — `set_strict!` /
`clear_strict!` / `strict?`, pinned exactly like `target_pointer_bytes`. When
active, the gate additionally rejects any declared type token that is not a known
native type (`native_meta.native_type?`), a program struct/union, or a pointer —
a located diagnostic instead of silent wrong code. It is OFF unless a build opts in
(`compile_freestanding` / `compile_surface_freestanding` honor `opts.mode:
"strict"`), so ordinary builds are unchanged.

## llvm.caap / wasm.caap — the backends (siblings)

Both consume `prep_units` and lower the tables to target text via
`codegen_common`'s shared classification (signedness, the arithmetic/comparison op
sets). **Do not edit them from the front-end unit** — they own only target-level
work. The supported subset is deliberately small and CERTAIN: anything outside it
is a named diagnostic, never wrong code. Highlights: int arithmetic, comparisons,
3-arm `if` / `while`, `ref`/`deref`/`set_ref`, multi-bind locals, struct
aggregates, strings (via caap-sys-runtime symbols), direct calls, typed pointers,
inline `asm`, MMIO (`volatile_read`/`write`), lock-free atomics, and `println`.
`emit_freestanding` (LLVM) drops the `main`/exit-code contract for an OS-kernel
object.

A head that one backend cannot realize is rejected PRECISELY: the WAT emitter
rejects every LLVM-only head (`asm`, the `volatile_*` MMIO pair, the `atomic_*`
family) with `native_meta`'s `wasm_gap` note, not a generic "unsupported form".

The **eval = native parity** invariant: a program that evaluates one way under the
CTFE interpreter must compile to the same behaviour. Front-end changes must
preserve it.

### Debug info (the two backends differ honestly)

- **LLVM** has real DWARF: `emit_program_debug` / `emit_freestanding_debug` take a
  `debug_level` (`none` | `line-tables` | `full`) and write textual `!DI…` metadata
  that `clang -x ir` preserves into `.debug_info` (per-instruction locations +
  composite types). This is the path for source-level debugging.
- **WASM** is best-effort. WAT — the WebAssembly *text* format — has **no**
  debug-metadata syntax, and the only binary debug channel (a DWARF custom section)
  is written by the BINARY assembler (`wat2wasm`/LLVM), not a textual emitter. So
  `emit_program_debug` / `emit_module_debug` produce an **external Source Map v3**
  (`.wasm.map`-style JSON) under the `source_map` key, mapping each generated
  `(func …)` line back to its `.caap` source line. It is **function-level** — for
  instruction-accurate wasm source debugging, use the LLVM backend. The non-debug
  (`none`) WAT is byte-identical to before; `line-tables` and `full` behave
  identically because the wasm map is function-granularity.

## driver.caap — the toolchain driver

Drives clang over the emitted IR (the LLVM IR → object → link path), the same
plumbing a C toolchain has, but stdlib-native. All host access (`fs`/`os`/
`process`) resolves **lazily** through `svc`, and every failure is returned as DATA
(`{ok:false, error|diagnostics}`) — nothing is left half-written.

- `compile_file path out` — a native source file → a hosted binary (load → emit →
  link).
- `compile_ir specs out` — BUILT specs (lib/ast/ir trees) → a binary (render → emit
  → link); the runtime-codegen entry — build a program as data and compile it.
- `link_ir!` links against the **caap-sys-runtime staticlib** when the program
  references runtime symbols (`println`/strings across the ABI), discovering it via
  `$CAAP_SYS_RUNTIME_LIB` / the cargo target dir (built on demand); pure-compute
  programs link without it.
- `compile_freestanding path out opts` / `link_bare!` — a SEPARATE cross toolchain
  (e.g. Cortex-M): `triple`/`mcpu`/`datalayout`/`linker` from the `targets`
  registry, `clang -fuse-ld=lld -ffreestanding -nostdlib`, an optional ARM asm
  shim and linker script, NO host runtime. Input is an already-lowered KERNEL
  file; output is a bare ELF.
- `compile_surface_freestanding dir entry modules target out opts` — the one-call
  SURFACE → bare ELF helper: lowers each clike module to kernel + declares it,
  lowers the entry, looks `target` up in `targets`, and delegates to
  `compile_freestanding`, cleaning up generated files on success and failure. (The
  [URun slice](../../examples/urun/) uses exactly this.) See
  [docs/how-to/add-freestanding-target.md](../../docs/how-to/add-freestanding-target.md).
- `targets` — the cross-target registry (`{triple, cpu, datalayout, linker}` keyed
  by a short board name). Add a board here; nothing else changes.

## Loading order

`boot/native_emit.caap` loads the leaf groundwork (`render`/`equal`/`ir`) first,
then the kits in dependency order — `native_meta` (pure data, no deps) and `prep`
before `llvm`/`wasm` (which `use` them), and `driver` after `llvm` (which it
imports). It then registers the CLI emitters: `stdlib.native.emit`,
`stdlib.llvm.emit`, `stdlib.llvm.emit_freestanding`, `stdlib.wasm.emit`,
`stdlib.wasm.emit_module`.
