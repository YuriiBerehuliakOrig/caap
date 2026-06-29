# caap-peg

`caap-peg` is CAAP's grammar mechanism. It is intentionally independent from
CAAP compiler semantics: it owns grammar data structures, grammar mutation,
validation, parsing, recovery, incremental parse cache state, and semantic hook
dispatch. It must not know about modules, CTFE, stdlib policy, LLVM, or host
capabilities.

## Architectural Contract

- `Grammar` is a first-class data structure: rules can be inspected, mutated,
  serialized, validated, cached, and interpreted at runtime.
- Static grammar analysis is a parser mechanism. CAAP-facing tooling should
  consume `GrammarAnalysis` projections instead of duplicating rule-graph,
  ambiguity, or reachability checks.
- `ParseValue` is the parser value model. Named bindings are represented only
  by `ParseValue::Named`; magic node-name encodings are not accepted as named
  bindings.
- **There is one host-extension surface: the Parse Effects Protocol.** A
  host-supplied `ParseDriver` answers typed `ParseEffect` decision points with a
  `Directive` (`Proceed`/`Accept`/`Reject`/`Commit`/`Restrict`/`Fail`). It backs
  every semantic hook — `@action`, `@?pred`, `@!guard`, and behaviors — plus
  global control (choice steering, rule transform/reject), transactional host
  state across backtracking, facet-keyed memo, and scoped sub-parses. With no
  driver attached the protocol is inert and parsing is ordinary PEG; a grammar
  that uses `@action`/`@?pred` requires a driver or fails instead of silently
  passing through.
- Recovery is an explicit parser mode. Sync tokens or sync regex must be
  configured; invalid recovery configuration is an error.
- Scannerless parsing is the default. Token-stream parsing is available through
  `tok(...)` + `LexToken`, fed either by an external lexer via
  `ParseRequest::tokens` or by the built-in `Scanner` via `ParseRequest::scan`.
  The driver works on both the scannerless and token-stream paths.
- Incremental parsing exposes cache and edit metadata as parser mechanisms, not
  as CAAP compiler policy.
- Incremental edit and cache transplant math is checked at the parser boundary:
  malformed ranges, unrepresentable deltas, shifted-offset overflow, and
  unmappable projected spans are explicit errors or cache-entry drops, not
  wrapping arithmetic.
- Diagnostic line/column helpers preserve native `usize` source offsets and
  saturate only when projecting into compact public `ParseError` location
  fields.

## Public API layering

The surface is intentionally tiered so the everyday API stays small while the
full capability set remains reachable:

- **`caap_peg::prelude`** — the curated everyday surface: grammar building
  (`Grammar`, `GrammarBuilder`, `builder`, `PegExpr`), parsing (`ParseRequest`,
  `PEGParser`, `parse`), values (`ParseValue`, `ParseError`), the driver protocol
  (`ParseDriver`, `ParseDriverBuilder`, `ParseEffect`, `Directive`, `ParseView`),
  behaviors, and analysis (`analyze_grammar`). Most code wants
  `use caap_peg::prelude::*;`.
- **Crate root** — additionally exposes niche capabilities that are not part of
  the everyday flow: grammar mutation/diffing, validation, the registry,
  incremental parsing/pipelines, diagnostics projections, and signatures.
- **Feature-gated subsystems** (off by default) — enable in `Cargo.toml`:
  - `recovery` — error-recovery parsing (`PEGParser::recover_parse`, the
    `recovery` module).
  - `transaction` — transactional grammar editing (`transaction`,
    `transaction_stack`).
  - `derive` — `#[derive(FromParseValue)]` for typed extraction (pulls in the
    `caap-peg-derive` proc-macro crate).

Parsing goes through the `ParseRequest` builder:

```rust
use caap_peg::prelude::*;

let grammar = Grammar::trusted_new("root <- 'hi'").with_start_rule("root");
let value = ParseRequest::new(&grammar).spans().run("hi")?;
// .driver(&driver) attaches the Parse Effects Protocol; .tokens(tokens) a token stream.
```

Semantic hooks are wired through a `ParseDriver`. The `ParseDriverBuilder`
covers the common case (named handlers, no boilerplate):

```rust
use caap_peg::prelude::*;

// `@up(e)` upper-cases its match; `@?even` keeps only even-length text.
let grammar = Grammar::trusted_new("root <- @up(/[a-z]+/)").with_start_rule("root");
let driver = ParseDriverBuilder::new()
    .action("up", |value, _view| match value {
        ParseValue::Text(s) => ParseValue::Text(s.to_uppercase().into()),
        other => other,
    })
    .build();
let value = ParseRequest::new(&grammar).driver(&driver).run("hi")?;
assert_eq!(value, ParseValue::Text("HI".into()));
```

For stateful, context-sensitive parsing (symbol tables, scopes) implement
`ParseDriver` directly — see `tests/context_sensitive.rs` for the classic
"is this identifier a type name?" resolution.

The grammar body syntax is documented in [docs/peg/grammar-syntax.md](../docs/peg/grammar-syntax.md).
The full PEG docs set (guide, spec, syntax table) lives under [docs/peg/](../docs/peg/).

## Incremental parsing & subtree reuse

`parse_incremental_many` reuses a `PositionCache` across edits. Reuse is **sound
with respect to lookahead**: each cached rule result records the byte interval it
actually *examined* (`read_lo..read_hi`), not just the span it matched. Because
`&e` / `!e` / `&<e` / `!<e` lookahead and trivia can read bytes a rule never
consumes, an edit inside that examined-but-unmatched region correctly
invalidates the entry — replaying it would be unsound.

The examined interval is computed exactly for the built-in terminals. For regex
terminals it is derived from the pattern's own automaton: the terminal is matched
**anchored**, and its examined extent is the offset at which a start-anchored DFA
for the pattern dies (can no longer extend). That is a sound, tight bound for any
pattern — it includes the greedy "stop byte", lookahead, and bytes a greedy
quantifier reads then backtracks over, while not running to end-of-input for a
bounded terminal like `"[^"\\]*"`. (A pathological pattern whose DFA exceeds a
size limit falls back to "examined to end-of-input", which is still correct, just
less reusable.) The whole machinery — frame stack and the regex extent DFA — is
skipped entirely on a non-incremental `parse()`, which therefore pays nothing.

## Local Checks

```bash
cargo test -p caap-peg --all-features   # exercises recovery + transaction too
cargo test -p caap-peg                  # default (lean) build
```

`tests/panic_safety.rs` is a randomised (proptest) panic-safety suite: every
entry point that takes untrusted input — `parse`, `parse_ast_tolerant`,
`Grammar::try_new`, `Scanner::scan` — must return `Ok`/`Err`, never abort, for
arbitrary bytes. The `fuzz/` crate carries the same contract as coverage-guided
libFuzzer targets for longer campaigns:

```bash
cargo install cargo-fuzz
cargo +nightly fuzz run parse_input
cargo +nightly fuzz run parse_grammar
```
