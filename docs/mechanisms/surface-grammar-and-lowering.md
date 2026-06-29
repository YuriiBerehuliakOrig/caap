# Surface Grammar & Lowering

**Source:** PEG engine in the [peg crate](../../peg/src/); kernel reader grammar
in [frontend/grammar.rs](../../caap/src/frontend/grammar.rs); segmental reader
directives in [frontend/reader.rs](../../caap/src/frontend/reader.rs); grammar
builtins in [grammar/](../../caap/src/builtins/grammar/mod.rs) /
[grammar_builder.rs](../../caap/src/builtins/grammar_builder.rs); stdlib
surface kit in [stdlib/frontend/surface.caap](../../stdlib/frontend/surface.caap).
See also [caap-spec.md](../caap-spec.md).

CAAP lets a program define its **own surface syntax** — a custom concrete syntax
that is parsed by a PEG grammar and **lowered** to canonical CAAP IR. The core
parser bakes in no language-specific syntax; surface grammars are data.

## The PEG combinator surface

Grammars are built from compile-time PEG combinators (all `compile_time_pure`,
defined in [grammar_builder.rs](../../caap/src/builtins/grammar_builder.rs)).
Each takes/returns a PEG expression value:

| Builtin | Combinator |
|---|---|
| `ctfe_peg_lit` | literal string match |
| `ctfe_peg_regex` | regex match |
| `ctfe_peg_char_class` | character class |
| `ctfe_peg_ref` / `ctfe_peg_imported_ref` | reference another rule |
| `ctfe_peg_seq` | sequence |
| `ctfe_peg_choice` | ordered choice |
| `ctfe_peg_star` / `ctfe_peg_plus` / `ctfe_peg_opt` | repetition / optional |
| `ctfe_peg_and` / `ctfe_peg_not` | lookahead predicates |
| `ctfe_peg_action` / `ctfe_peg_transform` | semantic action / transform |
| `ctfe_peg_call` | parametric rule application |
| `ctfe_peg_island` / `ctfe_peg_raw_block` | island / raw-block parsing |
| `ctfe_peg_token_ref` | token reference (token-mode) |
| `ctfe_peg_behavior*` | rule behavior: predicate / trace / transform / diagnostic |

A grammar is assembled with the builder family `ctfe_peg_builder`,
`ctfe_peg_builder_rule`, `ctfe_peg_builder_parametric_rule`,
`ctfe_peg_builder_import`, `ctfe_peg_builder_build`, and inspected with
`ctfe_grammar_describe` / `ctfe_grammar_analyze` / `ctfe_grammar_conflicts`.
Lower-level grammar operations: `ctfe_grammar_new`, `ctfe_grammar_extend`,
`ctfe_grammar_set_start`, `ctfe_grammar_rule_get`, `ctfe_grammar_parse`,
`ctfe_grammar_parse_tokens`. Lexing: `ctfe_lexer_tokenize`, `ctfe_lex_token`.

## Syntax authoring (the textual DSL)

Grammars are most often written in a small authoring DSL rather than raw
combinators. `apply_authoring_grammar_source`
([syntax_authoring.rs](../../caap/src/syntax_authoring.rs)) parses lines like:

```
add rule c_ident = /[A-Za-z_][A-Za-z0-9_.-]*/
add rule c_value_form = "auto" name:c_ident "=" value:c_expr ";"
replace rule form = c_value_form
```

Supported directives are `add`, `replace`, `set`, and `include_grammar`
([syntax_authoring.rs](../../caap/src/syntax_authoring.rs)). The rules
accumulate in a `UnitSyntaxState`
([surface_syntax.rs](../../caap/src/surface_syntax.rs)).

`set <key> = "<value>"` stores grammar-level metadata. The canonical key is
`comment` — the grammar's line-comment convention:

