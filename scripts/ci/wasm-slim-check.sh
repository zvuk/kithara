#!/bin/bash
# Run wasm-slim size budget check for kithara-wasm.
#
# Usage:
#   bash scripts/ci/wasm-slim-check.sh

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
CRATE_DIR="${ROOT_DIR}/crates/kithara-wasm"
RESULT_FILE="${ROOT_DIR}/target/wasm-slim-result.json"
TOOLCHAIN="${WASM_SLIM_TOOLCHAIN:-nightly}"

run_wasm_slim() {
    if command -v wasm-slim >/dev/null 2>&1; then
        wasm-slim "$@"
        return
    fi

    if command -v cargo-wasm-slim >/dev/null 2>&1; then
        cargo wasm-slim "$@"
        return
    fi

    echo "wasm-slim is not installed" >&2
    exit 2
}

mkdir -p "$(dirname "$RESULT_FILE")"
cd "$CRATE_DIR"

echo "Running wasm-slim budget check in ${CRATE_DIR}"
RUSTUP_TOOLCHAIN="$TOOLCHAIN" run_wasm_slim build --check --no-emoji --json >"$RESULT_FILE"
echo "wasm-slim report: ${RESULT_FILE}"
