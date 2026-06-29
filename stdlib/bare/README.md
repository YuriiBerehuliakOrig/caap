# `stdlib.bare` — reusable wrappers over the bare-metal primitives

The kernel + LLVM/WASM backends already expose the bare-metal primitives a
freestanding / RTOS target needs — volatile MMIO, raw pointers, inline `asm`,
extern/global (see `docs/design-bare-metal.md`, Table 1). Until now every
bare-metal program (`examples/urun/`, the Phase 0–3 demos) reached those
primitives **directly**, repeating `(volatile_write addr u32 v)` and
`(asm "cpsid i" "" void)` at each use site. `stdlib.bare` factors the common
shapes into small, named, reusable wrappers — the module map planned in
`docs/design-bare-metal.md` §3.5 (lines 149–151), now implemented.

These wrappers are **policy over existing primitives** — zero kernel and zero
backend changes. They add no new IR node type and no new lowering: each wrapper
body is an ordinary `Call` over a primitive the backend already understands.

## How this layer is used (NATIVE/clike-targeted, not eval)

The wrappers lower **only through the native backend**. They are designed to be
pulled into a native program by its `(use …)` dependencies, which the backend
prep path **inlines** into the single translation unit (the same mechanism
`examples/urun` uses for its `ur_*` fragments):

```lisp
; a native program (compiled via tools/s2_emit.caap / s2_build.caap)
(use stdlib.bare.mmio mmio_write32)
(bind UART0_DATA 1073758208)            ; 0x40004000 — CMSDK UART0 TX
(bind main (lambda () (mmio_write32 UART0_DATA 65)))   ; emit 'A'
```

```sh
caap stdlib/bootstrap.caap tools/s2_emit.caap  prog.caap > prog.ll   # LLVM IR
caap stdlib/bootstrap.caap tools/s2_build.caap prog.caap app         # native binary
```

> **Not loadable by the plain eval loader / CTFE.** `volatile_*` and `asm` are
> *native-only* heads — the kernel evaluator and the module loader's semantic
> checker do not know them (they are seeded only into the backend's pre-codegen
> gate, `stdlib/backend/prep.caap`). A bare `(load "stdlib.bare.mmio")` under
> plain bootstrap therefore fails load-time checks **by design**: MMIO and inline
> assembly have no meaning in the CTFE sandbox. Reach these modules through a
> native program's `(use …)`, never via the eval loader.

## Modules

| Module | Wraps | Status |
|--------|-------|--------|
| `stdlib.bare.mmio` | `volatile_read` / `volatile_write` (8/16/32/64-bit) | **fully working** |
| `stdlib.bare.cpu` | `asm` — single CPU instructions (ARM Cortex-M) | **fully working** |
| `stdlib.bare.critical` | the cpu IRQ helpers → enter/leave + save/restore | **fully working** |
| `stdlib.bare.atomic` | `atomic_load` / `atomic_store` / `atomic_add` / `atomic_cas` (32/64-bit) | **fully working** |

### `mmio` — typed register helpers (fully working)

- `mmio_read8/16/32/64 addr` → a volatile load of the named width.
- `mmio_write8/16/32/64 addr val` → a volatile store; returns 0.
- `mmio_set_bits32 addr mask` / `mmio_clear_bits32 addr mask` → a (non-atomic)
  read-modify-write that ORs-in / ANDs-out `mask`. **Guard with a critical
  section if an interrupt may touch the same register.**

A volatile access lowers to LLVM `load/store volatile` — never reordered or
elided — which is the contract a device register needs. The native head and
type vocabulary these wrappers emit (`volatile_*`, `cpsid`/`asm`, the atomics)
is centralized in the module `stdlib.backend.native_meta`.

### `cpu` — single-instruction wrappers (fully working, ARM Cortex-M)

- `nop`, `wfi`, `wfe`, `sev` — pipeline / idle / event instructions.
- `dmb`, `dsb`, `isb` — data/instruction synchronization barriers.
- `irq_disable` / `irq_enable` — `cpsid i` / `cpsie i` (mask / unmask PRIMASK).
- `primask_read` / `primask_write v` — read/write PRIMASK through an `asm`
  **output operand** (`(asm "mrs $0, primask" "=r" u32)`) and input operand;
  both verified to lower cleanly. These back the nesting-safe critical section.

The templates are ARM (thumb) mnemonics — `cpu.caap` is ARM-specific by
construction (as is `examples/urun/ur_port`). A different ISA supplies its own
`cpu.caap`; the *wrapper shape* is portable, the mnemonic is not.

### `critical` — critical sections (fully working)

- `critical_enter` / `critical_leave` — mask, then **unconditionally** re-enable
  IRQs. Correct for a non-nested section (the common driver case). This is the
  exact mechanism `docs/design-bare-metal.md` Phase 3 / `examples/urun` use
  for single-core mutual exclusion against the scheduler.
- `critical_save` / `critical_restore prev` — **nesting-safe**: `critical_save`
  masks and returns the prior PRIMASK; `critical_restore` puts it back exactly
  as it was (so a nested leave does not prematurely re-enable interrupts).

### `atomic` — lock-free atomic words (fully working)

The SMP / multi-core building block (`docs/design-bare-metal.md` §7). A
single-core Cortex-M masks interrupts (`critical`) for mutual exclusion, but a
multi-core / SMP target needs real atomics. Each helper pins one width (32/64):

- `atomic_load32/64 addr` → an atomic (seq_cst) load of the word at `addr`.
- `atomic_store32/64 addr val` → an atomic store; returns 0.
- `atomic_add32/64 addr delta` → atomic fetch-add; returns the value **before**
  the add (the lock-free counter idiom).
- `atomic_cas32/64 addr expected desired` → compare-and-swap; returns the value
  found in memory (`== expected` on success). The lock-free loop compares the
  result to `expected` to decide whether to retry.

These lower to LLVM atomics — `load atomic` / `store atomic` / `atomicrmw add` /
`cmpxchg`, all `seq_cst` (the strongest ordering, the right default for a safe
wrapper). Unlike a `volatile` MMIO access, an atomic access is safe against
concurrent access from another core. The backend `atomic_*` heads are the first
real SMP mechanism (added with these wrappers); they were verified end-to-end
(emit + a clang build that runs with the expected result).

## Design-stage / deliberately out of scope

Nothing in the four modules above is faked — every export lowers and was
verified end-to-end. The remaining `docs/design-bare-metal.md` §3.5 module is
**not** built here because it needs a mechanism that does not exist yet:

- **`stdlib.bare.vectors`** (vector table via a CTFE provider over fixed-address
  placement) — needs section / fixed-address placement, an unimplemented
  backend attribute (`docs/design-bare-metal.md` Table 1, "Section / fixed-addr
  placement"). Today a vector table lives in the ARM `.s` shim
  (`examples/urun/ur_port_cortexm3.s`); pulling it into CAAP needs that
  placement mechanism first.

It is noted, not stubbed — consistent with the no-silent-fallback rule.
