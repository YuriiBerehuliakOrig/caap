# `semantics/types/`: Stdlib Type Layer

`stdlib/semantics/types/` is the tier-4 type and effect policy layer. The kernel
itself remains dynamic; this layer adds descriptors, inert markers, effect
inference, and a load-time walker over expanded AST.

| File | Role | Important concepts |
| --- | --- | --- |
| [`registry`](registry.caap) | Type descriptors and compatibility. | `resolve`, `assignable?`, `elem_type`, constructors, type variables |
| [`records`](records.caap) | The single reader for inert markers. | `sig_marker`, `struct_marker`, `enum_marker`, `union_marker` |
| [`effects`](effects.caap) | Effect inference. | `effect_scan`, `settle_effect!`, owned mutation |
| [`infer`](infer.caap) | Walker and per-module store. | `tc`, `check_module_types`, `check_call_sig`, unification |

## Type Descriptors

A type is a string name. A descriptor is a flat map with a `kind`, a `name`, and
kind-specific fields. Current descriptor families include:

- primitives and sized numeric types;
- structs and aliases;
- enums and unions;
- lists, maps, and pointers;
- type variables.

Parameterized spelling such as `list<int>` or `map<string,list<u8>>` is treated
as applying a type constructor to descriptor arguments. `resolve` instantiates
constructors lazily and memoizes the canonical descriptor.

### Assignability

`assignable? want got` is conservative:

- unknown and `any` pass;
- dynamic `int`/`float` are compatible with sized numeric declarations where
  the runtime value can be checked or the literal fits;
- structs are runtime maps, so struct/map compatibility is structural where
  known;
- typed containers check element/key/value types when both sides carry enough
  information;
- bare kernel containers remain compatible with typed containers because the
  bare side carries no element certainty.

`literal_fits?` performs strict range checks for sized integer literals.

## Type Variables And Generic Signatures

A generic signature may mention type variables in parameter and result
positions:

```lisp
(defn map ((xs (list T)) (f callable)) (list T) ...)
```

`T` is not a registered concrete type. It binds at the call site, based on the
argument type, and is substituted into the result.

Spelling convention:

- one uppercase Latin letter, such as `T`, `K`, `V`, `E`, `A`, or `B`;
- the name must not already be registered as a concrete type.

Helpers:

| Helper | Meaning |
| --- | --- |
| `type_var? name` | True when `name` is an unregistered type variable. |
| `ctor_of name` | Top-level constructor spelling, such as `list` for `list<T>`. |
| `type_args name` | Top-level type arguments. |
| `subst_type name subst` | Recursive type-variable substitution. |

The mechanism is additive: an alias named `T` would make `T` concrete again.

## Call-Site Unification

`check_call_sig` checks a call against a signature and returns an instantiated
result type.

Non-generic path:

1. Check each argument with `check_one_arg`.
2. Return the declared result unchanged.
3. Short-circuit through `sig_has_vars?` so old behavior remains unchanged.

Generic path:

1. `unify` each parameter type against the argument type and bind variables.
2. Re-check each argument against the substituted parameter type.
3. Substitute variables into the result type.
4. Finalize unresolved variables: a bare unresolved variable becomes unknown;
   `list<T>` with unresolved `T` becomes bare `list`.

`unify` only binds and compares variables. Ordinary assignability errors remain
owned by `check_one_arg`, so diagnostics are not duplicated.

## Deliberate A2 Boundary

Implemented A2 is call-site polymorphism, not full HM inference.

Not implemented:

- generic typing for direct kernel builtin facades such as `(bind map sequence_map)`;
- result polymorphism through arbitrary `callable` ranges;
- heterogeneous nested operations such as `get_in` and `assoc_in`;
- body-local inference of generic variables inside a generic function;
- variance and bounded polymorphism.

The chosen slice gives useful `list<T>`/`map<K,V>` precision at call sites
without false positives in dynamic or partially known code.

## Let-Generalization Slice

Plain lambdas can receive fresh type variables for untyped parameters:

```lisp
(bind id (lambda (x) x))
```

Each call instantiates those fresh variables through the same unification path
used by generic `defn` signatures. Multi-parameter lambdas receive one fresh
variable per untyped parameter.

Merged pieces:

- fresh variable generation in a namespace separate from user `T`/`K`/`V`;
- reachability/divergence analysis with bottom type `never`;
- divergence propagation through `throw`, `leave`, `runtime_error`, and calls
  returning `never`.

Rejected piece:

- body-usage sharpening of parameter types.

That sharpening was made sound through conservative whole-body gating, but an
empirical scan found no real benefit in the current codebase. It added load-gate
complexity for no observed bugs caught, so it was removed while keeping the
reachability substrate.

## Markers

Forms such as `defn`, `struct`, `alias`, `enum`, and `union` leave inert tuple
literals next to bindings. `records.caap` is the only module that knows those
layouts.

| Reader | Marker shape | Result |
| --- | --- | --- |
| `sig_marker` | `["::defn_sig" name result effect pn pt ...]` | function signature record |
| `struct_marker` | `["::struct" Name f1 t1 ...]` | struct fields |
| `alias_marker` | `["::alias" name target]` | alias record |
| `enum_marker` | `["::enum" Name backing v0 val0 ...]` | enum variants |
| `union_marker` | `["::union" Name m0 t0 ...]` | union members |

Consumers must use these readers rather than reparsing tuples manually.

## Effects

An effect is a set of tags inferred from a function body.

Sources:

- kernel vocabulary effect metadata;
- signatures of called functions;
- sys facade declarations;
- ownership analysis for local mutation.

Mutation of fresh local state created and returned by the function is not an
external mutation effect. Mutation of parameters or free names is.

Declared effects are verified overrides:

- `pure` means no inferred tags;
- `impure` is a blanket declaration;
- an explicit tag list must cover all confidently inferred tags.

Consumers include the `const` form and any pass that needs purity facts.

## Inference Walker

`tc` both infers expression types and checks call arguments where declarations
exist.

Signature sources:

1. Kernel vocabulary projections.
2. `defn` markers next to bindings.
3. Inferred plain-bind signatures for lambdas and builtin aliases.

Branch joins are conservative: equal names join exactly, mixed numeric families
join to dynamic numeric types, and incompatible dynamic branches become unknown
instead of errors.

Match guard predicates can refine the matched binding type.

`check_module_types` harvests top-level signatures and returns them to the
loader, which registers them for cross-module `use` imports.

All pre-eval phases operate per top-level form, so findings carry the form's
`path:line:col` location, with a fallback for synthetic nodes without spans.
