# Testing Kithara

How the workspace is tested, what the `flash` virtual clock is for, and an
honest read on whether it earns its keep. This is a guide, not a contract — the
authoritative commands live in the `justfile` and `.config/nextest.toml`; the
per-crate behavior contracts live in each crate's `README.md`.

## Running tests

The canonical gate is plain `just test` (whole workspace, all backends, `flash`
ON, `test-release` profile). Backend features (symphonia / apple / android) are
activated automatically by `tests/Cargo.toml`, so a single run exercises every
compiled-in decoder.

```sh
just test                  # whole workspace, flash ON (default), all backends
just test --flash=off      # real wall clock — the regression baseline
just test -p kithara-hls   # one package
just test --profile ci     # a specific nextest profile
just test EXPR             # a nextest filter expression, e.g. test(seek)
just test-doc              # doc-tests (nextest does not run them)
just test-e2e              # gated by the `e2e` feature (suite_e2e)
just test-cached           # opt-in ephemeral L2 fixture cache (profile `cache`)
```

The `--flash=*` token is stripped before reaching nextest; every other argument
passes through. `--flash=off` (aliases `--no-flash` / `--flash=false`) drops
`--features flash` and runs the suite on the real clock in a separate
`target-flash-off/` directory, so the two modes never share build artifacts.

### Two-mode gate

Time-sensitive behavior must hold under both clocks. Run them separately — not
concurrently — so neither mode's load perturbs the other (load contention is a
common source of false "flake" signals):

```sh
just test --flash=on    # virtual clock (the primary, default mode)
just test --flash=off   # real wall clock (regression baseline)
```

A change is considered certified for a mode only after consecutive clean runs;
a single failure resets the count. Reach for isolation (`just test <filter>`) to
tell a deterministic failure from a load-correlated one: a test that fails under
the full suite but passes alone is load-correlated, not deterministic.

### Profiles (`.config/nextest.toml`)

- `default` — `slow-timeout` 120s, terminate-after 1 (a hung test is killed, not
  retried).
- `ci` — CI tuning.
- `fast` — quick local iteration.
- `stress` — for the stress lanes (`--stress-count N` / `--stress-duration`).
- `cache` — `default` plus a setup script that wipes and re-exports a fresh
  `KITHARA_FIXTURE_CACHE` for an ephemeral, explicitly-scoped per-run cache.

### Fixture cache (L2)

Encode/mux fixtures are expensive to regenerate, so an on-disk cache is **on by
default** (unset `KITHARA_FIXTURE_CACHE` ⇒ a persistent default dir; see
`tests/src/fixture_cache.rs`). The opt-in `cache` profile gives an ephemeral,
per-run cache instead. Generated fixtures and test logs must stay a reasonable
size — `src/` is production code, large fixtures belong under `tests/`.

## Test attributes (`kithara-test-macros`)

- `#[kithara::test]` — unified sync/async/native/wasm test attribute. Options
  include `tokio`, `multi_thread`, `timeout(Duration::…)`, and
  `env(KEY = "value")`. Under `flash` the `timeout` is a **virtual** deadline.
- `#[kithara::flash]` / `#[kithara::flash(true|false)]` — a production
  dynamic-flash guard that propagates flash through the callstack and across
  spawns (sync: an RAII guard; async: a per-poll combinator). A no-op when the
  `flash` feature is off. Used to mark the few production async helpers whose
  virtual `sleep` must participate in the sim (see the cancel/flash contracts in
  `kithara-platform/README.md`).
- `#[kithara::hang_watchdog]` — wraps a function with a `HangDetector`.
- `#[kithara::fixture]`, `#[kithara::mock]`, `#[kithara::probe]` — rstest /
  unimock / USDT replacements gated to `cfg(any(test, feature = …))` so they are
  no-ops in production.

## Hang detection

A `HangDetector` watchdog fails a wedged test with the **source location** of
the stuck `hang_tick!` and of the last `hang_reset!` progress point, plus a spin
count — so a hang names *what* stopped making progress, not just "something hung."
The timeout is `KITHARA_HANG_TIMEOUT_SECS` (native) or a built-in default.

