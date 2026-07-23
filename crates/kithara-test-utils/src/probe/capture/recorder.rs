use std::sync::{Mutex, OnceLock, PoisonError};

use kithara_platform::{
    sync::Arc,
    thread::sleep as thread_sleep,
    time::{Duration, Instant, sleep},
};

use super::event::ProbeEvent;

pub(super) type SharedLog = Arc<Mutex<Vec<ProbeEvent>>>;

static GLOBAL_LOG: OnceLock<SharedLog> = OnceLock::new();

/// Lazily allocate (or fetch) the process-wide probe log. Called by
/// [`test::init_tracing`](crate::test::init_tracing) when it
/// constructs the global subscriber, and by [`install`] when a test
/// requests a recorder. Never sets a global subscriber by itself.
pub(crate) fn shared_log() -> SharedLog {
    GLOBAL_LOG
        .get_or_init(|| Arc::new(Mutex::new(Vec::new())))
        .clone()
}

/// Snapshot handle anchored at the moment of installation.
///
/// Cross-test isolation relies on the **install-id task-local** that
/// `#[kithara::test]` enters before the test body runs: every probe
/// firing stamps the owning `install_id` into its tracing event, and
/// this recorder filters by that id. Tasks spawned via `tokio::spawn`
/// inside the test inherit the task-local automatically; orphan
/// tasks from a just-finished test (downloader on-complete, audio
/// worker draining its last buffer) carry the *previous* test's id
/// and fall out of the new recorder's snapshot — even though they
/// emit after the new test's `install()`.
///
/// The global log is intentionally not drained: under stress runs
/// with multiple recorder lifetimes, draining could wipe an
/// actively-recorded sibling's events. The id filter alone is
/// sufficient. The timestamp filter (`start_at`) handles the small
/// window between scope entry and recorder construction.
#[must_use]
pub fn install() -> Recorder {
    Recorder {
        log: shared_log(),
        start_at: Instant::now(),
        install_id: crate::probe::current_install_id(),
    }
}

/// Per-test snapshot handle.
#[derive(Clone, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub struct Recorder {
    #[field(get, copy)]
    start_at: Instant,
    log: SharedLog,
    install_id: u64,
}

impl Recorder {
    /// Filter snapshot by `probe = ...` discriminator.
    #[must_use]
    pub fn events_with_probe(&self, name: &str) -> Vec<ProbeEvent> {
        self.snapshot()
            .into_iter()
            .filter(|e| e.probe_name() == Some(name))
            .collect()
    }

    /// All events recorded since this `Recorder` was created.
    ///
    /// Filters by `install_id` first (excludes orphan-task events from
    /// prior tests that outlive their `Drop`) and `start_at` second
    /// (excludes anything emitted before the recorder was anchored —
    /// possible if the install-id counter was bumped before drain
    /// completed).
    #[must_use]
    pub fn snapshot(&self) -> Vec<ProbeEvent> {
        let log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        log.iter()
            .filter(|e| e.u64("install_id") == Some(self.install_id) && e.at >= self.start_at)
            .cloned()
            .collect()
    }

    /// Block until a probe event matches `predicate` or `budget` elapses.
    ///
    /// Tests should drive their progress through this, not through
    /// time-based polling loops on `Audio::read` / `Stream::len()`.
    /// Each `#[kithara::probe]` site emits a `seq`-stamped event the
    /// moment the production code reaches it; `wait_for_probe` makes
    /// that arrival the test's clock instead of wall time.
    ///
    /// Returns the first event recorded by this recorder for which
    /// `predicate` returns `true`. Includes events that arrived *before*
    /// this call (so a probe that fires before the first poll isn't lost
    /// to a race). Returns `None` only when `budget` elapsed without a
    /// match.
    ///
    /// `budget` should be a real budget — fail the test if it elapses
    /// rather than masking a hang. Polls every 5 ms inside the budget.
    pub fn wait_for_probe<F>(&self, predicate: F, budget: Duration) -> Option<ProbeEvent>
    where
        F: Fn(&ProbeEvent) -> bool,
    {
        let deadline = Instant::now() + budget;
        loop {
            if let Some(evt) = self.snapshot().into_iter().find(|e| predicate(e)) {
                return Some(evt);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread_sleep(Duration::from_millis(5));
        }
    }

    /// Async variant of [`Self::wait_for_probe`].
    ///
    /// Same semantics — but yields the runtime via `kithara_platform::time::sleep`
    /// instead of `std::thread::sleep`. Required for tests that drive
    /// async work between probe polls (HTTP fetches, peer schedulers,
    /// reader tasks) on a `current_thread` runtime: the blocking variant
    /// starves the runtime, preventing those tasks from progressing.
    pub async fn wait_for_probe_async<F>(
        &self,
        predicate: F,
        budget: Duration,
    ) -> Option<ProbeEvent>
    where
        F: Fn(&ProbeEvent) -> bool,
    {
        let deadline = Instant::now() + budget;
        loop {
            if let Some(evt) = self.snapshot().into_iter().find(|e| predicate(e)) {
                return Some(evt);
            }
            if Instant::now() >= deadline {
                return None;
            }
            sleep(Duration::from_millis(5)).await;
        }
    }
}
