## Forms, Atoms, and Comments

A CAAP source file is a sequence of **forms**. There are only two kinds of form,
and once you can read them you can read any CAAP program.

### Atoms

An *atom* is a single indivisible token. CAAP has these atoms:

| Kind | Examples | Notes |
|------|----------|-------|
| Integer | `0`, `42`, `-7` | 64-bit signed |
| Float | `3.5`, `1e10`, `-0.25` | a number token with `.`, `e`, or `E`; 64-bit IEEE 754 |
| String | `"hi"`, `"line\nbreak"` | JSON-style escapes |
| Boolean | `true`, `false` | |
| Null | `null` | the absence of a value |
| Symbol | `int_add`, `play`, `lo`, `+`, `string->int` | a name |

Symbols are liberal: besides letters, digits, and `_`, a symbol may contain
`+ - * / < > = ! ? $ % & : .`. That's why `int_add`, `<=`-style names, and
dotted names like `sys.io` are all single symbols.

### Lists

A *list* is one or more forms inside parentheses:

```scheme
(int_add 2 3)
```

The first element is the **head**; the rest are arguments. The default meaning of
a list is *application*: call the head with the arguments. `(int_add 2 3)` calls
the `int_add` builtin with `2` and `3`.

Lists nest, and that nesting *is* the structure of your program:

```scheme
(int_add (int_mul 2 3) 4)   ; => 10
```

The inner list `(int_mul 2 3)` evaluates to `6`, then `(int_add 6 4)` evaluates
to `10`. There is no operator precedence to memorise — the parentheses say
exactly what is applied to what.

A few list shapes are special:

- The empty list `()` is the value `null`.
- When the head is a **special form** — `if`, `bind`, `lambda`, `do`, `while`,
  `block`, `leave`, `macro`, `try`, `effect_scope`, `set!`, `and`, `or` — the
  form is *not* a normal call. Special forms control evaluation themselves (for
  example, `if` only evaluates one of its branches). You'll meet each one in
  this chapter and the next.

### Comments

```scheme
; line comments run from a semicolon to end of line
#| block comments
   span multiple lines |#
/* this block-comment style also works */
"after the comments"   ; => after the comments
```

Comments are *trivia*: the reader discards them. Most files in the repository
open with a `;` comment naming the file and what it shows, and this book follows
that style.

### Try It

```scheme
; arith.caap — runnable on the bare kernel
(int_add (int_mul 6 7) (int_sub 10 10))
```

```bash
$ caap arith.caap
42
```

Now that you can read forms, let's give values names.
