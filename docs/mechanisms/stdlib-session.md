# The unified compile-session context (`stdlib.semantics.session`)

> Audit item #1 ("Session-global mutable state is too scattered"), round-3 Wave B.

## The problem

Compile-time mutable state used to be **scattered** across the stdlib, each
subsystem owning its own cells with its own ad-hoc reset:

| subsystem | state | old reset |
|---|---|---|
| `backend/prep` | `target_ptr_bytes`, `strict_mode` | `clear_target_ptr_bytes!`, `clear_strict!` |
| `semantics/passes/registry` | `registry`, `transforms`, `facts`, `schemas` | `clear_passes!` |
| `frontend/clike/state` | the per-file record | `reset_file_state!` |
| `semantics/types/infer` | `signatures`, harvest/counters | (per-module, internal) |
| `backend/emit/llvm/debug` | `current_target` | `clear_target!` |

Nothing knew "what is the live session state" or could "reset the session for a
fresh compile". A tool wanting a clean slate had to know — and call — every
scattered clear by hand; a new stateful subsystem was invisible to the others.

## The contract

`stdlib/semantics/session.caap` is **one** session context that:

1. **Owns** new mutable cells on behalf of subsystems —
   `session_cell!(name, init_thunk)` returns a stable ref, created once. A
   module-level `(bind x (session_cell! "…" (lambda () init)))` reads/writes that
   ref exactly as before; `session_reset!` mutates the *same* ref in place (never
   replaces it), so the binding stays valid.
2. **Aggregates** every subsystem's reset under one named registry —
   `session_register!(name, reset_thunk)` (idempotent: re-registering a name
   replaces its reset). For in-place containers (the pass fact store) the
   subsystem's existing `clear_*!` becomes the reset thunk.
3. Exposes a live **inventory** — `session_subsystems()` and
   `session_cell_names()` answer "what holds session state?", which nothing could
   before.
4. Resets the whole session with one call — `session_reset!()` re-inits every
   owned cell, then runs every registered reset.

It is loaded **eagerly** in `bootstrap.caap` (before the type layer), depends on
**kernel builtins only**, and lives at the `semantics` tier (rank 4) so the
`semantics` (same tier) and `backend`/`frontend` (higher) subsystems can attach.

## What is wired today (round-3 Wave B #1)

- `backend/prep` — `target_ptr_bytes` / `strict_mode` are now **session-owned
  cells**; `session_reset!` restores the host defaults (8 / off).
- `semantics/passes/registry` — registers `clear_passes!` as the
  `"semantics.passes"` reset hook.
- `frontend/clike/state` — registers `reset_file_state!` as the
  `"frontend.clike.state"` hook.

This is **additive / behaviour-preserving**: the existing cells and their
`clear_*!` keep working; the session sits above them as the lifecycle owner. The
type-pass `signatures` are intentionally **not** reset (they accumulate inferred
knowledge across modules); `llvm/debug.current_target` is left local for now to
avoid churn on the freshly byte-identical emitter.

## Why this is the foundation for the rest of the #1 cluster

- **#8 (per-run facts)** makes the pass fact store a per-run session cell so it
  auto-resets each run instead of needing a manual `clear_passes!`.
- **#13 / #16** thread the registry-key ABI and the diagnostic bag through this
  one context rather than through ambient globals.

Test: `stdlib/lib/tests/test_session.caap` (cells re-init, hooks run once, the
real prep/passes/clike subsystems reset, inventory lists them).
