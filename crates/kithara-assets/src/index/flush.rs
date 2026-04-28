#![forbid(unsafe_code)]

//! Shared flush coordinator for `AssetStore` on-disk indexes.
//!
//! `FlushHub` aggregates `Flushable` sources (`PinsInner`, `LruInner`,
//! `AvailabilityInner`) and serves two roles:
//!
//! - **Sync flush coordinator**: [`FlushHub::flush_now`] iterates the
//!   registry, invokes `Flushable::flush` on every source whose
//!   `dirty` flag is set, and clears the flag on success. Mutators
//!   call this directly when no background worker is attached
//!   (default).
//!
//! - **Background coalescer (opt-in)**: [`FlushHub::start_worker`]
//!   spawns a dedicated `kithara-flush-hub` thread (via
//!   [`kithara_platform::thread::spawn_named`]) that wakes on
//!   [`FlushHub::signal`] notifications, debounces a configurable
//!   window (`FlushPolicy::debounce`), and then flushes dirty sources
//!   under the same `flush_lock` as `flush_now` — so worker and
//!   `flush_now` never race. A long sustained burst that would
//!   otherwise stay debounced is force-flushed every
//!   `FlushPolicy::force_every_n_ops` operations.
//!
//! Mutators always go through [`signal_or_flush_sync`]: with a worker
//! they `signal()` and return immediately; without one they call
//! `flush_now()`. The hub is **mandatory** in every disk-backed index,
//! so the path is always the same regardless of worker presence.
//!
//! Indexes register a `Weak<dyn Flushable>` so the hub does not extend
//! their lifetime; dropped indexes are GC'd from the registry on the
//! next flush cycle.

use std::{
    num::NonZeroUsize,
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(test)]
use kithara_platform::thread::sleep;
use kithara_platform::{
    Condvar, Mutex,
    thread::{JoinHandle, spawn_named},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

use crate::error::AssetsResult;

/// Source that can serialise its in-memory state to disk on demand.
///
/// Implemented by `PinsInner`, `LruInner`, and `AvailabilityInner`.
/// Each inner exposes a `dirty` flag the hub can swap to discover and
/// clear pending work. `flush()` must be idempotent — replaying after a
/// successful flush should be a no-op or write the same bytes.
pub(crate) trait Flushable: Send + Sync {
    fn name(&self) -> &'static str;
    fn dirty(&self) -> &AtomicBool;
    fn flush(&self) -> AssetsResult<()>;
}

/// Tunables for [`FlushHub`].
#[derive(Clone, Debug)]
pub struct FlushPolicy {
    /// Coalesce window: when the worker sees a signal, it sleeps this
    /// long before draining dirty sources, so a burst of mutations
    /// produces a single flush. Ignored when `force_every_n_ops` is
    /// reached.
    pub debounce: Duration,
    /// Cap on coalescing: if `signal()` is called this many times
    /// without a flush, the worker bypasses `debounce` and flushes
    /// immediately. Protects against sustained bursts that would
    /// otherwise grow the in-memory backlog without bound.
    pub force_every_n_ops: NonZeroUsize,
    /// Cancel-token poll interval. The worker wakes from `cv.wait_for`
    /// at least this often to check for shutdown.
    pub poll_interval: Duration,
}

impl Default for FlushPolicy {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(50),
            force_every_n_ops: NonZeroUsize::new(256).expect("256 > 0"),
            poll_interval: Duration::from_millis(100),
        }
    }
}

/// Internal mutable state shared between `signal()`, `flush_now()`, and
/// the worker thread.
#[derive(Default)]
struct HubState {
    /// `true` when at least one source has been marked dirty since the
    /// last flush cycle.
    pending: bool,
    /// Number of `signal()` calls since the last flush cycle. Resets
    /// to zero on every flush. When it reaches
    /// `policy.force_every_n_ops` the worker bypasses the debounce.
    op_count: usize,
}

/// Shared flush coordinator. Created once per process or per
/// `AssetStore` builder; threaded into every index via `register`.
///
/// See module docs for the worker / sync-fallback semantics.
pub struct FlushHub {
    cancel: CancellationToken,
    policy: FlushPolicy,
    state: Mutex<HubState>,
    cv: Condvar,
    flush_lock: Mutex<()>,
    sources: Mutex<Vec<Weak<dyn Flushable>>>,
    worker: OnceLock<JoinHandle<()>>,
}

