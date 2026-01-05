#![forbid(unsafe_code)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use futures::StreamExt;
use kithara_assets::AssetResource;
use kithara_net::HttpClient;
use kithara_storage::{Resource, StreamingResource, StreamingResourceExt};
use thiserror::Error;
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};
use url::Url;

type AssetStreamRes = AssetResource<
    StreamingResource,
    kithara_assets::LeaseGuard<kithara_assets::EvictAssets<kithara_assets::DiskAssetStore>>,
>;

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("net error: {0}")]
    Net(String),

    #[error("net stream error: {0}")]
    NetStream(String),

    #[error("storage write_at error: {0}")]
    StorageWrite(String),

    #[error("received empty chunk from network stream at offset={offset}")]
    EmptyNetChunk { offset: u64 },

    #[error(
        "empty read after storage reported range as Ready (offset={offset}, requested_len={requested_len})"
    )]
    EmptyAfterReady { offset: u64, requested_len: usize },

    #[error("download offset overflowed u64")]
    OffsetOverflow,

    #[error("fetch task join error: {0}")]
    Join(#[from] JoinError),
}

impl FetchError {
    pub fn to_fail_message(&self) -> String {
        self.to_string()
    }
}

/// Shared "download loop" abstraction: network stream → `write_at` → `commit`/`fail`.
///
/// This is internal to `kithara-file`.
/// It intentionally depends on `kithara-net` + `kithara-storage` through the concrete resource
/// handle provided by `kithara-assets`.
#[derive(Clone, Debug)]
pub struct Fetcher {
    client: HttpClient,
    url: Url,
}

impl Fetcher {
    pub fn new(client: HttpClient, url: Url) -> Self {
        Self { client, url }
    }

    /// Spawn the download task that writes into `res`.
    ///
    /// Notes:
    /// - Errors are materialized by calling `res.fail(...)` so that readers unblock deterministically.
    /// - Successful completion calls `res.commit(Some(final_len))`.
    /// - The fetch loop stops early only if `cancel` is cancelled (it does *not* cancel automatically
    ///   when the consumer stream ends; this supports "seek forward, then seek backward" reuse).
    ///
    /// Return:
    /// - A join handle that resolves to Ok(()) on success, or Err(FetchError) on failure.
    pub fn spawn(
        self,
        res: AssetStreamRes,
        cancel: CancellationToken,
    ) -> JoinHandle<Result<(), FetchError>> {
        self.spawn_with_progress(res, Arc::new(Progress::new()), cancel)
    }

    /// Same as `spawn`, but with a shared progress handle so the reader side can update it.
    pub fn spawn_with_progress(
        self,
        res: AssetStreamRes,
        progress: Arc<Progress>,
        cancel: CancellationToken,
    ) -> JoinHandle<Result<(), FetchError>> {
        tokio::spawn(async move { self.run(res, progress, cancel).await })
    }

    async fn run(
        self,
        res: AssetStreamRes,
        progress: Arc<Progress>,
        cancel: CancellationToken,
    ) -> Result<(), FetchError> {
        trace!(url = %self.url, "kithara-file fetcher: starting download");

        // Helper: materialize an error into the resource so readers unblock,
        // then return that structured error.
        async fn fail(res: &AssetStreamRes, err: FetchError) -> Result<(), FetchError> {
            let _ = res.fail(err.to_fail_message()).await;
            Err(err)
        }

        let mut stream = match self.client.stream(self.url, None).await {
            Ok(s) => s,
            Err(e) => {
                warn!("kithara-file fetcher: net error: {e}");
                return fail(&res, FetchError::Net(e.to_string())).await;
            }
        };

        let started_at = Instant::now();
        let mut last_progress_log = Instant::now();

        let mut offset: u64 = 0;
        let mut chunks_seen: u64 = 0;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    trace!(offset, "kithara-file fetcher: cancelled");
                    // Do not fail the resource: cancellation is an intentional stop, and the resource
                    // may remain readable for already-downloaded ranges.
                    return Ok(());
                }

                next = stream.next() => {
                    let Some(next) = next else { break; };

                    let bytes = match next {
                        Ok(b) => b,
                        Err(e) => {
                            warn!("kithara-file fetcher: net stream error: {e}");
                            return fail(&res, FetchError::NetStream(e.to_string())).await;
                        }
                    };

                    if bytes.is_empty() {
                        // If the network stream yields empty chunks, that can cause downstream
                        // readers to interpret it as EOF and stop early.
                        warn!(offset, "kithara-file fetcher: received empty net chunk");
                        return fail(&res, FetchError::EmptyNetChunk { offset }).await;
                    }

                    if let Err(e) = res.write_at(offset, &bytes).await {
                        warn!(
                            offset,
                            bytes = bytes.len(),
                            "kithara-file fetcher: storage write_at error: {e}"
                        );
                        return fail(&res, FetchError::StorageWrite(e.to_string())).await;
                    }

                    offset = offset
                        .checked_add(bytes.len() as u64)
                        .ok_or(FetchError::OffsetOverflow)?;

                    chunks_seen = chunks_seen.saturating_add(1);

                    progress.set_downloaded(offset);

                    // Log roughly once per second (or on first chunk), to avoid spamming.
                    let now = Instant::now();
                    if chunks_seen == 1 || now.duration_since(last_progress_log).as_secs_f32() >= 1.0 {
                        last_progress_log = now;

                        let read = progress.read_pos();
                        let backlog = offset.saturating_sub(read);
                        let elapsed = started_at.elapsed().as_secs_f32().max(0.001);
                        let mbps = (offset as f32) / (1024.0 * 1024.0) / elapsed;

                        debug!(
                            downloaded = offset,
                            read,
                            backlog,
                            chunks = chunks_seen,
                            mbps,
                            "kithara-file fetcher: progress"
                        );
                    }
                }
            }
        }

        trace!(final_len = offset, "kithara-file fetcher: commit");
        let _ = res.commit(Some(offset)).await;

        Ok(())
    }
}

/// Shared progress state between fetcher (download loop) and reader (Seek+Read consumer).
///
/// Note: this is *only* for logging/debugging; it does not affect correctness.
#[derive(Debug)]
pub struct Progress {
    downloaded: AtomicU64,
    read_pos: AtomicU64,
}

impl Progress {
    pub fn new() -> Self {
        Self {
            downloaded: AtomicU64::new(0),
            read_pos: AtomicU64::new(0),
        }
    }

    pub fn set_downloaded(&self, v: u64) {
        self.downloaded.store(v, Ordering::Relaxed);
    }

    pub fn downloaded(&self) -> u64 {
        self.downloaded.load(Ordering::Relaxed)
    }

    pub fn set_read_pos(&self, v: u64) {
        self.read_pos.store(v, Ordering::Relaxed);
    }

    pub fn read_pos(&self) -> u64 {
        self.read_pos.load(Ordering::Relaxed)
    }
}
