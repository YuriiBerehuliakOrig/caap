# caap-peg — Language & Engine Specification

Status: normative for `caap-peg` 0.1. This document specifies the grammar
language, its abstract syntax, the value model, and the operational semantics of
the parsing engine, plus the auxiliary protocols (trivia, layout, left recursion,
memoization, token streams, the Parse Effects Protocol, incremental reuse, and
errors). Where this document and the code disagree, the code is authoritative and
the discrepancy is a bug in one of them.

Companion documents: [`grammar-syntax.md`](grammar-syntax.md) (quick syntax
reference), [`GUIDE.md`](GUIDE.md) (how to use the crate), and the runnable
[`../../peg/examples/`](../../peg/examples).

---

## 1. Overview

caap-peg is a **Parsing Expression Grammar** engine. A *grammar* is an ordered
set of named *rules*; each rule body is a *parsing expression*. Parsing is
deterministic: ordered choice (`/`) commits to the first matching alternative,
and there is no ambiguity. The engine is an **interpreter** over a compiled
expression tree (`PegExpr`) — grammars are data, defined and modified at runtime,
not generated code.

Key properties:

- **Scannerless by default**, with an optional pre-produced **token stream**.
- **Packrat memoization** (optional) bounding worst-case work to linear.
- **Left recursion** support (direct, indirect, mutual) via bounded seed-grow.
- **Host control** via the Parse Effects Protocol (a `ParseDriver`), with no
  semantics baked into the engine.
- **Incremental reuse** across edits, sound on the *examined* read extent.

### 1.1 Notational conventions

- `e`, `e1`, `e2` denote parsing expressions; `s`, the input string (UTF-8).
- A *position* is a byte offset into `s` on a UTF-8 character boundary.
- Matching a sub-expression at position `p` either **succeeds**, yielding a value
  and an end position `q ≥ p`, or **fails** (consuming nothing). PEG matching
  never partially commits within an expression except through `~` (cut).

---

## 2. Lexical structure of the grammar language

A grammar text is a sequence of lines. Each non-blank, non-comment line is one
rule:

```
name              <- expr          # ordinary rule
name(p1, p2)      <- expr          # parametric rule (p1, p2 are parameters)
```

- `<-` or `=` separates the rule head from its body.
- `#` begins a line comment (to end of line).
- Blank lines are ignored.
- Rule and parameter names are identifiers: `[A-Za-z_][A-Za-z0-9_]*`.
- The default start rule is `root`; override with `Grammar::with_start_rule`.

Within a rule body, whitespace separates adjacent expressions (juxtaposition =
sequence) and is otherwise insignificant **except** where *tightness* matters
(§3.4). Note: this layout whitespace is distinct from *input* trivia (§7).

---

## 3. Concrete syntax (rule-body expression language)

### 3.1 Grammar of expressions (EBNF)

```ebnf
expr        = choice ;
choice      = sequence , { "/" , sequence } ;            (* ordered choice *)
sequence    = { prefixed } ;                             (* juxtaposition *)
prefixed    = [ "&" | "!" | "&<" | "!<" | "~" ] , postfixed ;
postfixed   = atom , { "?" | "*" | "+" | "{" , bounds , "}" } ;
bounds      = number | number "," | number "," number | "," number ;
atom        = literal | iliteral | regex | charclass | "." 
            | "(" expr ")"                               (* grouping *)
            | call | builtin | ref | imported_ref
            | parameter | named | semantic ;
named       = ident ":" prefixed ;                       (* name:e binding *)
ref         = ident ;                                    (* rule reference *)
imported_ref= ident "::" ident ;                         (* grammar::rule *)
parameter   = "$" ident ;
call        = ident "(" [ expr { "," expr } ] ")" ;      (* TIGHT '(' — §3.4 *)
literal     = "'" chars "'" | "\"" chars "\"" ;
iliteral    = "i" literal ;                              (* case-insensitive *)
regex       = "/" pattern "/" ;
charclass   = "[" [ "^" ] , { range | shorthand | char } "]" ;
semantic    = "@" ident [ "(" expr ")" ]                 (* action / predicate *)
            | "@?" ident                                 (* predicate *)
            | "@!" ident "(" expr ")" ;                  (* guard *)
```

