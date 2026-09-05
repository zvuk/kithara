# kithara-platform — Context

Contracts and invariants for kithara-platform; the README is the overview.

## Backend Selection

The consumer surface is the root facade; `system`, `loom`, and `backend/*` are private and
`lib.rs` picks one combination at compile time. `flash` decorates the selected primitive backend
and never exposes a second backend path; `wasm32` compiles its own tree and ignores these
features. Exception: the process-wide flash engine, its async task gate, and the diagnostics
registry always take bookkeeping locks from the private `system::lock::Mutex`, because they
outlive Loom model permutations.

Features are all off by default. `flash`, `loom`, and `no-block` are native and test-only and no
shipping crate may reference them; `loom` composes independently with `flash`.

## Synchronization Types

`Arc<T>`, `Weak<T>`, and `OnceLock<T>` route through the system backend on every lane — one
ownership type workspace-wide, with Loom modelling only the coordination primitives stored inside
that ownership graph. Consumers import every synchronization type from this crate, never from
`std`.

Third-party crates that block through `parking_lot_core` — `dashmap` shards are the workspace's
only such consumer on wasm — are not covered by those wrappers. `parking_lot_core` picks its wasm
thread parker at compile time and, unless its `nightly` feature is on, selects the one whose every
method panics; `+atomics` alone is not enough, so a contended shard aborts the worker that touched
it. This crate turns that feature on for `cfg(target_arch = "wasm32")` and holds the reference in
`wasm/thread.rs`, which makes every wasm build nightly-only — the wasm target already is, since
`+atomics` forces `-Z build-std`. The resulting parker calls `memory.atomic.wait32`, which is legal
in a Worker and traps on the browser main thread; wasm playback owns its work in Workers for that
reason.

`Mutex` and `RwLock` are **flash-blind by contract**: they guard bounded critical sections whose
holder always runs and never parks while held, so virtual time can never advance with the lock
held. Flash-awareness lives only in the wait primitives, and `flash::sync::{Mutex, RwLock}` are
plain re-exports.

`ThreadGate` (`common/gate.rs`) has ONE waiter. Same-thread registration performs no publication;
a changed waiter publishes through `ArcSwap` and keeps displaced handles in a writer-only retire
list that `signal` never touches, so signalling stays lock-free. An RT signal thread must pre-warm
`arc_swap`'s per-thread debt node; the audio source does so in `warm_up`. Every `WaitGate` caller
snapshots `current()` BEFORE checking its predicate, so a racing `signal` is never lost.

## Loom Lane

- Model tests opt in with `#[kithara::test(loom)]`, which renames them `loom_model_<name>`.
- `just test run --loom=on` selects the lane, filtering on that prefix, so unmarked tests never
  run under Loom. Plain `just test` leaves the feature off and a marked test runs once on the
  system backend; `--flash=on` runs the same models through the flash decorator.
- Loom has no timed waits: every timed wait panics in the `loom`-only lane, so a model needing
  virtual deadlines runs under `flash,loom`.
- `Arc` / `Weak` / `ArcSwap` operations stay outside the model. A model asserts a small
  deterministic coordination contract, never a network, decoder, runtime, or wall-clock test.

## Thread and Task Primitives

On wasm32 a `thread::spawn` is a Web Worker and the thread-affinity assertions
(`assert_main_thread` / `assert_not_main_thread`) actually panic; natively they are no-ops, so an
affinity bug stays invisible until it runs in a browser.

`tokio` is re-exported as an ENUMERATED subset: no glob, no root `spawn` / `spawn_blocking`, no
`time` / `fs` / `process`. This crate is the sole sanctioned holder of a direct `tokio`
dependency (`arch.tokio_dep_quarantine`); consumers route through the platform spawn chokepoints.

## Time

