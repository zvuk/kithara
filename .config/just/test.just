set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := "../.."

# Run workspace tests via the xtask/nextest harness. Use `just test run ...`

# when passing lane selectors, package filters, or nextest expressions.
[default]
[positional-arguments]
run *ARGS:
    just _xtask test "$@"

all:
    just test
    just test run --lane=doc

# These are workspace tests like any other, but `wasm32` has no host binary for
# nextest to execute: the harness is `wasm-bindgen-test` driving a real browser,
# and the target rebuilds std, which only the pinned nightly honours. That is
# why they do not go through `just test run`. `just platform wasm` keeps the
# artifacts (check, build, size-check); the tests belong here.
#
# SUITE `all` adds the integration browser suite; `webcodecs` runs only
# kithara-decode's own target, which is what the browser CI job asks for.

# Run the Rust tests that need a browser (SUITE: all | webcodecs).
[positional-arguments]
wasm BROWSER="chrome" SUITE="all":
    #!/usr/bin/env bash
    set -euo pipefail
    browser="{{ BROWSER }}"
    suite="{{ SUITE }}"
    toolchain="${KITHARA_NIGHTLY_TOOLCHAIN:-nightly}"
    case "$suite" in
      all|webcodecs) ;;
      *)
        echo "unknown wasm test suite: $suite (use all|webcodecs)"
        exit 2
        ;;
    esac
    if [[ "$suite" == "webcodecs" && "$browser" != "chrome" ]]; then
      echo "the WebCodecs target runs on chrome only"
      exit 2
    fi
    case "$browser" in
      chrome)  export CHROMEDRIVER="${CHROMEDRIVER:-chromedriver}" ;;
      firefox) export GECKODRIVER="${GECKODRIVER:-geckodriver}" ;;
      safari)  export SAFARIDRIVER="${SAFARIDRIVER:-safaridriver}" ;;
      *)
        echo "unknown wasm test browser: $browser (use chrome|firefox|safari)"
        exit 2
        ;;
    esac
    # The browser talks to a fixture server that runs on this machine, and the
    # runner looks for it beside itself — for every wasm test binary, not just
    # the integration suite. Nothing in a wasm build produces a host binary, so
    # without this the run dies with "failed to start test_server ... No such
    # file or directory".
    cargo build -p kithara-integration-tests --bin test_server
    if [[ "$suite" == "all" ]]; then
      # The integration browser tests live in `suite_heavy`. Building the whole
      # crate for wasm compiles every native suite too, and those reach for
      # threads, the filesystem, and a local test server the browser cannot
      # offer. `optimize_for_size` matches crates/kithara-ffi/.cargo/config.toml
      # so both lanes rebuild one identical std. The release-grade test profile
      # is what lets the seek-pressure contract reach its final oracle inside
      # its own ten-second deadline, which stays untouched.
      WASM_BINDGEN_TEST_TIMEOUT=300 WASM_BINDGEN_USE_BROWSER=1 \
        cargo "+$toolchain" test --profile test-release \
        --target wasm32-unknown-unknown \
        -Z build-std=std,panic_abort,core,alloc \
        -Z build-std-features=optimize_for_size \
        -p kithara-integration-tests --test suite_heavy
    fi
    # `webcodecs_browser` belongs to kithara-decode, not to any integration
    # suite, so it is its own cargo invocation. WebCodecs is a Chromium API;
    # the other engines have nothing to answer here.
    #
    # It rebuilds std for the same reason the suite above does: the repository
    # compiles wasm32 with `+atomics`, and linking that against a stock std
    # fails with "--shared-memory is disallowed by std ... not compiled with
    # 'atomics' or 'bulk-memory'". Both invocations must carry the flags.
    if [[ "$browser" == "chrome" ]]; then
      WASM_BINDGEN_TEST_TIMEOUT=300 WASM_BINDGEN_USE_BROWSER=1 \
        cargo "+$toolchain" test --profile test-release \
        --target wasm32-unknown-unknown \
        -Z build-std=std,panic_abort,core,alloc \
        -Z build-std-features=optimize_for_size \
        -p kithara-decode --no-default-features \
        --features symphonia,webcodecs,client-reqwest,tls-rustls \
        --test webcodecs_browser
    fi

