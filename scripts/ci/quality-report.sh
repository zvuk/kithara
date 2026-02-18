#!/usr/bin/env bash
# Generate static testing/code-quality inventory report.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
OUTPUT_PATH="${QUALITY_REPORT_OUTPUT:-$ROOT_DIR/quality-report.md}"

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
bench_targets="$(count_files_rg '*/benches/*.rs' "$ROOT_DIR/crates" "$ROOT_DIR/tests")"
local_http_servers="$(count_rg 'TcpListener::bind\("127\.0\.0\.1:0"\)' "$ROOT_DIR/tests")"

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
  echo "## Notes"
  echo
  echo "- This report is inventory-oriented and intentionally non-blocking."
  echo "- Use it to track trendlines while tightening specific gates separately."
} >"$OUTPUT_PATH"

echo "==> quality report written to $OUTPUT_PATH"
