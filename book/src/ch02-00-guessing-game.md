# A Guided Tour: The Number Game

Let's write a complete little program and see a lot of CAAP at once. The classic
first project is a number-guessing game; ours plays *itself* with a binary
search and reports how many guesses it took. This keeps the program a pure
computation, so it runs directly on the bare kernel — no I/O, no tower.

We'll show the whole thing first, run it, then take it apart line by line. Don't
worry about understanding every detail yet; the following chapters cover each
piece. The goal here is to feel the shape of the language.

## The Whole Program

Save this as `number_game.caap`:

```scheme
; number_game.caap — runnable on the bare kernel.
; play binary-searches 1..100 for `secret` and returns the number of guesses.
(bind play
  (lambda (secret)
    (bind ((lo (ref 1)) (hi (ref 100)) (attempts (ref 0)) (found (ref false)))
      (do
        (while (not (deref found))
          (bind ((mid (int_div (int_add (deref lo) (deref hi)) 2)))
            (do
              (set_ref attempts (int_add (deref attempts) 1))
              (if (eq mid secret)
                (set_ref found true)
                (if (lt mid secret)
                  (set_ref lo (int_add mid 1))
                  (set_ref hi (int_sub mid 1)))))))
        (deref attempts)))))

; main: play two rounds and sum the guesses (this is the final value).
(int_add (play 42) (play 7))
```

Run it:

```bash
$ caap number_game.caap
13
```

The secret `42` takes 7 guesses and `7` takes 6, so the program's value — the
last top-level form — is `13`.

## Reading It Top to Bottom

### Defining a function with `bind` and `lambda`

```scheme
(bind play
  (lambda (secret)
    ...))
```

`bind` introduces a name. In its *flat* form, `(bind name value)`, it defines
`name` in the current scope so that following forms can use it — this is the
define-like workhorse you'll use constantly. The value here is a `lambda`: a
function of one parameter, `secret`. (We'll return to the other form of `bind`,
the parenthesised "let" form, in a moment — it's already in this program.)

### Local, mutable state with `ref`

```scheme
(bind ((lo (ref 1)) (hi (ref 100)) (attempts (ref 0)) (found (ref false)))
  ...)
```

This is the *paired* form of `bind`: a list of `(name value)` pairs, then a
body. It introduces four locals at once.

Each value is a `(ref …)` — a **reference cell**, CAAP's first-class mutable box.
`(ref 1)` makes a cell holding `1`. You read a cell with `(deref cell)` and
overwrite it with `(set_ref cell new-value)`. We need cells here because the
search loop updates `lo`, `hi`, `attempts`, and `found` on each iteration.
Plain bindings, by contrast, don't change once set.

### Looping with `while`

```scheme
(while (not (deref found))
  ...)
```

`while` evaluates its body repeatedly as long as the condition is *truthy*. The
condition reads the `found` cell and negates it with `not`, so the loop runs
until we've found the secret.

### Sequencing with `do`

```scheme
(do
  (set_ref attempts (int_add (deref attempts) 1))
  (if ...))
```

`while`'s body is a single form, but we want to do two things each pass: bump the
counter, then branch. `do` runs several forms in order and returns the last
one's value. You'll see `do` wherever a single form slot needs multiple steps.

### Branching with `if`

```scheme
(if (eq mid secret)
  (set_ref found true)
  (if (lt mid secret)
    (set_ref lo (int_add mid 1))
    (set_ref hi (int_sub mid 1))))
```

`if` takes a condition, a then-branch, and an optional else-branch. Nesting a
second `if` in the else position gives a three-way decision: equal, too low, or
too high. `eq` tests equality; `lt` is "less than"; `int_add`/`int_sub` are
integer arithmetic.

### The result

```scheme
(deref attempts)
```

A function's body is its value, and the last form in the `do` is `(deref
attempts)` — the guess count. Back at the top level, the final form

```scheme
(int_add (play 42) (play 7))
```

calls `play` twice and adds the results. *That* value, `13`, is what the program
prints.

## One Language, Many Surfaces

The repository ships the same game written in CAAP's **C-like surface**
(`examples/guess_game.caap`). Its first line switches
surfaces:

```c
(surface stdlib.frontend.clike)
// guess_game.caap — the same game, a different skin.

play (secret i32) i32 = {
  lo i32 = 1;
  hi i32 = 100;
  attempts i32 = 0;
  found i32 = 0;
  while found == 0 {
    mid i32 = (lo + hi) / 2;
    attempts = attempts + 1;
    if mid == secret { found = 1; }
    if mid < secret { lo = mid + 1; }
    if mid > secret { hi = mid - 1; }
  }
  attempts;
}

main () i32 = {
  play (pick_secret ()) + play (7);
}
```

Curly braces, infix `+`, `name type = value` declarations — and yet this is
*exactly the same language*. The C-like surface is a grammar layered on the same
kernel, and `main`'s integer result becomes the program's exit code. We'll build
up to writing surfaces ourselves in Chapter 12.

## What's Next

You've now seen bindings, functions, mutable cells, loops, conditionals, and
arithmetic working together. The next chapter slows down and treats each of
these "common programming concepts" properly.
