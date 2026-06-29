# caap-peg — User Guide

A practical, detailed guide to using `caap-peg`. For the formal semantics see
[`SPECIFICATION.md`](SPECIFICATION.md); for a one-page syntax table see
[`grammar-syntax.md`](grammar-syntax.md); for runnable code see
[`../../peg/examples/`](../../peg/examples)
(`cargo run -p caap-peg --example tour`, etc.).

## Contents
1. [Quick start](#1-quick-start)
2. [Building grammars](#2-building-grammars)
3. [Parsing: the `ParseRequest` builder](#3-parsing-the-parserequest-builder)
4. [The result value & typed extraction](#4-the-result-value--typed-extraction)
5. [Grammar constructs](#5-grammar-constructs)
6. [Trivia, keywords, and layout](#6-trivia-keywords-and-layout)
7. [Tokens and the built-in Scanner](#7-tokens-and-the-built-in-scanner)
8. [Cross-grammar imports & the registry](#8-cross-grammar-imports--the-registry)
9. [Semantics via the driver protocol](#9-semantics-via-the-driver-protocol)
10. [Incremental reparsing & AST diff](#10-incremental-reparsing--ast-diff)
11. [Analysis, validation, mutation](#11-analysis-validation-mutation)
12. [Errors & diagnostics](#12-errors--diagnostics)
13. [Performance & resource notes](#13-performance--resource-notes)
14. [Feature flags](#14-feature-flags)

---

## 1. Quick start

```rust
use caap_peg::{Grammar, ParseRequest};

let g = Grammar::trusted_new("greeting <- 'hello ' /[a-z]+/").with_start_rule("greeting");
let value = ParseRequest::new(&g).run("hello world")?;        // -> ParseValue
// or the one-liner:
let value = caap_peg::parse("hello world", &g)?;
```

`Grammar::trusted_new` panics on invalid grammar text (use it for in-process,
literal grammars); `Grammar::try_new` returns `Result` and is the boundary for
*untrusted* grammar text.

---

## 2. Building grammars

Three ways, all producing a `Grammar`:

**a. From PEG text** (most common):
```rust
let g = Grammar::trusted_new(
    "expr <- /[0-9]+/ (('+' / '-') /[0-9]+/)*"
).with_start_rule("expr");
```

**b. The `GrammarBuilder` DSL** (programmatic, no text):
```rust
use caap_peg::builder::{GrammarBuilder, choice, seq, lit, rule_ref, plus, char_class};
let g = GrammarBuilder::new()
    .start("expr")
    .rule("expr", choice(vec![
        seq(vec![lit("("), rule_ref("expr"), lit(")")]),
        plus(char_class("0-9").unwrap()),
    ]))
    .build();
```
Other DSL helpers: `dot`, `regex`, `opt`, `star`, `sep_plus`, `interspersed`,
`named`, `capture`, `precedence`/`infixl`/`infixr`/`prefix`/`postfix`,
`semantic_action`/`semantic_predicate`/`semantic_guard`, `param`, `call`,
`imported_ref`, `grammar_scope`, `parametric`, … (see `caap_peg::builder`).

**c. From a JSON spec** (`SpecCompiler`):
```rust
use caap_peg::SpecCompiler;
let spec = serde_json::json!([
    "grammar", "greeting", "root",
    [["rule", "root", ["seq", [["lit", "hi "], ["regex", "[a-z]+"]]]]]
]);
let g = SpecCompiler::new().compile(&spec)?;
```
Note `seq`/`choice` take their children as a **single nested array**
(`["seq", [c1, c2]]`). The JSON spec carries no semantic-hook tag — attach
semantics through the driver (§9).

**Mutating a grammar** (all bump the version and reset the analysis cache):
`with_start_rule`, `with_import`, `with_metadata`/`set_metadata_value`, `set_rule`,
`extend(&[(name, src)])`, `remove_rule`, plus fallible `try_*` twins; `seal()` /
`thaw()` freeze/unfreeze it.

---

## 3. Parsing: the `ParseRequest` builder

`ParseRequest` is the single entry point. Configure it, then pick a **terminal**
for the result shape:

```rust
ParseRequest::new(&grammar)
    .config(cfg)          // ParserConfig (memo, max_steps, …)
    .spans()              // wrap the root result in a SpannedValue
    .driver(&driver)      // attach a ParseDriver (semantics/control)
    .tokens(toks)         // parse a pre-produced token stream
    .scan(&scanner)       // OR: tokenise with the built-in Scanner
    .registry(&registry)  // resolve cross-grammar imports
    .start_rule("r")      // override the start rule (used by run_prefix)
    .ast()                // emit an AstNode from run_output
    // ── terminals ──
    .run(text)            // -> Result<ParseValue>
    .run_output(text)     // -> Result<ParseOutput>  (Value | Ast)
    .run_profiled(text)   // -> Result<(ParseValue, ParseProfile)>
    .run_prefix(text, 0)  // -> CompletedPrefixParse  (parse a leading slice)
    .run_incremental(text, &mut cache) // -> Result<Arc<ParseValue>>
```

`ParserConfig::default()` is `memo = true`, `max_steps = 4096`, no spans, value
output. Tune with `.with_memo(bool)`, `.with_max_steps(n)`, `.with_spans()`,
`.with_output_mode(…)`.

---

## 4. The result value & typed extraction

`ParseValue` (see SPECIFICATION §5) is `Nil | Text | Number | Node(tag,kids) |
Named(name,val) | SpannedValue`. Inspect it with the accessor API:

```rust
v.text()                  // Option<&str>            (unwraps SpannedValue)
v.node()                  // Option<(&str, &[ParseValue])>
v.field("name")           // Option<&ParseValue>     (a Named child by name)
v.is_spanned() / v.inner()
v.parse_as::<i64>()       // Result<i64, FromParseValueError>
v.parse_field::<i64>("n") // parse a named binding into a type
```

`parse_as` works for `String`, `i64`, `bool`, `Option<T>`, `Vec<T>`, and — with
the **`derive`** feature — any `#[derive(FromParseValue)]` struct/enum.

---

## 5. Grammar constructs

The complete catalogue with a one-line example each lives in
[`grammar-syntax.md`](grammar-syntax.md) and is exercised by
[`../../peg/examples/grammar_constructs.rs`](../../peg/examples/grammar_constructs.rs). Highlights:

- **Terminals**: `'lit'`, `i"CaseInsensitive"`, `/regex/`, `[char-class]`, `.`.
- **Combinators**: sequence (juxtaposition), `/` choice, `e?`/`e*`/`e+`/`e{m,n}`,
  `&e`/`!e` lookahead, `&<e`/`!<e` lookbehind, `~` cut, `!!e` eager.
- **Precedence climbing**: `prec(operand, infixl("+","-"), infixl("*","/"), …)`
  — no left recursion needed; yields `binop`/`unary_*` nodes.
- **Separated repetition**: `sep_plus(e, sep)` (drops separators),
  `interspersed(e, sep)` (keeps them).
- **Bindings**: `name:e` (Named), `capture("label", e)` (SpannedValue),
  `backref("name")` (match text equal to a prior binding).
- **Delimited text**: `island("(", ")")`, `raw_block("(", ")", "kind")` (nested).
- **Keywords**: `kw("if")` / `hard_keyword`, `soft_keyword`.
- **Error labelling & recovery**: `expected("msg", e)`, `recover("sync", …)`.

⚠️ **Tightness**: write `name(args)` and `/regex/` *tight*. `f(x)` is a call;
`f (x)` is a reference `f` followed by a group `(x)`. `a /re/` is a sequence;
`a / b` is a choice. (See SPECIFICATION §3.4.)

---

## 6. Trivia, keywords, and layout

Inter-token skipping is governed by the `__grammar__.trivia` metadata key:

```rust
use std::collections::HashMap;
let g = Grammar::trusted_new("doc <- 'a' 'b' 'c'")
    .with_start_rule("doc")
    .with_metadata("__grammar__",
        HashMap::from([("trivia".to_string(), serde_json::json!("whitespace"))]));
// now "a b c" with spaces parses
```
Values: **absent → `"default"`** (whitespace + comments — the default when you
set no key), `"none"` (no skipping), `"whitespace"`, `"default"`, or any regex.
Scope it locally with `no_trivia(e)` / `with_trivia(spec, e)`. Because the
default skips whitespace, matching significant whitespace (a lone `\n`, trailing
whitespace) needs `"none"`/`no_trivia` or indentation mode.

Set `"indentation": true` in `__grammar__` to enable layout-aware `newline` /
`indent` / `dedent` terminals.

---

## 7. Tokens and the built-in Scanner

For a lexer/parser split, match `tok(KIND)` against a token stream. Produce the
stream with the declarative `Scanner` (maximal munch; ties broken by declaration
order):

```rust
use caap_peg::{Grammar, ParseRequest, Scanner};
let scanner = Scanner::new()
    .token("NUMBER", r"[0-9]+")?
    .literal("PLUS", "+")
    .skip(r"\s+")?;                       // trivia: consumed, not emitted
let g = Grammar::trusted_new("sum <- tok(NUMBER) (tok(PLUS) tok(NUMBER))*")
    .with_start_rule("sum");
let v = ParseRequest::new(&g).scan(&scanner).run("1 + 2 + 3")?;
```
Or supply tokens directly with `.tokens(vec![LexToken::new(kind, text, start, end), …])`
(e.g. from an external lexer). An explicit `tokens(…)` wins over a `scan(…)`.

---

## 8. Cross-grammar imports & the registry

Reference another grammar's rules with `grammar::rule` (`ImportedRef`) or
`scope("grammar", e)` (`GrammarScope`). Resolve them either inline:

```rust
let main = Grammar::trusted_new("start <- ident::rule")
    .with_start_rule("start")
    .with_import("ident", Grammar::trusted_new("rule <- /[a-z]+/").with_start_rule("rule"));
```
or through a shared, namespaced `GrammarRegistry`:

```rust
let mut reg = caap_peg::GrammarRegistry::new();
reg.register("ident", Grammar::trusted_new("rule <- /[a-z]+/").with_start_rule("rule"))?;
let v = ParseRequest::new(&main).registry(&reg).run("hello")?;
```

---

## 9. Semantics via the driver protocol

The engine carries no semantics; the host attaches a `ParseDriver`. Build one
fluently and attach it with `.driver(&d)`:

```rust
use caap_peg::{ParseDriverBuilder, Directive, ParseValue};
let driver = ParseDriverBuilder::new()
    .action("upper", |v, _view| match v {                 // @upper(e)
        ParseValue::Text(s) => ParseValue::Text(s.to_uppercase().into()),
        other => other })
    .predicate("even", |_view| true)                       // @?even
    .guard("short", |v, _view| match v {                   // @!short(e)
        ParseValue::Text(s) if s.len() > 3 => Directive::Reject,
        _ => Directive::Proceed })
    .on_event(|_effect, _view| { /* observe every effect */ })
    .with_auto_scope()                                     // @?in_<rule> / @?not_in_<rule>
    .build();
```

Hooks receive a `&ParseView` exposing `matched_text`, `span`, `pos`,
`rule_stack`, `named()`, `grammar()`, `config()`, `state()`, and a scoped
`sub_parse(rule, pos)`. The driver also supports transactional host state
(`checkpoint`/`rollback`/`commit`) and per-rule memo soundness (`memo_facet`).
See [`../../peg/examples/driver_protocol.rs`](../../peg/examples/driver_protocol.rs) and
[`../../peg/tests/context_sensitive.rs`](../../peg/tests/context_sensitive.rs). For a worked
**context-dependent** parse (a `name → meta → value` chain where the parsed type
meta selects the value grammar at runtime, with `checkpoint`/`rollback` and
`memo_facet`), see [`../../peg/examples/context_dependent.rs`](../../peg/examples/context_dependent.rs).

---

## 10. Incremental reparsing & AST diff

For editors: reuse a `ParseCache` across edits, and diff/share AST subtrees.

```rust
use caap_peg::{ParseRequest, ParseCache};
let mut cache = ParseCache::default();
let v1 = ParseRequest::new(&g).run_incremental(text_v1, &mut cache)?;
let v2 = ParseRequest::new(&g).run_incremental(text_v2, &mut cache)?; // reuses unchanged subtrees
```

AST trees and their structural diff:
```rust
use caap_peg::{parse_ast, parse_ast_tolerant};
use caap_peg::ast_diff::{changed_ranges, reparse_ast_incremental, AstEdit};

let old = parse_ast(&g, "ab.cd.", None)?;          // Result<AstNode>
let _   = parse_ast_tolerant(&g, "ab.!!", None);   // always returns a tree (<error> nodes)
let new = parse_ast(&g, "ab.ce.", None)?;
let edit = AstEdit::new(4, 5, 5);                  // [start, old_end) -> [start, new_end)
let ranges = changed_ranges(&old, "ab.cd.", &new, "ab.ce.", &edit);  // Vec<AstSpan>
let merged = reparse_ast_incremental(&old, "ab.cd.", new, "ab.ce.", &edit); // shares Arcs
```
Unchanged, unshifted subtrees in `merged` are *physically* shared with `old`
(`Arc::ptr_eq`), so downstream tooling can skip them. Reuse is sound on the
examined read extent (SPECIFICATION §11.1). See
[`../../peg/examples/incremental_and_tools.rs`](../../peg/examples/incremental_and_tools.rs).

**Editor primitives** (`caap_peg::editor`) turn an `AstNode` into the data an
LSP/editor needs — pair them with `changed_ranges` to recompute only the dirty
regions:
```rust
use caap_peg::editor::{semantic_tokens, folding_ranges, selection_ranges, RuleKinds};
let kinds: RuleKinds = [("num", "number"), ("op", "operator")]
    .iter().map(|(r, k)| (r.to_string(), k.to_string())).collect();
let tokens = semantic_tokens(&tree, &kinds, source); // highlighting (whitespace-trimmed)
let folds  = folding_ranges(&tree, source);           // multi-line node spans
let chain  = selection_ranges(&tree, cursor_byte);    // expand-selection, innermost first
// document outline: map rules -> (kind, name-child rule); symbols nest.
let outline = document_symbols(&tree, source, &symbol_rules); // Vec<Symbol{name,kind,span,children}>
```
See [`../../peg/examples/editor.rs`](../../peg/examples/editor.rs). This is the tree-sitter
highlighting/structure layer, but driven by a runtime grammar.

---

## 11. Analysis, validation, mutation

```rust
let a = caap_peg::analyze_grammar(&g);        // refs, reachable, left_recursive_sccs, errors…
let r = caap_peg::validate_grammar(&g);       // ValidationReport: ok(), errors(), warnings() (with codes)
caap_peg::add_rule(&mut g, "extra", "'y'")?;  // also replace_rule / remove_rule / set_start_rule
let d = caap_peg::diff_grammars(&base, &g);   // added/removed/changed rules, start_changed
let sig = caap_peg::grammar_signature(&g);    // u64 identity for caching
let graph = caap_peg::rule_graph(&g);         // rule -> referenced rules
```

---

## 12. Errors & diagnostics

`ParseError` carries `message`, `span`, an optional machine-readable `code`,
`expected`/`found`, and `line`/`col`. The `diagnostics` module renders editor
diagnostics:

```rust
let pretty = caap_peg::diagnostics::render_parse_error(source, &err);     // rustc-style caret
let diag   = caap_peg::peg_error_to_diagnostic_with_source(/* … */);       // structured Diagnostic
let (line, ch) = caap_peg::diagnostics::lsp_position(source, byte_offset); // 0-based UTF-16
```
For invalid input you want a tree from anyway, use `parse_ast_tolerant`.

---

## 13. Performance & resource notes

Measured on `benches/peg_bench.rs` + an instrumented allocator (release):

- **Throughput**: ~30 MB/s on flat grammars (identifier lists), ~6 MB/s on deeply
  nested grammars (JSON-like), single-µs on small inputs. As a runtime
  *interpreter* (not codegen), expect to sit below code-generating PEGs and well
  below specialised parsers like `serde_json`.
- **Memory**: ~0.5–1.3 heap allocations per input byte (every node/token is
  `Arc`-wrapped); sub-MB working set for KB inputs. **Memo roughly doubles peak
  heap** (the packrat table) and only pays off on grammars with real backtracking
  — for simple grammars `config.with_memo(false)` halves memory at no speed cost.
- **Feature overhead**: a no-op driver adds ~6%; AST building ~20% over a value
  parse; the protocol is free when no driver is attached.
- **Incremental**: localized edits reparse ~6× faster than a full parse.
- **Recursion**: nesting is bounded by `config.max_depth` (default 1024) — deeply
  nested input fails with a `recursion_limit` error, never a stack overflow.
  Raise `max_depth` (+ `RUST_MIN_STACK`) for genuinely deep data.

---

## 14. Feature flags

`caap-peg` is lean by default; opt in to heavier subsystems:

| feature | enables |
|---|---|
| `derive` | `#[derive(FromParseValue)]` for typed extraction |
| `recovery` | batch error-recovery parsing (`recover_parse`, the `recovery` module) |
| `transaction` | transactional grammar editing (`GrammarTransaction`, savepoints) |

```toml
caap-peg = { version = "0.1", features = ["derive", "recovery", "transaction"] }
```

(The grammar-level `recover("sync", …)` terminal works in the default build; the
`recovery` *feature* adds the multi-error batch driver.)
