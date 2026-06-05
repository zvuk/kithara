# Sim-Time Blocking/Wait Primitive Coverage

Every primitive that blocks or waits MUST route through `kithara_platform` so the
single virtual clock (`feature = "sim-time"`) governs it. A raw primitive either
races the clock (runs uncounted) or freezes/crawls it (pins quiescence). This is
the canonical checklist; an ast-grep rule enforces each so deviations are caught
mechanically, not by stumbling on a hung test.

Status legend: ✅ sim-accounted · 🔁 routed-but-passthrough (sync, no time wait)
· ❌ not wrapped (raw) · 🛡️ real-by-design (wall-clock safety net).

## std

| Primitive | Wrapper | Sim status | ast-grep rule |
|---|---|---|---|
| `thread::sleep` | `kithara_platform::thread::sleep` | ✅ (virtual under sim) | `arch.no-direct-thread-wait` |
| `thread::park_timeout` | `kithara_platform::thread::park_timeout` | ✅ `sched::park_timed_unparkable` | `arch.no-direct-thread-wait` |
| `thread::park` (untimed) | `kithara_platform::thread::park` | 🔁 raw (not on a wait path) | — (TODO if used) |
| `thread::yield_now` | `kithara_platform::thread::yield_now` | ✅ `sched::yield_until_advance` | — (TODO rule) |
| `thread::unpark` | `kithara_platform::thread::unpark` | ✅ `sched::unpark` | — |
| `sync::Condvar` | `kithara_platform::sync::Condvar` | ✅ `register_condvar_*` + `signal_condvar` | — |
| `sync::Mutex` / `RwLock` | `kithara_platform::{Mutex,RwLock}` | 🔁 parking_lot (sync lock, no time wait) | `arch.no-std-sync-mutex` |
| `sync::mpsc` (recv/recv_timeout) | `kithara_platform::sync::mpsc` | 🔁 sync recv (TODO: timed recv sim-account) | — (TODO rule) |
| `sync::Barrier` / `Once` | — | ❌ unused; forbid if introduced | — (TODO rule) |

## tokio

| Primitive | Wrapper | Sim status | ast-grep rule |
|---|---|---|---|
| `time::sleep` | `kithara_platform::time::sleep` | ✅ `SimSleep` | `arch.no-direct-time` |
| `time::timeout` | `kithara_platform::time::timeout` | ✅ races `SimSleep` | `arch.no-direct-time` |
| `time::real_timeout` | `kithara_platform::time::real_timeout` | 🛡️ ALWAYS real (test watchdog) | — |
| `time::sleep_until` | — | ❌ glob passthrough (unused) | `arch.no-direct-time` (TODO extend) |
| `time::interval` / `interval_at` | — | ❌ glob passthrough (unused) | `arch.no-direct-time` (TODO extend) |
| `task::spawn` | `kithara_platform::tokio::task::spawn` | ✅ `participate` poll-wrapper (gate) | `arch.no-raw-tokio-task` |
| `task::spawn_blocking` | `kithara_platform::tokio::task::spawn_blocking` | ✅ `io_guard` bracket | `arch.no-raw-tokio-task` |
| `task::yield_now` | `kithara_platform::tokio::task::yield_now` | ✅ `SimYield` (parks until advance) | `arch.no-raw-tokio-task` |
| `Handle::spawn` (specific runtime) | wrap future in `time::participate` | ✅ (manual) | — (method call; manual review) |
| `sync::Notify` | `kithara_platform::sync::Notify` | ✅ `register_notify_async` + `signal_notify` | `arch.no-raw-tokio-sync` |
| `sync::mpsc` | `kithara_platform::tokio::sync::mpsc` | ✅ `register_channel_async` + `signal_channel` | `arch.no-raw-tokio-sync` |
| `sync::oneshot` | `kithara_platform::tokio::sync::oneshot` | ✅ engine-backed | `arch.no-raw-tokio-sync` |
| `sync::Semaphore` | — | ❌ raw (`queue/loader.rs`); TODO wrap | `arch.no-raw-tokio-sync` |
| `sync::{Mutex,RwLock,watch,broadcast,Barrier}` | — | ❌ glob passthrough (mostly unused); forbid | `arch.no-raw-tokio-sync` |

## Real I/O bridge

| Mechanism | Wrapper | Sim status |
|---|---|---|
| reqwest/socket byte transfer | `kithara_platform::time::io_guard()` (`GuardedStream`) | ⚠️ paces the clock for the guard's lifetime — see the open issue below |

## The quiescence engine (the "advance" rule)

`active` (sync threads) + `active_async` (async tasks) must both reach 0 before
the clock advances. The async count is per-task via the spawn gate
(`TaskGate`): a task is counted from RUNNABLE (woken/queued) through RUNNING,
released only on PARK / DONE / DROP. This closes the wake→poll window (a woken
task is counted before it is re-polled, so the clock cannot jump past it).

## Open issue — io_guard pacing vs. a server-side virtual throttle

The HLS seek test still crawls under sim. Root: `io_guard`/`io_pending` paces the
virtual clock (1 ms real per advance) for the **whole** fetch lifetime, including
the server's virtual `throttle` sleep — during which **no bytes flow**, so the
clock should COLLAPSE, not pace. This is the io-bridge design (task #27: replace
the blanket IN_FLIGHT pacing with an io-wait release guard that only paces while
real bytes are actually moving). It is NOT a missing primitive wrapper.
