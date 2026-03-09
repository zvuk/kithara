#!/usr/bin/env bash
# Build KitharaFFI Android shared libraries + Kotlin bindings.
#
# Usage:
#   ./scripts/build-android-bindings.sh [--release]
#
# Prerequisites:
#   - cargo-ndk (cargo install cargo-ndk)
#   - Android SDK + NDK
#   - Rust Android targets installed via rustup

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ANDROID_DIR="$ROOT_DIR/android"
LIB_DIR="$ANDROID_DIR/lib"
CRATE_DIR="$ROOT_DIR/crates/kithara-ffi"
PROFILE="debug"
ANDROID_API_LEVEL=26

export PATH="$HOME/.cargo/bin:$PATH"

if [ "${1:-}" = "--release" ]; then
    PROFILE="release"
fi

JNI_DIR="$LIB_DIR/build/generated/jniLibs"
KOTLIN_OUT_DIR="$LIB_DIR/build/generated/uniffi/kotlin"
RUST_TARGETS=(
    "aarch64-linux-android"
    "armv7-linux-androideabi"
    "x86_64-linux-android"
)

log() { printf "==> %s\n" "$*"; }
err() { printf "ERROR: %s\n" "$*" >&2; exit 1; }

has_rust_target() {
    local expected="$1"
    local installed
    while IFS= read -r installed; do
        if [ "$installed" = "$expected" ]; then
            return 0
        fi
    done <<EOF
$(rustup target list --installed)
EOF
    return 1
}

command -v cargo >/dev/null 2>&1 || err "cargo not found"
command -v rustup >/dev/null 2>&1 || err "rustup not found"
command -v cargo-ndk >/dev/null 2>&1 || err "cargo-ndk not found. Install with: cargo install cargo-ndk"

for target in "${RUST_TARGETS[@]}"; do
    has_rust_target "$target" || err "Rust target '$target' is not installed. Run: rustup target add $target"
done

mkdir -p "$JNI_DIR" "$KOTLIN_OUT_DIR"
rm -rf "$JNI_DIR" "$KOTLIN_OUT_DIR"
mkdir -p "$JNI_DIR" "$KOTLIN_OUT_DIR"

log "Building Android shared libraries"
cd "$ROOT_DIR"

BUILD_ARGS=(cargo ndk -P "$ANDROID_API_LEVEL" -t arm64-v8a -t armeabi-v7a -t x86_64 -o "$JNI_DIR" build -p kithara-ffi --features backend-uniffi)
if [ "$PROFILE" = "release" ]; then
    BUILD_ARGS+=(--release)
fi
"${BUILD_ARGS[@]}"

LIB_PATH="$JNI_DIR/arm64-v8a/libkithara_ffi.so"

[ -f "$LIB_PATH" ] || err "compiled library not found at $LIB_PATH"

log "Generating Kotlin bindings"
cd "$CRATE_DIR"

GENERATE_ARGS=(cargo run --bin uniffi-bindgen --features backend-uniffi)
if [ "$PROFILE" = "release" ]; then
    GENERATE_ARGS+=(--release)
fi
GENERATE_ARGS+=(-- generate --library "$LIB_PATH" --language kotlin --no-format --out-dir "$KOTLIN_OUT_DIR")
"${GENERATE_ARGS[@]}"

log "Done!"
log "JNI libs: $JNI_DIR"
log "Kotlin bindings: $KOTLIN_OUT_DIR"
