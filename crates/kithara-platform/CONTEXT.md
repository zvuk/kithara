# kithara-platform — Context

Contracts and invariants for the kithara-platform crate; the README is the overview.

## Backend Selection

Consumer surface is the root facade (`sync`, `thread`, `time`, `tokio`, `env`, `logging`,
`maybe_send`). `system`, `loom`, `backend/*` are private; `lib.rs` picks one combination at
compile time.

| Features | Facade | Primitive backend |
|---|---|---|
| none | system | system |
| `flash` | flash wrappers | system |
| `loom` | loom | loom |
| `flash,loom` | flash wrappers | loom |

`flash` decorates the selected primitive backend; it never exposes a second backend path.
`wasm32` compiles its own tree (`src/wasm/`) and ignores these features. Exception: the
process-wide flash engine (`flash/system/`), its async task gate, and the diagnostics registry
always take bookkeeping locks from the private `system::lock::Mutex` (they outlive individual
Loom model permutations).

## Synchronization Types

| Type | system | loom | wasm32 |
|---|---|---|---|
| `Mutex` / `RwLock` / `Condvar` | `parking_lot` wrappers | `loom::sync` wrappers | `wasm_safe_thread` wrappers |
| `atomic::*` | `std::sync::atomic` | `loom::sync::atomic` | `std::sync::atomic` |
| `MaybeSend` / `MaybeSync` | equivalent to `Send` / `Sync` | same | blanket trait, no bound |

`Arc<T>`, `Weak<T>`, `OnceLock<T>` route through the system backend (`std::sync`) on every lane
— one ownership type workspace-wide; Loom models only the coordination primitives stored inside
that ownership graph. Consumers import every synchronization type from this crate, never from
`std` directly.

`Mutex` and `RwLock` are **flash-blind by contract**: they guard bounded critical sections whose
holder always runs (never parks while held), so virtual time can never advance with the lock
held. Flash-awareness lives only in the wait primitives (`Condvar`, `Notify`, `mpsc`);
`flash::sync::{Mutex, RwLock}` are plain re-exports.

`ThreadGate` (`common/gate.rs`) has ONE waiter. Same-thread registration performs no publication;
a changed waiter publishes through `ArcSwap` and keeps displaced handles in a writer-only retire
list until no signal guard remains. `signal` advances the sequence, lock-free snapshots the waiter,
and unparks without touching that list. An RT signal thread must pre-warm `arc_swap`'s per-thread
debt node; the audio source does so in `warm_up`. Every `WaitGate` caller snapshots `current()`
BEFORE checking its predicate, so a racing `signal` is never lost.

## Loom Lane

- Model tests opt in with `#[kithara::test(loom)]`; the macro renames to `loom_model_<name>` and
  calls the hidden `__private::model` hook. No Loom type or module is public.
- `just test run --loom=on` selects the lane (feature `kithara/loom`, nextest filter
  `test(/(^|::)loom_model_/)`), so unmarked tests never run under Loom. Plain `just test` leaves the
  feature off and a marked test runs once on the system backend; `--flash=on` runs the same models
  through the flash decorator.
- Loom has no timed waits: `thread::sleep`, `thread::park_timeout`, `Condvar::wait_timeout`,
  `mpsc::recv_timeout` panic in the `loom`-only lane. Models needing virtual deadlines run under
  `flash,loom`.
- `Arc` / `Weak` / `ArcSwap` operations stay outside the model. A model asserts a small
  deterministic coordination contract, never a network, decoder, Tokio runtime, random fixture, or
  wall-clock integration test.

## Thread and Task Primitives

| API | native | wasm32 |
|---|---|---|
| `thread::spawn` | `std::thread::spawn` | `wasm_safe_thread::Builder::spawn` (Web Worker) |
| `tokio::task::spawn` | tokio task runtime | worker-aware wrapper over `tokio_with_wasm` |
| `tokio::task::spawn_blocking` | tokio blocking pool | dedicated worker-thread execution |
| `thread::is_main_thread` / `is_worker_thread` | always true / false | browser main vs Worker scope |
| `thread::assert_main_thread` / `assert_not_main_thread` | no-op | panic on wrong affinity |

