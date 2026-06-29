# Segmental Reader And In-Stream Grammar Extension

This document is the architecture contract for CAAP's segmental reader and its
reader directives:

- `extend_syntax`
- `define_grammar`
- `begin_scope`
- `end_scope`

Implementation lives in `caap/src/frontend/{mod.rs,reader.rs}`. See also
[`architecture.md`](architecture.md) and [`principles.md`](principles.md).

## Reading Model

CAAP can read source one top-level form at a time. Between forms, the active
grammar may change. Non-directive forms are collected and then lowered into one
whole-program graph.

```text
read one top-level form with active grammar
  -> directive? yes -> mutate reader state; consume directive
                no  -> append form to program
  -> read next form with current grammar

collected forms -> parsed_source_to_ir -> IRGraph
```

The collected graph remains a whole program. Forward references, mutual
recursion, semantic analysis, and evaluation work the same way after reading.

## `parse_next_form`

```rust
pub fn parse_next_form(
    grammar: &Grammar,
    source: &str,
    pos: usize,
    source_path: Option<&str>,
) -> CaapResult<Option<ReadStep>>;
```

Contract:

- Reads exactly one top-level form from byte offset `pos`.
- Returns `Ok(None)` only when the remaining input is trivia or EOF.
- Returns `Ok(Some(step))` with absolute source spans and the next byte offset.
- Returns `Err` for real syntax errors.
- Uses the same ParsedForm-to-IR projection as whole-file parsing.
- Keeps grammar stable during the parse of that form.

## `parse_segmental`

```rust
pub fn parse_segmental(source: &str) -> CaapResult<IRGraph>;
pub fn parse_segmental_with_source_path(
    source: &str,
    path: impl AsRef<str>,
) -> CaapResult<IRGraph>;
```

Fast-path invariant: if the source contains no directive trigger token,
`parse_segmental` parses the whole file and returns a graph identical to the
ordinary parser. For normal code, segmental reading changes neither behavior nor
cost materially.

## Execution Paths

Both canonical paths use segmental reading:

| Path | Entry | Reading | Evaluation/compile model |
| --- | --- | --- | --- |
| Run | `eval_source`, `caap PROGRAM` | `parse_segmental` | whole-program top-level environment |
| Compile | session/unit source artifact | `parse_segmental` | whole-program semantics and hoisting |

The only thing segmental reading cannot change is evaluation order. Using a
value before its defining form has executed is still an `unknown name` error.

## Soundness And Determinism

- Each form is parsed with a stable grammar.
- Grammar mutation happens only at form boundaries.
- Source without directives is byte-for-byte equivalent to ordinary parsing.
- `parsed_source_to_ir` is the only lowering path from `ParsedForm` to IR.
- Directives are consumed and never appear in the program graph.

## Directive Rules

Shared rules:

- Directives are top-level forms recognized by inspection, not evaluation.
- Arguments are string literals where specified.
- A directive affects only later forms.
- A malformed directive shape is not partially accepted.
- If a form resembles a directive but does not match the exact shape, it remains
  program code and may fail later as an ordinary unknown call.

## `extend_syntax`

```caap
(extend_syntax "rule" "peg-source")
```

Replaces `rule` in the active grammar with `peg-source` using PEG
`replace_rule` semantics. Later forms are read with the updated grammar.

If the active grammar is scoped, the replacement is scoped too and rolls back at
the matching `end_scope`.

Errors:

- invalid PEG source: `cannot replace grammar rule "rule": ...`

Boundary:

- This only changes spelling that still lowers to existing ParsedForm shapes.
  It does not add custom programmatic lowering.

## `define_grammar`

```caap
(define_grammar "name" "rule" "peg-source")
```

Registers or extends a named grammar. The first call clones the base surface
grammar; later calls replace additional rules in the same named grammar.

It does not change the active grammar by itself.

Errors:

- invalid PEG source, reported the same way as `extend_syntax`.

## `begin_scope` And `end_scope`

```caap
(begin_scope "name")
  ...
(end_scope)
```

`begin_scope` pushes the current grammar and activates the named grammar.
`end_scope` pops back to the previous grammar.

Properties:

- Scopes may nest.
- Forms inside the region are read with the scoped grammar.
- `extend_syntax` inside the region is reverted with the scope.

Errors:

- unknown grammar: `begin_scope: unknown grammar "name"`;
- unmatched end: `end_scope without a matching begin_scope`;
- EOF with open scope: `unbalanced begin_scope: missing end_scope`.

## Extension Mechanism

The reader loop is mechanism. Directives are policy objects.

```rust
pub trait ReaderDirective {
    fn trigger_token(&self) -> &'static str;
    fn apply(
        &self,
        form: &ParsedForm,
        state: &mut ReaderState,
    ) -> CaapResult<bool>;
}
```

`ReaderState` owns the active grammar, named grammar registry, and scope stack.
Adding a directive means implementing `ReaderDirective` and passing it to
`read_segmental`; the loop does not need to know the directive's semantics.

## Invariants

1. Source without directive trigger tokens parses exactly like ordinary source.
2. Directives are consumed and do not appear in IR.
3. Directives affect only following forms.
4. Scoped grammars restore exactly on `end_scope`.
5. Collected forms become one whole-program graph.
6. Packrat memoization remains sound because grammar changes only between forms.
7. Unknown grammars, unbalanced scopes, and invalid PEG source are explicit
   errors, not silent fallbacks.

## Deliberate Boundaries

- No read-time eval. Directive arguments are string literals.
- Grammar edits are base-plus-rule-replacement. They introduce new spellings,
  not new IR constructs.
- Fully new syntax with custom lowering belongs to the surface-syntax pipeline:
  unit grammar, semantic hooks, and `transform (lower ...)`.

## Relationship To Other Mechanisms

| Need | Mechanism |
| --- | --- |
| Change parsing of later forms in a file | Reader directives |
| Scope a grammar to a file region | `begin_scope` / `end_scope` |
| Compute over values or IR at compile time | CTFE |
| Transform already parsed syntax lazily | Runtime macros or surface hooks |
| Add new constructs with custom lowering | Full surface-syntax pipeline |

Reader directives are pre-parse. CTFE and macros are post-parse. They do not
replace each other.

## Examples

In-stream spelling extension:

```caap
(extend_syntax "null" "'null' / 'nil' / 'none'")
(eq nil none)
```

Two scoped grammars:

```caap
(define_grammar "a" "null" "'null' / 'nil'")
(define_grammar "b" "null" "'null' / 'none'")

(begin_scope "a")
  (eq nil null)
(end_scope)

(begin_scope "b")
  (eq none null)
(end_scope)
```

## Error Catalog

| Message | Cause |
| --- | --- |
| `cannot replace grammar rule "R": ...` | Invalid PEG source in `extend_syntax` or `define_grammar`. |
| `begin_scope: unknown grammar "N"` | Scope references a grammar that was not defined. |
| `end_scope without a matching begin_scope` | Extra `end_scope`. |
| `unbalanced begin_scope: missing end_scope` | EOF reached with an open scope. |
| `unknown name: X` | Evaluation-order error, not a reader error. |

## Principle Fit

- Minimal kernel: IR and evaluator are unchanged.
- Determinism: no directives means ordinary parsing.
- No silent fallback: directive errors are explicit.
- CTFE separation: reader directives are pre-parse; CTFE is post-parse.
- Policy split: the loop is generic, directives are swappable policy objects.
  The default directives live in Rust because pre-parse behavior cannot be
  implemented by stdlib code without introducing read-time eval.
