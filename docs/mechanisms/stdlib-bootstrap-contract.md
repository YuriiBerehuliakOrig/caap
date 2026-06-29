# Stdlib bootstrap contract — the ordered boot manifest

**Source:** the tier loader in
[stdlib/bootstrap.caap](../../stdlib/bootstrap.caap); the boot modules it runs
under [stdlib/boot/](../../stdlib/boot/); the module-identity governance test in
[caap/tests/stdlib_governance_tests.rs](../../caap/tests/stdlib_governance_tests.rs).

A bare CAAP compiler session knows **nothing** — there is no autoloading. Running
`stdlib/bootstrap.caap` (itself ordinary CAAP, evaluated in the compile-time
phase) is what registers the expander, the loader, the type layer, and the
session-command surface. The order is load-bearing: each phase is written in the
language the phase below it has just provided, so a missing or reordered phase
does not "degrade gracefully" — it makes later phases reference something that
does not exist yet. This page pins that order as a **contract** and documents the
post-phase assertions that enforce it.

## The phase sequence (the `boot_manifest`)

`bootstrap.caap` carries an explicit ordered manifest — a data table — that is the
single source of truth for *which phase registers which registry key*. It does
**not** drive the load order (that stays the hand-written sequence the comments
document); it is the contract the assertions check.

| # | Phase | File | Registry key it must publish | Run via |
|---|-------|------|------------------------------|---------|
| 1 | `expander`   | `boot/expander.caap`           | `stdlib.expand`                    | `execute_bootstrap_file` (raw) |
| 2 | `forms`      | `boot/forms.caap`              | `stdlib.expand` (forms registered)| `execute_bootstrap_file` (raw) |
| 3 | `check`      | `boot/check.caap`              | `stdlib.check`                     | `run_expanded` |
| 4 | `namespace`  | `boot/namespace.caap`          | `stdlib.namespace`                 | `run_expanded` |
| 5 | `resolve`    | `boot/resolve.caap`            | `stdlib.resolve`                   | `run_expanded` |
| 6 | `gate`       | `boot/gate.caap`               | `stdlib.gate`                      | `run_expanded` |
| 7 | `reader`     | `boot/reader.caap`             | `stdlib.reader`                    | `run_expanded` |
| 8 | `unit_build` | `boot/unit_build.caap`         | `stdlib.unit_build`                | `run_expanded` |
| 9 | `loader`     | `boot/loader.caap`             | `stdlib.load`                      | `run_expanded` |
| 10| `types`      | `semantics/types/infer.caap`   | `stdlib.semantics.types.infer`     | loader `load` (after registry/records/effects) |
| 11| `commands`   | `boot/commands.caap`           | `caap.session.commands`            | loader `load` |

Phases 1–2 build the expander; once `stdlib.expand` exists, the remaining boot
modules are run **through** the expander (`run_expanded` = expand-then-eval per
top-level form) so they may use stdlib sugar (`cond`/`with_map`/`for`/…). Phase 9
(`loader`) wires phases 4–8 together — it looks each up by its `stdlib.*` key and
threads its shared `state`. After phase 9 the loader is live, so phases 10–11 are
ordinary `load` calls. Between phases 9 and 10 the bootstrap also loads the tier-2
groundwork (`sequence`/`map`/`syntax.ast`) the type layer is written *with*, then
`backfill`s their signatures once `infer` exists; these are implied by the
`types` phase and need no separate manifest row.

After phase 9 the bootstrap self-declares the `stdlib` root (so `stdlib.*` names
resolve by name in every session) and the `sys` compat root (so the legacy
`sys.<lib>` facade names still resolve alongside their canonical
`stdlib.sys.<lib>` — see [stdlib/CONVENTIONS.md](../../stdlib/CONVENTIONS.md)).

## The post-phase assertions

For each manifest row, `bootstrap.caap` calls `(check_phase! "<phase>")` right
after that phase runs. `check_phase!` looks the phase up in `boot_manifest` and
raises a located error if its registry key is **absent**:

```
stdlib bootstrap: phase "reader" (boot/reader.caap) did not register
"stdlib.reader" — the boot sequence is broken or reordered
```

Properties:

- **Additive.** The assertions only *read* the registry
  (`ctfe_compiler_lookup_value`); they never change the load order. Removing every
  `check_phase!` call would leave behaviour identical (until a phase silently
  breaks).
- **Loud, early, precise.** A dropped or reordered phase fails *at boot* with the
  phase name, the file, and the missing key — instead of surfacing later as a
  confusing "unknown module `stdlib.…`" deep inside an unrelated `load`.
- **Not vacuous.** Each assertion is a real registry lookup; pointing a manifest
  row at a bogus key makes the corresponding `check_phase!` fail (verified during
  development), so the contract cannot rot into a no-op.

Phases 1–2 (`expander`/`forms`) have no explicit `check_phase!` call because the
bootstrap *immediately* dereferences `stdlib.expand` to build `run_expanded`; a
missing expander fails there, before any later phase can run.

## Why the boot files carry no `(module …)` form

The phases run via `execute_bootstrap_file` / `run_expanded` are **raw bootstrap
scripts**: they execute before (or as part of bringing up) the machinery that
gives `(module …)` meaning, so they cannot carry a `(module …)` form — adding one
raises `unknown name: module`. They set their registry identity explicitly with
`ctfe_compiler_register_value` and are allowlisted in the
`stdlib_module_identity_check` governance test. The boot modules that *are* loaded
through the loader (`analyze`/`commands`/`run`/`opt`) do carry `(module …)`. See
the "Module Identity Policy" section of
[stdlib/CONVENTIONS.md](../../stdlib/CONVENTIONS.md) for the full policy and the
intentional short-name allowlist.
