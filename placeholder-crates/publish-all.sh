#!/usr/bin/env bash
set -e

cd "$(dirname "$0")"

crates=(
    "kithara-storage"
    "kithara-assets"
    "kithara-bufpool"
    "kithara-worker"
    "kithara"
)

for crate in "${crates[@]}"; do
    echo "Publishing $crate..."
    cd "$crate"
    cargo publish --allow-dirty
    cd ..
    echo "✓ Published $crate"
    echo ""
    sleep 15  # longer pause to avoid rate limits
done

echo "All crates published!"
