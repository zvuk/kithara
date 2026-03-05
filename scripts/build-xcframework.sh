#!/usr/bin/env bash
# Build KitharaFFI.xcframework from kithara-ffi crate.
#
# Usage:
#   ./scripts/build-xcframework.sh [--release]
#
# Prerequisites:
#   - Xcode CLI tools
#   - Rust targets: aarch64-apple-ios, aarch64-apple-ios-sim, x86_64-apple-ios

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CRATE="kithara-ffi"
FEATURES="backend-uniffi"
PROFILE="${1:---release}"
PROFILE_DIR="release"
if [ "$PROFILE" != "--release" ]; then
    PROFILE_DIR="debug"
    PROFILE=""
fi

OUT_DIR="$ROOT_DIR/target/xcframework"
SWIFT_DIR="$OUT_DIR/swift"
HEADERS_DIR="$OUT_DIR/headers"
XCFRAMEWORK="$OUT_DIR/KitharaFFI.xcframework"

TARGETS=(
    "aarch64-apple-ios"
    "aarch64-apple-ios-sim"
    "x86_64-apple-ios"
)

LIB_NAME="libkithara_ffi.a"

# ── Helpers ──────────────────────────────────────────────────────────────────

log() { printf "==> %s\n" "$*"; }
err() { printf "ERROR: %s\n" "$*" >&2; exit 1; }

check_target() {
    local target="$1"
    if ! rustup target list --installed | grep -q "^${target}\$"; then
        log "Installing target $target"
        rustup target add "$target"
    fi
}

run_uniffi_bindgen() {
    cargo run -p kithara-ffi --features backend-uniffi --bin uniffi-bindgen -- "$@"
}

# ── Build ────────────────────────────────────────────────────────────────────

log "Building KitharaFFI xcframework"

# Check prerequisites
for target in "${TARGETS[@]}"; do
    check_target "$target"
done
# Clean output
rm -rf "$OUT_DIR"
mkdir -p "$SWIFT_DIR" "$HEADERS_DIR"

# Build for each target
for target in "${TARGETS[@]}"; do
    log "Building for $target"
    cargo build ${PROFILE} --target "$target" -p "$CRATE" --features "$FEATURES"
done

# ── Generate Swift bindings ──────────────────────────────────────────────────

# Use any target's library to generate bindings (they're all identical)
FIRST_LIB="$ROOT_DIR/target/${TARGETS[0]}/$PROFILE_DIR/$LIB_NAME"

log "Generating Swift bindings"
run_uniffi_bindgen generate \
    --library "$FIRST_LIB" \
    --language swift \
    --out-dir "$SWIFT_DIR"

# Move the modulemap and header to headers dir
mv "$SWIFT_DIR/"*.modulemap "$HEADERS_DIR/" 2>/dev/null || true
mv "$SWIFT_DIR/"*.h "$HEADERS_DIR/" 2>/dev/null || true

# ── Create fat library for simulator (arm64 + x86_64) ───────────────────────

SIM_FAT_DIR="$OUT_DIR/sim-fat"
mkdir -p "$SIM_FAT_DIR"

log "Creating fat simulator library"
lipo -create \
    "$ROOT_DIR/target/aarch64-apple-ios-sim/$PROFILE_DIR/$LIB_NAME" \
    "$ROOT_DIR/target/x86_64-apple-ios/$PROFILE_DIR/$LIB_NAME" \
    -output "$SIM_FAT_DIR/$LIB_NAME"

# ── Create xcframework ──────────────────────────────────────────────────────

log "Creating xcframework"
xcodebuild -create-xcframework \
    -library "$ROOT_DIR/target/aarch64-apple-ios/$PROFILE_DIR/$LIB_NAME" \
    -headers "$HEADERS_DIR" \
    -library "$SIM_FAT_DIR/$LIB_NAME" \
    -headers "$HEADERS_DIR" \
    -output "$XCFRAMEWORK"

log "Done: $XCFRAMEWORK"
log "Swift sources: $SWIFT_DIR"

# Show contents
echo ""
echo "XCFramework slices:"
ls -d "$XCFRAMEWORK"/*/
echo ""
echo "Generated Swift files:"
ls "$SWIFT_DIR"/*.swift 2>/dev/null || echo "(none)"
