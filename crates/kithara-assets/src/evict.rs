#![forbid(unsafe_code)]

use std::{collections::HashSet, path::Path, sync::Arc};

use async_trait::async_trait;
use kithara_storage::AtomicResource;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{
    base::{Assets, delete_asset_dir},
    error::AssetsResult,
    index::{EvictConfig, LruIndex, PinsIndex},
    key::ResourceKey,
};

/// Trait for recording asset bytes for eviction tracking.
#[async_trait]
pub trait ByteRecorder: Send + Sync {
    /// Record asset bytes and check if eviction is needed.
    async fn record_bytes(&self, asset_root: &str, bytes: u64);
}

/// A decorator that enforces LRU eviction with optional per-asset byte accounting.
///
/// This layer sits between `LeaseAssets` (pinning) and the concrete store (disk).
/// It does NOT wrap resources; it only intercepts `open_*_resource` calls to
/// perform eviction decisions at asset-creation time.
///
/// ## Normative
/// - Eviction is evaluated only when a new `asset_root` is observed (i.e., the first
///   `open_*_resource` for that root in this process).
/// - The decision uses the persisted LRU index, not in‑memory state (except for a
///   small “already seen” set to avoid reloading the index on every open).
/// - Pinned assets are excluded from eviction candidates.
/// - Both `max_assets` and `max_bytes` are soft caps enforced best‑effort.
/// - Byte accounting is best‑effort and must be explicitly updated via
///   `touch_asset_bytes`; the evictor does NOT walk the filesystem.
#[derive(Clone, Debug)]
pub struct EvictAssets<A>
where
    A: Assets,
{
    inner: Arc<A>,
    cfg: EvictConfig,
    seen: Arc<Mutex<HashSet<String>>>,
    cancel: CancellationToken,
}

impl<A> EvictAssets<A>
where
    A: Assets,
{
    pub(crate) fn new(inner: Arc<A>, cfg: EvictConfig, cancel: CancellationToken) -> Self {
        Self {
            inner,
            cfg,
            seen: Arc::new(Mutex::new(HashSet::new())),
            cancel,
        }
    }

    pub(crate) fn inner(&self) -> &A {
        &self.inner
    }

    /// Record asset size for byte-based eviction (best-effort).
    ///
    /// This is called automatically from commit() via callback.
    /// Can also be called manually if needed.
    pub async fn record_asset_bytes(&self, asset_root: &str, bytes: u64) -> AssetsResult<()> {
        tracing::debug!(asset_root = %asset_root, bytes, "Recording asset bytes");
        let lru = self.open_lru().await?;
        let _ = lru.touch(asset_root, Some(bytes)).await?;
        tracing::debug!(asset_root = %asset_root, bytes, "Asset bytes recorded");

        Ok(())
    }

    /// Check if byte limit is exceeded and run eviction if needed.
    pub async fn check_and_evict_if_over_limit(&self) {
        if self.cancel.is_cancelled() || self.cfg.max_bytes.is_none() {
            return;
        }

        let lru = match self.open_lru().await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("Failed to open LRU index: {:?}", e);
                return;
            }
        };

        let pins = match self.open_pins().await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("Failed to open pins index: {:?}", e);
                return;
            }
        };

        let pinned = match pins.load().await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("Failed to load pins: {:?}", e);
                return;
            }
        };

        let lru_state = match lru.load().await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("Failed to load LRU state: {:?}", e);
                return;
            }
        };

        let total_bytes = lru_state.total_bytes_best_effort();
        tracing::debug!(
            total_bytes,
            max_bytes = ?self.cfg.max_bytes,
            pinned = ?pinned,
            "check_and_evict_if_over_limit"
        );

        // Check candidates
        let candidates = match lru.eviction_candidates(&self.cfg, &pinned).await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("Failed to get eviction candidates: {:?}", e);
                return;
            }
        };

        tracing::debug!(candidates = ?candidates, "Eviction candidates selected");

        // Delete candidates
        for cand in candidates {
            if self.cancel.is_cancelled() {
                break;
            }

            if pinned.contains(&cand) {
                tracing::debug!(asset_root = %cand, "Skipping pinned asset");
                continue;
            }

            tracing::debug!(asset_root = %cand, "Attempting to delete asset");
            if delete_asset_dir(self.inner.root_dir(), &cand).await.is_ok() {
                tracing::debug!(asset_root = %cand, "Asset deleted successfully");
                let _ = lru.remove(&cand).await;
            } else {
                tracing::debug!(asset_root = %cand, "Failed to delete asset");
            }
        }
    }

    async fn open_lru(&self) -> AssetsResult<LruIndex> {
        let res = self.inner.open_lru_index_resource().await?;
        Ok(LruIndex::new(res))
    }

    async fn open_pins(&self) -> AssetsResult<PinsIndex> {
        PinsIndex::open(self.inner()).await
    }

    async fn mark_seen(&self, asset_root: &str) -> bool {
        let mut g = self.seen.lock().await;
        g.insert(asset_root.to_string())
    }

    async fn on_asset_created(&self, asset_root: &str) {
        if self.cancel.is_cancelled() {
            return;
        }

        // 1) Touch in LRU index (new asset already inserted by caller logic)
        // 2) If constraints exceeded, compute candidates and attempt deletions.
        let lru = match self.open_lru().await {
            Ok(v) => v,
            Err(_) => return,
        };

        let pins = match self.open_pins().await {
            Ok(v) => v,
            Err(_) => return,
        };

        let pinned = match pins.load().await {
            Ok(v) => v,
            Err(_) => return,
        };

        // Get LRU state to check its length
        let _lru_state = match lru.load().await {
            Ok(state) => state,
            Err(_) => return,
        };

        // Exclude the newly created asset from eviction candidates
        let mut pinned_with_new = pinned.clone();
        pinned_with_new.insert(asset_root.to_string());

        let candidates = match lru.eviction_candidates(&self.cfg, &pinned_with_new).await {
            Ok(v) => v,
            Err(_) => return,
        };

        for cand in candidates {
            if self.cancel.is_cancelled() {
                break;
            }

            // Re-check pinned set (best-effort) to avoid deleting newly pinned assets.
            if pinned.contains(&cand) {
                continue;
            }

            // Best-effort: delete directory, then best-effort remove from LRU.
            if delete_asset_dir(self.inner.root_dir(), &cand).await.is_ok() {
                let _ = lru.remove(&cand).await;
            }

            // If delete/remove fails, continue with other candidates.
        }
    }

    async fn touch_and_maybe_evict(&self, asset_root: &str, bytes_hint: Option<u64>) {
        if self.cancel.is_cancelled() {
            return;
        }

        // Fast path: if we've already seen this asset_root in this process, skip LRU operations.
        // mark_seen returns false if the asset_root was already in the set.
        if !self.mark_seen(asset_root).await {
            return;
        }

        // First time seeing this asset_root - update LRU and maybe evict.
        let lru = match self.open_lru().await {
            Ok(v) => v,
            Err(_) => return,
        };

        // Create LRU entry. Bytes will be recorded later via commit callback.
        let created = match lru.touch(asset_root, bytes_hint).await {
            Ok(created) => created,
            Err(_) => return,
        };

        if created {
            self.on_asset_created(asset_root).await;
        }
    }
}

