#![forbid(unsafe_code)]

//! Generic backend for streaming sources.
//!
//! Backend spawns a Downloader task. Source is owned by Reader directly.

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::downloader::Downloader;

/// Spawns and owns a Downloader task.
///
/// The downloader runs independently (async, writing data to storage).
/// Source is owned by Reader and accessed directly (sync).
///
/// Backend exists only to manage the downloader lifecycle.
/// Drop it to abort the downloader task.
pub struct Backend {
    _downloader_handle: JoinHandle<()>,
}

impl Backend {
    /// Spawn a downloader task.
    ///
    /// The downloader runs in the background writing data to storage.
    pub fn new<D: Downloader>(downloader: D, cancel: CancellationToken) -> Self {
        let handle = tokio::spawn(Self::run_downloader(downloader, cancel));

        Self {
            _downloader_handle: handle,
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
