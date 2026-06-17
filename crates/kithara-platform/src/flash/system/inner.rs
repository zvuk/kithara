//! [`FlashInner`] — the single owner of all engine state — plus its parts:
//! [`Clock`], [`Core`] (= [`Registry`] + [`Scheduler`] data behind ONE lock),
//! the typed engine ids it mints, and the process-wide [`FLASH`] instance.
//! The engine mechanics (advance rule, register/signal surface) live as
//! `FlashInner` methods in the sibling files.

use std::{
    collections::{BTreeMap, BTreeSet},
    panic::Location,
    sync::{
        Arc, LazyLock, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use super::{pace::Pacer, sched::Entry, wake::Wake};
use crate::{common::time::Instant as RealInstant, flash::ids::ThreadKey, native::sync::Mutex};

/// Engine condvar id: one per stateful-primitive latch (Condvar/Notify/channel
/// halves). The inner field is private to `system/` — only the engine's
/// [`Registry`] mints these (`fresh_cv`), so nothing outside the engine can
/// fabricate a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::flash) struct CvId(pub(super) u64);

/// Engine waiter id: one per parked entry. The inner field is private to
/// `system/` — only the engine's [`Registry`] mints these (`fresh_id`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::flash) struct WaiterId(pub(super) u64);

/// The engine's timeline: the virtual clock plus the real-clock anchor for the
/// REAL arm. Engine writes happen ONLY inside a `core` critical section
/// (`Core::try_advance` is the sole engine writer); the lock-free reads are
/// the control-surface `Instant` arms.
pub(in crate::flash) struct Clock {
    /// Virtual timeline in nanoseconds. Only moves forward, via the quiescence
    /// engine (and the test-only additive [`Clock::advance`]); starts at
    /// [`Instant::BASE_NANOS`](crate::flash::Instant).
    nanos: AtomicU64,
    /// Anchor for the real monotonic clock, sampled once on first use. Real
    /// instants are reported as `BASE_NANOS + elapsed-since-anchor`, so a
    /// thread outside a flash scope sees a forward-moving clock in the same
    /// nanos space as the virtual one (the two are never compared across the
    /// boundary — a watchdog samples both its start and its checks in the
    /// same mode). NOT cleared by [`FlashInner::reset`], exactly like the old
    /// function-local `REAL_EPOCH`.
    real_epoch: OnceLock<web_time::Instant>,
}

impl Clock {
    const fn new() -> Self {
        Self {
            nanos: AtomicU64::new(crate::flash::Instant::BASE_NANOS),
            real_epoch: OnceLock::new(),
        }
    }

    /// Current virtual instant in nanoseconds.
    pub(in crate::flash) fn now_nanos(&self) -> u64 {
        self.nanos.load(Ordering::Acquire)
    }

    /// Engine-side store. Called only by `Core::try_advance` (the sole engine
    /// writer), under the `core` lock.
    pub(super) fn store(&self, nanos: u64) {
        self.nanos.store(nanos, Ordering::Release);
    }

    /// Reset the timeline to its base (the harness `reset()` path).
    fn reset(&self) {
        self.nanos
            .store(crate::flash::Instant::BASE_NANOS, Ordering::Release);
    }

    /// REAL arm of `Instant::now`: `BASE_NANOS + elapsed` since the lazily
    /// sampled anchor — a u64 in the same nanos space as the virtual clock,
    /// not a `web_time` composition.
    pub(in crate::flash) fn real_now_nanos(&self) -> u64 {
        let epoch = self.real_epoch.get_or_init(web_time::Instant::now);
        let elapsed = u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
        crate::flash::Instant::BASE_NANOS.saturating_add(elapsed)
    }

    /// Manually advance the virtual clock. Additive and test-only: the
    /// production clock is driven solely by the quiescence engine, so the
    /// engine is the single clock writer. The arithmetic clock tests use this
    /// as a manual bump to exercise `Instant` arithmetic without the engine.
    #[cfg(test)]
    pub(in crate::flash) fn advance(&self, delta_nanos: u64) {
        self.nanos.fetch_add(delta_nanos, Ordering::Release);
    }
}

/// Participant bookkeeping: the two quiescence counters plus the id mints.
pub(in crate::flash) struct Registry {
    /// SYNC participants currently RUNNING (OS threads not inside a wrapped
    /// `park_timeout`/`Condvar` wait). Bumped by the firer on wake (real OS
    /// scheduling latency must be covered), decremented at the next wait /
    /// thread exit via the thread-local `Credit` bracket (`credit` module).
    pub(super) active: usize,
    /// ASYNC tasks the engine counts as NON-QUIESCENT. A task is counted from the
    /// moment it becomes RUNNABLE — spawned, or woken (its waker fired and it is
    /// queued to be polled) — until it next PARKS (poll returns `Pending` with no
    /// pending re-wake), completes, or is dropped. Maintained by the spawn
    /// poll-wrapper's per-task gate via `async_acquire`/`release_async`, NOT
    /// the firer. Counting a woken-but-not-yet-repolled task (not merely a
    /// mid-poll one) is what stops the clock jumping past a runnable task — it
    /// closes the wake→poll window. The engine may advance only when BOTH
    /// `active` and `active_async` are zero.
    pub(super) active_async: usize,
    /// Monotonic waiter-id mint — see `Registry::fresh_id`.
    pub(super) next_id: u64,
    /// Monotonic condvar-id mint — see `Registry::fresh_cv`.
    pub(super) next_cv: u64,
    /// Monotonic async-task-id mint (one per [`crate::flash::participate`]).
    pub(super) next_task_id: u64,
    /// Identity of every async task currently holding an `active_async` slot,
    /// keyed by its task id and valued by its spawn site. Inserted on acquire
    /// (`async_acquire` / `gate_wake_parked`), removed on release
    /// (`gate_park` / `gate_complete` / `gate_drop_release`), so the set always
    /// names exactly the tasks the counter counts. Purely diagnostic: the hang
    /// dump lists it so a quiescence pin reports WHICH task pins it (and where
    /// it was spawned) instead of a bare `active_async=N`.
    pub(super) active_async_holders: BTreeMap<u64, &'static Location<'static>>,
    /// SYNC counterpart of [`active_async_holders`](Self::active_async_holders):
    /// the dedicated-pacer OS threads currently counted as `Running` in `active`
    /// (audio worker, downloader runtime, flush hub, …), keyed by `ThreadKey`
    /// and named by the thread name. Inserted when a pacer claims/resumes
    /// `Running` (`mark_dedicated` / dedicated `resume_after_wait`), removed when
    /// it parks (`enter_wait_locked` `Running` arm) or exits
    /// (`on_participant_exit`). It names the PERSISTENT clock pinners; `active`
    /// may briefly exceed it by transient reserve / wake-handoff units (a parent
    /// `pre_count_dedicated` slot not yet claimed, or a just-fired wake bump
    /// before its thread resumes) — the dump annotates that gap.
    pub(super) active_sync_holders: BTreeMap<ThreadKey, SyncHolder>,
}

/// Diagnostic identity of a sync `active` holder — a dedicated pacer thread.
/// Purely for the hang dump (see
/// [`Registry::active_sync_holders`](Registry::active_sync_holders)).
pub(in crate::flash) struct SyncHolder {
    /// The OS thread name, if it was named (`spawn_named` pacers always are).
    pub(super) name: Option<String>,
}

/// Parked-waiter queues and pacing state of the engine.
pub(in crate::flash) struct Scheduler {
    /// Timed waiters keyed by (virtual deadline nanos, unique id) so the
    /// minimum deadline is `first_key_value`; ties share the same deadline and
    /// differ only by id. The map doubles as entry storage.
    pub(super) timed: BTreeMap<(u64, WaiterId), Entry>,
    /// Untimed waiters (no deadline) keyed by unique id; woken only by a signal
    /// ([`FlashInner::signal_condvar`]), never by a clock jump.
    pub(super) indef: BTreeMap<WaiterId, Entry>,
    /// Cooperative-yield waiters (sim `thread::yield_now` AND
    /// `tokio::task::yield_now`): a busy-poll loop that relinquishes the engine so
    /// the clock can advance. They carry NO deadline — woken on the NEXT clock
    /// advance, then re-check (the loop's own deadline / poll condition is
    /// re-evaluated on re-poll). A `Sync` entry is a parked OS thread; a `Task`
    /// entry is a parked async task whose `active_async` slot the spawn gate
    /// releases while it waits. Keyed by id.
    pub(super) yielders: BTreeMap<WaiterId, Wake>,
    /// Thread ids whose `unpark` arrived while not parked: the next
    /// `park_timed_unparkable` for that id consumes the flag and returns at once.
    pub(super) unpark_pending: BTreeSet<ThreadKey>,
    /// Condvar ids whose `notify_one` arrived while no waiter
    /// was registered: the next `notified()` first-poll for that cvid consumes
    /// the permit and resolves immediately (mirrors `tokio::Notify`'s stored
    /// permit). Only set by the async [`Notify`](crate::sync::Notify) path.
    pub(super) notify_permits: BTreeSet<CvId>,
    /// REAL I/O operations currently in flight (socket sends / body-chunk
    /// awaits bracketed by [`crate::flash::RealIoScope`]). While non-zero the clock
    /// is PACED, not pinned: `Core::try_advance` refuses any jump beyond
    /// `pace_anchor`'s virtual base plus the REAL time elapsed since it was
    /// set — virtual time may not outrun real time while real-world transit
    /// is pending. Maintained by [`FlashInner::real_io_enter`]/[`FlashInner::real_io_exit`].
    pub(super) real_io: usize,
    /// `(real instant, virtual nanos)` sampled when `real_io` went 0 -> 1.
    /// The pace limit is `anchor_virtual + (real_now - anchor_real)`; cleared
    /// when the last op completes so full-speed collapse resumes at once.
    pub(super) pace_anchor: Option<(RealInstant, u64)>,
    /// Test-only oracle: the sequence of clock values the engine jumped
    /// to, recorded under the `core` lock alongside each advance. Proves
    /// min-jump, tie-batching (fewer entries than waiters), and determinism
    /// (identical sequence across runs). Cleared by [`FlashInner::reset`]; not
    /// present in non-test builds.
    #[cfg(test)]
    pub(super) advance_log: Vec<u64>,
}

/// The engine's lock-protected state. ALL fields mutate only under the ONE
/// `core` Mutex — deliberately one lock: the invariant "register + account +
/// advance happen in one critical section" requires atomic visibility of the
/// counters AND the queues. [`Registry`]/[`Scheduler`] decompose the data, not
/// the lock. The clock is written only by `Core::try_advance` (and the
/// test-only additive `Clock::advance`, which the harness never calls).
pub(in crate::flash) struct Core {
    pub(super) registry: Registry,
    pub(super) sched: Scheduler,
}

impl Core {
    const fn new() -> Self {
        Self {
            registry: Registry {
                active: 0,
                active_async: 0,
                next_id: 0,
                next_cv: 0,
                next_task_id: 0,
                active_async_holders: BTreeMap::new(),
                active_sync_holders: BTreeMap::new(),
            },
            sched: Scheduler {
                timed: BTreeMap::new(),
                indef: BTreeMap::new(),
                yielders: BTreeMap::new(),
                unpark_pending: BTreeSet::new(),
                notify_permits: BTreeSet::new(),
                real_io: 0,
                pace_anchor: None,
                #[cfg(test)]
                advance_log: Vec::new(),
            },
        }
    }
}

/// Single owner of ALL flash-engine state: clock, scheduler core and pacer.
/// Engine methods live on `&self` (defined in `sched.rs`/`credit.rs`/`pace.rs`)
/// and never touch the [`FLASH`] global, so a local instance is a complete
/// engine. Methods that need the clock inside a critical section lock
/// `self.core` and read/write `self.clock` in the SAME hold (or pass `&Clock`
/// into `Core` methods) — never a pre-lock snapshot.
pub(in crate::flash) struct FlashInner {
    pub(in crate::flash) clock: Clock,
    /// The ONE engine lock (former global `SCHED`).
    pub(super) core: Mutex<Core>,
    /// Real-I/O pacer state + its lazily-spawned eternal thread. Lock order:
    /// `pacer.armed` is taken BEFORE `core`, never the other way.
    pub(super) pacer: Pacer,
}

impl FlashInner {
    fn new() -> Arc<Self> {
        Arc::new_cyclic(|weak| Self {
            clock: Clock::new(),
            core: Mutex::new(Core::new()),
            pacer: Pacer::new(Weak::clone(weak)),
        })
    }

    /// Test-only: a fresh LOCAL engine instance. A complete engine — clock,
    /// `core`, per-instance pacer — fully isolated from the process [`FLASH`],
    /// so the pure-scheduler harness tests need no serialization and cannot
    /// split bookkeeping with primitive-path tests that drive the global.
    #[cfg(test)]
    pub(in crate::flash) fn new_arc() -> Arc<Self> {
        Self::new()
    }

    /// Reset the timeline to its base and clear the engine state (for the
    /// in-process test harness; nextest gives production tests per-process
    /// isolation). Order matters: store the base first, then drop the engine
    /// state, so afterwards the clock reads `Instant::BASE_NANOS` and the
    /// engine is empty. Deliberately untouched (exactly as before W2): the
    /// pacer's `armed` flag (disarmed only by `real_io_exit`), the real-clock
    /// anchor, and the per-thread credit cells (separate `reset_credit()`).
    pub(in crate::flash) fn reset(&self) {
        self.clock.reset();
        *self.core.lock() = Core::new();
    }
}

/// The process-wide engine instance, created lazily on first touch. Only the
/// free-fn forwards (and the control surface in `api.rs`) bind to this global;
/// every engine method below it is instance-addressed.
pub(in crate::flash) static FLASH: LazyLock<Arc<FlashInner>> = LazyLock::new(FlashInner::new);
