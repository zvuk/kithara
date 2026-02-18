#!/usr/bin/env bash
# Generate static testing/code-quality inventory report.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
OUTPUT_PATH="${QUALITY_REPORT_OUTPUT:-$ROOT_DIR/target/quality-report.md}"

MIN_UNIMOCK_TRAITS="${QUALITY_MIN_UNIMOCK_TRAITS:-}"
MIN_RSTEST_CASES="${QUALITY_MIN_RSTEST_CASES:-}"
MIN_PERF_TEST_FILES="${QUALITY_MIN_PERF_TEST_FILES:-}"
MIN_BENCH_TARGETS="${QUALITY_MIN_BENCH_TARGETS:-}"
MAX_LOCAL_HTTP_SERVERS="${QUALITY_MAX_LOCAL_HTTP_SERVERS:-}"

mkdir -p "$(dirname "$OUTPUT_PATH")"

count_rg() {
  local pattern="$1"
  shift
  (rg -n "$pattern" "$@" 2>/dev/null || true) | wc -l | tr -d ' '
}

count_files_rg() {
  local glob="$1"
  shift
  (rg --files "$@" -g "$glob" 2>/dev/null || true) | wc -l | tr -d ' '
}

total_traits="$(count_rg 'pub trait ' "$ROOT_DIR/crates")"
unimock_traits="$(count_rg 'unimock::unimock\(' "$ROOT_DIR/crates")"
rstest_cases="$(count_rg '#\[rstest\]' "$ROOT_DIR/crates" "$ROOT_DIR/tests")"
plain_tests="$(count_rg '#\[(tokio::)?test\]' "$ROOT_DIR/crates" "$ROOT_DIR/tests")"
perf_tests="$(count_files_rg '*.rs' "$ROOT_DIR/tests/perf")"
bench_targets="$(count_files_rg '**/benches/*.rs' "$ROOT_DIR/crates" "$ROOT_DIR/tests")"
local_http_servers="$(count_rg 'TcpListener::bind\("127\.0\.0\.1:0"\)' "$ROOT_DIR/tests")"

check_min() {
  local label="$1"
  local value="$2"
  local threshold="$3"
  if [[ -z "$threshold" ]]; then
    return 0
  fi
  if (( value < threshold )); then
    echo "FAILED: $label is $value (minimum: $threshold)"
    return 1
  fi
  return 0
}

check_max() {
  local label="$1"
  local value="$2"
  local threshold="$3"
  if [[ -z "$threshold" ]]; then
    return 0
  fi
  if (( value > threshold )); then
    echo "FAILED: $label is $value (maximum: $threshold)"
    return 1
  fi
  return 0
}

passes=true
gate_errors=()

if ! check_min "unimock traits" "$unimock_traits" "$MIN_UNIMOCK_TRAITS"; then
  passes=false
  gate_errors+=("unimock_traits")
fi
if ! check_min "rstest cases" "$rstest_cases" "$MIN_RSTEST_CASES"; then
  passes=false
  gate_errors+=("rstest_cases")
fi
if ! check_min "perf test files" "$perf_tests" "$MIN_PERF_TEST_FILES"; then
  passes=false
  gate_errors+=("perf_tests")
fi
if ! check_min "bench targets" "$bench_targets" "$MIN_BENCH_TARGETS"; then
  passes=false
  gate_errors+=("bench_targets")
fi
if ! check_max "local HTTP server bind sites" "$local_http_servers" "$MAX_LOCAL_HTTP_SERVERS"; then
  passes=false
  gate_errors+=("local_http_servers")
fi

{
  echo "# Quality Report"
  echo
  echo "- generated_at_utc: $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
  echo
  echo "## Inventory"
  echo
  echo "| Metric | Count |"
  echo "| --- | ---: |"
  echo "| Traits in crates/ | $total_traits |"
  echo "| Traits with unimock::unimock | $unimock_traits |"
  echo "| rstest test cases | $rstest_cases |"
  echo "| Plain #[test]/#[tokio::test] markers | $plain_tests |"
  echo "| perf test files (tests/perf) | $perf_tests |"
  echo "| bench targets (*/benches/*.rs) | $bench_targets |"
  echo "| Local HTTP server bind sites in tests | $local_http_servers |"
  echo
  echo "## Gates"
  echo
  echo "| Gate | Threshold | Status |"
  echo "| --- | ---: | --- |"
  echo "| Traits with unimock::unimock | ${MIN_UNIMOCK_TRAITS:-off} | $( [[ -z "$MIN_UNIMOCK_TRAITS" || "$unimock_traits" -ge "$MIN_UNIMOCK_TRAITS" ]] && echo "pass" || echo "fail" ) |"
  echo "| rstest test cases | ${MIN_RSTEST_CASES:-off} | $( [[ -z "$MIN_RSTEST_CASES" || "$rstest_cases" -ge "$MIN_RSTEST_CASES" ]] && echo "pass" || echo "fail" ) |"
  echo "| perf test files | ${MIN_PERF_TEST_FILES:-off} | $( [[ -z "$MIN_PERF_TEST_FILES" || "$perf_tests" -ge "$MIN_PERF_TEST_FILES" ]] && echo "pass" || echo "fail" ) |"
  echo "| bench targets | ${MIN_BENCH_TARGETS:-off} | $( [[ -z "$MIN_BENCH_TARGETS" || "$bench_targets" -ge "$MIN_BENCH_TARGETS" ]] && echo "pass" || echo "fail" ) |"
  echo "| local HTTP server bind sites | ${MAX_LOCAL_HTTP_SERVERS:-off} | $( [[ -z "$MAX_LOCAL_HTTP_SERVERS" || "$local_http_servers" -le "$MAX_LOCAL_HTTP_SERVERS" ]] && echo "pass" || echo "fail" ) |"
  echo
  echo "## Notes"
  echo
  echo "- Inventory is always generated; gates are optional and env-driven."
  echo "- Failing gates exit non-zero to make regressions visible in CI."
} >"$OUTPUT_PATH"

echo "==> quality report written to $OUTPUT_PATH"

if [[ "$passes" != "true" ]]; then
  echo "==> quality gates failed: ${gate_errors[*]}"
  exit 1
fi
