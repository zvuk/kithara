set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := "../.."

[positional-arguments]
report *ARGS:
    just _xtask quality report "$@"

[positional-arguments]
assess *ARGS:
    just _xtask quality assess "$@"

unimock-check:
    just _xtask quality unimock-check

rstest-audit:
    just _xtask quality rstest-audit

trait-mock-audit:
    just _xtask quality trait-mock-audit

trait-mock-exceptions:
    just _xtask quality trait-mock-exceptions

# Heavyweight analyzers kept outside lint-fast, audit, and pre-commit.
[positional-arguments]
lab *ARGS:
    just _xtask quality lab "$@"

# Run cargo-crap against the LCOV report.
[positional-arguments]
coverage-risk *ARGS:
    #!/usr/bin/env bash
    set -uo pipefail
    LCOV="${COVERAGE_OUTPUT_DIR:-./coverage}/lcov.info"
    coverage_rc=0
    just test coverage || coverage_rc=$?
    risk_rc=0
    if [[ -f "$LCOV" ]]; then
      just quality lab run coverage --lcov "$LCOV" "$@" || risk_rc=$?
    else
      echo "coverage-risk: LCOV artifact was not produced: $LCOV" >&2
      risk_rc=1
    fi
    if (( coverage_rc != 0 )); then
      exit "$coverage_rc"
    fi
    exit "$risk_rc"

# CI-tuned quality report; thresholds come from QUALITY_* variables.
report-ci:
    just _xtask quality report \
        --min-unimock-traits "${QUALITY_MIN_UNIMOCK_TRAITS:-0}" \
        --min-rstest-cases "${QUALITY_MIN_RSTEST_CASES:-0}" \
        --min-perf-test-files "${QUALITY_MIN_PERF_TEST_FILES:-0}" \
        --min-bench-targets "${QUALITY_MIN_BENCH_TARGETS:-0}" \
        --max-local-http-servers "${QUALITY_MAX_LOCAL_HTTP_SERVERS:-999}"
