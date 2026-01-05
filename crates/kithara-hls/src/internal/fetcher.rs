#![forbid(unsafe_code)]

use std::collections::HashMap;

use futures::StreamExt;
use kithara_assets::{AssetResource, AssetStore, ResourceKey};
use kithara_storage::{Resource, StreamingResource, StreamingResourceExt};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};
use url::Url;

use crate::{FetchManager, HlsError, HlsResult};

type Lease =
    kithara_assets::LeaseGuard<kithara_assets::EvictAssets<kithara_assets::DiskAssetStore>>;
type Res = AssetResource<StreamingResource, Lease>;

/// Shared-handle download manager.
///
/// Why this exists:
/// `kithara-storage::StreamingResource` tracks "available ranges" in-memory (per handle).
/// If you open the same disk path twice, the second handle does not automatically observe
/// the first handle's `write_at`/`commit` progress. Therefore, any "file-like" reader must
/// wait/read from the *same* `StreamingResource` handle that the writer updates.
///
/// This type:
/// - opens a `StreamingResource` once per `(asset_root, rel_path)`
/// - spawns a best-effort writer task once
/// - returns a reusable handle for waiting/reading
pub struct Fetcher {
    assets: AssetStore,
    asset_root: String,
    net: kithara_net::HttpClient,
    variant_index: usize,

    // rel_path -> shared handle (writer updates this, readers wait/read this).
    res_by_rel_path: Mutex<HashMap<String, Res>>,
}

impl Fetcher {
    pub fn new(
        assets: AssetStore,
        asset_root: String,
        net: kithara_net::HttpClient,
        variant_index: usize,
    ) -> Self {
        Self {
            assets,
            asset_root,
            net,
            variant_index,
            res_by_rel_path: Mutex::new(HashMap::new()),
        }
    }

    pub fn variant_index(&self) -> usize {
        self.variant_index
    }

    /// Ensure a streaming resource exists for `rel_path` and that a writer has been started.
    ///
    /// Returns the shared handle that the writer will update.
    pub async fn get_or_start(&self, url: &Url, rel_path: &str) -> HlsResult<Res> {
        // Fast path: already opened.
        {
            let guard = self.res_by_rel_path.lock().await;
            if let Some(res) = guard.get(rel_path) {
                return Ok(res.clone());
            }
        }

        // Slow path: open, insert, then start writer.
        let key = ResourceKey::new(self.asset_root.clone(), rel_path.to_string());
        let cancel = CancellationToken::new();
        let res = self.assets.open_streaming_resource(&key, cancel).await?;

        {
            let mut guard = self.res_by_rel_path.lock().await;
            // If another task won the race, use the cached one.
            if let Some(existing) = guard.get(rel_path) {
                return Ok(existing.clone());
            }
            guard.insert(rel_path.to_string(), res.clone());
        }

        self.spawn_writer(url.clone(), rel_path.to_string(), res.clone());

        Ok(res)
    }

    /// Spawn a best-effort background writer that streams bytes from network into `res`.
    ///
    /// Multiple writers for same rel_path are not expected when using `get_or_start`, but even if
    /// they happen (races), the resource contract handles sealing/failure propagation.
    fn spawn_writer(&self, url: Url, rel_path: String, res: Res) {
        let net = self.net.clone();
        let url_for_logs = url.clone();
        let asset_root = self.asset_root.clone();

        debug!(
            asset_root = %asset_root,
            rel_path = %rel_path,
            url = %url_for_logs,
            "kithara-hls internal fetcher: spawning segment writer"
        );

        tokio::spawn(async move {
            debug!(url = %url, "kithara-hls internal segment writer: started");

            let mut stream = match net.stream(url.clone(), None).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(url = %url, error = %e, "kithara-hls internal segment writer: net error");
                    let _ = res.fail(format!("net error: {e}")).await;
                    return;
                }
            };

            let mut off: u64 = 0;
            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk_bytes) => {
                        let bytes_len = chunk_bytes.len();
                        trace!(
                            url = %url,
                            off,
                            bytes = bytes_len,
                            "kithara-hls internal segment writer: write_at"
                        );
                        if let Err(e) = res.write_at(off, &chunk_bytes).await {
                            warn!(
                                url = %url,
                                off,
                                error = %e,
                                "kithara-hls internal segment writer: storage write_at error"
                            );
                            let _ = res.fail(format!("storage write_at error: {e}")).await;
                            return;
                        }
                        off = off.saturating_add(chunk_bytes.len() as u64);
                    }
                    Err(e) => {
                        warn!(
                            url = %url,
                            off,
                            error = %e,
                            "kithara-hls internal segment writer: net stream error"
                        );
                        let _ = res.fail(format!("net stream error: {e}")).await;
                        return;
                    }
                }
            }

            debug!(
                url = %url,
                final_len = off,
                "kithara-hls internal segment writer: committing"
            );
            let _ = res.commit(Some(off)).await;
            debug!(
                url = %url,
                final_len = off,
                "kithara-hls internal segment writer: committed"
            );
        });
    }

    /// Best-effort init segment trigger.
    ///
    /// Returns the shared handle for the segment, so callers can `wait_range`/`read_at` on it.
    pub async fn trigger_init(&self, url: &Url) -> HlsResult<(String, Res)> {
        let keys = crate::CacheKeyGenerator::new(url);
        let full_rel = keys
            .init_segment_rel_path_from_url(self.variant_index, url)
            .ok_or_else(|| HlsError::InvalidUrl("Failed to derive init segment basename".into()))?;

        // Strip "<master_hash>/" if present; assets already scopes under asset_root.
        let rel_path = full_rel
            .strip_prefix(&format!("{}/", self.asset_root))
            .unwrap_or(full_rel.as_str())
            .to_string();

        let res = self.get_or_start(url, &rel_path).await?;
        Ok((rel_path, res))
    }

    /// Best-effort media segment trigger.
    ///
    /// Returns the shared handle for the segment, so callers can `wait_range`/`read_at` on it.
    pub async fn trigger_media(&self, url: &Url) -> HlsResult<(String, Res)> {
        let keys = crate::CacheKeyGenerator::new(url);
        let full_rel = keys
            .media_segment_rel_path_from_url(self.variant_index, url)
            .ok_or_else(|| HlsError::InvalidUrl("Failed to derive segment basename".into()))?;

        let rel_path = full_rel
            .strip_prefix(&format!("{}/", self.asset_root))
            .unwrap_or(full_rel.as_str())
            .to_string();

        let res = self.get_or_start(url, &rel_path).await?;
        Ok((rel_path, res))
    }

    /// Probe `Content-Length` for a URL (used to precompute segment lengths / total length).
    pub async fn content_length(&self, url: &Url) -> HlsResult<u64> {
        let fm = FetchManager::new(
            self.asset_root.clone(),
            self.assets.clone(),
            self.net.clone(),
        );
        fm.probe_content_length(url)
            .await?
            .ok_or_else(|| HlsError::Driver("Content-Length is unknown".into()))
    }

    /// Open an already-known rel_path handle if it exists (no I/O, no open).
    pub async fn get_cached(&self, rel_path: &str) -> Option<Res> {
        let guard = self.res_by_rel_path.lock().await;
        guard.get(rel_path).cloned()
    }
}
