//! [`TrackHandle`] — per-track handle exposing direct fetch operations.
//!
//! Each call to [`Downloader::new_track`](super::Downloader::new_track)
//! or [`Downloader::register`](super::Downloader::register) creates a
//! distinct track with its own [`TrackInner`] state. Cloning a
//! `TrackHandle` returns another reference to the **same** track —
//! components inside one HLS stream / one File source share one track,
//! but two streams instantiated separately get distinct tracks. The
//! shared resource (the `HttpClient` and pool) lives in
//! [`DownloaderInner`](super::downloader::DownloaderInner) and is
//! reached via the `pool` Arc.
//!
//! `TrackHandle` exposes [`execute`](Self::execute),
//! [`execute_batch`](Self::execute_batch), and
//! [`execute_batch_blocking`](Self::execute_batch_blocking) — used by
//! protocol code (HLS playlists, DRM keys, init/media segments, HEAD
//! probes) that needs to issue fetches outside the
//! `Stream<Item = FetchCmd>` loop. The track-local
//! [`CancellationToken`](tokio_util::sync::CancellationToken) cancels
//! all in-flight requests when the last clone of the handle is dropped,
//! giving each track real teardown isolation from siblings.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use tokio_util::sync::CancellationToken;

use super::{
    cmd::{FetchCmd, FetchResult},
    downloader::{DownloaderInner, fetch_only},
};

/// Monotonic counter for assigning unique track IDs.
static TRACK_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Per-track state held behind an `Arc` so that all clones of one
/// `TrackHandle` share it. Two distinct tracks have two distinct
/// `Arc<TrackInner>` allocations.
pub(super) struct TrackInner {
    /// Unique track id (for tracing / debugging).
    pub(super) id: u64,
    /// Track-local cancellation. Child of the downloader-level cancel —
    /// dropping the last clone of the [`TrackHandle`] cancels all
    /// in-flight fetches issued through this track.
    pub(super) cancel: CancellationToken,
}

impl TrackInner {
    pub(super) fn new(parent_cancel: &CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            id: TRACK_ID_COUNTER.fetch_add(1, Ordering::Relaxed),
            cancel: parent_cancel.child_token(),
        })
    }
}

impl Drop for TrackInner {
    fn drop(&mut self) {
        // Cancel any in-flight fetches issued through this track when
        // the last handle clone is dropped.
        self.cancel.cancel();
    }
}

/// Per-track handle exposing direct [`execute`](Self::execute) /
/// [`execute_batch`](Self::execute_batch) /
/// [`execute_batch_blocking`](Self::execute_batch_blocking) operations
/// on a shared [`Downloader`](super::Downloader) pool.
///
/// Cheap to [`Clone`] (two Arc bumps). Holding a `TrackHandle` keeps
/// both the underlying download pool **and** the per-track state alive.
/// When the last clone of one track's handle is dropped, that track's
/// in-flight requests are cancelled — siblings using other tracks are
/// unaffected.
#[derive(Clone)]
pub struct TrackHandle {
    /// Shared download pool (HTTP client, runtime, pool, registration).
    pub(super) pool: Arc<DownloaderInner>,
    /// Per-track state — distinct allocation per track. Two clones of
    /// the same `TrackHandle` share this `Arc`; two tracks created via
    /// separate `new_track()` / `register()` calls have different ones.
    pub(super) state: Arc<TrackInner>,
}

impl std::fmt::Debug for TrackHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackHandle")
            .field("track_id", &self.state.id)
            .finish_non_exhaustive()
    }
}

impl TrackHandle {
    /// Track id (for tracing / debugging).
    #[must_use]
    pub fn id(&self) -> u64 {
        self.state.id
    }

    /// Track-local cancellation token.
    ///
    /// Cancelling this token aborts in-flight fetches issued through
    /// this track. Cloning the handle and dropping a clone does NOT
    /// cancel — the cancel fires when the last clone is dropped.
    #[must_use]
    pub fn cancel(&self) -> CancellationToken {
        self.state.cancel.clone()
    }

    /// Execute a single [`FetchCmd`] directly and return the result.
    ///
    /// Uses the same `HttpClient` and pool as the streaming pipeline.
    /// Intended for control-plane requests (playlists, DRM keys) where
    /// the caller needs the result before proceeding.
    ///
    /// If `cmd.on_complete` is set, it is called with `&result` before
    /// the result is returned — both the callback and the caller observe
    /// the same result.
    pub async fn execute(&self, mut cmd: FetchCmd) -> FetchResult {
        let on_complete = cmd.on_complete.take();
        let result = fetch_only(
            &self.pool.client,
            self.pool.chunk_timeout,
            &self.pool.pool,
            cmd,
        )
        .await;
        if let Some(cb) = on_complete {
            cb(&result);
        }
        result
    }

