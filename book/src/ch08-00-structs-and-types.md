# Structs and Types

The kernel is untyped: every integer is a 64-bit `int` and nothing checks how
you combine values. The tower adds an optional, name-attached **type layer**.
Types are declared *at the name* of a definition, travel with a module's
exports, and are checked at load time — in the defining module and in every
module that imports it. This chapter covers `defn`, `struct`, and the sized
numeric types.

## Typed Functions with `defn`

`defn` is the tower's function form. Its signature sits right at the name, in the
order *name → parameters → result → body* (the project calls this **NMV** —
name, type, value, the same order a declaration uses):

```scheme
; typed.caap — the signature lives AT the name.
(module stdlib.examples.typed)

(defn add8 ((a u8) (b u8)) u8
  (int_add a b))

(defn shout ((name string)) string
  (string_concat_many "HEY " (string_upcase name)))

(export add8 shout)
```

`add8` declares two `u8` (8-bit unsigned) parameters and a `u8` result. At load
time the checker verifies that the body is consistent with the signature and
that any *literal* arguments are in range — both in this module and in importers.
Pass `add8` a value that can't be a `u8` and you get a compile-time diagnostic,
not a runtime surprise.

Because signatures are stored at names and exported, type and effect information
**crosses module boundaries**: when another module `use`s `add8`, the checker
there knows its signature too.

## Sized Numeric Types

Where the kernel has one `int`, the typed layer has sized integers and floats:
`u8`, `i32`, `i64`, and so on. These matter for two reasons: the loader
**range-checks** sized literals (a `u8` can't be `300`), and the native backend
(Chapter 13) needs exact widths to emit machine code. On the bare kernel these
names don't exist; in `defn` signatures they're how you say what you mean.

## Structs

`struct` declares an aggregate type *at its name* and generates a typed
constructor `make_Type`:

```scheme
; structs.caap — a struct end to end.
(module stdlib.examples.structs)

(struct Point (x i32) (y i32))

; dist2 — squared distance from the origin.
(defn dist2 ((p Point)) int
  (int_add (int_mul (get p "x" 0) (get p "x" 0))
           (int_mul (get p "y" 0) (get p "y" 0))))

(export make_Point dist2)
```

`(struct Point (x i32) (y i32))` does three things: it registers the type
`Point` (its field names and types), it makes `dist2`'s `(p Point)` parameter
mean something to the checker, and it generates `make_Point` so callers can build
one. A struct *value* is a map carrying its fields plus a type tag, so you read
fields with the same `get` you already know — `(get p "x" 0)`.

You export `make_Point` (the generated constructor) alongside your functions so
importers can construct `Point`s.

## Aliases, Enums, and Unions

Three more type forms round out the layer:

- **`alias`** gives an existing type a new name.
- **`enum`** defines named integer constants grouped under an alias-like type —
  the readable way to name a set of small integers.
- **`union`** describes overlapping storage and is *native-only* (Chapter 13),
  for systems code that needs C-style unions.

## How Checking Works

The type layer lives in `stdlib/semantics/types`: a **registry** of type descriptors
(primitive, sized, pointer, struct, alias, enum, generic), and an **inference**
walker that assigns and checks signatures. A few things worth knowing:

- Plain `lambda` bindings and builtin facades can *receive inferred signatures*
  when there's enough certainty — you don't have to annotate everything.
- Branches of an `if` (and arms of `match`) are **joined**: the checker computes
  a type consistent with every branch.
- Checking happens in the load pipeline's *type/effect* stage, before
  evaluation, so type errors stop a module from ever running.

Types are only half of what `defn` can declare. The other half — what a function
is allowed to *do* — is the effect system, and that's next.