`builtin` is any `ident "(" … ")"` whose `ident` is a reserved keyword
(§4.2): `prec`, `sep_plus`/`gather`, `interspersed`, `capture`, `expected`,
`no_trivia`/`tight`, `with_trivia`, `island`, `raw_block`, `kw`/`hard_keyword`,
`soft_keyword`, `backref`, `recover`, `tok`, `scope`/`grammar_scope`.

### 3.2 Operator precedence and associativity

From loosest to tightest binding:

1. **Ordered choice** `/` — n-ary, listed lowest-first; first match wins.
2. **Sequence** (juxtaposition) — matches each operand left to right.
3. **Postfix** `?` `*` `+` `{m,n}` — bind to the immediately preceding atom.
4. **Prefix** `&` `!` `&<` `!<` `~` — bind to the following postfixed atom.
5. **Atoms / grouping** `( … )`.

`a b / c d` parses as `(a b) / (c d)`. `a*` binds `*` to `a`, not the sequence.

### 3.3 Escapes

String literals support `\n \r \t \\ \0 \" \'`. Character classes support ranges
(`a-z`), negation (`^`), and shorthands `\d \w \s` (their negations and other
escapes fall back to a regex). Regexes use standard `regex` crate syntax,
compiled **anchored** at the current position.

### 3.4 Tightness rules

Two atoms are recognised only when written **tight** (no whitespace at the
boundary); with whitespace the tokens mean something different:

- **`/regex/`** acts as a sequence element only when tight (`a /re/`). Spaced,
  `a / b` is ordered choice.
- **`name(…)`** is a parametric call (or a builtin form) only when the `(`
  immediately follows the name. With a space, `name` is a plain reference and
  `( … )` is a separate grouped expression: `seq (a b)*` = reference `seq` then a
  repeated group; `seq(a, b)` = a call to rule `seq`.

---

## 4. Abstract syntax (`PegExpr`)

The concrete syntax compiles to a `PegExpr` tree. The variants, grouped:

- **Terminals**: `Literal(String)`, `Regex(CompiledRegex)`, `CharClass`, `Dot`,
  `Newline`, `Indent`, `Dedent`, `HardKeyword`, `SoftKeyword`, `Backref`,
  `Island`, `RawBlock`, `Recover`, `TokenRef`.
- **Combinators**: `Sequence`, `Choice`, `Optional`, `ZeroOrMore`, `OneOrMore`,
  `Repeat{min,max}`, `And`, `Not`, `LookBehind{negative}`, `Cut`, `Eager`,
  `SepOneOrMore`, `Interspersed`, `Precedence{operand,levels}`.
- **Bindings**: `Named{name,expr}`, `Capture{label,expr}`.
- **Trivia**: `NoTrivia`, `WithTrivia{spec,expr}`.
- **References**: `Ref(String)`, `Call{rule,args}`, `Parameter{name}`,
  `ImportedRef{grammar_name,rule_name}`, `GrammarScope{grammar_name,expr}`.
- **Error labelling**: `Expected{message,expr}`.
- **Semantic hooks**: `SemanticAction{name,expr}`, `SemanticPredicate{name}`,
  `SemanticGuard{name,expr}`.
- **Placeholder**: `Invalid(String)` — rule source that failed to parse;
  surfaces as an error when the grammar is compiled for a parse.

Grouping `( … )` is transparent: it produces no node, only affects structure.

---

## 5. Value model

A successful parse yields a `ParseValue`:

| Variant | Meaning |
|---|---|
| `Nil` | a position-only match (lookahead, cut, trivia, empty sequence) |
| `Text(Arc<str>)` | matched source text |
| `Number(i64)` | an integer — **never produced by the engine**; only by host actions |
| `Node(tag, children)` | a tagged node with child values |
| `Named(name, value)` | a named binding from `name:e` |
| `SpannedValue{value,start,end}` | a value decorated with a byte span |

### 5.1 Value produced by each construct

