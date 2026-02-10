#![forbid(unsafe_code)]

//! Generic backend for streaming sources.
//!
//! Backend spawns a Downloader task. Source is owned by Reader directly.
//! When Backend is dropped, the downloader task is cancelled automatically.

use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::downloader::Downloader;
use crate::pool::ThreadPool;

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

    async fn run_downloader<D: Downloader>(mut downloader: D, cancel: CancellationToken) {
        debug!("Downloader task started");
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    debug!("Downloader cancelled");
                    break;
                }
                has_more = downloader.step() => {
                    if !has_more {
                        debug!("Downloader complete");
                        break;
                    }
                }
            }
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
