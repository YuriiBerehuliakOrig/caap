# Modules and the Standard Library

From here on we're on the **tower**: programs are loaded by the stdlib loader,
which means modules, the convenience forms, type and effect checking, and the
whole standard library are available. This chapter introduces modules — how CAAP
code is organised, shared, and combined — and tours the standard library you'll
lean on.

Recall from Chapter 1 that the loader directives (`module`, `import`, `use`,
`export`) are interpreted *as a module is loaded*. The examples here come from
the tested corpus in `examples/`.

## A Module

A module is a CAAP file that declares its identity and what it exports:

```scheme
; greet.caap — demonstrates the (export …) directive.
(module stdlib.examples.greet)

(bind secret (lambda () "shh"))
(bind hello  (lambda (name) (string_concat_many "hi " name)))

(export hello)
```

Top-level flat `bind`s share one module scope. Only the names listed in
`(export …)` end up in the module's public export map, so `secret` stays
private. Everything you learned about `bind` and `lambda` in Chapter 3 applies
unchanged — a module is just kernel code with directives around it.

## The Directives

| Directive | Meaning |
|---|---|
| `(module name)` | declares this unit's module identity |
| `(import mod alias)` | binds `mod`'s whole export map under `alias` |
| `(use mod a b …)` | binds selected exports directly into scope |
| `(re_export mod a b …)` | imports selected names *and* re-exports them |
| `(export a b …)` | marks local names as the public contract |

Directive arguments are **names**, not strings. If you omit `(export …)`, the
module's *result* is the value of its last body form — handy for a file that
computes one thing.

### `import`: the whole map under an alias

`import` binds another module's export map as a value, which you index with
`get`:

```scheme
; combined.caap — imports another module.
(module stdlib.examples.combined)

(import stdlib.examples.arith arith)

(bind double (get arith "double" null))   ; reach in by name
(bind quad   (lambda (n) (double (double n))))
...
```

Because an export map is an ordinary map (Chapter 4), there's nothing new to
learn to consume it.

### `use`: selected names directly

`use` is the common case — bring just the names you need into scope:

```scheme
(use stdlib.lib.collections.sequence fold sort_by map reverse join)
(use stdlib.lib.collections.map update! entries)
```

Now `fold`, `map`, `join`, and friends are callable directly, no `get` needed.

## How a Module Loads

When the loader brings a module in, it runs a fixed pipeline:

```text
read  ->  expand forms  ->  semantic check  ->  type/effect check  ->  eval
```

Every pre-eval diagnostic is reported at the top-level source form that caused
it. Modules are resolved by explicit declaration, a `declare_root` naming
convention, or recursive discovery. **Dependency cycles** are allowed only if the
cross-cycle uses are delayed into function bodies — the loader publishes an empty
export map before building a module, so a mutual reference must not be needed at
load time.

## Convenience Forms

The tower's `forms` module defines syntax sugar that expands *before* checking,
so it costs nothing at runtime. You'll use these constantly:

| Form | What it does |
|---|---|
| `cond`, `when`, `unless`, `case` | readable multi-way and one-armed conditionals |
| `if_let`, `when_let` | bind-and-test in one form |
| `for` | iterate a sequence (sugar over `sequence_each`) |
| `->`, `->>` | threading: pipe a value through a series of calls |
| `const` | evaluate at compile time (Chapter 11) |
| `defn`, `struct`, `alias`, `enum`, `union` | typed definitions (Chapter 8) |

For example, `for` makes the accumulate loop from Chapter 4 read directly:

```scheme
(for x (list_of 1 2 3)
  (set_ref total (int_add (deref total) x)))
```

The kernel forms — `while`, `bind`, `if`, `and`, `or`, `ref`, `try`, `block`,
… — are *not* redefined by the tower; the sugar only adds.

## Touring the Standard Library

The library lives under `stdlib/lib`. The modules you'll meet most:

- **`lib.collections.sequence`** — `map`, `filter`, `fold`, `sort_by`,
  `reverse`, `range`, `each`, `join`, `unique`. These wrap the kernel
  `sequence_*` builtins under shorter names.
- **`lib.collections.map`** — `entries`, `update!`, and map helpers.
- **`lib.text.json`** — `json_parse` and rendering (used in Chapter 15).
- **`lib.core.math`** — numeric helpers such as `abs`, `clamp`.
- **`lib.core`**, **`lib.text`**, **`lib.diag`** — core values, text utilities
  (including the `pad_left`/`pad_right` that aren't in the kernel), and
  diagnostics.

A small but complete tower program, drawn from the corpus, combines several of
these — parsing JSON, folding per-key totals, sorting, and joining lines:

```scheme
(use stdlib.lib.text.json json_parse)
(use stdlib.lib.collections.sequence fold sort_by map reverse join)
(use stdlib.lib.collections.map update! entries)

(defn account_totals ((records list)) map
  (fold records (map_of)
    (lambda (acc rec)
      (update! acc (get rec "acct" "?")
        (lambda (old) (int_add old (get rec "amt" 0))) 0))))
```

We'll build this `json_report` program out fully in Chapter 15. But notice it
already uses something new: `defn` with typed parameters (`(records list)`) and
a declared result type (`map`). That's the subject of the next chapter.
