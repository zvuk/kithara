# kithara-test-macros — Context

Contracts and invariants for the kithara-test-macros crate; the README is the overview.

## `#[kithara::test]` flags

A bare `#[kithara::test]` is a sync test emitted for native + wasm; flags combine (e.g.
`#[kithara::test(native, tokio, timeout(Duration::from_secs(5)))]`). Every path supports `#[case]` / `#[case::name]`
parameterization and `#[kithara::fixture]` injection.

- `tokio` — async test on a manually built native runtime.
- `wasm` / `native` — single-platform emission; mutually exclusive.
- `browser` — browser wasm path; awaits `kithara_platform::tokio::ensure_thread_pool()` before the body so Web Workers exist.
- `timeout(Duration::...)` — wall-clock safety net that must fire on REAL time even under flash. Async wraps in
  `platform::time::timeout`; native sync runs the body on a helper thread with `recv_timeout`; wasm sync runs it unguarded.
- `env(KEY = "value", ...)` — sets vars under the process-wide `platform::env::mutation_lock()` and restores previous
  values on drop. Naming `NO_PROXY` also removes every `*_PROXY` variable not listed explicitly.
- `tracing("directives")` — `EnvFilter` directives, default `warn`; `RUST_LOG` still wins.
- `soft_fail("pattern", ...)` — catches panics whose message contains any pattern (case-insensitive; `"timeout"` also
  matches `timed out`) and prints `[SOFT FAIL]`; other panics resume unwinding. Requires `futures` at the call site for
  async tests.
- `serial` — emits `#[serial_test::serial]`; requires `serial_test` at the call site.
- `multi_thread` — `new_multi_thread().worker_threads(2)` instead of `new_current_thread()`; requires `tokio`.
- `selenium` — implies `native + tokio + serial + multi_thread`. It injects no `#[ignore]`: the suite is picked up only by
  the wasm-target driver (`just test run --lane=selenium`).
- `loom` — synchronous native model test returning `()`. Incompatible with `tokio`, async fns, non-unit returns,
  `soft_fail`, `wasm`, `browser`, `selenium`, and `multi_thread`.
- `flash(true|false)` — opt the body into or out of flash time rewriting; default `true`.

## Flash rewriting and ambient holders

Under `flash(true)` the macro lexically retargets the body's direct time-primitive calls onto the unconditional
`virtual_*` variants. The rewriter matches the last two path segments (`time::sleep`), so a single-segment `sleep(...)` in
the body is a compile error rather than a silent real-clock call: import the module and call `time::sleep(...)`, or use
`flash(false)`.

### Time rewriting only reaches path calls

`rewrite.rs` retargets **path** calls in a test body — `Instant::now()` becomes
`flash::virtual_now()`, which reads the engine clock unconditionally. It does not
touch **method** calls, so `started.elapsed()` stays `Instant::elapsed`, which
re-reads `now()` through the ordinary arm and gets REAL time: a test body never
sets the `active` flag, which only production `#[kithara::flash]` regions push.

Subtracting the two therefore mixes a virtual start from a real end. The error is
systematic, not jitter — the real arm is measured from the engine's real anchor,
so it grows with process age and a wall-clock assertion fails more often the
longer the process has lived.

In a test body, sample **both** endpoints with `Instant::now()` and subtract them:
`Instant::now().saturating_duration_since(started)`. Never `.elapsed()`, and never
any other method that internally re-reads `now()`. The rewrite reaches nested
closures and spawned blocks too, so a timing probe inside `tokio_spawn(async move
{ … })` carries the same rule.

Every emitted body is made flash-eligible by exactly ONE ambient holder. **async-native** emissions (manual tokio runtime,
with or without `timeout`) wrap the body in `kithara_platform::flash::with_ambient`, which re-asserts the ambient around
every poll; they must NOT also hold a body scope, because a body-held `ambient_scope` lives in the future's state inside
the cancellable timeout and tears down non-LIFO on `Elapsed` — a stale ambient resurrect, caught by the platform's
`restore_mode` guard. **native sync** and **wasm** emissions instead open the body with a single body-held `ambient_scope`
(`shared::make_ambient_stmt`), the sole ambient writer there. Off the `flash` feature both `virtual_*` and
`ambient_scope` are real-aliases / no-ops, so the emitted body stays behaviour-identical.

## Loom models

`loom` renames the test `loom_model_<name>` and wraps the body in the hidden `kithara_platform::__private::model` hook;
lane selection and Loom primitive semantics are owned by `crates/kithara-platform/CONTEXT.md`.

Do not annotate a full flaky async integration test: Loom cannot model sockets, Tokio scheduling, random input, decoder
FFI, or wall time. Extract the smallest deterministic synchronization contract that explains the flake and exercise it
with `kithara::platform::sync`, `thread`, and atomics. A spin/poll loop whose progress needs another modeled thread must
call `thread::yield_now`. Assertion and Loom panics propagate without `catch_unwind`; no per-permutation result is
buffered.

Debugging follows Loom's checkpoint contract: `just test loom-checkpoint FILE FILTER` saves progress while reproducing,
`loom-isolate` resumes with `LOOM_CHECKPOINT_INTERVAL=1` so the file pins the failing permutation, and `loom-debug`
replays it with `LOOM_LOG=trace`, `LOOM_LOCATION=1`, and uncaptured output. The checkpoint JSON is an opaque execution
path, not a report to read by hand; read a replay backward from the first panic or deadlock — `Iteration N` names the
schedule, `thread{id=N}` the executing modeled thread, `branch switch=true` an actual scheduler handoff, the primitive
trace (`mutex`, `atomic`, `notify`, channel) plus its `location=` field the operation to inspect, and `thread_done` normal
completion. A branch-limit failure usually means the model is too broad or a fairness-dependent loop is missing
`yield_now` — shrink the model before raising limits, and set `LOOM_MAX_PREEMPTIONS` (usually 2 or 3) explicitly for a
deliberately bounded large model.

## `#[kithara::probe(...)]` arguments

The probe body is gated `cfg(any(test, feature = "probe"))` and its emit runs inside a `kithara_test_utils::rtsan::permit`
guard, so probes stay active but `RTSan`-transparent under `--cfg rtsan`.

- `#[kithara::probe]` (no parens) — marker probe: zero wire args, only the cheap auto-fields (`caller_file`,
  `caller_line`, `seq`, `thread_id`, `thread_seq`, `install_id`). Use for very-frequent production functions whose
  parameters are not `IntoProbeArg` (e.g. `Future::poll_next(&self, cx: &mut Context)`).
- `#[kithara::probe(field1, field2, …)]` — parameter idents recorded as wire args; each must match a real parameter name.
- `#[kithara::probe(name = expr, …)]` — records a computed value under wire-name `name`. `expr` is evaluated inside the
  function body at probe-firing time (so it can read `self`, parameters, locals) and must implement `IntoProbeArg`. A
  computed name may not collide with a plain ident in the same attribute; `caller` and `probe_return` are reserved.
- Plain and computed entries share one ceiling: **6 wire args total**, the USDT provider arity limit. Over it, fold fields
  into a `#[derive(kithara::Probe)]` struct or split the function.
- `#[kithara::probe(caller, …)]` — additionally resolve `caller_fn` through `backtrace::trace`. Opt-in because resolution
  costs roughly a millisecond per firing; do NOT use on `poll_next`-style hot probes.
- `#[kithara::probe(probe_return)]` — records the return value through `Probe::record_probe`, emits no entry event, and is
  the only form that drops `#[track_caller]`.
