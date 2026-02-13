#!/usr/bin/env bash
# Generate code coverage with cargo-tarpaulin.
# Shared between GitHub Actions and GitLab CI.

set -euo pipefail

echo "==> generating coverage..."
cargo tarpaulin \
  --workspace \
  --timeout 300 \
  --out xml \
  --output-dir ./coverage \
  --exclude-files 'tests/*' \
  --exclude-files 'examples/*' \
  --exclude-files 'benches/*'

echo "==> coverage report written to ./coverage/"
