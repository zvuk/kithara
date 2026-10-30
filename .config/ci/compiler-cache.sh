#!/usr/bin/env bash
set -euo pipefail

phase=$1
shift
if [[ -z "${CI_JOB_ID:-}${GITHUB_RUN_ID:-}" ]]; then
    exec "$@"
fi

: "${KACHE_HOST_CONFIG:?CI requires its RustFS host configuration}"
: "${SCCACHE_BUCKET:?CI requires its RustFS bucket}"
: "${SCCACHE_ENDPOINT:?CI requires its RustFS endpoint}"
[[ -r "$KACHE_HOST_CONFIG" ]] || { printf 'CI RustFS host configuration is unreadable\n' >&2; exit 1; }
command -v kache >/dev/null
runtime=$(mktemp -d "${TMPDIR:-/tmp}/kithara-compiler.XXXXXX")
export KACHE_RUNTIME_DIR="$runtime"
if command -v cygpath >/dev/null; then
    KACHE_RUNTIME_DIR=$(cygpath -m "$runtime")
fi
unset KACHE_SOCKET_PATH

finish() {
    status=$?
    trap - EXIT
    if ! just _xtask-cached strict ci cache record "$phase"; then
        printf 'Compiler cache evidence could not be recorded for %s\n' "$phase" >&2
    fi
    env GITHUB_ACTIONS= GITLAB_CI= kache daemon stop >&2 || status=1
    rm -rf "$runtime"
    exit "$status"
}
trap finish EXIT

stats=$(env GITHUB_ACTIONS= GITLAB_CI= kache stats --json)
for required in '"remote"[[:space:]]*:[[:space:]]*"s3://[^" ]+' '"daemon_connected"[[:space:]]*:[[:space:]]*true' '"daemon_remote_readonly"[[:space:]]*:[[:space:]]*false'; do
    if ! printf '%s' "$stats" | grep -Eq "$required"; then
        printf 'CI compilation requires a connected, writable RustFS S3 cache\n' >&2
        exit 1
    fi
done
env GITHUB_ACTIONS= GITLAB_CI= RUSTC_WRAPPER=kache "$@"
