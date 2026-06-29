# Sys grant policy — three modes of granting host access

**Source:** the trust boundary in
[stdlib/boot/sys_grants.caap](../../stdlib/boot/sys_grants.caap); the facade
generator in [stdlib/sys/wrap.caap](../../stdlib/sys/wrap.caap); the scoped-take
layer in [stdlib/sys/grant_for.caap](../../stdlib/sys/grant_for.caap); the
capability primitives `host_service_capability` /
`host_service_capability_export` in
[caap/src/builtins](../../caap/src/builtins) (see
[docs/builtins.md](../builtins.md)).

CAAP has **no ambient `read_file`**. Every host service (`io`, `fs`, `os`,
`process`, `net`, `time`, `rand`, `path`) is reached only through a capability
handle, and a handle can only be *minted* inside a bootstrap frame that was
granted the matching capability. This page documents the policy choices an
embedder has — three modes, from fully trusted to fully sandboxed — and the
registry trust boundary they all turn on.

## The registry trust boundary

`boot/sys_grants.caap` is the one file that mints host authority. Run
explicitly, **with** the umbrella `sys` capability, by whoever owns the session:

```lisp
(ctfe_compiler_execute_bootstrap_file compiler
  ".../boot/sys_grants.caap" (list_of "sys"))
```

It mints **one** handle of kind `sys` (which prefix-covers every fine-grained
`sys.*` capability), and inside that granted frame projects every op of every
library into the compiler registry:

```
stdlib.sys.grant.<lib> = { capability: <umbrella handle>, raw: { op -> fn } }
```

The trust boundary is therefore a single decision: **does this file run at
all?** The catch, called out in `sys_grants.caap` itself, is granularity —
*once it has run, anyone who can read the registry can take any library's whole
grant*, capability handle included. The registry value is the authority: holding
`(get grant "capability")` lets a holder project new ops for that whole umbrella.
That is acceptable for a trusted session and too coarse for a restricted one,
which is what the three modes below address.

## Mode 1 — Trusted CLI / dev session (the default)

The session owner *is* the program author. Run `sys_grants.caap` with the `sys`
capability, then load the typed facades; every facade op resolves to a real host
callable.

```lisp
(ctfe_compiler_execute_bootstrap_file compiler
  ".../boot/sys_grants.caap" (list_of "sys"))
(use stdlib.sys.io println)
(println "hello")          ; -> real stdout
```

The umbrella is fine here: the code that can read the registry is the code you
already trust to run. This is what the flagless `caap` CLI does for a normal
program run. No narrowing is needed.

## Mode 2 — Restricted embedding (scoped grants via `grant_for`)

An embedder wants the host present but wants a particular component to hold
**only the ops it needs** — not the umbrella. `sys_grants.caap` still runs
(the host *is* available), but the embedder hands the component a *narrowed
read* of the registry through
[`stdlib.sys.grant_for`](../../stdlib/sys/grant_for.caap) instead of the raw
`stdlib.sys.grant.<lib>` value.

`grant_for` is **purely additive**: it mints, alters, and re-registers nothing
(`sys_grants.caap` and `wrap.caap` are untouched). It only offers narrower reads:

| Export | Take | Shape |
|---|---|---|
| `grant_for_operation lib op` | the **narrowest** take: one ready-to-call op | a callable with the handle already partially applied (call it `(op args…)`), or `null` if ungranted / unknown op |
| `grant_for_module module_name lib` | a **single-library** view tagged with its intended module | a fresh `{ capability, raw, for_module }` for just `lib`, or `null` if ungranted |
| `grant_of lib` | the registered umbrella grant for one library | `{ capability, raw }`, or `null` if ungranted |

