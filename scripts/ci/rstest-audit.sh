#!/usr/bin/env bash
# Advisory audit: list test files that likely benefit from rstest parameterization.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
OUTPUT_PATH="${RSTEST_AUDIT_OUTPUT:-$ROOT_DIR/target/rstest-audit.md}"

mkdir -p "$(dirname "$OUTPUT_PATH")"

candidate_files=()
total_files=0
total_plain_tests=0
total_rstest_cases=0

while IFS= read -r file; do
  total_files=$((total_files + 1))

  plain_tests="$( (rg -n '#\[(tokio::)?test\]' "$file" || true) | wc -l | tr -d ' ' )"
  rstest_cases="$( (rg -n '#\[rstest\]' "$file" || true) | wc -l | tr -d ' ' )"
  total_plain_tests=$((total_plain_tests + plain_tests))
  total_rstest_cases=$((total_rstest_cases + rstest_cases))

  if (( plain_tests >= 3 && rstest_cases == 0 )); then
    candidate_files+=("${file#$ROOT_DIR/}|$plain_tests")
  fi
done < <(rg --files "$ROOT_DIR/crates" "$ROOT_DIR/tests" -g '*.rs' | sort)

{
  echo "# Rstest Audit"
  echo
  echo "- generated_at_utc: $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
  echo
  echo "## Summary"
  echo
  echo "| Metric | Count |"
  echo "| --- | ---: |"
  echo "| Rust files scanned | $total_files |"
  echo "| Plain #[test]/#[tokio::test] markers | $total_plain_tests |"
  echo "| #[rstest] markers | $total_rstest_cases |"
  echo "| Migration candidates | ${#candidate_files[@]} |"
  echo
  echo "## Candidates"
  echo
  echo "| File | Plain test markers |"
  echo "| --- | ---: |"
  if (( ${#candidate_files[@]} == 0 )); then
    echo "| (none) | 0 |"
  else
    for item in "${candidate_files[@]}"; do
      IFS='|' read -r file plain_tests <<<"$item"
      echo "| $file | $plain_tests |"
    done
  fi
} >"$OUTPUT_PATH"

echo "rstest audit: scanned $total_files Rust test files"
echo "rstest audit: report written to $OUTPUT_PATH"
if (( ${#candidate_files[@]} == 0 )); then
  echo "rstest audit: no obvious parameterization candidates found"
  exit 0
fi

echo "rstest audit: candidates for rstest migration:"
for item in "${candidate_files[@]}"; do
  IFS='|' read -r file plain_tests <<<"$item"
  echo "  - $file (plain_tests=$plain_tests)"
done
