#!/usr/bin/env bash
# Validate and optionally run Criterion benchmarks.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

if command -v just >/dev/null 2>&1; then
    exec just bench-ci
fi

echo "==> 'just' is not installed; running compatibility bench flow"

echo "==> checking criterion benchmark builds..."
cargo bench -p kithara-abr --bench abr_estimator --no-run

if [[ "${RUN_BENCHMARKS:-0}" != "1" ]]; then
  echo "==> benchmark execution skipped (set RUN_BENCHMARKS=1 to execute)" \
    | tee bench-results.txt
  echo "==> bench results written to bench-results.txt"
  exit 0
fi

sample_size="${BENCH_SAMPLE_SIZE:-20}"
candidate_name="${BENCH_CANDIDATE_NAME:-ci}"
echo "==> running criterion benchmark (sample-size=${sample_size}, baseline=${candidate_name})..."
cargo bench -p kithara-abr --bench abr_estimator \
  -- --sample-size "${sample_size}" --save-baseline "${candidate_name}" \
  2>&1 | tee bench-results.txt

if [[ -n "${BENCH_COMPARE_BASELINE_NAME:-}" ]]; then
  if command -v critcmp >/dev/null 2>&1; then
    baseline_name="${BENCH_COMPARE_BASELINE_NAME}"
    critcmp "${baseline_name}" "${candidate_name}" 2>&1 | tee -a bench-results.txt
  else
    echo "FAILED: critcmp is required when BENCH_COMPARE_BASELINE_NAME is set" | tee -a bench-results.txt
    exit 1
  fi
fi

echo "==> bench results written to bench-results.txt"
