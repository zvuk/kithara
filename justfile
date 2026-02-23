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

semgrep-blocking:
    SEMGREP_BIN="$(command -v semgrep || true)"; \
    if [[ -z "$SEMGREP_BIN" ]]; then \
      PY_USER_BASE="$(python3 -c 'import site; print(site.getuserbase())' 2>/dev/null || true)"; \
      CANDIDATE="$PY_USER_BASE/bin/semgrep"; \
      if [[ -x "$CANDIDATE" ]]; then \
        SEMGREP_BIN="$CANDIDATE"; \
      fi; \
    fi; \
    if [[ -z "$SEMGREP_BIN" ]]; then \
      echo "FAILED: semgrep is required but not installed."; \
      exit 1; \
    fi; \
    REPO_ROOT="$(pwd)"; \
    XDG_DIR="$REPO_ROOT/.cache"; \
    SEMGREP_DIR="$XDG_DIR/semgrep"; \
    mkdir -p "$XDG_DIR" "$SEMGREP_DIR"; \
    export XDG_CACHE_HOME="$XDG_DIR"; \
    export XDG_CONFIG_HOME="$XDG_DIR"; \
    export SEMGREP_LOG_FILE="$SEMGREP_DIR/semgrep.log"; \
    export SEMGREP_SETTINGS_FILE="$SEMGREP_DIR/settings.yml"; \
    export PATH="$(dirname "$SEMGREP_BIN")":$PATH; \
    if [[ -z "${SSL_CERT_FILE:-}" ]]; then \
      CERT_PATH="$(python3 -c 'import certifi; print(certifi.where())' 2>/dev/null || true)"; \
      if [[ -n "$CERT_PATH" && -f "$CERT_PATH" ]]; then \
        export SSL_CERT_FILE="$CERT_PATH"; \
      fi; \
    fi; \
    "$SEMGREP_BIN" --config semgrep.yml --severity ERROR --error

semgrep-advisory:
    SEMGREP_BIN="$(command -v semgrep || true)"; \
    if [[ -z "$SEMGREP_BIN" ]]; then \
      PY_USER_BASE="$(python3 -c 'import site; print(site.getuserbase())' 2>/dev/null || true)"; \
      CANDIDATE="$PY_USER_BASE/bin/semgrep"; \
      if [[ -x "$CANDIDATE" ]]; then \
        SEMGREP_BIN="$CANDIDATE"; \
      fi; \
    fi; \
    if [[ -z "$SEMGREP_BIN" ]]; then \
      echo "FAILED: semgrep is required but not installed."; \
      exit 1; \
    fi; \
    REPO_ROOT="$(pwd)"; \
    XDG_DIR="$REPO_ROOT/.cache"; \
    SEMGREP_DIR="$XDG_DIR/semgrep"; \
    mkdir -p "$XDG_DIR" "$SEMGREP_DIR"; \
    export XDG_CACHE_HOME="$XDG_DIR"; \
    export XDG_CONFIG_HOME="$XDG_DIR"; \
    export SEMGREP_LOG_FILE="$SEMGREP_DIR/semgrep.log"; \
    export SEMGREP_SETTINGS_FILE="$SEMGREP_DIR/settings.yml"; \
    export PATH="$(dirname "$SEMGREP_BIN")":$PATH; \
    if [[ -z "${SSL_CERT_FILE:-}" ]]; then \
      CERT_PATH="$(python3 -c 'import certifi; print(certifi.where())' 2>/dev/null || true)"; \
      if [[ -n "$CERT_PATH" && -f "$CERT_PATH" ]]; then \
        export SSL_CERT_FILE="$CERT_PATH"; \
      fi; \
    fi; \
    "$SEMGREP_BIN" --config semgrep.yml --severity WARNING

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

lint-fast: fmt-check clippy semgrep-blocking semgrep-advisory arch

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
