//! HLS download worker — drives the [`HlsScheduler`] plan loop.
//!
//! Ported from the generic `kithara_stream::backend::Backend::run_downloader`
//! with the trait machinery replaced by direct calls into [`HlsScheduler`]
//! and a local [`fetch_plan`] helper that replaces the old `HlsIo` indirection.
//!
//! Responsibilities (identical to the deleted Backend):
//! - Backpressure (`should_throttle` + `wait_ready`)
//! - On-demand requests (`poll_demand_impl`, bypasses backpressure)
//! - Batch parallelism (plan → fetch in parallel → commit sequentially)
//! - Periodic yield to async runtime
//! - Cancellation via `CancellationToken`
//! - Hang detector ticks

use std::{sync::Arc, time::Duration};

use futures::{StreamExt, stream::FuturesUnordered};
use kithara_platform::{
    JoinHandle,
    thread::{spawn_named, yield_now},
    time::Instant,
    tokio,
    tokio::task::yield_now as task_yield_now,
};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::{HlsFetch, HlsPlan, HlsScheduler, plan::PlanOutcome};
use crate::{HlsError, loading::SegmentLoader};

/// Default yield interval (iterations between `yield_now` calls).
const DEFAULT_YIELD_INTERVAL: usize = 8;

/// Monotonic counter for unique download-worker thread names.
static DOWNLOAD_WORKER_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Handle to the HLS download worker — either an OS thread or an async task.
#[cfg(not(target_arch = "wasm32"))]
enum WorkerHandle {
    /// Dedicated OS thread (fallback for current-thread runtimes).
    Thread(JoinHandle<()>),
    /// Async task on a shared runtime (zero extra OS threads).
    Task(tokio::task::JoinHandle<()>),
}

