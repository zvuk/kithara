#!/usr/bin/env bash
# Test entrypoint.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

exec just test-all
