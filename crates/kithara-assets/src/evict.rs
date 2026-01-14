#![forbid(unsafe_code)]

use std::{
    collections::HashSet,
    path::Path,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use kithara_storage::{AtomicResource, StorageError, StreamingResource};
use tokio_util::sync::CancellationToken;

use crate::{
    cache::Assets,
    error::AssetsResult,
    index::{EvictConfig, LruIndex, PinsIndex},
    key::ResourceKey,
    store::delete_asset_dir,
};

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
    base: Arc<A>,
    cfg: EvictConfig,
    seen: Arc<Mutex<HashSet<String>>>,
    cancel: CancellationToken,
}

impl<A> EvictAssets<A>
where
    A: Assets,
{
    pub(crate) fn new(base: Arc<A>, cfg: EvictConfig, cancel: CancellationToken) -> Self {
        Self {
            base,
            cfg,
            seen: Arc::new(Mutex::new(HashSet::new())),
            cancel,
        }
    }

    pub(crate) fn base(&self) -> &A {
        &self.base
    }

    /// Explicitly record the byte size of an asset in the LRU index.
    ///
    /// This is a separate call because the evictor does not know the actual size
    /// of an asset (it does not walk the filesystem). Higher layers must call
    /// this after they have written the asset’s data.
    ///
    /// Normative:
    /// - this does NOT trigger eviction by itself; eviction is only evaluated on "asset creation"
    ///   (first `open_*_resource` for a new `asset_root`).
    pub async fn touch_asset_bytes(&self, asset_root: &str, bytes: u64) -> AssetsResult<()> {
        if self.cancel.is_cancelled() {
            return Err(StorageError::Cancelled.into());
        }

        let lru = self.open_lru().await?;
        let _created = lru.touch(asset_root, Some(bytes)).await?;
        Ok(())
    }

    async fn open_lru(&self) -> AssetsResult<LruIndex> {
        let res = self.base.open_lru_index_resource().await?;
        Ok(LruIndex::new(res))
    }

    async fn open_pins(&self) -> AssetsResult<PinsIndex> {
        PinsIndex::open(self.base()).await
    }

    fn mark_seen(&self, asset_root: &str) -> bool {
        let mut g = self.seen.lock().expect("evict.seen mutex poisoned");
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
            if delete_asset_dir(self.base.root_dir(), &cand).await.is_ok() {
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
        if !self.mark_seen(asset_root) {
            return;
        }

        // First time seeing this asset_root - update LRU and maybe evict.
        let lru = match self.open_lru().await {
            Ok(v) => v,
            Err(_) => return,
        };

        // Determine whether this is a "creation" (new entry in persisted LRU).
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
    fn root_dir(&self) -> &Path {
        self.base.root_dir()
    }

    fn asset_root(&self) -> &str {
        self.base.asset_root()
    }

    async fn open_atomic_resource(&self, key: &ResourceKey) -> AssetsResult<AtomicResource> {
        // Asset creation-time eviction decision happens before opening.
        //
        // Note: this decorator is about LRU/quotas and must not wrap/alter resources.
        // Pinning remains the responsibility of the `LeaseAssets` decorator.
        self.touch_and_maybe_evict(self.base.asset_root(), None)
            .await;

        self.base.open_atomic_resource(key).await
    }

    async fn open_streaming_resource(&self, key: &ResourceKey) -> AssetsResult<StreamingResource> {
        // Asset creation-time eviction decision happens before opening.
        self.touch_and_maybe_evict(self.base.asset_root(), None)
            .await;

        // Delegate to base.
        self.base.open_streaming_resource(key).await
    }

    async fn open_pins_index_resource(&self) -> AssetsResult<AtomicResource> {
        self.base.open_pins_index_resource().await
    }

    async fn open_lru_index_resource(&self) -> AssetsResult<AtomicResource> {
        self.base.open_lru_index_resource().await
    }

    async fn delete_asset(&self) -> AssetsResult<()> {
        self.base.delete_asset().await
    }
}
