set shell := ["bash", "-euo", "pipefail", "-c"]

# Empty when absent: cargo reads that as "no wrapper", not as an error.
sccache := `command -v sccache 2>/dev/null || true`

export RUSTC_WRAPPER := sccache

# Where this machine keeps the FFmpeg line the workspace binds to. `ffmpeg-next`
# generates its bindings from the headers pkg-config finds, so a host whose
# unversioned FFmpeg has moved on has to reach the keg-only formula
# `.config/ci-pins.toml` installs before it. Both halves belong to the machine
# and are read off it: the line from the pins, the prefix from where `brew`
# itself sits. Nothing here runs the package manager, because `just` evaluates
# every variable before it knows which recipe was asked for, so this is on the
# clock of every invocation including the nested ones a test drives. A keg the
# machine does not have leaves this empty and its own search path stands alone.
# `xtask` cannot own this: it is itself a cargo build that would need the answer
# before it could run.
export PKG_CONFIG_PATH := ```
    formula=$(grep -om1 'ffmpeg@[0-9][0-9]*' .config/ci-pins.toml 2>/dev/null || true)
    brew=$(command -v brew || true)
    keg="${brew%/bin/brew}/opt/$formula/lib/pkgconfig"
    [ -n "$brew" ] && [ -n "$formula" ] && [ -d "$keg" ] || keg=
    printf '%s' "$keg:${PKG_CONFIG_PATH:-}" | sed 's/^://; s/:$//'
```

# sccache refuses incremental compilations, so a wrapper without this is never
# hit. `check clippy` opts back in on a workstation, where the dependencies are
# already built and incremental turns 15s into 2.4s, and leaves the shared cache
# alone in CI, where nothing is already built.
export CARGO_INCREMENTAL := if sccache == "" { "" } else { "0" }

mod fmt ".config/just/fmt.just"
mod check ".config/just/check.just"
mod lint ".config/just/lint.just"
mod test ".config/just/test.just"
mod quality ".config/just/quality.just"
mod deps ".config/just/deps.just"
mod arch ".config/just/arch.just"
mod perf ".config/just/perf.just"
mod platform ".config/just/platform.just"
mod release ".config/just/release.just"
mod ci ".config/just/ci.just"
mod tooling ".config/just/tooling.just"

# Human-facing overview. Agents use the exact paths documented in AGENTS.md.
[default]
help:
    @just --list

[no-exit-message]
[positional-arguments]
_xtask *ARGS:
    @if [[ -z "${KITHARA_CI_CACHE_ROOT:-}" ]]; then exec just _xtask-unleased "$@"; fi; trust="${KITHARA_CACHE_TRUST:?a CI cache root needs the trust namespace it belongs to}"; system=$(uname -s); arch=$(uname -m); build_target="${CARGO_TARGET_DIR:-$PWD/target}"; if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then if [[ "$system" = Linux ]]; then job="${CI_JOB_ID:-${GITHUB_RUN_ID:-$$}}"; build_target="$KITHARA_CI_CACHE_ROOT/target-slots/$trust-linux-$arch-job-$job/cargo"; else owner="${CI_CONCURRENT_ID:-local}"; case "$owner" in *[!A-Za-z0-9_.-]*) printf 'error: invalid xtask bootstrap cache owner: %s\n' "$owner" >&2; exit 1 ;; esac; build_target="$KITHARA_CI_CACHE_ROOT/bootstrap/$trust/target-$system-$arch-$owner"; fi; fi; mkdir -p "$build_target"; helper="${TMPDIR:-/tmp}/kithara-target-lease-${CI_JOB_ID:-$$}-$$"; rustc --edition=2024 "$PWD/xtask/bootstrap_lease.rs" -o "$helper"; exec "$helper" "$build_target/.kithara-job-lease" just _xtask-unleased "$@"

[no-exit-message]
[positional-arguments]
[private]
_xtask-unleased *ARGS: _xtask-ready
    @exec just _xtask-cached strict "$@"