| Construct | Value |
|---|---|
| `Literal`, `Regex`, `CharClass`, `Dot`, `HardKeyword`, `SoftKeyword`, `Backref`, `Island`, `RawBlock`, `Newline` | `Text` of the matched slice |
| `Recover` | `Node("<recovered>", [Text(skipped)])` |
| `TokenRef` | `Text` of the matched token's text |
| `Sequence` | 0 elements → `Nil`; 1 → the element's value (passthrough); ≥2 → `Node("sequence", […])` |
| `Choice` | the matched alternative's value |
| `Optional` | the inner value, or `Nil` if absent |
| `ZeroOrMore` | `Node("zero_or_more", […])` |
| `OneOrMore` | `Node("one_or_more", […])` |
| `Repeat` | `Node("repeat", […])` |
| `SepOneOrMore` (`sep_plus`/`gather`) | `Node("sep_one_or_more", [elem, elem, …])` — separators dropped |
| `Interspersed` | `Node("interspersed", [elem, sep, elem, …])` — separators kept |
| `Precedence` (`prec`) | `Node("binop",[lhs,op,rhs])` / `Node("unary_prefix",[op,operand])` / `Node("unary_postfix",[operand,op])`, nested per associativity |
| `Named` | `Named(name, inner)` |
| `Capture` | `SpannedValue{value: inner, start, end}` |
| `And`, `Not`, `LookBehind`, `Cut`, bare `SemanticPredicate` | `Nil` (no consumption) |
| `Eager`, `NoTrivia`, `WithTrivia`, `Expected`, `SemanticGuard` | the inner value |
| `SemanticAction` | the driver-returned value (`Accept(v)`), else the inner value |
| `Ref`, `Call`, `ImportedRef`, `GrammarScope` | the referenced rule's value |
| `Parameter` | the bound argument expression's value |

`return_spans` (config / `ParseRequest::spans`) additionally wraps the **root**
result in a `SpannedValue` over the whole consumed range.

---

## 6. Operational semantics (matching)

Let `match(e, p)` → `Success(value, q)` | `Failure`. Trivia is skipped before a
terminal attempt (§7); positions below are post-trivia where relevant.

- **Literal `"t"`**: succeeds iff `s[p..]` starts with `t`; `q = p + |t|`,
  value `Text(t)`. Else fails.
- **Regex `/re/`**: `re` is anchored; succeeds iff it matches a prefix of
  `s[p..]`; `q = p + len`, value `Text(matched)`.
- **CharClass `[…]`**: matches exactly one character per the class; negation and
  ranges as written.
- **Dot `.`**: matches any one character (fails at EOF).
- **Sequence `e1 e2 … en`**: match each in order, threading the position; the
  sequence succeeds at the final `q` with the combined value (§5.1). If any `ei`
  fails, the sequence fails (unless a preceding `~` cut fired — §6.1).
- **Choice `e1 / … / en`**: try alternatives left to right at the same `p`;
  the first success is the result. If all fail, the choice fails. **First match
  wins** — no longest-match, no backtracking into an already-succeeded
  alternative.
- **Optional `e?`**: `match(e,p)` if success else `Success(Nil, p)`.
- **`e*` / `e+`**: greedily match `e` repeatedly; `*` allows zero, `+` requires
  ≥1. A zero-width iteration terminates the loop (no infinite repetition).
- **`e{m}` / `e{m,}` / `e{m,n}`**: as `*`/`+` but bounded; fails if fewer than
  `m` matched; stops at `n` (or unbounded).
- **And `&e`**: match `e`; on success return `Success(Nil, p)` (no input
  consumed); on failure, fail. **Positive lookahead.**
- **Not `!e`**: invert: success of `e` → failure; failure of `e` →
  `Success(Nil, p)`. **Negative lookahead.**
- **LookBehind `&<e` / `!<e`**: assert `e` matches a suffix ending exactly at
  `p`; consumes nothing; `!<` inverts.
- **Sequence with cut `~`**: `~` succeeds with `Nil` and **commits** the
  enclosing ordered choice: a later failure in the same sequence becomes fatal to
  the choice (it will not try further alternatives). Used to turn "try the next
  alternative" into "this is definitely the right alternative; report its error".
- **Eager `!!e`**: match `e`; escalate its failure to a hard parse error rather
  than a recoverable failure.
- **Recover `recover("s", …)`**: skip input up to **and including** the earliest
  sync literal at/after `p`, or to EOF if none; always succeeds with
  `Node("<recovered>", [Text(skipped)])`. Fails only at EOF. Intended as a
  fallback alternative (`stmt <- good / recover(".")`).

### 6.1 Determinism & error frontier

Matching is total and deterministic. On overall failure the engine reports the
**furthest** position reached and the set of expected items recorded there
(`Expected{msg,e}` overrides the label for `e`). The root must consume the entire
input; leftover input is an `incomplete_input` error.

---

## 7. Trivia (inter-token skipping)