`tokio` is re-exported as an ENUMERATED subset (`system/tokio/mod.rs`): no glob, no root `spawn`
/ `spawn_blocking`, no `time` / `fs` / `process`. `net` plus the async I/O extension traits sit
behind `tokio-net`, `signal` behind `signal`. This crate is the sole sanctioned holder of a
direct `tokio` dependency (`arch.tokio_dep_quarantine`); consumers route through the platform
spawn chokepoints.

## Time

`time::{sleep, timeout, Instant, Duration, SystemTime}` is the single clock chokepoint: native
delegates to `tokio` timers and `web_time::Instant`, wasm uses `setTimeout`-based scheduling,
and under `flash` it is the virtual clock. `web_time` is internal to this crate and
`std::time::{Instant, Duration}` is banned outside it. Gates: `arch.no-direct-time` (covers
`std::time`, `web_time`, `tokio::time`), `arch.no-direct-thread-wait` (blocking thread waits),
`arch.no-implicit-clock` (hidden clock reads in library code).

## Feature Flags

All off by default. `flash`, `loom`, `no-block` are native and test-only, referenced by no
shipping crate; `loom` composes independently with `flash`. `signal` forwards `tokio::signal`
for desktop binaries that own process shutdown. `tokio-net` adds `tokio::net` plus
`AsyncReadExt` / `AsyncWriteExt`. `tokio-rt-multi-thread` forwards Tokio's multi-thread runtime
feature.

## Virtual Time (`flash`)

`flash` replaces the wall clock with a process-global virtual timeline so warm-cache offline
playback tests run at CPU speed. Off the feature every flash macro is an identity no-op.

`time::Instant` follows that virtual timeline. `time::WallInstant` is the explicit real monotonic
clock for external host APIs such as a window event loop whose deadline type cannot be virtual;
application timers must continue to use `Instant`.

### Annotation model (REAL by default)

Flash is opt-in per region, not a global mode flip. Two flags of the per-thread `Mode` (the
`ambient` / `active` fields of the single `ThreadCtx` thread-local in `flash/ctx.rs`) decide
virtual vs real; **both default to false ⇒ REAL by default**.

- `ambient` — per-test gate, set by `#[kithara::test(flash(bool))]`, propagated across
  `thread::spawn`, `thread::spawn_named`, `tokio::task::spawn` / `spawn_on` (per poll, via
  `with_ambient`), and `spawn_blocking`. Read by `flash_ambient()`.
- `active` — dynamic gate, pushed by a production `#[kithara::flash(bool)]` region (sync RAII
  `enter_dynamic`, async per-poll `FlashDynamic`) and itself gated by ambient, so a dynamic flash
  is inert outside an ambient flash test. Read by `flash_enabled()`.

They govern disjoint primitive classes:

- `flash_enabled()` — **stateless** time primitives: `time::sleep` / `time::timeout`,
  `Instant::now`, `thread::{sleep, park_timeout, yield_now, paced_backoff, unpark}`.
- `flash_ambient()` — **stateful** sync primitives `Condvar`, `Notify`, `mpsc`, `oneshot`, each
  latching the gate ONCE at construction (`Backend` in `flash/ids.rs`), plus
  `tokio::task::yield_now`, which consults ambient per call (a yield has no signal partner, so no
  latch). Their wait and signal run on different threads; a per-callstack gate would let the two
  sides target different engines and hang.

Annotation scope:

- Production `#[kithara::flash(true|false)]` is **dynamic**: pushes `active` through the region's
  call stack and across its spawns (audio worker `run_loop`, downloader yield loops, preload
  gate).
