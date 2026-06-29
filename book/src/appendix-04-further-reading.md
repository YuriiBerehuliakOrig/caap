# Appendix D: Further Reading

This book is a tutorial. When you need authoritative, exhaustive detail, these
sources in the repository are the references it draws on.

## In This Repository

- **`KERNEL_REFERENCE.md`** — the complete kernel and CTFE reference: every
  special form, every builtin, the full `ctfe_*` compile-time surface (registry,
  providers, stages, IR introspection and construction, metadata, provider
  context, surface syntax, grammar mechanisms), host services, effect tags, and
  pipeline stages. This is the definitive catalog; this book's Appendix A is a
  condensed slice of it.

- **`docs/caap-spec.md`** — the audited language / stdlib / toolchain
  specification: the core model, kernel surface syntax, the segmental reader, the
  module language, surface forms, the type and effect layer, the surface
  protocol, the native toolchain, and the CLI contract. Concise and current.

- **`examples/`** — the tested example corpus. Almost
  every example in this book is drawn from here, so these files are guaranteed to
  parse and run. Highlights:
  - `greet.caap`, `arith.caap`, `combined.caap` — modules, exports, imports;
  - `structs.caap`, `typed.caap` — `struct`, `defn`, sized types;
  - `eff_tags.caap`, `eff_use.caap` — effects, ownership, `const` folding;
  - `derive_print.caap` — a `show!` derive built through the IR layer;
  - `surface_grammar.caap` — live grammar extension on the bare kernel;
  - `guess_game.caap` — the number game in the C-like surface;
  - `json_report.caap` — the Chapter 15 capstone;
  - the `native_*.caap` family — one file per native-subset feature.

- **`README.md`** — the workspace layout and how to build and test.

- **`stdlib/`** — the standard library itself. Since it's written in CAAP,
  reading it is the best way to see idiomatic tower code: `stdlib/lib/` for the
  library, `stdlib/semantics/types/` for the type and effect layer, `stdlib/frontend/`
  for the surface languages and `stdlib/backend/` for the code generators,
  `stdlib/sys/` for the host-service facades, and
  `stdlib/boot/` for the loader and forms.

## The Crates

CAAP is a Cargo workspace. The kernel and compiler live in `caap` (the
`caap-core` library) and `caap-cli` (the `caap` binary); `peg` is the parser
engine; `caap-sys-runtime` (+ `caap-sys-runtime-ffi`) is the system runtime;
`caap-lsp` and `caap-dap` are the editor and debugger integrations.

## Building the Book

This book is an [mdBook](https://rust-lang.github.io/mdBook/). To render it
locally, install mdBook and run, from the `book/` directory:

```bash
mdbook build      # render to book/book/
mdbook serve      # live-preview at http://localhost:3000
```

The Markdown sources under `book/src/` are also perfectly readable on their own,
and on GitHub.
