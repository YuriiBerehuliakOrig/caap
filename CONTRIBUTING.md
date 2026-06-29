# Contributing to CAAP

Thanks for your interest in CAAP. CAAP is a **compiler platform**, not a single
language: the Rust core is a minimal substrate, and almost all language and
toolchain policy lives in `.caap` code under `stdlib/`. That shape drives the
contribution rules below — please read them before opening a pull request.

This is an early-stage, pre-1.0 project. APIs, builtins, and stdlib boundaries
still move. Grounding every change in real code (not assumptions) matters more
here than usual.

## Read first

Before any non-trivial change, read the architectural contract. These documents
are not background reading — they are the rules a change is reviewed against:

- [`docs/principles.md`](docs/principles.md) — the 14 non-negotiable principles
  (minimal kernel, callee-defined semantics, libraries over language features,
  explicit bootstrap, no silent fallbacks, …). New work must check itself
  against these.
- [`docs/architecture.md`](docs/architecture.md) — current architectural
  contracts and subsystem boundaries.
- [`.agents/conventions.md`](.agents/conventions.md) — the shared conventions
  every contributor and agent follows.
- [`CLAUDE.md`](CLAUDE.md) — a concise orientation to the whole repository.

Two facts drive almost everything else:

- **The IR has exactly three node types**: `Name`, `Literal`, `Call`. There is
  no `IfNode`/`LambdaNode`/`MatchNode` — `(if a b c)` and `(lambda (x) …)` are
  `Call` nodes whose semantics come from the *callee's* policies. Adding a new
  IR node type, or branching on a name string in the evaluator, is almost always
  wrong; new semantics go through a callee policy / pass / grammar extension.
- **Core provides substrate; stdlib owns policy** (the cardinal invariant).
  Modules, the type/effect system, generics, pattern matching, LLVM/WASM
  codegen, and surface grammars are stdlib `.caap` code, not Rust.

## Repository layout

The workspace is split into Rust crates and CAAP code/assets.

Rust crates (Cargo workspace, `resolver = "2"`):

- `caap/` (`caap_core`) — the kernel: IR, dual-phase evaluator, builtins, CTFE,
  compiler session/query engine, semantic graph, host-service bridge,
  capability policy.
- `caap-cli/` — the flagless `caap` binary.
- `caap-lsp/` / `caap-dap/` — editor/debugger integrations; they call the
  bootstrapped `caap.session.commands` capability map.
- `peg/` (`caap_peg`) — standalone PEG engine. It must **not** depend on
  `caap-*` — that coupling is an architecture violation.
- `peg-derive/` — proc-macro support for PEG.
- `caap-sys-runtime/` (+ `-ffi/`) — native system services (fs/io/net/os/path/
  proc/time) across the FFI boundary, and the C-ABI wrapper static lib that
  native builds link against.

CAAP code and assets:

- `stdlib/` — the active standard library, layered in tiers (a module may only
  depend on lower/equal tiers, enforced by `semantics/passes/tiers.caap`):
  `boot/` (the seed) → `lib/` + `syntax/` → `semantics/` → `frontend/` /
  `backend/` / `sys/`. See [`stdlib/CONVENTIONS.md`](stdlib/CONVENTIONS.md).
- `tools/` — small CAAP programs (`bare.caap`, `ast_json.caap`, `s2_emit.caap`,
  `s2_build.caap`, …).
- `examples/` — runnable programs (the test corpus). `tests/` — intentionally
  broken fixtures for negative tests. `book/`, `docs/` — documentation.

## Editing `.caap` files — the most important rule

This is the single most important workflow rule
([`.agents/conventions.md` §1](.agents/conventions.md)):

- **Existing `.caap` files are edited via
  [`scripts/caap_refactor.py`](scripts/caap_refactor.py)** — span-based edits
  that preserve the formatting of untouched regions and auto-verify by
  re-parsing through `tools/ast_json.caap` under `tools/bare.caap`. It calls
  `./target/debug/caap`, so `cargo build` must have run first.

  ```bash
  python3 scripts/caap_refactor.py --help
  python3 scripts/caap_refactor.py check <file>   # parse a file as a verification
  ```

