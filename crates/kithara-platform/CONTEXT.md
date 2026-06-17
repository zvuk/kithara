# kithara-platform — Context

Detailed contracts and invariants for the kithara-platform crate; the README is the overview.

## Synchronization Backends

<table>
<tr><th>Type</th><th>Native</th><th>wasm32</th></tr>
<tr><td><code>Mutex&lt;T&gt;</code></td><td><code>parking_lot::Mutex</code> wrapper</td><td><code>wasm_safe_thread::Mutex</code> wrapper</td></tr>
<tr><td><code>RwLock&lt;T&gt;</code></td><td><code>parking_lot::RwLock</code> wrapper</td><td><code>wasm_safe_thread::rwlock::RwLock</code> wrapper</td></tr>
<tr><td><code>Condvar</code></td><td><code>parking_lot::Condvar</code> wrapper</td><td><code>wasm_safe_thread::condvar::Condvar</code> wrapper</td></tr>
<tr><td><code>MaybeSend</code></td><td>Equivalent to <code>Send</code></td><td>Blanket trait (no Send requirement)</td></tr>
<tr><td><code>MaybeSync</code></td><td>Equivalent to <code>Sync</code></td><td>Blanket trait (no Sync requirement)</td></tr>
</table>

## Thread and Task Primitives

<table>
<tr><th>API</th><th>Native</th><th>wasm32</th></tr>
<tr><td><code>thread::spawn</code></td><td><code>std::thread::spawn</code></td><td><code>wasm_safe_thread::Builder::spawn</code> (Web Worker)</td></tr>
<tr><td><code>tokio::task::spawn</code></td><td><code>tokio_with_wasm</code> alias runtime</td><td>Worker-aware wrapper with lifecycle hooks</td></tr>
<tr><td><code>tokio::task::spawn_blocking</code></td><td><code>tokio_with_wasm</code> blocking pool</td><td>Dedicated worker-thread execution</td></tr>
<tr><td><code>thread::is_main_thread / is_worker_thread</code></td><td>always main / false</td><td>detects browser main vs Worker global scope</td></tr>
<tr><td><code>thread::assert_main_thread / assert_not_main_thread</code></td><td>no-op</td><td>panic on wrong thread affinity</td></tr>
</table>

## Time Utilities

- `time::sleep(duration)`
- `time::timeout(duration, future)`
- `time::Instant` (via `web-time`, or the virtual clock under `flash`)

On native these delegate to `tokio` runtime primitives, on wasm they use `setTimeout`-based scheduling. `web_time` is an internal implementation detail of this crate: no other crate may depend on it directly (enforced by the `arch.no-web-time` ast-grep gate), and `std::time::{Instant, Duration}` is likewise banned outside this crate (`arch.no-std-time`). Routing every timestamp through `time::Instant` is what lets the clock be swapped wholesale, below.

### Virtual time (`flash`)

`flash` is an off-by-default cargo feature, native and test-only, that replaces the wall clock with a process-global virtual timeline so warm-cache offline playback tests run at CPU speed instead of real time. No shipping crate enables it; production builds are unchanged. Off the feature every flash macro is a no-op (identity) and everything is real.

#### Annotation model (REAL by default, virtual only where turned on)

Flash is **opt-in per region**, not a global mode flip. The two flags of the per-thread `Mode` (the `ambient` / `active` fields of the single `ThreadCtx` thread-local in `flash/ctx.rs`) decide whether a primitive sees virtual or real time; **both default to false ⇒ REAL by default**, virtual only where a region turns it on.

- The `ambient` flag (the per-test gate) is set by `#[kithara::test(flash(bool))]` and **propagated across `spawn` / `spawn_named` / `spawn_blocking`** so every thread/task of a flash test shares one ambient mode. `flash_ambient()` reads it.
- The `active` flag (the dynamic gate) is pushed by a production `#[kithara::flash(bool)]` region as it runs, and is itself **gated by ambient** (a dynamic flash takes effect only inside an ambient flash test). `flash_enabled()` reads it.

The two flags govern disjoint primitive classes, because their correctness requirements differ:

