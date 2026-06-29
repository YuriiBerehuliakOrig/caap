# Error Handling

CAAP gives you two complementary ways to deal with failure: raising and catching
values with `throw`/`try`, and the *errors-as-data* convention used widely in
the standard library. It also has a hard floor — *fatal* resource errors that no
program can catch — which is what keeps untrusted compile-time code from taking
down the host. This chapter covers all three. The `throw`/`try` examples run on
the bare kernel.

## Raising and Catching: `throw` and `try`

`throw` raises a value. `try` runs a body and, if anything is raised, hands the
value to a `(catch …)` clause:

```scheme
(try body (catch name handler))
```

If the body finishes normally, `try` returns its value and the handler never
runs:

```scheme
; runnable on the bare kernel
(try (int_add 40 2) (catch e -1))   ; => 42
```

If the body raises, the handler runs with `name` bound to the raised value, and
its result becomes the value of the whole `try`:

```scheme
; runnable on the bare kernel
(try (throw 99) (catch e (int_add e 1)))   ; => 100
```

A thrown value is passed through **exactly as thrown** — throw a string and you
catch that string; throw a map and you catch that map:

```scheme
; runnable on the bare kernel
(try (throw "boom") (catch e e))   ; => boom
```

### Runtime Errors Are Catchable Too

`try` also catches ordinary, non-fatal evaluation errors — a division by zero, a
bad parse, an out-of-range operation. These arrive as a **map with `message` and
`category` keys**, so a handler can inspect them:

```scheme
; runnable on the bare kernel
(try (int_div 1 0)
  (catch e (get e "message" "no message")))
; => int_div: division by zero

(try (string_to_int "nope")
  (catch e (get e "message" "?")))
; => string-to-int expects a base-10 integer string
```

This is why the two error shapes differ: a value *you* threw is yours to shape;
an error the evaluator raised is normalised into `{message, category}`.

## Diagnostics

Errors that escape to the top level are printed as **diagnostics** with a stable
code, like:

```text
error[CAAP-RUNTIME-001]: unknown name: println
```

The `CAAP-RUNTIME-001` code is stable and greppable. Compile-time problems
(type errors, effect violations, capability denials) surface the same way during
the tower passes — you'll see those codes in Chapters 8 and 9.

## Errors as Data

Raising is not always the right tool. Much of the standard library prefers to
**return** failure as ordinary data, so a caller decides what to do without a
`try`. A common shape is a *result map* with an `ok` flag:

```scheme
; sketch — the json parser returns a result map
(bind parsed (json_parse source))
(if (get parsed "ok" false)
  (use-it (get parsed "value" null))
  (report (get parsed "error" "?")))
```

The capstone project in Chapter 15 (a JSON report) uses exactly this pattern: a
parse failure rides through as a located error string and is rendered as one line
of output, never crashing the program. Errors-as-data composes well with the
`sequence_*` toolkit and keeps the happy path readable.

> **Rule of thumb.** Use `throw`/`try` for *exceptional* conditions that most
> callers can't handle locally. Use a result map for *expected* failures —
> parsing, lookups, validation — that callers routinely branch on.

## The Hard Floor: Fatal Budget Errors

Some failures are **fatal** and pierce straight through `try`: you cannot catch
them. These exist to make CAAP safe to run *untrusted* code at compile time
(remember, the type checker and macros are CAAP running during your build, and a
program can fold arbitrary code with `const`).

Two budgets enforce the floor:

- **An evaluation-depth budget** bounds recursion and iteration. CAAP grows the
  native stack on demand, so depth is limited by this work policy rather than by
  the operating-system stack — a runaway recursion is stopped cleanly instead of
  segfaulting the host.
- **An allocation budget** caps how much memory evaluated code may allocate
  (64 MiB by default for sandboxed/`effect_scope` code; trusted top-level code is
  unbounded). It's charged at the points where collections grow, so a hostile
  snippet cannot exhaust host memory and abort the process.

When either budget is exceeded the result is a *fatal* error that unwinds past
every `try`. This is deliberate: a sandbox you can `try`-catch your way out of is
not a sandbox. You'll see how to *grant* and *restrict* authority explicitly in
Chapter 9.

With the kernel — forms, data, functions, control flow, collections, strings,
and errors — fully in hand, we can climb onto the standard-library tower.