```
set comment = "//"     line comments run from `//` to end of line
set comment = none     no comment syntax at all (whitespace-only trivia)
```

When the directive is absent the default CAAP convention stays (`;` line
comments — full compatibility with existing grammars). Lower-level metadata
(e.g. raw `trivia`) can still be set through the `set_grammar_metadata` API.

## Trivia (whitespace & comments)

A surface grammar skips **trivia** between tokens. The strategy is chosen by
top-level `trivia` metadata, resolved in
[peg/src/parser_compile.rs](../../peg/src/parser_compile.rs) /
[peg/src/skip.rs](../../peg/src/skip.rs):

- `"none"` — no skipping;
- `"whitespace"` — whitespace only (` \t\r\n`);
- `"default"` — whitespace **plus** `;` line comments and `#|…|#` / `/* */`
  block comments (the markers are `DEFAULT_LINE_COMMENTS` /
  `DEFAULT_BLOCK_COMMENTS` in [peg/src/skip.rs:48](../../peg/src/skip.rs#L48));
- any other string — a custom regex skip pattern.

**Default selection** ([surface_syntax.rs](../../caap/src/surface_syntax.rs),
`surface_grammar_spec_from_syntax_state`): explicit `trivia` metadata always
wins. Next, a `set comment = …` directive: `none` emits `"whitespace"`, a
prefix emits a custom regex skipping whitespace plus `<prefix>…<eol>` line
comments. Without either, the grammar defaults to `"default"` (comment support
like ordinary CAAP files) — **unless** it uses a comment marker (`;`, `#|`,
`/*`) as a literal token (`grammar_uses_comment_marker_token`), in which case
it falls back to `"whitespace"` so the grammar's own punctuation is not
silently eaten.

Trailing trivia at end of input is consumed before the engine's
full-consumption check — a file ending in a comment without a final newline is
complete input ([peg/src/parser_engine/mod.rs](../../peg/src/parser_engine/mod.rs)).

## Surface forms & lowering

A parsed surface tree is a tree of **surface forms** — neutral `kind`/`value`
maps. The metadata fields hold to a strict contract
([surface_syntax.rs](../../caap/src/surface_syntax.rs)):

- `rule` — the name of the **producing rule** (the named rule whose semantic
  action emitted the form; anonymous/passthrough expressions report the nearest
  named rule), never the hook kind;
- `span` — starts at the first byte of the token itself; leading trivia
  (whitespace and the active comment convention) is never included;
- `delimiter` — `"paren"` / `"bracket"` / `"brace"` from the **real** bracket
  literal that opened the list (an explicit grammar arg such as
  `-> surface.list("brace")` wins), and `null` when the form is not
  bracket-delimited — atoms and bare list rules carry `null`, never a
  fabricated `"paren"`;
- repeated capture labels **concatenate left-to-right** (including captures
  inside groups / repetitions / optionals): `items:a ("," items:b)*` collects
  every capture into one `items` sequence, and unlabeled literals are dropped.

The accessors in [syntax.rs](../../caap/src/builtins/syntax.rs) read them:

| Builtin | Reads |
|---|---|
| `syntax_kind` | the form's kind tag |
| `syntax_name` / `syntax_name_identifier` | a name form / its identifier |
| `syntax_literal` / `syntax_literal_value` | a literal form / its value |
| `syntax_call` / `syntax_call_callee` / `syntax_call_args` | a call form / parts |

In the active stdlib path, lower hooks are ordinary kit functions. A file can
open with `(surface stdlib.frontend.clike)`; the loader reads the file text, loads
the kit, calls `lower_program` / `lower_program_at`, and feeds the returned
stdlib expression specs through the same `expand -> check -> typecheck -> eval`
pipeline as default-readable files.

For lower-level authoring, [stdlib/frontend/surface.caap](../../stdlib/frontend/surface.caap)
builds grammar source (`literal`, `regex`, `seq`, `choice`, `entry_rule`,
`set_comment`), applies it to a host unit, parses text through
`ctfe_grammar_parse_forms`, and converts parsed forms with `form_to_spec`.
Constructor helpers live in [stdlib/syntax/ast.caap](../../stdlib/syntax/ast.caap)
and [stdlib/syntax/ir.caap](../../stdlib/syntax/ir.caap), not in the
removed v1 `surface_builder`.

## CTFE-side parsing

`ctfe_grammar_parse`, `ctfe_grammar_parse_tokens`, and
`ctfe_grammar_parse_forms` parse strings with a grammar at compile time. The
resulting surface forms can be unwrapped and rebuilt with the `ctfe_surface_*`
primitives (see [CTFE & surface forms](ctfe-and-surface-forms.md)).
