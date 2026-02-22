#!/usr/bin/env bash
# Coverage entrypoint for CI and local runs.
# Primary path: just coverage
# Compatibility path: direct cargo llvm-cov execution.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

if command -v just >/dev/null 2>&1; then
    exec just coverage
fi

echo "==> 'just' is not installed; running compatibility coverage flow"

COVERAGE_MIN="${COVERAGE_MIN:-80}"
OUTPUT_DIR="${COVERAGE_OUTPUT_DIR:-./coverage}"

mkdir -p "$OUTPUT_DIR"

cargo llvm-cov \
  --workspace \
  --cobertura \
  --summary-only \
  --output-path "$OUTPUT_DIR/cobertura.xml" \
  --ignore-filename-regex '(tests/|examples/|benches/)' \
  --fail-under-lines "$COVERAGE_MIN"

echo "==> coverage report written to $OUTPUT_DIR/cobertura.xml"
echo "==> line coverage threshold: ${COVERAGE_MIN}%"
