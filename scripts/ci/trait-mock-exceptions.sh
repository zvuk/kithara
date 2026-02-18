#!/usr/bin/env bash
# Enforce explicit reasons for trait files that intentionally do not use unimock.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
EXCEPTIONS_FILE="${TRAIT_MOCK_EXCEPTIONS_FILE:-$ROOT_DIR/scripts/ci/trait-mock-exceptions.txt}"
OUTPUT_PATH="${TRAIT_MOCK_EXCEPTIONS_OUTPUT:-$ROOT_DIR/target/trait-mock-exceptions.md}"

mkdir -p "$(dirname "$OUTPUT_PATH")"

declare -A expected_reasons=()
expected_paths=()

if [[ ! -f "$EXCEPTIONS_FILE" ]]; then
  echo "FAILED: exceptions file not found: $EXCEPTIONS_FILE"
  exit 1
fi

while IFS='|' read -r raw_path raw_reason; do
  line="$(printf '%s|%s' "${raw_path:-}" "${raw_reason:-}")"
  [[ -z "${line//[[:space:]]/}" ]] && continue
  [[ "$raw_path" =~ ^[[:space:]]*# ]] && continue

  path="$(echo "${raw_path:-}" | xargs)"
  reason="$(echo "${raw_reason:-}" | xargs)"

  if [[ -z "$path" || -z "$reason" ]]; then
    echo "FAILED: malformed exceptions entry: $line"
    exit 1
  fi

  expected_reasons["$path"]="$reason"
  expected_paths+=("$path")
done <"$EXCEPTIONS_FILE"

actual_missing=()
declare -A actual_missing_set=()

while IFS= read -r file; do
  trait_count="$( (rg -n 'pub trait ' "$file" || true) | wc -l | tr -d ' ' )"
  (( trait_count == 0 )) && continue

  unimock_count="$( (rg -n 'unimock(::unimock)?\(' "$file" || true) | wc -l | tr -d ' ' )"
  if (( unimock_count == 0 )); then
    rel="${file#$ROOT_DIR/}"
    actual_missing+=("$rel")
    actual_missing_set["$rel"]=1
  fi
done < <(rg --files "$ROOT_DIR/crates" -g '*.rs' | sort)

unexpected_missing=()
for path in "${actual_missing[@]}"; do
  if [[ -z "${expected_reasons[$path]+_}" ]]; then
    unexpected_missing+=("$path")
  fi
done

stale_exceptions=()
for path in "${expected_paths[@]}"; do
  if [[ -z "${actual_missing_set[$path]+_}" ]]; then
    stale_exceptions+=("$path")
  fi
done

rows=()
for path in "${expected_paths[@]}"; do
  status="expected_missing"
  if [[ -z "${actual_missing_set[$path]+_}" ]]; then
    status="stale_exception"
  fi
  rows+=("| $path | ${expected_reasons[$path]} | $status |")
done
for path in "${unexpected_missing[@]}"; do
  rows+=("| $path | missing reason in $EXCEPTIONS_FILE | unexpected_missing |")
done

{
  echo "# Trait Mock Exceptions"
  echo
  echo "- generated_at_utc: $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
  echo
  echo "## Summary"
  echo
  echo "| Metric | Count |"
  echo "| --- | ---: |"
  echo "| Actual trait files without unimock | ${#actual_missing[@]} |"
  echo "| Declared exceptions | ${#expected_paths[@]} |"
  echo "| Unexpected missing unimock | ${#unexpected_missing[@]} |"
  echo "| Stale exceptions | ${#stale_exceptions[@]} |"
  echo
  echo "## Details"
  echo
  echo "| File | Reason | Status |"
  echo "| --- | --- | --- |"
  if (( ${#rows[@]} == 0 )); then
    echo "| (none) | n/a | n/a |"
  else
    printf '%s\n' "${rows[@]}"
  fi
} >"$OUTPUT_PATH"

echo "==> trait mock exceptions report written to $OUTPUT_PATH"

if (( ${#unexpected_missing[@]} > 0 )); then
  echo "FAILED: trait files without unimock and without explicit exceptions:"
  printf '  - %s\n' "${unexpected_missing[@]}"
  exit 1
fi

if (( ${#stale_exceptions[@]} > 0 )); then
  echo "FAILED: stale trait mock exceptions (remove from $EXCEPTIONS_FILE):"
  printf '  - %s\n' "${stale_exceptions[@]}"
  exit 1
fi

echo "OK: all trait files without unimock are explicitly documented."
