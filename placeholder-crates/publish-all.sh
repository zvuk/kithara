#!/usr/bin/env bash
set -e

cd "$(dirname "$0")"

crates=(
    "kithara-assets"
    "kithara-bufpool"
    "kithara-worker"
    "kithara"
)

for crate in "${crates[@]}"; do
    sleep 600
    echo "Publishing $crate..."
    cd "$crate"
    cargo publish --allow-dirty
    cd ..
    echo "✓ Published $crate"
    echo ""
    sleep 600  # longer pause to avoid rate limits
done

echo "All crates published!"
