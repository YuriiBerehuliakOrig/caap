# Design: Fine-Grained Capability Enforcement for `sys.*`

## Context

Earlier CAAP host access was coarse-grained: a module could request blanket host
service authority and receive every `sys.*` operation. CAAP now treats the
`sys` hierarchy itself as the capability vocabulary, so a module can say "this
module may read files but not write them, and may not touch the network".

This document designs **least-privilege capability enforcement**: a unit
requests narrow capabilities (e.g. `sys.fs.read`), and the host grants only
those, denying everything else.

The good news from recon: **the enforcement mechanism already exists** at the
right boundary. The work is mostly vocabulary, a hierarchical matcher, and
wiring — not new architecture.

## Current mechanism (grounded)

Three pieces already cooperate:

1. **Per-unit grants** — `BootstrapCapabilityGraph`
   ([compiler/bootstrap.rs:31](../caap/src/compiler/bootstrap.rs)) holds
   `grants: BTreeMap<unit_id, BTreeSet<CapabilityName>>`. The single chokepoint
   `normalize_bootstrap_capabilities` (bootstrap.rs:85) accepts explicit
   capability names from any host domain and rejects obsolete aliases.

2. **The allowlist policy** — `HostCapabilityPolicy`
   ([host/mod.rs:69](../caap/src/host/mod.rs)) is `AllowAll |
   AllowOnly(BTreeSet<CapabilityName>)`. Its canonical query is
   `allows_capability(required_capability)`, where `None` means pure/no host
   authority and `Some("sys.fs.write")` is checked through
   `CapabilityName::covers`.

3. **The enforcement point** — `HostServiceRegistry::export`
   ([host/registry.rs:147](../caap/src/host/registry.rs)) calls
   `capability_policy.allows_capability(required_capability)` when a host export
   is **bound into a module's environment** (at load, not per call). Denial →
   clear error.

Two metadata layers describe each operation:

- **caap-sys** `catalog::capability_effect(library, export) → (domain, effect)`
  ([caap-sys-runtime/src/catalog.rs](../caap-sys-runtime/src/catalog.rs)) —
  domain ∈ {filesystem, network, console, environment, clock, path, process},
  effect ∈ {read, write, pure, network, process}. Hashed into the ABI
  descriptor.
- **caap-core** `host_export_contract(library, export) → (module,
  capability_kind, policy, effect=pure/impure, signature)`
  ([host/fn_misc.rs:41](../caap/src/host/fn_misc.rs)) — authoritative, errors if
  an export has no contract. A completeness test already ties the two layers.

Separately, `HostSystemPolicy` (fs read-roots, net allowlists, env keys …) is
the **sandbox** — *which* paths/hosts/keys. That is **orthogonal** to
capability (*whether* the domain may be touched) and stays as-is.

## The two axes (reconciliation)

The apparent vocabulary clash (e.g. `os.platform`: caap-sys `read` vs caap-core
`pure`) dissolves once we separate two **orthogonal** axes:

| Axis | Question | Source | Used by |
|------|----------|--------|---------|
| **Purity** (`pure`/`impure`) | Is it deterministic / CTFE-safe? | contract `effect` | phase + optimization |
| **Capability** (`sys.fs.read`, …) | Which resource grant is needed? | derived (below) | enforcement |

`os.platform` is *pure* (deterministic, CTFE-safe) **and** needs *no
capability* (reads a constant). No conflict — they answer different questions.

### Required-capability derivation

Add one authoritative function in caap-core:

```
required_capability(library, export) -> Option<CapabilityName>
```

derived from the contract's `capability_kind` (the authority/domain) combined
with caap-sys's `effect` (the read/write granularity):

- `capability_kind == None`  → `None` (pure ops: `path.*`, `net.is-ip`,
  `os.platform`/`arch`). No grant required.
- `capability_kind == Some(domain)` → `Some("<cap>.<access>")` where `<cap>` is
  the domain and `<access>` comes from the caap-sys effect:
  - effect `read`  → `<cap>.read`   (e.g. `fs.read-text` → `sys.fs.read`)
  - effect `write` → `<cap>.write`  (e.g. `fs.write-text` → `sys.fs.write`)
  - effect `network`/`process`/`console` → `<cap>` (no read/write split needed;
    a socket/child/console is used as a whole) → `sys.net`, `sys.process`,
    `sys.io`
  - environment/clock reads → `sys.env`, `sys.clock`

The system mapping lives in **one place** and is covered by extending the
existing catalog↔contract completeness test. Custom and plugin host exports use
their explicit metadata capability names.

## Capability vocabulary

System capabilities are hierarchical, dot-separated, and rooted at `sys`:

```
sys                      (explicit coarse authority for all sys capabilities)
  sys.fs                 (all filesystem)
    sys.fs.read
    sys.fs.write
  sys.net                (all network)
  sys.process            (spawn / control children)
  sys.env                (read environment)
  sys.clock              (read time)
  sys.io                 (console in/out)
```

