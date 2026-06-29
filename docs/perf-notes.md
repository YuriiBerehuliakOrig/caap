# Performance notes — compile / load path

Concrete profiling of the CAAP compile/load pipeline, with a measured stage-by-stage
breakdown and grounded optimization recommendations. Written for the C7 (performance)
work unit.

The pipeline under study:

```
bootstrap  →  load (loader + check + type pass [+ transforms])  →  prep  →  emit (llvm/wasm)  →  clang
```

## How these numbers were produced

Measured **in-process** via `caap_cli::commands::run_cli` (the same entry the criterion
bench `caap-cli/benches/compile_bench.rs` drives), with a throwaway harness:
`caap-cli/tests/perf_profile.rs` (an `#[ignore]`d test — it is *not* part of the gate).
Each scenario is the **median of N runs** so warmup/noise don't dominate. Reproduce with:

```bash
# release numbers (headline); drop --release for the debug figures
cargo test -p caap-cli --release --test perf_profile -- --ignored --nocapture
# end-to-end native-emit bench (criterion):
cargo bench -p caap-cli --bench compile_bench
```

Numbers below are **release** mode on this machine. They are noisy at the ±15% level
(several back-to-back runs varied bootstrap between 0.70 s and 1.30 s under load); the
*proportions* are stable across runs. Debug-mode figures are ~5–7× larger but the
breakdown shape is identical.

## Measured breakdown (release)

| # | Scenario | Median |
|---|----------|--------|
| 1 | bare kernel (no stdlib), trivial program | ~0.001 s |
| 2 | `stdlib/bootstrap.caap` + trivial program | **~0.70 s** |
| 3 | bootstrap + load 5 real stdlib modules (no peval) | ~1.11 s |
| 4 | bootstrap + **peval** registered + load the same 5 modules | ~2.25 s |
| 5 | native emit: `tools/compose_native.caap` + `tools/s2_emit.caap` on a tiny program | **~7.7–8.7 s** |
| 6 | native emit, **peval NOT registered** (codegen layer only) | ~9.8–11 s* |

\* scenario 6 was measured in a second, noisier run where the whole table scaled up
(bootstrap 1.30 s); read it against scenario 5 *from the same run* (5 = 12.9 s, 6 = 11.1 s)
— peval added only ~1.8 s there because the user program is tiny.

The criterion bench (`compile/emit/native_emit`, scenario 5 shape) independently reports
**7.4–8.0 s** for the same end-to-end native emit — consistent with the harness.

### Derived deltas (release, quiet run)

| Stage | Cost | Notes |
|-------|------|-------|
| **bootstrap** (2 − 1) | **~0.70 s** | fixed cost paid once per process; matches the "~1 s bootstrap" session fact |
| per-module **load** (3 − 2) | ~0.42 s / 5 modules ≈ **~84 ms / module** | parse → expand → check → type-infer → eval; no transforms |
| **peval fixpoint** (4 − 3) | **+1.13 s** on the same 5 modules → load is now **3.7× slower** | confirms "enabling peval made the suite ~3× slower"; fixpoint runs per module |
| **codegen-layer load + emit** (5 − 2) | **~7–8 s** | dominates the native path — see below |

The peval multiplier reproduced as **3.44× / 3.56× / 3.70×** across three independent
runs — i.e. the ~3× session figure is robust.

## Where the time actually goes (top hot spots)

### 1. The native-emit cost is the *codegen-layer module load*, not the user program

The biggest surprise from the data: emitting LLVM IR for the tiny sample program is
cheap. The ~7.7 s native-emit wall time is almost entirely **loading the codegen layer**
(`boot/native_emit.caap` eager-loads `syntax/render`, `lib/core/equal`, `syntax/ir`,
`backend/prep`, `backend/emit/llvm`, `backend/emit/wasm`, `frontend/surface`,
`backend/driver`, `frontend/clike`). That layer is spread across facades plus
focused leaves and is **~8,500 lines of `.caap`** in the current tree:

