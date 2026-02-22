#!/usr/bin/env bash
# Benchmark entrypoint.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

exec just bench-ci
