# `stdlib.semantics.passes`: Load-Time Pass Framework

This package gives user/stdlib code a place in the loader's pre-eval pipeline.
The loader already runs expansion, semantic checks, and type checks. Registered
passes and transforms run with the same located diagnostic protocol and can fail
module load just like built-in phases.

The central module is [`registry.caap`](registry.caap)
(`stdlib.semantics.passes.registry`). It owns registries, the fact store,
ordering helpers, and shared pass-authoring utilities. Concrete passes live in
neighbor files and expose optional `register!` functions.

## Three Granularities

| Goal | Mechanism | Granularity |
| --- | --- | --- |
| Rewrite one head | `define_form` | per-head |
| Rewrite a whole module | `register_transform!` | whole module |
| Analyze and report | `register_pass!` | read-only walk |

### Analysis Passes

An analysis is a function shaped like:

```lisp
(lambda (located sink) ...)
```

`located` is a list of expanded top-level forms with `{node, loc}` data. `sink`
is where the pass appends located findings. If the pass itself fails, the
failure becomes a located finding; a broken pass should fail loudly.

### Transforms

A transform mutates the located form records in place by replacing each record's
`node` with a rewritten tree. The tree rewrite should be pure; the record update
is the only mutation.

Transforms run after expansion and before the check/type gate. The checked tree
is the evaluated tree. A failing transform is a hard error because continuing
would mean evaluating an uncertain partial rewrite.

The loader late-binds the registry by module name. Sessions that never load this
package pay nothing for it.

## Registration

Each pass file normally exposes `register!`. Users opt in explicitly:

```lisp
(use stdlib.semantics.passes.borrow)
((get borrow "register!"))
```

Constructors from `registry.caap`:

| Function | Purpose |
| --- | --- |
| `register_pass! name run` | Append an analysis pass. |
| `register_transform! name run` | Append a transform. |
| `install_pass! name run` | Idempotently replace/register one pass. |
| `install_transform! name run` | Idempotently replace/register one transform. |
| `unregister_pass! name` / `unregister_transform! name` | Remove one entry by name and return `bool`. |
| `clear_passes!` | Clear pass registries and fact store. |

## Ordering

Default behavior is append-order. Passes that consume other passes' output can
declare ordering with `_with!` constructors:

```lisp
(use stdlib.semantics.passes.registry install_pass_with! deps_of)
(install_pass_with! "borrow" borrow_run
  (deps_of (list_of) (list_of) (list_of "alias")))
```

`deps_of after before requires` takes three optional name lists:

| Field | Meaning |
| --- | --- |
| `after` | Soft hint: run after these installed passes. Missing names create no edge. |
| `before` | Soft mirror of `after`. |
| `requires` | Hard dependency: the named pass must be installed. |

The scheduler performs stable topological ordering. With no edges, registration
order is preserved exactly. Missing `requires` and cycles are hard errors before
any pass runs.

## Fact Store

Passes communicate through a session-scoped namespaced fact store. It is cleared
by `clear_passes!`.

### Soft Facts

```lisp
(fact! "myproj.effects" "f" "pure")
(fact_of "myproj.effects" "f" null)
(facts_of "myproj.effects")
```

Soft reads never fail. Misses return the provided default, and type mistakes are
left to consumers.

### Typed Facts

```lisp
(fact_schema! "myproj.recursive" "string" "bool")
(fact_typed! "myproj.recursive" "fib" true)
(fact_typed_of "myproj.recursive" "fib")
(fact_typed? "myproj.recursive" "fib")
```

Typed namespaces must be declared before use. Writes check key/value types, and
required reads fail if the key is absent. Soft readers can still read typed
entries because both APIs use the same underlying store.

`value_type` names are kernel value tags such as `int`, `string`, `bool`, `list`,
and `map`.

## Authoring Helpers

`registry.caap` re-exports shared helpers:

| Helper | Purpose |
| --- | --- |
| `loc_of node fallback` | Node location or fallback string. |
| `finding_at tag at msg` | Build a located finding from an existing location. |
| `finding tag node msg` | Build a located finding from a node. |
| `note! sink tag node msg` | Append a finding to a sink. |
| `ignored? name` | Conventional opt-out for names beginning with `_`. |

## Pass Inventory

All passes are optional unless a caller registers them.

### Analyses

| Pass | File | Finds | Facts/dependencies |
| --- | --- | --- | --- |
| `lint` | `lint.caap` | Unused bindings/params, shadowing, unreachable code, constant conditions. | None. |
| `callgraph` | `callgraph.caap` | Dead definitions and recursion. | Writes `callgraph` and `callgraph.recursive`. |
| `match_check` | `match_check.caap` | Unreachable arms and enum exhaustiveness. | Reads `match_enum` facts from inference. |
| `naming` | `naming.caap` | Boolean-computing functions without `?`. | Reads `fn_effect` facts. |
| `borrow` | `borrow.caap` | Move-after-use and borrow conflicts. | Conceptually after `alias`. |
| `alias` | `alias.caap` | Points-to information for alias-aware ownership. | Before `borrow`. |
| `escape` | `escape.caap` | Borrowed-handle escape through return, closure capture, or bind alias. | Companion to `borrow`. |

### Transforms

| Transform | File | Purpose | Ordering |
| --- | --- | --- | --- |
| `preresolve` | `preresolve.caap` | Infer omitted native generic compile-time args from declared runtime types. | Before `monomorph`. |
| `monomorph` | `monomorph.caap` | Specialize explicit native generic functions. | After `preresolve`; before other native prep transforms. |
| `struct_monomorph` | `struct_monomorph.caap` | Specialize generic structs from concrete type spellings. | After `monomorph`. |
| `constfold` | `constfold.caap` | Fold pure literal calls bottom-up. | Before `peval` if both are installed. |
| `simplify` | `simplify.caap` | Dead branches and capture-safe beta rewriting. | Before `peval` if both are installed. |
| `dce` | `dce.caap` | Remove unused local bindings when safe. | Before `peval` if both are installed. |
| `peval` | `peval.caap` | Run constprop, constfold, simplify, and DCE to fixed point. | Subsumes the standalone folders internally. |
| `pe` | `pe.caap` | Binding-time specialization over `peval`. | Depends on `peval` behavior. |

The dependency edges above describe the intended order when several transforms
are installed together. Many transforms also call lower-level helper functions
directly and do not require the standalone pass to be registered.

### Project Linters

These operate over raw forms, not expanded forms, because the loader consumes
module directives before ordinary passes run.

| Linter | File | Purpose |
| --- | --- | --- |
| `imports` | `imports.caap` | Unused `use`/`import` symbols. |
| `tiers` | `tiers.caap` | Stdlib tower invariant with documented allowlist. |

### Auxiliary Registries

| Module | File | Purpose |
| --- | --- | --- |
| `derive` | `derive.caap` | Runtime derive-generator registry. |

## Tests

`lib/tests/test_passes.caap` covers the fact store, location helpers, and
ordering scheduler. Concrete passes have their own `test_<pass>.caap` files for
pure analysis/transform cores.

Tests use local registry lists where possible so they do not affect every later
module load in the same session.
