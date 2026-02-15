#!/bin/bash
# Build kithara-wasm HLS player for browser.
#
# Requirements:
#   - Nightly Rust toolchain: rustup toolchain install nightly
#   - wasm32 target: rustup target add wasm32-unknown-unknown --toolchain nightly
#   - Trunk: cargo install trunk
#
# For multi-threading (SharedArrayBuffer):
#   RUSTUP_TOOLCHAIN=nightly trunk serve --config crates/kithara-wasm/Trunk.toml
#
# Without multi-threading (simpler, for initial testing):
#   trunk serve --config crates/kithara-wasm/Trunk.toml

set -euo pipefail

echo "Building kithara-wasm HLS player..."

# Check if trunk is installed.
if ! command -v trunk &> /dev/null; then
    echo "Error: trunk is not installed. Run: cargo install trunk"
    exit 1
fi

# Build with trunk (uses Trunk.toml for config).
cd "$(dirname "$0")"
trunk build --release

echo "Build complete. Output in dist/"
echo "To serve: trunk serve"
