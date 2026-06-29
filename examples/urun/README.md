# URun "common" — vertical slice on the CAAP clike (NMV) surface

A faithful re-implementation of the **portable URun kernel's core model** —
a **priority-preemptive scheduler**, **thread management** (create / suspend /
resume / terminate / delete / relinquish / priority-change / sleep /
wait-abort), a **time base + application timers**, a **counting semaphore**,
a **message queue**, a **mutex with priority inheritance**, **event flags**, and
a **fixed block pool** — authored entirely in CAAP's C-like (NMV) surface
(`stdlib.frontend.clike`) and cross-compiled **freestanding** to ARM Cortex-M3,
verified on QEMU `mps2-an385`.

Both forms of URun preemption are present: **event-driven** (a blocking /
posting service pends PendSV after readying a higher-priority thread) and
**time-driven** (SysTick advances the tick base, wakes sleeping threads, expires
timed waits, and fires application timers). Blocking services take a real
URun **wait option** (`UR_NO_WAIT` / a tick timeout / `UR_WAIT_FOREVER`).

Control blocks are real **structs** (`UR_THREAD` / `UR_SEMAPHORE` / `UR_QUEUE` /
`UR_TIMER`) in static arrays, wired together by **pointer-linked intrusive
lists** — the same shape URun uses in C. Each concern is a real **stdlib
module** with explicit `use` imports/exports. The only irreducible assembly is
the CPU port (vector table, the PendSV context switch, the SysTick tick entry,
the first-thread launch); the entire kernel *policy* is CAAP.

## Project layout (URun-style, one module per concern)

| File | Module | URun analogue | Contents |
|---|---|---|---|
| `ur_port.caap` | `…urun.ur_port` | `ports/cortex_m3/*` (C side) | critical section (`cpsid`/`cpsie`, `~{memory}`-clobbered), pend-switch, `wfi` idle, SysTick config, UART, exception-frame builder (with a thread-exit trampoline) |
| `ur_status.caap` | `…urun.ur_status` | the `UR_*` status macros in `ur_api.h` | the `UR_STATUS` completion-code enum (`UR_SUCCESS` / `UR_NO_INSTANCE` / `UR_QUEUE_FULL` / …) and the wait options (`UR_NO_WAIT` / `UR_WAIT_FOREVER`) |
| `ur_list.caap` | `…urun.ur_list` | the intrusive `ListItem_t` / `List_t` pair | the generic intrusive container — `Node<T>` (the per-element hook) and `List<T, lf>` (the head/tail anchor, O(1) FIFO push), monomorphized per (element type, link field). Every waiter / ready / delayed / timer list is an instance |
| `ur_scheduler.caap` | `…urun.ur_scheduler` | the scheduler/ready-list half of `ur_thread_*.c` | the `UR_THREAD` control block + all scheduler state (running thread, priority ready list, timed-suspension delayed list), the intrusive list primitives, the low-level state-transition engine (`_ur_thread_resume` / `_ur_thread_suspend_*` / `_ur_thread_timer_tick` / `_ur_list_prioritize`), the PendSV dispatch (`_ur_thread_schedule`) and first-task launch (`ur_thread_first_psp`) |
| `ur_thread.caap` | `…urun.ur_thread` | the service half of `ur_thread_*.c` | the thread API over `ur_scheduler`: `create` / `suspend` / `resume` / `terminate` / `delete` / `relinquish` / `priority_change` / `sleep` / `wait_abort` / state+priority accessors (each masking interrupts around the scheduler engine) |
| `ur_timer.caap` | `…urun.ur_timer` | `ur_timer.h` + `ur_timer_*.c` + `ur_timer_interrupt` | the kernel time base (`ur_time_get`/`_set`), the `UR_TIMER` control block, `ur_timer_create` / `_activate` / `_deactivate` / `_delete`, and `_ur_timer_interrupt` (the SysTick callback driving sleeps, timeouts and timers) |
| `ur_semaphore.caap` | `…urun.ur_semaphore` | `ur_semaphore.h` + `ur_semaphore_*.c` | the `UR_SEMAPHORE` control block, `ur_semaphore_create` / `_get` (wait option) / `_put` / `_ceiling_put` / `_prioritize` / `_delete` |
| `ur_queue.caap` | `…urun.ur_queue` | `ur_queue.h` + `ur_queue_*.c` | the `UR_QUEUE` control block (configurable message word-size), `ur_queue_create` / `_send` / `_front_send` / `_receive` (all with a wait option) / `_flush` / `_prioritize` / `_delete` |
| `ur_mutex.caap` | `…urun.ur_mutex` | `ur_mutex.h` + `ur_mutex_*.c` | the `UR_MUTEX` control block + `ur_mutex_create` / `_get` (wait option) / `_put`, with **priority inheritance** — a blocked high-priority caller boosts the lower-priority owner so it releases promptly (bounding inversion), restored on release |
| `ur_event_flags.caap` | `…urun.ur_event_flags` | `ur_event_flags_*` | the `UR_EVENT_FLAGS` control block + `create` / `set` / `get`, with OR/AND wait modes and optional bit consumption |
| `ur_block_pool.caap` | `…urun.ur_block_pool` | `ur_block_pool_*` | a fixed-block heapless allocator with interrupt-masked create/alloc/free and address/double-free validation |
| `sample_urun.caap` | *(the program)* | `sample_urun.c` + `ur_initialize_*.c` | control-block storage, the demo threads (+ an idle thread + an application timer), `ur_application_define`, `c_entry` (board + PendSV/SysTick bring-up, then launch) |
| `ur_port_cortexm3.s` | — | `ports/cortex_m3/.../ur_thread_context_switch.s` etc. | the irreducible asm: vector table, PendSV, SysTick, `start_first_task` |
| `ur.ld` | — | the board linker script | FLASH @ 0x0, RAM @ 0x20000000 |
| `ur_build.caap` | — | the Makefile / build system | pre-lowers + declares the modules, then builds |

