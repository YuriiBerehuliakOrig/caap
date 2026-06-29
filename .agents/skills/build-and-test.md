---
name: build-and-test
description: Build the project, run tests/gates, or run the caap binary. Use before considering a change complete.
---

# Skill: build-and-test

Use this skill to build the project, run gates, run tests, or execute the `caap`
binary.

Shared rule: [`conventions.md`](../conventions.md) section 9.

## Verification Gates

Run from the repository root:

| Command | Purpose |
| --- | --- |
| `scripts/strict-gate.sh` | Fast gate: formatting, workspace tests, and clippy. |
| `scripts/test-acceptance.sh` | Ignored acceptance scenarios for CLI and stdlib smoke. |
| `scripts/production-gate.sh` | Strict + acceptance + benchmark compilation. Set `CAAP_RUN_BENCHMARKS=1` to run benchmarks. |

Default completion gate:

```bash
scripts/strict-gate.sh
```

## Test Tiers

See [`docs/testing.md`](../../docs/testing.md).

- Unit tests: `src/**` under `#[cfg(test)]`.
- Integration tests: `*/tests/*.rs`, not ignored.
- Acceptance tests: ignored end-to-end scenarios.

## Run The Binary

```bash
cargo build
./target/debug/caap <file>.caap
./target/debug/caap stdlib/bootstrap.caap tools/run.caap <file>.caap

cargo build --release
./target/release/caap <file>.caap
./target/release/caap stdlib/bootstrap.caap tools/run.caap <file>.caap
```

The CLI has no subcommands and no flags. Checking, AST dumps, and native emit
are tool programs:

```bash
./target/debug/caap tools/bare.caap tools/ast_json.caap <file>.caap
./target/debug/caap stdlib/bootstrap.caap tools/s2_emit.caap <file>.caap > out.ll
./target/debug/caap stdlib/bootstrap.caap tools/s2_build.caap <file>.caap out
```

## Focused Tests

```bash
cargo test -p caap-sys-runtime
cargo test -p caap-core --test host_system_tests
cargo test -p caap-cli --test cli_scenarios -- --ignored
```
