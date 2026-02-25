#!/bin/bash
# Run WASM integration tests in headless Chrome with rayon thread pool.
#
# The wasm_test_runner binary automatically starts/stops the fixture server,
# so no manual server management is needed.
#
# Prerequisites:
#   - wasm-bindgen-cli: cargo install wasm-bindgen-cli
#   - chromedriver installed and in PATH (or set CHROMEDRIVER env var)
#   - Nightly Rust toolchain (for build-std)
#
# Usage:
#   bash scripts/ci/wasm-test.sh

set -euo pipefail

echo "=== Running WASM integration tests (nightly, build-std) ==="
CHROMEDRIVER="${CHROMEDRIVER:-chromedriver}" \
WASM_BINDGEN_TEST_TIMEOUT=300 \
cargo +nightly test --target wasm32-unknown-unknown \
    -p kithara-integration-tests --test integration

echo "=== WASM tests passed ==="
