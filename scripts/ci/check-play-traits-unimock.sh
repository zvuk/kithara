#!/usr/bin/env bash
# Ensure all kithara-play traits are configured for unimock generation.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
TRAITS_DIR="$ROOT_DIR/crates/kithara-play/src/traits"

missing=()
checked=0

while IFS= read -r file; do
  if rg -q 'pub trait ' "$file"; then
    checked=$((checked + 1))
    if ! rg -q 'unimock::unimock\(' "$file"; then
      missing+=("${file#$ROOT_DIR/}")
    fi
  fi
done < <(rg --files "$TRAITS_DIR" -g '*.rs' | sort)

if (( ${#missing[@]} > 0 )); then
  echo "FAILED: traits without unimock in kithara-play:"
  for path in "${missing[@]}"; do
    echo "  - $path"
  done
  exit 1
fi

echo "OK: kithara-play traits unimock coverage is complete ($checked files with traits checked)."
