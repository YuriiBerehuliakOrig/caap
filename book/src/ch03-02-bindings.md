## Bindings and Mutation

You give a value a name with `bind`. `bind` has two forms that scope
differently, and the difference matters, so we'll cover both carefully. Then
we'll look at the two ways CAAP lets you *change* state: rebinding with `set!`
and reference cells with `ref`.

### The Flat Form: `(bind name value rest…)`

The flat form defines a name into the **current** scope and keeps it visible to
the forms that follow — it's the define-like workhorse of everyday CAAP:

```scheme
; runnable on the bare kernel
(bind x 10
  (bind y 32
    (int_add x y)))     ; => 42
```

Here `x` is in scope for the rest of the enclosing form, and `y` is in scope for
its body. At the top level the trailing body is optional, so a file can read as a
sequence of definitions:

```scheme
(bind greeting "hello")
(bind name "world")
(string_concat_many greeting ", " name)   ; => hello, world
```

Each `bind` adds a name that the following sibling forms can see. This is exactly
how the module examples in the repository define their functions before
exporting them.

### The Paired Form: `(bind ((n v)…) body…)`

The paired form takes a list of `(name value)` pairs and a body. It creates a
**fresh child scope** with all the names available (so the bindings can refer to
one another — `letrec` semantics), and the names **disappear after the body**:

```scheme
; runnable on the bare kernel
(bind ((a 3)
       (b 4))
  (int_add (int_mul a a) (int_mul b b)))    ; => 25
```

Use the paired form for locals that belong to one expression; use the flat form
for definitions that should remain visible to later code. If a `lambda` or
`bind` body has several forms, they're automatically wrapped in a `do`.

### Rebinding with `set!`

A binding's value can be changed in place with `set!`, which updates the
nearest enclosing binding of that name:

```scheme
; runnable on the bare kernel
(bind x 1
  (do
    (set! x 5)
    x))            ; => 5
```

`set!` is the *one* spelling for mutation in source. (Under the hood the frontend
lowers it to an internal assignment builtin; you never call that directly.)

### Reference Cells with `ref`

`set!` changes a *name's* binding. Sometimes you instead want a value you can
*pass around* and mutate through any alias — a first-class mutable box. That's a
**reference cell**:

```scheme
; runnable on the bare kernel
(bind ((counter (ref 0)))
  (do
    (set_ref counter (int_add (deref counter) 1))
    (set_ref counter (int_add (deref counter) 1))
    (deref counter)))     ; => 2
```

- `(ref v)` boxes `v` into a fresh cell.
- `(deref r)` reads the current contents.
- `(set_ref r v)` writes `v` into the cell; every alias sees the change.

Reference equality is *cell identity*, not contents, and the native backend
lowers a `ref` to a pointer — so cells are also how you express
pointer-like, aliasing, mutate-in-place data. We used four of them to drive the
loop in Chapter 2.

> **`set!` vs `ref`.** Reach for `set!` to update a local you own within one
> function. Reach for `ref` when a *value* needs to be shared and mutated across
> calls or stored in a data structure. Chapter 4 returns to references in depth.
