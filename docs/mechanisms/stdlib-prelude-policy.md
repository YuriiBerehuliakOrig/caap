# Stdlib Prelude — Stability Policy

**Source:** [stdlib/lib/core/prelude.caap](../../stdlib/lib/core/prelude.caap)
(`module stdlib.lib.core.prelude`); the existence snapshot that guards it is
[stdlib/lib/tests/test_prelude_exports.caap](../../stdlib/lib/tests/test_prelude_exports.caap).

The prelude is **one import surface over the everyday set** — a single module
you can `(use stdlib.lib.core.prelude …)` instead of importing a dozen home
modules. This page is the stability contract: what belongs in it, how name
collisions are resolved, and how to recover a module's full surface when the
prelude deliberately leaves a name out.

## What the prelude is (and is not)

- **Pure re-exports — nothing is *defined* here.** Every name stays defined in,
  and documented by, its *home* module. A `(re_export <module> a b c …)` line
  forwards those names unchanged; the prelude only saves the import boilerplate.
  Forwarding is identity: `(use stdlib.lib.core.prelude map)` binds the *same*
  `map` you would get from `(use stdlib.lib.collections.sequence map)`.
- **A curated everyday subset, not the union of exports.** The prelude is a hand-
  picked slice of each home module, never the whole module. A name's absence from
  the prelude is intentional, not a gap — it is still reachable via its module.

## Criteria for inclusion

A name earns a prelude slot only when all of these hold (derived from the curation
notes in `prelude.caap`):

1. **High-frequency / everyday.** It is part of the set you reach for in ordinary
   programs — the core sequence/map/option/result/math/equality/functional verbs
   (`map`, `filter`, `fold`, `keys`, `some`, `ok`, `deep_eq`, `abs`, `compose`, …).
2. **Dependency-light.** It is a small, general primitive that pulls in no heavy
   machinery — not something whose use implies adopting a larger subsystem.
3. **No domain-specific API.** Specialized or subsystem surfaces stay in their own
   modules. The prelude re-exports only the everyday combinators; e.g. it forwards
   `compose`/`pipe`/`identity` while *functional's full set stays in its module*.
4. **No ambiguous duplicate — unless the collision is explicitly resolved.** Where a
   bare name is exported by more than one module, the prelude must pick exactly one
   home (see below). It never re-exports the same bare name from two modules.

When a name fails any criterion, the rule is the same: **leave it out of the
prelude and import its module directly.**

## How collisions are resolved — one home wins

When a bare name is exported by more than one module, the prelude routes it to a
**single documented home**; the other variants stay reachable by importing their
module directly. The currently-resolved collisions are:

| Name(s) | Prelude home (winner) | Loser — reach it via |
|---|---|---|
| `clamp`, `pow` | `stdlib.lib.core.math` (int variants) | `stdlib.lib.core.float` (f64 variants) |
| `error_code`, `error_message` | `stdlib.lib.collections.result` (Result accessors) | `stdlib.lib.diag.error` (its own same-named accessors) |
| `concat` | `stdlib.lib.text.string` | the other exporter(s), imported directly |
| `min`, `max` | `stdlib.lib.core.math` | the other exporter(s), imported directly |
| `empty?` | *(dropped — re-exported by no module)* | each exporting module, imported directly |

The prelude's own header (`prelude.caap` lines 8-9) names `empty?`, `concat`, and
`min`/`max` as examples of bare names exported by more than one module — "only one
home wins here, and the rest stay reachable via their module." `concat` and
`min`/`max` win toward the homes shown above (text.string, math); `empty?` is left
out of the prelude entirely, so neither exporter's `empty?` is re-hosted here.

Two named-and-justified resolutions, quoted from the prelude:

- **float vs. math (`clamp`/`pow`).** The prelude re-exports `abs sign min max
  clamp pow gcd even? odd?` from `core.math`. It re-exports only `approx_eq` from
  `core.float` — *NOT* float `clamp`/`pow`, because "both names are already
  curated toward the int (math) home … import `stdlib.lib.core.float` directly for
  the f64 variants." `approx_eq` is forwarded because it is the right way to
  compare floats and has no int counterpart, so there is no collision.
- **result vs. diag.error (`error_code`/`error_message`).** These are re-exported
  as the **Result** accessors. "The diag.error module keeps its own same-named
  accessors, reachable by importing it directly (a curated prelude resolves the
  name collision toward Result)." The prelude still forwards diag.error's
  *non-colliding* names (`make_error error? raise! as_error`).

The invariant: **a curated prelude resolves every name collision toward a single
home.** Changing a winner is a breaking change to the import surface and must be
called out, never done silently.

## The current prelude surface

The names below are exactly what the prelude re-exports today, grouped by home
module. This list IS the contract that
[`test_prelude_exports.caap`](../../stdlib/lib/tests/test_prelude_exports.caap)
snapshots — every name must still resolve through the prelude.

| Home module | Re-exported names |
|---|---|
| `stdlib.lib.collections.sequence` | `map filter fold each range find any? all? take drop join first last sum contains? flat_map unique partition` |
| `stdlib.lib.collections.map` | `keys values merge clone entries get_in` |
| `stdlib.lib.text.string` | `split trim concat chars parse_int` |
| `stdlib.lib.collections.option` | `some none some? none? option_of option_map option_and_then option_or option_unwrap_or` |
| `stdlib.lib.collections.result` | `ok err ok? err? error_of error_code error_message unwrap unwrap_or map_ok and_then map_err or_else` |
| `stdlib.lib.core.equal` | `deep_eq deep_ne` |
| `stdlib.lib.core.math` | `abs sign min max clamp pow gcd even? odd?` |
| `stdlib.lib.core.functional` | `compose pipe identity` |
| `stdlib.lib.core.float` | `approx_eq` |
| `stdlib.lib.diag.error` | `make_error error? raise! as_error` |

## Finding a module's full surface vs. the prelude subset

The prelude is a slice; the home module is the whole surface. To get a name the
prelude leaves out:

1. **Identify the home module** from the table above (or the `re_export` line in
   `prelude.caap`). The home is where the name is *defined* and documented.
2. **Read the home module's exports** to see its full surface — e.g.
   `stdlib/lib/core/functional.caap` for the combinators beyond
   `compose`/`pipe`/`identity`, or `stdlib/lib/core/float.caap` for the f64
   `clamp`/`pow`. The flat [stdlib reference](../stdlib-reference.md) also lists
   every module and its public exports.
3. **Import the module directly:** `(use stdlib.lib.core.float clamp pow)` for the
   float variants, `(use stdlib.lib.diag.error error_code)` for the diag-error
   accessor, and so on. Importing the home module is always available and is the
   intended escape hatch — the prelude never hides a name, it only declines to
   re-host it.

## Stability rules of thumb

- **Adding a name** to the prelude is additive and safe *iff* it passes the four
  criteria and introduces no unresolved collision.
- **Removing or re-homing a name** changes the import surface — it is a breaking
  change and must be stated by the task, never silent.
- **The snapshot test is the guard:** `test_prelude_exports.caap` imports every
  re-exported name and asserts each resolves to a bound (non-null / callable)
  value. A broken `re_export` (typo'd name, deleted home definition, renamed
  symbol) fails that test instead of silently shrinking the surface.
