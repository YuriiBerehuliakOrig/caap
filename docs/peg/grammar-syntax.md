# caap-peg grammar syntax

A grammar is a list of rules, one per line:

```
name      <- expr
rule(a, b) <- expr        # parametric rule (a, b are parameters)
```

`<-` (or `=`) separates the rule head from its body. `#` starts a line comment.
The default start rule is `root`; set another with `Grammar::with_start_rule`.

The body grammar below is parsed by `RuleTextParser` (see `src/expr.rs`); the
authoritative producer of each form is `peg_expr_to_source`.

## Terminals

| Form | `PegExpr` | Matches |
|------|-----------|---------|
| `"..."` / `'...'` | `Literal` | the exact string (escapes: `\n \r \t \\ \0 \" \'`) |
| `i"..."` / `i'...'` | `Regex` | case-insensitive literal (written tight; compiled to `(?i)`) |
| `.` | `Dot` | any single character |
| `[...]` | `CharClass` | a single-character class matched natively (ranges, `^` negation, `\d \w \s`); bodies it can't represent as plain ranges fall back to `Regex`. `/[...]/ ` stays a regex. |
| `/regex/` | `Regex` | an arbitrary regex, anchored at the current position |
| `newline` | `Newline` | `\r?\n` / `\r` (layout-aware when indentation is on) |
| `indent` / `dedent` | `Indent` / `Dedent` | an indentation increase / matching decrease |

> A `/regex/` only acts as a sequence element when written tight (`a /re/`).
> `/` written spaced (`a / b`) is the ordered-choice operator.

## Structural combinators

| Form | `PegExpr` | Meaning |
|------|-----------|---------|
| `a b c` | `Sequence` | match each in order |
| `a / b / c` | `Choice` | ordered choice; first match wins |
| `e?` | `Optional` | zero or one |
| `e*` | `ZeroOrMore` | zero or more |
| `e+` | `OneOrMore` | one or more |
| `e{m}` / `e{m,}` / `e{m,n}` | `Repeat` | exactly `m` / at least `m` / between `m` and `n` |
| `&e` | `And` | positive lookahead (no input consumed) |
| `!e` | `Not` | negative lookahead (no input consumed) |
| `&<e` / `!<e` | `LookBehind` | positive / negative lookbehind: `e` matches a suffix ending at the current position (no input consumed; short operands) |
| `(e)` | — | grouping |
| `~` | `Cut` | commit: later failure in the sequence is fatal to the choice |
| `!!e` | `Eager` | match `e`, escalating its failure to a hard error |
| `recover("s", …)` | `Recover` | error recovery: skip to and past the earliest sync literal (or end-of-input), succeeding with a `<recovered>` node over the skipped text. Use as a fallback alternative, e.g. `stmt <- good / recover(".")`, so one malformed region is localised instead of failing the whole parse. Fails only at end-of-input. |

## Operator precedence

| Form | `PegExpr` | Meaning |
|------|-----------|---------|
| `prec(operand, infixl(op, …), infixr(op, …), prefix(op, …), postfix(op, …), …)` | `Precedence` | precedence climbing over `operand` with operator levels |

Levels are listed **lowest precedence first**; each level is `infixl`/`infixr`
(left/right-associative infix), `prefix` (unary, e.g. `-a`), or `postfix`
(unary, e.g. `a!`), and the operators within a level share precedence. No left
recursion required. Output: `Node("binop", [lhs, op, rhs])`,
`Node("unary_prefix", [op, operand])`, `Node("unary_postfix", [operand, op])`.
Example: `prec(num, infixl("+", "-"), infixl("*", "/"), infixr("^"))` parses
`1+2*3^2` as `1 + (2 * (3 ^ 2))`.

## Repetition with separators

| Form | `PegExpr` | Output |
|------|-----------|--------|
| `sep_plus(e, s)` / `gather(e, s)` | `SepOneOrMore` | `e (s e)*`, separators dropped |
| `interspersed(e, s)` | `Interspersed` | `e (s e)*`, separators kept in the node |

## Value bindings & captures

| Form | `PegExpr` | Effect |
|------|-----------|--------|
| `name:e` | `Named` | wrap the matched value as a named binding |
| `capture("label", e)` | `Capture` | wrap the value in a `SpannedValue` |
| `backref("name")` | `Backref` | match text equal to the most recent `name:` binding's captured token (context-sensitive: heredocs, matched tags) |

