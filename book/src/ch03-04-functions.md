## Functions

Functions are made with `lambda` and are ordinary values. We met them in the
number game; here's the full story.

### Defining and Calling

```scheme
; runnable on the bare kernel
(bind square (lambda (x) (int_mul x x)))
(square 9)        ; => 81
```

`(lambda (params…) body…)` creates a closure. The first element is the
parameter list; the rest is the body (multiple body forms are wrapped in an
implicit `do`, so the value is the last form). You call a function by putting it
in head position, exactly like a builtin.

A `lambda` can be called inline, without naming it:

```scheme
((lambda (x) (int_mul x x)) 9)   ; => 81
```

Parameters are positional and arity is fixed: a two-parameter lambda must be
called with two arguments. The empty parameter list `()` makes a zero-argument
function (`greet.caap` in the corpus defines `(lambda () "shh")`).

### Closures Capture Their Environment

A `lambda` remembers the bindings in scope where it was created. This is what
makes a function factory work:

```scheme
; runnable on the bare kernel
(bind make_adder (lambda (n) (lambda (x) (int_add x n))))
(bind add10 (make_adder 10))
(add10 5)         ; => 15
```

`make_adder` returns a new function that has *captured* `n`. `add10` is "the
adder where `n` is 10," and calling it with `5` yields `15`.

### Recursion

Because the flat `bind` keeps a name visible to later code — including the body
of the function it names — a function can call itself:

```scheme
; runnable on the bare kernel
(bind fact
  (lambda (n)
    (if (le n 1)
      1
      (int_mul n (fact (int_sub n 1))))))
(fact 5)          ; => 120
```

CAAP grows the call stack on demand, so deep recursion does not overflow a fixed
thread stack; how deep you may recurse is governed by an evaluation budget
(Chapter 6), not by the operating system.

### Functions as Arguments

Since functions are values, you can pass them around. The sequence builtins are
*higher-order* — they take a function:

```scheme
; runnable on the bare kernel
(sequence_map (list_of 1 2 3) (lambda (x) (int_mul x x)))   ; => [1, 4, 9]
(sequence_fold_left (list_of 1 2 3 4) 0
  (lambda (acc x) (int_add acc x)))                          ; => 10
```

We'll use these heavily in Chapter 4.

### Looking Ahead: `defn`

On the standard-library tower you'll write functions with `defn`, which puts a
*typed signature* right at the name:

```scheme
(defn add8 ((a u8) (b u8)) u8
  (int_add a b))
```

That declares two `u8` parameters and a `u8` result, and the loader checks types
and literal ranges at load time — in this module and in any module that imports
it. `defn` is built *on top of* `lambda` by the tower; we cover it properly in
Chapter 8. On the bare kernel, `lambda` is all you have, and all you need.