`time::{sleep, timeout, Instant, Duration, SystemTime}` is the single clock chokepoint, and
under `flash` it is the virtual clock. `web_time` is internal to this crate and
`std::time::{Instant, Duration}` is banned outside it. Gates: `arch.no-direct-time`,
`arch.no-direct-thread-wait`, and `arch.no-implicit-clock` (hidden clock reads in library code).

## Ranged Values

`ranged!` declares a float newtype that cannot hold a value outside its range, so a bound
crosses a crate boundary as a type rather than as loose constants a caller must remember to
clamp against. `From` clamps and maps `NaN` to `DEFAULT` rather than propagating it through every
later comparison; `checked` rejects instead. The owning crate declares the type next to the code
that defines the range; every other crate imports it. Plain `f32` stays at ABI and RT-message
boundaries, which convert once at the door.

## Virtual Time (`flash`)

`flash` replaces the wall clock with a process-global virtual timeline so warm-cache offline
playback tests run at CPU speed. Off the feature every flash macro is an identity no-op.

`time::Instant` follows that virtual timeline and its arithmetic saturates. `time::WallInstant` is
the explicit real monotonic clock for external host APIs — a window event loop whose deadline type
cannot be virtual — and application timers must keep using `Instant`.

### Annotation model (REAL by default)

Flash is opt-in per region, not a global mode flip. Two flags of the per-thread `Mode` (in
`flash/ctx.rs`) decide virtual vs real; **both default to false ⇒ REAL by default**.

- `ambient` — per-test gate, set by `#[kithara::test(flash(bool))]` and propagated across every
  spawn form (per poll for async). Read by `flash_ambient()`.
- `active` — dynamic gate, pushed by a production `#[kithara::flash(bool)]` region and itself
  gated by ambient, so a dynamic flash is inert outside an ambient flash test. Read by
  `flash_enabled()`.

They govern disjoint primitive classes:

- `flash_enabled()` — the **stateless** time primitives (`time::sleep` / `time::timeout`,
  `Instant::now`, and the `thread` time helpers).
- `flash_ambient()` — the **stateful** sync primitives (`Condvar`, `Notify`, `mpsc`, `oneshot`),
  each latching the gate ONCE at construction, plus `tokio::task::yield_now`, which consults
  ambient per call (a yield has no signal partner, so no latch). Wait and signal run on different
  threads; a per-callstack gate would let the two sides target different engines and hang.

Annotation scope:

- Production `#[kithara::flash(true|false)]` is **dynamic**: it pushes `active` through the
  region's call stack and across its spawns.
- Test `#[kithara::test(flash(true|false))]` is **lexical**: it rewrites the test BODY's *direct*
  time calls to unconditional virtual entry points and sets ambient for the whole test graph. So a
  prod fn the body calls keeps its stateless time real unless that fn is itself `#[flash]`, and
  since the rewriter keys on the last two path segments the body must use a qualified path
  (`time::sleep`) — a bare single-segment call is a compile error, not a silent real-clock call.
  Default under the feature is `flash(true)`; real-socket suites are `flash(false)`, whose graph is
  identical to production.

`thread::spawn_named` threads are **dedicated virtual-time pacers**: the child sets
`active = ambient` for its whole callstack, so its `Instant::now` reads and its waits share one
clock domain despite being keyed on different flags.

### Quiescence engine

The clock is **quiescence-driven**, not additive: it advances only when every participating root
is parked, and only ever jumps to the **earliest** registered deadline. Clock values are a pure
function of the pending-deadline multiset, independent of scheduling: runs are deterministic and
every timed wait collapses to zero real time.

- Participant accounting is **intrinsic to the wrapped primitives**: there is no registration API
  and no consumer registers anything. A thread earns `Credit` lazily on its first wrapped wait,
  and until then is invisible to the engine — so a busy-spin thread that never waits cannot stall
  the clock, and neither can a test body that never waits.
- The platform spawn forms reset the credit on entry (a reused pool thread must not inherit a
  stale credit) and own the exit decrement via an RAII participant after the closure returns *or
  unwinds*, so a wrapped wait on a blocking pool thread must be spawned through platform
  `task::spawn_blocking`. They reserve the child's slot synchronously on the spawning thread,
  before the OS thread exists.
