set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := "../.."

# Run perf-feature integration tests.
[default]
run:
    cargo test -p kithara-integration-tests --features perf --release \
      -- --ignored --test-threads=1 --nocapture 2>&1 | tee perf-results.txt

[positional-arguments]
suite *ARGS:
    just _xtask perf "$@"

[positional-arguments]
compare *ARGS:
    just _xtask perf-compare "$@"

bench MODE="build":
    #!/usr/bin/env bash
    set -uo pipefail
    case "{{ MODE }}" in
      build)
        cargo bench -p kithara-integration-tests --bench abr_estimator --no-run
        cargo bench -p kithara-integration-tests --bench bufpool --no-run
        cargo bench -p kithara-integration-tests --bench refactor_hotpaths --no-run
        ;;
      run)
        sample_size="${BENCH_SAMPLE_SIZE:-20}"
        for b in abr_estimator bufpool refactor_hotpaths; do
          cargo bench -p kithara-integration-tests --bench "$b" -- --sample-size "$sample_size" 2>&1 | tee -a bench-results.txt
        done
        ;;
      ci)
        cargo bench -p kithara-integration-tests --bench abr_estimator --no-run
        cargo bench -p kithara-integration-tests --bench bufpool --no-run
        cargo bench -p kithara-integration-tests --bench refactor_hotpaths --no-run
        if [[ "${RUN_BENCHMARKS:-0}" != "1" ]]; then
          echo "==> benchmark execution skipped (set RUN_BENCHMARKS=1 to execute)" | tee bench-results.txt
          exit 0
        fi
        sample_size="${BENCH_SAMPLE_SIZE:-20}"
        candidate_name="${BENCH_CANDIDATE_NAME:-ci}"
        for b in abr_estimator bufpool refactor_hotpaths; do
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

memory:
    @echo "=== Tier 1: Pool-native budget tests ==="
    cargo test -p kithara-bufpool --test memory_budget
    @echo ""
    @echo "=== Tier 2: Allocation regression tests ==="
    cargo test -p kithara-bufpool --test alloc_regression -- --test-threads=1
    @echo ""
    @echo "=== Tier 3: Audio hot path zero-alloc ==="
    cargo test -p kithara-integration-tests --test suite_heavy kithara_audio::alloc_free_hotpath -- --test-threads=1
    @echo ""
    @echo "=== Tier 4: RSS budget (HLS playback) ==="
    cargo test --test memory_rss -- --test-threads=1
    @echo ""
    @echo "All memory checks passed."
