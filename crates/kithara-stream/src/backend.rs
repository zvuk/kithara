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

use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::{
    downloader::{Downloader, DownloaderIo, PlanOutcome, StepResult},
    pool::ThreadPool,
};

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

        let handle = tokio::runtime::Handle::current();
        pool.spawn(move || {
            handle.block_on(Self::run_downloader(downloader, task_cancel));
        });

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