impl FlushHub {
    /// Create a hub without a background worker. Mutators flush
    /// synchronously through [`Self::flush_now`].
    #[must_use]
    pub fn new(cancel: CancellationToken, policy: FlushPolicy) -> Arc<Self> {
        Arc::new(Self {
            cancel,
            policy,
            state: Mutex::new(HubState::default()),
            cv: Condvar::new(),
            flush_lock: Mutex::new(()),
            sources: Mutex::new(Vec::new()),
            worker: OnceLock::new(),
        })
    }

    /// Convenience: [`Self::new`] followed by [`Self::start_worker`].
    #[must_use]
    pub fn with_worker(cancel: CancellationToken, policy: FlushPolicy) -> Arc<Self> {
        let hub = Self::new(cancel, policy);
        hub.start_worker();
        hub
    }

    /// Spawn the background flush worker (idempotent). Subsequent
    /// `signal()` calls coalesce mutations through `policy.debounce`
    /// before flushing.
    pub fn start_worker(self: &Arc<Self>) {
        let hub = Arc::clone(self);
        self.worker
            .get_or_init(|| spawn_named("kithara-flush-hub", move || hub.run()));
    }

    /// Whether a background worker is attached.
    #[must_use]
    pub fn has_worker(&self) -> bool {
        self.worker.get().is_some()
    }

    /// Register an index for background-driven flushing. The hub holds
    /// a `Weak`: if the index is dropped, the slot is GC'd on the next
    /// `flush_dirty` pass.
    pub(crate) fn register(self: &Arc<Self>, source: Weak<dyn Flushable>) {
        self.sources.lock_sync().push(source);
    }

    /// Wake the worker. Bumps the operation counter so a sustained
    /// burst eventually bypasses the debounce. No-op when no worker is
    /// attached — the helper [`signal_or_flush_sync`] is the canonical
    /// entry point for mutators and routes through `flush_now()`
    /// instead.
    pub(crate) fn signal(self: &Arc<Self>) {
        {
            let mut s = self.state.lock_sync();
            s.pending = true;
            s.op_count = s.op_count.saturating_add(1);
        }
        self.cv.notify_one();
    }

    /// Synchronously flush every dirty source. Serialises with the
    /// worker through `flush_lock`, so concurrent worker invocations
    /// see the same dirty set exactly once.
    ///
    /// # Errors
    ///
    /// Returns the first per-source flush error encountered (others
    /// are logged through `tracing::warn!` and the source's `dirty`
    /// flag is restored so the next cycle retries).
    pub fn flush_now(&self) -> AssetsResult<()> {
        let _g = self.flush_lock.lock_sync();
        {
            let mut s = self.state.lock_sync();
            s.pending = false;
            s.op_count = 0;
        }
        self.flush_dirty()
    }

