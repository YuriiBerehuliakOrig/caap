# `stdlib/sys` — typed, capability-gated facades over caap-sys

The host exposes its system services (`io`, `fs`, `os`, `process`, `net`,
`time`, `rand`, `path`) through the kernel's host-service surface: raw, untyped
callables that each take a capability handle as their first argument. This
directory turns that raw surface into **eight typed CAAP facades** with a single,
verified shape — and
proves, at startup, that the facades never drift from the live host catalog.

A facade gives you three things the raw surface does not:

1. **A typed, named API.** `(println "hi")` — not `(println <handle> "hi")`. The
   capability handle is partially applied away; parameters and results are
   typed, so importers get full call-site checking and the catalog effect from
   the type pass.
2. **Opt-in capability gating.** No grant ⇒ every callable is a *self-describing
   throwing stub* and the facade is *declaration-only* (it loads, types check,
   but calling a stub raises `sys.<lib>.<op> requires a sys grant …` rather than
   returning a silent `null` that fails later as "not callable"). One explicit
   bootstrap step mints the grant; until then there is no ambient authority to
   reach the host.
3. **A drift guard.** `verify_sys` diffs every facade's typed declaration
   against the live `caap-sys` catalog and throws on any mismatch — a phantom
   op, a wrong type, a forgotten op. The declaration cannot rot.

## Files

| File | Role |
|------|------|
| `verify.caap` | `p`/`op` (build a typed signature), `verify_facade` (diff one facade vs a catalog), `verify_sys` (the startup check: discover + verify every facade). |
| `wrap.caap` | The **facade generator**: `make_facade` (file the typed surface + build the wrapper bundle in one call), plus its parts `declare_ops!` and `make_fn`. |
| `io.caap` `fs.caap` `os.caap` `process.caap` `net.caap` `time.caap` `rand.caap` `path.caap` | The eight facades — each is just an `ops` table + the names it exports. |

## Module names — canonical `stdlib.sys.*`, compat alias `sys.*`

Each facade declares the **path-canonical** module name `(module stdlib.sys.<lib>)`
(matching its `stdlib/sys/<lib>.caap` path), so `(use stdlib.sys.fs …)` resolves
via the `stdlib` root like every other stdlib module. The historical short name
`sys.<lib>` is kept as a **compat alias**: `stdlib/bootstrap.caap` declares a `sys`
root mapping `sys.* -> stdlib/sys/*.caap`, so `(use sys.fs …)` still resolves to
the same file. The loader reads the file's `(module stdlib.sys.<lib>)` directive
and registers it canonically, so loading under either name returns the same export
map. Call-site type checking works under both names because `make_facade` files the
op signatures under both `stdlib.sys.<lib>` and `sys.<lib>`. The intentional
short-name alias is recorded in `stdlib/CONVENTIONS.md` and pinned by
`stdlib_module_identity_check` (`caap/tests/stdlib_governance_tests.rs`).

## How a facade is built

Every facade is the same three lines of structure around its op table, so the
boilerplate lives in `wrap.caap` and a facade carries only what is unique to it:
its typed ops and the names it publishes.

```lisp
(module stdlib.sys.time)                     ; canonical, path-derived name
(use stdlib.sys.verify op p)
(use stdlib.sys.wrap make_facade)

(bind ops                                  ; 1. the TYPED surface
  (list_of
    (op "sleep_ms" (list_of (p "ms" "int")) "null" "impure")
    ...))

(bind sys (make_facade "time" ops))        ; 2. file types + build wrappers
(bind sleep_ms (get sys "sleep_ms"))       ; 3. publish each op by name
...
(export ops sleep_ms ...)
```

`make_facade "<lib>" ops`:

- runs `declare_ops!` for the facade (under BOTH the canonical `stdlib.sys.<lib>`
  and the compat alias `sys.<lib>`), handing the type pass each op's
  params/result/effect, so a mistyped sys call at *any importer* — whether it
  writes `(use stdlib.sys.io …)` or the legacy `(use sys.io …)` — is a load-time
  error (`` `write` arg 1: expected string, got int ``), and
