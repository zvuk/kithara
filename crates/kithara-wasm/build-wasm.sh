#!/bin/bash
# Build kithara-wasm HLS player for browser.
#
# Requirements:
#   - Nightly Rust toolchain: rustup toolchain install nightly
#   - wasm32 target: rustup target add wasm32-unknown-unknown --toolchain nightly
#   - Trunk: cargo install trunk
#   - rust-src: rustup component add rust-src --toolchain nightly

set -euo pipefail

echo "Building kithara-wasm HLS player..."

# Check if trunk is installed.
if ! command -v trunk &> /dev/null; then
    echo "Error: trunk is not installed. Run: cargo install trunk"
    exit 1
fi

cd "$(dirname "$0")"

# Use nightly for wasm32 target (needed for -Z build-std and atomics).
RUSTUP_TOOLCHAIN=nightly trunk build --release --config Trunk.toml

echo "Build complete. Output in dist/"
echo "To serve: RUSTUP_TOOLCHAIN=nightly trunk serve --config Trunk.toml"
