# Flash-Time Blocking/Wait Primitive Coverage

Every primitive that blocks or waits MUST route through `kithara_platform` so the
single virtual clock (`feature = "flash-time"`) can govern it when a flash region
is active. A raw primitive either races the clock (runs uncounted) or
freezes/crawls it (pins quiescence). This is the canonical checklist; an ast-grep
rule enforces each so deviations are caught mechanically, not by stumbling on a
hung test.

Flash is **opt-in per region** and **REAL by default** (see the README "Annotation
model"). Two per-thread gates decide virtual-vs-real:

- `flash_enabled()` (reads `FLASH_ACTIVE`) gates the **stateless** time primitives
  (`time::sleep`/`timeout`, `Instant::now`, `thread::park_timeout`/`sleep`).
- `flash_ambient()` (reads `FLASH_AMBIENT`, the per-test gate) gates the
  **stateful** sync primitives (`Condvar`/`Notify`/`mpsc`/`oneshot`) and
  `task::yield_now`, because a stateful primitive's wait and its signal run on
  different threads and must agree on the clock (a per-callstack gate could
  mismatch and hang).

Status legend: ✅ flash-accounted (virtual when its gate is on, real otherwise)
· 🔁 routed-but-passthrough (sync, no time wait) · ❌ not wrapped (raw).

## std

| Primitive | Wrapper | Flash status | ast-grep rule |
|---|---|---|---|
| `thread::sleep` | `kithara_platform::thread::sleep` | ✅ branches on `flash_enabled()` | `arch.no-direct-thread-wait` |
| `thread::park_timeout` | `kithara_platform::thread::park_timeout` | ✅ `sched::park_timed_unparkable` when `flash_enabled()` | `arch.no-direct-thread-wait` |
| `thread::park` (untimed) | `kithara_platform::thread::park` | 🔁 raw (not on a wait path) | — (TODO if used) |
| `thread::yield_now` | `kithara_platform::thread::yield_now` | ✅ `sched::yield_until_advance` | — (TODO rule) |
| `thread::unpark` | `kithara_platform::thread::unpark` | ✅ `sched::unpark` | — |
| `sync::Condvar` | `kithara_platform::sync::Condvar` | ✅ `register_condvar_*` + `signal_condvar` when `flash_ambient()` | — |
| `sync::Mutex` / `RwLock` | `kithara_platform::{Mutex,RwLock}` | 🔁 parking_lot (sync lock, no time wait) | `arch.no-std-sync-mutex` |
| `sync::mpsc` (recv/recv_timeout) | `kithara_platform::sync::mpsc` | 🔁 sync recv (TODO: timed recv flash-account) | — (TODO rule) |
| `sync::Barrier` / `Once` | — | ❌ unused; forbid if introduced | — (TODO rule) |

## tokio

| Primitive | Wrapper | Flash status | ast-grep rule |
|---|---|---|---|
| `time::sleep` | `kithara_platform::time::sleep` | ✅ virtual when `flash_enabled()`, real otherwise | `arch.no-direct-time` |
| `time::timeout` | `kithara_platform::time::timeout` | ✅ races the (virtual-or-real) sleep, branching on `flash_enabled()` | `arch.no-direct-time` |
| `time::sleep_until` | — | ❌ glob passthrough (unused) | `arch.no-direct-time` (TODO extend) |
| `time::interval` / `interval_at` | — | ❌ glob passthrough (unused) | `arch.no-direct-time` (TODO extend) |
| `task::spawn` | `kithara_platform::tokio::task::spawn` | ✅ `participate` poll-wrapper + per-poll ambient re-assert | `arch.no-raw-tokio-task` |
| `task::spawn_blocking` | `kithara_platform::task::spawn_blocking` | ✅ `reset_credit()` / `on_participant_exit()` credit bracket | `arch.no-raw-tokio-task` |
| `task::yield_now` | `kithara_platform::tokio::task::yield_now` | ✅ quiescence yield-waiter when `flash_ambient()`, plain scheduler yield otherwise | `arch.no-raw-tokio-task` |
| `Handle::spawn` (specific runtime) | wrap future in `time::participate` | ✅ (manual) | — (method call; manual review) |
| `sync::Notify` | `kithara_platform::sync::Notify` | ✅ `register_notify_async` + `signal_notify` when `flash_ambient()` | `arch.no-raw-tokio-sync` |
| `sync::mpsc` | `kithara_platform::tokio::sync::mpsc` | ✅ `register_channel_async` + `signal_channel` when `flash_ambient()` | `arch.no-raw-tokio-sync` |
| `sync::oneshot` | `kithara_platform::tokio::sync::oneshot` | ✅ engine-backed when `flash_ambient()` | `arch.no-raw-tokio-sync` |
| `sync::Semaphore` | — | ❌ raw (`queue/loader.rs`); TODO wrap | `arch.no-raw-tokio-sync` |
| `sync::{Mutex,RwLock,watch,broadcast,Barrier}` | — | ❌ glob passthrough (mostly unused); forbid | `arch.no-raw-tokio-sync` |

## Participant accounting (the spawn credit bracket)

There is NO io-pacing and NO `io_guard` / `io_pending` / `IO_PACE` / `GuardedStream`
machinery — it was deleted (campaign step A2), and a deny rule
(`arch.no-flash-time-hacks`) forbids its return. A thread/task earns its
quiescence credit **lazily** on its first wrapped wait; the only bracket is at the
spawn boundary: `thread::spawn_named` and the platform `task::spawn_blocking` call
`reset_credit()` on entry (a reused pool thread must not inherit a stale credit)
and `on_participant_exit()` after the closure returns (a thread that woke to
`Running` drops its `active` slot). This is the credit bracket the
`spawn_blocking` row above refers to — NOT an I/O guard.

## The quiescence engine (the "advance" rule)

`active` (sync threads) + `active_async` (async tasks) must both reach 0 before
the clock advances. The async count is per-task via the spawn poll-wrapper
(`participate`): a task is counted from RUNNABLE (woken/queued) through RUNNING,
released only on PARK / DONE / DROP. This closes the wake→poll window (a woken
task is counted before it is re-polled, so the clock cannot jump past it).

## Real I/O

Socket/reqwest byte transfer is plain real I/O — there is no flash bridge for it
(the io-pacing that used to wrap it was deleted in A2). Real-socket suites run
under `#[kithara::test(flash(false))]`, which keeps the whole graph on the real
clock (identical to production), so the network's wall-clock timing is never
virtualized.
