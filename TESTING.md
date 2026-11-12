# Testing kithara

How to run the tests and what the test harness gives you. Commands live in
`.config/just/test.just`, lanes in `.config/xtask.toml`, nextest profiles in
`.config/nextest.toml`.

## Running tests

```sh
just test                                # product suite: all decoders, virtual clock
just test run seek                       # tests whose name contains "seek"
just test run -E 'package(kithara-hls)'  # a nextest filter expression
just test run --flash=off                # the same suite on the real clock
just test run --profile ci               # a nextest profile
just test run --lane=doc                 # doc-tests (nextest does not run them)
just test all                            # suite + doc-tests
```

A positional argument is a substring of the test name; filter expressions go
after `-E`. Decoder backends (Symphonia, Apple, Android) are enabled by
`tests/Cargo.toml`, so one run covers every backend the platform compiles.

Tests of the tooling and of the harness itself are separate lanes; CI runs them
on every `main` push and nightly:

```sh
just test run --lane=tooling    # kithara-devtools, xtask
just test run --lane=harness    # kithara-test-utils, kithara-test-macros
just test run --lane=fixtures   # fixture library, local test server, signal oracles
```

## Virtual clock (`flash`)

By default tests run under `flash`, a virtual clock in `kithara-platform`.
Sleeps, timeouts, `Instant::now`, and the tokio sync primitives are replaced by
versions that park on an engine; the engine advances time only when no task can
make progress without it.

- **Fast.** A test that models minutes of playback runs in milliseconds.
- **Deterministic.** Seek, ABR switch, and underrun ordering does not depend on
  machine load.
- **Catches lost wakeups.** A primitive the engine cannot see lets virtual time
  jump to the deadline, so the bug fails every run instead of rarely.

Rules that follow from it:

- A primitive that crosses between a real thread and a virtual-time task must
  be the flash-aware one from `kithara-platform`; a plain tokio primitive there
  makes virtual time run past the handoff.
- A production async helper whose sleep must take part in virtual time carries
  `#[kithara::flash(true)]`.
- `flash` does not fix logic: a bug on the real clock fails the same way on the
  virtual one. Run `--flash=off` for time-sensitive changes, after the
  `--flash=on` run and not concurrently with it, so one run's load does not
  disturb the other.

A test that fails in the full suite but passes alone is load-dependent; one
that fails alone is deterministic.

## Test attributes

From `kithara-test-macros`, re-exported as `kithara::*`:

- `#[kithara::test]` — one attribute for sync, async, native, and wasm tests.
  Options: `tokio`, `multi_thread`, `browser`, `timeout(Duration)`,
  `hang_timeout_secs(N)`. Under `flash` the timeout is virtual.
- `#[kithara::fixture]`, `#[kithara::mock]`, `#[kithara::probe]` — fixtures,
  `unimock` mocks, USDT probes; compiled out of production builds.
- `#[kithara::hang_watchdog]` — fails a stuck loop with the source location of
  the last progress point instead of waiting for the global timeout. The budget
  is `hang_timeout_secs(N)`, else `KITHARA_HANG_TIMEOUT_SECS`, else the
  default. Set it per test, not globally: a tight global value trips slow
  network tests. A deadlock in `Drop` or runtime shutdown is caught by the
  nextest hard timeout instead.

## Audio fixtures

Test audio is generated at build time by `kithara-fixture-gen` and stored in a
content-addressed cache; tests receive it through `#[kithara::fixture]`
parameters and never encode audio themselves. `KITHARA_FIXTURE_CACHE` selects
the cache directory and must be set before the build.

## Nextest profiles

- `default` — 120 s slow timeout; a hung test is killed, not retried.
- `fast` — quick local iteration.
- `ci` — CI settings and JUnit output.
- `stress` — repeated runs (`--stress-count N` or `--stress-duration`).
