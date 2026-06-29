# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

CAAP is a **compiler platform**, not a single language. The Rust core is a minimal
substrate; almost all language and toolchain policy lives in `.caap` code under
`stdlib/`. Two facts drive everything else:

- **The IR has exactly three node types**: `Name`, `Literal`, `Call`. There is no
  `IfNode`/`LambdaNode`/`MatchNode`. `(if a b c)`, `(lambda (x) ...)`, `(+ 1 2)` are
  all `Call` nodes — semantics come from the *callee's* policies, not the node shape.
- **Core provides substrate; stdlib owns policy** (the cardinal invariant). Modules,
  import/export, the type/effect system, generics, pattern matching, LLVM/WASM codegen,
  and surface grammars are all stdlib `.caap` code, not Rust. The core only knows how to
  register units, run queries, evaluate IR, and bridge host services.

A bare compiler session knows *nothing*. Everything is registered explicitly by running
`stdlib/bootstrap.caap` (itself ordinary CAAP code executed in the compile-time phase).
There is no hidden autoloading.

Read `docs/principles.md` (the 14 non-negotiable principles) and `docs/architecture.md`
before any non-trivial change. `docs/` and the Ukrainian-language design docs are the
architectural contract.

## Build, test, lint

```bash
scripts/strict-gate.sh          # THE definition-of-done gate: fmt --check + clippy -D + nextest + cargo test --doc
cargo fmt --all -- --check      # formatting only
cargo nextest run --workspace   # all non-ignored tests in ONE parallel pool (~2x; install: cargo install cargo-nextest --locked)
cargo test --workspace --doc    # doctests (cargo-nextest does NOT run these)
cargo clippy --workspace --all-targets -- -D warnings

cargo nextest run -p caap-core                    # focused crate (also: caap-peg, caap-cli, caap-sys-runtime)
cargo nextest run -p caap-core -E 'test(<name>)'  # single test by name (or: cargo test -p caap-core <name>)

scripts/test-acceptance.sh      # slow acceptance tests (Rust tests marked #[ignore = "acceptance: ..."])
scripts/production-gate.sh      # strict gate + acceptance + bench compile (CAAP_RUN_BENCHMARKS=1 to actually run)
```

Test tiers are defined in `docs/testing.md`: unit (`src/** #[cfg(test)]`, in-memory, no
bootstrap), integration (`*/tests/*.rs`), acceptance (ignored with an `acceptance:` reason).
The stdlib proof-gate is split by scenario across `caap/tests/stdlib_{forms,loader,types,codegen,
sys,passes}_tests.rs`; the in-language stdlib `test_*.caap` corpus is loaded as an acceptance test
in `stdlib_loader_tests.rs`. `common::bootstrapped_session()` caches the stdlib bootstrap (cloned
per test) so tests don't re-run it. Tests run via **`cargo-nextest`** (parallel across all binaries).

## Running CAAP programs (the CLI is flagless)

There are **no subcommands and no flags**. The binary is a launcher:

```
caap PROGRAM                      # bare-kernel eval, no stdlib ('-' reads stdin)
caap BOOTSTRAP PROGRAM [ARG...]   # run BOOTSTRAP (the policy), then PROGRAM
```

The bootstrap decides what "running a program" means. Common invocations:

```bash
# run a stdlib program through the loader (tools/run.caap is the reusable driver)
cargo run -p caap-cli -- stdlib/bootstrap.caap tools/run.caap PROGRAM.caap

# native / LLVM IR (each s2_* tool load_modules the codegen layer on demand)
cargo run -p caap-cli -- stdlib/bootstrap.caap tools/s2_emit.caap  FILE > out.ll
cargo run -p caap-cli -- stdlib/bootstrap.caap tools/s2_build.caap FILE OUTPUT

# parse / AST dump on the fast bare (no-stdlib) policy
cargo run -p caap-cli -- tools/bare.caap tools/ast_json.caap     FILE
cargo run -p caap-cli -- tools/bare.caap tools/canonicalize.caap FILE
```

Under `caap BOOTSTRAP PROGRAM [ARG...]`, an `int` result becomes the process
exit code (same contract as natively compiled binaries). In bare
`caap PROGRAM` mode, non-`null` values are printed and successful evaluation
exits `0`.
NOTE: `caap check` / `caap run` / `caap ast-json` are **not** CLI subcommands — they are
programs run under a bootstrap (and `check` is the helper in `scripts/caap_refactor.py`).

## Editing `.caap` files — read this first

This is the single most important workflow rule (`.agents/conventions.md` §1):

- **Existing `.caap` files → edit via `scripts/caap_refactor.py`** (span-based: preserves
  formatting of untouched regions and auto-verifies by re-parsing). It calls `./target/debug/caap`,
  so `cargo build` must have run first. CLI: `python3 scripts/caap_refactor.py --help`
  (e.g. `check <file>` parses a file as a verification).
- **Raw Write/Edit is only for brand-new `.caap` files.** After creating one, still run a
  parse check on it.

For programmatic edits, use the span API (`load_ast`, `collect_span_edits`, `apply_span_edits`,
`check`) — see `.agents/skills/caap-refactor.md`. Behavior-preserving refactors are verified by
golden-comparing lowered output (AST/LLVM-IR before vs after must match).

## Repository layout

Rust crates (Cargo workspace, `resolver = "2"`):

- `caap/` (`caap_core`) — the kernel: IR, evaluator (dual-phase), builtins, CTFE, compiler
  session/query engine, semantic graph, host-service bridge, capability policy.
