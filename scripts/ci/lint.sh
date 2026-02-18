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

echo "==> generating quality report..."
bash "$SCRIPT_DIR/quality-report.sh"

echo "==> checking kithara-play trait unimock coverage..."
bash "$SCRIPT_DIR/check-play-traits-unimock.sh"

echo "==> running rstest parameterization audit..."
bash "$SCRIPT_DIR/rstest-audit.sh"

echo "==> running trait mock audit..."
bash "$SCRIPT_DIR/trait-mock-audit.sh"

echo "==> validating trait mock exceptions..."
bash "$SCRIPT_DIR/trait-mock-exceptions.sh"

echo "==> checking architecture constraints..."
bash "$SCRIPT_DIR/check-arch.sh"

echo "==> all lint checks passed."
