#![forbid(unsafe_code)]

use std::{
    num::NonZeroUsize,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(test)]
use kithara_platform::thread::sleep;
#[cfg(test)]
use kithara_platform::time::Instant;
use kithara_platform::{Condvar, Mutex, time::Duration};
use tokio_util::sync::CancellationToken;

use super::worker::WorkerSlot;
use crate::error::AssetsResult;

/// Source that can serialise its in-memory state to disk on demand.
///
/// Implemented by `PinsInner`, `LruInner`, and `AvailabilityInner`.
/// Each inner exposes a `dirty` flag the hub can swap to discover and
/// clear pending work. `flush()` must be idempotent — replaying after a
/// successful flush should be a no-op or write the same bytes.
pub(crate) trait Flushable: Send + Sync {
    fn dirty(&self) -> &AtomicBool;
    /// Best-effort flush. Atomic at the directory-entry level (POSIX
    /// rename), but does NOT `sync_data` — payload pages may still be
    /// in the OS page cache when this returns. Used on the per-
    /// mutation hot path where throughput trumps strict durability.
    fn flush(&self) -> AssetsResult<()>;
    /// Durable flush. Forces payload pages to physical disk BEFORE
    /// the atomic rename, so any process that observes the canonical
    /// path post-return either sees no file or sees the fully durable
    /// committed bytes — survives power-loss.
    ///
    /// Default forwards to [`Self::flush`]; disk-backed inners
    /// override to call `Atomic::write_all_durable`. Used only on the
    /// explicit `checkpoint()` path (rare; throughput is irrelevant).
    fn flush_durable(&self) -> AssetsResult<()> {
        self.flush()
    }
    fn name(&self) -> &'static str;
}

/// Tunables for [`FlushHub`].
#[derive(Clone, Debug)]
pub struct FlushPolicy {
    /// Coalesce window: when the worker sees a signal, it sleeps this
    /// long before draining dirty sources, so a burst of mutations
    /// produces a single flush. Ignored when `force_every_n_ops` is
    /// reached.
    pub debounce: Duration,
    /// Cancel-token poll interval. The worker wakes from `cv.wait_for`
    /// at least this often to check for shutdown.
    pub poll_interval: Duration,
    /// Cap on coalescing: if `signal()` is called this many times
    /// without a flush, the worker bypasses `debounce` and flushes
    /// immediately. Protects against sustained bursts that would
    /// otherwise grow the in-memory backlog without bound.
    pub force_every_n_ops: NonZeroUsize,
}

impl Default for FlushPolicy {
    fn default() -> Self {
        /// Default debounce window; coalesces bursts of writes into one
        /// flush without delaying isolated writes too long.
        const DEFAULT_DEBOUNCE_MS: u64 = 50;
        /// Default ops cap before a forced flush — prevents unbounded
        /// dirty growth when writes never debounce.
        const DEFAULT_FORCE_OPS: usize = 256;
        /// Default poll cadence for the background flush worker.
        const DEFAULT_POLL_INTERVAL_MS: u64 = 100;
        Self {
            debounce: Duration::from_millis(DEFAULT_DEBOUNCE_MS),
            force_every_n_ops: NonZeroUsize::new(DEFAULT_FORCE_OPS)
                .expect("BUG: DEFAULT_FORCE_OPS const is statically non-zero"),
            poll_interval: Duration::from_millis(DEFAULT_POLL_INTERVAL_MS),
        }
    }
}

/// Internal mutable state shared between `signal()`, `flush_now()`, and
/// the worker thread.
#[derive(Default)]
pub(super) struct HubState {
    /// `true` when at least one source has been marked dirty since the
    /// last flush cycle.
    pub(super) pending: bool,
    /// Number of `signal()` calls since the last flush cycle. Resets
    /// to zero on every flush. When it reaches
    /// `policy.force_every_n_ops` the worker bypasses the debounce.
    pub(super) op_count: usize,
}

/// Wait primitive shared between `signal()`, the worker thread, and
/// `flush_now()`. Held in its own `Arc` so the worker can park on it
/// without keeping a strong `Arc<FlushHub>` alive — that is what lets
/// the hub reach refcount zero and run its `Drop`.
pub(in crate::flush) struct HubWait {
    pub(in crate::flush) cv: Condvar,
    pub(in crate::flush) state: Mutex<HubState>,
}

