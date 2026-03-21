#![forbid(unsafe_code)]

//! Generic backend for streaming sources.
//!
//! Backend spawns a Downloader task and orchestrates:
//! - Backpressure (`should_throttle` + `wait_ready`)
//! - On-demand requests (`poll_demand`, bypasses backpressure)
//! - Batch parallelism (plan → io.fetch in parallel → commit sequentially)
//! - Streaming steps (plan → step → commit)
//! - Periodic yield to async runtime
//! - Cancellation via `CancellationToken`

use std::sync::atomic::{AtomicU64, Ordering};

use futures::{StreamExt, stream::FuturesUnordered};
use kithara_platform::{
    JoinHandle, MaybeSend,
    thread::{spawn_named, yield_now},
    tokio,
};
#[cfg(target_arch = "wasm32")]
use tokio::task::spawn as task_spawn;
use tokio::task::yield_now as task_yield_now;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::downloader::{Downloader, DownloaderIo, PlanOutcome, StepResult};

/// Default yield interval (iterations between `yield_now` calls).
const DEFAULT_YIELD_INTERVAL: usize = 8;

/// Monotonic counter for unique download-worker thread names.
static DOWNLOAD_WORKER_ID: AtomicU64 = AtomicU64::new(0);

enum LoopControl {
    Continue { made_progress: bool },
    Exit,
    Restart,
}

/// Handle to the downloader worker — either an OS thread or an async task.
#[cfg(not(target_arch = "wasm32"))]
enum WorkerHandle {
    /// Dedicated OS thread (fallback for current-thread runtimes).
    Thread(JoinHandle<()>),
    /// Async task on a shared runtime (zero extra OS threads).
    Task(tokio::task::JoinHandle<()>),
}

/// Spawns and owns a Downloader task.
///
/// The downloader runs independently (async, writing data to storage).
/// Source is owned by Reader and accessed directly (sync).
///
/// Backend manages the downloader lifecycle:
/// - On creation, spawns the downloader on a dedicated thread.
/// - On drop, cancels the child token, causing the downloader loop to exit.
///
/// Store the Backend alongside the Source to ensure the downloader is
/// stopped when the stream is destroyed.
pub struct Backend {
    /// Child token created from the caller's cancel token.
    /// Cancelled on drop to stop the downloader.
    cancel: CancellationToken,

    /// Worker handle (joined/aborted on drop after cancellation).
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<WorkerHandle>,

    #[cfg(target_arch = "wasm32")]
    worker: Option<()>,
}

impl Backend {
    /// Spawn a downloader task.
    ///
    /// The downloader runs in the background writing data to storage.
    /// A child cancellation token is created: dropping this Backend
    /// cancels the child (and thus the downloader) without affecting
    /// the parent token.
    ///
    /// # Runtime resolution
    ///
    /// 1. `runtime` is `Some(handle)` → async task on that handle (0 OS threads)
    /// 2. `runtime` is `None` + current runtime is multi-thread → async task (0 OS threads)
    /// 3. Otherwise → dedicated OS thread with its own current-thread runtime
    ///
    /// # Panics
    ///
    /// Panics if creating the dedicated current-thread Tokio runtime fails
    /// (only in fallback path 3).
    pub fn new<D: Downloader + Send>(
        downloader: D,
        cancel: &CancellationToken,
        runtime: Option<tokio::runtime::Handle>,
    ) -> Self {
        let child_cancel = cancel.child_token();
        let task_cancel = child_cancel.clone();

        #[cfg(not(target_arch = "wasm32"))]
        let worker = Some(Self::spawn_downloader(downloader, task_cancel, runtime));

        #[cfg(target_arch = "wasm32")]
        let worker = {
            let _ = runtime;
            // On WASM, spawn the downloader as an async task on the current
            // Worker's tokio runtime instead of creating a dedicated Web Worker.
            task_spawn(Self::run_downloader(downloader, task_cancel));
            None
        };

        Self {
            cancel: child_cancel,
            worker,
        }
    }

