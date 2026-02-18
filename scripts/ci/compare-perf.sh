#!/usr/bin/env bash
# Compare performance test results with baseline.
#
# Usage:
#   ./scripts/ci/compare-perf.sh <current_results.txt> <baseline_results.txt> [threshold_percent]
#
# Exit codes:
#   0 - no significant regression
#   1 - regression detected above threshold
#   2 - error (missing files, parse issues, etc.)

set -euo pipefail

CURRENT="${1:-}"
BASELINE="${2:-baseline-results.txt}"
THRESHOLD="${3:-10}"  # Fail if function is >THRESHOLD% slower.

if [[ -z "$CURRENT" ]]; then
    echo "Usage: $0 <current_results.txt> [baseline_results.txt] [threshold_percent]"
    exit 2
fi

if [[ ! -f "$CURRENT" ]]; then
    echo "Error: current results file not found: $CURRENT"
    exit 2
fi

if [[ ! -f "$BASELINE" ]]; then
    echo "Warning: baseline file not found: $BASELINE"
    echo "Run tests on main branch to create baseline"
    exit 0
fi

echo "Comparing performance results..."
echo "Current:  $CURRENT"
echo "Baseline: $BASELINE"
echo "Threshold: ${THRESHOLD}%"
echo ""

# Extract hotpath table rows as:
#   <function>\t<avg_time>
# where avg_time is one of: ns/us/ms/s.
extract_metrics() {
    local file="$1"

    awk -F'|' '
      {
        name = $2
        avg = $4
        gsub(/^[ \t]+|[ \t]+$/, "", name)
        gsub(/^[ \t]+|[ \t]+$/, "", avg)
        gsub(/µs/, "us", avg)

        if (name == "" || avg == "" || tolower(name) == "function") {
          next
        }
        if (avg !~ /^[0-9]+(\.[0-9]+)?(ns|us|ms|s)$/) {
          next
        }
        print name "\t" avg
      }
    ' "$file"
}

to_nanoseconds() {
    local value="$1"
    local num factor

    case "$value" in
        *ns)
            num="${value%ns}"
            factor="1"
            ;;
        *us)
            num="${value%us}"
            factor="1000"
            ;;
        *ms)
            num="${value%ms}"
            factor="1000000"
            ;;
        *s)
            num="${value%s}"
            factor="1000000000"
            ;;
        *)
            echo "0"
            return
            ;;
    esac

    awk -v n="$num" -v f="$factor" 'BEGIN { printf "%.6f\n", n * f }'
}

baseline_metrics="$(mktemp)"
current_metrics="$(mktemp)"
joined_metrics="$(mktemp)"
trap 'rm -f "$baseline_metrics" "$current_metrics" "$joined_metrics"' EXIT

extract_metrics "$BASELINE" | LC_ALL=C sort -u >"$baseline_metrics"
extract_metrics "$CURRENT" | LC_ALL=C sort -u >"$current_metrics"

if [[ ! -s "$baseline_metrics" ]]; then
    echo "Error: no parsable metrics found in baseline file"
    exit 2
fi

if [[ ! -s "$current_metrics" ]]; then
    echo "Error: no parsable metrics found in current file"
    exit 2
fi

join -t $'\t' -j 1 "$current_metrics" "$baseline_metrics" >"$joined_metrics" || true

regression_found=0
comparison_count=0

echo "Function | Current | Baseline | Change"
echo "---------|---------|----------|-------"

while IFS=$'\t' read -r name current_time baseline_time; do
    current_ns="$(to_nanoseconds "$current_time")"
    baseline_ns="$(to_nanoseconds "$baseline_time")"

    change="$(
        awk -v c="$current_ns" -v b="$baseline_ns" '
          BEGIN {
            if (b <= 0.0) {
              print "NaN"
            } else {
              printf "%.2f", ((c - b) / b) * 100.0
            }
          }
        '
    )"

    if [[ "$change" == "NaN" ]]; then
        continue
    fi

    echo "$name | $current_time | $baseline_time | ${change}%"
    comparison_count=$((comparison_count + 1))

    if awk -v change="$change" -v threshold="$THRESHOLD" \
        'BEGIN { exit(!(change > threshold)) }'
    then
        echo "  REGRESSION: $name is ${change}% slower (threshold: ${THRESHOLD}%)"
        regression_found=1
    fi
done <"$joined_metrics"

echo ""
echo "Comparisons made: $comparison_count"

if [[ "$comparison_count" -eq 0 ]]; then
    echo "Error: no overlapping metrics found between current and baseline"
    exit 2
fi

if [[ "$regression_found" -eq 1 ]]; then
    echo "Performance regression detected"
    exit 1
fi

echo "No significant regression detected"
exit 0