- `flash_enabled()` (the `active` flag) governs the **stateless** time primitives — `time::sleep` / `time::timeout`, `Instant::now`, `thread::park_timeout` / `thread::sleep`. These read or wait on the clock with no cross-thread handshake, so a per-callstack gate is safe.
- `flash_ambient()` (the `ambient` flag) governs the **stateful** sync primitives — `Condvar` / `Notify` / `mpsc` / `oneshot`, each of which latches the gate ONCE at construction (see `Backend` in `flash/ids.rs`) — and `task::yield_now`. A stateful primitive's *wait* and its *signal* run on different threads; if those two sides disagreed on virtual-vs-real (which a per-callstack gate would allow) the wait and the wake would target different engines and the test would hang. The ambient gate is uniform per test and the latch fixes it per primitive, so wait+signal always agree.

The two annotations differ in scope:

- **Production `#[kithara::flash(true|false)]` is DYNAMIC**: it pushes the `active` flag through the annotated region's call stack and across its spawns (e.g. the audio worker `run_loop`, downloader yield loops, the preload gate), so the prod code's own stateless time reads go virtual while the region runs. It is gated by ambient, so off a flash test it is inert.
- **Test `#[kithara::test(flash(true|false))]` is LEXICAL**: it rewrites the test BODY's *direct* `time::*` / `Instant::now` / `thread::park_timeout` calls to their virtual variants (body-only — a prod fn the body calls keeps its stateless time real unless that fn is itself `#[flash]`), and sets the ambient gate for the whole test graph. Default is `flash(true)` under the feature; real-socket suites are `flash(false)` (the whole graph stays real, identical to production).

The lexical rewriter keys on the **last two path segments** of a call, so a flash test body MUST use a QUALIFIED time path (`time::sleep`, not a bare-imported `sleep`); a bare single-segment import is intentionally not rewritten and would stay real (a mixed-clock hazard) — see the rewriter doc comment in `kithara-test-macros`.

#### Quiescence engine

The clock is **quiescence-driven**, not additive: an engine's `Clock` (one per
`FlashInner`; the process-wide `FLASH` instance backs the wrapped primitives,
read lock-free by `Instant::now`) advances only when every participating root is
parked, and only ever jumps to the **earliest** registered deadline. Because the
clock moves to a deterministic minimum over the multiset of pending deadlines,
the sequence of clock values is a pure function of those deadlines — independent
of thread scheduling — so a sim run is deterministic and collapses every timed
wait to zero real time.

Contract:

- `time::Instant` becomes a drop-in struct backed by the engine `Clock`. Same surface
  (`now`, `elapsed`, `duration_since`, `saturating_duration_since`, `+`/`-`,
  ordering); all arithmetic saturates.
- Participant accounting is **intrinsic to the platform-wrapped primitives** —
  there is NO registration API and no consumer ever registers anything. A thread
  earns its quiescence credit *lazily*, on its first wrapped wait, tracked in a
  per-thread `Credit` (`None` → `Running` → `Parked`):
  - A thread that has never entered a wrapped wait is `None`: invisible to the
    engine. It owns no deadline and is not counted in `active`, so a busy-spin
    thread cannot stall the clock.
  - The FIRST wrapped wait *bootstraps*: it moves the thread to `Parked` WITHOUT
    decrementing `active` (the thread was running uncounted; there was nothing to
    remove). When the engine wakes that waiter it does `active += 1` and the
    thread becomes `Running`, balancing the bootstrap.
  - A subsequent wait on a `Running` thread decrements `active` (`Running` →
    `Parked`); its wake re-increments (`Parked` → `Running`). A thread that exits
    while `Running` drops its `active` slot via the spawn bracket.
- The spawn bracket is where the exit decrement happens — again with no consumer
  call. `thread::spawn_named` (the named-thread bracket) and the platform
  `task::spawn_blocking` both reset the credit on entry (a reused pool thread
  must not inherit a stale credit) and run the exit decrement after the closure
  returns. A consumer that runs a wrapped wait on a blocking pool thread spawns
  through `task::spawn_blocking` instead of `tokio::task::spawn_blocking` — that
  is "use the platform primitive", not "inject accounting".
- A wrapped wait, in ONE critical section under the engine lock: register its
  deadline (or, for an untimed condvar wait, an entry with no deadline), account
  the wait (bootstrap, or `active -= 1` if already `Running`), then evaluate the
  advance rule. On wake the firer does `active += 1`, removes the entry, and the
  woken thread marks itself `Running`. Register-deadline and accounting are
  atomic (single lock hold), closing the wake-to-re-register race. The engine
  never calls back into a domain lock (lock order is always domain → engine).
