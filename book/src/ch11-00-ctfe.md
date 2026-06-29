# Compile-Time Evaluation (CTFE)

This is the chapter the whole book has been building toward. **Compile-time
evaluation** is CAAP running CAAP *during compilation*, with access to the
program's own intermediate representation (IR). It is how the standard library
delivers a type checker, an effect checker, `const` folding, and derive-style
code generation — not as privileged compiler internals, but as ordinary CAAP
registered with the compiler.

## The Compiler Is a Value

During a build there is an implicit object called `compiler`: a *bridge* to the
compilation in progress. Bootstrap code uses it to publish and look up values in
a global registry, and to wire up the pipeline:

```scheme
(ctfe_compiler_register_value compiler "my.thing" some-value)
(ctfe_compiler_lookup_value   compiler "my.thing" default)
```

Everything the tower adds — forms, types, passes — is registered through this
bridge while the bootstrap runs. "Bringing up the tower" *is* a sequence of CTFE
registrations.

## `const`: Folding at Compile Time

The gentlest taste of CTFE is `const`, which evaluates an expression while
compiling and bakes the result into the program — but only when it can prove the
expression is **pure** (Chapter 9):

```scheme
(use stdlib.examples.eff_lib inc)
(bind answer (const (inc 41)))   ; becomes 42 before the program ever runs
```

The fold is safe precisely because `inc`'s purity arrived with its signature.
`const` turns work you'd otherwise pay for at run time into a value computed
once, at build time.

## Providers and Stages: the Pipeline Is Data

Recall the load pipeline:

```text
read -> expand -> semantic check -> type/effect check -> eval
```

Those checks are **stages**, and the work inside them is done by **providers** —
compile-time functions registered against a stage:

```scheme
(ctfe_compiler_stage_register    compiler "my-stage" …)
(ctfe_compiler_provider_register compiler "my-pass" "my-stage" impl requires effects spec)
```

A provider's `impl` is a callback. Whole-unit passes take `(lambda (ctx root) …)`;
passes that don't need the root take `(lambda (ctx) …)`. The callback is
**context-first**: it receives an opaque provider context `ctx` and works
through it, so its effects stay explicit and tracked.

### What a Provider Can Do

Through `ctx`, a provider can:

- **Declare and require effects.** `(ctfe_provider_require_effect ctx 'write_ir)`
  asserts the pass declared an effect; the registration's `effects` list is its
  contract, mirroring Chapter 9. A pass that mutates IR must declare `write_ir`;
  one that emits diagnostics must declare `emit_diagnostics`.
- **Walk the IR.** `ctfe_provider_traversal_walk` visits nodes in pre- or
  post-order, with modes for plain walking, `find_first`, `filter`, and stateful
  folds.
- **Rewrite the IR.** `ctfe_provider_node_replace` swaps a node's subtree for a
  built `ExprSpec`; `ctfe_provider_node_rewrite` does declarative match-and-
  replace; `ctfe_provider_node_erase` removes a node.
- **Report problems.** The `ctfe_provider_diagnostics_{error,warning,note,hint}`
  family emits located diagnostics with codes — the same diagnostics you saw in
  Chapter 6, now produced *by your pass*. An `error` halts the provider.

A lint that forbids some pattern is, in outline, a provider that walks the unit,
matches the offending node shape, and calls `ctfe_provider_diagnostics_error`
with a helpful message and code. A desugaring is a provider that walks and calls
`node_rewrite`. There is nothing the built-in passes can do that your pass can't.

## Inspecting and Building IR

Providers reason over IR **nodes**: you can ask a node its kind and structure,
read a call's callee and arguments, read names and literals, and inspect resolved
bindings — the `ctfe_ir_*`/`ctfe_node_*` families in the reference. To *produce*
IR you build `ExprSpec` values — the very same syntax values from Chapter 10 —
and either splice them in (`node_replace`) or compile them to a callable
(`eval_ir`).

The `derive_print.caap` example shows this end to end. Its `show` helper, given a
struct value, **generates a formatter specialised to that struct's type** by
building a lambda's IR from the type's field list and compiling it with
`eval_ir`:

```scheme
; build_formatter (sketch): assemble the IR for a per-type printer, then eval_ir it
(bind ((spec (lam (list_of "s")
               (calln "string_concat_many" args))))   ; args built from the fields
  (assoc (map_of) "fn" (eval_ir spec) "spec" spec))
```

The first time a `Point` is shown, CAAP *codegens* a printer for `Point`,
compiles it, and caches it; the next `Point` reuses it; a different struct gets
its own. The generated IR is literally a `(lambda (s) (string_concat_many …))`
built from the registry's field names and types. Inspecting the type, building
the IR, compiling it, caching per type — all at run time, all from CAAP.

## Annotations and Facts: How Passes Communicate

Passes need to record conclusions and read each other's. CAAP gives two stores:

- **Annotations** — per-node key/value pairs attached to an IR node
  (`ctfe_meta_annotation_get`/`_set`, or the effect-tracked
  `ctfe_provider_annotation_*` inside a provider, gated by `read_attributes` /
  `write_attributes`).
- **Facts** — semantic values in the unit's *fact table*, keyed by a schema
  namespace plus a node (`ctfe_provider_fact_get`/`_set`, gated by `read_facts` /
  `write_facts`). Fact *schemas* are registered up front, and facts are
  versioned (a deleted fact stays visible to older-version queries and can be
  revived).

The type and effect checkers are exactly this: passes that compute signatures and
effect sets and store them as facts on names, so later passes — and importing
modules — can read them. The "signatures live at names and cross module
boundaries" property from Chapters 8 and 9 *is* the fact table at work.

## Partial Evaluation

CAAP treats a function that can run at *either* compile time or run time as one
mechanism rather than two. With purity established, the compiler may **partially
evaluate** — fold the parts of a computation whose inputs are known at build
time and leave the rest for run time. `const` is the explicit, all-or-nothing
door to this; the fold policy is the general mechanism underneath. The core
provides the substrate; the standard library sets the policy for when folding is
allowed.

## Why This Matters

In most ecosystems, extending the compiler means forking it in another language.
In CAAP, your lint, your derive, your domain-specific optimisation, and your new
checking rule are all ordinary CAAP, registered as providers, running in the same
sandboxed, effect-checked, budgeted environment as everything else (Chapters 6
and 9). The compiler is not a wall around the language — it's part of it.

The full `ctfe_*` API is large; this chapter taught the model. Use the [kernel
reference](appendix-04-further-reading.md) sections 8–18 when you write a pass for
real. Next: changing not what the compiler *does*, but the *syntax* it reads.
