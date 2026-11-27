#![forbid(unsafe_code)]

use std::{
    num::NonZeroUsize,
    sync::{
        Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use dashmap::DashSet;
use kithara_config::Config;
use kithara_derive::Patch;
use kithara_platform::{
    CancelToken,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use super::worker::WorkerSlot;
use crate::error::AssetsResult;

/// Source that can serialise its in-memory state to disk on demand.
///
/// Implemented by `PinsInner`, `LruInner`, and `AvailabilityInner`.
/// Each inner exposes a `dirty` flag that the shared flush protocol consumes
/// before snapshotting. `flush()` must be idempotent — replaying after a
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
    /// override to `sync_data` the new index file. Used only on the
    /// explicit `checkpoint()` path (rare; throughput is irrelevant).
    fn flush_durable(&self) -> AssetsResult<()> {
        self.flush()
    }
    fn name(&self) -> &'static str;
}

/// Tunables for [`FlushHub`].
#[derive(Clone, Debug, Config, Patch)]
#[config(builder(none), fields(value))]
pub struct FlushPolicy {
    /// Coalesce window: when the worker sees a signal, it sleeps this
    /// long before draining dirty sources, so a burst of mutations
    /// produces a single flush. Ignored when `force_every_n_ops` is
    /// reached.
    #[config(patch(humantime))]
    pub debounce: Duration,
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
        Self {
            debounce: Duration::from_millis(DEFAULT_DEBOUNCE_MS),
            force_every_n_ops: NonZeroUsize::new(DEFAULT_FORCE_OPS)
                .expect("BUG: DEFAULT_FORCE_OPS const is statically non-zero"),
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
pub(super) struct HubWait {
    pub(super) cv: Condvar,
    pub(super) state: Mutex<HubState>,
}

/// Shared flush coordinator. Created once per process or per
/// `AssetStore` builder; threaded into every index via `register`.
///
/// See module docs for the worker / sync-fallback semantics.
pub struct FlushHub {
    pub(super) wait: Arc<HubWait>,
    pub(super) cancel: CancelToken,
    /// Live `Arc` identities of sources with a best-effort publication in
    /// flight or completed by the worker or an eager writer. An explicit
    /// durable checkpoint must re-flush these even when their `dirty` flag is
    /// clear, superseding the best-effort snapshot under the per-file lock.
    /// Pruned to the alive set each `flush_dirty` pass. Hub passes and attached
    /// eager calls share `flush_lock` from registration through the write result,
    /// so an older pass cannot prune or discharge a newer eager publication.
    pub(super) non_durable: DashSet<usize>,
    pub(super) policy: FlushPolicy,
    pub(super) flush_lock: Mutex<()>,
    pub(super) sources: Mutex<Vec<Weak<dyn Flushable>>>,
    pub(super) worker: WorkerSlot,
}

impl FlushHub {
    /// Create a hub without a background worker. Mutators flush
    /// synchronously through [`Self::flush_now`].
    ///
    /// `cancel` is a caller-owned child token; dropping the hub cancels it to stop the worker.
    #[must_use]
    pub fn new(cancel: CancelToken, policy: FlushPolicy) -> Arc<Self> {
        Arc::new(Self {
            cancel,
            policy,
            flush_lock: Mutex::new(()),
            sources: Mutex::default(),
            non_durable: DashSet::new(),
            wait: Arc::new(HubWait {
                cv: Condvar::default(),
                state: Mutex::default(),
            }),
            worker: WorkerSlot::default(),
        })
    }

    /// A durable flush also re-flushes a source with an in-flight or completed
    /// best-effort publication, since its dirty flag may already be clear.
    pub(super) fn flush_dirty(&self, durable: bool) -> AssetsResult<()> {
        let alive: Vec<Arc<dyn Flushable>> = {
            let mut g = self.sources.lock();
            g.retain(|w| w.strong_count() > 0);
            g.iter().filter_map(Weak::upgrade).collect()
        };
        self.non_durable.retain(|key| {
            alive
                .iter()
                .any(|source| Arc::as_ptr(source).cast::<()>().addr() == *key)
        });

        let mut first_err = None;
        for src in &alive {
            let key = Arc::as_ptr(src).cast::<()>().addr();
            let force = durable && self.non_durable.contains(&key);
            match flush_source(&**src, durable, force) {
                Ok(false) => {}
                Ok(true) => {
                    if durable {
                        self.non_durable.remove(&key);
                    } else {
                        self.non_durable.insert(key);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        source = src.name(),
                        error = %e,
                        "FlushHub: source flush failed",
                    );
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Force an immediate synchronous flush of all dirty indexes.
    ///
    /// # Errors
    /// Returns `AssetsError` if persisting any index resource fails.
    pub fn flush_now(&self) -> AssetsResult<()> {
        let _g = self.flush_lock.lock();
        {
            let mut s = self.wait.state.lock();
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
        self.sources.lock().push(source);
        self.start_worker();
    }

    /// Wake the worker to coalesce a burst of mutations into one
    /// background flush. Bumps the operation counter so a sustained
    /// burst eventually bypasses the debounce. Availability changes and
    /// byte-only LRU updates debounce through the worker. Pin changes and
    /// LRU recency/removal instead flush eagerly via [`flush_sync`] for
    /// crash safety.
    pub(crate) fn signal(self: &Arc<Self>) {
        {
            let mut s = self.wait.state.lock();
            s.pending = true;
            s.op_count = s.op_count.saturating_add(1);
        }
        self.wait.cv.notify_one();
    }

    /// Lazily spawn the background flush worker (idempotent). Called on
    /// the first source registration, mirroring other long-lived runtime
    /// worker lazy-start. Final hub destruction cancels the worker without
    /// waiting; source operations retain the hub until they finish.
    pub(crate) fn start_worker(self: &Arc<Self>) {
        self.worker.start_with(self);
    }
}

impl Drop for FlushHub {
    fn drop(&mut self) {
        self.cancel.cancel();
        {
            let _g = self.wait.state.lock();
            self.wait.cv.notify_all();
        }
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

fn flush_source(source: &dyn Flushable, durable: bool, force: bool) -> AssetsResult<bool> {
    let was_dirty = source.dirty().swap(false, Ordering::AcqRel);
    if !was_dirty && !force {
        return Ok(false);
    }
    let result = if durable {
        source.flush_durable()
    } else {
        source.flush()
    };
    if result.is_err() {
        source.dirty().store(true, Ordering::Release);
    }
    result.map(|()| true)
}

/// Flush pin/LRU crash-safety state synchronously so a crash cannot lose a live pin.
/// Forces serialized publication even when another flusher consumed the dirty mark.
/// Attached calls hold the hub lock from durability registration through the write
/// result, ordering them against checkpoint selection, cleanup, and pruning.
/// Hubless calls use the direct per-file path. Availability and byte-only LRU
/// updates use the debounced [`FlushHub`].
///
/// # Errors
/// Propagates the underlying flush error.
pub(crate) fn flush_sync<S: Flushable>(
    source: &Arc<S>,
    hub: Option<&Arc<FlushHub>>,
) -> AssetsResult<()> {
    let _hub_guard = hub.map(|hub| hub.flush_lock.lock());
    if let Some(hub) = hub {
        let key = Arc::as_ptr(source).cast::<()>().addr();
        hub.non_durable.insert(key);
    }
    source.dirty().store(true, Ordering::Release);
    flush_source(&**source, false, true).map(|_| ())
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use kithara_platform::sync::mpsc::{self, Sender, TryRecvError};
    use kithara_storage::StorageError;
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn a_patch_writes_only_the_field_it_names() {
        let settings: FlushPolicyPatch =
            serde_yaml_ng::from_str("force_every_n_ops: 512\n").expect("the document types");
        // Seeded away from the default of 50ms, so the assertion below can tell
        // "left alone" from "reset to the default".
        let mut policy = FlushPolicy {
            debounce: Duration::from_millis(250),
            ..FlushPolicy::default()
        };

        policy.apply(settings);

        assert_eq!(policy.force_every_n_ops.get(), 512);
        assert_eq!(
            policy.debounce,
            Duration::from_millis(250),
            "a silent field must keep the value it already had"
        );
    }

    /// Counts `flush()` and `flush_durable()` invocations separately.
    /// Used to validate hub invariants without depending on real disk
    /// I/O.
    struct CountingSource {
        name: &'static str,
        dirty: AtomicBool,
        durable_flushes: AtomicUsize,
        flushes: AtomicUsize,
        flushed: Option<Sender<()>>,
    }

    impl CountingSource {
        fn new(name: &'static str) -> Arc<Self> {
            Arc::new(Self {
                name,
                dirty: AtomicBool::new(false),
                flushes: AtomicUsize::new(0),
                durable_flushes: AtomicUsize::new(0),
                flushed: None,
            })
        }

        fn durable_flush_count(&self) -> usize {
            self.durable_flushes.load(Ordering::Acquire)
        }

        fn flush_count(&self) -> usize {
            self.flushes.load(Ordering::Acquire)
        }

        fn with_completion(name: &'static str, flushed: Sender<()>) -> Arc<Self> {
            Arc::new(Self {
                name,
                dirty: AtomicBool::new(false),
                flushes: AtomicUsize::new(0),
                durable_flushes: AtomicUsize::new(0),
                flushed: Some(flushed),
            })
        }
    }

    impl Flushable for CountingSource {
        fn dirty(&self) -> &AtomicBool {
            &self.dirty
        }
        fn flush(&self) -> AssetsResult<()> {
            self.flushes.fetch_add(1, Ordering::AcqRel);
            if let Some(flushed) = &self.flushed {
                flushed.send(()).unwrap();
            }
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
        }
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn flush_sync_flushes_eagerly() {
        let src = CountingSource::new("src");

        flush_sync(&src, None).unwrap();
        assert_eq!(
            src.flush_count(),
            1,
            "pin/LRU mutators flush eagerly and synchronously"
        );
    }

    #[kithara::test(flash(false), timeout(Duration::from_secs(5)))]
    fn eager_flush_returns_after_a_competing_consumer_publishes() {
        use std::{fs, path::PathBuf};

        enum Event {
            EagerFlushStarted,
            EagerReturned(Vec<u8>),
        }

        struct Source {
            dirty: AtomicBool,
            dirty_calls: AtomicUsize,
            dirty_claimed: Sender<()>,
            dirty_resume: Mutex<mpsc::Receiver<()>>,
            publication: Mutex<()>,
            competitor_started: Sender<()>,
            competitor_release: Mutex<mpsc::Receiver<()>>,
            flush_calls: AtomicUsize,
            events: Sender<Event>,
            path: PathBuf,
        }

        impl Flushable for Source {
            fn dirty(&self) -> &AtomicBool {
                if self.dirty_calls.fetch_add(1, Ordering::AcqRel) == 1 {
                    self.dirty_claimed.send(()).unwrap();
                    self.dirty_resume.lock().recv().unwrap();
                }
                &self.dirty
            }

            fn flush(&self) -> AssetsResult<()> {
                let call = self.flush_calls.fetch_add(1, Ordering::AcqRel);
                if call == 1 {
                    self.events.send(Event::EagerFlushStarted).unwrap();
                }
                let _publication = self.publication.lock();
                if call == 0 {
                    self.competitor_started.send(()).unwrap();
                    self.competitor_release.lock().recv().unwrap();
                }
                fs::write(&self.path, [1])?;
                Ok(())
            }

            fn name(&self) -> &'static str {
                "competing-eager"
            }
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("index.bin");
        fs::write(&path, [0]).unwrap();
        let (dirty_claimed, eager_claimed) = mpsc::channel();
        let (dirty_resume, resume_eager) = mpsc::channel();
        let (competitor_started, competing) = mpsc::channel();
        let (competitor_release, release_competitor) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let source = Arc::new(Source {
            dirty: AtomicBool::new(false),
            dirty_calls: AtomicUsize::new(0),
            dirty_claimed,
            dirty_resume: Mutex::new(resume_eager),
            publication: Mutex::new(()),
            competitor_started,
            competitor_release: Mutex::new(release_competitor),
            flush_calls: AtomicUsize::new(0),
            events: event_tx.clone(),
            path: path.clone(),
        });

        let eager = kithara_platform::thread::spawn_named("eager-caller", {
            let source = Arc::clone(&source);
            let events = event_tx;
            move || {
                flush_sync(&source, None).unwrap();
                let persisted = fs::read(&source.path).unwrap();
                events.send(Event::EagerReturned(persisted)).unwrap();
            }
        });
        eager_claimed.recv().unwrap();

        let competitor = kithara_platform::thread::spawn_named("competing-flush", {
            let source = Arc::clone(&source);
            move || {
                assert!(flush_source(&*source, false, false).unwrap());
            }
        });
        competing.recv().unwrap();
        dirty_resume.send(()).unwrap();

        let first = event_rx.recv().unwrap();
        competitor_release.send(()).unwrap();
        let persisted_at_return = match first {
            Event::EagerFlushStarted => match event_rx.recv().unwrap() {
                Event::EagerReturned(persisted) => persisted,
                Event::EagerFlushStarted => panic!("eager flush started twice"),
            },
            Event::EagerReturned(persisted) => persisted,
        };
        eager.join().unwrap();
        competitor.join().unwrap();

        assert_eq!(
            persisted_at_return,
            vec![1],
            "eager success must follow serialized publication, not a competitor's consumed mark"
        );
    }

    #[kithara::test(flash(false), timeout(Duration::from_secs(5)))]
    fn eager_flush_preserves_updates_made_during_io_through_checkpoint_and_reopen() {
        use std::{fs, path::PathBuf};

        struct Source {
            dirty: AtomicBool,
            current: AtomicUsize,
            path: PathBuf,
            snapshot: Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
        }

        impl Flushable for Source {
            fn dirty(&self) -> &AtomicBool {
                &self.dirty
            }

            fn flush(&self) -> AssetsResult<()> {
                let snapshot = self.current.load(Ordering::Acquire);
                self.snapshot.send(()).unwrap();
                self.release.lock().recv().unwrap();
                fs::write(&self.path, snapshot.to_le_bytes())?;
                Ok(())
            }

            fn flush_durable(&self) -> AssetsResult<()> {
                fs::write(
                    &self.path,
                    self.current.load(Ordering::Acquire).to_le_bytes(),
                )?;
                Ok(())
            }

            fn name(&self) -> &'static str {
                "eager-byte-update"
            }
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("index.bin");
        let hub = FlushHub::new(
            CancelToken::never(),
            FlushPolicy {
                debounce: Duration::from_secs(60),
                force_every_n_ops: NonZeroUsize::new(8).unwrap(),
            },
        );
        let (snapshot, captured) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let source = Arc::new(Source {
            dirty: AtomicBool::new(false),
            current: AtomicUsize::new(100),
            path: path.clone(),
            snapshot,
            release: Mutex::new(released),
        });
        hub.register(Arc::downgrade(&source) as Weak<dyn Flushable>);

        let eager = kithara_platform::thread::spawn_named("eager-byte-flush", {
            let source = Arc::clone(&source);
            move || flush_sync(&source, None).unwrap()
        });
        captured.recv().unwrap();
        source.current.store(200, Ordering::Release);
        source.dirty.store(true, Ordering::Release);
        hub.signal();
        release.send(()).unwrap();
        eager.join().unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            100_usize.to_le_bytes(),
            "the eager write published the snapshot captured before the concurrent update"
        );

        hub.flush_now().unwrap();
        drop(source);
        drop(hub);

        assert_eq!(
            fs::read(&path).unwrap(),
            200_usize.to_le_bytes(),
            "a checkpoint and restart must retain the update made during eager I/O"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn flush_now_drains_dirty_sources() {
        let hub = FlushHub::new(CancelToken::never(), fast_policy());
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
    fn durable_checkpoint_supersedes_a_stale_worker_snapshot() {
        struct Source {
            dirty: AtomicBool,
            current: AtomicUsize,
            persisted: AtomicUsize,
            durable: AtomicBool,
            snapshot: Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
            flushed: Sender<()>,
        }
        impl Flushable for Source {
            fn dirty(&self) -> &AtomicBool {
                &self.dirty
            }
            fn flush(&self) -> AssetsResult<()> {
                let snapshot = self.current.load(Ordering::Acquire);
                self.snapshot.send(()).unwrap();
                self.release.lock().recv().unwrap();
                self.persisted.store(snapshot, Ordering::Release);
                self.flushed.send(()).unwrap();
                Ok(())
            }
            fn flush_durable(&self) -> AssetsResult<()> {
                self.persisted
                    .store(self.current.load(Ordering::Acquire), Ordering::Release);
                self.durable.store(true, Ordering::Release);
                Ok(())
            }
            fn name(&self) -> &'static str {
                "stale-worker"
            }
        }

        let hub = FlushHub::new(CancelToken::never(), fast_policy());
        let (snapshot, captured) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (flushed, completion) = mpsc::channel();
        let source = Arc::new(Source {
            dirty: AtomicBool::new(true),
            current: AtomicUsize::new(1),
            persisted: AtomicUsize::new(0),
            durable: AtomicBool::new(false),
            snapshot,
            release: Mutex::new(released),
            flushed,
        });
        hub.register(Arc::downgrade(&source) as Weak<dyn Flushable>);
        hub.signal();
        captured.recv().unwrap();
        source.current.store(2, Ordering::Release);
        release.send(()).unwrap();
        completion.recv().unwrap();
        assert_eq!(source.persisted.load(Ordering::Acquire), 1);
        assert!(!source.dirty.load(Ordering::Acquire));

        hub.flush_now().unwrap();

        assert_eq!(source.persisted.load(Ordering::Acquire), 2);
        assert!(source.durable.load(Ordering::Acquire));
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn checkpoint_durably_rewrites_a_completed_eager_publication() {
        let hub = FlushHub::new(CancelToken::never(), fast_policy());
        let src = CountingSource::new("eager-durability");
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);

        flush_sync(&src, Some(&hub)).unwrap();
        assert_eq!(src.flush_count(), 1);
        assert_eq!(src.durable_flush_count(), 0);

        hub.flush_now().unwrap();
        assert_eq!(
            src.durable_flush_count(),
            1,
            "checkpoint must supersede a completed eager best-effort publication"
        );

        hub.flush_now().unwrap();
        assert_eq!(
            src.durable_flush_count(),
            1,
            "the durable checkpoint leaves a clean source"
        );
    }

    #[kithara::test(flash(false), timeout(Duration::from_secs(5)))]
    fn checkpoint_cannot_discharge_a_newer_eager_publication() {
        use std::{fs, path::PathBuf};

        struct Source {
            dirty: AtomicBool,
            current: AtomicUsize,
            path: PathBuf,
            publication: Mutex<()>,
            durable_calls: AtomicUsize,
            first_durable_started: Sender<()>,
            first_durable_release: Mutex<mpsc::Receiver<()>>,
        }

        impl Flushable for Source {
            fn dirty(&self) -> &AtomicBool {
                &self.dirty
            }

            fn flush(&self) -> AssetsResult<()> {
                let _publication = self.publication.lock();
                fs::write(
                    &self.path,
                    self.current.load(Ordering::Acquire).to_le_bytes(),
                )?;
                Ok(())
            }

            fn flush_durable(&self) -> AssetsResult<()> {
                let _publication = self.publication.lock();
                let call = self.durable_calls.fetch_add(1, Ordering::AcqRel);
                if call == 0 {
                    self.first_durable_started.send(()).unwrap();
                    self.first_durable_release.lock().recv().unwrap();
                }
                fs::write(
                    &self.path,
                    self.current.load(Ordering::Acquire).to_le_bytes(),
                )?;
                Ok(())
            }

            fn name(&self) -> &'static str {
                "checkpoint-discharge"
            }
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("index.bin");
        let hub = FlushHub::new(
            CancelToken::never(),
            FlushPolicy {
                debounce: Duration::from_secs(60),
                force_every_n_ops: NonZeroUsize::new(8).unwrap(),
            },
        );
        let (eager_call, eager_called) = mpsc::channel();
        let (first_durable_started, first_checkpoint_started) = mpsc::channel();
        let (first_durable_release, release_first) = mpsc::channel();
        let source = Arc::new(Source {
            dirty: AtomicBool::new(false),
            current: AtomicUsize::new(1),
            path: path.clone(),
            publication: Mutex::new(()),
            durable_calls: AtomicUsize::new(0),
            first_durable_started,
            first_durable_release: Mutex::new(release_first),
        });
        hub.register(Arc::downgrade(&source) as Weak<dyn Flushable>);
        flush_sync(&source, Some(&hub)).unwrap();

        let first_checkpoint = kithara_platform::thread::spawn_named("discharge-checkpoint", {
            let hub = Arc::clone(&hub);
            move || hub.flush_now().unwrap()
        });
        first_checkpoint_started.recv().unwrap();
        source.current.store(2, Ordering::Release);

        let eager = kithara_platform::thread::spawn_named("newer-eager", {
            let source = Arc::clone(&source);
            let hub = Arc::clone(&hub);
            move || {
                eager_call.send(()).unwrap();
                flush_sync(&source, Some(&hub)).unwrap();
            }
        });
        eager_called.recv().unwrap();
        first_durable_release.send(()).unwrap();
        eager.join().unwrap();
        first_checkpoint.join().unwrap();

        hub.flush_now().unwrap();

        assert_eq!(
            source.durable_calls.load(Ordering::Acquire),
            2,
            "the checkpoint after a newer eager publication must sync that publication"
        );
        assert_eq!(fs::read(&path).unwrap(), 2_usize.to_le_bytes());
    }

    #[kithara::test(flash(false), timeout(Duration::from_secs(5)))]
    fn checkpoint_selection_is_serialized_with_the_eager_claim() {
        use std::{fs, path::PathBuf};

        struct Source {
            dirty: AtomicBool,
            dirty_calls: AtomicUsize,
            checkpoint_claimed: Sender<()>,
            checkpoint_resume: Mutex<mpsc::Receiver<()>>,
            current: AtomicUsize,
            path: PathBuf,
            publication: Mutex<()>,
            eager_release: Mutex<mpsc::Receiver<()>>,
            durable: AtomicBool,
        }

        impl Flushable for Source {
            fn dirty(&self) -> &AtomicBool {
                if self.dirty_calls.fetch_add(1, Ordering::AcqRel) == 0 {
                    self.checkpoint_claimed.send(()).unwrap();
                    self.checkpoint_resume.lock().recv().unwrap();
                }
                &self.dirty
            }

            fn flush(&self) -> AssetsResult<()> {
                let _publication = self.publication.lock();
                self.eager_release.lock().recv().unwrap();
                fs::write(
                    &self.path,
                    self.current.load(Ordering::Acquire).to_le_bytes(),
                )?;
                Ok(())
            }

            fn flush_durable(&self) -> AssetsResult<()> {
                let _publication = self.publication.lock();
                fs::write(
                    &self.path,
                    self.current.load(Ordering::Acquire).to_le_bytes(),
                )?;
                self.durable.store(true, Ordering::Release);
                Ok(())
            }

            fn name(&self) -> &'static str {
                "checkpoint-selection"
            }
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("index.bin");
        fs::write(&path, 100_usize.to_le_bytes()).unwrap();
        let hub = FlushHub::new(
            CancelToken::never(),
            FlushPolicy {
                debounce: Duration::from_secs(60),
                force_every_n_ops: NonZeroUsize::new(8).unwrap(),
            },
        );
        let (checkpoint_claimed, claimed) = mpsc::channel();
        let (checkpoint_resume, resume_checkpoint) = mpsc::channel();
        let (eager_call, eager_called) = mpsc::channel();
        let (eager_release, release_eager) = mpsc::channel();
        let (checkpoint_returned, returned) = mpsc::channel();
        let source = Arc::new(Source {
            dirty: AtomicBool::new(true),
            dirty_calls: AtomicUsize::new(0),
            checkpoint_claimed,
            checkpoint_resume: Mutex::new(resume_checkpoint),
            current: AtomicUsize::new(200),
            path: path.clone(),
            publication: Mutex::new(()),
            eager_release: Mutex::new(release_eager),
            durable: AtomicBool::new(false),
        });
        hub.register(Arc::downgrade(&source) as Weak<dyn Flushable>);

        let checkpoint = kithara_platform::thread::spawn_named("selecting-checkpoint", {
            let hub = Arc::clone(&hub);
            let path = path.clone();
            move || {
                hub.flush_now().unwrap();
                checkpoint_returned.send(fs::read(&path).unwrap()).unwrap();
            }
        });
        claimed.recv().unwrap();

        let eager = kithara_platform::thread::spawn_named("claiming-eager", {
            let source = Arc::clone(&source);
            let hub = Arc::clone(&hub);
            move || {
                eager_call.send(()).unwrap();
                flush_sync(&source, Some(&hub)).unwrap();
            }
        });
        eager_called.recv().unwrap();
        checkpoint_resume.send(()).unwrap();
        let persisted_at_return = returned.recv().unwrap();
        eager_release.send(()).unwrap();
        eager.join().unwrap();
        checkpoint.join().unwrap();

        assert_eq!(
            persisted_at_return,
            200_usize.to_le_bytes(),
            "checkpoint selection and the eager claim must have one order"
        );
        assert!(source.durable.load(Ordering::Acquire));
    }

    #[kithara::test(flash(false), timeout(Duration::from_secs(5)))]
    fn older_pass_cannot_prune_a_new_live_writer() {
        use std::{fs, path::PathBuf};

        use kithara_test_utils::wait_until_blocking;

        struct Source {
            name: &'static str,
            dirty: AtomicBool,
            durable_writes: AtomicUsize,
            path: PathBuf,
        }

        impl Source {
            fn new(name: &'static str, path: PathBuf) -> Arc<Self> {
                Arc::new(Self {
                    name,
                    dirty: AtomicBool::new(false),
                    durable_writes: AtomicUsize::new(0),
                    path,
                })
            }
        }

        impl Flushable for Source {
            fn dirty(&self) -> &AtomicBool {
                &self.dirty
            }

            fn flush(&self) -> AssetsResult<()> {
                fs::write(&self.path, [1])?;
                Ok(())
            }

            fn flush_durable(&self) -> AssetsResult<()> {
                self.durable_writes.fetch_add(1, Ordering::AcqRel);
                fs::write(&self.path, [2])?;
                Ok(())
            }

            fn name(&self) -> &'static str {
                self.name
            }
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let root_x = temp_dir.path().join("x");
        let root_y = temp_dir.path().join("y");
        fs::create_dir_all(&root_x).unwrap();
        fs::create_dir_all(&root_y).unwrap();
        let hub = FlushHub::new(
            CancelToken::never(),
            FlushPolicy {
                debounce: Duration::from_secs(60),
                force_every_n_ops: NonZeroUsize::new(8).unwrap(),
            },
        );
        let source_x = Source::new("x", root_x.join("pins.bin"));
        let writer = Source::new("y", root_y.join("pins.bin"));
        hub.register(Arc::downgrade(&source_x) as Weak<dyn Flushable>);
        assert_eq!(Arc::strong_count(&source_x), 1);

        let writer_key = Arc::as_ptr(&writer).cast::<()>().addr();
        let shard_count =
            (std::thread::available_parallelism().map_or(1, usize::from) * 4).next_power_of_two();
        let shift = usize::BITS as usize - shard_count.trailing_zeros() as usize;
        let shard = |key| (hub.non_durable.hash_usize(&key) << 7) >> shift;
        let guard_key = (0..)
            .find(|key| shard(*key) < shard(writer_key))
            .expect("an earlier non_durable shard");
        hub.non_durable.insert(guard_key);
        let prune_guard = hub.non_durable.get(&guard_key).unwrap();

        let older_pass = kithara_platform::thread::spawn_named("older-prune", {
            let hub = Arc::clone(&hub);
            move || {
                let _hub_guard = hub.flush_lock.lock();
                hub.flush_dirty(false).unwrap();
            }
        });
        wait_until_blocking(Duration::from_secs(1), "older pass captured X", || {
            Arc::strong_count(&source_x) == 2
        })
        .unwrap();

        hub.register(Arc::downgrade(&writer) as Weak<dyn Flushable>);
        let (eager_call, eager_called) = mpsc::channel();
        let (eager_done, eager_completed) = mpsc::channel();
        let eager = kithara_platform::thread::spawn_named("new-live-writer", {
            let writer = Arc::clone(&writer);
            let hub = Arc::clone(&hub);
            move || {
                eager_call.send(()).unwrap();
                flush_sync(&writer, Some(&hub)).unwrap();
                eager_done.send(()).unwrap();
            }
        });
        eager_called.recv().unwrap();
        drop(prune_guard);
        eager_completed.recv().unwrap();
        eager.join().unwrap();
        older_pass.join().unwrap();

        assert!(
            hub.non_durable.contains(&writer_key),
            "the newer eager obligation must survive the older pass"
        );
        assert!(!writer.dirty.load(Ordering::Acquire));
        hub.flush_now().unwrap();

        assert_eq!(
            writer.durable_writes.load(Ordering::Acquire),
            1,
            "checkpoint must cover the newly registered live writer's eager pin"
        );
        assert_eq!(fs::read(&writer.path).unwrap(), vec![2]);
        drop(writer);
    }

    #[kithara::test(flash(false), timeout(Duration::from_secs(5)))]
    fn checkpoint_waits_for_eager_publication_and_reopens_the_latest_bytes() {
        use std::{fs, path::PathBuf};

        enum Event {
            CheckpointStarted,
            CheckpointReturned(Vec<u8>),
        }

        struct Source {
            dirty: AtomicBool,
            current: AtomicUsize,
            path: PathBuf,
            snapshot: Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
            publication: Mutex<()>,
            durable: AtomicBool,
        }

        impl Flushable for Source {
            fn dirty(&self) -> &AtomicBool {
                &self.dirty
            }

            fn flush(&self) -> AssetsResult<()> {
                let _publication = self.publication.lock();
                let snapshot = self.current.load(Ordering::Acquire);
                self.snapshot.send(()).unwrap();
                self.release.lock().recv().unwrap();
                fs::write(&self.path, snapshot.to_le_bytes())?;
                Ok(())
            }

            fn flush_durable(&self) -> AssetsResult<()> {
                let _publication = self.publication.lock();
                fs::write(
                    &self.path,
                    self.current.load(Ordering::Acquire).to_le_bytes(),
                )?;
                self.durable.store(true, Ordering::Release);
                Ok(())
            }

            fn name(&self) -> &'static str {
                "inflight-eager-checkpoint"
            }
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("index.bin");
        fs::write(&path, 100_usize.to_le_bytes()).unwrap();
        let hub = FlushHub::new(
            CancelToken::never(),
            FlushPolicy {
                debounce: Duration::from_secs(60),
                force_every_n_ops: NonZeroUsize::new(8).unwrap(),
            },
        );
        let (snapshot, captured) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let source = Arc::new(Source {
            dirty: AtomicBool::new(true),
            current: AtomicUsize::new(200),
            path: path.clone(),
            snapshot,
            release: Mutex::new(released),
            publication: Mutex::new(()),
            durable: AtomicBool::new(false),
        });
        hub.register(Arc::downgrade(&source) as Weak<dyn Flushable>);

        let eager = kithara_platform::thread::spawn_named("inflight-eager", {
            let source = Arc::clone(&source);
            let hub = Arc::clone(&hub);
            move || flush_sync(&source, Some(&hub)).unwrap()
        });
        captured.recv().unwrap();

        let checkpoint = kithara_platform::thread::spawn_named("eager-checkpoint", {
            let hub = Arc::clone(&hub);
            let path = path.clone();
            move || {
                event_tx.send(Event::CheckpointStarted).unwrap();
                hub.flush_now().unwrap();
                let persisted = fs::read(&path).unwrap();
                event_tx.send(Event::CheckpointReturned(persisted)).unwrap();
            }
        });
        match event_rx.recv().unwrap() {
            Event::CheckpointStarted => {}
            Event::CheckpointReturned(_) => panic!("checkpoint returned before it was released"),
        }
        release.send(()).unwrap();
        let persisted_at_return = match event_rx.recv().unwrap() {
            Event::CheckpointReturned(persisted) => persisted,
            Event::CheckpointStarted => panic!("checkpoint started twice"),
        };
        eager.join().unwrap();
        checkpoint.join().unwrap();

        assert_eq!(
            persisted_at_return,
            200_usize.to_le_bytes(),
            "checkpoint must not return while an earlier eager claim is still unpublished"
        );
        assert!(
            source.durable.load(Ordering::Acquire),
            "the completed checkpoint used its durable publication path"
        );
        drop(source);
        drop(hub);
        assert_eq!(fs::read(&path).unwrap(), 200_usize.to_le_bytes());
    }

    #[kithara::test(timeout(Duration::from_secs(2)))]
    fn flush_sync_uses_non_durable_variant() {
        let src = CountingSource::new("src");

        flush_sync(&src, None).unwrap();
        assert_eq!(src.flush_count(), 1, "eager mutator path → non-durable");
        assert_eq!(
            src.durable_flush_count(),
            0,
            "eager mutator path must NOT pay the sync_data fence"
        );
    }

    /// The worker leaves its coalescing window either when the debounce
    /// elapses or when the burst reaches `force_every_n_ops`. Sizing the
    /// burst to the cap and putting the debounce beyond the test timeout
    /// leaves the cap as the only exit, so every signal provably shares
    /// one window.
    ///
    /// The clock has to be real. Under flash the parked worker is the
    /// only quiescence participant, so the engine jumps the virtual clock
    /// to the debounce deadline the moment the worker parks: the debounce
    /// closes the window instead of the cap, and each signal buys its own
    /// flush.
    #[kithara::test(flash(false), timeout(Duration::from_secs(5)))]
    fn worker_coalesces_burst_into_single_flush() {
        const BURST: usize = 5;

        let hub = FlushHub::new(
            CancelToken::never(),
            FlushPolicy {
                debounce: Duration::from_secs(10),
                force_every_n_ops: NonZeroUsize::new(BURST).unwrap(),
            },
        );
        let (flushed_tx, flushed_rx) = mpsc::channel();
        let src = CountingSource::with_completion("burst", flushed_tx);
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);

        for _ in 0..BURST {
            src.dirty.store(true, Ordering::Release);
            hub.signal();
        }

        flushed_rx.recv().unwrap();
        assert_eq!(
            src.flush_count(),
            1,
            "{BURST} signals must coalesce into one flush",
        );
        assert_eq!(flushed_rx.try_recv(), Err(TryRecvError::Empty));
    }

    /// The other exit from the window, which the burst above no longer
    /// reaches: a lone signal waits the debounce out. Waiting on the
    /// flush itself keeps the window length out of the claim.
    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn worker_flushes_once_the_debounce_window_elapses() {
        let hub = FlushHub::new(CancelToken::never(), fast_policy());
        let (flushed_tx, flushed_rx) = mpsc::channel();
        let src = CountingSource::with_completion("debounced", flushed_tx);
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);

        src.dirty.store(true, Ordering::Release);
        hub.signal();

        flushed_rx.recv().unwrap();
        assert_eq!(
            src.flush_count(),
            1,
            "a debounced signal ends in exactly one flush"
        );
        assert_eq!(flushed_rx.try_recv(), Err(TryRecvError::Empty));
    }

    /// A burst that overshoots the cap still buys exactly one flush, and
    /// the debounce cannot be what released it: ten seconds outlive the
    /// test. The surplus signals land in the window the cap already
    /// closed or open a fresh one that the debounce holds shut, and
    /// neither reaches the source twice. Real clock, for the reason
    /// spelled out above `worker_coalesces_burst_into_single_flush`.
    #[kithara::test(flash(false), timeout(Duration::from_secs(5)))]
    fn force_every_n_ops_bypasses_debounce() {
        let policy = FlushPolicy {
            debounce: Duration::from_secs(10),
            force_every_n_ops: NonZeroUsize::new(4).unwrap(),
        };
        let hub = FlushHub::new(CancelToken::never(), policy);
        let (flushed_tx, flushed_rx) = mpsc::channel();
        let src = CountingSource::with_completion("force", flushed_tx);
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);

        for _ in 0..6 {
            src.dirty.store(true, Ordering::Release);
            hub.signal();
        }

        flushed_rx.recv().unwrap();
        assert_eq!(
            src.flush_count(),
            1,
            "the cap closes the window; the surplus does not add a flush",
        );
        assert_eq!(flushed_rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn cancel_triggers_final_flush() {
        let cancel = CancelToken::never();
        let hub = FlushHub::new(cancel.clone(), fast_policy());
        let (flushed_tx, flushed_rx) = mpsc::channel();
        let src = CountingSource::with_completion("final", flushed_tx);
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);

        src.dirty.store(true, Ordering::Release);
        cancel.cancel();
        flushed_rx.recv().unwrap();
        drop(hub);

        assert_eq!(
            src.flush_count(),
            1,
            "cancel must trigger exactly one final flush"
        );
        assert_eq!(flushed_rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[kithara::test(timeout(Duration::from_secs(5)))]
    fn dropped_source_is_gc_d_on_next_cycle() {
        let hub = FlushHub::new(CancelToken::never(), fast_policy());
        let src = CountingSource::new("ephemeral");
        hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);
        assert_eq!(
            hub.sources
                .lock()
                .iter()
                .filter(|source| source.strong_count() > 0)
                .count(),
            1
        );
        drop(src);

        hub.flush_now().unwrap();

        assert_eq!(
            hub.sources
                .lock()
                .iter()
                .filter(|source| source.strong_count() > 0)
                .count(),
            0,
            "dropped sources must be GC'd from registry"
        );
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
        let hub = FlushHub::new(CancelToken::never(), fast_policy());
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
        let hub = FlushHub::new(CancelToken::never(), fast_policy());
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
    fn repeated_hub_drop_cancels_cleanly() {
        for _ in 0..20 {
            let hub = FlushHub::new(CancelToken::never(), fast_policy());
            let src = CountingSource::new("churn");
            hub.register(Arc::downgrade(&src) as Weak<dyn Flushable>);
            for _ in 0..3 {
                src.dirty.store(true, Ordering::Release);
                hub.signal();
            }
        }
    }

    #[kithara::test(native, tokio, timeout(Duration::from_secs(2)))]
    async fn dropping_the_last_hub_does_not_block_an_async_poll() {
        let _mode = kithara_platform::no_block::force_panic_mode();
        let hub = FlushHub::new(CancelToken::never(), fast_policy());
        let source = CountingSource::new("async-drop");
        hub.register(Arc::downgrade(&source) as Weak<dyn Flushable>);
        let weak = Arc::downgrade(&hub);

        drop(hub);

        assert!(
            weak.upgrade().is_none(),
            "the final external owner was dropped"
        );
        assert_eq!(
            source.flush_count(),
            0,
            "dropping a clean hub writes nothing"
        );
    }
}