Each object's control block lives **in its own module** (no separate header
module) — `UR_TIMER` in `ur_timer`, `UR_SEMAPHORE` in `ur_semaphore`, `UR_QUEUE`
in `ur_queue`; `UR_THREAD` lives with the scheduler that links it, in
`ur_scheduler`. A named module (`(surface stdlib.frontend.clike
stdlib.urun.ur_semaphore)`) `use`s the modules it depends on:

```
use stdlib.urun.ur_scheduler (UR_THREAD, is_null, _ur_thread_resume, …)
use stdlib.urun.ur_port       (ur_disable, ur_restore, ur_pend_switch)
```

**Visibility — `pub`.** A declaration is module-PRIVATE by default; prefix it
`pub` to export it. Only `pub` functions and struct types are exported and
importable via `use`; importing a private name is rejected by the loader
(*"module X imports Y, which does not export it"*). So `ur_thread` exports its
API (`UR_THREAD`, `ur_thread_create`, `_ur_thread_resume`, …) but keeps the
ready-list helpers (`_ur_ready_insert`/`_ur_ready_remove`) and the scheduler
globals private. clike's mutable-state tracking is per-module, so the running
thread is reached through the `ur_thread_current()` accessor, not a shared
global — cleaner than URun's bare `_ur_thread_current_ptr`.

## Static safety gate (the compiler enforces the kernel's rules)

A distinguishing feature of this slice: **the build runs static-analysis passes
that reject code violating the kernel's safety invariants.** Each pass is ordinary
CAAP (in `stdlib/semantics/passes/ur_*.caap`), registered in `ur_build.caap`, and
run at the native codegen gate — so a violation aborts the build with a *located*
diagnostic and never reaches the linker. Seven passes:

