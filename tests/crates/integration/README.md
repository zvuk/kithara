# Integration tests

Cross-component suites cover multi-instance playback, phase continuity, thread
budgets and integration regressions. Shared fixture helpers live in `src/`;
server binaries, benchmarks and opt-in performance scenarios stay in this package.

Domain packages consume the existing `kithara-integration-tests` helper library.
See [test organization and execution](../../README.md) for suite gates and lanes.
