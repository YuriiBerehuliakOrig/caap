#!/usr/bin/env bash
set -euo pipefail

# The name-first CLI scenarios parse/evaluate deeply recursive surface programs.
# Rust spawns each test on a ~2MB thread by default, which overflows before the
# real `caap` binary (8MB+ main thread) ever would. Give test threads a larger
# stack so the gate is reproducible from a clean clone. A caller-set value wins.

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings

# Tests run via cargo-nextest: it executes every test across ALL binaries in one
# parallel pool, whereas `cargo test` runs the test binaries sequentially — on this
# workspace that is ~170s -> ~78s. Install once: `cargo install cargo-nextest --locked`.
# Falls back to `cargo test` if nextest is absent so the gate still works everywhere.
if command -v cargo-nextest >/dev/null 2>&1; then
  cargo nextest run --workspace
else
  echo "note: cargo-nextest not found — using the slower 'cargo test' fallback." >&2
  echo "      install it for ~2x faster runs: cargo install cargo-nextest --locked" >&2
  cargo test --workspace
fi

# nextest does not run doctests; `cargo test` did, so run them separately to keep
# the same coverage. (No-op build if the workspace has no doctests.)
cargo test --workspace --doc