| Pass | Rejects |
|---|---|
| `ur-norec` | a (transitively) recursive function — unbounded stack growth on a fixed thread stack |
| `ur-crit` | an `ur_interrupt_save()` not matched by `ur_interrupt_restore()` on every path — interrupts stuck masked |
| `ur-isr` | a blocking API reachable from an `*_interrupt` handler — an ISR has no thread to suspend |
| `ur-intrusive` | the same node pushed onto the same list twice with no intervening remove — double insertion corrupts both lists |
| `ur-create` | a second `ur_*_create` on a live control block — re-initialising in-flight state |
| `ur-suspend` | a park (`_ur_thread_suspend_timed`) with no reachable waiter enqueue — a lost-wakeup deadlock |
| `ur-fastpath` | a suspend at conditional-depth 0 — a thread that blocks even when the resource is free (self-deadlock) |

Each pass has a negative-fixture test (`tests/urun_probe_*.caap`, asserted in
`caap/tests/stdlib_codegen_tests.rs`) proving it FIRES, and the clean `urun` build
proves none false-positive. This is the platform thesis in miniature: the
*compiler* is extended, in stdlib, to understand this kernel's rules — the ISR-safe
callback contract below is **enforced**, not just documented.

## Robustness & footprint

- **Optimised build.** The freestanding image is compiled `-Os`; the interrupt
  barriers carry a `~{memory}` clobber so the optimiser can't move kernel state
  across a critical section.
- **Diagnosable halts.** An empty ready list (a missing idle thread) emits
  `0xDEADBEEF` over UART before spinning; a clobbered **stack canary** — seeded at
  each stack's low end, checked on every context switch — emits `0xBAADF00D`. A
  serial log thus tells a config error, a stack overflow, and a true hang apart.
- **Lifecycle.** A thread that returns from its entry lands on a trampoline that
  marks it `UR_COMPLETED` and yields, instead of faulting on `bx 0`.
- **Power.** The idle thread sleeps on `wfi` rather than hot-spinning.
- **Mutual exclusion.** `ur_mutex` adds priority inheritance — the one inversion
  cure the ownerless semaphore cannot provide.

**How it builds.** The clike surface lowers one file at a time and the loader
parses a `use` dependency as kernel source, so `ur_build.caap` pre-lowers each
clike module to kernel source and `declare`s it (module name → generated file),
then lowers the entry program and emits it **freestanding**. At emit time the
loader resolves the module graph through the `use` directives, shares the struct
types, and the native backend **inlines** every module into one Cortex-M3 image
— exactly how a C toolchain compiles each `.c` and links one binary.

## Build & run

From the repository root. The bare `stdlib/bootstrap.caap` is enough —
`ur_build` `load_module`s the codegen and the clike surface by name on demand,
so no `native_emit` / `compose_native` leg is needed (composing it still works):

```bash
caap stdlib/bootstrap.caap examples/urun/ur_build.caap \
     examples/urun /tmp/urun.elf cortex-m3

qemu-system-arm -M mps2-an385 -cpu cortex-m3 -nographic -kernel /tmp/urun.elf
```

