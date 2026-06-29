# Extending the Syntax

CAAP's parenthesised notation is only one *surface* over the language. The same
program can be written in a C-like syntax, and you can grow the grammar from
*inside* a file or define an entirely new surface with a PEG grammar. This
chapter shows the three levels: switching surfaces, extending the live grammar,
and authoring grammars.

## One Language, Many Surfaces

A file can begin with a surface header:

```scheme
(surface stdlib.frontend.clike)
```

or a named variant, `(surface stdlib.frontend.clike name)`. When the loader sees it,
it reads the file's text, loads the named kit, calls the kit's `lower_program`
(or `lower_program_at`) to turn the text into stdlib expression specs, and feeds
those through the **same** pipeline as ordinary CAAP — expand, check, type, eval,
native-prep. Tooling (the LSP) uses the same path, so editor support works for
surfaces too.

You saw the C-like surface in Chapter 2: the guessing game written with `{ … }`
blocks, infix `+`, and `name type = value` declarations is exactly the same CAAP
underneath. A surface changes *spelling*, never semantics.

## Extending the Live Grammar

You don't need a whole kit to add a little syntax. The kernel's reader is
**segmental** — it reads one top-level form at a time — and recognises a few
*reader directives* that change how the *following* forms are read. They never
become program code; they mutate the reader.

| Directive | Effect |
|---|---|
| `(extend_syntax "rule" "peg")` | replace `rule` in the *live* grammar for the rest of the file/scope |
| `(define_grammar "name" "rule" "peg")` | register a *named* grammar (a base clone with rule overrides) without activating it |
| `(begin_scope "name")` … `(end_scope)` | read the forms in between with the named grammar, then restore |

Here is a complete, **bare-kernel-runnable** example (it's
`surface_grammar.caap` in the corpus), teaching three spellings of `null`:

```scheme
; surface_grammar.caap — runnable on the bare kernel
(extend_syntax "null" "'null' / 'nil' / 'nada'")     ; grow the live grammar

(define_grammar "lax" "null" "'null' / 'none'")       ; a named, inactive grammar

(bind a nil)      ; `nil`  — from the extend_syntax above
(bind b nada)     ; `nada` — likewise

(begin_scope "lax")
(bind c none)     ; `none` — parses as null ONLY inside this scope
(end_scope)

(bind d nil)      ; outside the scope, `nil`/`nada` still parse; `none` would not

(if (and (eq a null) (and (eq b null) (and (eq c null) (eq d null)))) 42 0)
```

```bash
$ caap surface_grammar.caap
42
```

The directives took effect *as the file was read*: after the `extend_syntax`
line, `nil` and `nada` parse as the null literal for the rest of the stream;
inside `begin_scope "lax"`, `none` does too; after `end_scope`, the grammar pops
back. The forms they read still flow through the entire pipeline. This is why
CAAP needs no separate macro preprocessor: the grammar itself is mutable, in
stream.

## Authoring Grammars with PEG

Underneath the directives is a full PEG (Parsing Expression Grammar) engine,
exposed at compile time. You construct a grammar from PEG source, extend it,
analyse it, and parse text with it:

| Primitive | Role |
|---|---|
| `ctfe_grammar_new "src"` | parse PEG source into an opaque grammar |
| `ctfe_grammar_extend g rules` | add/replace rules, returning a new grammar |
| `ctfe_grammar_set_start g name` | choose the start rule |
| `ctfe_grammar_analyze g` / `ctfe_grammar_conflicts g` | static analysis: reachability, left-recursion, overlapping prefixes, nullable repetitions, … |
| `ctfe_grammar_parse text g [opts] [semantics]` | parse, returning `{ok, tree, errors}`; `semantics` supplies per-rule predicate and action closures |
| `ctfe_grammar_parse_forms compiler units text` | one call: merge grammar units and their lower hooks, parse, return lowered surface forms as data |

A grammar's *semantics* — the predicate and action closures attached to rules —
are themselves CAAP, so a surface's lowering rules are written in the same
language they parse. The higher-level `stdlib.frontend.surface` kit packages this:
it builds PEG source, parses arbitrary text with `ctfe_grammar_parse_forms`, and
converts parsed forms into stdlib expression specs — exactly the machinery the
C-like kit is built from.

A note from the engine: left-recursive rules require memoization (disabling it is
rejected), and for large inputs you set `max_steps` and a memo policy explicitly.
Grammar analysis returns structured diagnostics, so a misdesigned grammar is
reported the same way a type error is.

## Building Your Own Surface

Putting the levels together, a new surface is:

1. A **PEG grammar** describing your concrete syntax (`ctfe_grammar_new` /
   `ctfe_grammar_extend`, or authored through `stdlib.frontend.surface`).
2. **Lowering rules** — action closures that turn parsed nodes into stdlib
   expression specs (the same `ExprSpec`/syntax values from Chapter 10).
3. A **kit module** exposing `lower_program`, so a file can select it with
   `(surface your.kit)`.

From there your syntax is a first-class peer of the parenthesised default and the
C-like dialect: same checker, same types, same effects, same backends. The
language didn't grow a special case — you used the general mechanism it already
runs on itself.

The next chapters leave the front end behind and turn finished programs into
running artifacts.
