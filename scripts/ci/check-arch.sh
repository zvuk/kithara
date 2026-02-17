#!/usr/bin/env bash
# Architecture validation for the kithara workspace.
# Catches structural drift: duplicate types, wrong dependency direction.
# Run: bash scripts/ci/check-arch.sh

set -euo pipefail

errors=0

# Canonical types that must have a single definition (not re-exports).
# These are the shared types from kithara-stream that other crates use.
CANONICAL_TYPES=(
    "enum AudioCodec"
    "enum ContainerFormat"
    "struct MediaInfo"
)

for type_decl in "${CANONICAL_TYPES[@]}"; do
    kind="${type_decl%% *}"     # enum or struct
    name="${type_decl##* }"     # AudioCodec, etc.
    # Find definitions (pub enum Foo / pub struct Foo), skip re-exports and comments
    count=$(grep -rn "pub ${kind} ${name}\b" --include='*.rs' crates/ \
        | grep -v '^\s*//' \
        | grep -v 'pub use' \
        | wc -l)
    if [ "$count" -gt 1 ]; then
        echo "ERROR: '${type_decl}' defined $count times (expected 1):"
        grep -rn "pub ${kind} ${name}\b" --include='*.rs' crates/ \
            | grep -v '^\s*//' \
            | grep -v 'pub use'
        errors=$((errors + 1))
    fi
done

# Check for duplicate error enums across crates.
# Each crate should have at most one Error type.
# Flag if the same error enum name appears in multiple crates.
while IFS= read -r line; do
    file=$(echo "$line" | cut -d: -f1)
    crate_dir=$(echo "$file" | sed 's|crates/\([^/]*\)/.*|\1|')
    echo "$crate_dir"
done < <(grep -rn 'pub enum Error\b' --include='*.rs' crates/ || true) \
    | sort | uniq -d | while IFS= read -r dup; do
    if [ -n "$dup" ]; then
        echo "WARNING: crate '$dup' defines 'pub enum Error' multiple times"
    fi
done

# Dependency direction: base crates must not depend on higher-level crates.
# kithara-platform, kithara-bufpool, kithara-storage must not depend on
# kithara-hls, kithara-file, kithara-decode, kithara-audio, kithara-wasm.
BASE_CRATES=("kithara-platform" "kithara-abr" "kithara-drm")
HIGH_CRATES=("kithara-hls" "kithara-file" "kithara-audio" "kithara-wasm" "kithara-decode")

for base in "${BASE_CRATES[@]}"; do
    cargo_file="crates/${base}/Cargo.toml"
    if [ ! -f "$cargo_file" ]; then continue; fi
    for high in "${HIGH_CRATES[@]}"; do
        if grep -q "^${high}\b" "$cargo_file" 2>/dev/null || \
           grep -q "\"${high}\"" "$cargo_file" 2>/dev/null; then
            echo "ERROR: base crate '$base' depends on higher-level crate '$high'"
            errors=$((errors + 1))
        fi
    done
done

# Mid-level crates must not depend on facade
MID_CRATES=("kithara-storage" "kithara-bufpool" "kithara-assets" "kithara-net"
            "kithara-stream" "kithara-decode" "kithara-file" "kithara-hls"
            "kithara-audio" "kithara-events")

for mid in "${MID_CRATES[@]}"; do
    cargo_file="crates/${mid}/Cargo.toml"
    if [ ! -f "$cargo_file" ]; then continue; fi
    # Must not depend on kithara (facade) — check for bare "kithara" dep
    if grep -qE '^\[dependencies\]' "$cargo_file" && \
       sed -n '/^\[dependencies\]/,/^\[/p' "$cargo_file" | grep -qE '^kithara[[:space:]]*='; then
        echo "ERROR: crate '$mid' depends on facade crate 'kithara'"
        errors=$((errors + 1))
    fi
done

# No .rs files outside crates/ and tests/ (prevents accidental source files at root)
stray_rs=$(find . -maxdepth 1 -name '*.rs' 2>/dev/null | head -5)
if [ -n "$stray_rs" ]; then
    echo "ERROR: .rs files found at repository root:"
    echo "$stray_rs"
    errors=$((errors + 1))
fi

if [ "$errors" -gt 0 ]; then
    echo ""
    echo "FAILED: $errors architecture violation(s) found."
    exit 1
else
    echo "OK: no architecture violations."
    exit 0
fi
