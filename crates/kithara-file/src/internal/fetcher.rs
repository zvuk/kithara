#![forbid(unsafe_code)]

use std::{sync::Arc, time::Instant};

use futures::StreamExt;
use kithara_assets::AssetResource;
use kithara_net::HttpClient;
use kithara_storage::{Resource, StreamingResource, StreamingResourceExt};
use tracing::{debug, trace, warn};
use url::Url;

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
    ///
    /// Debugging:
    /// - Emits periodic progress logs (`debug`) so you can verify that downloading progresses
    ///   while the consumer is already reading.
    pub fn spawn(
        self,
        res: AssetResource<
            StreamingResource,
            kithara_assets::LeaseGuard<kithara_assets::EvictAssets<kithara_assets::DiskAssetStore>>,
        >,
    ) {
        let read_progress = Arc::new(Progress::new());
        self.spawn_with_progress(res, read_progress);
    }

    /// Same as `spawn`, but with a shared progress handle so the reader side can update it.
    ///
    /// This lets us emit combined logs like:
    /// - downloaded bytes
    /// - last known read offset
    /// - downloaded - read (rough backlog)
    pub fn spawn_with_progress(
        self,
        res: AssetResource<
            StreamingResource,
            kithara_assets::LeaseGuard<kithara_assets::EvictAssets<kithara_assets::DiskAssetStore>>,
        >,
        progress: Arc<Progress>,
    ) {
        tokio::spawn(async move {
            trace!(url = %self.url, "kithara-file fetcher: starting download");

            let mut stream = match self.client.stream(self.url, None).await {
                Ok(s) => s,
                Err(e) => {
                    warn!("kithara-file fetcher: net error: {e}");
                    let _ = res.fail(format!("net error: {e}")).await;
                    return;
                }
            };

            let mut off: u64 = 0;
            let mut chunks: u64 = 0;

            let started = Instant::now();
            let mut last_log = Instant::now();

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        if let Err(e) = res.write_at(off, &bytes).await {
                            warn!(
                                off,
                                bytes = bytes.len(),
                                "kithara-file fetcher: storage write_at error: {e}"
                            );
                            let _ = res.fail(format!("storage write_at error: {e}")).await;
                            return;
                        }

                        off = off.saturating_add(bytes.len() as u64);
                        chunks = chunks.saturating_add(1);

                        progress.set_downloaded(off);

                        // Log roughly once per second (or on first chunk), to avoid spamming.
                        let now = Instant::now();
                        if chunks == 1 || now.duration_since(last_log).as_secs_f32() >= 1.0 {
                            last_log = now;

                            let read = progress.read_pos();
                            let backlog = off.saturating_sub(read);
                            let elapsed = started.elapsed().as_secs_f32().max(0.001);
                            let mbps = (off as f32) / (1024.0 * 1024.0) / elapsed;

                            debug!(
                                downloaded = off,
                                read,
                                backlog,
                                chunks,
                                mbps = mbps,
                                "kithara-file fetcher: progress"
                            );
                        }
                    }
                    Err(e) => {
                        warn!("kithara-file fetcher: net stream error: {e}");
                        let _ = res.fail(format!("net stream error: {e}")).await;
                        return;
                    }
                }
            }

            trace!(final_len = off, "kithara-file fetcher: commit");
            let _ = res.commit(Some(off)).await;
        });
    }
}

/// Shared progress state between fetcher (download loop) and reader (Seek+Read consumer).
///
/// Note: this is *only* for logging/debugging; it does not affect correctness.
#[derive(Debug)]
pub struct Progress {
    downloaded: std::sync::atomic::AtomicU64,
    read_pos: std::sync::atomic::AtomicU64,
}

impl Progress {
    pub fn new() -> Self {
        Self {
            downloaded: std::sync::atomic::AtomicU64::new(0),
            read_pos: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn set_downloaded(&self, v: u64) {
        self.downloaded
            .store(v, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_read_pos(&self, v: u64) {
        self.read_pos.store(v, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn read_pos(&self) -> u64 {
        self.read_pos.load(std::sync::atomic::Ordering::Relaxed)
    }
}