/// RAII guard for the HLS download worker. Dropping cancels the
/// child cancel token (stopping the worker) and joins/aborts the
/// underlying task or thread. Stored on `HlsSource` so that dropping
/// the source tears down the worker automatically.
pub(crate) struct WorkerGuard {
    cancel: CancellationToken,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<WorkerHandle>,
    #[cfg(target_arch = "wasm32")]
    worker: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(handle) = self.worker.take() {
            match handle {
                WorkerHandle::Thread(join) => {
                    if join.join().is_err() {
                        debug!("HLS worker thread panicked during shutdown");
                    }
                }
                WorkerHandle::Task(task) => {
                    task.abort();
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        if let Some(task) = self.worker.take() {
            task.abort();
        }
    }
}

/// Spawn the HLS download worker.
///
/// Creates a child cancel token from `cancel`, moves `downloader` and
/// the runtime-resolution machinery into the worker, and returns a
/// [`WorkerGuard`] that cancels + joins on drop.
///
/// Runtime resolution matches the deleted `Backend::new`:
/// 1. `runtime` is `Some(handle)` → async task on that handle (0 OS threads)
/// 2. `runtime` is `None` + current runtime is multi-thread → async task (0 OS threads)
/// 3. Otherwise → dedicated OS thread with its own current-thread runtime
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn spawn_hls_worker(
    downloader: HlsScheduler,
    loader: Arc<SegmentLoader>,
    cancel: &CancellationToken,
    runtime: Option<tokio::runtime::Handle>,
) -> WorkerGuard {
    let child_cancel = cancel.child_token();
    let task_cancel = child_cancel.clone();

    let shared_handle = runtime.or_else(|| {
        tokio::runtime::Handle::try_current().ok().filter(|h| {
            !matches!(
                h.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::CurrentThread
            )
        })
    });

    let handle = if let Some(rt_handle) = shared_handle {
        debug!("hls-worker: spawning as async task on shared runtime");
        WorkerHandle::Task(rt_handle.spawn(run_hls_worker(downloader, loader, task_cancel)))
    } else {
        debug!("hls-worker: fallback — creating dedicated current-thread runtime");
        let id = DOWNLOAD_WORKER_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        WorkerHandle::Thread(spawn_named(format!("kithara-hls-worker-{id}"), move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build hls-worker runtime");
            rt.block_on(run_hls_worker(downloader, loader, task_cancel));
        }))
    };

    WorkerGuard {
        cancel: child_cancel,
        worker: Some(handle),
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn spawn_hls_worker(
    downloader: HlsScheduler,
    loader: Arc<SegmentLoader>,
    cancel: &CancellationToken,
    _runtime: Option<tokio::runtime::Handle>,
) -> WorkerGuard {
    let child_cancel = cancel.child_token();
    let task_cancel = child_cancel.clone();
    debug!("hls-worker: spawning as wasm task");
    let task = tokio::task::spawn(run_hls_worker(downloader, loader, task_cancel));
    WorkerGuard {
        cancel: child_cancel,
        worker: Some(task),
    }
}

enum LoopControl {
    Continue { made_progress: bool },
    Exit,
    Restart,
}

#[kithara_hang_detector::hang_watchdog]
async fn run_hls_worker(
    mut dl: HlsScheduler,
    loader: Arc<SegmentLoader>,
    cancel: CancellationToken,
) {
    debug!("HLS worker started");
    let yield_interval = DEFAULT_YIELD_INTERVAL;
    let mut steps_since_yield: usize = 0;

    loop {
        if cancel.is_cancelled() {
            debug!("HLS worker cancelled");
            return;
        }

        // Drain demand BEFORE throttle: a demand request may reset the
        // download cursor (via reset_for_seek_epoch) and relieve
        // segment-based throttle pressure. Without this, the subsequent
        // wait_while_throttled can block forever when the pending
        // demand would have un-throttled the downloader.
        let Ok(mut made_progress) = drain_demand_requests(&mut dl, &loader, &cancel).await else {
            return;
        };
        let Ok(throttle_progress) = wait_while_throttled(&mut dl, &loader, &cancel).await else {
            return;
        };
        let Ok(outcome) = plan_next(&mut dl, &cancel).await else {
            return;
        };

        hang_tick!();
        yield_now();
        made_progress |= throttle_progress;

        let control = match outcome {
            PlanOutcome::Batch(plans) => handle_batch(&mut dl, &loader, &cancel, plans).await,
            PlanOutcome::Step => {
                // HLS never emits Step — it always batches.
                debug!("HLS worker: unexpected Step outcome, ignoring");
                LoopControl::Restart
            }
            PlanOutcome::Complete => {
                debug!("HLS worker complete");
                LoopControl::Exit
            }
            PlanOutcome::Idle => handle_idle(&mut dl, &cancel).await,
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

        // Periodic yield (only when not throttled — backpressure loop
        // already yields via wait_ready).
        if made_progress && !dl.should_throttle() && steps_since_yield >= yield_interval {
            task_yield_now().await;
            steps_since_yield = 0;
        }
    }
}

async fn wait_while_throttled(
    dl: &mut HlsScheduler,
    loader: &Arc<SegmentLoader>,
    cancel: &CancellationToken,
) -> Result<bool, ()> {
    let mut made_progress = false;
    while dl.should_throttle() {
        // Check demand BEFORE blocking: a demand request may have
        // arrived between drain_demand_requests() and entering this
        // loop, or between a previous wait_ready wake and re-checking
        // throttle. Processing it can reset the cursor and relieve
        // throttle.
        if let Some(plan) = try_poll_demand(dl, cancel).await
            && fetch_and_commit_demand(dl, loader, plan, cancel)
                .await
                .map(|progress| {
                    made_progress |= progress;
                })
                .is_err()
        {
            return Err(());
        }
        if !dl.should_throttle() {
            break;
        }

        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                debug!("HLS worker cancelled during backpressure");
                return Err(());
            }
            () = dl.wait_ready_future() => {}
        }

        if let Some(plan) = try_poll_demand(dl, cancel).await
            && fetch_and_commit_demand(dl, loader, plan, cancel)
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

async fn drain_demand_requests(
    dl: &mut HlsScheduler,
    loader: &Arc<SegmentLoader>,
    cancel: &CancellationToken,
) -> Result<bool, ()> {
    let mut made_progress = false;
    while let Some(plan) = try_poll_demand(dl, cancel).await {
        if fetch_and_commit_demand(dl, loader, plan, cancel)
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

async fn plan_next(dl: &mut HlsScheduler, cancel: &CancellationToken) -> Result<PlanOutcome, ()> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => {
            debug!("HLS worker cancelled during plan");
            Err(())
        }
        outcome = dl.plan_next() => Ok(outcome),
    }
}

async fn handle_idle(dl: &mut HlsScheduler, cancel: &CancellationToken) -> LoopControl {
    tokio::select! {
        biased;
        () = cancel.cancelled() => LoopControl::Exit,
        () = dl.demand_signal_future() => {
            task_yield_now().await;
            // Completing an idle wait is forward progress.
            LoopControl::Continue { made_progress: true }
        }
    }
}

#[kithara_hang_detector::hang_watchdog]
async fn handle_batch(
    dl: &mut HlsScheduler,
    loader: &Arc<SegmentLoader>,
    cancel: &CancellationToken,
    plans: Vec<HlsPlan>,
) -> LoopControl {
    if plans.is_empty() {
        task_yield_now().await;
        return LoopControl::Restart;
    }

    let plan_count = plans.len();
    let mut pending = plans
        .into_iter()
        .enumerate()
        .map(|(index, plan)| {
            let loader = Arc::clone(loader);
            async move { (index, fetch_plan(&loader, plan).await) }
        })
        .collect::<FuturesUnordered<_>>();

    let mut made_progress = false;
    let mut next_commit = 0usize;
    let mut results: Vec<Option<Result<HlsFetch, HlsError>>> =
        std::iter::repeat_with(|| None).take(plan_count).collect();

    while next_commit < plan_count {
        let Some((index, result)) = (tokio::select! {
            biased;
            () = cancel.cancelled() => {
                debug!("HLS worker cancelled during batch fetch");
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
                    dl.commit_fetch(fetch);
                    made_progress = true;
                }
                Err(e) => debug!(?e, "batch fetch error"),
            }
            next_commit += 1;

            // NOTE: demand processing is intentionally NOT done here.
            // Fetching a demand segment inside handle_batch can deadlock
            // when the demand targets a segment still pending in the
            // batch's FuturesUnordered — both share the same OnceCell,
            // and the demand's get_or_try_init blocks waiting for the
            // batch future that is not being polled. Demand is processed
            // at the top of the main loop via drain_demand_requests().
        }
    }

    LoopControl::Continue { made_progress }
}

async fn try_poll_demand(dl: &mut HlsScheduler, cancel: &CancellationToken) -> Option<HlsPlan> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => None,
        plan = dl.poll_demand_next() => plan,
    }
}

async fn fetch_and_commit_demand(
    dl: &mut HlsScheduler,
    loader: &Arc<SegmentLoader>,
    plan: HlsPlan,
    cancel: &CancellationToken,
) -> Result<bool, ()> {
    let result = tokio::select! {
        biased;
        () = cancel.cancelled() => {
            debug!("HLS worker cancelled during demand fetch");
            return Err(());
        }
        result = fetch_plan(loader, plan) => result,
    };

    match result {
        Ok(fetch) => {
            dl.commit_fetch(fetch);
            Ok(true)
        }
        Err(e) => {
            debug!(?e, "demand fetch error");
            // Demand errors are not fatal — reader will retry or timeout.
            Ok(false)
        }
    }
}

/// Fetch a single HLS plan — replaces the deleted `HlsIo::fetch`.
///
/// Kicks off the init-segment load (if requested) in parallel with the
/// media-segment load via `tokio::join!`, measures duration, and
/// packages the result into an [`HlsFetch`].
async fn fetch_plan(loader: &Arc<SegmentLoader>, plan: HlsPlan) -> Result<HlsFetch, HlsError> {
    let start = Instant::now();

    let init_fut = {
        let loader = Arc::clone(loader);
        let variant = plan.variant;
        let need_init = plan.need_init;
        async move {
            if need_init {
                match loader.load_init_segment(variant).await {
                    Ok(m) => (Some(m.url), m.len),
                    Err(e) => {
                        tracing::warn!(
                            variant,
                            error = %e,
                            "init segment load failed"
                        );
                        (None, 0)
                    }
                }
            } else {
                (None, 0)
            }
        }
    };

    let seg_idx = plan
        .segment
        .media_index()
        .expect("fetch_plan called with non-Media segment");

    let (media_result, (init_url, init_len)) = tokio::join!(
        loader.load_media_segment_with_source_for_epoch(plan.variant, seg_idx, plan.seek_epoch),
        init_fut,
    );

    let duration: Duration = start.elapsed();
    let (media, media_cached) = media_result?;

    Ok(HlsFetch {
        init_len,
        init_url,
        media,
        media_cached,
        segment: plan.segment,
        variant: plan.variant,
        duration,
        seek_epoch: plan.seek_epoch,
    })
}
