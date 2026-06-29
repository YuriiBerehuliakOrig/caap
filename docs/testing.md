# CAAP Test Architecture

This document defines the test taxonomy for the repository. The goal is that
tests are atomic, isolated, independent, fast for their tier, and named as a
scenario.

## Test Tiers

### Unit Tests

Location: `src/**` under `#[cfg(test)]`.

Purpose:

- verify one module, parser rule, data structure, normalizer, validator, or
  small service boundary;
- use in-memory fixtures by default;
- avoid full stdlib bootstrap, CLI binary spawning, external tools, or shared
  filesystem state.

Current groups:

- `caap_core` unit tests: compiler/session, provider/effect contracts, IR,
  semantic graph, source spans, runtime loader, host registry, frontend helpers.
- `caap_peg` unit tests: grammar, parser engine internals, recovery,
  validation, mutation, signatures, registry, S-expression loader.
- `caap_sys_runtime` unit tests: FFI boundary, catalog, fs/io/net/os/path/proc/time
  argument validation and explicit timeout behavior.
- `caap_cli` unit tests: command dispatch helpers, non-bootstrap command
  behavior, diagnostic rendering, canonicalization, AST JSON roundtrip.

Rules:

- one behavior per test;
- no reliance on test order;
- temp files must include process/thread or a unique suffix and must be cleaned
  up;
- no silent skip unless the test is explicitly about optional host tooling;
- test names must describe the scenario and expected behavior.

### Integration Tests

Location: `*/tests/*.rs`, non-ignored.

Purpose:

- verify crate-public APIs and binary/API boundaries;
- use realistic but small scenarios;
- may use temp files/directories, but each test owns its own paths;
- should avoid full demo-scale bootstrap if a smaller fixture covers the same
  contract.

Current groups:

- `caap-cli/tests/cli_tests.rs`: binary-level CLI smoke without bootstrap.
- `caap-cli/tests/cli_scenarios.rs`: stdlib-backed CLI scenarios, excluding
  optional native/clang paths that self-skip when host tools are absent.
- `caap/tests/core_state_tests.rs`: focused core state scenarios for IR graph
  invariants, unit lifecycle, source spans, semantic graph, and builtin
  metadata.
- `caap/tests/runtime_language_tests.rs`: focused runtime/language scenarios for
  literals, arithmetic, control flow, lambdas, application helpers, runtime
  predicates, and evaluator diagnostics.
- `caap/tests/runtime_collection_tests.rs`: focused runtime collection, string,
  sequence, map ordering, and stable-hash scenarios.
- `caap/tests/compiler_cache_tests.rs`: focused compiler event log,
  artifact-cache, source-artifact, source-template cache, and cache lineage
  scenarios.
- `caap/tests/compiler_session_tests.rs`: focused compiler session, registry,
  CTFE compiler API, and query-surface scenarios.
- `caap/tests/compiler_services_tests.rs`: focused compiler service builtins,
  fact schema registration, semantic policy, catalog, evaluation, and query
  service scenarios.
- `caap/tests/ctfe_ir_builder_tests.rs`: focused CTFE IR construction,
  detached expression specs, node projection, and stdlib builder composition
  scenarios.
- `caap/tests/ctfe_compiler_builtins_tests.rs`: focused CTFE compiler query,
  query-plan projection, evaluation capture, bootstrap execution, and source
  template scenarios.
- `caap/tests/ctfe_unit_node_builtins_tests.rs`: focused CTFE unit, node,
  metadata, surface-form, rewrite-report, and generic mutation/projection
  scenarios.
- `caap/tests/ctfe_provider_tests.rs`: focused CTFE provider context,
  effect/capability enforcement, traversal, diagnostics, execution records,
  call descriptors, and compile-time fold scenarios.
- `caap/tests/host_system_tests.rs`: focused host function value, host service
  registry, host capability policy, host builtins, and phase-boundary
  scenarios.
- `caap/tests/host_system_library_tests.rs`: focused concrete CAAP SYS
  fs/path/net/process/os/io/time export and native sandbox scenarios.
- `caap/tests/query_effect_tests.rs`: focused query/native-provider effect
  enforcement, effect allowlist, and semantic transaction scenarios.
- `caap/tests/query_pipeline_tests.rs`: focused query planning, provider
  ordering, rollback, restart, and stage dependency scenarios.
- `caap/tests/query_cache_replay_tests.rs`: focused query cache keys, CTFE
  replay, cache invalidation, and cache rollback scenarios.
- `caap/tests/bootstrap_session_tests.rs`: focused bootstrap execution,
  bootstrap image, trust policy, virtual file, and cross-unit link scenarios.
