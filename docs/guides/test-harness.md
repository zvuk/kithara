# Test Harness

Use this when adding or debugging tests, changing test utilities, or explaining
validation scope.

## Acceptance

- `just test` is the acceptance entrypoint. Pass harness arguments through
  `just test run <args>`.
- `just test ui` is the complete UI acceptance entrypoint: unit and integration
  tests, GPU renderer tests, and host-parity captures. The general workspace
  lane excludes these tests because CI runs them separately. UI uses wall-clock
  scheduling because it exercises the real window and graphics contracts;
  virtual-clock coverage remains in the general runtime lanes.
- Raw `cargo test` or `cargo nextest` is a scoped probe, not a final claim.
- If a probe is reported, name the package, filter, lane, and why it is enough
  for that local question.

### Axes

- `flash` is the default axis and defaults ON.
- `no-block` is off by default; enable with `--no-block=on` for poll-blocking
  detector coverage.
- `just ci gate` keeps two explicit lanes: flash ON + no-block ON, and flash
  OFF.
- Tests that verify detector behavior are gated behind the `no-block` feature.

## Regression Tests

- A regression test must fail on the broken surface and pass after the fix.
- Prove the right reason: setup preconditions must be asserted, and the fixed
  code path must be load-bearing.
- Flash-sensitive changes should verify the relevant runtime surface. If
  `flash=off` is required because the test is real-time or live I/O, say so.
- Loom models run through `just test run --loom=on`; add `--flash=on` only when
  the modeled contract also requires Flash virtual-time behavior.

## Harness Shape

Each testing task has one primitive. Reach for it before writing a local
helper; if it lacks a knob, extend it in its owner.

| Task | Primitive | Owner |
| --- | --- | --- |
| Local HTTP server for an `axum::Router` | `TestHttpServer::new(router)`, `.url(path)` (feature `http-server`) | `kithara-test-utils` |
| HLS stream from a spec | `TestServerHelper`, `HlsFixtureBuilder` | `tests/src` |
| Offline player or host render | `OfflinePlayer`, `OfflineHostHarness` | `tests/src::offline` |
| Disk-backed queue | `DiskQueue` | `tests/src::offline` |
| Wait on a predicate | `wait_until` | `kithara-test-utils` |
| Wait on playback position, events, loader | `waits::*`, `render_until_position` | `tests/src::waits` |
| Temp dir or path | `temp_dir`, `temp_path`, `TestTempDir` (feature `temp-dir`) | `kithara-test-utils` |
| Cancel token | `cancel_token`, `cancel_token_cancelled` | `kithara-test-utils` |
| Flash-aware pacing | `virtual_pace` | `kithara-test-utils` |
| Seeded randomness | `Xorshift64` | `kithara-test-utils` |
| Log capture | `#[kithara::test(tracing("<filter>"))]` | `kithara-test-macros` |
| Buffer pools | `bufpool::{pools, pools_with_budget, TestPools}` | `kithara-test-utils` |
| Signal level, tone, phase | `signal::{rms, peak, goertzel_magnitude, ...}` | `kithara-test-fixtures` |
| Phase and output continuity oracles | `phase_continuity`, `output_continuity` | `tests/src` |

`arch.tests-use-shared-primitives` rejects local copies in test directories and
test files, and `arch.test-modules-use-shared-primitives` in `#[cfg(test)]`
modules under `src`: raw `TcpListener::bind("127.0.0.1:0")`, direct
`tracing_subscriber` setup, and items named like the primitives above.

- Do not hard-code ports or random global paths.
- Wait for observable conditions, events, or bounded predicates. Do not sleep
  arbitrary wall-clock windows unless the test is explicitly real-time.
- Live-network, device, or OS-surface tests must be marked by lane/attributes and
  must not be treated as deterministic unit coverage.

## Organization

- Add coverage to the owner suite or existing module test first.
- Create a new regression file only for a real named reproduction that does not
  fit an owner suite.
- Tests should assert contracts: state, events, bytes, positions, typed errors,
  or resource cleanup. Do not test only that nothing panicked.

## Test-Driven Development

- Behavior changes are driven by tests that describe the intended contract.
- Tests are deterministic and never depend on the external network.
- A test captures the contract, not an incidental implementation detail.
- Test logs and generated data stay at a reasonable size.
- `src/` is production code, not a fixture warehouse. Large fixtures, local
  servers, generated content, and multi-step scenarios belong in `tests/`;
  small unit-test modules may stay next to the code under `#[cfg(test)]`.
- Keep test helpers inside test modules. Do not add test-only fields, methods,
  functions, or branches to production types and implementations.
- Observe internal state through real behavior, events, or USDT probes attached
  to real operations. Do not add no-op functions solely as probe points.
- Any public API change comes with tests that capture the contract.