- returns a `name -> wrapper` map. Each wrapper is the host callable with the
  capability handle partially applied — or a *self-describing throwing stub* (not
  `null`) if there is no grant, or no such op in the grant.

The per-name binds read from that one bundle, so the wiring exists in exactly
one place and the bound names come from the same op table `verify_sys` checks.
The `(export …)` directive is the only hand-kept list, and the loader validates
it against the actual binds.

> The `ops` export is also the contract `verify_sys` reads. It must stay a
> top-level export with one `(op …)` per host op.

## Dual-phase callables

Sys exports are **dual-phase**: the same wrapper runs at compile time *and* at
runtime. What a call may *do* is decided by the active **sandbox policy**, not by
the phase. The compile-time sandbox allows `os` queries and stdout logging but
denies stdin, filesystem writes, network, and process control; a runtime-phase
evaluation under the grant allows the full surface. So:

- `os.platform` is catalog-`pure` and runs in both phases — `(const (platform))`
  even folds it at expansion time.
- `net.connect` / `fs.write_text` are live only in a runtime-phase evaluation
  that holds the grant; at CTFE the policy denies them.

## Opt-in: `sys_grants`

Capabilities are **off by default**. Projecting a host callable demands ambient
capability (the kernel checks the current bootstrap frame), so the raw callables
are minted inside a granted frame by `boot/sys_grants.caap` — run explicitly,
with the umbrella `sys` capability, by whoever owns the session:

```lisp
(ctfe_compiler_execute_bootstrap_file compiler
  ".../boot/sys_grants.caap" (list_of "sys"))
```

That registers `stdlib.sys.grant.<lib> = { capability, raw }` for each library
(catalog-driven, so the projection itself never drifts). `make_fn` looks the
grant up by `<lib>`; with no grant registered, every wrapper is a self-describing
throwing stub that raises "requires a sys grant" when called, and the facade is
declaration-only. The embedder decides whether the grant file runs at
all — that single decision is the trust boundary for all of `sys`.

## `verify_sys` — the startup drift guard

```lisp
(verify_sys ".../stdlib/sys")   ; -> true, or throws on the first drift
```

Facades are **discovered, not registered**: every `<lib>.caap` in the directory
that exports `ops` is verified against the `<lib>` host catalog (filename =
library name). Adding a facade file is therefore impossible to forget, and files
without an `ops` export (`verify.caap`, `wrap.caap`) are skipped. `verify_sys`
throws on any of: an op the host does not provide, an effect/return-type
mismatch, a param count/name/type mismatch, or a host op the facade forgot to
declare. This is the call an embedder makes right after bootstrap.

## Status of each facade

All eight mirror their host library 1:1 against the live catalog (`verify_sys` is
green), and every op is backed by a real host implementation:

| Facade | Surface |
|--------|---------|
| `io` | stdout/stderr print/write/flush, stdin read — runtime-phase. |
| `fs` | files + directories: open/read/write/seek, metadata, links, dir listing. |
| `os` | env vars, cwd, hostname, arch/platform/family (some catalog-`pure`). |
| `process` | spawn/run/wait/kill, stdio pipes, args, pid. |
| `net` | **full TCP + UDP**: listen/accept/connect, stream read/write, UDP bind/send/recv, poll, resolve, shutdown — backed by real sockets in `caap-sys-runtime/net.rs`. Not a skeleton. |
| `time` | monotonic + wall-clock nanos/millis/seconds, sleep. |
| `rand` | host randomness facade (bytes/int/float/bool). |
| `path` | host path operations + relative-path helper. |

There is no runtime-blocked gap in this layer: each facade op resolves to a host
catalog entry that `verify_sys` confirms, and the host implements it.