/// Shared flush coordinator. Created once per process or per
/// `AssetStore` builder; threaded into every index via `register`.
///
/// See module docs for the worker / sync-fallback semantics.
pub struct FlushHub {
    pub(in crate::flush) cancel: CancellationToken,
    pub(in crate::flush) policy: FlushPolicy,
    pub(in crate::flush) flush_lock: Mutex<()>,
    pub(in crate::flush) sources: Mutex<Vec<Weak<dyn Flushable>>>,
    pub(in crate::flush) wait: Arc<HubWait>,
    pub(in crate::flush) worker: WorkerSlot,
}

impl FlushHub {
    /// Create a hub without a background worker. Mutators flush
    /// synchronously through [`Self::flush_now`].
    #[must_use]
    pub fn new(cancel: CancellationToken, policy: FlushPolicy) -> Arc<Self> {
        Arc::new(Self {
            // The caller passes a child token it owns exclusively (see
            // the cancel-hierarchy rule): `Drop` cancels it to stop the
            // worker without touching the shared parent (storage, app).
            cancel,
            policy,
            flush_lock: Mutex::new(()),
            sources: Mutex::new(Vec::new()),
            wait: Arc::new(HubWait {
                cv: Condvar::new(),
                state: Mutex::new(HubState::default()),
            }),
            worker: WorkerSlot::default(),
        })
    }

    pub(super) fn flush_dirty(&self, durable: bool) -> AssetsResult<()> {
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
            let result = if durable {
                src.flush_durable()
            } else {
                src.flush()
            };
            if let Err(e) = result {
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
            let mut s = self.wait.state.lock_sync();
            s.pending = false;
            s.op_count = 0;
        }
        self.flush_dirty(true)
    }

    /// Whether a background worker is attached. Always `false` on wasm32
    /// where the worker module is gated out entirely.
    #[must_use]
    pub fn has_worker(&self) -> bool {
        self.worker.is_started()
    }

    /// Register an index for background-driven flushing. The hub holds
    /// a `Weak`: if the index is dropped, the slot is GC'd on the next
    /// `flush_dirty` pass.
    pub(crate) fn register(self: &Arc<Self>, source: Weak<dyn Flushable>) {
        self.sources.lock_sync().push(source);
        self.start_worker();
    }

    /// Lazily spawn the background flush worker (idempotent). Called on
    /// the first source registration, mirroring the downloader / audio
    /// worker lazy-start. The thread is torn down by [`FlushHub`]'s
    /// `Drop`.
    pub(crate) fn start_worker(self: &Arc<Self>) {
        self.worker.start_with(self);
    }

    /// Wake the worker to coalesce a burst of mutations into one
    /// background flush. Bumps the operation counter so a sustained
    /// burst eventually bypasses the debounce. The availability index
    /// is the caller: it is rewritten on every write/commit, so it
    /// debounces through the worker. Pins and the LRU index instead
    /// flush eagerly via [`flush_sync`] for crash safety.
    pub(crate) fn signal(self: &Arc<Self>) {
        {
            let mut s = self.wait.state.lock_sync();
            s.pending = true;
            s.op_count = s.op_count.saturating_add(1);
        }
        self.wait.cv.notify_one();
    }
}

impl Drop for FlushHub {
    fn drop(&mut self) {
        self.cancel.cancel();
        {
            let _g = self.wait.state.lock_sync();
            self.wait.cv.notify_all();
        }
        self.worker.shutdown_join();
    }
}

impl std::fmt::Debug for FlushHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlushHub")
            .field("policy", &self.policy)
            .field("has_worker", &self.has_worker())
            .field("sources", &self.sources.try_lock().map(|s| s.len()).ok())
            .finish_non_exhaustive()
    }
}