- Test `#[kithara::test(flash(true|false))]` is **lexical**: rewrites the test BODY's *direct*
  `time::*` / `Instant::now` / `thread::park_timeout` calls to the unconditional `virtual_*` entry
  points (body-only — a prod fn the body calls keeps its stateless time real unless that fn is
  itself `#[flash]`) and sets ambient for the whole test graph. Default under the feature is
  `flash(true)`; real-socket suites are `flash(false)`, whose graph is identical to production. The
  rewriter keys on the **last two path segments**: a body must use a qualified path (`time::sleep`)
  and a bare single-segment call is a compile error, not a silent real-clock call.

`thread::spawn_named` threads are **dedicated virtual-time pacers**: the child sets
`active = ambient` for its whole callstack, so its `Instant::now` reads (keyed on `active`) share
a clock domain with its waits (keyed on ambient). Ambient `spawn_blocking` closures reserve the
same kind of slot on the parent, before the pool queues the closure.

### Quiescence engine

The clock is **quiescence-driven**, not additive: an engine's `Clock` (one per `FlashInner`; the
process-wide `FLASH` instance backs the wrapped primitives, read lock-free by `Instant::now`)
advances only when every participating root is parked, and only ever jumps to the **earliest**
registered deadline. Clock values are a pure function of the pending-deadline multiset,
independent of scheduling: runs are deterministic and every timed wait collapses to zero real time.

- `time::Instant` is a drop-in struct backed by the engine `Clock` with the std `Instant` surface;
  all arithmetic saturates.
- Participant accounting is **intrinsic to the wrapped primitives**: no registration API, no
  consumer registers anything. A thread earns credit lazily on its first wrapped wait, tracked in
  a per-thread `Credit` (`None` → `Running` → `Parked`). `None` is invisible to the engine (owns
  no deadline, not in `active`), so a busy-spin thread cannot stall the clock. The FIRST wait
  bootstraps to `Parked` WITHOUT decrementing `active`; its wake does `active += 1` → `Running`,
  balancing the bootstrap. Later waits decrement and their wakes re-increment.
- `thread::spawn`, `thread::spawn_named`, and platform `task::spawn_blocking` reset the credit on
  entry (a reused pool thread must not inherit a stale credit) and own the exit decrement via an
  RAII participant after the closure returns *or unwinds*. A consumer running a wrapped wait on a
  blocking pool thread must spawn through platform `task::spawn_blocking`.
- A wrapped wait does register-deadline (or a deadline-less entry for an untimed condvar wait),
  wait accounting, and advance-rule evaluation in ONE critical section under the engine lock,
  closing the wake-to-re-register race. Lock order is always domain → engine; wakes fire only after
  the engine lock is released.
- Async tasks are counted separately in `active_async`, owned by the spawn poll-wrapper
  (`participate`), not by the firer. Quiescence requires `active == 0 && active_async == 0`, with
  the single exception below: a counted task no thread can poll.

Virtualized waits:

