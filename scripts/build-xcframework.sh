#!/usr/bin/env bash
# Build KitharaFFI xcframework + Swift bindings using cargo-swift.
#
# Usage:
#   ./scripts/build-xcframework.sh [--release]
#
# Prerequisites:
#   - cargo-swift (cargo install cargo-swift)
#   - Xcode CLI tools
#   - Rust targets for iOS and macOS (installed automatically by cargo-swift)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CRATE_DIR="$ROOT_DIR/crates/kithara-ffi"
APPLE_DIR="$ROOT_DIR/apple"
PROFILE_FLAG=""

if [ "${1:---release}" = "--release" ]; then
    PROFILE_FLAG="--release"
fi

# ── Helpers ──────────────────────────────────────────────────────────────────

log() { printf "==> %s\n" "$*"; }
err() { printf "ERROR: %s\n" "$*" >&2; exit 1; }

# ── Checks ───────────────────────────────────────────────────────────────────

command -v cargo-swift >/dev/null 2>&1 || err "cargo-swift not found. Install with: cargo install cargo-swift"

# ── Build with cargo-swift ──────────────────────────────────────────────────

log "Building KitharaFFI with cargo-swift"

cd "$CRATE_DIR"
cargo swift package \
    -p ios macos \
    -n KitharaFFI \
    $PROFILE_FLAG \
    --lib-type static \
    -F backend-uniffi \
    --swift-tools-version 6.0 \
    -y

# ── Copy outputs to apple/ ──────────────────────────────────────────────────

log "Copying outputs to apple/"

# xcframework
rm -rf "$APPLE_DIR/KitharaFFIInternal.xcframework"
cp -R "$CRATE_DIR/KitharaFFI/KitharaFFIInternal.xcframework" "$APPLE_DIR/"

# generated Swift bindings
cp "$CRATE_DIR/KitharaFFI/Sources/KitharaFFI/KitharaFFI.swift" \
   "$APPLE_DIR/Sources/KitharaFFI/KitharaFFI.swift"

# ── Done ────────────────────────────────────────────────────────────────────

log "Done!"
log "XCFramework: $APPLE_DIR/KitharaFFIInternal.xcframework"
log "Swift bindings: $APPLE_DIR/Sources/KitharaFFI/KitharaFFI.swift"

echo ""
echo "XCFramework slices:"
ls -d "$APPLE_DIR/KitharaFFIInternal.xcframework"/*/
echo ""
echo "To build and test:"
echo "  cd $APPLE_DIR && swift build && swift test"