    /// Execute a batch of [`FetchCmd`] concurrently and return all results.
    ///
    /// Runs all commands in parallel via `join_all` using the same
    /// `HttpClient` and pool as the streaming pipeline. Intended for
    /// batched control-plane requests (e.g. HEAD size-map queries for all
    /// segments at once) and for HLS plan loop dispatch where a set of
    /// segment fetches should run concurrently but deliver `on_complete`
    /// callbacks in a deterministic order so that downstream state
    /// transitions (commit, layout update) are applied in batch-index
    /// order regardless of network completion order.
    ///
    /// Ordering guarantees:
    /// - **Network fetches**: run concurrently; completion order is
    ///   determined by network timing.
    /// - **`on_complete` callbacks**: fired in input order (index 0
    ///   first, then index 1, etc.), after **all** fetches in the batch
    ///   have finished. A slow fetch at index 0 delays the callbacks for
    ///   indices 1..N until index 0 finishes.
    /// - **Returned `Vec<FetchResult>`**: preserves input order of `cmds`.
    pub async fn execute_batch(&self, cmds: Vec<FetchCmd>) -> Vec<FetchResult> {
        let client = self.pool.client.clone();
        let chunk_timeout = self.pool.chunk_timeout;
        let pool = self.pool.pool.clone();

        // Strip on_completes so we can fire them in input order after all
        // fetches finish. The fetch itself runs without on_complete.
        let mut on_completes: Vec<Option<super::cmd::OnCompleteFn>> =
            Vec::with_capacity(cmds.len());
        let mut stripped: Vec<FetchCmd> = Vec::with_capacity(cmds.len());
        for mut cmd in cmds {
            on_completes.push(cmd.on_complete.take());
            stripped.push(cmd);
        }

        // Run all fetches concurrently (without firing on_complete).
        let futs: Vec<_> = stripped
            .into_iter()
            .map(|cmd| {
                let client = client.clone();
                let pool = pool.clone();
                async move { fetch_only(&client, chunk_timeout, &pool, cmd).await }
            })
            .collect();
        let results = futures::future::join_all(futs).await;

        // Fire on_completes in input order, after all fetches are done.
        for (result, cb_opt) in results.iter().zip(on_completes.into_iter()) {
            if let Some(cb) = cb_opt {
                cb(result);
            }
        }

        results
    }

    /// Execute a batch synchronously — blocks the caller thread until all
    /// commands complete.
    ///
    /// Bridges from non-async contexts (e.g. a dedicated OS thread running
    /// a legacy HLS plan loop) to the async download pool. The batch runs
    /// on the downloader's tokio runtime (either the one supplied via
    /// [`DownloaderConfig::with_runtime`](super::DownloaderConfig::with_runtime)
    /// or the one the caller is already inside), and the blocking wait is
    /// implemented via a `std::sync::mpsc` channel so it works from any
    /// OS thread — no tokio runtime is required on the calling thread.
    ///
    /// Same ordering guarantees as [`execute_batch`](Self::execute_batch):
    /// fetches run concurrently, `on_complete` callbacks fire in input
    /// order after all fetches finish, and the returned `Vec<FetchResult>`
    /// preserves input order.
    ///
    /// # Panics
    ///
    /// Panics if no tokio runtime handle is available — either explicitly
    /// provided via [`DownloaderConfig`](super::DownloaderConfig) or
    /// implicitly from the calling thread. Callers on pure std threads
    /// must construct the downloader with a runtime handle.
    #[must_use]
    pub fn execute_batch_blocking(&self, cmds: Vec<FetchCmd>) -> Vec<FetchResult> {
        let handle = self
            .pool
            .runtime
            .clone()
            .or_else(|| kithara_platform::tokio::runtime::Handle::try_current().ok())
            .expect("no tokio runtime available for execute_batch_blocking");
        let this = self.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        handle.spawn(async move {
            let results = this.execute_batch(cmds).await;
            // Receiver may have been dropped if the caller thread was
            // cancelled — ignore the send failure in that case.
            let _ = tx.send(results);
        });
        rx.recv()
            .expect("downloader task dropped sender without sending results")
    }
}