| module | lines |
|--------|------:|
| `frontend/clike.caap` + `frontend/clike/*` | 2585 |
| `backend/emit/llvm.caap` + `backend/emit/llvm/*` | 2307 |
| `backend/emit/wasm.caap` + `backend/emit/wasm/*` | 1450 |
| `backend/prep.caap` | 759 |
| `syntax/ir.caap` | 626 |
| `backend/driver.caap` | 478 |
| `frontend/surface.caap` | 240 |

Every line is parsed, expanded, semantically checked, **type-inferred**, partially
evaluated (peval is registered by `native_emit.caap`), and evaluated at load time. At
~84 ms/module *unloaded* and a 3.7× peval multiplier, these large modules are
the ~7–8 s. The user program's actual emit is a rounding error.

Implication: **the native path pays a fixed ~7 s "load the compiler" tax** before it
looks at the program. This is the number a user feels on `caap stdlib/bootstrap.caap s2_emit FILE`.

### 2. The peval fixpoint runs per module and re-folds converged trees

`stdlib/semantics/passes/peval.caap` installs a whole-module **transform** that, for every
located form, drives `constprop → constfold → simplify → dce` to a fixpoint
(`peval_node`, cap **16** iterations), using `node_eq` (a full structural deep-equal,
`syntax/ir.caap:150`) to detect convergence:

```
(while (and (not (deref done)) (lt (deref i) 16))
  (bind ((next (round (deref cur))))          ; 4 full tree-walks (constprop+fold+simplify+dce)
    (if (node_eq next (deref cur))            ; + 1 full deep-equal walk
      (set_ref done true)
      (do (set_ref cur next) ...))))
```

