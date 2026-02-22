#!/usr/bin/env bash
# Performance test entrypoint.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

exec just perf-test