#[async_trait]
impl<A> Assets for EvictAssets<A>
where
    A: Assets,
{
    type StreamingRes = A::StreamingRes;
    type AtomicRes = A::AtomicRes;
    type Context = A::Context;

    fn root_dir(&self) -> &Path {
        self.inner.root_dir()
    }

    fn asset_root(&self) -> &str {
        self.inner.asset_root()
    }

    async fn open_streaming_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::StreamingRes> {
        // Asset creation-time eviction decision happens before opening.
        self.touch_and_maybe_evict(self.inner.asset_root(), None)
            .await;

        // Delegate to base.
        self.inner.open_streaming_resource_with_ctx(key, ctx).await
    }

    async fn open_atomic_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::AtomicRes> {
        // Asset creation-time eviction decision happens before opening.
        //
        // Note: this decorator is about LRU/quotas and must not wrap/alter resources.
        // Pinning remains the responsibility of the `LeaseAssets` decorator.
        self.touch_and_maybe_evict(self.inner.asset_root(), None)
            .await;

        self.inner.open_atomic_resource_with_ctx(key, ctx).await
    }

    async fn open_pins_index_resource(&self) -> AssetsResult<AtomicResource> {
        self.inner.open_pins_index_resource().await
    }

    async fn open_lru_index_resource(&self) -> AssetsResult<AtomicResource> {
        self.inner.open_lru_index_resource().await
    }

    async fn delete_asset(&self) -> AssetsResult<()> {
        self.inner.delete_asset().await
    }
}

#[async_trait]
impl<A> ByteRecorder for EvictAssets<A>
where
    A: Assets,
{
    async fn record_bytes(&self, asset_root: &str, bytes: u64) {
        // Record bytes
        let _ = self.record_asset_bytes(asset_root, bytes).await;

        // Check if eviction is needed
        self.check_and_evict_if_over_limit().await;
    }
}