`host_services` is not a capability alias. Kernel capability checks reject it;
use `sys` for explicit coarse authority or a narrower `sys.*` grant.
Custom host domains may use their own explicit dotted names such as
`test.host` or `vendor.device.read`.

## Hierarchical matcher

`CapabilityName::covers(grant, requested)` uses **prefix-by-segment** matching:
a grant covers a request when the grant is a dotted-segment prefix of (or equal
to) the request.

```
grant "sys"           covers sys.fs.read, sys.net, …
grant "sys.fs"        covers sys.fs.read, sys.fs.write
grant "sys.fs.read"   covers sys.fs.read only
grant "sys.net"       covers sys.net
```

Segment-aware (so `sys.fs` does **not** match `sys.fsx`). `requested` is the
output of `required_capability`, not the raw `library.export`.

## Wiring (grant → policy)

At the boundary where a unit's host exports are bound:

1. Read the unit's grants from `BootstrapCapabilityGraph` (already per-unit).
2. Build `HostCapabilityPolicy::AllowOnly(grants)` for that binding (instead of
   blanket `AllowAll` policy).
3. `registry.export` resolves `required_capability(library, name)` and checks it
   with `HostCapabilityPolicy::allows_capability`.

No per-call caller-identity threading is needed — enforcement stays at
bind-time, which already knows the module.

## Declaration & seed

- Source: `(module_capability "sys.fs.read")` (multiple allowed). The root
  `(module_capability "sys")` is the explicit coarse grant.
- `normalize_bootstrap_capabilities` (bootstrap.rs:85): replace the
  old coarse-name gate with generic capability-name validation.
- The seed gate still decides *whether a module may request root `sys`
  authority at all*; per-capability grants are validated separately.

## Enforcement Status

- **Phase 0 (done)**: capability-effect catalog populated + hashed; catalog↔
  contract drift guard.
- **Phase 1 (done)**: `required_capability`, the capability vocabulary, and the
  hierarchical matcher are first-class host mechanisms.
- **Phase 2 (superseded)**: warn-only enforcement was intentionally skipped in
  favor of explicit host-boundary validation.
- **Phase 3 (enforce — done at the host boundary)**: `HostServiceRegistry::export`
  now enforces the fine-grained model — an export is allowed only when its
  required capability is covered by a grant (pure ops need none). Built-in
  `sys.*` exports derive read/write granularity from the caap-sys catalog;
  custom and plugin exports derive their required capability from explicit
  `HostExportMetadata.capability_kind`. An embedder can sandbox a program with
  `HostCapabilityPolicy::allow_only(["sys.fs.read"])`. `AllowAll` is available
  only as an explicit opt-in for trusted host boundaries.
- **Phase 3b (done)**: per-module enforcement at the bootstrap gate.
  `host_service_export` requires the current bootstrap unit to hold the
  export's `required_capability` for either compile-time or runtime projection;
  the bootstrap capability matcher is hierarchical, and
  `normalize_bootstrap_capabilities` accepts explicit capability names. A module
  can declare `(module_capability "sys.fs.read")` and is held to it; a module
  declaring `(module_capability "sys")` has explicit coarse authority.
- **Phase 3c (done)**: stdlib `system/*` modules migrated to explicit
  least-privilege declarations (`sys.io`, `sys.fs`, `sys.net`, `sys.proc`,
  `sys.time`; `sys.os` → `sys.env` + `sys.path` + `sys.os`). Enabled by making
  `host_service_capability` require the capability it projects (minting a
  `sys.io` projection needs `sys.io`).

## Testing

- `required_capability` returns the expected `Some/None` for every catalog
  export (extend the existing completeness test).
- Matcher: `sys.fs` covers `sys.fs.read`/`write`; `sys.fs.read` does not cover
  `sys.fs.write`; `sys` covers all; `sys.fs` ∤ `sys.net`.
- Enforcement: a unit granted only `sys.fs.read` binds `fs.read-text` but is
  denied `fs.write-text` with a clear message.
- Coarse authority: a `sys` unit gets every `sys.*` export.

## Risks & open questions

- **`net`/`io`/`process` granularity**: read and write share one socket/console/
  child, so a single `sys.net`/`sys.io`/`sys.process` is proposed (no r/w
  split). Revisit if finer control is wanted later.
- **Interaction with `HostSystemPolicy` sandbox**: capability says *may use fs*;
  sandbox says *which paths*. Both must pass. Keep separate; document the layering.
- **Per-unit policy plumbing**: today `set_capability_policy` is registry-wide;
  binding per-module grants may need a per-binding policy override rather than a
  single shared policy. This is the main implementation question for Phase 3.
- **`os.platform`/`arch`**: classified no-capability via `capability_kind ==
  None`; confirm the contracts mark them so.

## Effort

- Phase 1: small, self-contained in caap-core (+ tests) — low risk.
- Phase 2: small (warn logging at `registry.export`).
- Phase 3: medium — per-module policy plumbing + stdlib migration; the only part
  touching the load path, so it carries the real risk and deserves its own PR.