Expected UART output: **`MABCPDT`** — `M` (kernel entered); `A`,`B`,`C` (the
high-priority consumer woken from its semaphore/queue block by each of the
low-priority producer's posts, preempting in to receive and print the byte);
`P` (a one-shot application timer firing on the first tick); `D` (the producer
sleeping three ticks, then waking); `T` (the consumer's 5-tick timed semaphore
get expiring with `UR_NO_INSTANCE`). So the single run exercises priority
preemption, the semaphore, the queue, application timers, `ur_thread_sleep`, and
timed waits.

A pinned acceptance test is `stdlib_urun_slice_phase_qemu` in
`caap/tests/stdlib_codegen_tests.rs` (run with `--ignored`; needs `clang` +
`ld.lld`, optionally `qemu-system-arm`).

## Debugging the compiled ELF on QEMU (gdb)

The default URun debug flow emits an ELF that is **not stripped** (it keeps every
FUNC symbol — `_ur_thread_schedule`, `ur_semaphore_get`, `ur_queue_send`, …) but
does not enable **DWARF** line info. So debugging is **symbol/instruction level**:
break on a symbol, step ARM instructions, inspect registers and memory. CAAP
*source-line* stepping of the compiled binary requires threading the backend's
DWARF mode through this freestanding build flow; the compile-time `caap-dap`
debugger is a separate thing — it steps CTFE, not the binary.

**Requirements:** the [Cortex-Debug](https://marketplace.visualstudio.com/items?itemName=marus25.cortex-debug)
VS Code extension (recommended in `.vscode/extensions.json`) and an
ARM-capable gdb on `PATH`:

```bash
sudo apt-get install gdb-multiarch        # Debian/Ubuntu/WSL
# or use arm-none-eabi-gdb and set "gdbPath" accordingly in launch.json
```

**In VS Code** — pick a config from the Run and Debug panel:

- **URun on QEMU (attach, cortex-debug)** — the pre-launch task builds the
  ELF and starts QEMU halted at reset (`-S`) on `tcp::1234`, then gdb attaches.
  By default it breaks at `_ur_thread_schedule` and continues; edit
  `postAttachCommands` in `.vscode/launch.json` to stop elsewhere (or at reset).
  Terminate the *"urun: QEMU (debug)"* task when you're done.
- **URun on QEMU (launch, cortex-debug manages QEMU)** — one click;
  Cortex-Debug spawns and owns QEMU itself (no lingering task).

**From a bare terminal** (no editor) the same thing by hand:

```bash
# 1. build (or run the "urun: build ELF" task)
cargo run -p caap-cli -- stdlib/bootstrap.caap examples/urun/ur_build.caap \
     examples/urun "$PWD/target/urun.elf" cortex-m3

# 2. QEMU, halted at reset, gdbstub on :1234
qemu-system-arm -M mps2-an385 -cpu cortex-m3 -nographic \
     -kernel "$PWD/target/urun.elf" -S -gdb tcp::1234

# 3. in another shell
gdb-multiarch "$PWD/target/urun.elf" \
     -ex 'target remote :1234' \
     -ex 'break _ur_thread_schedule' -ex 'continue'
```

## Surface features

The kernel is written with the clike (NMV) surface's struct/pointer support:

- **Struct declarations** with the `struct` keyword — `UR_THREAD struct = { … }`
  (the older `type` keyword still works).
- **Field access through a pointer** with the C arrow `->` — control blocks are
  always reached through a pointer, so the code reads like URun C:
  `thread_ptr->ur_thread_state = 0`, `p->ur_thread_ready_next->ur_thread_priority`.
  Both reads (`p->f`, lowering to `ptr_read(field_ptr(p,"f"))`) and writes
  (`p->f = v`, lowering to `ptr_write(field_ptr(p,"f"), v)`) are supported, as
  are chains. The dot `.` accesses a field of a struct **value** (not a pointer).
- **C-style postfix pointer/array types** — modifiers read left to right:
  - `T*` — a pointer (`ur_thread_ready_next UR_THREAD*`, `base u32*`).
  - `T[5]` — an array of 5 (`ur_thread_pool UR_THREAD[4]`, `stack0 u32[256]`);
    a variable so declared is a zero-init buffer whose name decays to a pointer.
  - `T*[5]` — an array of 5 pointers.
  - `T[5]*` — a pointer to an array (decays to a pointer to the element, since
    the backend's pointers are to elements).
- **`*p` / `&name`** value operators: `*p` dereferences (reads through a
  pointer), `&name` takes a name's address. Pointer field *writes* use `->`;
  raw stores still use `ptr_write`.
- **`mut` — variables are const by default.** A plain `x u32 = 0` is immutable
  (a value binding); reassigning it is a compile error. Add `mut` to make it a
  mutable cell: `i mut u32 = 0`, `ur_ready_head mut UR_THREAD* = 0`. Function
  parameters are always immutable. (Global arrays are const handles whose
  *contents* still mutate through pointers, so they need no `mut`.)
- **`pub` — declarations are module-private by default.** In a named module
  only `pub` functions/structs are exported and importable via `use`; the rest
  are internal and the loader rejects any attempt to import them.
- **`enum` — named integer constants under a type.** `UR_STATE enum = { UR_READY,
  UR_COMPLETED, … }` (auto-incremented, or `= N` for an explicit value) replaces
  magic 0/1/2/3 for thread states; the shared `ur_status` module exports the
  service codes (`UR_SUCCESS`, `UR_QUEUE_EMPTY`, `UR_NO_INSTANCE`, …) so every
  return and comparison reads by name. A variant is usable as a value anywhere
  and the enum names a type. The loader evaluates it (a compile-time value) and
  the backend compiles the same value, so there is no runtime cost.
- **`match` — multi-way dispatch.** `match r { UR_NO_INSTANCE => { … }, _ => { … } }`
  lowers to right-folded typed `if (eq r PAT)` arms over a required `_` default.
- **`const` — COMPILE-TIME constants (CTFE).** `XPSR_THUMB u32 const = 1 << 24`
  folds to the literal `16777216` at load time — no runtime arithmetic — and each
  use inlines the immediate (the Cortex-M register addresses, PendSV/SysTick bit
  masks and the xPSR thumb bit in `ur_port` are all named consts). The fold is
  **effect-guarded**: an impure expression is refused, so a side-effecting write
  can never be silently constant-folded. MMIO addresses use `const volatile`, for
  example `ICSR u32 const volatile = 0xE000ED04`; assigning `ICSR = PENDSVSET`
  lowers to `volatile_write` while `PENDSVSET` stays a normal folded bit mask. In
  emitted native IR, `ur_pend_switch` still becomes a `store volatile i32
  268435456`.

## Scope

This covers URun's portable **core**: the priority-preemptive scheduler, the
full thread-management API (create / suspend / resume / terminate / delete /
relinquish / priority-change / sleep / wait-abort / identify), the kernel time
base with application timers, semaphore and queue objects with their complete
service sets, a mutex with priority inheritance, event flags, and a fixed block
pool. Blocking services honour URun wait options (`UR_NO_WAIT` / tick timeout /
`UR_WAIT_FOREVER`).

Still **out of scope**: byte pools, full `*_info_get` / `*_performance_*`
introspection across every object, event-trace, MISRA conformance, recursive
mutexes, and exact priority-inheritance recomputation when a boosted wait times
out. Each missing object still drops in as a sibling `ur_*.caap` module following
the same pattern — control-block struct in its own module, a new `(surface …
stdlib.urun.ur_X)` that `use`s what it needs, registered in `ur_build`'s module
list.

## Concurrency & ISR safety

Single-core mutual exclusion is interrupt masking (PRIMASK). Two critical-section
APIs live in `ur_port`:

- `ur_disable()` / `ur_restore()` unconditionally mask / **unmask**. They are
  correct only at the **outermost** level (interrupts already enabled) — the
  demo's top-level use.
- `ur_interrupt_save()` / `ur_interrupt_restore(prev)` read PRIMASK, mask, then
  **restore the prior state** (same `mrs/msr primask` mechanism as
  `stdlib.bare.cpu`). These are **nesting-safe**: correct when entered with
  interrupts already masked — a nested critical section, or code reachable from
  an ISR. The internal kernel services (queue, semaphore, timer, thread) were
  migrated to this save/restore pair and the result is QEMU-validated — e.g. the
  SysTick path that re-enters `ur_timer_deactivate` no longer unmasks mid-ISR.
  (The queue also keeps SEPARATE sender / receiver suspension lists, so a wake
  never reaches the wrong waiter kind.)

**Timer callbacks run in interrupt context.** `ur_timer_create`'s `expiry`
callback is invoked from `_ur_timer_interrupt` (SysTick) during the timer-list
traversal, so a callback MUST be ISR-safe: no blocking API, no `ur_timer_*` list
mutation, keep it short (set a flag / signal a thread). For arbitrary work, post
to a service thread. The sample callback only writes UART, which is safe. The
no-blocking-from-an-`*_interrupt`-handler half of this contract is enforced at
build time by the **`ur-isr`** pass (see *Static safety gate* above); the
no-list-mutation-from-a-`call_ptr`-callback half is left to discipline (it needs
call-pointer-target data-flow the current passes don't do).