- Virtualized waits:
  - `thread::park_timeout(d)` registers an **unparkable** timed waiter and
    blocks off-lock until the clock crosses `now + d` OR a peer
    `thread::unpark(&t)` wakes it. `unpark` routes through the engine (it does
    not touch the OS park slot the engine cannot observe); an unpark that
    arrives before the park is remembered so the next park returns at once.
  - `thread::paced_backoff(d)` is the backoff for a **synchronous poll loop**
    whose data is produced by another engine-visible thread (e.g. the analysis
    decode loop polling the audio worker's ring). Off the sim path it is a real
    `sleep(d)` throttle. Under `flash` it relinquishes the engine as a
    **deadline-less** cooperative yield (`yield_until_advance`) — unlike a plain
    `sleep`, it registers no virtual `Timed` deadline of its own, so it cannot
    inflate the clock on its own; it re-polls in lockstep with the producer's
    clock advances, which are paced by the producer's real I/O. Use it instead
    of `sleep` whenever a sync poll loop waits on data delivered by a separate
    flash-visible producer.
  - `sync::Condvar` waits (`wait_timeout`, `wait`) register a
    condvar-group waiter keyed by a per-condvar id; `notify_all` / `notify_one`
    signal that group through the engine. The waiter registers its engine entry
    **before** releasing the domain guard, so a predicate change + notify (which
    must re-take the domain lock) is serialized after the entry — no lost wake.
    The caller re-checks its predicate after re-acquiring (storage already
    loops). `thread::sleep` and the async `time::{sleep, timeout}` are routed:
    they branch on `flash_enabled()` — virtual inside an active flash region,
    real otherwise — like the other stateless time primitives.
  - `tokio::task::yield_now()` is a cooperative async yield. Like the stateful
    sync primitives it keys on `flash_ambient` (the per-test ambient gate),
    consulted per call — a yield has no cross-thread signal partner, so no
    construction latch is needed — and NOT on `flash_enabled`: engine-backed (a
    quiescence yield-waiter) only inside an
    ambient flash test, and a plain scheduler yield otherwise. A yield's grant
    comes from an engine clock advance, which requires `active_async == 0`; in a
    non-ambient (`flash(false)` / production) task the surrounding work keeps its
    `active_async` slot across the yield while its other primitives are real, so
    an engine-backed yield could never be granted — a circular wedge. Gating on
    ambient keeps the build behavior-transparent for non-ambient callers.

    The advance rule carries a sharper invariant than "jump to the earliest
    deadline once all roots are parked": **the clock must not advance while
    anything is runnable at the current instant, and a pending yield-waiter IS
    runnable-now.** So when every other parked waiter is an event-driven `Thread`
    park (a `park_timeout` watchdog, no real timer), the engine drains the
    yield-waiters at the current instant instead of jumping to a watchdog
    deadline — a producer yielding mid-work (e.g. the audio worker between
    `produce` chunks) gets to unpark a blocked consumer before its watchdog can
    fire a false hang. A real `Timed`/`Condvar` waiter present means a yielder
    may be busy-waiting on that timer, so the clock advances normally (draining
    yielders on the jump); a yielder that cannot make progress re-parks-timed,
    emptying the yield set, so this never livelocks.
- `time::reset()` clears the timeline and the engine. nextest's per-test process
  isolation keeps the global state clean between tests; `reset()` is for runners
  that share a process.

#### Real I/O pacing

The engine cannot observe real-world transit: a task awaiting a real socket
parks and releases its quiescence slot, so without correction the clock would
jump to the next virtual deadline while bytes are still on the wire — firing
virtual watchdogs (hang detector budgets, net idle timers) spuriously ahead of
an in-flight loopback fetch. The corrective invariant extends the pooled-participant
one (`PoolParticipant` in `flash/system/credit.rs`: an ambient `spawn_blocking`
closure is real work the clock must not advance past): **virtual time must never
outrun real time while real I/O is in flight.**

- `time::real_io()` returns a `RealIoScope` RAII bracket for ONE real I/O
  operation. `kithara-net` holds it across its socket awaits (request
  establish, full-body reads, and each `Pending` window of a streaming body
  chunk via a stream adapter).
- While at least one scope is live the clock is **paced, not pinned**: the
  first scope anchors `(real instant, virtual nanos)`, and the advance rule
  refuses any jump beyond `anchor_virtual + real_elapsed`. A real-time pacer
  thread (armed only while `real_io > 0`, invisible to the engine) re-runs the
  advance rule every millisecond so paced deadlines still fire once the
  equivalent REAL time has passed.
- Pace, not pin, is what keeps a **virtually-delayed peer** live: a fixture
  server sleeping on a virtual delay before responding still makes progress at
  1:1 real pace while the client awaits the response. A hard pin would freeze
  the very deadline the peer is sleeping on — a deadlock. Conversely a virtual
  watchdog racing a healthy loaded fetch now measures at least the equivalent
  real time, so it fires only on a genuine stall.
- When the last scope drops, the anchor clears and full-speed collapse resumes
  immediately. Off `flash` (and on wasm) the scope is a ZST no-op.

The intended workflow is two runs compared as ground truth: the default
real-time run (catches concurrency/timing bugs) and the `flash` run (fast).
Divergence in sample-count positions or PCM between them flags that
virtualization distorted something. The FILE phase-continuity cluster is the
first equivalence oracle: its sub-0.5-sample phase assertions fail on any
divergence.

## CancelToken

`CancelToken` is the **single** cancellation token type across the workspace — no
crate depends on an async-runtime cancel crate (`tokio_util`); they use this one. It
is a real-time-safe token built on a private **propagate-down `Node` tree**
(`common/cancel/node.rs`): each node holds one `AtomicBool` flag plus a cold list of
`Weak` children (and a held-only `_parent` `Arc` that keeps the ancestor chain alive
for a descendant). The hot read path — `is_cancelled()` — is a single `Acquire` load,
lock-free and wait-free, so it is RT-safe on the audio produce-core (a `tokio_util`
token takes a `Mutex<Inner>` on `is_cancelled()`, which traps under RealtimeSanitizer).

- `cancel()` swaps the node's flag to `true` (`AcqRel`, so a repeat cancel is
  idempotent — neither re-drains nor re-recurses), drains the node's own wakers, then
  recurses **down** through live `Weak` children. A master cancel reaches every
  descendant by *writing* their flags; the `Release` store happens before any waker
  fires (paired with the `Acquire` load in `is_cancelled`), so a thread observing a
  wake is guaranteed to see the flag.
