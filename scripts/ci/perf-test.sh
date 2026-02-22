#!/usr/bin/env bash
# Run performance tests and save results.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

if command -v just >/dev/null 2>&1; then
    exec just perf-test
fi

echo "==> 'just' is not installed; running compatibility perf flow"
cargo test -p kithara-integration-tests --features perf --release \
  -- --ignored --test-threads=1 --nocapture 2>&1 | tee perf-results.txt

echo "==> perf results written to perf-results.txt"
