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

use kithara_platform::ThreadPool;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::downloader::{Downloader, DownloaderIo, PlanOutcome, StepResult};

/// Default yield interval (iterations between `yield_now` calls).
const DEFAULT_YIELD_INTERVAL: usize = 8;

/// Spawns and owns a Downloader task.
///
/// The downloader runs independently (async, writing data to storage).
/// Source is owned by Reader and accessed directly (sync).
///
/// Backend manages the downloader lifecycle:
/// - On creation, spawns the downloader on the provided [`ThreadPool`].
/// - On drop, cancels the child token, causing the downloader loop to exit.
///
/// Store the Backend alongside the Source to ensure the downloader is
/// stopped when the stream is destroyed.
pub struct Backend {
    /// Child token created from the caller's cancel token.
    /// Cancelled on drop to stop the downloader.
    cancel: CancellationToken,
}

impl Backend {
    /// Spawn a downloader task on the given thread pool.
    ///
    /// The downloader runs in the background writing data to storage.
    /// A child cancellation token is created: dropping this Backend
    /// cancels the child (and thus the downloader) without affecting
    /// the parent token.
    pub fn new<D: Downloader>(
        downloader: D,
        cancel: &CancellationToken,
        pool: &ThreadPool,
    ) -> Self {
        let child_cancel = cancel.child_token();
        let task_cancel = child_cancel.clone();

        #[cfg(not(target_arch = "wasm32"))]
        {
            let handle = tokio::runtime::Handle::current();
            pool.spawn(move || {
                handle.block_on(Self::run_downloader(downloader, task_cancel));
            });
        }

        #[cfg(target_arch = "wasm32")]
        {
            let _ = pool; // suppress unused warning
            wasm_bindgen_futures::spawn_local(Self::run_downloader(downloader, task_cancel));
        }

        Self {
            cancel: child_cancel,
        }
    }