## Trivia control

| Form | `PegExpr` | Effect |
|------|-----------|--------|
| `no_trivia(e)` / `tight(e)` | `NoTrivia` | disable trivia skipping while matching `e` |
| `with_trivia("spec", e)` | `WithTrivia` | match `e` under a different trivia skipper, scoped to `e` (restored after). `spec` uses the `__grammar__.trivia` vocabulary: `"none"`, `"whitespace"`, `"default"`, or a regex. Lets one grammar mix lexical conventions (e.g. a `;`-significant region inside a `;`-comment grammar). |

## Error labelling

| Form | `PegExpr` | Effect |
|------|-----------|--------|
| `expected("msg", e)` | `Expected` | replace the failure label for `e` with `msg` |

## Delimiter-bounded text

| Form | `PegExpr` | Matches |
|------|-----------|---------|
| `island("s", "e")` / `island("s", "e", true)` | `Island` | text between `s` and `e` (no nesting); `true` keeps delimiters |
| `raw_block("s", "e", "kind")` | `RawBlock` | nested balanced delimiters (`s` ≠ `e` required) |

## Keywords

| Form | `PegExpr` | Matches |
|------|-----------|---------|
| `kw("word")` / `hard_keyword("word")` | `HardKeyword` | `word` not followed by an identifier char |
| `soft_keyword("word")` | `SoftKeyword` | currently identical to `kw`; see the variant docs |

## Rules, parameters & cross-grammar references

| Form | `PegExpr` | Meaning |
|------|-----------|---------|
| `name` | `Ref` | reference another rule |
| `rule(arg1, arg2)` | `Call` | call a parametric rule with argument expressions |
| `$name` | `Parameter` | reference a parameter inside a parametric rule body |
| `grammar::rule` | `ImportedRef` | reference a rule in an imported grammar |
| `scope("grammar", e)` / `grammar_scope(...)` | `GrammarScope` | evaluate `e` against an imported grammar |

> A `(` is a call (or a builtin form like `prec(…)`) only when written **tight**
> — directly after the name, no space. With a space, `name` is a plain `Ref` and
> `( … )` is a separate grouped expression: `seq (a b)*` is a reference `seq`
> followed by a repeated group, whereas `seq(a, b)` calls the rule `seq`. (Same
> tightness rule as `/regex/`: `a /re/` is a sequence, `a / b` is a choice.)

Imports are attached with `Grammar::with_import` / `add_import`, or resolved
from a `GrammarRegistry` via `ParseRequest::registry`.

## Semantic hooks (require a `SemanticRuntime`)

| Form | `PegExpr` | Effect |
|------|-----------|--------|
| `@action(e)` | `SemanticAction` | transform the matched value via the named host action |
| `@?pred` / `@pred` | `SemanticPredicate` | succeed/fail based on a named host predicate |
| `@!guard(e)` | `SemanticGuard` | match `e`, then let the host driver accept / reject (backtrack) / commit / fail it |

Parse with semantics through `ParseRequest::new(&g).semantic(&runtime).run(text)`.

`@!guard(e)` is part of the **Parse Effects Protocol** — a host control surface
attached with `ParseRequest::new(&g).driver(&driver)`. A `ParseDriver` is asked
at decision points (rule enter/exit, choice enter, alternative matched, guard,
failure) and answers with a `Directive` (`Proceed`, `Accept`, `Reject`,
`Commit`, `Restrict`, `Fail`). The driver may also keep transactional state
across backtracking (`checkpoint`/`rollback`/`commit`), declare per-rule memo
soundness (`memo_facet`), and run isolated `ParseView::sub_parse`s. With no
driver attached the protocol is inert and parsing is ordinary PEG.

## Token-stream terminal

| Form | `PegExpr` | Matches |
|------|-----------|---------|
| `tok(KIND)` / `tok(KIND, "text")` / `tok("text")` | `TokenRef` | a token from a pre-produced stream |

Provide the stream with `ParseRequest::new(&g).tokens(tokens).run(text)`, or let
the **built-in `Scanner`** produce it: `ParseRequest::new(&g).scan(&scanner).run(text)`.
A `Scanner` is an ordered list of token rules (`token(kind, regex)` /
`literal(kind, text)`) plus trivia `skip(regex)` patterns; it lexes by maximal
munch (longest match wins, ties broken by declaration order). An explicit
`tokens(...)` stream takes precedence over an attached scanner.
