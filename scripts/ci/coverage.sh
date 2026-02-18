#!/usr/bin/env bash
# Generate code coverage with cargo-tarpaulin.
# Shared between GitHub Actions and GitLab CI.

set -euo pipefail

COVERAGE_MIN="${COVERAGE_MIN:-80}"
OUTPUT_DIR="${COVERAGE_OUTPUT_DIR:-./coverage}"

echo "==> generating coverage..."
cargo tarpaulin \
  --workspace \
  --timeout 300 \
  --out xml \
  --output-dir "$OUTPUT_DIR" \
  --fail-under "$COVERAGE_MIN" \
  --exclude-files 'tests/*' \
  --exclude-files 'examples/*' \
  --exclude-files 'benches/*'

report_path="$OUTPUT_DIR/cobertura.xml"
if [[ ! -f "$report_path" ]]; then
  echo "Error: coverage report not found at $report_path"
  exit 2
fi

line_rate="$(
  awk -F'"' '
    /line-rate=/ {
      for (i = 1; i <= NF; i++) {
        if ($i == "line-rate=") {
          print $(i + 1)
          exit
        }
      }
    }
  ' "$report_path"
)"

if [[ -z "$line_rate" ]]; then
  echo "Error: failed to parse line-rate from $report_path"
  exit 2
fi

coverage_percent="$(
  awk -v rate="$line_rate" 'BEGIN { printf "%.2f", rate * 100.0 }'
)"

echo "==> coverage report written to $OUTPUT_DIR/"
echo "==> line coverage: ${coverage_percent}% (minimum: ${COVERAGE_MIN}%)"