- `caap-cli/` — the flagless `caap` binary.
- `caap-lsp/` / `caap-dap/` — editor/debugger integrations; they call the bootstrapped
  `caap.session.commands` capability map (no v1 fallback).
- `peg/` (`caap_peg`) — standalone PEG engine that *interprets* a grammar as a first-class data
  structure. **It must not depend on `caap-*`** — that coupling is an architecture violation.
- `peg-derive/` — proc-macro support for PEG.
- `caap-sys-runtime/` (+ `-ffi/`) — native system services (fs/io/net/os/path/proc/time) across
  the FFI boundary; the C-ABI wrapper static lib that native builds link against.

CAAP code and assets:

- `stdlib/` — the active standard library, layered in tiers (a module may only depend on
  lower/equal tiers, enforced by `semantics/passes/tiers.caap`): `boot/` (expander → forms →
  loader → commands; the seed, rank 0), `lib/` (collections/text/core/diag + `project`/`test`,
  rank 2), `syntax/` (ast/ir/render — the general code-as-data substrate, rank 2), `semantics/`
  (`types/` + `passes/`, rank 4), `frontend/` (surface/clike source-language front-ends, rank 5),
  `backend/` (native_meta/prep/codegen_common/emit/{llvm,wasm}/driver — codegen, rank 5, lazily loaded),
  `storage/` (binary-format DSL), `sys/` (typed sys facades), `bare/` (rank 5 native-only
  bare-metal wrappers — mmio/cpu/critical over `volatile_*`/`asm`; reached by native programs'
  `(use …)`, not the eval loader). See `stdlib/CONVENTIONS.md`.
- `tools/` — small CAAP programs (`bare.caap`, `run.caap`, `ast_json.caap`, `s2_emit.caap`, `s2_build.caap`, …).
- `examples/urun/` — the `urun` example: a freestanding clike-surface RTOS kernel (codegen-gated
  by `stdlib_codegen_tests`, highlight-gated by `caap-lsp/tests/urun_highlight.rs`). `tests/` —
  intentionally broken fixtures for negative tests. `book/`, `docs/` — documentation.
- `vscode-caap/` — the VS Code extension (syntax + LSP/DAP integration via `caap-lsp`/`caap-dap`);
  a standalone npm package, NOT a Cargo workspace member.

## Architecture essentials (the parts that span multiple files)

- **Surface → IR**: source is read *segmentally* (one top-level form at a time), so top-level
  reader directives (`extend_syntax`, `define_grammar`, `begin_scope`/`end_scope`) can mutate the
  live grammar for *subsequent* forms. PEG produces an AST; semantic hooks lower it to the IR graph.
- **Unit** is the compilation unit (IR + semantics + attributes + syntax + snapshots). It is
  *module-agnostic* — it knows nothing about import/export. It supports transactions
  (begin → mutate → commit/rollback).
- **Query-driven pipeline**: the compiler doesn't call passes imperatively. It *queries* for an
  artifact; the query engine resolves provider dependencies, runs them in order, and caches results
  by fingerprint (enables incremental recompilation). A **Provider** is a typed compilation edge
  declaring its `from`/`to` stage, `phase`, and `effect_tags`. Writing IR without the `write_ir`
  effect tag is a contract violation even if the output looks right — the cache trusts the metadata.
- **Dual-phase / CTFE**: the same evaluator runs in `RUNTIME` and `COMPILE_TIME`. CTFE is not a
  separate language — compile-time code looks identical to runtime code but has `ctfe_*` builtins to
  read/mutate IR, register symbols/stages/providers. `PhasePolicy.DUAL` is a coarse per-symbol flag
  ("callable in either phase"), *not* partial evaluation (that mechanism is designed in
  `docs/design-partial-evaluation.md` but not yet implemented).
- **Effects & capabilities**: there is no ambient `read_file`. System access goes through typed
  `sys.*` facades + capability policy. `effect_scope` dynamically attenuates privileges (and installs
  an allocation budget) around untrusted callbacks; nested scopes can only narrow, never regain.

## Conventions that constrain changes

From `.agents/conventions.md` and the principles checklist — check these before changing core:

- **Adding a new IR node type, or branching on a name string in the evaluator, is almost always
  wrong.** New semantics go through a callee policy / kit / pass / grammar extension. Types, generics,
  and pattern matching belong in stdlib, not the kernel.
- **Default is behavior-preserving.** Breaking changes must be stated by the task, never silent. No
  silent fallbacks — a contract violation must surface as a diagnostic/error, not a magic correction.
- **Ground everything in real code.** Verify a primitive exists (grep, `KERNEL_REFERENCE.md`, a test)
  before using it — the builtin set changes. Don't invent APIs/paths; mark uncertainty explicitly.
- **Atomic commits**; commit `.agents/` changes separately from code. On a genuine fork in the
  decision, ask rather than guess silently.

`.agents/` is the canonical home for agent skills, playbooks, and conventions (the `.claude/`
copies are materialized from it via `.agents/sync.py` — don't hand-edit `.claude/skills/*`).
`KERNEL_REFERENCE.md` and `docs/builtins.md` are the reference for registered builtins.

## Notes

- The v1 `stdlib/` (194 files) and its compiler-kit/provider/partial-evaluation architecture were
  **deleted** (see `MIGRATION.md`). stdlib is the only standard library; do not reference `stdlib/`.
- `Cargo.lock` is committed (ships runnable binaries). `.caap_build/` (cached C/runtime artifacts)
  is gitignored. Line endings are forced to LF via `.gitattributes`.
- Native executable tests self-skip when host tools (`clang`) are absent.
