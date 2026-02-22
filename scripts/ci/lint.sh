#!/usr/bin/env bash
# Lint checks entrypoint for CI and local runs.
# Primary path: just lint-full
# Compatibility path: legacy inline checks when just is unavailable.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

cd "$REPO_ROOT"

if command -v just >/dev/null 2>&1; then
    exec just lint-full
fi

echo "==> 'just' is not installed; running compatibility lint flow"

echo "==> checking formatting (nightly)..."
cargo +nightly fmt --all --check

echo "==> running clippy..."
cargo clippy --workspace -- -D warnings

SEMGREP_BIN="$(command -v semgrep || true)"
if [[ -z "$SEMGREP_BIN" ]]; then
    PY_USER_BASE="$(python3 -c 'import site; print(site.getuserbase())' 2>/dev/null || true)"
    CANDIDATE="${PY_USER_BASE}/bin/semgrep"
    if [[ -x "$CANDIDATE" ]]; then
        SEMGREP_BIN="$CANDIDATE"
    fi
fi
if [[ -z "$SEMGREP_BIN" ]]; then
    echo "FAILED: semgrep is required but not installed."
    exit 1
fi
XDG_DIR="$REPO_ROOT/.cache"
SEMGREP_DIR="$XDG_DIR/semgrep"
mkdir -p "$XDG_DIR" "$SEMGREP_DIR"
export XDG_CACHE_HOME="$XDG_DIR"
export XDG_CONFIG_HOME="$XDG_DIR"
export SEMGREP_LOG_FILE="$SEMGREP_DIR/semgrep.log"
export SEMGREP_SETTINGS_FILE="$SEMGREP_DIR/settings.yml"
export PATH="$(dirname "$SEMGREP_BIN"):$PATH"
if [[ -z "${SSL_CERT_FILE:-}" ]]; then
    CERT_PATH="$(python3 -c 'import certifi; print(certifi.where())' 2>/dev/null || true)"
    if [[ -n "$CERT_PATH" && -f "$CERT_PATH" ]]; then
        export SSL_CERT_FILE="$CERT_PATH"
    fi
fi

echo "==> running semgrep (blocking rules)..."
"$SEMGREP_BIN" --config "$REPO_ROOT/semgrep.yml" --severity ERROR --error

echo "==> running semgrep (advisory rules)..."
"$SEMGREP_BIN" --config "$REPO_ROOT/semgrep.yml" --severity WARNING

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
