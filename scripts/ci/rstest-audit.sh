#!/usr/bin/env bash
# Advisory audit: list test files that likely benefit from rstest parameterization.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"

candidate_files=()
total_files=0

while IFS= read -r file; do
  total_files=$((total_files + 1))

  plain_tests="$( (rg -n '#\[(tokio::)?test\]' "$file" || true) | wc -l | tr -d ' ' )"
  rstest_cases="$( (rg -n '#\[rstest\]' "$file" || true) | wc -l | tr -d ' ' )"

  if (( plain_tests >= 3 && rstest_cases == 0 )); then
    candidate_files+=("${file#$ROOT_DIR/} (plain_tests=$plain_tests)")
  fi
done < <(rg --files "$ROOT_DIR/crates" "$ROOT_DIR/tests" -g '*.rs' | sort)

echo "rstest audit: scanned $total_files Rust test files"
if (( ${#candidate_files[@]} == 0 )); then
  echo "rstest audit: no obvious parameterization candidates found"
  exit 0
fi

echo "rstest audit: candidates for rstest migration:"
for item in "${candidate_files[@]}"; do
  echo "  - $item"
done
