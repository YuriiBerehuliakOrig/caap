# How to add a Clike surface feature

The C-like surface (`stdlib.frontend.clike`) is a public facade backed by focused
implementation leaves under `stdlib/frontend/clike/`. It is still a
lexer-as-tokens grammar feeding a hand-written recursive-descent lowerer that
emits **stdlib surface forms** (`defn`/`bind`/`while`/`if`/…). Keep that model:
the grammar produces a flat token stream with nested bracket lists, and the
lowerer owns structure.

A new feature touches **at most four places**:

1. **Lexer / tokens** — `clike/grammar.caap` for `syntax_rule`s; shared token
   helpers live in `clike/tokens.caap`.
2. **Parser / lowerer** — `clike/expr.caap` for expressions, `clike/block.caap`
   for statements, `clike/decl_lower.caap` for declarations, or
   `clike/program.caap` for top-level program/module framing.
3. **Tests** — `stdlib/lib/tests/test_clike.caap` (lowering goldens; the source of
   truth for "what does X desugar to").
4. **LSP** (optional) — `clike/analyze.caap` and `clike/sem_tokens.caap` only if
   the feature needs editor analysis beyond what falls out for free.

## File map

- `clike.caap` is the public facade. Do not add feature logic there.
- `clike/ast.caap` is a small internal AST-constructor shim used by clike leaves.
- `clike/sequence.caap` is a small internal collection-helper shim used by clike leaves.
- `clike/grammar.caap` owns only the lexer/bracket grammar and `clike_units`.
- `clike/tokens.caap` owns token accessors, source spans, `oops`, and splitters.
- `clike/meta.caap` owns generic metadata bags/scanners used by declarations,
  locals, params, and fields.
- `clike/sem_tokens.caap` owns the semantic-token sink and name classification.
- `clike/types.caap` owns structural type spelling (`T*`, `T[N]`, generic type
  args) and the declaration-head resolver that splits `metadata... type
  metadata...` after lexing.
- `clike/expr.caap` owns literals, calls, precedence, casts, pointer/subscript
  reads, assignment helper tables, and the block service map.
- `clike/block.caap` owns statement and block lowering.
- `clike/decls.caap` owns the declaration/attribute registries.
- `clike/decl_lower.caap` owns built-in declarations and `classify_named`.
- `clike/program.caap` owns `lower_program_at` and module/use/export/main framing.

## The ONE rule that will bite you: native-safety

The lowerer emits **only heads that BOTH the evaluator and the native LLVM/WASM
backend implement** (the eval=native invariant). The native backend specifically
does **not** have: `block`/`leave`, short-circuit `and`/`or`/`not`, and it
requires an `if` condition to be a **comparison**. So a feature that works in eval
can still fail to cross-compile. Desugar to `if` + comparisons + the `int_*`
builtins, never to `and`/`or`/`block`. (This is why `&&`/`||`/`!` lower to `if`,
and `return` folds to nested `if`/`else` instead of `block`/`leave`.)

Always validate natively, not just in eval (see Verify).

## Recipe A — a new binary operator

Operators are registry-driven. The lexer already accepts generic runs of
operator characters (`+ - * / % < > = & | ^ ! ~`), so a new operator made from
those characters does **not** need a grammar edit.

1. Register the operator in `clike/expr.caap`, or from a downstream module
   through the public facade, for example:
   `(register_infix_op! "add" "%%" "int_add")`.
2. If it needs a new precedence tier, add the tier in `clike/expr.caap` and
   thread it into the parse chain. Keep the native-safety rule in mind.

Short-circuit (`&&`/`||`) is the exception: it can't use `parse_level` (native has
no `and`/`or`) — see `parse_logical`, which builds `(if a b false)` / `(if a true b)`.

The same generic operator token can mean something else in type context. Type
spelling uses the shared operator-run helpers from `clike/tokens.caap`, so
`List<T, field>*` and `T**` are resolved by `clike/types.caap`, not by adding
more lexer alternatives.

## Recipe B — a new literal

Keep the lexer broad. `clike/grammar.caap` already keeps digit-leading
non-decimal atoms whole with `numatom`; `clike/expr.caap` decides whether a
spelling is a valid literal. Extend or register primary handling there, and
report invalid spellings with a located semantic error.

Hex/binary literals are the model: `0x...` / `0b...` are recognized by expression
semantics, base-specific digits are validated there, and invalid atoms like
`0xZZ` fail as `invalid numeric literal ...` instead of needing a grammar rule.

## Recipe C — a new statement / control flow

