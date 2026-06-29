## Data Types

Every value in CAAP has a *type tag* you can ask for with `value_type`:

```scheme
; runnable on the bare kernel
(list_of
  (value_type 42)            ; int
  (value_type 3.5)           ; float
  (value_type "hi")          ; string
  (value_type true)          ; bool
  (value_type null)          ; null
  (value_type (list_of)))    ; list
```

```bash
$ caap types.caap
[int, float, string, bool, null, list]
```

### Scalar Types

- **`int`** — a 64-bit signed integer. Integer literals: `0`, `42`, `-7`.
  Arithmetic (`int_add`, `int_sub`, `int_mul`, `int_div`, …) is *checked*:
  overflow and division by zero raise catchable errors rather than wrapping
  silently.
- **`float`** — a 64-bit IEEE-754 double. A number is a float if it has a `.`,
  `e`, or `E`: `3.5`, `1e10`. Use the `float_*` family (`float_add`,
  `float_div`, …) and convert with `int_to_float` / (float-to-int) helpers.
- **`bool`** — `true` or `false`.
- **`null`** — the absence of a value. It prints as nothing and, as a program
  result, exits `0`.
- **`string`** — immutable UTF-8 text. Covered in its own chapter (Chapter 5).

> **Truthiness.** Conditions in `if`, `while`, `and`, and `or` treat values as
> *truthy* unless they are `false` or `null`. So `0` and `""` are truthy.

### Compound Types

- **`list`** — an ordered, **mutable** sequence. Build with `(list_of …)`;
  it prints as `[a, b, c]`. Lists are the everyday collection.
- **`map`** — a **mutable** key/value table that preserves **insertion order**.
  Build with `(map_of)` and `(assoc m k v …)`; it prints as `{k: v}`.
- **`tuple`** — an immutable, fixed-arity sequence. Tuples mostly arise from the
  language itself (for example, a `lambda`'s parameter list, or the `[a, b]`
  pairs produced by `sequence_zip`). You read them with `get` like any sequence.
- **`ref`** — a shared mutable cell (previous section). `value_type` reports
  `"ref"`.

Chapter 4 covers lists, maps, and references as a group.

### Callables

Functions are values too. `value_type` reports them all as `"callable"`:

- a **closure** made by `lambda`,
- a **builtin** like `int_add`,
- a **macro** (Chapter 10).

You can store callables in variables, pass them to other functions, and return
them — which is exactly what higher-order functions like `sequence_map` rely on.

### A Note on Sized Types

You will see types like `u8`, `i32`, and `i64` in later chapters. Those are
**not** kernel values — they are annotations in the *typed* layer of the standard
library (Chapter 8) and the native backend (Chapter 13), where a `defn` declares
the exact width of its parameters and result and the loader checks them. On the
bare kernel, every integer is a 64-bit `int`.