`grant_for_operation` is the tightest: a taker that needs `fs.read_text` holds
exactly that one callable — with the capability handle already partially applied
(just like a facade wrapper) — and never sees the handle itself or the library's
sibling ops, so it cannot re-project new ops. `grant_for_module` is one notch
wider — one library rather than all eight — carries the handle (so a facade can
be built from it), and returns a **fresh** `raw` map, so mutating the taker's
copy cannot reach back into the live registry value.

```lisp
(use stdlib.sys.grant_for grant_for_operation grant_for_module)

; hand a sandboxed reader EXACTLY one op — the handle is applied away, so the
; taker calls it directly and never holds the capability itself
(bind ((read_text (grant_for_operation "fs" "read_text")))
  (if (eq read_text null)
    (do (eprintln "fs.read_text not granted") 1)
    (read_text "/etc/hostname")))

; or a single-library view, recorded against the module it is for
(bind ((view (grant_for_module "acme.plugin.logsink" "io")))
  view)   ; { capability, raw, for_module: "acme.plugin.logsink" }, or null
```

Crucially, `grant_for` cannot *manufacture* authority. With no grant registered
it returns `null`, exactly as for an untrusted module — it can only narrow what
`sys_grants.caap` already published, never widen it.

Attenuation is tested by
[`stdlib/lib/tests/test_grant_for_attenuation.caap`](../../stdlib/lib/tests/test_grant_for_attenuation.caap):
the test runner never runs `sys_grants.caap`, so — exactly as `test_sys_wrap`
drives the facade generator on a synthetic op table — it registers a *synthetic*
grant under the same `stdlib.sys.grant.<lib>` key `grant_of` reads and then
asserts the narrowing actually holds. `grant_for_operation` hands back only the
one op's handle-applied callable (a sibling op of the same library is reachable
only as its own separate take, an unknown op is `null`, and the bare callable is
never the grant map, so the handle cannot be re-read); `grant_for_module` yields
a single-library view that cannot reach another library's grant, whose fresh
`raw` map is decoupled from the live registry (a mutation of the taker's copy
does not reach back into it). The granted op still works in-scope, so the test
proves attenuation without breaking legitimate use.
(`test_grant_for.caap` pins the complementary ungranted contract — every take is
`null` when no grant was registered.)

## Mode 3 — Untrusted module / plugin (no ambient grant)

The embedder runs the bootstrap **without** ever running `sys_grants.caap` (or
runs it but never shares any registry value with the untrusted code). There is no
ambient authority anywhere:

- `stdlib.sys.grant.<lib>` is absent, so `make_fn` (in `wrap.caap`) gives every
  facade wrapper a **self-describing throwing stub**: the facade still loads and
  its typed surface is still verified by `verify_sys`, but *calling* an op raises
  `sys.<lib>.<op> requires a sys grant …` rather than returning a silent `null`.
  This is **declaration-only mode**.
- `grant_for_operation` and `grant_for_module` both return `null` — there is no
  grant to narrow, and nothing they can do to invent one.

```lisp
(use stdlib.sys.io println)
(println "hi")    ; raises: sys.io.println requires a sys grant — load
                  ;          boot/sys_grants.caap with "io" before calling it
```

The untrusted code can be loaded, type-checked, and analyzed; it simply has no
path to the host. Capability is **off by default**, and `effect_scope` can only
ever *narrow* an already-held privilege, never regain one — so a callback run
under an attenuated scope cannot climb back to the umbrella.

## Summary

| Mode | `sys_grants.caap` runs? | What the code holds | Calling a host op |
|---|---|---|---|
| 1 — trusted CLI/dev | yes | the registry directly | real host call |
| 2 — restricted embedding | yes | a `grant_for` narrowed take | real host call for the taken op(s) only |
| 3 — untrusted module/plugin | no (or never shared) | nothing | throws "requires a sys grant" |

All three share one invariant: **a handle, not a name string, is authority**, and
the only place a handle is minted is inside the granted `sys_grants.caap` frame.
`grant_for` lets mode 2 exist without weakening that — it hands out *less*, never
more.