Statements are registry-driven. Add a matcher/lowerer pair with
`register_statement!`, keyed on the surface shape you want, and leave
`block_lower_block` as the generic dispatcher. Emit a stdlib surface spec via
`calln`, `stamp` it with the token (for located errors), and advance `i`.
Mind the native-safety rule:
`return` is pure surface sugar that folds guard clauses into the `if` else-arm
(it does **not** use `block`/`leave`). `else if` is desugared in the `if` handler
(`skip_if_chain` delimits the chain so trailing statements stay in the outer block).

Casts are also registered postfix forms, but they live in the cast tier (after
prefix unary, before binary operators) to preserve `*p as T` and `a + b as T`
precedence.

## Recipe D — a new top-level declaration / attribute

Top-level declarations are **registry-driven**: `lower_program_at` parses every
named declaration generically (`classify_named`) and routes it to a per-kind hook
through two file-local tables — `decl_kinds` (struct/type/enum/function/extern/
global/const/bss_array) and `decl_attrs` (export/const/mut/extern/volatile plus
user metadata). You do **not** edit the dispatch to extend it:

- **A new metadata word** (prefix or postfix, like `packed`): call
  `(register_attribute! "packed" (lambda (an kind attrs name) …))`. The validator
  throws (via `(oops … name)`, located) when the attribute is illegal for the
  classified `kind`, and is a no-op otherwise. Metadata is parsed broadly first:
  unknown words are accepted by the scanner, then rejected later as
  `unknown clike metadata \`x\``. Your hook reads accepted metadata with
  `(node_export? node)`-style access, `(get (get node "attrs") "packed")`.
- **A new kind-WORD** (like `struct`/`enum` — `NAME record … = { … }`): write a
  `lower_<kind>` hook in `clike/decl_lower.caap` that emits the kernel form and
  RETURNS the cursor just past the declaration, then register it as a source word:
  `(register_decl_kind! "record" "record" lower_record true)`.
- **A new inferred kind** (recognised by SHAPE, not a leading word) should use
  `register_decl_classifier!` with a priority, plus `(register_decl_kind! … false)`.

Declaration heads are not parsed by keyword position. The resolver reads a broad
head such as `name metadata... type metadata... = ...`, chooses the structural
type spelling in the semantic/lowering layer, and leaves the other words as
metadata for validation. That is why both `G u32 meta_ok = 1` and
`G meta_ok u32 = 1` work after `meta_ok` is registered.

`register_decl_kind!` and `register_attribute!` are **exported**, so a downstream
library can extend the surface without touching `clike.caap` (see the `record` /
`packed` goldens in `test_clike.caap`). `lower_fndef` still handles any paren-headed
decl (function with `= { body }`, or a bodyless extern) and is shared by the
`function` / `extern` kinds.

MMIO address constants use the built-in `volatile` attribute:
`REG u32 const volatile = 0x...`. A later statement `REG = value` lowers to
`volatile_write(<address>, u32, value)`, while using `REG` in an expression still
reads as the folded address. There is no volatile-read or compound volatile
assignment sugar yet.

## URun as integration consumer

`examples/urun` is the main embedded consumer of clike. If a clike readability
feature changes real embedded code for the better, update URun in the same pass
and verify the freestanding build/run. Do not change URun behavior or the UART
string; the expected output is `MABCPDT`.

## Verify (run these, in order)

`cargo build` first (the refactor/parse tools call `./target/debug/caap`).

```bash
# 1. the grammar file still parses (it is kernel s-expr, so caap_refactor reads it)
python3 scripts/caap_refactor.py check stdlib/frontend/clike.caap
python3 scripts/caap_refactor.py check stdlib/frontend/clike/*.caap

# 2. lower a snippet and eyeball the IR it desugars to (your fast inner loop).
#    Write a tiny composed script that loads clike + render and prints the result;
#    feed it `name (a i32) i32 = { <your feature> }` and read the (defn …) it emits.
cargo run -p caap-cli -- stdlib/bootstrap.caap <lower.caap> '<clike snippet>'

# 3. the in-language corpus (includes test_clike.caap — add goldens for your feature)
cargo nextest run -p caap-core -E 'test(stdlib_run_all_in_language_tests)' --run-ignored all

# 4. NATIVE end-to-end: build + run the URun slice (it exercises real clike
#    through prep -> LLVM -> a Cortex-M3 ELF). Needs clang + ld.lld.
#    The UART must print exactly MABCPDT.
cargo run -p caap-cli -- stdlib/bootstrap.caap examples/urun/ur_build.caap \
    examples/urun out/urun.elf cortex-m3
qemu-system-arm -M mps2-an385 -cpu cortex-m3 -nographic -kernel out/urun.elf
```

If step 2 lowers but step 4 fails with `unsupported head …` or `if condition must
be a comparison`, you hit the native-safety rule — re-desugar to `if` + `int_*`.

Always add a `test_clike.caap` golden (step 3) for the exact lowering, and — if the
feature changes real embedded code readably — exercise it in the URun slice so
step 4 covers it natively.
