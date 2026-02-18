#!/usr/bin/env bash
# Advisory report: trait files with/without unimock annotation.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
OUTPUT_PATH="${TRAIT_MOCK_AUDIT_OUTPUT:-$ROOT_DIR/target/trait-mock-audit.md}"

mkdir -p "$(dirname "$OUTPUT_PATH")"

rows=()
total_trait_files=0
unimock_trait_files=0
missing_unimock_trait_files=0

while IFS= read -r file; do
  trait_count="$( (rg -n 'pub trait ' "$file" || true) | wc -l | tr -d ' ' )"
  if (( trait_count == 0 )); then
    continue
  fi

  total_trait_files=$((total_trait_files + 1))
  unimock_count="$( (rg -n 'unimock(::unimock)?\(' "$file" || true) | wc -l | tr -d ' ' )"
  rel="${file#$ROOT_DIR/}"

  if (( unimock_count > 0 )); then
    unimock_trait_files=$((unimock_trait_files + 1))
    rows+=("| $rel | $trait_count | $unimock_count | yes |")
  else
    missing_unimock_trait_files=$((missing_unimock_trait_files + 1))
    rows+=("| $rel | $trait_count | 0 | no |")
  fi
done < <(rg --files "$ROOT_DIR/crates" -g '*.rs' | sort)

{
  echo "# Trait Mock Audit"
  echo
  echo "- generated_at_utc: $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
  echo
  echo "## Summary"
  echo
  echo "| Metric | Count |"
  echo "| --- | ---: |"
  echo "| Trait files | $total_trait_files |"
  echo "| Trait files with unimock | $unimock_trait_files |"
  echo "| Trait files without unimock | $missing_unimock_trait_files |"
  echo
  echo "## Details"
  echo
  echo "| File | pub trait count | unimock attr count | has unimock |"
  echo "| --- | ---: | ---: | --- |"
  if (( ${#rows[@]} == 0 )); then
    echo "| (none) | 0 | 0 | n/a |"
  else
    printf '%s\n' "${rows[@]}"
  fi
} >"$OUTPUT_PATH"

echo "==> trait mock audit written to $OUTPUT_PATH"