/// Eagerly flush a single index source. Pins and the LRU index are
/// crash-safety state — a pinned resource must be durable immediately
/// so a crash can't lose it and let the resource be evicted — so their
/// mutators flush synchronously here rather than deferring to the
/// debounced [`FlushHub`] worker. The worker instead serves the
/// frequently-rewritten availability index, which wakes it via
/// [`FlushHub::signal`].
///
/// Marks the source dirty before flushing: `Flushable::flush` clears
/// the flag on success, so a failed flush leaves `dirty == true` and
/// the next cycle retries.
///
/// # Errors
///
/// Propagates the underlying flush error.
pub(crate) fn flush_sync(source: &dyn Flushable) -> AssetsResult<()> {
    source.dirty().store(true, Ordering::Release);
    source.flush()
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use kithara_storage::StorageError;
    use kithara_test_utils::kithara;

    use super::*;

    /// Counts `flush()` and `flush_durable()` invocations separately.
    /// Used to validate hub invariants without depending on real disk
    /// I/O.
    struct CountingSource {
        name: &'static str,
        dirty: AtomicBool,
        durable_flushes: AtomicUsize,
        flushes: AtomicUsize,
    }

    impl CountingSource {
        fn new(name: &'static str) -> Arc<Self> {
            Arc::new(Self {
                name,
                dirty: AtomicBool::new(false),
                flushes: AtomicUsize::new(0),
                durable_flushes: AtomicUsize::new(0),
            })
        }

        fn durable_flush_count(&self) -> usize {
            self.durable_flushes.load(Ordering::Acquire)
        }

        fn flush_count(&self) -> usize {
            self.flushes.load(Ordering::Acquire)
        }
    }

    impl Flushable for CountingSource {
        fn dirty(&self) -> &AtomicBool {
            &self.dirty
        }
        fn flush(&self) -> AssetsResult<()> {
            self.flushes.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
        fn flush_durable(&self) -> AssetsResult<()> {
            self.durable_flushes.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
        fn name(&self) -> &'static str {
            self.name
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
    fn flush_sync_flushes_eagerly() {
        let src = CountingSource::new("src");

        flush_sync(&*src).unwrap();
        assert_eq!(
            src.flush_count(),
            1,
            "pin/LRU mutators flush eagerly and synchronously"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn flush_now_drains_dirty_sources() {
        let hub = FlushHub::new(CancellationToken::new(), fast_policy());
        let src = CountingSource::new("src");
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);

        src.dirty.store(true, Ordering::Release);
        hub.flush_now().unwrap();
        assert_eq!(
            src.durable_flush_count(),
            1,
            "flush_now is the explicit-checkpoint path → durable variant"
        );
        assert_eq!(
            src.flush_count(),
            0,
            "flush_now must not call the non-durable variant"
        );
        assert!(!src.dirty.load(Ordering::Acquire), "dirty cleared on flush");

        hub.flush_now().unwrap();
        assert_eq!(
            src.durable_flush_count(),
            1,
            "second flush is no-op when clean"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn flush_sync_uses_non_durable_variant() {
        let src = CountingSource::new("src");

        flush_sync(&*src).unwrap();
        assert_eq!(src.flush_count(), 1, "eager mutator path → non-durable");
        assert_eq!(
            src.durable_flush_count(),
            0,
            "eager mutator path must NOT pay the sync_data fence"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn worker_coalesces_burst_into_single_flush() {
        let hub = FlushHub::new(CancellationToken::new(), fast_policy());
        let src = CountingSource::new("burst");
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);

        for _ in 0..5 {
            src.dirty.store(true, Ordering::Release);
            hub.signal();
        }

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
        let policy = FlushPolicy {
            debounce: Duration::from_secs(10),
            force_every_n_ops: NonZeroUsize::new(4).unwrap(),
            poll_interval: Duration::from_millis(20),
        };
        let hub = FlushHub::new(CancellationToken::new(), policy);
        let src = CountingSource::new("force");
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);

        for _ in 0..6 {
            src.dirty.store(true, Ordering::Release);
            hub.signal();
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
        let hub = FlushHub::new(cancel.clone(), fast_policy());
        let src = CountingSource::new("final");
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);

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
                    Err(crate::error::AssetsError::Storage(StorageError::Failed(
                        "simulated".into(),
                    )))
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

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn register_lazily_starts_worker() {
        let hub = FlushHub::new(CancellationToken::new(), fast_policy());
        assert!(
            !hub.has_worker(),
            "fresh hub has no worker until a source registers"
        );
        let src = CountingSource::new("lazy");
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);
        assert!(
            hub.has_worker(),
            "first registration lazily starts the background worker"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn repeated_hub_drop_joins_cleanly() {
        for _ in 0..20 {
            let hub = FlushHub::new(CancellationToken::new(), fast_policy());
            let src = CountingSource::new("churn");
            hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);
            for _ in 0..3 {
                src.dirty.store(true, Ordering::Release);
                hub.signal();
            }
        }
    }
}