> Set `KITHARA_HANG_TIMEOUT_SECS` **per test** (via the attribute's `env(...)`),
> never globally on a whole `just test` run — a tight global value false-trips
> slow network-integration tests (e.g. the `hls_seek_*` family with built-in
> hundreds-of-ms delays). The watchdog does not catch runtime-shutdown / `Drop`
> deadlocks; those surface as the nextest hard-timeout (`SIGABRT`), not the
> in-test watchdog.

## The `flash` virtual clock

### What it is

`flash` (in `kithara-platform`, behind the `flash` feature) replaces the real
clock and async runtime primitives with a **virtual clock driven by a quiescence
engine**. Instead of sleeping for real, a task parks on the engine; the engine
advances virtual time only when every participant is quiescent — i.e. nobody can
make progress without time moving. `time::sleep`, `timeout`, `Instant::now`, and
the `tokio` sync primitives (`broadcast`, `mpsc`, `oneshot`, `watch`,
`Semaphore`, `Notify`, …) each have a sim-participating sibling under `flash`;
the production `native` / `wasm` paths re-export the real `tokio` primitives
unchanged.

### Why it exists

- **Determinism.** Time-dependent ordering (seek settle, ABR switch, underrun,
  preload gating) becomes reproducible — no wall-clock jitter, so a pass/fail is
  a property of the code, not of the machine's load that second.
- **Speed.** A test that would wait seconds of real time skips straight to the
  next virtual deadline, so the suite is fast despite modeling long timelines.
- **Bug-surfacing.** Because the engine only advances at genuine quiescence, a
  *lost wakeup* or a primitive the engine cannot see becomes a hard, repeatable
  failure instead of a rare real-time flake (see the analysis below).

### How real work and virtual time coexist

Real I/O and real CPU work **pace** the virtual clock: while a counted pacer
thread (e.g. a `spawn_named` decode worker) holds an `active` slot, the engine
cannot advance, so virtual time is pinned to real progress on that path. A
receiver that parks on a sim-participating channel registers an *untimed*
engine waiter, which the clock can never race past. This is exactly why every
cross-boundary primitive must be sim-aware — see the limitation below.

### Limitations

- **Real-I/O-paced vs fully-buffered.** A real-time-paced timer tracks virtual
  time only while real I/O paces it (e.g. HLS). On a fully-buffered source
  (a local file / in-memory MP3) virtual time can run ahead, which can read as a
  false "silent hang"; such async helpers must carry `#[kithara::flash(true)]`.
- **Flash-blind primitives.** Any async primitive that crosses the real-thread ↔
  sim-task boundary without a sim-participating wrapper is invisible to the
  engine: the clock advances past the handoff and a virtual `timeout` fires
  before the real work completes. The `flash/tokio/sync.rs` surface is
  deliberately enumerated (no glob) precisely so a flash-blind primitive cannot
  leak in unnoticed.
- **It does not fix logic.** `flash` makes *timing* deterministic; it does not
  paper over a real algorithmic defect — a bug that fails on the real clock
  fails identically on the virtual one.

## Is `flash` worth it? — an honest read

`flash` carries a real maintenance tax: every async primitive that can cross the
real↔sim boundary needs a sim-participating wrapper, and production async helpers
with virtual sleeps need explicit `#[kithara::flash]` annotation. Getting one of
these wrong produces *new* failure modes (spurious virtual timeouts, false silent
hangs) that don't exist on the real clock. And `flash` is not a substitute for
good test design: the real-clock (`--flash=off`) baseline still carries
timing-race flakes (manual-switch wake-delivery, seek-near-end, mmap-stress
lanes) that only an event-driven rewrite — wait on the *fact* (a byte written, a
state published), never a wall-clock sleep — truly removes. `flash` masks those
under the virtual clock; it does not fix them.

Against that cost, the upside is concrete and was demonstrated directly during
this integration:

- A background analysis worker delivered its result over a *flash-blind* `watch`
  channel. On the real clock the tests passed; under `flash` they timed out,
  because the engine — unable to see the parked receiver — advanced virtual time
  straight to the test deadline. That is a genuine class of bug (a primitive the
  scheduler can't account for) that the real-clock suite hides entirely. Making
  the `watch` sim-participating both fixed the tests and closed the blind spot.
- The same virtual clock turns multi-second timing scenarios into millisecond
  test runs and removes load-dependent jitter, so a red is a real red.

**Verdict.** For a real-time audio engine — where wakeup correctness, ordering,
and backpressure *are* the product — `flash` pays for itself: it makes the
timing-correctness contract testable, fast, and reproducible, and it actively
surfaces lost-wakeup / scheduler-blindness bugs that real-time tests cannot. The
condition is discipline: keep the sim-primitive surface complete and enumerated,
annotate cross-boundary helpers, and keep driving the `--flash=off` baseline
toward event-driven waits rather than leaning on the virtual clock to hide
real-time races. Used that way it is worth the tax; used as a flake-suppressor it
would not be.
