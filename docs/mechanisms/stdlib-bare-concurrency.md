# `stdlib.bare` Concurrency Primitives

**Source:** critical sections + spinlock + SPSC ring in
[stdlib/bare/sync.caap](../../stdlib/bare/sync.caap), lock-free words in
[stdlib/bare/atomic.caap](../../stdlib/bare/atomic.caap), IRQ helpers in
[stdlib/bare/critical.caap](../../stdlib/bare/critical.caap), single-instruction
wrappers in [stdlib/bare/cpu.caap](../../stdlib/bare/cpu.caap), MMIO in
[stdlib/bare/mmio.caap](../../stdlib/bare/mmio.caap). Native head/type vocabulary
in [stdlib/backend/native_meta.caap](../../stdlib/backend/native_meta.caap); the
pre-codegen gate that scopes those heads in
[stdlib/backend/prep.caap](../../stdlib/backend/prep.caap). The tier rank is fixed
by [stdlib/semantics/passes/tiers.caap](../../stdlib/semantics/passes/tiers.caap)
(`bare` = rank 5). Overview in [stdlib/bare/README.md](../../stdlib/bare/README.md)
and the design background in `docs/design-bare-metal.md`.

The `stdlib.bare.*` modules are **reusable policy over the bare-metal
primitives** the LLVM backend already exposes (`volatile_*`, `asm`, `atomic_*`).
They add no kernel node, no new lowering, and no backend change — each wrapper
body is an ordinary `Call` over a primitive the backend understands. This page
documents the concurrency contract those wrappers carry, because two of the
contracts (the normal-return scope of a critical section, and tail-self-call
lowering of the atomic RMW loops) are easy to misread.

## Native-only import rule

The bodies of the bare modules bottom out in heads the **kernel evaluator and the
plain module loader do not know** — `volatile_read`/`volatile_write`, `asm`, and
`atomic_load`/`atomic_store`/`atomic_add`/`atomic_cas`. Those heads are seeded
into the semantic checker's scope **only** in the backend pre-codegen gate
(`gate!` / `native_vocab` in [prep.caap](../../stdlib/backend/prep.caap), derived
from `native_meta.head_names`). The eval loader's checker has no such seed.

Consequently:

- A native program reaches a bare module through its **`(use …)` dependency**,
  which `stdlib.backend.prep` **inlines** into the single translation unit before
  the backend lowers it (the same mechanism `examples/urun` uses). This is the
  supported path. Compile it via [tools/s2_emit.caap](../../tools/s2_emit.caap) /
  [tools/s2_build.caap](../../tools/s2_build.caap).
- Loading a bare module through the **plain eval loader / CTFE fails by design** —
  MMIO, inline assembly, and atomic words have no meaning in the CTFE sandbox. A
  bare `(load "stdlib.bare.mmio")` under plain bootstrap fails the load-time check.

Because the generic failure points *inside* the wrapper at the first unknown
head (not at the import that caused it), a standalone targeted diagnostic exists:
[stdlib/semantics/passes/bare_gate.caap](../../stdlib/semantics/passes/bare_gate.caap)
inspects a unit's raw module-loading directives — `(use …)`, `(import …)`, and
`(re_export …)` (which loads its module like `use`, even with no prior import) —
and reports each `stdlib.bare.*` import as "native-only — reach it only through
the native backend path". It is **opt-in / standalone** — a tool or test calls
`check_forms` /
`check_file` explicitly. It is deliberately **not** wired into the default load
pipeline, because the native backend path legitimately imports bare modules and
auto-firing the gate would reject those valid native programs.

## Two mutual-exclusion mechanisms: single-core vs multi-core

A freestanding target has two distinct exclusion problems, and the bare layer
supplies one mechanism for each. Choosing the wrong one is a correctness bug, not
a performance choice.

| Contender | Mechanism | Module | Why |
|---|---|---|---|
| An interrupt on the **same core** (vs. the scheduler / an ISR) | mask interrupts (critical section) | `stdlib.bare.critical` / `stdlib.bare.sync` `with_critical` | The contender is an ISR; masking PRIMASK makes the section uninterruptible on this core. |
| A holder running on **another core** (SMP) | lock-free atomics / a spinlock | `stdlib.bare.atomic` / `stdlib.bare.sync` `spin_*` | Masking interrupts does nothing to another core; a real atomic / busy-wait is required. |

Mixing them up deadlocks: a **spinlock** does **not** mask interrupts, so spinning
against a same-core ISR that already holds the lock deadlocks (the ISR can never
run to release it) — use a critical section there. Conversely, masking interrupts
gives no mutual exclusion against another core — use atomics there.

