#!/usr/bin/env bash
# Validate and optionally run Criterion benchmarks.

set -euo pipefail

run_cargo_no_proxy() {
  HTTP_PROXY= HTTPS_PROXY= http_proxy= https_proxy= ALL_PROXY= all_proxy= \
    NO_PROXY= no_proxy= cargo "$@"
}

echo "==> checking criterion benchmark builds..."
run_cargo_no_proxy bench -p kithara-abr --bench abr_estimator --no-run

if [[ "${RUN_BENCHMARKS:-0}" != "1" ]]; then
  echo "==> benchmark execution skipped (set RUN_BENCHMARKS=1 to execute)" \
    | tee bench-results.txt
  echo "==> bench results written to bench-results.txt"
  exit 0
fi

sample_size="${BENCH_SAMPLE_SIZE:-20}"
echo "==> running criterion benchmark (sample-size=${sample_size})..."
run_cargo_no_proxy bench -p kithara-abr --bench abr_estimator -- --sample-size \
  "${sample_size}" 2>&1 | tee bench-results.txt

echo "==> bench results written to bench-results.txt"