    /// Resolve the best spawn strategy and start the downloader.
    ///
    /// Resolution order:
    /// 1. Explicit handle → reuse via `block_on` on a helper thread (no extra runtime)
    /// 2. Current multi-thread runtime → reuse its handle (no extra runtime)
    /// 3. Current-thread runtime or none → dedicated thread + own runtime (fallback)
    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_downloader<D: Downloader + Send>(
        downloader: D,
        cancel: CancellationToken,
        runtime: Option<tokio::runtime::Handle>,
    ) -> WorkerHandle {
        // Try to find a usable multi-thread handle: explicit > current > none.
        let shared_handle = runtime.or_else(|| {
            tokio::runtime::Handle::try_current().ok().filter(|h| {
                !matches!(
                    h.runtime_flavor(),
                    tokio::runtime::RuntimeFlavor::CurrentThread
                )
            })
        });

        if let Some(handle) = shared_handle {
            // Spawn as async task — zero extra OS threads.
            debug!("Backend: spawning downloader as async task on shared runtime");
            return WorkerHandle::Task(handle.spawn(Self::run_downloader(downloader, cancel)));
        }

        // Fallback: no multi-thread runtime available — dedicated thread
        // with its own current-thread runtime.
        debug!("Backend: fallback — creating dedicated current-thread runtime");
        let id = DOWNLOAD_WORKER_ID.fetch_add(1, Ordering::Relaxed);
        WorkerHandle::Thread(spawn_named(
            format!("kithara-download-worker-{id}"),
            move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build downloader runtime");
                rt.block_on(Self::run_downloader(downloader, cancel));
            },
        ))
    }

    #[kithara_hang_detector::hang_watchdog]
    async fn run_downloader<D: Downloader + MaybeSend>(mut dl: D, cancel: CancellationToken) {
        debug!("Downloader task started");
        let yield_interval = DEFAULT_YIELD_INTERVAL;
        let mut steps_since_yield: usize = 0;
        loop {
            if cancel.is_cancelled() {
                debug!("Downloader cancelled");
                return;
            }

            // Drain demand BEFORE throttle check: a demand request may
            // reset the download cursor (reset_for_seek_epoch), which
            // relieves segment-based throttle pressure. Without this,
            // handle_idle can consume the Notify permit, and the
            // subsequent wait_while_throttled blocks forever because
            // the demand (still in the queue) would have un-throttled
            // the downloader.
            let Ok(mut made_progress) = Self::drain_demand_requests(&mut dl, &cancel).await else {
                return;
            };
            let Ok(throttle_progress) = Self::wait_while_throttled(&mut dl, &cancel).await else {
                return;
            };
            let Ok(outcome) = Self::plan_next(&mut dl, &cancel).await else {
                return;
            };

            hang_tick!();
            yield_now();
            made_progress |= throttle_progress;

            let control = match outcome {
                PlanOutcome::Batch(plans) => Self::handle_batch(&mut dl, &cancel, plans).await,
                PlanOutcome::Step => Self::handle_step(&mut dl, &cancel).await,
                PlanOutcome::Complete => {
                    debug!("Downloader complete");
                    LoopControl::Exit
                }
                PlanOutcome::Idle => Self::handle_idle(&mut dl, &cancel).await,
            };

            match control {
                LoopControl::Exit => return,
                LoopControl::Restart => {
                    if made_progress {
                        hang_reset!();
                        steps_since_yield += 1;
                    } else {
                        steps_since_yield = 0;
                    }
                    continue;
                }
                LoopControl::Continue {
                    made_progress: loop_progress,
                } => {
                    made_progress |= loop_progress;
                }
            }

            if made_progress {
                hang_reset!();
                steps_since_yield += 1;
            } else {
                steps_since_yield = 0;
            }

            // 4. Periodic yield (only when not throttled — backpressure loop
            //    already yields to runtime via wait_ready).
            if made_progress && !dl.should_throttle() && steps_since_yield >= yield_interval {
                task_yield_now().await;
                steps_since_yield = 0;
            }
        }
    }

    async fn wait_while_throttled<D: Downloader + MaybeSend>(
        dl: &mut D,
        cancel: &CancellationToken,
    ) -> Result<bool, ()> {
        let mut made_progress = false;
        while dl.should_throttle() {
            // Check demand BEFORE blocking: a demand request may have arrived
            // between drain_demand_requests() and entering this loop, or
            // between the previous wait_ready() wake and re-checking throttle.
            // Processing it can reset the download cursor and relieve throttle.
            if let Some(plan) = Self::try_poll_demand(dl, cancel).await
                && Self::fetch_and_commit_demand(dl, plan, cancel)
                    .await
                    .map(|progress| {
                        made_progress |= progress;
                    })
                    .is_err()
            {
                return Err(());
            }
            // Demand processing may have relieved throttle — recheck.
            if !dl.should_throttle() {
                break;
            }

            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    debug!("Downloader cancelled during backpressure");
                    return Err(());
                }
                () = dl.wait_ready() => {}
            }

            if let Some(plan) = Self::try_poll_demand(dl, cancel).await
                && Self::fetch_and_commit_demand(dl, plan, cancel)
                    .await
                    .map(|progress| {
                        made_progress |= progress;
                    })
                    .is_err()
            {
                return Err(());
            }
        }
        Ok(made_progress)
    }

    async fn drain_demand_requests<D: Downloader + MaybeSend>(
        dl: &mut D,
        cancel: &CancellationToken,
    ) -> Result<bool, ()> {
        let mut made_progress = false;
        while let Some(plan) = Self::try_poll_demand(dl, cancel).await {
            if Self::fetch_and_commit_demand(dl, plan, cancel)
                .await
                .map(|progress| {
                    made_progress |= progress;
                })
                .is_err()
            {
                return Err(());
            }
        }
        Ok(made_progress)
    }

    async fn plan_next<D: Downloader + MaybeSend>(
        dl: &mut D,
        cancel: &CancellationToken,
    ) -> Result<PlanOutcome<D::Plan>, ()> {
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                debug!("Downloader cancelled during plan");
                Err(())
            }
            outcome = dl.plan() => Ok(outcome),
        }
    }

    async fn handle_idle<D: Downloader + MaybeSend>(
        dl: &mut D,
        cancel: &CancellationToken,
    ) -> LoopControl {
        tokio::select! {
            biased;
            () = cancel.cancelled() => LoopControl::Exit,
            () = dl.demand_signal() => {
                // Yield to let other tasks run (e.g. TUI updates).
                task_yield_now().await;
                // Completing an idle wait is forward progress: the downloader
                // successfully blocked until woken.  Without this the hang
                // detector fires on legitimate post-download idle (all
                // segments cached, reader still consuming).
                LoopControl::Continue { made_progress: true }
            }
        }
    }

    #[kithara_hang_detector::hang_watchdog]
    async fn handle_batch<D: Downloader + MaybeSend>(
        dl: &mut D,
        cancel: &CancellationToken,
        plans: Vec<D::Plan>,
    ) -> LoopControl {
        if plans.is_empty() {
            task_yield_now().await;
            return LoopControl::Restart;
        }

        let io = dl.io().clone();
        let plan_count = plans.len();
        let mut pending = plans
            .into_iter()
            .enumerate()
            .map(|(index, plan)| {
                let io = io.clone();
                async move { (index, io.fetch(plan).await) }
            })
            .collect::<FuturesUnordered<_>>();

        let mut made_progress = false;
        let mut next_commit = 0usize;
        let mut results: Vec<Option<Result<D::Fetch, D::Error>>> =
            std::iter::repeat_with(|| None).take(plan_count).collect();

        while next_commit < plan_count {
            let Some((index, result)) = (tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    debug!("Downloader cancelled during batch fetch");
                    return LoopControl::Exit;
                }
                item = pending.next() => item,
            }) else {
                break;
            };
            results[index] = Some(result);
            hang_reset!();

            while next_commit < plan_count {
                let Some(result) = results[next_commit].take() else {
                    break;
                };

                match result {
                    Ok(fetch) => {
                        dl.commit(fetch);
                        made_progress = true;
                    }
                    Err(e) => debug!(?e, "batch fetch error"),
                }
                next_commit += 1;

                // NOTE: demand processing is intentionally NOT done here.
                // Fetching a demand segment inside handle_batch can deadlock
                // when the demand targets a segment still pending in the batch's
                // FuturesUnordered — both share the same OnceCell, and the
                // demand's get_or_try_init blocks waiting for the batch future
                // that is not being polled.
                //
                // Demand is processed at the top of the main loop via
                // drain_demand_requests() and during wait_while_throttled().
            }
        }

        LoopControl::Continue { made_progress }
    }

    async fn handle_step<D: Downloader + MaybeSend>(
        dl: &mut D,
        cancel: &CancellationToken,
    ) -> LoopControl {
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                debug!("Downloader cancelled during step");
                return LoopControl::Exit;
            }
            () = dl.demand_signal() => {
                return LoopControl::Restart;
            }
            result = dl.step() => result,
        };

        match result {
            Ok(StepResult::Item(fetch)) => {
                dl.commit(fetch);
                LoopControl::Continue {
                    made_progress: true,
                }
            }
            Ok(StepResult::PhaseChange) => LoopControl::Restart,
            Err(e) => {
                debug!(?e, "step error");
                LoopControl::Exit
            }
        }
    }

    /// Non-blocking demand poll: returns `Some(plan)` if demand is available,
    /// `None` if not (without waiting).
    async fn try_poll_demand<D: Downloader + MaybeSend>(
        dl: &mut D,
        cancel: &CancellationToken,
    ) -> Option<D::Plan> {
        // Use select with a ready future to make poll_demand non-blocking
        // when there's no demand. poll_demand implementations should return
        // None immediately when no demand is queued.
        tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            plan = dl.poll_demand() => plan,
        }
    }

    /// Fetch a demand plan via I/O and commit. Returns Err(()) on cancellation
    /// or fatal error that should stop the downloader.
    async fn fetch_and_commit_demand<D: Downloader + MaybeSend>(
        dl: &mut D,
        plan: D::Plan,
        cancel: &CancellationToken,
    ) -> Result<bool, ()> {
        let io = dl.io().clone();
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                debug!("Downloader cancelled during demand fetch");
                return Err(());
            }
            result = io.fetch(plan) => result,
        };

        match result {
            Ok(fetch) => {
                dl.commit(fetch);
                Ok(true)
            }
            Err(e) => {
                debug!(?e, "demand fetch error");
                // Demand errors are not fatal — reader will retry or timeout.
                Ok(false)
            }
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.cancel.cancel();

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(handle) = self.worker.take() {
            match handle {
                WorkerHandle::Thread(join) => {
                    if join.join().is_err() {
                        debug!("Downloader worker thread panicked during shutdown");
                    }
                }
                WorkerHandle::Task(task) => {
                    task.abort();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use std::{error::Error as StdError, fmt, future, sync::Arc, time::Duration};

    use kithara_platform::{BoxFuture, Mutex, tokio::sync::Notify};
    use tokio::time::{sleep as tokio_sleep, timeout as tokio_timeout};
    use tokio_util::sync::CancellationToken;

    use super::*;

    // -- Mock infrastructure --

    #[derive(Debug)]
    struct MockError;

    impl fmt::Display for MockError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "mock error")
        }
    }

    impl StdError for MockError {}

    /// Mock I/O executor that signals when a demand fetch completes.
    #[derive(Clone)]
    struct MockIo {
        demand_fetched: Arc<Notify>,
    }

    impl DownloaderIo for MockIo {
        type Plan = u64;
        type Fetch = u64;
        type Error = MockError;

        fn fetch(&self, plan: u64) -> BoxFuture<'_, Result<u64, MockError>> {
            Box::pin(async move {
                self.demand_fetched.notify_one();
                Ok(plan)
            })
        }
    }

    /// Mock downloader with a slow `step()` that blocks for 30 seconds,
    /// simulating a stalled sequential HTTP stream.
    struct SlowStepDownloader {
        io: MockIo,
        demand_queue: Arc<Mutex<Vec<u64>>>,
        demand_notify: Arc<Notify>,
        step_entered: Arc<Notify>,
    }

    impl Downloader for SlowStepDownloader {
        type Plan = u64;
        type Fetch = u64;
        type Error = MockError;
        type Io = MockIo;

        fn io(&self) -> &MockIo {
            &self.io
        }

        fn poll_demand(&mut self) -> BoxFuture<'_, Option<u64>> {
            Box::pin(async move { self.demand_queue.lock_sync().pop() })
        }

        fn plan(&mut self) -> BoxFuture<'_, PlanOutcome<u64>> {
            Box::pin(async move { PlanOutcome::Step })
        }

        fn step(&mut self) -> BoxFuture<'_, Result<StepResult<u64>, MockError>> {
            Box::pin(async move {
                self.step_entered.notify_one();
                // Simulate stalled HTTP stream — block forever.
                // (test timeout on native prevents actual hang)
                future::pending::<()>().await;
                Ok(StepResult::Item(0))
            })
        }

        fn commit(&mut self, _fetch: u64) {}

        fn should_throttle(&self) -> bool {
            false
        }

        fn wait_ready(&self) -> BoxFuture<'_, ()> {
            Box::pin(future::pending())
        }

        fn demand_signal(&self) -> BoxFuture<'static, ()> {
            let notify = Arc::clone(&self.demand_notify);
            Box::pin(async move {
                notify.notified().await;
            })
        }
    }

    /// Demand (on-demand range request) must be processed promptly even when
    /// `step()` is blocked on a slow sequential HTTP stream.
    ///
    /// Scenario: sequential download stalls (slow server), user seeks forward.
    /// The reader queues a demand. The downloader should process it immediately,
    /// not wait for the current `step()` to finish.
    ///
    /// Timeout: 2s — `step()` blocks for 30s, so if demand waits for `step`,
    /// the test will be killed by rstest timeout.
    #[kithara::test(tokio, browser, timeout(Duration::from_secs(2)))]
    async fn demand_must_not_wait_for_step() {
        let demand_queue = Arc::new(Mutex::new(Vec::new()));
        let demand_notify = Arc::new(Notify::new());
        let step_entered = Arc::new(Notify::new());
        let demand_fetched = Arc::new(Notify::new());

        let downloader = SlowStepDownloader {
            io: MockIo {
                demand_fetched: demand_fetched.clone(),
            },
            demand_queue: demand_queue.clone(),
            demand_notify: demand_notify.clone(),
            step_entered: step_entered.clone(),
        };

        let cancel = CancellationToken::new();
        let _backend = Backend::new(downloader, &cancel, None);

        // Wait for the downloader to enter step() (blocked on slow stream).
        step_entered.notified().await;

        // Queue demand while step() is blocked (simulates reader seek).
        demand_queue.lock_sync().push(42);
        demand_notify.notify_one();

        // Must resolve promptly — if step() blocks demand, the 2s timeout fires.
        demand_fetched.notified().await;
    }

    struct IdleWakeDownloader {
        io: MockIo,
        wake_notify: Arc<Notify>,
    }

    impl Downloader for IdleWakeDownloader {
        type Plan = u64;
        type Fetch = u64;
        type Error = MockError;
        type Io = MockIo;

        fn io(&self) -> &MockIo {
            &self.io
        }

        fn poll_demand(&mut self) -> BoxFuture<'_, Option<u64>> {
            Box::pin(async move { None })
        }

        fn plan(&mut self) -> BoxFuture<'_, PlanOutcome<u64>> {
            Box::pin(async move { PlanOutcome::Idle })
        }

        fn commit(&mut self, _fetch: u64) {}

        fn should_throttle(&self) -> bool {
            false
        }

        fn wait_ready(&self) -> BoxFuture<'_, ()> {
            Box::pin(future::pending())
        }

        fn demand_signal(&self) -> BoxFuture<'static, ()> {
            let notify = Arc::clone(&self.wake_notify);
            Box::pin(async move {
                notify.notified().await;
            })
        }
    }

    #[kithara::test(
        tokio,
        timeout(Duration::from_secs(3)),
        env(KITHARA_HANG_TIMEOUT_SECS = "1")
    )]
    async fn idle_wakeups_without_commits_do_not_trigger_hang_detector() {
        let wake_notify = Arc::new(Notify::new());
        let downloader = IdleWakeDownloader {
            io: MockIo {
                demand_fetched: Arc::new(Notify::new()),
            },
            wake_notify: Arc::clone(&wake_notify),
        };
        let cancel = CancellationToken::new();
        let wake_cancel = cancel.clone();
        let wake_task = tokio::spawn({
            let wake_notify = Arc::clone(&wake_notify);
            async move {
                while !wake_cancel.is_cancelled() {
                    wake_notify.notify_one();
                    tokio_sleep(Duration::from_millis(10)).await;
                }
            }
        });
        let handle = tokio::spawn(Backend::run_downloader(downloader, cancel.clone()));

        // Idle wakeups with successful demand_signal() count as progress,
        // so the hang detector must NOT fire.  Let it run for 1.5× the
        // timeout and verify it stays alive.
        tokio_sleep(Duration::from_millis(1500)).await;
        assert!(
            !handle.is_finished(),
            "downloader should stay alive during idle wakeups"
        );

        cancel.cancel();
        wake_task.abort();
        let _ = handle.await;
    }

    #[derive(Clone)]
    struct BatchProgressIo {
        release_slow: Arc<Notify>,
    }

    impl DownloaderIo for BatchProgressIo {
        type Plan = u64;
        type Fetch = u64;
        type Error = MockError;

        fn fetch(&self, plan: u64) -> BoxFuture<'_, Result<u64, MockError>> {
            Box::pin(async move {
                if plan == 1 {
                    self.release_slow.notified().await;
                }
                Ok(plan)
            })
        }
    }

    struct BatchProgressDownloader {
        commits: Arc<Mutex<Vec<u64>>>,
        first_commit: Arc<Notify>,
        io: BatchProgressIo,
        plan_sent: bool,
    }

    impl Downloader for BatchProgressDownloader {
        type Plan = u64;
        type Fetch = u64;
        type Error = MockError;
        type Io = BatchProgressIo;

        fn io(&self) -> &Self::Io {
            &self.io
        }

        fn poll_demand(&mut self) -> BoxFuture<'_, Option<u64>> {
            Box::pin(async move { None })
        }

        fn plan(&mut self) -> BoxFuture<'_, PlanOutcome<u64>> {
            Box::pin(async move {
                if self.plan_sent {
                    PlanOutcome::Idle
                } else {
                    self.plan_sent = true;
                    PlanOutcome::Batch(vec![0, 1])
                }
            })
        }

        fn commit(&mut self, fetch: u64) {
            self.commits.lock_sync().push(fetch);
            if fetch == 0 {
                self.first_commit.notify_one();
            }
        }

        fn should_throttle(&self) -> bool {
            false
        }

        fn wait_ready(&self) -> BoxFuture<'_, ()> {
            Box::pin(future::pending())
        }

        fn demand_signal(&self) -> BoxFuture<'static, ()> {
            Box::pin(future::pending())
        }
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(2)))]
    async fn batch_commits_ready_prefix_without_waiting_for_slowest_fetch() {
        let release_slow = Arc::new(Notify::new());
        let first_commit = Arc::new(Notify::new());
        let commits = Arc::new(Mutex::new(Vec::new()));
        let downloader = BatchProgressDownloader {
            commits: Arc::clone(&commits),
            first_commit: Arc::clone(&first_commit),
            io: BatchProgressIo {
                release_slow: Arc::clone(&release_slow),
            },
            plan_sent: false,
        };

        let cancel = CancellationToken::new();
        let handle = tokio::spawn(Backend::run_downloader(downloader, cancel.clone()));

        tokio_timeout(Duration::from_millis(250), first_commit.notified())
            .await
            .expect("first batch item must commit before the slowest fetch completes");
        assert_eq!(
            commits.lock_sync().as_slice(),
            &[0],
            "only the ready prefix should be committed before the slow fetch completes"
        );

        cancel.cancel();
        release_slow.notify_one();
        let _ = handle.await;
    }
}