- `thread::park_timeout(d)` registers an **unparkable** timed waiter and blocks off-lock until the
  clock crosses `now + d` OR a peer `thread::unpark` wakes it. A flash-active `unpark` fires BOTH
  the engine entry and the OS park slot (park mode is decided by the TARGET's flags). An unpark
  arriving before the park is remembered.
- `thread::sleep(d)` registers a pure timed waiter with no early wake.
- `thread::paced_backoff(d)` is the backoff for a **synchronous poll loop** whose data comes from
  another engine-visible thread. Off the sim path it is a real `sleep(d)` throttle; under `flash` a
  **deadline-less** cooperative yield (`yield_until_advance`) that cannot inflate the clock and
  re-polls in lockstep with the producer. Use it instead of `sleep` in every such loop.
- `sync::Condvar` waits register a condvar-group waiter keyed by per-condvar id; `notify_all` /
  `notify_one` signal that group through the engine. The waiter registers its engine entry
  **before** releasing the domain guard, so a predicate change plus notify is serialized after the
  entry — no lost wake. The caller re-checks its predicate after re-acquiring.
- `tokio::task::yield_now()` is engine-backed (a quiescence yield-waiter) only inside an ambient
  flash test, a plain scheduler yield otherwise: its grant needs `active_async == 0`, which a
  non-ambient task can never reach while holding its own slot across the yield.

Sharper advance rule: **the clock must not advance while anything is runnable at the current
instant, and a pending yield-waiter IS runnable-now.** When every parked timed waiter is an
event-driven `Thread` park (a `park_timeout` watchdog, no real timer), the engine drains
yield-waiters at the current instant instead of jumping to a watchdog deadline. With a real
`Timed`/`Condvar` waiter present the clock advances normally (draining yielders on the jump); a
yielder that cannot progress re-parks-timed, emptying the yield set, so this never livelocks.

Async counterpart: **a task no live thread can poll must not pin the clock.** A synchronous
wrapped wait taken inside a poll (a BRIDGED wait) releases the blocking task's own slot — but on a
`current_thread` runtime that thread is the only poller, so every OTHER task it drives is equally
unpollable while it blocks. One left `RUNNABLE` by a wake would pin the clock that wait's own
deadline needs, and neither side can move first: the crossfade hang, where a render loop's
`virtual_pace` released the root slot and the resulting quiescent edge granted a peer task's yield.
The gate samples each task's driver thread and whether that runtime has a single poller (per poll,
so a task spawned onto another runtime's handle is judged by the runtime that actually drives it);
`enter_wait_locked` marks the bridged thread; `Registry::pinning_async` subtracts exactly those
tasks. A multi-thread runtime keeps pinning — another worker can still deliver the poll.

`flash::reset()` clears the timeline and the engine. nextest's per-test process isolation keeps
global state clean between tests; `reset()` is for runners sharing a process.

### Real I/O pacing

A task awaiting a real socket parks and releases its quiescence slot, so the engine cannot observe
real transit. Corrective invariant, extending the pooled-participant one (`PoolParticipant` in
`flash/system/credit.rs`: an ambient `spawn_blocking` closure is real work the clock must not
advance past): **virtual time must never outrun real time while real I/O is in flight.**

- `flash::real_io()` returns a `RealIoScope` RAII bracket for ONE real I/O operation. Consumers
  use `#[kithara::flash(io)]`, which expands to the guard; `kithara-net` applies it around request
  establishment, full-body reads, and streaming body pending windows.
- While at least one scope is live the clock is **paced, not pinned**: the first scope anchors
  `(real instant, virtual nanos)` and the advance rule refuses any jump beyond
  `anchor_virtual + real_elapsed`. Pacing (not pinning) keeps a virtually-delayed peer live: a
  fixture server sleeping on a virtual delay still progresses at 1:1 real pace.
- The pacer is a raw `std` thread named `kithara-flash-io-pacer`, spawned lazily on first entry and
  deliberately INVISIBLE to the engine (a platform `spawn_named` would count it as a participant
  and pin the clock it exists to advance). It is **event-driven**: with no paced target it parks
  untimed, otherwise until the earliest paced deadline, re-running the advance rule on wake. A
  deferred advance unparks it to re-target; the pacer never unparks itself (that would arm its own
  token and busy-spin).
- When the last scope drops the anchor clears and full-speed collapse resumes immediately. Off
  `flash` (and on wasm) the scope is a ZST no-op.

### Hang diagnostics and the equivalence oracle

`flash::hang_dump(context)` renders the dumping thread's context, the engine snapshot (parked
participants, deadlines, pending signals), and the sync-primitive registry (live `Mutex`/`RwLock`
with holder and waiters, plus engine-backed kinds); it is pure, so the caller routes it anywhere.
`flash::log_hang_dump` emits it via `tracing` at ERROR. The test harness records the same dump in
its hang artifact before timeout panic/abort. The registry is gated at runtime by
`KITHARA_FLASH_SYNC_TRACE` (default off - a wrapped primitive then pays only a null check);
`KITHARA_FLASH_SYNC_BT=1` adds backtraces. Holder and waiter evidence otherwise comes from
registered identities and static source locations, not from live stacks.

