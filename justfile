set shell := ["bash", "-euo", "pipefail", "-c"]

# Show available commands.
default:
    @just --list

fmt-check:
    cargo +nightly fmt --all --check

clippy:
    cargo clippy --workspace -- -D warnings

test:
    cargo nextest run --workspace

test-ci:
    cargo nextest run --workspace --profile ci --no-fail-fast

test-doc:
    cargo test --doc --workspace

test-all: test test-doc

ast-grep-blocking:
    AST_GREP_BIN="$(command -v ast-grep || command -v sg || true)"; \
    if [[ -z "$AST_GREP_BIN" ]]; then \
      echo "FAILED: ast-grep is required but not installed."; \
      exit 1; \
    fi; \
    "$AST_GREP_BIN" scan --config sgconfig.yml --report-style short \
      --filter '^(style.no-tests-in-lib-or-mod-rs|rust.no-thin-async-wrapper|style.no-separator-comments-toml)$'

ast-grep-advisory:
    AST_GREP_BIN="$(command -v ast-grep || command -v sg || true)"; \
    if [[ -z "$AST_GREP_BIN" ]]; then \
      echo "FAILED: ast-grep is required but not installed."; \
      exit 1; \
    fi; \
    "$AST_GREP_BIN" scan --config sgconfig.yml --report-style short --warning

arch:
    bash scripts/ci/check-arch.sh

machete:
    cargo machete

perf-compare-selftest:
    bash scripts/ci/test-compare-perf.sh

quality-report:
    bash scripts/ci/quality-report.sh

play-unimock-check:
    bash scripts/ci/check-play-traits-unimock.sh

rstest-audit:
    bash scripts/ci/rstest-audit.sh

trait-mock-audit:
    bash scripts/ci/trait-mock-audit.sh

trait-mock-exceptions:
    bash scripts/ci/trait-mock-exceptions.sh

lint-fast: fmt-check clippy ast-grep-blocking arch

lint-full: lint-fast perf-compare-selftest quality-report play-unimock-check rstest-audit trait-mock-audit trait-mock-exceptions

coverage:
    OUTPUT_DIR="${COVERAGE_OUTPUT_DIR:-./coverage}"; \
    COVERAGE_MIN="${COVERAGE_MIN:-80}"; \
    mkdir -p "$OUTPUT_DIR"; \
    cargo llvm-cov nextest \
      --workspace \
      --profile ci \
      --cobertura \
      --output-path "$OUTPUT_DIR/cobertura.xml" \
      --ignore-filename-regex '(tests/|examples/|benches/)' \
      --fail-under-lines "$COVERAGE_MIN"; \
    echo "==> coverage report written to $OUTPUT_DIR/cobertura.xml"; \
    echo "==> line coverage threshold: $COVERAGE_MIN%"

perf-test:
    cargo test -p kithara-integration-tests --features perf --release \
      -- --ignored --test-threads=1 --nocapture 2>&1 | tee perf-results.txt

bench-build:
    cargo bench -p kithara-abr --bench abr_estimator --no-run

bench-run:
    sample_size="${BENCH_SAMPLE_SIZE:-20}"; \
    cargo bench -p kithara-abr --bench abr_estimator -- --sample-size "$sample_size" 2>&1 | tee bench-results.txt

bench-ci:
    just bench-build; \
    if [[ "${RUN_BENCHMARKS:-0}" != "1" ]]; then \
      echo "==> benchmark execution skipped (set RUN_BENCHMARKS=1 to execute)" | tee bench-results.txt; \
      exit 0; \
    fi; \
    sample_size="${BENCH_SAMPLE_SIZE:-20}"; \
    candidate_name="${BENCH_CANDIDATE_NAME:-ci}"; \
    cargo bench -p kithara-abr --bench abr_estimator \
      -- --sample-size "$sample_size" --save-baseline "$candidate_name" \
      2>&1 | tee bench-results.txt; \
    if [[ -n "${BENCH_COMPARE_BASELINE_NAME:-}" ]]; then \
      if ! command -v critcmp >/dev/null 2>&1; then \
        echo "FAILED: critcmp is required when BENCH_COMPARE_BASELINE_NAME is set" | tee -a bench-results.txt; \
        exit 1; \
      fi; \
      baseline_name="${BENCH_COMPARE_BASELINE_NAME}"; \
      critcmp "$baseline_name" "$candidate_name" 2>&1 | tee -a bench-results.txt; \
    fi

wasm-test:
    bash scripts/ci/wasm-test.sh

wasm-build:
    bash crates/kithara-wasm/build-wasm.sh

wasm-size-check:
    bash scripts/ci/wasm-slim-check.sh
