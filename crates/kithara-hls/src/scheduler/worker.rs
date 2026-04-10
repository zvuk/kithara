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

use std::sync::Arc;

use futures::{StreamExt, stream::FuturesUnordered};
use kithara_platform::{
    JoinHandle,
    thread::{spawn_named, yield_now},
    tokio,
    tokio::task::yield_now as task_yield_now,
};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::{DemandOutcome, HlsFetch, HlsPlan, HlsScheduler, PlanOutcome, cmd_builder::fetch_plan};
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
        let mut made_progress = drain_demand_requests(&mut dl, &loader, &cancel).await;
        let throttle_progress = wait_while_throttled(&mut dl, &loader, &cancel).await;
        let outcome = dl.plan_next();

        hang_tick!();
        yield_now();
        made_progress |= throttle_progress;

        let control = match outcome {
            PlanOutcome::Batch(plans) => handle_batch(&mut dl, &loader, &cancel, plans).await,
            PlanOutcome::FetchMetadata(variant) => {
                if handle_metadata(&mut dl, &loader, variant, &cancel)
                    .await
                    .is_err()
                {
                    LoopControl::Restart
                } else {
                    // Immediate restart after metadata to plan the new segments.
                    LoopControl::Restart
                }
            }
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
) -> bool {
    let mut made_progress = false;
    while dl.should_throttle() {
        if let Some(outcome) = dl.poll_demand_next() {
            match handle_demand_outcome(dl, loader, cancel, outcome).await {
                Ok(progress) => made_progress |= progress,
                Err(()) => return made_progress,
            }
        }
        if !dl.should_throttle() {
            break;
        }

        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                debug!("HLS worker cancelled during backpressure");
                return made_progress;
            }
            () = dl.wait_ready_future() => {}
        }

        if let Some(outcome) = dl.poll_demand_next() {
            match handle_demand_outcome(dl, loader, cancel, outcome).await {
                Ok(progress) => made_progress |= progress,
                Err(()) => return made_progress,
            }
        }
    }
    made_progress
}

async fn drain_demand_requests(
    dl: &mut HlsScheduler,
    loader: &Arc<SegmentLoader>,
    cancel: &CancellationToken,
) -> bool {
    let mut made_progress = false;
    while let Some(outcome) = dl.poll_demand_next() {
        match handle_demand_outcome(dl, loader, cancel, outcome).await {
            Ok(progress) => made_progress |= progress,
            Err(()) => return made_progress,
        }
    }
    made_progress
}

async fn handle_demand_outcome(
    dl: &mut HlsScheduler,
    loader: &Arc<SegmentLoader>,
    cancel: &CancellationToken,
    outcome: DemandOutcome,
) -> Result<bool, ()> {
    match outcome {
        DemandOutcome::Fetch(plan) => fetch_and_commit_demand(dl, loader, plan, cancel).await,
        DemandOutcome::FetchMetadata(variant) => {
            if handle_metadata(dl, loader, variant, cancel).await.is_err() {
                // If metadata fetch fails, we don't abort the worker, just log
                // and proceed. The next demand request will retry or the reader will fail.
                return Ok(false);
            }
            // After metadata is loaded, we loop back to `try_poll_demand`
            // since the next request will evaluate against the new metadata.
            Ok(true)
        }
    }
}

async fn handle_metadata(
    dl: &mut HlsScheduler,
    loader: &Arc<SegmentLoader>,
    variant: usize,
    cancel: &CancellationToken,
) -> Result<(), ()> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => {
            debug!("HLS worker cancelled during metadata fetch");
            return Err(());
        }
        res = async {
            loader.cache.load_media_playlist(variant).await?;
            HlsScheduler::calculate_size_map(&dl.playlist_state, &dl.size_probe, variant).await
        } => {
            if let Err(e) = res {
                debug!(?e, variant, "failed to fetch variant metadata");
                // Demand relies on this, so we should publish the error.
                dl.publish_download_error("variant metadata preparation error", &e);
                dl.coord.condvar.notify_all();
                return Err(());
            }
        }
    }
    // After calculating size map, we should apply cached progress.
    let (cached_count, cached_end_offset) = dl.populate_cached_segments_if_needed(variant);
    dl.apply_cached_segment_progress(variant, cached_count, cached_end_offset);
    dl.coord.condvar.notify_all();
    Ok(())
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
                Ok(fetch) => dl.enqueue_commit(fetch),
                Err(e) => debug!(?e, "batch fetch error"),
            }
            next_commit += 1;
        }

        if dl.drain_commits() {
            made_progress = true;
        }
    }

    LoopControl::Continue { made_progress }
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
            dl.enqueue_commit(fetch);
            dl.drain_commits();
            Ok(true)
        }
        Err(e) => {
            debug!(?e, "demand fetch error");
            // Demand errors are not fatal — reader will retry or timeout.
            Ok(false)
        }
    }
}