Deadline-less (`indef`) waiters are the exception, because for them the parking site is the whole
diagnosis: nothing but a matching signal frees one, so a dump's own backtrace - taken by the
watchdog, not by the code that parked - names the wrong thread. Each `indef` entry therefore records
where it parked: the OS thread always (an id read), and that thread's own backtrace under
`KITHARA_FLASH_SYNC_BT`. Capture happens BEFORE the `core` lock; a stack walk under `core` would
hold the whole engine for its duration. Timed waiters record nothing - their deadline already names
the give-up authority.

The dump marks an `indef` waiter `pins_clock` when it is the reason the clock is stuck: an async
waiter whose task still holds an `active_async` slot, or a sync waiter whose parking thread is
`bridged` (parked mid-poll, so every task it drives is stranded). The marker needs no env var, so
every lane carries it; the recorded stack prints only for a marked waiter, since a healthy test
parks hundreds of unmarked ones.

Ground truth is two runs compared: the default real-time run (catches concurrency/timing bugs) and
the `flash` run (fast). Divergence in sample-count positions or PCM flags that virtualization
distorted something; the FILE phase-continuity cluster (sub-0.5-sample phase assertions) is the
first equivalence oracle.

## Blocking Detection (`no-block`)

Poll-scoped async blocking detection, test lanes only: enabled by `just test run --no-block=on`
and kept ON by `just ci gate` lane 1 (flash ON + no-block ON; lane 2 is flash OFF). The inert twin
`common/no_block_inert.rs` keeps prod builds byte-identical; the real tree `src/no_block/` is
cfg-free inside and selected in `lib.rs` like `flash` / `flash_inert`.

Two levels, both scoped by the `Watched` per-poll combinator (installed automatically at every
`#[kithara::test]` async root and at both spawn chokepoints — native and flash
`tokio::task::spawn` / `spawn_on`):

