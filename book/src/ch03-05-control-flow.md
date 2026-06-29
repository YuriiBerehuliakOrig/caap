## Control Flow

CAAP's kernel control-flow forms are `if`, `and`, `or`, `do`, `while`, `block`,
and `leave`. Because everything is an expression, each of these *returns a
value*.

### `if`

```scheme
(if condition then-form [else-form])
```

`if` evaluates the condition; if it is truthy it evaluates the then-form,
otherwise the else-form (or yields `null` if there is no else). Only one branch
runs.

```scheme
; runnable on the bare kernel
(bind classify
  (lambda (n)
    (if (lt n 0) "negative"
      (if (eq n 0) "zero"
        "positive"))))
(classify -4)     ; => negative
```

Nesting `if` in the else position is how you write a multi-way choice — the same
trick the number game used.

The comparison builtins are `lt`, `gt`, `le`, `ge`, and `eq`:

```scheme
(list_of (lt 2 3) (ge 3 5) (eq 4 4))   ; => [true, false, true]
```

### `and` and `or`

`and` and `or` short-circuit and **return a value**, not just a boolean:

```scheme
(and 1 2 3)            ; => 3   (all truthy → last value)
(or false (and true 7)) ; => 7   (first truthy value)
(and 1 false 3)        ; => false (first falsy value)
```

This makes `or` a handy "default" operator: `(or maybe-value fallback)`.

### `do`

`do` evaluates several forms in order and returns the last one. Use it wherever
a single-form slot needs multiple steps (the body of a `while`, a branch of an
`if`):

```scheme
(do
  (set! total (int_add total 1))
  total)
```

### `while`

`while` repeats its body while the condition is truthy:

```scheme
; runnable on the bare kernel — sum 1..=5
(bind ((acc (ref 0)) (i (ref 1)))
  (do
    (while (le (deref i) 5)
      (do
        (set_ref acc (int_add (deref acc) (deref i)))
        (set_ref i   (int_add (deref i) 1))))
    (deref acc)))     ; => 15
```

A loop needs mutable state to make progress, which is why `ref` cells (or `set!`)
show up alongside `while`.

### `block` and `leave`: Structured Exit

A `block` is a named region you can jump out of with `leave`, carrying a value.
It's CAAP's structured early-return / `break`:

```scheme
; runnable on the bare kernel — first index equal to 3, else -1
(block found
  (do
    (sequence_each (sequence_range 0 100)
      (lambda (i) (if (eq i 3) (leave found i) null)))
    -1))              ; => 3
```

`(leave found i)` immediately unwinds to the `block` labelled `found` and makes
the whole block evaluate to `i`. If the block finishes normally, its value is the
value of its body.

### What's Not in the Kernel

You will see `for`, `when`, and `unless` in tower code (and in this book from
Chapter 7 on). They are *convenience forms provided by the standard library*, not
kernel special forms — try `for` on the bare kernel and you'll get `unknown
name`. On the kernel, `while` + `block`/`leave` and the `sequence_*` higher-order
builtins cover the same ground. Once you're on the tower, the sugar makes loops
read more directly:

```scheme
(for x (list_of 1 2 3)
  (set_ref total (int_add (deref total) x)))
```

That's the kernel. Next we look at the collections that most programs spend
their time manipulating.
