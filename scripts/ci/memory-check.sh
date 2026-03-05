#!/usr/bin/env bash
set -euo pipefail

echo "=== Tier 1: Pool-native budget tests ==="
cargo test -p kithara-bufpool --test memory_budget

echo ""
echo "=== Tier 2: Allocation regression tests ==="
cargo test -p kithara-bufpool --test alloc_regression -- --test-threads=1

echo ""
echo "=== Tier 3: Audio hot path zero-alloc ==="
cargo test -p kithara-audio --test alloc_free_hotpath -- --test-threads=1

echo ""
echo "=== Tier 4: RSS budget (HLS playback) ==="
cargo test --test memory_rss -- --test-threads=1

echo ""
echo "All memory checks passed."
