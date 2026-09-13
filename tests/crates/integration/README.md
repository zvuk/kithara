# Integration tests

Cross-component suites cover multi-instance playback, phase continuity, thread
budgets and integration regressions. Shared fixture helpers, server binaries,
benchmarks and performance scenarios live in the root `tests/` package.

These targets consume the existing `kithara-integration-tests` helper library.
See [test organization and execution](../../README.md) for suite gates and lanes.
