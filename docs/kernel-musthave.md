# Kernel must-haves — 2026-06-10

The ten items the kernel needs most right now, ranked. Same maintenance rule as
the remediation plan: items whose DoD is met move to the Done section — this
list is wrong the moment it goes stale.

Effort: S < half a day · M ≈ 1–3 days.

---

## Security & semantics (real gaps)

*(#1 — memory budget for effect-scoped code — moved to Done below.)*

*(#2 — error-channel decision — moved to Done below.)*

*(#3 — TCO — moved to Done below.)*

## Open tails (ready to execute)

*(#4 — wrapper retirement — moved to Done below.)*

### 5 · Prune the programmatic PEG combinators (−34) — S
Second way to build grammars, zero uses; the textual rule path is closed and
complete. Awaiting the owner's go. **DoD:** combinators + builder gone, doc
bijection/classification updated by their locks, battery green.

### 6 · Engine-tail decision (−20 or a declared tooling bet) — S
`ctfe_grammar_*` incremental/profiled/recover/diff/… have no in-language
consumer; either cut to the working core (~12) or explicitly declare in-CAAP
editor tooling a roadmap bet and keep them. Any decision beats limbo. **DoD:**
decision recorded here + executed.

### 7 · Completeness tail: `string_chars` + float precision + stdlib `deep_eq` — S (bundle)
Last three items of the completeness audit (chars iteration is O(n²) via slice
today; no float formatting control; map `eq` is identity — stdlib needs the
structural twin). **DoD:** all three landed with tests; completeness audit
closed 100%.

## Platform guarantees

### 8 · Determinism gate: byte-identical artifacts — S–M
Principle #9 has no end-to-end lock: compile the same input twice (and across
processes) → LLVM IR/artifacts must be byte-identical, enforced as a test like
the classification bijection. **DoD:** the double-compile test in the battery.

### 9 · Daemon mode (`caap serve`) — M
Cold start is eval-bound (~1.3 s of stdlib bootstrap execution; the parse-cache
image was measured net-negative and reverted — see
`memory: bootstrap-startup-profile`). The only real fix is a resident process;
the LSP already is one — extract the shared server layer. **DoD:** repeat
invocations skip bootstrap; CLI latency test.

### 10 · Stability tiers for the 338 builtins — S
No stable/experimental marking; for a language-building platform that contract
is the difference between promise and quicksand. **DoD:** a tier column in
builtins.md + a lock test that every builtin carries a tier.

---

**Watch item (not yet a list entry):** the stdlib type-system tests run deep;
if a stack abort reproduces on a settled commit, that is a sixth unprotected
recursion (likely the value-conversion helpers) — same `grow_stack` treatment
as eval/peg/CST/lowering.

## Done

- **Memory budget for effect-scoped code** (was #1) — ✅ 2026-06-13: a
  cumulative allocation budget (collection/string elements), opt-in and scoped
  exactly like the step budget, `Rc`-shared across sub-evaluators so the ceiling
  holds end-to-end. Charged at every super-`O(1)` allocation chokepoint (the
  same sites that enforce `runtime_collection_limit`); FATAL on exhaustion so it
  pierces `try`. `effect_scope` (the documented untrusted-code boundary)
  installs a default budget when none is active (nesting cannot reset it), and
  the CTFE fold installs one alongside its step budget. Before: a pure-scope
  `(append … (string_repeat …))` loop OOM-**aborted** the process (core dump,
  uncatchable) — reproduced under `ulimit -v`. After: clean structured failure.
  Trusted top-level execution stays unbounded. Tests in
  `caap/tests/allocation_budget_tests.rs` (9, incl. hostile-loop / try-pierce /
  nesting-reset / trusted-unbounded); the no-panic fuzz harness now runs under
  both budgets.

- **Tail-call optimization** (was #3) — ✅ 2026-06-12, `b517e7a`: a tail call
  whose callee evaluates to the executing closure reuses the frame
  (trampoline). Self-recursive tail loops run at constant evaluation depth.
  Tail positions compose through `if`/`do`/`bind`/`match`; `try` bodies and
  arguments are not tail. See `caap/tests/tail_call_tests.rs`.

- **Error channel: `try` catches evaluation errors** (was #2) — ✅ 2026-06-12,
  owner's decision. Handler receives the thrown value as-is or
  `{message, category}` for an error; FATAL errors (step/depth budget
  exhaustion, marked via EvaluationError::into_fatal) pierce `try` so hostile
  folds cannot trap their own bounds. Dual-phase law extended (caught errors
  equal across phases). Unlocks lib-wide validate-before-eval removal,
  recoverable parse_float, recoverable `const`.

- **P3 wrapper retirement** (was #4) — ✅ 2026-06-12: core took over the stdlib
  migration (14 sites; the "dynamic" builder helper turned out all-literal and
  was deleted); kernel surface 340 → 339, no transitional spellings left.

- **Dual-phase equivalence law** (was #1) — ✅ 2026-06-10, `0ed39b6`:
  CompileTime ≡ Runtime as a property suite (4096 cases/law) + pinned edges +
  budgeted-fold law.
- **No-panic fuzz harness** (was #2) — ✅ 2026-06-10, `0ed39b6`: deterministic
  parser/evaluator no-panic sweeps, `CAAP_FUZZ_ITERS`-tunable, 60k-iteration
  release soak clean; cargo-fuzz is the declared upgrade once nightly is
  available.