- Every peer a timed waiter depends on must therefore be spawned (its slot reserved) BEFORE the
  waiter can park: a waiter parked in a spawn→spawn gap is the only credit, and the clock jumps
  its whole backstop before the peer exists
  (`thread_gate_refreshes_waiter_after_thread_handoff`).
- A wrapped wait does deadline registration, wait accounting, and advance-rule evaluation in ONE
  critical section under the engine lock, closing the wake-to-re-register race. Lock order is
  always domain → engine; wakes fire only after the engine lock is released.
- Async tasks are counted separately in `active_async`, owned by the spawn poll-wrapper rather
  than the firer. Quiescence requires both counts at zero, with the single exception below: a
  counted task no thread can poll.

Virtualized waits:

- `thread::park_timeout(d)` is **unparkable**: it wakes when the clock crosses `now + d` or when a
  peer `thread::unpark` wakes it, whereas `thread::sleep(d)` has no early wake. A flash-active
  `unpark` fires BOTH the engine entry and the OS park slot (park mode is decided by the TARGET's
  flags), and an unpark arriving before the park is remembered.
- `thread::paced_backoff(d)` is the backoff for a **synchronous poll loop** whose data comes from
  another engine-visible thread: a real `sleep(d)` throttle off the sim path, and under `flash` a
  **deadline-less** cooperative yield that cannot inflate the clock and re-polls in lockstep with
  the producer. Use it instead of `sleep` in every such loop.
- A `sync::Condvar` waiter registers its engine entry **before** releasing the domain guard, so a
  predicate change plus notify is serialized after the entry — no lost wake. The caller re-checks
  its predicate after re-acquiring.
- `tokio::task::yield_now()` is engine-backed (a quiescence yield-waiter) only inside an ambient
  flash test, a plain scheduler yield otherwise: its grant needs `active_async == 0`, which a
  non-ambient task can never reach while holding its own slot across the yield.

Sharper advance rule: **the clock must not advance while anything is runnable at the current
instant, and a pending yield-waiter IS runnable-now.** When every parked timed waiter is an
event-driven thread park (a watchdog, no real timer), the engine drains yield-waiters at the
current instant instead of jumping to a watchdog deadline. With a real timed or condvar waiter
present the clock advances normally, draining yielders on the jump; a yielder that cannot progress
re-parks-timed, emptying the yield set, so this never livelocks.

Async counterpart: **a task no live thread can poll must not pin the clock.** A synchronous
wrapped wait taken inside a poll (a BRIDGED wait) releases the blocking task's own slot — but on a
`current_thread` runtime that thread is the only poller, so every OTHER task it drives is equally
unpollable while it blocks. One left runnable by a wake would pin the clock that wait's own
deadline needs, and neither side can move first — this was the crossfade hang. The gate therefore
subtracts exactly the tasks driven by a bridged thread, sampling each task's driver thread and
single-poller status per poll, so a task spawned onto another runtime's handle is judged by the
runtime that actually drives it. A multi-thread runtime keeps pinning — another worker can still
deliver the poll.

`flash::reset()` clears the timeline and the engine; nextest's per-test process isolation already
keeps global state clean, so it exists for runners that share a process.

### Real I/O pacing

A task awaiting a real socket parks and releases its quiescence slot, so the engine cannot observe
real transit. Corrective invariant, extending the pooled-participant one that covers an ambient
`spawn_blocking` closure: **virtual time must never outrun real time while real I/O is in
flight.**

- `flash::real_io()` brackets ONE real I/O operation; consumers use `#[kithara::flash(io)]`, and
  `kithara-net` applies it around request establishment, full-body reads, and streaming body
  pending windows.