1. **Chokepoint (`forbid`)** — deterministic, timing-independent. The native blocking primitives
  (`thread::sleep` / `park` / `park_timeout`, `Condvar::wait` / `wait_timeout`, `mpsc::recv` /
  `recv_timeout`) refuse to run inside a poll, attributing the caller via an unbroken
  `#[track_caller]` chain. `Mutex`/`RwLock` locks, `spawn_blocking`, and wake operations are
  deliberately NOT intercepted (short locks in async are legal; long waits are level 2's job).
1. **Budget** — wall/CPU timing of each poll on the REAL clock (`std::time`, never the virtualized
  platform clock). Thread CPU comes from a per-thread snapshot with 1 ms max age, so a poll can
  include up to 1 ms of pre-poll CPU. Blanket default 3000 ms (`KITHARA_NO_BLOCK_BUDGET_MS`
  overrides); the strict 25 ms tier is opt-in via `#[kithara::no_block]` /
  `#[kithara::no_block(budget_ms = N)]`. Reports classify CPU-spin vs blocked-wait from the
  thread-CPU/wall ratio (spin ⇔ cpu ≥ 0.8 × wall). In panic mode the blanket tier panics ONLY on
  CPU-spin, still emitting census lines for blocked-wait/unclassified; the strict tier panics on
  every over-budget class.

Modes via `KITHARA_NO_BLOCK`: `panic` (default), `census` (log-only; `KITHARA_NO_BLOCK_LOG=<file>`
adds an append-mode sink, since nextest swallows passing-test stderr), `off` (skips timing). A
configured census sink is evidence: an open or write failure panics the attempt instead of silently
dropping a record.

Escape: `#[kithara::allow_block]` — RAII `Permit` on sync fns, per-poll `PermitPoll` combinator on
async fns. Guards are `!Send`: they mutate thread-local state in `Drop` and must not cross
`.await`. A permit suppresses level 1 AND pauses the level-2 timer.

Flash interop runs one way — flash calls `no_block`, never the reverse. A BRIDGED wait (wrapped
sync wait inside an async poll, see `flash/system/credit.rs`) reports through `forbid_bridged`
BEFORE the engine wait; the engine's own `Token::wait` runs under a permit, so engine coordination
neither trips level 1 nor counts against the budget.

`just test rtsan-async` compiles every watched poll as an RTSan nonblocking context (nightly,
`--cfg rtsan`); suppressions taxonomy in `.config/rtsan/async-suppressions.txt`.
`arch.no-raw-no-block` (ast-grep) bans hand-calling the expansion targets outside their owner
files.

## CancelToken

`CancelToken` is the **single** cancellation token type across the workspace — no crate depends on
an async-runtime cancel crate (`tokio_util`). It is built on a private **propagate-down `Node`
tree** (`common/cancel/node.rs`): one `AtomicBool` flag, a cold list of `Weak` children, a waker
registry, and a held-only `_parent` `Arc` that is never read (the walk is always *down*) but keeps
an intermediate node alive for its `Weak` children. The hot read path `is_cancelled()` is a single
`Acquire` load, lock-free and wait-free, hence RT-safe on the audio produce-core.

The tree uses `std::sync` only (no `parking_lot`, no flash variant), so it compiles unchanged on
wasm32. Every guarded section is a linear structural edit and wakers fire outside the lock, so a
poisoned lock is recovered with `into_inner()` rather than propagated.

- `cancel()` swaps the flag to `true` (`AcqRel`, so a repeat cancel is idempotent — neither
  re-drains nor re-recurses), drains this node's wakers, then recurses **down** through live `Weak`
  children. A master cancel reaches every descendant by *writing* their flags; the `Release` store
  happens before any waker fires (paired with the `Acquire` load in `is_cancelled`), so a thread
  observing a wake is guaranteed to see the flag.
- `child()` derives a **new** node under the parent's children-lock, so a concurrent `cancel()`
  either includes it in the snapshot or has already set the parent flag — in which case the child
  is born cancelled (flag and waker `fired` latch both set) and a future/waker on it never parks.
  Cancelling a child or sibling never marks the parent. `Clone` keeps the **same** node: cancelling
  a clone is observed by the original.
- `cancelled()` returns a borrowed `Cancelled<'_>` future, cancel-safe in `tokio::select!`
  (dropping it unregisters its slot). `on_cancel()` registers a synchronous waker — the counterpart
  for a thread parked on a flash-aware `Condvar`/`Notify` — and returns a `CancelWakerGuard` that
  unregisters on drop. The waker runs on the cancelling thread: keep it cheap, non-blocking,
  idempotent. A registration arriving after the drain fires immediately (the `fired` latch).

### Roots, scopes, groups

Two constructors mint a **fresh subtree root** instead of deriving from a parent:

- `CancelToken::root()` — the owning master a consumer-crate top holds and `cancel()`s on teardown
  (`kithara-app` `main`, the FFI player). `Drop` is passive: dropping the root does **not** cancel
  its subtree.
- `CancelToken::never()` — a sentinel where a token is structurally required but no cancellation
  source exists.

Both are restricted to owner/sentinel sites, enforced by `just lint arch` (`cancel_root_sites`) via
a per-file allowlist plus exempt crates in `.config/arch/thresholds.toml`. Everywhere else, derive
with `.child()` or take a `CancelToken` from your caller.

`CancelScope::new(Option<CancelToken>)` is the seam between composed and standalone subsystems and
the canonical replacement for a `cancel.unwrap_or_default()` fallback: `Some(parent)` makes the
scope's token a child (a master cancel reaches it), `None` mints a fresh root. `token()` vends
clones of that one node, `cancel()` cancels exactly that subtree, and `Drop` is passive (a composed
scope never cancels a token handed from above).

`CancelGroup` is a read-only OR-combinator built from `CancelToken` or `Vec<CancelToken>` via
`From` and composable with `|`: `is_cancelled()` is true once **any** source is cancelled,
`cancelled()` is one future parking a slot per source (dropping it unregisters every slot), and
equality is source-array identity (`Arc::ptr_eq`). An empty group never resolves.
