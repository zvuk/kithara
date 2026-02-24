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

#[cfg(test)]
use std::future::Future;

use kithara_platform::ThreadPool;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::downloader::{Downloader, DownloaderIo, PlanOutcome, StepResult};

/// Default yield interval (iterations between `yield_now` calls).
const DEFAULT_YIELD_INTERVAL: usize = 8;

enum LoopControl {
    Exit,
    Proceed,
    Restart,
}

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

    async fn run_downloader<D: Downloader>(mut dl: D, cancel: CancellationToken) {
        debug!("Downloader task started");
        let yield_interval = DEFAULT_YIELD_INTERVAL;
        let mut steps_since_yield: usize = 0;

        loop {
            if cancel.is_cancelled() {
                debug!("Downloader cancelled");
                return;
            }

            if Self::wait_while_throttled(&mut dl, &cancel).await.is_err() {
                return;
            }
            if Self::drain_demand_requests(&mut dl, &cancel).await.is_err() {
                return;
            }

            let Ok(outcome) = Self::plan_next(&mut dl, &cancel).await else {
                return;
            };

            let control = match outcome {
                PlanOutcome::Batch(plans) => Self::handle_batch(&mut dl, &cancel, plans).await,
                PlanOutcome::Step => Self::handle_step(&mut dl, &cancel).await,
                PlanOutcome::Complete => {
                    debug!("Downloader complete");
                    LoopControl::Exit
                }
            };

            match control {
                LoopControl::Exit => return,
                LoopControl::Restart => continue,
                LoopControl::Proceed => {
                    steps_since_yield += 1;
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

    async fn wait_while_throttled<D: Downloader>(
        dl: &mut D,
        cancel: &CancellationToken,
    ) -> Result<(), ()> {
        while dl.should_throttle() {
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
                    .is_err()
            {
                return Err(());
            }
        }
        Ok(())
    }

    async fn drain_demand_requests<D: Downloader>(
        dl: &mut D,
        cancel: &CancellationToken,
    ) -> Result<(), ()> {
        while let Some(plan) = Self::try_poll_demand(dl, cancel).await {
            if Self::fetch_and_commit_demand(dl, plan, cancel)
                .await
                .is_err()
            {
                return Err(());
            }
        }
        Ok(())
    }

    async fn plan_next<D: Downloader>(
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

    async fn handle_batch<D: Downloader>(
        dl: &mut D,
        cancel: &CancellationToken,
        plans: Vec<D::Plan>,
    ) -> LoopControl {
        if plans.is_empty() {
            tokio::task::yield_now().await;
            return LoopControl::Restart;
        }

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
                return LoopControl::Exit;
            }
            results = futures::future::join_all(futures) => results,
        };

        for result in results {
            match result {
                Ok(fetch) => dl.commit(fetch),
                Err(e) => debug!(?e, "batch fetch error"),
            }

            if let Some(plan) = Self::try_poll_demand(dl, cancel).await
                && Self::fetch_and_commit_demand(dl, plan, cancel)
                    .await
                    .is_err()
            {
                return LoopControl::Exit;
            }
        }

        LoopControl::Proceed
    }

    async fn handle_step<D: Downloader>(dl: &mut D, cancel: &CancellationToken) -> LoopControl {
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
                LoopControl::Proceed
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
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use std::{sync::Arc, time::Duration};

    use kithara_platform::{Mutex, ThreadPool};
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
            self.demand_queue.lock().pop()
        }

        async fn plan(&mut self) -> PlanOutcome<u64> {
            PlanOutcome::Step
        }

        async fn step(&mut self) -> Result<StepResult<u64>, MockError> {
            self.step_entered.notify_one();
            // Simulate stalled HTTP stream — block forever.
            // (test timeout on native prevents actual hang)
            std::future::pending::<()>().await;
            Ok(StepResult::Item(0))
        }

        fn commit(&mut self, _fetch: u64) {}

        fn should_throttle(&self) -> bool {
            false
        }

        fn wait_ready(&self) -> impl Future<Output = ()> {
            std::future::pending()
        }

        fn demand_signal(&self) -> impl Future<Output = ()> + use<> {
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
        let pool = ThreadPool::global();
        let _backend = Backend::new(downloader, &cancel, &pool);

        // Wait for the downloader to enter step() (blocked on slow stream).
        step_entered.notified().await;

        // Queue demand while step() is blocked (simulates reader seek).
        demand_queue.lock().push(42);
        demand_notify.notify_one();

        // Must resolve promptly — if step() blocks demand, the 2s timeout fires.
        demand_fetched.notified().await;
    }
}