- While at least one scope is live the clock is **paced, not pinned**: the first scope anchors a
  real instant to a virtual one and the advance rule refuses any jump past that anchor plus the
  real time elapsed since. Pacing rather than pinning keeps a virtually-delayed peer live — a
  fixture server sleeping on a virtual delay still progresses at 1:1 real pace.
- The pacer is a raw `std` thread, spawned lazily on first entry and deliberately INVISIBLE to the
  engine (a platform `spawn_named` would count it as a participant and pin the clock it exists to
  advance). It is event-driven: it parks until the earliest paced deadline and re-runs the advance
  rule on wake, and never unparks itself (that would arm its own token and busy-spin).
- When the last scope drops the anchor clears and full-speed collapse resumes immediately.

### Hang diagnostics and the equivalence oracle

`flash::hang_dump(context)` is pure, so the caller routes it anywhere; the test harness records it
in the hang artifact before its timeout panic. The sync-primitive registry it reads is gated at
runtime by `KITHARA_FLASH_SYNC_TRACE` (default off - a wrapped primitive then pays only a null
check), and `KITHARA_FLASH_SYNC_BT=1` adds backtraces. Holder and waiter evidence otherwise comes
from registered identities and static source locations, not from live stacks.

Deadline-less (`indef`) waiters are the exception: nothing but a matching signal frees one, so the
parking site is the whole diagnosis, and a dump's own backtrace - taken by the watchdog, not by the
code that parked - names the wrong thread. Each `indef` entry therefore records where it parked,
BEFORE taking the engine lock (a stack walk under it would hold the whole engine for its duration).
Timed waiters record nothing - their deadline already names the give-up authority.

The dump marks an `indef` waiter `pins_clock` when it is the reason the clock is stuck: an async
waiter whose task still holds an `active_async` slot, or a sync waiter parked mid-poll on a
bridged thread. The marker needs no env var, so every lane carries it; the recorded stack prints
only for a marked waiter, since a healthy test parks hundreds of unmarked ones.

Ground truth is two runs compared: the default real-time run, which catches concurrency and timing
bugs, and the fast `flash` run. Divergence in sample-count positions or PCM flags that
virtualization distorted something; the FILE phase-continuity cluster is the first such oracle.

## Blocking Detection (`no-block`)

Poll-scoped async blocking detection, test lanes only: enabled by `just test run --no-block=on`
and kept ON by `just ci gate` lane 1 (flash ON + no-block ON; lane 2 is flash OFF). Like `flash`
it is an inert twin swapped in at `lib.rs`, so prod builds stay byte-identical.

Two levels, both scoped by the `Watched` per-poll combinator, installed automatically at every
`#[kithara::test]` async root and at both spawn chokepoints:

