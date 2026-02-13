#!/usr/bin/env bash
# Run performance tests and save results.
# Shared between GitHub Actions and GitLab CI.

set -euo pipefail

echo "==> running performance tests..."
cargo test -p kithara-integration-tests --features perf --release \
  -- --ignored --test-threads=1 --nocapture 2>&1 | tee perf-results.txt

echo "==> perf results written to perf-results.txt"