Before each terminal match, the engine skips *trivia* per the active **skip
strategy**, selected by the grammar's `__grammar__.trivia` metadata key:

| `trivia` value | strategy |
|---|---|
| **absent (default)** | `"default"` (whitespace + comments) — see below |
| `""` / `"none"` | no skipping |
| `"whitespace"` | ASCII whitespace only |
| `"default"` | whitespace + line comments (`;`) + nested block comments (`#| |#`, `/* */`) |
| any other string | that string compiled as a regex skip pattern |

> The default when no `trivia` key is set is **`"default"`**, not "none": tokens
> are whitespace/comment separated unless you opt out with `"none"`. A consequence
> is that significant whitespace (e.g. matching a lone `\n` as a terminal, or
> trailing whitespace the root must consume) requires `"none"`/`no_trivia` or
> indentation mode (§8).

- **`no_trivia(e)` / `tight(e)`**: disable trivia skipping while matching `e`.
- **`with_trivia("spec", e)`**: match `e` under a *different* skipper (`spec`
  uses the same vocabulary), restored after `e`. Lets one grammar mix lexical
  conventions.

## 8. Layout sensitivity (indentation)

When `__grammar__.indentation` is `true`, the engine tracks an indentation stack:

- **`newline`** matches `\r?\n` / `\r`, layout-aware.
- **`indent`** matches an indentation increase; **`dedent`** a matching decrease.

These let off-side-rule (Python-like) grammars be expressed without an external
lexer.

---

## 9. Rule references and parametricity

- **`Ref(name)`**: evaluate rule `name` at `p`; its value/end become the
  reference's. Pushes a rule frame (for the rule stack and lifecycle effects).
- **`Call{rule, args}`**: bind `rule`'s parameters to the argument expressions,
  then evaluate its body. `$name` (`Parameter`) inside the body re-invokes the
  bound argument expression at the current position.
- **`ImportedRef{grammar, rule}`** (`grammar::rule`) and
  **`GrammarScope{grammar, e}`** (`scope("grammar", e)`): resolve against an
  imported grammar — either an inline import (`Grammar::with_import`) or one
  hydrated from a `GrammarRegistry` attached via `ParseRequest::registry`.

## 10. Left recursion

Direct, indirect, and mutual left recursion are supported via **bounded
seed-grow** (Warth-style head/involved-set split):

1. The left-recursive rule first matches a non-recursive *seed*.
2. The engine re-evaluates the rule, allowing it to consume the seed, and keeps
   the longer result; it repeats until the match stops growing.
3. Growth is gated per strongly-connected component so a mutually-recursive
   peer cannot prune the head's seed.

Left recursion **requires memoization** (`config.memo`); a left-recursive grammar
parsed with memo disabled is rejected.

---

## 11. Memoization & incremental reuse

- **Packrat memo** (per run, when `config.memo`): a rule's result at a position
  is cached so re-entry at the same position is O(1). This bounds total work to
  linear in input × rules.
- **Position cache** (across runs, `ParseRequest::run_incremental` +
  `ParseCache`): rule results persist between parses. On an edit, surviving
  entries are position-shifted; entries whose **examined** interval overlaps the
  edit are dropped.

### 11.1 Soundness invariant (examined read extent)

Each cached entry records not just its matched span `[start, end)` but the
**examined** interval `[read_lo, read_hi) ⊇ [start, end)` — every byte the match
*inspected* (lookahead, lookbehind, trivia, the byte at which a regex automaton
died). An entry may be reused across an edit **only if the edit region is
disjoint from its examined interval**, not merely its matched span. This is the
correctness condition for all incremental reuse, including
`ast_diff::reparse_ast_incremental`.

---

## 12. Token-stream mode

When a token stream is supplied (`ParseRequest::tokens` or `::scan`), `tok(KIND)`
/ `tok(KIND, "text")` / `tok("text")` (`TokenRef`) match a token from the stream.

- A `tok(…)` matches the token whose `start` equals the current position
  (post-trivia). Matching from the middle of a token never happens.
- The stream must be **ordered, non-overlapping, on char boundaries**, and each
  token's `text` must equal `input[start..end]` (validated; a violation is a
  parse error).
- The built-in `Scanner` (§ guide) produces a conforming stream by maximal
  munch.

---

## 13. The Parse Effects Protocol (host control)