- `caap/tests/ctfe_peg_grammar_tests.rs`: focused CTFE PEG builder and grammar
  runtime builtin scenarios.
- `caap/tests/stdlib_*_tests.rs`: stdlib proof-gate split by area: forms,
  loader, import/use/re_export, discovery, projects, type/effect checks, sys
  facades, surface protocol, LSP/DAP command contracts, governance, reference
  drift, specialization, and native codegen scenarios.
- `caap/tests/native_meta_tests.rs`: pins the canonical native head/type
  vocabulary (the single declarative source `stdlib.backend.native_meta`) so
  adding/removing a native head is a deliberate change, and verifies the WAT
  (wasm) emitter rejects `atomic_load` with a precise located diagnostic rather
  than the generic "unsupported form".
- `caap/tests/strict_native_profile_tests.rs`: the opt-in strict native profile:
  strict mode rejects an unknown declared type (an extern result `u33`) while the
  permissive default passes the same program clean.
- `caap/tests/tinylogfs_tests.rs`: declarative binary-layout compiler and
  TinyLogFS generated eval/native artifacts.
- `peg/tests/*.rs`: public PEG parser, registry, incremental parsing, invalid
  rule, and analysis scenarios.

Rules:

- tests should be runnable independently by exact name;
- each filesystem scenario creates and deletes only its own fixture path;
- assertions must check the observable contract: output, diagnostics, exit
  code, structured value, or state transition;
- no dependency on previous bootstrap/session/cache state.

### Acceptance Tests

Location: regular Rust tests marked `#[ignore = "acceptance: ..."]`.

Purpose:

- verify full product workflows that are intentionally slower;
- exercise real stdlib/bootstrap/demo behavior end-to-end;
- remain explicit so normal unit/integration runs stay fast.

Current acceptance tests are whatever is explicitly marked ignored with an
`acceptance:` reason in the Rust test tree. The old v1 purity-pass and
stdlib-smoke acceptance entries were removed with the v1 stdlib.

Acceptance command:

```bash
scripts/test-acceptance.sh
```

The script runs all ignored Rust tests across the workspace. Keep ignored tests
reserved for acceptance workflows, or add a separate script path before
introducing a non-acceptance ignored test.

### Benchmarks

Location: `*/benches/*.rs`.

Purpose:

- measure performance, not correctness;
- compile in the production gate so API drift breaks early;
- run Criterion benches only when explicitly requested.

Commands:

```bash
cargo bench --workspace --no-run
CAAP_RUN_BENCHMARKS=1 scripts/production-gate.sh
```

## Gate Commands

Fast correctness gate:

```bash
scripts/strict-gate.sh
```

This runs formatting, strict clippy, all non-ignored unit/integration tests, and
doctests. Tests run via **`cargo-nextest`**, which executes every test across all
binaries in one parallel pool instead of `cargo test`'s sequential binary-by-binary
run — on this workspace ~170s → ~78s. Install it once with
`cargo install cargo-nextest --locked`; the gate falls back to `cargo test` if it
is absent (nextest skips doctests, so the gate runs `cargo test --doc` separately).

Full production gate:

```bash
scripts/production-gate.sh
```

This runs the strict gate, acceptance tests, and benchmark target compilation.

## Current Classification Snapshot

Generated from the current workspace shape after the stdlib migration.

| Package | Unit | Integration | Acceptance | Bench |
|---|---:|---:|---:|---:|
| `caap_cli` | command helper and non-bootstrap command tests | binary smoke, stdlib-backed CLI scenarios | explicit ignored acceptance only | CLI compile workflow |
| `caap_core` | core/session/provider/evaluator/semantic/host tests | focused files under `caap/tests/*_tests.rs`, including `stdlib_*_tests.rs` and `tinylogfs_tests.rs` | explicit ignored acceptance only | evaluator/query pipeline |
| `caap_peg` | parser internals, grammar, recovery, validation | public parser/registry/incremental scenarios | none | PEG parse/memo/analysis |
| `caap_sys_runtime` | catalog/FFI/system primitive validation | none | none | none |

## Anti-Patterns

Do not add:

- full bootstrap demo tests inside `src/** #[cfg(test)]`;
- tests that share a temp file name across parallel runs;
- tests that pass only because another test registered global state first;
- broad "does everything" tests when a smaller behavior test is possible;
- ignored tests without an explicit acceptance reason.
- manual perf/demo checks as `#[ignore]` tests; make them normal tests that
  return early unless an explicit environment variable is set.