- `child()` derives a **new** node linked under the parent (born cancelled if the
  parent is already cancelled, so a future/waker on it never parks). Cancelling a child
  or sibling never marks the parent — the correct hierarchy. `Clone` keeps the **same**
  node (same identity): cancelling a clone is observed by the original.
- `cancelled()` returns a borrowed `Cancelled<'_>` future that resolves on cancel and
  is cancel-safe in `tokio::select!` (dropping it unregisters its slot). `on_cancel()`
  registers a synchronous waker — the counterpart for a thread parked on a flash-aware
  `Condvar`/`Notify` — and returns a guard that unregisters on drop.

### Roots, scopes, and the `cancel_root_sites` guard

Two constructors mint a **fresh subtree root** instead of deriving from a parent:

- `CancelToken::root()` — the owning master a consumer-crate top (`App`, FFI player,
  `PlayerImpl` fallback) holds and `cancel()`s on teardown. `Drop` is passive: dropping
  the root does **not** cancel its subtree.
- `CancelToken::never()` — a sentinel for where a token is structurally required but no
  cancellation source exists.

Both root a fresh hierarchy, so they are restricted to owner/sentinel sites, enforced by
`cargo xtask lint arch` (`cancel_root_sites`) via a per-file allowlist in
`.config/arch/thresholds.toml`. Everywhere else, derive a child with `.child()` or take a
`CancelToken` from your caller.

`CancelScope::new(Option<CancelToken>)` is the canonical replacement for the legacy
`cancel.unwrap_or_default()` fallback and the seam between composed and standalone
subsystems: `Some(parent)` makes the scope's token a child (a master cancel reaches it),
`None` mints a fresh root. `token()` vends children from that one node, `cancel()` cancels
exactly that subtree, and `Drop` is passive (a composed scope never cancels a token handed
from above).

## Trait Bridges

- `CancelToken` → `CancelGroup` (`From`) — single-token group
- `Vec<CancelToken>` → `CancelGroup` (`From`) — multi-token group. `CancelGroup` is a
  read-only OR-combinator (`|` composes): `is_cancelled()` is true once **any** source is
  cancelled; equality is source-array identity.
- `NotAvailable` / `TimeoutError` / `JoinError` / `TryCurrentError` (`Display`) — human-readable error rendering
