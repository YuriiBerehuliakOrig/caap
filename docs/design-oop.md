# Stdlib OOP Design Note

> Status: historical design note. The active stdlib tree does not expose this as
> a supported public OOP API. Missing mechanisms must remain explicitly missing
> through diagnostics or tests; do not replace them with demo-only shims.

This document records the intended shape for classes, traits, method dispatch,
and eventual native lowering if an OOP layer is revived.

## Thesis

OOP is a layer over structs and registries, not a new kernel kind.

The kernel still has only values, maps/lists, callables, and `Name`/`Literal`/
`Call` IR. A class is a struct-backed type descriptor plus OOP metadata held in
a stdlib registry. Methods, traits, impls, dispatch, and conformance checks are
stdlib policy.

Target model:

- classes with single inheritance;
- traits as interfaces;
- static dispatch by default;
- dynamic dispatch through explicit trait objects and vtables.

## Descriptors

### Class

```text
{
  "kind": "class",
  "key": "class:Name",
  "name": "Name",
  "struct_key": "struct:Name",
  "super": "class:Base" | null,
  "methods": { name -> method-record },
  "traits": ["trait:Shape", ...]
}
```

A class always has a companion struct descriptor that owns data layout.

### Method Record

```text
{
  "name": "area",
  "fn_type": <function-type>,
  "impl": <lambda | node-ref | null>,
  "kind": "static" | "virtual",
  "self_param": 0
}
```

A method is an ordinary CAAP function. The first parameter is `self`.

### Trait

```text
{
  "kind": "trait",
  "key": "trait:Shape",
  "name": "Shape",
  "methods": { name -> function-type },
  "supertraits": ["trait:...", ...]
}
```

### Trait Impl

An impl is keyed by `(class-key, trait-key)` and maps method names to method
records. Conformance requires every trait method, including inherited
supertrait methods, to be present and type-compatible.

### Trait Object

Dynamic dispatch uses a fat value:

```text
{
  "__data__": <struct value>,
  "__trait__": "trait:Shape",
  "__vtable__": <vtable>
}
```

The native target would lower this to a fat pointer or equivalent ABI structure.

## Method Resolution

Single inheritance gives a linear method-resolution order:

```text
class -> super -> super.super -> ...
```

Resolution order:

1. Look in the class method table.
2. Walk the `super` chain.
3. If not found, look in trait impls for the class.

Overrides are legal only when the function type matches the overridden method.

## Subtyping

The OOP registry extends ordinary type compatibility with:

- class-to-superclass compatibility;
- class-to-trait compatibility when the class implements the trait.

This extension should live in an OOP registry wrapper, not in the base equality
predicate, so non-OOP type compatibility remains unchanged.

## Dispatch

Static dispatch:

1. Receiver type is known at compile time.
2. Resolve method record.
3. Call the concrete implementation directly with `(self . args)`.

Dynamic dispatch:

1. `as_dyn obj trait` builds a trait object and vtable.
2. `send dyn name args...` finds the vtable slot.
3. The implementation is invoked indirectly.

Vtable order must be deterministic, usually trait declaration order.

## Native Lowering Target

The intended LLVM building blocks are:

- direct calls for statically resolved methods;
- method symbols such as `Class__method`;
- vtable globals such as `@VT_Class_Trait`;
- `getelementptr`, `load ptr`, and indirect call for dynamic `send`;
- a concrete trait-object ABI, likely `{data*, vtable*}`;
- monomorphized method bodies available as IR, not only runtime lambdas.

These are codegen-policy tasks in stdlib backend layers, not Rust kernel
changes.

## Surface Syntax

The minimal functional surface can be:

```lisp
(define_trait "Shape" ...)
(define_class "Circle" ...)
(impl_trait class trait ...)
(call_method obj "area" args...)
(as_dyn obj trait)
(send dyn "area" args...)
```

Dot syntax such as `obj.method(...)` is optional surface sugar and should lower
to the same explicit forms.

## Operator Overloading

CAAP core has named calls rather than infix operators. Operator overloading is
therefore method dispatch over named operator methods:

- `Add.plus`
- `Vec2_add`
- `Money_plus`

An infix surface grammar may map `+` to either a primitive call or a
type-directed method call, but the grammar should stay thin. Operator semantics
belong in lowering/type data, not in PEG hardcoding.

## Invariants

- A class always has a valid companion struct descriptor.
- `class` and `trait` are not new kernel type kinds.
- Override requires function-type compatibility.
- A trait impl is accepted only when conformance succeeds.
- OOP registry state is unit-scoped fact/policy data.
- Static and dynamic dispatch must agree on method identity.
- Native lowering must use the same checked method-resolution results as eval.