    fn flush_dirty(&self) -> AssetsResult<()> {
        let alive: Vec<Arc<dyn Flushable>> = {
            let mut g = self.sources.lock_sync();
            g.retain(|w| w.strong_count() > 0);
            g.iter().filter_map(Weak::upgrade).collect()
        };
        let mut first_err = None;
        for src in alive {
            if !src.dirty().swap(false, Ordering::AcqRel) {
                continue;
            }
            if let Err(e) = src.flush() {
                tracing::warn!(
                    source = src.name(),
                    error = %e,
                    "FlushHub: source flush failed",
                );
                src.dirty().store(true, Ordering::Release);
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    fn run(self: Arc<Self>) {
        loop {
            // 1. Wait for a pending signal or cancellation.
            let mut guard = self.state.lock_sync();
            while !guard.pending {
                if self.cancel.is_cancelled() {
                    drop(guard);
                    let _g = self.flush_lock.lock_sync();
                    let _ = self.flush_dirty();
                    return;
                }
                let deadline = Instant::now() + self.policy.poll_interval;
                let (next, _) = self.cv.wait_sync_timeout(guard, deadline);
                guard = next;
            }
            drop(guard);

            // 2. Debounce window. Wait on the condvar so additional
            //    signals can either wake us early when the force-flush
            //    threshold is reached or extend coalescing within the
            //    same window. Cancellation also breaks out early.
            let debounce_deadline = Instant::now() + self.policy.debounce;
            loop {
                let guard = self.state.lock_sync();
                if guard.op_count >= self.policy.force_every_n_ops.get() {
                    break;
                }
                if self.cancel.is_cancelled() {
                    drop(guard);
                    let _g = self.flush_lock.lock_sync();
                    let _ = self.flush_dirty();
                    return;
                }
                if Instant::now() >= debounce_deadline {
                    break;
                }
                let (_next, _) = self.cv.wait_sync_timeout(guard, debounce_deadline);
            }

            // 3. Reset counters and flush under flush_lock.
            {
                let mut guard = self.state.lock_sync();
                guard.pending = false;
                guard.op_count = 0;
            }
            let _g = self.flush_lock.lock_sync();
            let _ = self.flush_dirty();
        }
    }
}

impl std::fmt::Debug for FlushHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Internal locks/handles intentionally elided — they don't
        // round-trip through `Debug` and would either deadlock or
        // poison the formatter under contention.
        f.debug_struct("FlushHub")
            .field("policy", &self.policy)
            .field("has_worker", &self.has_worker())
            .field("sources", &self.sources.try_lock().map(|s| s.len()).ok())
            .finish_non_exhaustive()
    }
}

/// Helper used by every index mutator. Marks the source dirty, then:
///
/// - **Hub attached, with worker** — wakes the worker; flush happens
///   on the background thread under `flush_lock`.
/// - **Hub attached, no worker** — runs [`FlushHub::flush_now`] which
///   drains every dirty source registered with that hub.
/// - **No hub attached** — runs `Flushable::flush` on the source
///   directly, matching the legacy inline-flush behaviour for ad-hoc
///   tests that never call `attach_to`.
///
/// # Errors
///
/// Propagates whichever flush path runs.
pub(crate) fn signal_or_flush_sync(
    hub: Option<&Arc<FlushHub>>,
    source: &dyn Flushable,
) -> AssetsResult<()> {
    source.dirty().store(true, Ordering::Release);
    if let Some(h) = hub
        && h.has_worker()
    {
        h.signal();
        return Ok(());
    }
    // No worker — flush THIS source only. Do NOT call `hub.flush_now()`:
    // that would flush every dirty source registered with the hub and
    // break per-source durability semantics (e.g. Availability is
    // explicit-checkpoint only — its observer marks dirty but must not
    // be persisted by an unrelated Pins/LRU mutation).
    source.flush()
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use kithara_test_utils::kithara;

    use super::*;

    /// Counts `flush()` invocations and reports a configurable name.
    /// Used to validate hub invariants without depending on real disk
    /// I/O.
    struct CountingSource {
        name: &'static str,
        dirty: AtomicBool,
        flushes: AtomicUsize,
    }

    impl CountingSource {
        fn new(name: &'static str) -> Arc<Self> {
            Arc::new(Self {
                name,
                dirty: AtomicBool::new(false),
                flushes: AtomicUsize::new(0),
            })
        }

        fn flush_count(&self) -> usize {
            self.flushes.load(Ordering::Acquire)
        }
    }

    impl Flushable for CountingSource {
        fn name(&self) -> &'static str {
            self.name
        }
        fn dirty(&self) -> &AtomicBool {
            &self.dirty
        }
        fn flush(&self) -> AssetsResult<()> {
            self.flushes.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    fn fast_policy() -> FlushPolicy {
        FlushPolicy {
            debounce: Duration::from_millis(10),
            force_every_n_ops: NonZeroUsize::new(8).unwrap(),
            poll_interval: Duration::from_millis(20),
        }
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn no_worker_flushes_synchronously_via_signal_or_flush_sync() {
        let hub = FlushHub::new(CancellationToken::new(), fast_policy());
        let src = CountingSource::new("src");
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);

        signal_or_flush_sync(Some(&hub), &*src).unwrap();
        assert_eq!(
            src.flush_count(),
            1,
            "no worker → mutator drives sync flush"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn flush_now_drains_dirty_sources() {
        let hub = FlushHub::new(CancellationToken::new(), fast_policy());
        let src = CountingSource::new("src");
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);

        // Manually mark dirty without going through signal_or_flush_sync.
        src.dirty.store(true, Ordering::Release);
        hub.flush_now().unwrap();
        assert_eq!(src.flush_count(), 1);
        assert!(!src.dirty.load(Ordering::Acquire), "dirty cleared on flush");

        hub.flush_now().unwrap();
        assert_eq!(src.flush_count(), 1, "second flush is no-op when clean");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn worker_coalesces_burst_into_single_flush() {
        let hub = FlushHub::with_worker(CancellationToken::new(), fast_policy());
        let src = CountingSource::new("burst");
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);

        for _ in 0..5 {
            signal_or_flush_sync(Some(&hub), &*src).unwrap();
        }

        // Wait long enough for the debounce to elapse and the worker
        // to drain the pending flag.
        let deadline = Instant::now() + Duration::from_secs(2);
        while src.flush_count() == 0 && Instant::now() < deadline {
            sleep(Duration::from_millis(10));
        }

        assert_eq!(
            src.flush_count(),
            1,
            "5 signals must coalesce into one flush",
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn force_every_n_ops_bypasses_debounce() {
        // Long debounce + small force threshold → force kicks in.
        let policy = FlushPolicy {
            debounce: Duration::from_secs(10),
            force_every_n_ops: NonZeroUsize::new(4).unwrap(),
            poll_interval: Duration::from_millis(20),
        };
        let hub = FlushHub::with_worker(CancellationToken::new(), policy);
        let src = CountingSource::new("force");
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);

        for _ in 0..6 {
            signal_or_flush_sync(Some(&hub), &*src).unwrap();
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        while src.flush_count() == 0 && Instant::now() < deadline {
            sleep(Duration::from_millis(10));
        }

        assert!(
            src.flush_count() >= 1,
            "force-every-N must flush before the long debounce window elapses",
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn cancel_triggers_final_flush() {
        let cancel = CancellationToken::new();
        let hub = FlushHub::with_worker(cancel.clone(), fast_policy());
        let src = CountingSource::new("final");
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);

        // Mark dirty without signalling — cancellation must still
        // drain it via the final-flush path.
        src.dirty.store(true, Ordering::Release);
        cancel.cancel();

        let deadline = Instant::now() + Duration::from_secs(2);
        while src.flush_count() == 0 && Instant::now() < deadline {
            sleep(Duration::from_millis(10));
        }

        assert_eq!(src.flush_count(), 1, "cancel must trigger a final flush");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn dropped_source_is_gc_d_on_next_cycle() {
        let hub = FlushHub::new(CancellationToken::new(), fast_policy());
        let src = CountingSource::new("ephemeral");
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);
        drop(src);

        // flush_now retains alive sources only; no panic, no flush.
        hub.flush_now().unwrap();

        let remaining = hub.sources.lock_sync().len();
        assert_eq!(remaining, 0, "dropped sources must be GC'd from registry");
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn failed_flush_keeps_dirty_for_retry() {
        struct FailingSource {
            dirty: AtomicBool,
            attempts: AtomicUsize,
            fail_first: AtomicBool,
        }
        impl Flushable for FailingSource {
            fn name(&self) -> &'static str {
                "failing"
            }
            fn dirty(&self) -> &AtomicBool {
                &self.dirty
            }
            fn flush(&self) -> AssetsResult<()> {
                self.attempts.fetch_add(1, Ordering::AcqRel);
                if self.fail_first.swap(false, Ordering::AcqRel) {
                    Err(crate::error::AssetsError::Storage(
                        kithara_storage::StorageError::Failed("simulated".into()),
                    ))
                } else {
                    Ok(())
                }
            }
        }

        let src = Arc::new(FailingSource {
            dirty: AtomicBool::new(false),
            attempts: AtomicUsize::new(0),
            fail_first: AtomicBool::new(true),
        });
        let hub = FlushHub::new(CancellationToken::new(), fast_policy());
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);

        src.dirty.store(true, Ordering::Release);
        let _ = hub.flush_now();
        assert_eq!(src.attempts.load(Ordering::Acquire), 1);
        assert!(
            src.dirty.load(Ordering::Acquire),
            "failed flush keeps dirty=true for retry",
        );

        hub.flush_now().unwrap();
        assert_eq!(src.attempts.load(Ordering::Acquire), 2, "retried");
        assert!(!src.dirty.load(Ordering::Acquire));
    }
}
