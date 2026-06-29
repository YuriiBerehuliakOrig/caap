#!/usr/bin/env bash
set -euo pipefail

# Acceptance tests are explicit Rust tests marked ignored with an
# `acceptance:` reason. Keep this as one workspace command so newly added
# acceptance scenarios are picked up without editing the script.
if command -v cargo-nextest >/dev/null 2>&1; then
  cargo nextest run --workspace --run-ignored ignored-only
else
  cargo test --workspace -- --ignored
fi