1. **Chokepoint (`forbid`)** — deterministic, timing-independent. The native blocking primitives
  (thread parks, `Condvar` waits, blocking `mpsc` receives) refuse to run inside a poll,
  attributing the caller via an unbroken `#[track_caller]` chain. `Mutex`/`RwLock` locks,
  `spawn_blocking`, and wake operations are deliberately NOT intercepted (short locks in async are
  legal; long waits are level 2's job).
1. **Budget** — wall/CPU timing of each poll on the REAL clock (`std::time`, never the virtualized
  platform clock); thread CPU comes from a cached snapshot, so a poll can include a little pre-poll
  CPU. A lenient blanket budget applies everywhere (`KITHARA_NO_BLOCK_BUDGET_MS` overrides) and the
  strict tier is opt-in via `#[kithara::no_block]`. Reports classify CPU-spin vs blocked-wait from
  the thread-CPU/wall ratio: in panic mode the blanket tier panics ONLY on CPU-spin, still emitting
  census lines for the rest, while the strict tier panics on every over-budget class.

Modes via `KITHARA_NO_BLOCK`: `panic` (default), `census`, `off`. A census observation reaches one
sink: `KITHARA_NO_BLOCK_LOG=<file>` when it names one, `tracing` otherwise. Never both — a profile
storing the output of passing tests would carry the whole census into its JUnit as well. A
configured census sink is evidence: an open or write failure panics the attempt instead of silently
dropping a record.

Escape: `#[kithara::allow_block]` suppresses level 1 AND pauses the level-2 timer. Its guards are
`!Send`: they mutate thread-local state in `Drop` and must not cross `.await`.

Flash interop runs one way — flash calls `no_block`, never the reverse. A BRIDGED wait (a wrapped
sync wait inside an async poll) is reported BEFORE the engine wait, while the engine's own wait
runs under a permit, so engine coordination neither trips level 1 nor counts against the budget.

`just test rtsan-async` compiles every watched poll as an RTSan nonblocking context (nightly,
`--cfg rtsan`); suppressions live in `.config/rtsan/async-suppressions.txt`. `arch.no-raw-no-block`
bans hand-calling the expansion targets outside their owner files.

## CancelToken

`CancelToken` is the **single** cancellation token type across the workspace — no crate depends on
an async-runtime cancel crate (`tokio_util`). It is built on a private **propagate-down `Node`
tree** (`common/cancel/node.rs`): a flag, a cold list of `Weak` children, and a waker registry. A
node holds its parent `Arc` without ever reading it — the walk is always *down* — purely to keep
an intermediate node alive for its `Weak` children. `is_cancelled()` is a single `Acquire` load,
lock-free and wait-free, hence RT-safe on the audio produce-core.

The tree uses `std::sync` only (no `parking_lot`, no flash variant), so it compiles unchanged on
wasm32. Every guarded section is a linear structural edit and wakers fire outside the lock, so a
poisoned lock is recovered rather than propagated.

- `cancel()` is idempotent (a repeat neither re-drains nor re-recurses): it swaps the flag, drains
  this node's wakers, then recurses **down** through live `Weak` children. A master cancel reaches
  every descendant by *writing* their flags, and the flag store happens before any waker fires, so
  a thread observing a wake is guaranteed to see the flag.
- `child()` derives a **new** node under the parent's children-lock, so a concurrent `cancel()`
  either includes it in the snapshot or has already set the parent flag — in which case the child
  is born cancelled and a future or waker on it never parks. Cancelling a child or sibling never
  marks the parent. `Clone` keeps the **same** node: cancelling a clone is observed by the
  original.
- `cancelled()` is cancel-safe in `tokio::select!` (dropping it unregisters its slot).
  `on_cancel()` registers a synchronous waker — the counterpart for a thread parked on a
  flash-aware `Condvar`/`Notify` — and its guard unregisters on drop. The waker runs on the
  cancelling thread: keep it cheap, non-blocking, idempotent. A registration arriving after the
  drain fires immediately (the `fired` latch).

### Roots, scopes, groups

`CancelToken::root()` (an owning master a consumer-crate top `cancel()`s on teardown) and
`CancelToken::never()` (a sentinel where a token is structurally required but no cancellation
source exists) mint a **fresh subtree root** instead of deriving from a parent, and are restricted
to owner/sentinel sites — see `docs/guides/cancel-policy.md`, enforced by `just lint arch`
(`cancel_root_sites`). Everywhere else, derive with `.child()` or take a `CancelToken` from your
caller. Dropping a root is passive: it does **not** cancel its subtree.

`CancelScope::new(Option<CancelToken>)` is the seam between composed and standalone subsystems and
the canonical replacement for a `cancel.unwrap_or_default()` fallback: `Some(parent)` makes the
scope's token a child, so a master cancel reaches it, while `None` mints a fresh root. `token()`
vends clones of that one node, and `Drop` is passive — a composed scope never cancels a token
handed from above.

`CancelGroup` is a read-only OR-combinator over several tokens, composable with `|`:
`is_cancelled()` is true once **any** source is cancelled, `cancelled()` parks a slot per source
(dropping it unregisters every slot), and equality is source-array identity (`Arc::ptr_eq`).
`on_cancel()` registers one shared once-gated wake across all sources. An empty group never
resolves.
