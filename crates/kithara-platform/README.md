<div align="center">
  <img src="../../logo.svg" alt="kithara" width="300">
</div>

<div align="center">

[![crates.io](https://img.shields.io/crates/v/kithara-platform.svg)](https://crates.io/crates/kithara-platform)
[![docs.rs](https://docs.rs/kithara-platform/badge.svg)](https://docs.rs/kithara-platform)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](../../LICENSE-MIT)

</div>

# kithara-platform

Platform-aware runtime primitives for native and `wasm32` targets. This crate is the portability boundary for synchronization, thread/task spawning, timing helpers, and conditional trait bounds used across the workspace.

## Usage

```rust
use kithara_platform::{Mutex, time};

let lock = Mutex::new(42_u32);
{
    let mut guard = lock.lock_sync();
    *guard += 1;
}

time::sleep(std::time::Duration::from_millis(10)).await;
```

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
- `time::Instant` (via `web-time`, or the virtual clock under `sim-time`)

On native these delegate to `tokio` runtime primitives, on wasm they use `setTimeout`-based scheduling. `web_time` is an internal implementation detail of this crate: no other crate may depend on it directly (enforced by the `arch.no-web-time` ast-grep gate), and `std::time::{Instant, Duration}` is likewise banned outside this crate (`arch.no-std-time`). Routing every timestamp through `time::Instant` is what lets the clock be swapped wholesale, below.

### Virtual time (`sim-time`)

`sim-time` is an off-by-default cargo feature, native and test-only, that replaces the wall clock with a process-global virtual timeline so warm-cache offline playback tests run at CPU speed instead of real time. No shipping crate enables it; production builds are unchanged.

The clock is **quiescence-driven**, not additive: a single global `SIM_NANOS`
(read lock-free by `Instant::now`) advances only when every participating root is
parked, and only ever jumps to the **earliest** registered deadline. Because the
clock moves to a deterministic minimum over the multiset of pending deadlines,
the sequence of clock values is a pure function of those deadlines — independent
of thread scheduling — so a sim run is deterministic and collapses every timed
wait to zero real time.

Contract:

- `time::Instant` becomes a drop-in struct backed by `SIM_NANOS`. Same surface
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
  - `sync::Condvar` waits (`wait_sync_timeout`, `wait_sync`) register a
    condvar-group waiter keyed by a per-condvar id; `notify_all` / `notify_one`
    signal that group through the engine. The waiter registers its engine entry
    **before** releasing the domain guard, so a predicate change + notify (which
    must re-take the domain lock) is serialized after the entry — no lost wake.
    The caller re-checks its predicate after re-acquiring (storage already
    loops). `thread::sleep` and the async `time::{sleep, timeout}` stay real for
    now (later increments route them).
- `time::reset()` clears the timeline and the engine. nextest's per-test process
  isolation keeps the global state clean between tests; `reset()` is for runners
  that share a process.

The intended workflow is two runs compared as ground truth: the default
real-time run (catches concurrency/timing bugs) and the `sim-time` run (fast).
Divergence in sample-count positions or PCM between them flags that
virtualization distorted something. The FILE phase-continuity cluster is the
first equivalence oracle: its sub-0.5-sample phase assertions fail on any
divergence.

## CancellationToken

`CancellationToken` is the **single** cancellation token type across the workspace —
crates never depend on `tokio_util` directly; they use this one. It is a real-time-safe
token: a `tokio_util::CancellationToken` (async `cancelled()` side + the wider
propagation tree) paired with a **chain of per-node cancel flags**. `tokio_util`'s own
`is_cancelled()` takes the `TreeNode` `Mutex<Inner>` (no atomic fast path), which traps
under RealtimeSanitizer on the audio produce-core. `CancellationToken::is_cancelled()`
walks `self → root` loading each node's flag (`Acquire`) — lock-free, wait-free, bounded
by tree depth (master → consumer is 2–3 nodes), RT-safe.

- `cancel()` `Release`-stores **this node's** flag **then** calls `inner.cancel()`, so a
  thread observing the inner async cancellation (or any later `Acquire` walk) is
  guaranteed to see the flag set; the flag can never lag the inner token.
- `child_token()` creates a **new** flag node linked to the parent's chain. A
  parent/master `cancel()` is observed by every descendant's lock-free `is_cancelled()`
  (the descendant's walk reaches the parent's flag), while cancelling a child or sibling
  never marks the parent cancelled — the correct hierarchy. `Clone` keeps the **same**
  node (same identity), like `tokio_util`'s clone.
- `cancelled()` / `cancelled_owned()` delegate to the inner token (async side unchanged),
  so the token drops into `tokio::select!` exactly like a `tokio_util` token.

Consumer-crate masters (`Queue`, `App`, FFI, `PlayerImpl` fallback) mint a root
`CancellationToken` (via `new()` / `default()`) and thread `child_token()`s down the
player → audio-worker and player → HLS-coord chains so a master `cancel()` is seen by the
worker's lock-free read. `new()` / `default()` root a fresh hierarchy and belong only at
marked owner sites (see the `AGENTS.md` cancel-hierarchy contract, enforced by
`cargo xtask lint arch`).

## Trait Bridges

- `CancellationToken` → `CancelGroup` (`From`) — single-token group
- `Vec<CancellationToken>` → `CancelGroup` (`From`) — multi-token group
- `NotAvailable` / `TimeoutError` / `JoinError` / `TryCurrentError` (`Display`) — human-readable error rendering

## Integration

Foundation crate used across the workspace (`kithara-storage`, `kithara-assets`, `kithara-stream`, `kithara-play`, `kithara-wasm`, and test infrastructure) to keep platform-specific branching isolated in one place.
