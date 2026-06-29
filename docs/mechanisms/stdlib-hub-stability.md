# Stdlib Hub Stability Policy

**Source:** the active stdlib dependency graph (static scan over
`(use|import|re_export)` directives across [stdlib/](../../stdlib/)), enforced by
the `stdlib_hub_fanin_within_budget` governance test.

The stdlib dependency graph has **no cycles**, but it does have a few strong
**hubs** — modules with high fan-in (many dependents). A small API change in a
hub has a wide blast radius, so these modules carry a heavier stability
contract than ordinary leaf modules. This page names them, gives the change
checklist a hub edit must satisfy, and the cohesion-boundary guidance for *if*
and *when* a hub should be split.

This is policy, not a mechanism description: it tells you how to change a hub
safely, not what a hub does internally.

## Stability-critical hub modules

These four modules are **stability-critical**. Fan-in figures are approximate
(a static `(use|import|re_export)` scan over `stdlib/`, excluding `lib/tests/`);
they drift as modules are added, so treat them as orders of magnitude, not exact
counts.

| Module | Approx. fan-in | What it anchors |
|---|---|---|
| [`stdlib.lib.collections.sequence`](../../stdlib/lib/collections/sequence.caap) | ~41 dependents | the core list/sequence algorithm vocabulary nearly every module builds on. |
| [`stdlib.syntax.ast`](../../stdlib/syntax/ast.caap) | ~40 dependents | the code-as-data AST substrate: node predicates, builders, spans, eval helpers. |
| [`stdlib.semantics.passes.registry`](../../stdlib/semantics/passes/registry.caap) | ~27 dependents | pass ordering, the transform/fact/schema store, pass-finding diagnostics. |
| [`stdlib.syntax.ir`](../../stdlib/syntax/ir.caap) | ~22 dependents | the IR-level companion to `syntax.ast` (Name/Literal/Call node manipulation). |

These are healthy, foundational modules — wide fan-in is *expected* here. The
point is not to shrink the fan-in; it is to change these modules with more care
than a leaf module, because "everyone depends on this" is exactly what makes a
small mistake expensive.

**Enforced, not just documented.** `stdlib_hub_fanin_within_budget` in
[`caap/tests/stdlib_governance_tests.rs`](../../caap/tests/stdlib_governance_tests.rs)
turns these figures into a build gate. It rebuilds the import graph (a static
`(use|import|re_export)` scan, `lib/tests/` excluded) and, for each hub, asserts
the importer count both (a) stays within a *ceiling* a little above the current
figure — so a new dependent that widens the blast radius fails the build until
the budget is consciously raised — and (b) equals an *exact pinned* number, so any
drift up or down surfaces as a checked-artifact change. When you legitimately add
or remove a hub dependent (and follow the change checklist below), update the
`HUB_FANIN_CEILINGS` / `HUB_FANIN_EXACT` constants in that test — and, if the
order of magnitude shifts, the approximate figure in the table above.

## The change checklist for a hub edit

Any change to a stability-critical hub must satisfy all three:

1. **Changelog entry.** Record the change at the hub explicitly, even for an
   additive export — so a downstream maintainer chasing a behavioral shift can
   find the cause at the hub rather than re-deriving it from a diff.
2. **Full stdlib test run.** A focused test pass is not enough; the blast radius
   crosses tiers. Run the loader corpus plus the passes/forms/types scenarios —
   i.e. the full split proof-gate
   ([`stdlib_loader_tests.rs`](../../caap/tests/stdlib_loader_tests.rs),
   `stdlib_passes_tests.rs`, `stdlib_forms_tests.rs`, `stdlib_types_tests.rs`),
   not just the suite nearest the file you touched.
3. **Dependency-impact note.** State which dependents the change can reach and
   why the change is safe for them (behavior-preserving, additive-only, or — if
   genuinely breaking — which dependents were updated in the same change). The
   blast radius must be reasoned about in the change, not discovered later by a
   broken downstream build.

## When (and when not) to split a hub

The rule: **split only on a real cohesion boundary, not on fan-in alone.** High
fan-in is not by itself a reason to split — a foundational module is *supposed*
to have many dependents. Splitting purely to lower a number trades one big
dependency for several, multiplies import lines across the tree, and buys
nothing. A split is justified only when the module already contains two or more
genuinely separable concerns that different dependents use independently.

When a split *is* warranted, these are the natural cohesion boundaries (from the
audit):

- **`syntax.ast`** — predicates / builders / spans / eval-helpers. These are four
  distinct concerns: shape tests, construction, source-location handling, and
  compile-time evaluation helpers. A consumer that only inspects node shape need
  not pull in the eval-helper surface.
- **`sequence`** — core vs derived algorithms. A small core (the irreducible
  primitives) versus the derived algorithms expressible in terms of it; most
  dependents lean on the core.
- **`passes.registry`** — ordering vs facts vs diagnostics. Pass scheduling/order,
  the fact/schema store, and pass-finding diagnostics are three responsibilities
  that happen to share a file; they have different consumers and could move apart.

In every case the public module name should remain a facade so the split is
byte-compatible for dependents (facade-first, the same pattern the audit
recommends for the compiler monoliths). Do not split unless one of these
boundaries is real for your change.

## Why hubs accumulate helpers — and the rule that stops it

Hubs attract helpers by gravity: when a new helper is "needed by several
modules," the hub already imported everywhere is the path of least resistance,
so it lands there by default. Repeated, this is how a focused foundational module
slowly turns into a kitchen sink — and every helper added to a hub inherits the
hub's full blast radius, whether it needs it or not.

The rule:

> A new helper belongs in the **smallest module that owns its concern**, not in
> the hub by default. Add it to the hub only when the hub *is* that smallest
> owner.

If two unrelated modules both want a helper, that is a signal to create (or find)
the small shared module that owns it — not to widen a hub. Keeping helpers at
their natural owner is also what keeps the cohesion boundaries above real:
boundaries only stay splittable if unrelated helpers were not piled onto the hub
in the first place.
