#!/usr/bin/env bash
# Lint checks: formatting, clippy, style.
# Shared between GitHub Actions and GitLab CI.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "==> checking formatting (nightly)..."
cargo +nightly fmt --all --check

echo "==> running clippy..."
cargo clippy --workspace -- -D warnings

echo "==> running style linter..."
bash "$SCRIPT_DIR/lint-style.sh"

echo "==> testing perf comparison script..."
bash "$SCRIPT_DIR/test-compare-perf.sh"

echo "==> checking architecture constraints..."
bash "$SCRIPT_DIR/check-arch.sh"

echo "==> all lint checks passed."
