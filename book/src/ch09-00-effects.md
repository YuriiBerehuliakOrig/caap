# Effects and Capabilities

CAAP tracks not just the *types* a function works with but the *effects* it has —
whether it mutates state that outlives the call, touches the file system, opens a
socket, emits diagnostics. Effects are checked at compile time, and the authority
to perform them is granted as **capabilities**. Together they are what let CAAP
run untrusted code at compile time and still sleep at night.

## Effects Are Tags, Not a Flag

A function isn't simply "pure" or "impure." Its effects are a *set of tags* the
checker infers — and you can *declare* a function's effects, which the checker
then **verifies** as an override.

The crucial subtlety is **ownership**: mutating state a function *freshly
allocated itself* is not an escaping effect, but mutating a *parameter* or a
*free name* is. Compare the two functions from the corpus:

```scheme
; eff_tags.caap
(module stdlib.examples.eff_tags)

; rebuild builds a NEW list and mutates only that — no escaping effect,
; so the `pure` declaration verifies.
(defn rebuild ((xs list)) list pure
  (bind ((out (list_of)))
    (do
      (sequence_each xs (lambda (e) (append out e)))
      out)))

; push mutates its CALLER's list — an escaping mutation it declares as a tag.
(defn push ((xs list) (x int)) list (mutation)
  (do (append xs x) xs))

(export rebuild push)
```

`rebuild` appends to `out`, but `out` was created inside `rebuild`, so from the
outside `rebuild` is pure — and declaring `pure` checks out. `push` appends to
`xs`, which it received from its caller, so the mutation *escapes*; it declares
the `(mutation)` tag, and the checker confirms that's accurate.

Declared effects are *verified*: claim `pure` while secretly mutating a
parameter and you get a compile-time error. There are tags for other effects too
— reading or writing files, using host services, emitting events — and the full
list is in the [kernel reference](appendix-04-further-reading.md). The principle
is always the same: the checker infers the real effect set and confirms your
declaration is honest.

## Why Effects Enable Compile-Time Evaluation

Effects are what make `const` safe. `const` evaluates an expression *at compile
time* — but only if it can prove the expression is pure. Because effect
information rides along with exported signatures, `const` can fold a call to an
*imported* function the moment it knows that function is pure:

```scheme
; eff_use.caap — const folds a call to an imported pure defn.
(module stdlib.examples.eff_use)

(use stdlib.examples.eff_lib inc)

(bind answer (const (inc 41)))   ; folded at load time to 42
(export answer)
```

The settled effect arrived with `inc`'s signature, so the loader's import-aware
evaluator runs the fold at expansion time. We'll dig into compile-time
evaluation in Chapter 11; the point here is that it *rests on* the effect
system.

## Capabilities: Authority to Act

Knowing a function *wants* to write a file is one thing; being *allowed* to is
another. Side effects on the outside world — I/O, file system, processes,
network — are **host services**, and reaching them requires a **capability**.

Capabilities flow from the launcher:

- The bare kernel has **no** ambient authority. That's why `println` is
  `unknown name` there — not because the name is missing, but because no
  capability grants `sys.io`.
- `caap BOOTSTRAP PROGRAM` runs the bootstrap with the `sys` capability, which
  is how the tower brings up `sys.io`, `sys.fs`, and the rest (Chapter 14).

## Restricting Authority with `effect_scope`

`effect_scope` runs a body with the active effect set **replaced** by a list you
specify. Its iron rule: a nested scope may only request a **subset** of what its
parent already has. Untrusted code can *drop* privileges but can never *regain*
them.

```scheme
; run body with NO effects at all — pure-only execution
(effect_scope (list_of)
  body…)
```

`(effect_scope (list_of) …)` is the pure sandbox: the body may compute but may
not touch the world. Combined with the *fatal* allocation and depth budgets from
Chapter 6 — which pierce `try` and cannot be caught — this is what lets the
compiler fold arbitrary user code during a build without that code being able to
escape, exhaust memory, or hang the host.

> **The big picture.** Types say *what* values are; effects say *what code
> does*; capabilities say *what it's allowed to do*; budgets put a hard ceiling
> on resources. The kernel stays tiny and untrusted-by-default, and every
> privilege is something the tower explicitly grants. This is the safety model
> that makes “the compiler runs your code” a feature instead of a footgun.

With types, effects, and capabilities understood, we can finally look at the
feature that ties the whole language together: programming the compiler itself.
