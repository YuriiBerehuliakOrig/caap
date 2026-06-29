# caap-peg documentation

| Document | What it is |
|----------|------------|
| [GUIDE.md](GUIDE.md) | **Start here.** Task-oriented user guide: building grammars, the `ParseRequest` API, the value model, tokens/scanner, the driver protocol, incremental reparsing, analysis, performance. |
| [SPECIFICATION.md](SPECIFICATION.md) | Normative spec: grammar EBNF, abstract syntax, value model, operational semantics, trivia/layout, left recursion, memoization & incremental soundness, the Parse Effects Protocol, errors, and conformance invariants. |
| [grammar-syntax.md](grammar-syntax.md) | One-page reference table of every grammar construct and the `PegExpr` it maps to. |
| [future-incremental-ast-and-typed-ast.md](future-incremental-ast-and-typed-ast.md) | Design notes & remaining follow-ups for incremental AST reuse and typed extraction. |

Runnable examples live in [`../../peg/examples/`](../../peg/examples):
`tour`, `grammar_constructs`, `driver_protocol`, `tokens_and_scanner`,
`incremental_and_tools`, `context_dependent` (the name→meta→value chain),
`editor` (semantic tokens / folding / selection ranges) — run e.g.
`cargo run -p caap-peg --example tour`.

API docs: `cargo doc -p caap-peg --all-features --open` (the crate is
`#![deny(missing_docs)]`, so every public item is documented).
