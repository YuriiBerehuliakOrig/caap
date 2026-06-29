# CAAP Bare-Metal / RTOS Capability — Audit, Delta, Design

Status: Phases 0-3 implemented (UART-over-MMIO, a cooperative context switch, a
**preemptive** round-robin scheduler, and **task synchronization** on Cortex-M,
authored in the `stdlib.frontend.clike` NMV surface, run on QEMU). All four phases
landed with **zero core changes**. After Phase 0's stdlib backend target policy,
the later RTOS phases needed no new backend mechanisms. The original RTOS goal is
met.

This document is the Step-1/Step-2 deliverable: it audits what already exists,
states an honest delta, and proves the *layer* each missing piece belongs to.
The governing rule is the platform principle (`docs/principles.md` #1, #4, #14):
the kernel stays a 3-node IR (`Name`/`Literal`/`Call`) + builtins; every feature
lives above it as builtin+policy lowered in `stdlib.backend.emit.llvm` /
`stdlib.backend.emit.wasm`, an `stdlib` pass/macro, or a surface form in
`stdlib.frontend.clike` / `stdlib.frontend.surface`. LLVM lowering and grammar
live in stdlib layers, never in core.

---

## 0. Invariant rubric (from `docs/principles.md`, not invented)

Every addition below was checked against this rubric:

| # | Principle | Gate question used here |
|---|---|---|
| 1 | Minimal Semantic Kernel | Does it add an IR node type? If expressible as a `Call`, it must be. |
| 2 | Callee-Defined Semantics | Is the meaning attached to a callee name + policy, not a node shape? |
| 3 | Policy-Driven Behavior | Are Eval/Effect/Fold/Phase policies registered, not hard-coded in the evaluator? |
| 4 | Libraries over Features | Can it live in `stdlib` (kit/pass/macro/surface)? Then it must. |
| 6 | Explicit Bootstrap | Is it opt-in via a bootstrap/tool, never auto-loaded? |
| 14 | Core=Substrate, Stdlib=Policy | Does any core adapter start making policy decisions? It must not. |

**Result of the gate:** every bare-metal capability reduces to existing kernel
builtins reached through `Call` nodes. **Core changes required: zero.** The work
is target *policy* in `stdlib.backend.emit.llvm` / `stdlib.backend.driver` and
*spelling* in `stdlib.frontend.clike`.

---

## 1. Audit — Table (1): Primitives

"Already?" = exists today (builtin head + `stdlib.backend.emit.llvm` lowering +
corpus example).
Layer = where any missing piece belongs. Core-change = does it touch crate `caap`?

| Capability | Already? | Lowering / head | Example | If missing → layer | Core change? |
|---|---|---|---|---|---|
| Volatile MMIO | ✅ | `volatile_read addr ty` / `volatile_write addr ty v` → `inttoptr` + `load/store volatile` | `native_mmio.caap` | — | No |
| Raw pointers + deref | ✅ | `ptr_add` (GEP), `ptr_read`/`ptr_write`, `int_to_ptr`/`ptr_to_int`, `ptr_<elem>` types | `native_ptr_asm.caap` | — | No |
| Inline asm | ✅ | `asm "tmpl" "constraints" ty arg…` → `call … asm sideeffect` | `native_ptr_asm.caap` | — | No |
| Extern symbol + ABI | ✅ | `(extern sym (params) ret)` → `declare` + `call @sym` | `native_device.caap`, `native_kernel.caap` | — | No |
| Mutable global | ✅ | `(global name ty init)` → `@name = global …`; reached by `deref`/`set_ref` | `native_kernel.caap` | — | No |
| Global array (BSS) | ✅ | `(global_array name N elem)` → `[N x T] zeroinitializer`, decays to ptr | (backend) | — | No |
| Function pointers / indirect call | ✅ | `fn_ptr name`, `call_ptr fp ret arg…` | `native_vtable.caap` | — | No |
| Structs / unions / fields | ✅ | named aggregates; `get`/`field_ptr`/`union_field` | `native_struct/union.caap` | — | No |
| Width casts | ✅ | `cast v ty` → trunc/zext/sext/fpext/…/sitofp | `native_cast.caap` | — | No |
| Freestanding emit (no `main`) | ✅ | `emit_freestanding` (`stdlib.backend.emit.llvm`) — no exit-code contract | (backend) | — | No |
| Freestanding **build** (clang) | ✅ | `stdlib.backend.driver.compile_freestanding` cross-links via clang for the active target triple — verified by the bare-metal + URun acceptance suite (clang + qemu) | (backend) | — | No |
| **Target triple + datalayout** | ✅ | `stdlib.backend.emit.llvm.set_target!` stamps `target triple` + `datalayout` (e.g. `thumbv7m-none-eabi`); host default when unset | (backend) | — | No |
| Section / fixed-addr placement | ❌ (not needed for P0) | — | — | **stdlib.backend.emit.llvm** attribute (one mechanism) | No |
| Naked / no-prologue fn | ❌ (not needed for P0) | — | — | **stdlib.backend.emit.llvm** fn-attribute (DESIGN-STAGE) | No |
| Atomics | ❌ (not needed for P0) | — | — | **stdlib.backend.emit.llvm** `atomicrmw`/`cmpxchg`/`fence`, or asm | No |
| pack / align | ❌ (not needed for P0) | — | — | **stdlib.backend.emit.llvm** type policy | No |
| IRQ enable/disable | ❌ (by design) | — | — | **stdlib** over `asm` (`cpsie/cpsid`) | No |
| Memory barriers | ❌ (by design) | — | — | **stdlib** over `asm` (`dmb/dsb/isb`) | No |

### Honest delta for Phase 0

Of everything above, **Phase 0 (UART over MMIO on QEMU)** required only these
additions, now implemented:

1. **Target triple + datalayout** in `stdlib.backend.emit.llvm` — the one real
   target-policy gap. Without it the IR compiles for the host (x86-64), not
   Cortex-M.
2. **A freestanding cross build** in `stdlib.backend.driver` — cross `clang -target …
   -ffreestanding -nostdlib`, assemble an ARM asm shim, link with a custom
   `.ld`, **no** host runtime.
3. The **surface vocabulary** for `extern`/`global` in
   `stdlib.frontend.clike` (Table 2).

Placement / naked / atomics / barriers / IRQ are **later-phase** concerns and
are intentionally *not* built in this pass (rule: nothing wired silently).

---

## 2. Audit — Table (2): Surface (`stdlib.frontend.clike` NMV)

The clike lowerer already covers functions, `let`/decl (`name type = expr`),
assignment, `while`+`break`, `if`/`else`, calls, infix, struct decls/literals,
string interpolation, and the `(surface …)` header. The system **calls** lower
for free, because an undeclared type identifier (e.g. `u32`, `void`, `ptr_u64`)
lowers to a bare `Name` — exactly the bare type token the backend wants.

| Primitive | clike lowers today? | NMV form | Lowers to | Where (stdlib layer) |
|---|---|---|---|---|
| `volatile_read/write` | ✅ (generic call) | `volatile_write(addr, u32, v)` | `(volatile_write addr u32 v)` | none (type arg → `Name`) |
| `ptr_*`, `int_to_ptr`, `cast` | ✅ (generic call) | `ptr_add(p, i)`, `cast(x, u32)` | `(ptr_add p i)` … | none |
| `asm` | ✅ (generic call) | `asm("wfi", "", void)` | `(asm "wfi" "" void)` | none |
| **extern** (foreign decl) | ✅ | `halt () i32` *(bodyless — no `= {…}`)* | `(extern halt () i32)` | **stdlib.frontend.clike** top-level bodyless-fn rule |
| **global** (mutable) | ✅ | `ticks u32 = 0` *(top level)*; read/write by name | `(global ticks u32 0)`; `ticks`→`(deref ticks)`, `ticks = x`→`(set_ref ticks x)` | **stdlib.frontend.clike** top-level NMV-with-value rule + name read/write |

So the only two genuine surface additions were the **two declaration forms**:
a *bodyless* NMV at top level → `extern`, and a *valued* NMV at top level →
`global`. Both reuse the registry-checked meta position (a system type must be
known to `types.registry`; `u8…u64`, `ptr_<scalar>` already are).

---

## 3. Design (Step 2)

### 3.1 Core (crate `caap`)
**Nothing.** The gate found no semantics that cannot be expressed as a `Call`
over existing builtins. Target selection is codegen *policy*, not kernel
semantics.

### 3.2 `stdlib.backend.emit.llvm` — target triple + datalayout (opt-in policy)
- A module-level `current_target` ref + `set_target!(triple, datalayout)` /
  `clear_target!()` (exported). `emit_core` prepends
  `target datalayout = "…"` and `target triple = "…"` **only when set**.
- Default unset ⇒ identical output to today (host default) — backward
  compatible; every existing native test is unaffected.
- Rationale vs. principles: emission is `stdlib.backend.emit.llvm`'s job
  (#4/#14); the policy is
  data set by a tool/bootstrap, never a CLI flag (#7) and never in core (#1).

### 3.3 `stdlib.backend.driver` — `compile_freestanding`
- New entry: lower a (surface or kernel) source unit through
  `emit_freestanding`, write `.ll`, then:
  `clang -target <triple> -mcpu=<cpu> -ffreestanding -nostdlib -c prog.ll`,
  `clang -target <triple> -mcpu=<cpu> -c shim.s`,
  `clang -target <triple> -mcpu=<cpu> -nostdlib -ffreestanding -T <ld> prog.o shim.o -o out.elf`.
- **No** `caap-sys-runtime` link, **no** platform libs (those are the hosted
  path). Failures are DATA (`{ok:false, error}`); temporaries cleaned.
- A `targets` table maps a friendly name (`cortex-m3`) → `{triple, cpu,
  datalayout}` — the single source of target truth, consumed by the tool.

### 3.4 `stdlib.frontend.clike` — system NMV vocabulary
- **Top-level bodyless fn** `name (params) ret` (no `=`): emit
  `(extern name (params) ret)`. One mechanism — it reuses the existing param
  parser; the *absence* of `= {body}` is the discriminator (NMV `value?`-absent).
- **Top-level valued NMV** `name type = expr` where the value is **not** a
  `{brace}` (that is the struct-decl form): emit `(global name type expr)`. The
  meta is registry-checked, same as a local decl. A function body then reaches
  the global **by name** — a use lowers to `(deref name)`, an assignment to
  `(set_ref name …)` — so scheduler/shared state needs no raw-address arithmetic
  (the bare-metal examples were refactored to named globals once this landed).
- **Global array** `name elem = array(N)` → `(global_array name N elem)`: a
  zero-init BSS buffer whose name decays to a typed pointer (a use stays a bare
  name, not deref'd, so `ptr_add`/`ptr_read`/`ptr_write` index it). Lets task
  stacks be named, not raw addresses.
- MMIO/ptr/asm stay generic calls — documented, no code.

### 3.5 `stdlib` wrappers over primitives
Phase 0 needs none beyond the primitives. The later-phase module map
(planned, not built):
- `stdlib/bare/mmio.caap` — typed register helpers over `volatile_*`+`ptr_*`.
- `stdlib/bare/cpu.caap` — `irq_enable/irq_disable/wfi/dsb/dmb/isb` over `asm`.
- `stdlib/bare/critical.caap` — `critical_section` over the irq helpers.
- `stdlib/bare/atomic.caap` — atomic word/lock over the atomic mechanism.
- `stdlib/bare/vectors.caap` — vector table via CTFE provider over placement.

### 3.6 Tool + target selection (flagless CLI)
- `tools/s2_bare.caap` (composed bootstrap = `bootstrap.caap` +
  `boot/native_emit.caap`): args = `SOURCE OUTPUT [TARGET SHIM LD]`. It looks up
  the target in `stdlib.backend.driver.targets`, calls `set_target!`, then
  `compile_freestanding`. Target choice is bootstrap/tool policy, **not** a CLI
  flag (#7).

---

## 4. Phase 0 — acceptance criterion

A CAAP program **authored in the `(surface stdlib.frontend.clike)` NMV surface**
that, on QEMU `mps2-an385` (Cortex-M3), writes to the CMSDK UART via volatile
MMIO, links its **own** `.ld`, with **one** ARM asm shim reached via `extern`
(bodyless-NMV), **without** the host runtime (`caap-sys-runtime-ffi` not linked).

Assets: `examples/urun/` (`ur_build.caap`, `ur_port_cortexm3.s`, `ur.ld`, and
the clike kernel modules). Tests in `caap/tests/stdlib_codegen_tests.rs` build
the thumbv7m ELF, verify it statically (ARM machine, vector table at `0x0`), and
run the UART assertion when `qemu-system-arm` is present. The workspace CI
installs `clang`, `lld`, and `qemu-system-arm` for this acceptance path.

---

## 5. Phase 1 — context switch (IMPLEMENTED)

A **cooperative** context switch, authored in the clike NMV surface
(`examples/bare_ctx.caap` + `bare_ctx.s`), verified on QEMU (UART shows
`MABABAB…` — the switch goes both directions). Key conformance result:

- The **one irreducible primitive** — saving/restoring `r4–r11` + the stack
  pointer — lives in the asm shim as a plain AAPCS routine `ctx_switch`, reached
  from CAAP via an `extern` bodyless-NMV. **No naked function and no new backend
  change were needed**: a cooperative switch is an ordinary call, not an
  exception handler, so the existing `extern`-asm mechanism (Phase 0) suffices.
- Everything else is CAAP **policy** over the Step-1 primitives: crafting each
  task's initial stack frame (`int_to_ptr`/`ptr_write` — write the entry as the
  saved `lr`, thumb bit forced), holding the saved stack pointers in raw RAM,
  and the yield/ping-pong. Function addresses come from `fn_ptr`+`ptr_to_int`.
- Footgun found + documented: a CAAP value named `entry` collides with the
  emitter's `entry:` block label (LLVM shares the value/label namespace) — avoid
  it in native code (the example renamed its param).

## 6. Phase 2 — preemptive scheduler (IMPLEMENTED)

A **preemptive** round-robin scheduler, authored in the clike NMV surface
(`examples/bare_pre.caap` + `bare_pre.s`), verified on QEMU. The two tasks are
busy loops that **never yield**; a `SysTick` timer drives the switch, so the UART
shows long timer-sliced runs — `MAAA…BBB…AAA…` (collapsed: `MABABAB…`).

Conformance result — **again no kernel and no new backend change**, including
*no naked-function attribute*. The DESIGN-STAGE expectation was that a preemptive
handler would force naked functions into `stdlib.backend.emit.llvm`; it did not, because the
exception machinery stays in the **vector-table asm shim** (reached by hardware,
exactly as `ctx_switch` was reached by `extern`):

- asm shim: a full Cortex-M vector table; `SysTick_Handler` (pends PendSV via
  ICSR); `PendSV_Handler` (saves `r4–r11` below PSP, calls the CAAP scheduler,
  restores, `EXC_RETURN`); `start_first_task` (launches task 0 on the PSP).
  `pick_next` is an ordinary AAPCS callback — `lr`/EXC_RETURN is preserved across
  it. `Fault_Handler` emits `F` to the UART for diagnosis.
- CAAP policy (clike NMV): `init_task` crafts a 16-word exception frame (`pc`,
  `xPSR.T`) per task; `pick_next` round-robins by storing the preempted PSP and
  returning the other task's saved PSP; `c_entry` configures `SysTick`
  (RVR/CVR/CSR) and PendSV priority (SHPR3, lowest) over MMIO, then launches.
- Saved PSPs + current-task id live in raw RAM — the same idiom as Phase 1.

> If one later *wanted* the PendSV/SysTick handlers themselves authored in CAAP
> rather than asm, *that* is what would require a backend naked-function
> attribute — still a clean, justified, future `stdlib.backend.emit.llvm`
> addition, never core.

## 7. Phase 3 — synchronization (IMPLEMENTED)

Task synchronization on the preemptive scheduler, authored in the clike NMV
surface (`examples/bare_sync.caap`, reusing the Phase-2 `bare_pre.s` shim),
verified on QEMU. Two tasks each do 100 increments of a shared counter through a
deliberately **wide** read-modify-write (a delay loop) wrapped in a critical
section; with the lock all 200 survive → the program prints `MY`, while the same
workload without the lock loses updates → `MN`. So `MY` is a *real* proof the
critical section synchronizes, not a vacuous pass.

Conformance result — the DESIGN-STAGE expectation was that synchronization would
need the first genuinely new backend mechanism (an `ldrex/strex` atomic). It
did **not**: on **single-core** Cortex-M, mutual exclusion against the scheduler
is just **interrupt masking** — `cpsid i` / `cpsie i`, expressed with the
existing `asm` primitive (`lock`/`unlock` are two-line CAAP functions). So Phase
3 also lands with **zero core and zero new backend changes**.

This demo uses **named state throughout** — no raw addresses remain (except the
genuine MMIO register addresses). The scalars are module globals (`current`,
`saved0/1`, `counter`, `done_b`) reached by name, and each task stack is a
**named global array** (`stack_a u32 = array(256)` → `(global_array …)`, the name
decaying to a typed pointer). Both spellings (global read/write and
`global_array`) were added to `stdlib.frontend.clike` in the same pass (Table 2); the
earlier phases' raw-RAM idiom was a workaround for that gap. `pick_next`, the
shared counter, and stack setup now read like ordinary code.

> The `ldrex/strex` atomic (lowered to `atomicrmw`/`cmpxchg`) remains genuinely
> deferred — it is needed only for **lock-free / multi-core (SMP)** primitives,
> which the single-core mps2-an385 does not require. Adding it would be the first
> real `stdlib.backend.emit.llvm` mechanism for a future SMP target, with explicit
> justification, and still never in core.

## 8. Outcome

The original goal — drive CAAP's bare-metal/RTOS path to a working Cortex-M RTOS
under QEMU without violating any CAAP principle or bloating the kernel — is met
across Phases 0–3. The final tally:

- **core (crate caap): 0 changes.** Every primitive reduces to `Call` nodes over
  existing builtins.
- **stdlib.backend.emit.llvm: 1 change total** — the opt-in target triple/datalayout policy
  (Phase 0). No naked functions, no placement, no atomics were needed.
- **stdlib.backend.driver: freestanding cross link** (Phase 0).
- **stdlib.frontend.clike: `extern` + `global` NMV spellings** (Phase 0).
- **Everything else is CAAP policy** in the clike NMV surface + a minimal ARM asm
  shim reached by hardware/`extern`: MMIO drivers, context switch, preemptive
  scheduler, critical-section synchronization.