Cost per form = (#iterations) × (4 rebuild walks + 1 deep-equal walk). Convergent inputs
settle in 1–2 iterations, but **even a form that doesn't change at all pays one full
`round` + one `node_eq`** before stopping. Two concrete redundancies:

- **`constprop` rebuilds every `bind` body even when there is nothing to propagate.**
  For a `(bind …)` whose pairs hold no literal/name values, `constprop` builds an *empty*
  `lit_map` and then still calls `subst_safe` on every pair value and every body element
  (`peval.caap:45–54`). `subst_safe` with an empty map is the identity on tree *shape* but
  still does a full capture-avoiding tree-walk-and-rebuild per child. This is the common
  case (most binds bind computed values, not literals).
- **The convergence `node_eq` is a second full walk over a tree `round` just rebuilt.**
  The sub-passes already know whether they fired; the deep-equal re-derives that fact.

> Important correction to a common assumption: the **base** bootstrap registers **no**
> transforms at all (grep confirms only `boot/native_emit.caap`, `boot/peval.caap`,
> `boot/pe.caap` call a transform `register!`). So peval/constfold/simplify/dce cost is
> paid **only** when a PE leg is composed in (native builds, or explicit `boot/peval.caap`).
> Plain `stdlib/bootstrap.caap` loads are transform-free.

### 3. Type inference re-walks each `defn` body twice

`semantics/types/infer.caap` (`check_module_types` → `tc`) walks a `(defn …)` with a sig
once *untyped* during the flat-bind walk and again *typed* via `check_fn_body`
(`infer.caap:529` then `:540–550`). For modules dense in `defn`s — the whole `lib/` and
codegen layer — that is ~2 body walks per function plus per-scope `clone env` copies. This
is the per-module load cost (the ~84 ms/module above), paid on *every* load, peval or not.

## Recommendations (ranked by expected impact)

Each is grounded in a measured hot spot above. None has been applied except where noted.

### R1 — Cache a bootstrapped image for the native/codegen path *(highest impact)*

The ~7 s native-emit tax is *loading the compiler*, repeated on every `s2_emit`/`s2_build`
invocation and (more painfully) on every test that needs codegen. The Rust test harness
already caches the *plain* bootstrap (`common::bootstrapped_session()` clones a cached
session, per `docs/testing.md`); there is no equivalent for the **codegen-layer** session.
Provide a cached `native_bootstrapped_session()` (bootstrap + `native_emit.caap` loaded
once, cloned per test). Expected impact: codegen-touching tests drop from ~8 s of setup to
a clone. This is the single biggest lever and touches only test infrastructure (no
shared compiler file). *Out of scope for a one-file micro-opt; recommended as the lead
follow-up.*

### R2 — Skip `peval`'s `subst_safe` when there is nothing to propagate

In `constprop` (`stdlib/semantics/passes/peval.caap`), when the collected `lit_map` is
empty, return `null` (the transform "keep the rebuilt node" signal) instead of running
`subst_safe` over every pair value and body element. `transform` already rebuilt the
children bottom-up, and `subst_safe` with an empty map is shape-preserving — so the result
is identical, minus N redundant full tree rebuilds per literal-free `bind`. Behaviour-
preserving and isolated to `peval.caap` (a file only loaded under a PE leg, not a hot
shared file). Expected impact: a meaningful constant-factor cut on the peval leg for the
many `bind`s that hold computed (non-literal) values — most of them. **Applied** (see
below); the empty-map case is the common one in the codegen layer.

### R3 — Avoid the redundant convergence deep-equal in the peval fixpoint

`peval_node` calls `node_eq` (a full structural walk) every iteration to detect a
fixpoint, immediately after `round` rebuilt the whole tree. If the four sub-passes returned
a "changed?" flag (or `round` did), the fixpoint could stop on that flag and skip the
deep-equal entirely — roughly a 20% walk-count reduction per iteration (5 walks → 4).
This requires threading a changed-flag through `constfold`/`simplify`/`dce`/`constprop`,
so it touches several pass files; left as a recommendation, not applied.

### R4 — Single-pass the transform chain instead of 4 independent tree-walks

When peval (or constfold+simplify+dce) is registered, each `round` is four separate
bottom-up walks. Fusing `constprop`/`fold_node`/`simplify_node`/`rewrite_node` into one
bottom-up visitor (each applied at the node on the way up) would cut the per-iteration
walk count from 4 to 1. Larger refactor across the pass modules; recommendation only.

### R5 — Cache the transform-registry topological order per session

`semantics/passes/registry.caap`'s `ordered` re-topologically-sorts the transform list on
*every* module load, and its `present?` check is a linear scan → O(R²) in the number of
registered transforms per load. R is tiny today (1–4), so impact is small, but caching the
sorted order on registration (invalidate on `register!`) removes the per-load resort.
Low impact; recommendation only.

### R6 — Don't re-run check + type pass in `prep` when the loader already gated

`backend/prep.caap`'s `gate!` re-runs `check_forms` + `check_module_types` before codegen
(`prep.caap` gate stage), duplicating the loader's gate for modules that were already
loaded-and-checked. Gating only the *freshly inlined* forms would avoid a full re-check on
the native path. Touches `prep.caap` (a file owned by another worker) — recommendation
only, not applied.

## Applied optimization

**R2** was applied to `stdlib/semantics/passes/peval.caap` (a non-forbidden, non-hot-shared
file): `constprop` now returns `null` (no-op) when its `lit_map` is empty, skipping the
redundant `subst_safe` rebuild of every body/pair for the common literal-free `bind`. It is
behaviour-preserving (empty-map `subst_safe` is the identity on tree shape) and the full
gate (`fmt` + `clippy -D` + `cargo nextest run --workspace`) stays green. See the commit
for the exact diff.

## Reproduction artifacts

- Harness: `caap-cli/tests/perf_profile.rs` (`#[ignore]`d; run with `--ignored --nocapture`).
- Compose files used by the harness: `tools/compose_peval.caap`, `tools/compose_native_nopeval.caap`
  (alongside the existing `tools/compose_native.caap`).
- Existing end-to-end bench: `caap-cli/benches/compile_bench.rs`.