[no-exit-message]
_xtask-refresh:
    @if just _xtask-cached strict self-cache probe </dev/null >/dev/null 2>&1; then \
      if just _xtask-cached strict self-cache refresh --force </dev/null; then exit 0; fi; \
      printf 'warning: cached xtask self-cache maintenance failed; rebuilding from source\n' >&2; \
    fi; exec just _xtask-bootstrap --force </dev/null

[no-exit-message]
[private]
_xtask-ready:
    @if ! just _xtask-cached strict self-cache probe </dev/null >/dev/null 2>&1; then exec just _xtask-bootstrap </dev/null >/dev/null; fi; state=$(just _xtask-cached strict self-cache status </dev/null) || exit $?; case "$state" in current) ;; stale) exec just _xtask-cached strict self-cache refresh </dev/null >/dev/null ;; *) printf 'error: invalid xtask cache status: %s\n' "$state" >&2; exit 1 ;; esac

# The one build with no caches of its own. Their variables are normally produced
# by `CiEnvironment`, inside the binary this build is compiling. Keep both in the
# bootstrap namespace the host cleaner owns; Cargo's source cache is split by
# platform because Cargo also installs native tools below the same home.
[no-exit-message]
[positional-arguments]
[private]
_xtask-bootstrap *ARGS:
    @target="$PWD/target/xtask-self-cache"; if [[ -n "${KITHARA_CI_CACHE_ROOT:-}" ]]; then trust="${KITHARA_CACHE_TRUST:?a CI cache root needs the trust namespace it belongs to}"; root="$KITHARA_CI_CACHE_ROOT/bootstrap/$trust"; system=$(uname -s); arch=$(uname -m); owner="${CI_CONCURRENT_ID:-local}"; case "$owner" in *[!A-Za-z0-9_.-]*) printf 'error: invalid xtask bootstrap cache owner: %s\n' "$owner" >&2; exit 1 ;; esac; export SCCACHE_DIR="$root/sccache" CARGO_HOME="$root/cargo-$system-$arch"; target="$root/target-$system-$arch-$owner"; fi; exec env CARGO_TARGET_DIR="$target" cargo run --locked --manifest-path "$PWD/Cargo.toml" -p xtask --bin xtask -- self-cache bootstrap "$@"

[no-exit-message]
[positional-arguments]
[private]
_xtask-cached MODE *ARGS:
    @set -eu; \
      mode=$1; shift; \
      case "$mode" in \
        strict|optional) ;; \
        *) printf 'error: invalid xtask transport mode: %s\n' "$mode" >&2; exit 2 ;; \
      esac; \
      unavailable() { \
        if [ "$mode" = optional ]; then \
          printf 'warning: cached xtask transport is unavailable; run just tooling xtask --help to install it\n' >&2; \
          exit 0; \
        fi; \
        printf 'error: cached xtask transport is unavailable\n' >&2; \
        exit 1; \
      }; \
      pointer="$PWD/xtask/.xtask-cache"; \
      [ -f "$pointer" ] && [ ! -L "$pointer" ] && [ -r "$pointer" ] || unavailable; \
      size=$(wc -c < "$pointer") || unavailable; \
      { [ "$size" -ge 1 ] 2>/dev/null && [ "$size" -le 4096 ] 2>/dev/null; } || unavailable; \
      generation=; extra=; \
      if ! { IFS= read -r generation && ! IFS= read -r extra && [ -z "$extra" ]; } < "$pointer"; then \
        unavailable; \
      fi; \
      system=$(uname -s) || unavailable; \
      case "$system" in \
        MINGW*|MSYS*|CYGWIN*) windows=1; suffix=.exe ;; \
        *) windows=0; suffix= ;; \
      esac; \
      case "$generation" in \
        /*) ;; \
        [A-Za-z]:[\\/]*) [ "$windows" -eq 1 ] || unavailable ;; \
        *) unavailable ;; \
      esac; \
      binary="$generation/xtask$suffix"; \
      [ -f "$binary" ] && [ ! -L "$binary" ] && [ -x "$binary" ] || unavailable; \
      if [ "$mode" = optional ]; then \
        "$binary" self-cache probe --config </dev/null >/dev/null 2>&1 || unavailable; \
      fi; \
      exec "$binary" "$@"

[no-exit-message]
_agent-hook: (_xtask-cached "optional" "agent-hook")