The engine raises typed **effects** at decision points; a host **driver**
(`ParseDriver`, attached via `ParseRequest::driver`) answers with a **directive**.
With no driver, every effect's default is `Proceed`, reproducing ordinary PEG —
so the protocol is zero-cost when unused.

### 13.1 Effects (`ParseEffect`)

`RuleEnter{rule,pos}`, `RuleExit{rule,pos,end,value}`, `RuleFail{rule,pos}`,
`ChoiceEnter{rule,alt_count,pos}`, `AltMatched{rule,index,pos,end,value}`,
`Guard{name,pos,end,value}`, `SemanticAction{name,args,pos,end,value}`,
`SemanticPredicate{name,args,pos,end,value}`, `Failed{furthest,expected}`.

> `args` is currently always empty: grammar-level semantic hooks carry a name and
> an inner expression but no scalar arguments. Policy/data lives in the driver.

### 13.2 Directives (`Directive`)

`Proceed` (apply the default), `Accept(value)` (replace the matched value),
`Reject` (discard the match; the enclosing choice continues), `Commit` (commit
the current alternative), `Restrict(indices)` (limit a choice's candidate set),
`Fail(message)` (fail the whole parse with a message).

### 13.3 Semantic-hook expressions

- **`@name(e)`** (`SemanticAction`): match `e`, raise `SemanticAction`; the host
  returns `Accept(v)` to transform, else the inner value passes through.
- **`@?name`** / **`@name`** (`SemanticPredicate`): zero-width; `Proceed`
  accepts, `Reject` fails.
- **`@!name(e)`** (`SemanticGuard`): match `e`, then the host accepts /
  rejects (backtrack) / commits / fails it.

### 13.4 Transactional state, memo facet, sub-parse

A `ParseDriver` may additionally: keep host state consistent across PEG
backtracking via `checkpoint`/`rollback`/`commit`; declare per-rule memo
soundness via `memo_facet` (so context-sensitive host state does not poison the
packrat cache); and run an isolated `ParseView::sub_parse(rule, pos)`.

---

## 14. Error model

`ParseError { code, message, span:{start,end}, expected, found, line, col,
rule_stack }`. Codes include `parse_failed`, `incomplete_input`,
`step_budget_exhausted`, `memo_budget_exhausted`, and grammar-construction codes
(`raw_block_identical_delimiters`, `invalid_import_metadata`, `import_cycle`, …).
The reported position is the furthest reached; `expected` lists the items the
parser was looking for there.

The **error-tolerant** entry `parse_ast_tolerant` never errors: it returns a
best-effort `AstNode` tree with synthetic `<error>` nodes over unmatched input.

---

## 15. Limits & configuration

`ParserConfig`: `return_spans`, `memo`, `max_steps` (caps both input size and the
expression-step budget), `max_depth` (expression-nesting recursion limit, default
`1024`), `include_invalid_rules` / `invalid_rule_prefixes` (filter rules whose
names match a prefix, default `["invalid_"]`), `memo_policy` (global memo-entry
budget), `output_mode` (`Value` | `Ast`).

### 15.1 Recursion-depth guard

The recursive-descent engine bounds its expression-evaluation nesting by
`config.max_depth` (default `1024`; cf. serde_json's default of 128). Input
nested beyond the limit fails with a `recursion_limit` `ParseError` rather than
overflowing the stack — so deeply-nested untrusted input is rejected cleanly, not
a denial-of-service. Raise `max_depth` (and, for genuinely deep data, the thread
stack via `RUST_MIN_STACK`) when a workload needs deeper nesting.

---

## 16. Conformance invariants

A conforming implementation must uphold:

1. **Determinism**: ordered choice is first-match; no result depends on
   evaluation order beyond what §6 specifies.
2. **PEG non-ambiguity**: a successful parse has exactly one value.
3. **Whole-input consumption** for root parses (else `incomplete_input`).
4. **Memo transparency**: a memo hit yields the identical value/end a fresh
   evaluation would (the packrat invariant), subject to the `memo_facet`
   declarations.
5. **Incremental soundness**: a reused cached entry is indistinguishable from a
   fresh parse of the unchanged region (§11.1).
6. **Protocol inertness**: with no driver attached, parsing is ordinary PEG.
7. **Panic-safety on input**: every entry point returns `Ok`/`Err` for arbitrary
   byte input and never panics, including deeply-nested input (bounded by the
   `max_depth` recursion guard, §15.1).
