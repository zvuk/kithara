#![forbid(unsafe_code)]

use std::{collections::HashSet, path::Path, sync::Arc};

use kithara_storage::StorageResource;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{
    base::{Assets, delete_asset_dir},
    error::AssetsResult,
    index::{EvictConfig, LruIndex, PinsIndex},
    key::ResourceKey,
};

/// Trait for recording asset bytes for eviction tracking.
pub trait ByteRecorder: Send + Sync {
    /// Record asset bytes and check if eviction is needed.
    fn record_bytes(&self, asset_root: &str, bytes: u64);
}

/// A decorator that enforces LRU eviction with optional per-asset byte accounting.
///
/// This layer sits between `LeaseAssets` (pinning) and the concrete store (disk).
/// It does NOT wrap resources; it only intercepts `open_resource` calls to
/// perform eviction decisions at asset-creation time.
///
/// ## Normative
/// - Eviction is evaluated only when a new `asset_root` is observed (i.e., the first
///   `open_resource` for that root in this process).
/// - The decision uses the persisted LRU index, not in-memory state (except for a
///   small "already seen" set to avoid reloading the index on every open).
/// - Pinned assets are excluded from eviction candidates.
/// - Both `max_assets` and `max_bytes` are soft caps enforced best-effort.
/// - Byte accounting is best-effort and must be explicitly updated via
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
    pub fn record_asset_bytes(&self, asset_root: &str, bytes: u64) -> AssetsResult<()> {
        tracing::debug!(asset_root = %asset_root, bytes, "Recording asset bytes");
        let lru = self.open_lru()?;
        let _ = lru.touch(asset_root, Some(bytes))?;
        tracing::debug!(asset_root = %asset_root, bytes, "Asset bytes recorded");
        Ok(())
    }

    /// Check if byte limit is exceeded and run eviction if needed.
    pub fn check_and_evict_if_over_limit(&self) {
        if self.cancel.is_cancelled() || self.cfg.max_bytes.is_none() {
            return;
        }

        let lru = match self.open_lru() {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("Failed to open LRU index: {:?}", e);
                return;
            }
        };

        let pins = match self.open_pins() {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("Failed to open pins index: {:?}", e);
                return;
            }
        };

        let pinned = match pins.load() {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("Failed to load pins: {:?}", e);
                return;
            }
        };

        let lru_state = match lru.load() {
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

        let candidates = match lru.eviction_candidates(&self.cfg, &pinned) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("Failed to get eviction candidates: {:?}", e);
                return;
            }
        };

        tracing::debug!(candidates = ?candidates, "Eviction candidates selected");

        for cand in candidates {
            if self.cancel.is_cancelled() {
                break;
            }

            if pinned.contains(&cand) {
                tracing::debug!(asset_root = %cand, "Skipping pinned asset");
                continue;
            }

            tracing::debug!(asset_root = %cand, "Attempting to delete asset");
            if delete_asset_dir(self.inner.root_dir(), &cand).is_ok() {
                tracing::debug!(asset_root = %cand, "Asset deleted successfully");
                let _ = lru.remove(&cand);
            } else {
                tracing::debug!(asset_root = %cand, "Failed to delete asset");
            }
        }
    }

    fn open_lru(&self) -> AssetsResult<LruIndex> {
        let res = self.inner.open_lru_index_resource()?;
        Ok(LruIndex::new(res))
    }

    fn open_pins(&self) -> AssetsResult<PinsIndex> {
        PinsIndex::open(self.inner())
    }

    fn mark_seen(&self, asset_root: &str) -> bool {
        let mut g = self.seen.lock();
        g.insert(asset_root.to_string())
    }

    fn on_asset_created(&self, asset_root: &str) {
        if self.cancel.is_cancelled() {
            return;
        }

        let lru = match self.open_lru() {
            Ok(v) => v,
            Err(_) => return,
        };

        let pins = match self.open_pins() {
            Ok(v) => v,
            Err(_) => return,
        };

        let pinned = match pins.load() {
            Ok(v) => v,
            Err(_) => return,
        };

        let _lru_state = match lru.load() {
            Ok(state) => state,
            Err(_) => return,
        };

        let mut pinned_with_new = pinned.clone();
        pinned_with_new.insert(asset_root.to_string());

        let candidates = match lru.eviction_candidates(&self.cfg, &pinned_with_new) {
            Ok(v) => v,
            Err(_) => return,
        };

        for cand in candidates {
            if self.cancel.is_cancelled() {
                break;
            }

            if pinned.contains(&cand) {
                continue;
            }

            if delete_asset_dir(self.inner.root_dir(), &cand).is_ok() {
                let _ = lru.remove(&cand);
            }
        }
    }

    fn touch_and_maybe_evict(&self, asset_root: &str, bytes_hint: Option<u64>) {
        if self.cancel.is_cancelled() {
            return;
        }

        if !self.mark_seen(asset_root) {
            return;
        }

        let lru = match self.open_lru() {
            Ok(v) => v,
            Err(_) => return,
        };

        let created = match lru.touch(asset_root, bytes_hint) {
            Ok(created) => created,
            Err(_) => return,
        };

        if created {
            self.on_asset_created(asset_root);
        }
    }
}

impl<A> Assets for EvictAssets<A>
where
    A: Assets,
{
    type Res = A::Res;
    type Context = A::Context;

    fn root_dir(&self) -> &Path {
        self.inner.root_dir()
    }

    fn asset_root(&self) -> &str {
        self.inner.asset_root()
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        self.touch_and_maybe_evict(self.inner.asset_root(), None);
        self.inner.open_resource_with_ctx(key, ctx)
    }

    fn open_pins_index_resource(&self) -> AssetsResult<StorageResource> {
        self.inner.open_pins_index_resource()
    }

    fn open_lru_index_resource(&self) -> AssetsResult<StorageResource> {
        self.inner.open_lru_index_resource()
    }

    fn delete_asset(&self) -> AssetsResult<()> {
        self.inner.delete_asset()
    }
}

impl<A> ByteRecorder for EvictAssets<A>
where
    A: Assets,
{
    fn record_bytes(&self, asset_root: &str, bytes: u64) {
        let _ = self.record_asset_bytes(asset_root, bytes);
        self.check_and_evict_if_over_limit();
    }
}
