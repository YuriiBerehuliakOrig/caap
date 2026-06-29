#!/usr/bin/env bash
set -euo pipefail

scripts/strict-gate.sh

scripts/test-acceptance.sh

if [[ "${CAAP_RUN_BENCHMARKS:-0}" == "1" ]]; then
  cargo bench --workspace
else
  cargo bench --workspace --no-run
fi