- **Raw `Write`/`Edit` (or any text editor) is only for brand-new `.caap`
  files.** After creating one, still run a parse check on it:

  ```bash
  python3 scripts/caap_refactor.py check <file>
  ```

Behavior-preserving refactors of `.caap` code must be verified by
golden-comparing lowered output (AST / LLVM-IR before vs after must match).

## Definition of done — the strict gate

Before submitting, run the definition-of-done gate and make sure it is green:

```bash
scripts/strict-gate.sh
```

This runs, in order:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace          # falls back to `cargo test --workspace` if nextest is absent
cargo test --workspace --doc           # nextest does not run doctests
```

Tests run via [`cargo-nextest`](https://nexte.st/), which executes every test
across all binaries in one parallel pool (~2x faster than `cargo test`'s
sequential per-binary run on this workspace). Install it once:

```bash
cargo install cargo-nextest --locked
```

Useful focused runs while iterating:

```bash
cargo fmt --all -- --check                          # formatting only
cargo nextest run -p caap_core                      # one crate (also caap_peg, caap_cli, caap_sys_runtime)
cargo nextest run -p caap_core -E 'test(<name>)'    # a single test by name
```

If you touch host or stdlib behavior, also run the slower acceptance workflows:

```bash
scripts/test-acceptance.sh    # Rust tests marked #[ignore = "acceptance: ..."]
scripts/production-gate.sh    # strict gate + acceptance + bench compile
```

## Test tiers

The taxonomy is defined in [`docs/testing.md`](docs/testing.md). Put each new
test in the right tier:

- **Unit** — `src/**` under `#[cfg(test)]`. One module/parser-rule/data-structure
  boundary, in-memory fixtures, no full stdlib bootstrap, no CLI spawning.
- **Integration** — `*/tests/*.rs`, non-ignored. Crate-public APIs and
  binary/API boundaries with small, realistic scenarios; each test owns its own
  filesystem paths.
- **Acceptance** — regular Rust tests marked `#[ignore = "acceptance: ..."]`.
  Full, intentionally slow end-to-end workflows that exercise real
  stdlib/bootstrap behavior; run via `scripts/test-acceptance.sh`.

Test rules: one behavior per test, no reliance on test order, no shared temp
file names across parallel runs, and no `#[ignore]` without an explicit
`acceptance:` reason.

## Continuous integration

`.github/workflows/` gates every push and pull request:

- `workspace-ci.yml` runs five jobs across the whole workspace — `rustfmt
  --check`, `build`, the full `cargo test --workspace` suite (unit + integration
  + doctests, including the CAAP-native stdlib tests), `clippy -D warnings`, and
  the acceptance suite (`scripts/test-acceptance.sh`).
- `peg-ci.yml` keeps the `caap-peg` / `caap-peg-derive` crates fmt/clippy/test/
  doc clean and MSRV-pinned (Rust 1.82) independently of the rest of the
  workspace.

A pull request must be green. Running `scripts/strict-gate.sh` locally covers
the fmt/clippy/test jobs before you push.

## Commit hygiene

- **Atomic commits**: one commit is one logically complete change.
- **Commit `.agents/` changes separately from code.** `.agents/` is the
  canonical home for agent skills, playbooks, and conventions (the `.claude/`
  copies are materialized from it via `.agents/sync.py` — don't hand-edit
  `.claude/skills/*`).
- **Behavior-preserving by default.** Breaking changes must be stated by the
  task, never silent. No silent fallbacks — a contract violation must surface as
  a diagnostic/error, not a magic correction (principle #11).
- **Ground everything in real code.** Verify a primitive exists (grep,
  `KERNEL_REFERENCE.md`, `docs/builtins.md`, or a test) before using it — the
  builtin set changes. Don't invent APIs or paths; mark uncertainty explicitly.

## License

CAAP is licensed under the Apache License, Version 2.0 (see
[`LICENSE`](LICENSE) and [`NOTICE`](NOTICE)). By contributing, you agree that
your contributions are licensed under the same terms.