# Locate or drop the on-disk fixture cache. Each build fingerprint gets its own
# sub-directory, so an obsolete one wastes disk rather than serving stale bytes;
# dropping the roots costs the next run one re-encode of everything it touches.
fixture-cache ACTION="path":
    #!/usr/bin/env bash
    set -euo pipefail
    tmp="${TMPDIR:-/tmp}"
    warm="${tmp%/}/kithara-fixture-cache"
    cold="${tmp%/}/kithara-fixture-cold"
    case "{{ ACTION }}" in
      path) printf '%s\n%s\n' "$warm" "$cold" ;;
      clear) rm -rf "$warm" "$cold" && echo "==> dropped $warm and $cold" ;;
      *) echo "usage: just test fixture-cache [path|clear]" >&2; exit 2 ;;
    esac

# Render the same active no-SYNC scenarios into listening artifacts. The
# recorders are ignored in ordinary CI so filesystem work cannot consume their
# behavioral-test deadlines.
[positional-arguments]
audio-artifacts OUTPUT:
    #!/usr/bin/env bash
    set -euo pipefail
    output="{{ OUTPUT }}"
    if [[ "$output" != /* ]]; then
      echo "audio artifact output must be an absolute path" >&2
      exit 2
    fi
    KITHARA_AUDIO_ARTIFACT_DIR="$output" \
      just test run -p kithara-integration-tests --test suite_light \
        --run-ignored=only \
        -E 'test(/kithara_audio::no_sync_passthrough::record_no_sync_(unity_playback|active_keylock)_artifacts/)'
    KITHARA_AUDIO_ARTIFACT_DIR="$output" \
      just test run -p kithara-integration-tests --test suite_heavy \
        --run-ignored=only \
        -E 'test(=no_sync_real_media::record_no_sync_real_media_artifacts)'

# All three lanes require a real output device.
[positional-arguments]
e2e-resamplers *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    just test run --lane=e2e "$@"
    just test run --lane=e2e-fused "$@"
    just test run --lane=e2e-glide "$@"

# Cobertura and LCOV reports from one instrumented test run.
coverage:
    #!/usr/bin/env bash
    set -uo pipefail
    OUTPUT_DIR="${COVERAGE_OUTPUT_DIR:-./coverage}"
    # The bar stands at 80 while the workspace holds 76.5% of lines. The gap is
    # the UI surface — kithara-ui widgets and kithara-app/src/gui, several files
    # of it at zero — and it closes by covering that surface, not by moving the
    # number. The number lives only here: the CI lane inherits it, because a
    # second copy is one edit away from gating a local run and CI at different
    # bars.
    COVERAGE_MIN="${COVERAGE_MIN:-80}"
    IGNORE_REGEX='(tests/|examples/|benches/|xtask/)'
    COBERTURA="$OUTPUT_DIR/cobertura.xml"
    LCOV="$OUTPUT_DIR/lcov.info"
    mkdir -p "$OUTPUT_DIR"
    rm -f "$COBERTURA" "$LCOV"
    cargo llvm-cov clean --workspace
    test_rc=0
    cargo llvm-cov nextest --no-report \
      --workspace --exclude kithara-fuzz \
      --cargo-profile test-release || test_rc=$?
    report_rc=0
    cargo llvm-cov report \
      --profile test-release \
      --cobertura \
      --output-path "$COBERTURA" \
      --ignore-filename-regex "$IGNORE_REGEX" || report_rc=$?
    cargo llvm-cov report \
      --profile test-release \
      --lcov \
      --output-path "$LCOV" \
      --ignore-filename-regex "$IGNORE_REGEX" || report_rc=$?
    cargo llvm-cov report \
      --profile test-release \
      --summary-only \
      --ignore-filename-regex "$IGNORE_REGEX" \
      --fail-under-lines "$COVERAGE_MIN" || report_rc=$?
    echo "==> coverage report written to $COBERTURA"
    echo "==> LCOV report written to $LCOV"
    echo "==> line coverage threshold: $COVERAGE_MIN%"
    if (( test_rc != 0 )); then
      exit "$test_rc"
    fi
    exit "$report_rc"

# Show lines not executed by any test.
uncovered:
    cargo llvm-cov clean --workspace
    cargo llvm-cov nextest --no-report --workspace --exclude kithara-fuzz \
      --features kithara-audio/rodio \
      --cargo-profile test-release --no-fail-fast || true
    cargo llvm-cov nextest --no-report \
      -p kithara-integration-tests --features e2e \
      --cargo-profile test-release \
      --test suite_e2e --run-ignored all --no-fail-fast || true
    cargo llvm-cov report \
      --ignore-filename-regex '(tests/|examples/|benches/)' \
      --show-missing-lines

# Focused mutation testing. Only suites declared in mutation-suites.toml are accepted.
[positional-arguments]
mutants *ARGS:
    just _xtask mutants "$@"

[positional-arguments]
loom-checkpoint FILE *ARGS:
    #!/bin/sh
    set -eu
    file=$1
    shift
    LOOM_CHECKPOINT_FILE="$file" exec just test run --loom=on "$@"

[positional-arguments]
loom-isolate FILE *ARGS:
    #!/bin/sh
    set -eu
    file=$1
    shift
    LOOM_CHECKPOINT_INTERVAL=1 LOOM_CHECKPOINT_FILE="$file" exec just test run --loom=on "$@"

[positional-arguments]
loom-debug FILE *ARGS:
    #!/bin/sh
    set -eu
    file=$1
    shift
    LOOM_LOG=trace LOOM_LOCATION=1 LOOM_CHECKPOINT_INTERVAL=1 LOOM_CHECKPOINT_FILE="$file" exec just test run --loom=on --no-capture "$@"

# Interpret the crates that carry shared state under Miri.
#
# Miri cannot enter a C library, and every `unsafe` block that matters here is
# an FFI call into one, so the selection is the crates that hold atomics and
# forbid `unsafe` themselves. What it can still answer is whether their memory
# ordering is sound and whether a dependency reaches undefined behaviour.
[positional-arguments]
miri *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    # Isolation hides the clock and the environment, which every one of these
    # crates reads on the way to its first assertion.
    MIRIFLAGS="-Zmiri-disable-isolation" \
        cargo +nightly miri test --lib \
        -p kithara-abr \
        -p kithara-bufpool \
        -p kithara-events \
        -p kithara-queue \
        -p kithara-stream \
        "$@"

# Two backends answer to the same annotations, chosen by KITHARA_RTSAN_BACKEND:
#
#   instrumented (default) — the pinned nightly marks the checked entry points
#     through `sanitize(realtime = "nonblocking")` and links the runtime from
#     `-Zsanitizer=realtime`. Plain `nightly` is whatever the host last
#     downloaded, so the pin is what the image names.
#   standalone — stable Rust; the mark is a runtime guard and the runtime comes
#     from the `rtsan-standalone` crate. Its build script gates on the presence
#     of RTSAN_ENABLE (the value is never read, so `0` also enables it) and
#     downloads the library with curl unless RTSAN_LIB_PATH names a staged one.
#     That path must keep the library's canonical file name or the link fails.
#
# Either way a symbolizer is required rather than hoped for: a stack of bare
# addresses is the one part of a sanitizer report nobody can reconstruct
# afterwards, because the binary goes with the container. The stress run of
# 2026-08-15 found a real violation fifty times over and could not say where
# any of them was. `llvm-symbolizer` is a system package, not a rustup
# component — `llvm-tools-preview` does not ship it. On Darwin the sanitizer
# runtime falls back to `atos` from the command line tools, so there it needs
# nothing else.
#
# Both run under nextest: a violation aborts the process, and one process per
# test is what turns that abort into a named failure plus JUnit instead of a
# bare exit code for the whole suite.
_rtsan SUITE FEATURES FILTER SUPPRESSIONS="":
    #!/usr/bin/env bash
    set -euo pipefail
    target="$(rustc -vV | sed -n 's/^host: //p')"
    backend="${KITHARA_RTSAN_BACKEND:-instrumented}"
    # A sanitized binary that dies on a signal reports only the signal number
    # through the harness above it — nextest saw one lane die on SIGSEGV while
    # merely listing its tests, and the log named neither the frame nor the
    # library. The runtime prints a symbolized stack for the signal it catches.
    options="symbolize=1:handle_segv=1:handle_abort=1:print_stacktrace=1"
    if symbolizer="$(command -v llvm-symbolizer)"; then
        options="$options:external_symbolizer_path=$symbolizer"
    elif ! { [ "$(uname -s)" = Darwin ] && command -v atos >/dev/null; }; then
        echo "rtsan: no symbolizer, so a violation would be reported against a" >&2
        echo "rtsan: binary that no longer exists by the time anyone reads it." >&2
        echo "rtsan:   Debian/Ubuntu: apt-get install -y llvm" >&2
        echo "rtsan:   macOS:         install the Xcode command line tools" >&2
        exit 1
    fi
    if [ -n "{{ SUPPRESSIONS }}" ]; then
        options="$options:suppressions=$(pwd)/{{ SUPPRESSIONS }}"
    fi
    case "$backend" in
    instrumented)
        toolchain="+${KITHARA_NIGHTLY_TOOLCHAIN:-nightly}"
        flags="-Zsanitizer=realtime --cfg rtsan"
        ;;
    standalone)
        toolchain="+${KITHARA_STABLE_TOOLCHAIN:-stable}"
        flags="--cfg rtsan --cfg rtsan_standalone"
        export RTSAN_ENABLE=1
        # The image stages the runtime and names its directory; the file keeps
        # the name the linker asks for, so derive it from the target rather
        # than storing a second copy of that name in the image. With neither
        # variable set the crate fetches its own — right for a developer host,
        # and what staging exists to avoid in CI.
        if [ -z "${RTSAN_LIB_PATH:-}" ] && [ -n "${KITHARA_RTSAN_LIB_DIR:-}" ]; then
            case "$target" in
            x86_64-unknown-linux-gnu) library=libclang_rt.rtsan_linux_x86_64.a ;;
            aarch64-unknown-linux-gnu) library=libclang_rt.rtsan_linux_aarch64.a ;;
            *-apple-darwin) library=libclang_rt.rtsan_osx_dynamic.dylib ;;
            *)
                echo "rtsan: no staged runtime name for target '$target'" >&2
                exit 1
                ;;
            esac
            export RTSAN_LIB_PATH="$KITHARA_RTSAN_LIB_DIR/$library"
            if [ ! -f "$RTSAN_LIB_PATH" ]; then
                echo "rtsan: staged runtime missing at $RTSAN_LIB_PATH" >&2
                exit 1
            fi
        fi
        ;;
    *)
        echo "rtsan: unknown backend '$backend' (instrumented | standalone)" >&2
        exit 1
        ;;
    esac
    # A run hands its repeat count here instead of launching this recipe once
    # per repeat: nextest owns the repetition, so the suite is built once and
    # every repeat lands in one report that names the test it belongs to. Unset —
    # a gate, or a developer — runs the selection once.
    repeats=""
    if [ -n "${KITHARA_STRESS_REPEATS:-}" ]; then
        repeats="--stress-count ${KITHARA_STRESS_REPEATS}"
    fi
    RTSAN_OPTIONS="$options" \
    RUSTFLAGS="$flags" \
    RUSTDOCFLAGS="$flags" \
        cargo "$toolchain" nextest run --profile rtsan \
        -p kithara-integration-tests --test {{ SUITE }} \
        --features {{ FEATURES }} \
        --target "$target" $repeats {{ FILTER }}

rtsan-async FILTER="no_block": (_rtsan "suite_light" "no-block" FILTER ".config/rtsan/async-suppressions.txt")

# `rt_metrics` drives `process()` directly: decode error, underrun, eviction,
# and the on-core seek re-base. `mix_tap` drives the post-limiter PCM sink.
# `analysis_offer_is_realtime_safe` puts the analysis offer where a decode tick
# makes it. All four drive a synthetic source, so they answer for the processor
# and say nothing about what a decoder, a ring and an event bus do under it.
# `single_track_silence_trim_strips_leading_priming` renders a decoded fixture
# through the same processor and is the cheapest selection that does
# (~1.2s here; a test that waits on playback position instead of rendering flat
# out costs four times that for the same catch).
rtsan FILTER="offline_harness kithara_play::rt_metrics kithara_play::mix_tap kithara_audio::analysis_offer_is_realtime_safe single_track_silence_trim_strips_leading_priming": (_rtsan "suite_light" "apple-fused-src" FILTER)

rtsan-file FILTER="phase_continuity::file": (_rtsan "suite_stress" "apple-fused-src" FILTER)

rtsan-hls FILTER="phase_continuity::hls": (_rtsan "suite_stress" "apple-fused-src" FILTER)
