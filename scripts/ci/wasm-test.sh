#!/bin/bash
# Run WASM stress tests in headless Chrome with rayon thread pool.
#
# Prerequisites:
#   - wasm-bindgen-cli: cargo install wasm-bindgen-cli
#   - chromedriver installed and in PATH (or set CHROMEDRIVER env var)
#   - Nightly Rust toolchain (for build-std)
#
# Usage:
#   bash scripts/ci/wasm-test.sh

set -euo pipefail

FIXTURE_PORT=3333
FIXTURE_URL="http://127.0.0.1:${FIXTURE_PORT}/master.m3u8"

cleanup() {
    if [ -n "${SERVER_PID:-}" ]; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

echo "=== Building HLS fixture server ==="
cargo build --bin hls_fixture_server

echo "=== Starting HLS fixture server on port ${FIXTURE_PORT} ==="
cargo run --bin hls_fixture_server &
SERVER_PID=$!

# Wait for server to be ready
for i in $(seq 1 30); do
    if curl -s -o /dev/null "http://127.0.0.1:${FIXTURE_PORT}/master.m3u8" 2>/dev/null; then
        echo "Fixture server ready (attempt ${i})"
        break
    fi
    if [ "$i" -eq 30 ]; then
        echo "ERROR: fixture server did not start within 30s"
        exit 1
    fi
    sleep 1
done

echo "=== Running WASM stress tests (nightly, build-std) ==="
CHROMEDRIVER="${CHROMEDRIVER:-chromedriver}" \
HLS_TEST_URL="${FIXTURE_URL}" \
WASM_BINDGEN_TEST_TIMEOUT=300 \
cargo +nightly test --target wasm32-unknown-unknown \
    -p kithara-integration-tests --test integration

echo "=== WASM tests passed ==="