`critical_save` / `critical_restore` are the **nesting-safe** form: `critical_save`
masks and returns the prior PRIMASK, `critical_restore` puts it back exactly as it
was (so a nested leave does not prematurely re-enable interrupts). `critical_enter`
/ `critical_leave` are the simpler non-nested form that unconditionally re-enables.

## The normal-return scope of `with_critical`

`with_critical` / `with_critical_arg`
([sync.caap](../../stdlib/bare/sync.caap)) pair the save and the restore once, so
the restore always matches the save **on a normal return** — the common bug of a
hand-written section that forgets the restore on one path is structurally avoided.

The pairing is **NORMAL-RETURN-SCOPED**, not exception-safe:

```text
(defn with_critical ((thunk u32)) i32 (mutation)
  (bind ((prev (critical_save)))                       ; save + mask
    (bind ((result (call_ptr (int_to_ptr thunk u8) i32))) ; run the thunk
      (do
        (critical_restore prev)                        ; restore — only reached
        result))))                                     ;   AFTER a normal return
```

The native backend has **no unwind / finally primitive** — the IR carries no
exception or cleanup edge. `critical_restore` is just the statement of the `do`
that runs *after* the thunk's indirect `call_ptr` returns its value. So a thunk
that **traps or aborts** — a fault, a `wfi` that never wakes, a longjmp-style
escape that does not return through this frame — **skips `critical_restore` and
may leave interrupts masked**. The mitigation is a discipline, not a guard: keep
the thunk total and short, and do any fallible / abortable work *outside* the
critical section.

## Atomic RMW loops rely on tail-self-call lowering

The backend exposes exactly four atomic heads
(`atomic_load`/`atomic_store`/`atomic_add`/`atomic_cas` — see
[native_meta.caap](../../stdlib/backend/native_meta.caap)). The remaining
read-modify-write ops (`atomic_sub`/`and`/`or`/`xor`/`xchg`) are **composed from a
CAS loop** in [atomic.caap](../../stdlib/bare/atomic.caap): snapshot `old`,
compute the desired word, `cmpxchg old -> desired`, and on a lost race **retry**.

The retry is written as a **tail self-call** (it is the function's returned
value), so a backend that rewrites tail self-calls into a loop runs each RMW in
**constant stack regardless of contention**. This is a property the backend *may*
exploit, **not a guarantee**:

- The tail-call pass
  [stdlib/semantics/passes/tailcall.caap](../../stdlib/semantics/passes/tailcall.caap)
  is an **analysis** — it publishes a `tailcall` fact and a TCO-candidate advisory
  finding. It does **not force** the rewrite.
- A backend without TCO would recurse on the C stack, so under pathological
  unbounded contention the loop could grow the stack. The wrappers are written
  tail-recursive so the optimization is *available*, not *mandated*.

## Portable atomics / MMIO vs. ARM-only CPU wrappers (target matrix)

The bare modules differ in how portable they are across ISAs:

| Module | Portability | Why |
|---|---|---|
| `stdlib.bare.atomic` | **portable** across targets the LLVM backend supports | `atomic_*` lower to LLVM `load atomic` / `store atomic` / `atomicrmw add` / `cmpxchg` (`seq_cst`); no ISA mnemonic is written by hand. |
| `stdlib.bare.mmio` | **portable** | `mmio_*` lower to LLVM `load`/`store volatile`; the address is a plain integer `inttoptr`'d to a pointer. |
| `stdlib.bare.cpu` | **ARM Cortex-M only** | The templates are ARM (thumb) mnemonics (`cpsid i`, `wfi`, `dmb`, `mrs … primask`) emitted through `asm`. A different ISA supplies its own `cpu.caap`; the *wrapper shape* is portable, the mnemonic is not. |
| `stdlib.bare.critical` | **ARM-bound via `cpu`** | Built on the `cpu` IRQ / PRIMASK helpers, so it inherits the ARM dependence. |
| `stdlib.bare.sync` | follows its primitives | `with_critical` is ARM-bound through `critical`; `spin_*` and the SPSC ring are portable through `atomic`. |

The width tokens (`u32`/`u64`) name the LLVM scalar to access. Addresses are `u32`
on a 32-bit MCU; widen to `u64` on a 64-bit target — the underlying `atomic_*32` /
`atomic_*64` helpers fix the width. The pointer size the backend uses follows the
active target (`target_pointer_bytes` in [prep.caap](../../stdlib/backend/prep.caap)).
