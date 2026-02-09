#!/bin/bash
# Compare performance test results with baseline
#
# Usage:
#   ./scripts/compare_perf.sh <current_results.txt> <baseline_results.txt> [threshold_percent]
#
# Exit codes:
#   0 - No significant regression
#   1 - Regression detected above threshold
#   2 - Error (missing files, etc.)

set -euo pipefail

CURRENT="${1:-}"
BASELINE="${2:-baseline-results.txt}"
THRESHOLD="${3:-10}"  # Default: fail if >10% slower

if [[ -z "$CURRENT" ]]; then
    echo "Usage: $0 <current_results.txt> [baseline_results.txt] [threshold_percent]"
    exit 2
fi

if [[ ! -f "$CURRENT" ]]; then
    echo "Error: Current results file not found: $CURRENT"
    exit 2
fi

if [[ ! -f "$BASELINE" ]]; then
    echo "Warning: Baseline file not found: $BASELINE"
    echo "Run tests on main branch to create baseline"
    exit 0  # Don't fail if baseline missing
fi

echo "Comparing performance results..."
echo "Current:  $CURRENT"
echo "Baseline: $BASELINE"
echo "Threshold: ${THRESHOLD}%"
echo ""

# Extract average times from hotpath output
# Format: | function_name | Calls | Avg | P95 | Total | % Total |
extract_metrics() {
    local file="$1"
    # Extract lines with timing data (contains 'µs' or 'ms' or 'ns')
    grep -E '\|.*\|.*[0-9]+\.[0-9]+(µs|ms|ns)' "$file" 2>/dev/null | \
        awk -F'|' '{gsub(/^[ \t]+|[ \t]+$/, "", $2); gsub(/^[ \t]+|[ \t]+$/, "", $4); print $2 ":" $4}' | \
        grep -v "Function" | \
        sed 's/://g'
}

# Convert time to nanoseconds for comparison
to_nanoseconds() {
    local value="$1"
    if [[ "$value" =~ ([0-9.]+)(ns|µs|ms|s) ]]; then
        local num="${BASH_REMATCH[1]}"
        local unit="${BASH_REMATCH[2]}"
        case "$unit" in
            ns) echo "$num" ;;
            µs) echo "$num * 1000" | bc ;;
            ms) echo "$num * 1000000" | bc ;;
            s)  echo "$num * 1000000000" | bc ;;
        esac
    else
        echo "0"
    fi
}

REGRESSION_FOUND=0
COMPARISON_COUNT=0

echo "Function | Current | Baseline | Change"
echo "---------|---------|----------|-------"

# Simple comparison: look for common function names
while IFS=: read -r func current_time; do
    # Clean function name
    func=$(echo "$func" | xargs)
    current_time=$(echo "$current_time" | xargs)

    # Find baseline for same function
    baseline_time=$(grep "^$func " "$BASELINE" 2>/dev/null | awk '{print $2}' | head -1 || echo "")

    if [[ -n "$baseline_time" ]]; then
        current_ns=$(to_nanoseconds "$current_time")
        baseline_ns=$(to_nanoseconds "$baseline_time")

        if [[ $(echo "$baseline_ns > 0" | bc) -eq 1 ]]; then
            change=$(echo "scale=2; (($current_ns - $baseline_ns) / $baseline_ns) * 100" | bc)

            echo "$func | $current_time | $baseline_time | ${change}%"

            # Check if regression exceeds threshold
            if [[ $(echo "$change > $THRESHOLD" | bc) -eq 1 ]]; then
                echo "  ⚠️  REGRESSION: $func is ${change}% slower (threshold: ${THRESHOLD}%)"
                REGRESSION_FOUND=1
            fi

            ((COMPARISON_COUNT++))
        fi
    fi
done < <(extract_metrics "$CURRENT" | head -20)

echo ""
echo "Comparisons made: $COMPARISON_COUNT"

if [[ $REGRESSION_FOUND -eq 1 ]]; then
    echo "❌ Performance regression detected!"
    exit 1
else
    echo "✅ No significant regression detected"
    exit 0
fi