    // Core event loop coordinating backpressure, demand, batch/step execution, and
    // periodic yield. Splitting would fragment the control flow and hurt readability.
    #[expect(clippy::cognitive_complexity)]
    async fn run_downloader<D: Downloader>(mut dl: D, cancel: CancellationToken) {
        debug!("Downloader task started");
        let yield_interval = DEFAULT_YIELD_INTERVAL;
        let mut steps_since_yield: usize = 0;

        loop {
            if cancel.is_cancelled() {
                debug!("Downloader cancelled");
                return;
            }

            // 1. Backpressure — wait until reader catches up.
            //    Demand requests bypass throttle to prevent deadlock.
            while dl.should_throttle() {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => {
                        debug!("Downloader cancelled during backpressure");
                        return;
                    }
                    () = dl.wait_ready() => {}
                }

                // Check demand while throttled — demand bypasses backpressure.
                if let Some(plan) = Self::try_poll_demand(&mut dl, &cancel).await
                    && Self::fetch_and_commit_demand(&mut dl, plan, &cancel)
                        .await
                        .is_err()
                {
                    return;
                }
            }

            // 2. Drain on-demand requests (seek, range requests).
            while let Some(plan) = Self::try_poll_demand(&mut dl, &cancel).await {
                if Self::fetch_and_commit_demand(&mut dl, plan, &cancel)
                    .await
                    .is_err()
                {
                    return;
                }
            }

            // 3. Plan next work.
            let outcome = tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    debug!("Downloader cancelled during plan");
                    return;
                }
                outcome = dl.plan() => outcome,
            };

            match outcome {
                PlanOutcome::Batch(plans) => {
                    if plans.is_empty() {
                        continue;
                    }

                    // Execute all fetches in parallel.
                    let io = dl.io().clone();
                    let futures: Vec<_> = plans
                        .into_iter()
                        .map(|plan| {
                            let io = io.clone();
                            async move { io.fetch(plan).await }
                        })
                        .collect();

                    let results = tokio::select! {
                        biased;
                        () = cancel.cancelled() => {
                            debug!("Downloader cancelled during batch fetch");
                            return;
                        }
                        results = futures::future::join_all(futures) => results,
                    };

                    // Commit results sequentially, checking demand between commits.
                    for result in results {
                        match result {
                            Ok(fetch) => dl.commit(fetch),
                            Err(e) => {
                                debug!(?e, "batch fetch error");
                                // Continue with remaining results — partial success.
                            }
                        }

                        // Check demand between commits — allow seek to interrupt batch.
                        if let Some(plan) = Self::try_poll_demand(&mut dl, &cancel).await
                            && Self::fetch_and_commit_demand(&mut dl, plan, &cancel)
                                .await
                                .is_err()
                        {
                            return;
                        }
                    }

                    steps_since_yield += 1;
                }

                PlanOutcome::Step => {
                    let result = tokio::select! {
                        biased;
                        () = cancel.cancelled() => {
                            debug!("Downloader cancelled during step");
                            return;
                        }
                        () = dl.demand_signal() => {
                            // Demand arrived while step was blocked.
                            // Re-enter loop to drain demand before continuing.
                            continue;
                        }
                        result = dl.step() => result,
                    };

                    match result {
                        Ok(StepResult::Item(fetch)) => {
                            dl.commit(fetch);
                            steps_since_yield += 1;
                        }
                        Ok(StepResult::PhaseChange) => {
                            // Phase changed, re-plan on next iteration.
                            continue;
                        }
                        Err(e) => {
                            debug!(?e, "step error");
                            return;
                        }
                    }
                }

                PlanOutcome::Complete => {
                    debug!("Downloader complete");
                    return;
                }
            }

            // 4. Periodic yield (only when not throttled — backpressure loop
            //    already yields to runtime via wait_ready).
            if !dl.should_throttle() && steps_since_yield >= yield_interval {
                tokio::task::yield_now().await;
                steps_since_yield = 0;
            }
        }
    }

    /// Non-blocking demand poll: returns `Some(plan)` if demand is available,
    /// `None` if not (without waiting).
    async fn try_poll_demand<D: Downloader>(
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
    async fn fetch_and_commit_demand<D: Downloader>(
        dl: &mut D,
        plan: D::Plan,
        cancel: &CancellationToken,
    ) -> Result<(), ()> {
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
                Ok(())
            }
            Err(e) => {
                debug!(?e, "demand fetch error");
                // Demand errors are not fatal — reader will retry or timeout.
                Ok(())
            }
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use kithara_platform::ThreadPool;
    use rstest::rstest;
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    use super::*;

    // -- Mock infrastructure --

    #[derive(Debug)]
    struct MockError;

    impl std::fmt::Display for MockError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "mock error")
        }
    }

    impl std::error::Error for MockError {}

    /// Mock I/O executor that signals when a demand fetch completes.
    #[derive(Clone)]
    struct MockIo {
        demand_fetched: Arc<Notify>,
    }

    impl DownloaderIo for MockIo {
        type Plan = u64;
        type Fetch = u64;
        type Error = MockError;

        async fn fetch(&self, plan: u64) -> Result<u64, MockError> {
            self.demand_fetched.notify_one();
            Ok(plan)
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

        async fn poll_demand(&mut self) -> Option<u64> {
            self.demand_queue.lock().unwrap().pop()
        }

        async fn plan(&mut self) -> PlanOutcome<u64> {
            PlanOutcome::Step
        }

        async fn step(&mut self) -> Result<StepResult<u64>, MockError> {
            self.step_entered.notify_one();
            // Simulate slow/stalled HTTP stream — 30s between chunks.
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(StepResult::Item(0))
        }

        fn commit(&mut self, _fetch: u64) {}

        fn should_throttle(&self) -> bool {
            false
        }

        fn wait_ready(&self) -> impl std::future::Future<Output = ()> {
            std::future::pending()
        }

        fn demand_signal(&self) -> impl std::future::Future<Output = ()> + use<> {
            let notify = Arc::clone(&self.demand_notify);
            async move {
                notify.notified().await;
            }
        }
    }

    /// Demand (on-demand range request) must be processed promptly even when
    /// `step()` is blocked on a slow sequential HTTP stream.
    ///
    /// Scenario: sequential download stalls (slow server), user seeks forward.
    /// The reader queues a demand. The downloader should process it immediately,
    /// not wait for the current step() to finish.
    ///
    /// Timeout: 2s — step() blocks for 30s, so if demand waits for step,
    /// the test will be killed by rstest timeout.
    #[rstest]
    #[timeout(Duration::from_secs(2))]
    #[tokio::test]
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
        let pool = ThreadPool::with_num_threads(1).unwrap();
        let _backend = Backend::new(downloader, &cancel, &pool);

        // Wait for the downloader to enter step() (blocked on slow stream).
        step_entered.notified().await;

        // Queue demand while step() is blocked (simulates reader seek).
        demand_queue.lock().unwrap().push(42);
        demand_notify.notify_one();

        // Must resolve promptly — if step() blocks demand, the 2s timeout fires.
        demand_fetched.notified().await;
    }
}
