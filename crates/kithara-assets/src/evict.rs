#![forbid(unsafe_code)]

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use kithara_storage::{AtomicResource, StreamingResource};
use tokio_util::sync::CancellationToken;

use crate::{
    cache::Assets,
    error::AssetsResult,
    index::{EvictConfig, LruIndex, PinsIndex},
    key::ResourceKey,
};

/// Eviction decorator over a base [`Assets`] implementation.
///
/// ## Normative behavior
/// - Eviction is evaluated **only** when a *new* asset is created (first time `asset_root` is seen),
///   not as a background task.
/// - LRU bookkeeping is stored on disk via `open_lru_index_resource`.
/// - Pins are stored on disk via `open_pins_index_resource`, and pinned assets must not be evicted.
/// - If a candidate is pinned, it is skipped.
/// - Best-effort: eviction failures must not prevent opening resources for the new asset; errors
///   are swallowed during eviction attempts.
///
/// ## Asset creation time (definition)
/// An asset is considered "created" when the decorator observes an `open_*_resource` call for an
/// `asset_root` that does not yet exist in the persisted LRU index. (Touching an existing asset
/// does not trigger eviction.)
///
/// ## max_bytes accounting
/// `max_bytes` is enforced based on best-effort bytes accounting stored in the LRU index, not by
/// scanning the filesystem. This decorator updates bytes on:
/// - atomic writes: size = last written length (whole-object)
/// - streaming commits: `final_len` if provided
///
/// If bytes are unknown for many assets, `max_bytes` becomes a soft hint.
#[derive(Clone)]
pub struct EvictAssets<A>
where
    A: Assets,
{
    base: Arc<A>,
    cfg: EvictConfig,

    // In-memory fast path: avoid re-triggering eviction on repeated opens for the same asset_root
    // within this process. Persistence is still the source of truth.
    seen: Arc<Mutex<HashSet<String>>>,
}

impl<A> EvictAssets<A>
where
    A: Assets,
{
    pub fn new(base: Arc<A>, cfg: EvictConfig) -> Self {
        Self {
            base,
            cfg,
            seen: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn base(&self) -> &A {
        &self.base
    }

    pub fn config(&self) -> &EvictConfig {
        &self.cfg
    }

    /// Explicitly update best-effort byte accounting for an asset.
    ///
    /// This is the MVP `max_bytes` integration:
    /// - higher layers call this when they know the asset size (e.g. after writing/committing),
    /// - we persist `bytes` into the LRU index via `touch(asset_root, Some(bytes))`.
    ///
    /// Normative:
    /// - this does NOT trigger eviction by itself; eviction is only evaluated on "asset creation"
    ///   (first `open_*_resource` for a new `asset_root`).
    pub async fn touch_asset_bytes(
        &self,
        asset_root: &str,
        bytes: u64,
        cancel: CancellationToken,
    ) -> AssetsResult<()> {
        let lru = self.open_lru(cancel).await?;
        let _created = lru.touch(asset_root, Some(bytes)).await?;
        Ok(())
    }

    async fn open_lru(&self, cancel: CancellationToken) -> AssetsResult<LruIndex> {
        let res = self.base.open_lru_index_resource(cancel).await?;
        Ok(LruIndex::new(res))
    }

    async fn open_pins(&self, cancel: CancellationToken) -> AssetsResult<PinsIndex> {
        PinsIndex::open(self.base(), cancel).await
    }

    fn mark_seen(&self, asset_root: &str) -> bool {
        let mut g = self.seen.lock().expect("evict.seen mutex poisoned");
        g.insert(asset_root.to_string())
    }

    async fn on_asset_created(&self, _asset_root: &str, cancel: CancellationToken) {
        // 1) Touch in LRU index (new asset already inserted by caller logic)
        // 2) If constraints exceeded, compute candidates and attempt deletions.
        let lru = match self.open_lru(cancel.clone()).await {
            Ok(v) => v,
            Err(_) => return,
        };

        let pins = match self.open_pins(cancel.clone()).await {
            Ok(v) => v,
            Err(_) => return,
        };

        let pinned = match pins.load().await {
            Ok(v) => v,
            Err(_) => return,
        };

        let candidates = match lru.eviction_candidates(&self.cfg, &pinned).await {
            Ok(v) => v,
            Err(_) => return,
        };

        for cand in candidates {
            // Re-check pinned set (best-effort) to avoid deleting newly pinned assets.
            if pinned.contains(&cand) {
                continue;
            }

            // Best-effort: delete directory, then best-effort remove from LRU.
            if self.base.delete_asset(&cand, cancel.clone()).await.is_ok() {
                let _ = lru.remove(&cand).await;
            }

            // If delete/remove fails, continue with other candidates.
        }
    }

    async fn touch_and_maybe_evict(
        &self,
        asset_root: &str,
        bytes_hint: Option<u64>,
        cancel: CancellationToken,
    ) {
        // Fast path: if we've already seen it in this process, avoid re-loading LRU on every open.
        // We still need to update bytes/touch ordering best-effort when possible.
        let lru = match self.open_lru(cancel.clone()).await {
            Ok(v) => v,
            Err(_) => return,
        };

        // Determine whether this is a "creation".
        // This uses persisted LRU, not the in-memory set.
        let created = match lru.touch(asset_root, bytes_hint).await {
            Ok(created) => created,
            Err(_) => return,
        };

        // Track it locally too.
        let _ = self.mark_seen(asset_root);

        if created {
            self.on_asset_created(asset_root, cancel).await;
        }
    }
}

#[async_trait]
impl<A> Assets for EvictAssets<A>
where
    A: Assets,
{
    async fn open_atomic_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> AssetsResult<AtomicResource> {
        // Asset creation-time eviction decision happens before opening.
        //
        // Note: this decorator is about LRU/quotas and must not wrap/alter resources.
        // Pinning remains the responsibility of the `LeaseAssets` decorator.
        self.touch_and_maybe_evict(&key.asset_root, None, cancel.clone())
            .await;

        self.base.open_atomic_resource(key, cancel).await
    }

    async fn open_streaming_resource(
        &self,
        key: &ResourceKey,
        cancel: CancellationToken,
    ) -> AssetsResult<StreamingResource> {
        // Asset creation-time eviction decision happens before opening.
        self.touch_and_maybe_evict(&key.asset_root, None, cancel.clone())
            .await;

        // Delegate to base.
        self.base.open_streaming_resource(key, cancel).await
    }

    async fn open_pins_index_resource(
        &self,
        cancel: CancellationToken,
    ) -> AssetsResult<AtomicResource> {
        self.base.open_pins_index_resource(cancel).await
    }

    async fn delete_asset(&self, asset_root: &str, cancel: CancellationToken) -> AssetsResult<()> {
        self.base.delete_asset(asset_root, cancel).await
    }

    async fn open_lru_index_resource(
        &self,
        cancel: CancellationToken,
    ) -> AssetsResult<AtomicResource> {
        self.base.open_lru_index_resource(cancel).await
    }
}
