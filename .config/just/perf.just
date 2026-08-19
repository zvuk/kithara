set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := "../.."

# Run perf-feature integration tests.
[default]
run:
    cargo test -p kithara-integration-tests --features perf --release \
      -- --ignored --test-threads=1 --nocapture 2>&1 | tee perf-results.txt

# One hotpath guard may be alive at a time and each host case opens its own, so
# the cases may not share a thread. The metrics server is off: a second worktree
# measuring at the same moment would take the same port.
# Per-widget frame time on both UI hosts, immediate and retained.
frames:
    HOTPATH_METRICS_SERVER_OFF=1 \
      cargo test -p kithara-ui --test frame_perf --features perf,iced,masonry --release \
      -- --test-threads=1 --nocapture 2>&1 | tee ui-frame-perf.txt

# Whole-page frame time on both hosts: the control page and the bucket sweep.
pages:
    HOTPATH_METRICS_SERVER_OFF=1 \
      cargo test -p kithara-ui --test page_perf --features perf,iced,masonry --release \
      -- ui_page_perf --test-threads=1 --nocapture 2>&1 | tee ui-page-perf.txt

# Frame time on the pages that move on their own: posed objects, a sprite sheet
# and an artwork.
motion:
    HOTPATH_METRICS_SERVER_OFF=1 \
      cargo test -p kithara-ui --test page_perf --features perf,iced,masonry --release \
      -- ui_motion_perf --test-threads=1 --nocapture 2>&1 | tee ui-motion-perf.txt

# Cost per wheel event against cost per frame, swept over wheels per frame.
scroll:
    HOTPATH_METRICS_SERVER_OFF=1 \
      cargo test -p kithara-ui --test page_perf --features perf,iced,masonry --release \
      -- ui_scroll_perf --test-threads=1 --nocapture 2>&1 | tee ui-scroll-perf.txt

# Cost per pointer move against cost per frame, with a fader held under the hand.
drag:
    HOTPATH_METRICS_SERVER_OFF=1 \
      cargo test -p kithara-ui --test page_perf --features perf,iced,masonry --release \
      -- ui_drag_perf --test-threads=1 --nocapture 2>&1 | tee ui-drag-perf.txt

# The visualiser and document-shader passes, free and fenced (own scenario).
native:
    HOTPATH_METRICS_SERVER_OFF=1 \
      cargo test -p kithara-ui --test page_perf --features perf,iced,masonry --release \
      -- ui_native_perf --test-threads=1 --nocapture 2>&1 | tee ui-native-perf.txt

[positional-arguments]
suite *ARGS:
    just _xtask perf "$@"

[positional-arguments]
compare *ARGS:
    just _xtask perf-compare "$@"

bench MODE="build":
    #!/usr/bin/env bash
    set -uo pipefail
    benches=(abr_estimator bufpool refactor_hotpaths rt_block_budget)
    case "{{ MODE }}" in
      build)
        for b in "${benches[@]}"; do
          cargo bench -p kithara-integration-tests --bench "$b" --no-run
        done
        ;;
      run)
        sample_size="${BENCH_SAMPLE_SIZE:-20}"
        for b in "${benches[@]}"; do
          cargo bench -p kithara-integration-tests --bench "$b" -- --sample-size "$sample_size" 2>&1 | tee -a bench-results.txt
        done
        ;;
      ci)
        for b in "${benches[@]}"; do
          cargo bench -p kithara-integration-tests --bench "$b" --no-run
        done
        if [[ "${RUN_BENCHMARKS:-0}" != "1" ]]; then
          echo "==> benchmark execution skipped (set RUN_BENCHMARKS=1 to execute)" | tee bench-results.txt
          exit 0
        fi
        sample_size="${BENCH_SAMPLE_SIZE:-20}"
        candidate_name="${BENCH_CANDIDATE_NAME:-ci}"
        for b in "${benches[@]}"; do
          cargo bench -p kithara-integration-tests --bench "$b" \
            -- --sample-size "$sample_size" --save-baseline "$candidate_name" 2>&1 | tee -a bench-results.txt
        done
        if [[ -n "${BENCH_COMPARE_BASELINE_NAME:-}" ]]; then
          if ! command -v critcmp >/dev/null 2>&1; then
            echo "FAILED: critcmp is required when BENCH_COMPARE_BASELINE_NAME is set" | tee -a bench-results.txt
            exit 1
          fi
          critcmp "${BENCH_COMPARE_BASELINE_NAME}" "$candidate_name" 2>&1 | tee -a bench-results.txt
        fi
        ;;
      *)
        echo "unknown bench mode: {{ MODE }} (use build|run|ci)"
        exit 2
        ;;
    esac

# The pool budget and allocation tests live in the integration crate, as
# modules of `suite_light`, not as test targets of `kithara-bufpool`. They were
# named here as targets of the pool crate long after they moved.
memory:
    @echo "=== Tier 1: Pool-native budget tests ==="
    cargo test -p kithara-integration-tests --test suite_light kithara_bufpool::memory_budget
    @echo ""
    @echo "=== Tier 2: Allocation regression tests ==="
    cargo test -p kithara-integration-tests --test suite_light kithara_bufpool::alloc_regression -- --test-threads=1
    @echo ""
    @echo "=== Tier 3: Audio hot path zero-alloc ==="
    cargo test -p kithara-integration-tests --test suite_heavy kithara_audio::alloc_free_hotpath -- --test-threads=1
    @echo ""
    @echo "=== Tier 4: RSS budget (HLS playback) ==="
    cargo test -p kithara-integration-tests --features perf --test memory_rss -- --test-threads=1
    @echo ""
    @echo "All memory checks passed."
