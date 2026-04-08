//! [`TrackHandle`] — per-track handle exposing direct fetch operations.
//!
//! Each call to [`Downloader::register`](super::Downloader::register)
//! creates a distinct track with its own [`TrackInner`] state. Cloning a
//! `TrackHandle` returns another reference to the **same** track —
//! components inside one HLS stream / one File source share one track,
//! but two streams instantiated separately get distinct tracks. The
//! shared resource (the `HttpClient` and pool) lives in
//! [`DownloaderInner`](super::downloader::DownloaderInner) and is
//! reached via the `pool` Arc.
//!
//! `TrackHandle` exposes [`execute`](Self::execute) — used by protocol
//! code (HLS playlists, DRM keys, init/media segments, HEAD probes) that
//! needs to issue fetches outside the `Stream<Item = FetchCmd>` loop.
//! The track-local
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

/// Per-track handle exposing a direct [`execute`](Self::execute)
/// operation on a shared [`Downloader`](super::Downloader) pool.
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
    /// the same `TrackHandle` share this `Arc`; two tracks obtained via
    /// separate [`Downloader::register`](super::Downloader::register)
    /// calls have different ones.
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
    ///
    /// The fetch honors the per-track cancellation token: dropping the
    /// last clone of this handle cancels any in-flight `execute` promptly,
    /// without waiting for `chunk_timeout`.
    pub async fn execute(&self, mut cmd: FetchCmd) -> FetchResult {
        let on_complete = cmd.on_complete.take();
        let result = fetch_only(
            &self.pool.client,
            self.pool.chunk_timeout,
            &self.pool.pool,
            &self.state.cancel,
            cmd,
        )
        .await;
        if let Some(cb) = on_complete {
            cb(&result);
        }
        result
    }
}
