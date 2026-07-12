# kithara-test-macros — Context

Detailed contracts and invariants for the kithara-test-macros crate; the README is the overview.

## `#[kithara::test]` flags

A bare `#[kithara::test]` is a sync test on native + wasm; flags can be combined (e.g. `#[kithara::test(native, tokio, timeout(Duration::from_secs(5)))]`).

- `tokio` — async test on native runtime
- `wasm` — wasm-only test
- `native` — native-only test
- `browser` — browser wasm test path; injects `kithara_platform::tokio::ensure_thread_pool().await` on WASM to initialize Web Workers before the body
- `timeout(Duration::...)` — wraps test body with a timeout
- `env(KEY = "value", ...)` — sets env vars before the body runs
- `tracing("directives")` — initializes tracing with a custom `EnvFilter` directive string (defaults to `warn` when omitted)
- `soft_fail("pattern", ...)` — catches panics whose message contains any given substring (case-insensitive), printing them as `[SOFT FAIL]` warnings; non-matching panics propagate. Requires `futures` at the call site for async tests
- `serial` — emits `#[serial_test::serial]` so the test never runs in parallel with other `serial` tests; for resource-intensive / contention-sensitive tests. Requires `serial_test` at the call site
- `multi_thread` — uses `new_multi_thread().worker_threads(2)` instead of `new_current_thread()`; required when the body spawns tasks needing a multi-threaded executor (e.g. `thirtyfour` `WebDriver`)
- `selenium` — convenience flag implying `native + tokio + serial + multi_thread` and adding `#[ignore = "requires selenium"]`, for Selenium/WebDriver integration tests
- `loom` - marks a synchronous native model test returning `()`. The body runs once with the system backend in ordinary lanes, or through `loom::model` when `kithara/loom` is enabled. It is incompatible with `tokio`, async functions, non-unit returns, `soft_fail`, `wasm`, `browser`, `selenium`, and `multi_thread`
- `flash(true|false)` — opt the test body into or out of flash time rewriting

Supports `#[case]` / `#[case::name]` parameterization and fixture injection.

### Loom execution and debugging

The macro emits a `loom_model_` test-name prefix. `just test --loom=on` enables
`kithara/loom`, uses the optimized test profile, and selects only that
prefix. This is required: enabling the Loom primitive backend while running an
unmarked test would access modeled primitives outside `loom::model`. Ordinary
`just test` never enables Loom and still runs the marked regression once, so
the model annotation adds no permutation cost to the normal gate. Use
`just test --loom=on --flash=on` to compose Flash over the Loom primitives.

Do not annotate a full flaky async integration test. Loom cannot model sockets,
Tokio scheduling, random input, decoder FFI, or wall time. Extract the smallest
deterministic synchronization contract that explains the flake and exercise it
with `kithara::platform::sync`, `thread`, and atomics. A spin/poll loop whose
progress requires another modeled thread must call `thread::yield_now`.

The model hook calls `loom::model` directly. Assertion and Loom panics propagate
without `catch_unwind`, and no per-permutation result is buffered. The debug
workflow follows Loom's checkpoint contract:

1. `just loom-checkpoint path/to/checkpoint.json TEST_FILTER` periodically saves progress while reproducing the failure.
2. `just loom-isolate path/to/checkpoint.json TEST_FILTER` resumes with `LOOM_CHECKPOINT_INTERVAL=1`; after it fails, the file identifies the exact failing permutation.
3. `just loom-debug path/to/checkpoint.json TEST_FILTER` replays that permutation with `LOOM_LOG=trace`, `LOOM_LOCATION=1`, and uncaptured output.

The checkpoint JSON is an opaque execution path, not a report to inspect by
hand. In the replay, switch markers show which modeled thread became runnable;
location records identify the primitive operation that created the branch. An
assertion failure or deadlock is a reachable modeled execution. A branch-limit
failure usually means the model is too broad or a fairness-dependent loop is
missing `yield_now`; shrink the model before raising limits. For a deliberately
bounded large model, set `LOOM_MAX_PREEMPTIONS` (usually 2 or 3) or another Loom
`Builder` environment limit explicitly.

Read a replay backward from the first panic or deadlock. `Iteration N` names the
replayed schedule, `thread{id=N}` names the currently executing modeled thread,
and `branch switch=true` records an actual scheduler handoff; `switch=false`
means the current thread retained control at that branch. The primitive trace
(`mutex`, `atomic`, `notify`, or channel) plus its `location=` field identifies
the synchronization operation to inspect. `thread_done` is normal completion,
not evidence of the failure.

### Flash ambient holder per emit path

Every emitted test body is made flash-eligible by exactly ONE ambient holder:

- **async-native** emissions (manual tokio runtime, with or without `timeout`) wrap the body in `kithara_platform::flash::with_ambient`, which re-asserts `FLASH_AMBIENT` around every poll. They must NOT also hold a body scope: a body-held `ambient_scope` lives in the future's state inside the cancellable timeout and tears down non-LIFO on `Elapsed` — a stale ambient resurrect, caught by the platform's `restore_mode` guard.
- **native sync** and **wasm** emissions open the body with a single body-held `ambient_scope` (`shared::make_ambient_stmt`) — the sole ambient writer there.

## `#[kithara::probe(...)]` arguments

- `#[kithara::probe]` (no parens) — marker probe: emits only the cheap auto-fields (`seq`, `caller_file`, `caller_line`) and zero wire args. Use for very-frequent production functions whose parameters are not `IntoProbeArg` (e.g. `Future::poll_next(&self, cx: &mut Context)`).
- `#[kithara::probe(field1, field2, …)]` — explicit list of parameter idents to record as wire args (max 6, the USDT arity ceiling). Each ident must match a real parameter name.
- `#[kithara::probe(name = expr, …)]` — record a computed value under the wire-name `name`. `expr` is evaluated inside the function body at probe-firing time (so it can read `self`, parameters, locals); its result must implement `IntoProbeArg`. Plain idents and `name = expr` entries may be mixed; the combined count counts against the 6-arg ceiling.
- `#[kithara::probe(caller, …)]` — additionally capture `caller_fn` via `backtrace::trace`. Opt-in because backtrace resolution is ~ms per firing and blows up hot loops; do NOT use on `poll_next`-style hot probes.
- `#[kithara::probe(probe_return)]` — record the function's return value through `Probe::record_probe`.
