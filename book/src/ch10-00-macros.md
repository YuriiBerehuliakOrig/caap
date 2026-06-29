# Macros and Syntax Values

We now arrive at what makes CAAP *CAAP*: code that writes code. Because a program
is just nested lists, a CAAP program can build and inspect program fragments as
ordinary data, then splice them back in. This chapter covers **macros** and the
**syntax values** they manipulate. The next two chapters build on it.

## The Idea

A normal function receives *values* and returns a *value*. A **macro** receives
*syntax* — unevaluated program fragments — and returns *syntax*, which the
evaluator then expands in the caller's environment. Macros run during expansion,
before the program is checked or evaluated, so they let you add new constructs
that look built-in.

The kernel `macro` special form creates one:

```scheme
(macro (a b) body…)
```

At a call site, the arguments are *quoted* into detached **syntax values** and
bound to the parameters; the body must return syntax; the evaluator expands the
result where the macro was called.

## Syntax Values

A syntax value is an opaque, detached fragment of program structure (internally
an `ExprSpec`). There are three constructors and matching inspectors in the
kernel:

| Construct | Inspect | Kind |
|---|---|---|
| `(syntax_name "x")` | `syntax_name_identifier` | `"name"` — a reference to `x` |
| `(syntax_literal v)` | `syntax_literal_value` | `"literal"` — a constant |
| `(syntax_call callee args)` | `syntax_call_callee`, `syntax_call_args` | `"call"` — an application |

`syntax_kind` tells you which of the three you have. With these you can take a
fragment apart and build a new one:

```scheme
; build the syntax for (int_add x 1)
(syntax_call (syntax_name "int_add")
  (list_of (syntax_name "x") (syntax_literal 1)))
```

Because syntax values share the `ExprSpec` representation used by the
compile-time IR builders (Chapter 11), the fragment you build by hand and the
fragment the compiler builds are the same kind of thing.

## The Tower's Friendlier Helpers

Writing `syntax_call`/`syntax_name`/`syntax_literal` by hand is verbose, so the
standard library's `stdlib.syntax.ast` module wraps them with short names —
and adds the pieces you need for real metaprogramming:

| Helper | Role |
|---|---|
| `sym` | a name fragment (`syntax_name`) |
| `lit` | a literal fragment (`syntax_literal`) |
| `calln` | a call fragment by callee name |
| `lam` | a lambda fragment |
| `arg` | read the *nth* argument syntax of a call node |
| `name_of` | lift the identifier out of a name fragment (or `null`) |
| `eval_ir` | turn a built fragment into a callable value |
| `render` | pretty-print a fragment back to source text |

`eval_ir` is the bridge from *syntax* back to a *running function*: you build a
lambda fragment and `eval_ir` compiles it into something you can call. `render`
goes the other way, turning structure back into readable source — invaluable for
debugging generated code.

## A Real Macro-Like Form: `show!`

The corpus example `derive_print.caap` defines a `show!` form that prints a value
*with the name you wrote*. The hard part is that a value doesn't know the name of
the variable holding it — names live in *syntax*, not in values. So `show!` reads
the call syntax at expand time, lifts the identifier, and rewrites itself into a
runtime call:

```scheme
; the compile-time half of show!  (from derive_print.caap)
((get ex "define_form" null) "show!" 1 1
  (lambda (ctx node)
    (bind ((a (arg node 0)))                 ; the argument's syntax
      (calln "show_with_name"
        (list_of
          (lit (bind ((nm (name_of a)))       ; lift "a" from the syntax…
                 (if (eq nm null) (render a) nm)))
          a)))))                               ; …and pass the value too
```

`define_form` (from the `stdlib.expand` module) registers a *compile-time form*
named `show!` taking one argument. Its implementation receives the call `node`,
pulls argument 0's syntax with `arg`, lifts the written name with `name_of` (or
falls back to `render` for non-name arguments), and emits a call to the runtime
helper `show_with_name`. So `(show! total)` becomes, roughly,
`(show_with_name "total" total)` — the *string* `"total"` came from the program
text, the *value* came from evaluation.

That single form ties together everything in this chapter: reading syntax,
lifting names, and constructing a replacement call. In the next chapter the same
example goes further — `show_with_name` *generates a specialized formatter for the
value's type by building IR at run time*. That's compile-time and run-time
metaprogramming meeting in one feature.

## Quoting and Hygiene

When a `macro`'s arguments are quoted, they are detached from the call site and
re-expanded in the caller's environment, so a macro composes like a function
rather than a blunt textual paste. Prefer building syntax with `sym`/`lit`/`calln`
over string-concatenating source: you're manipulating *structure*, and the tower
renders it to text exactly once, at the end (a discipline the codebase follows
strictly). The result is metaprogramming that survives reformatting and refactors.
