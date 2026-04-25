#![forbid(unsafe_code)]

use std::{collections::HashSet, fmt, path::Path, sync::Arc};

use kithara_platform::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{
    AssetResourceState,
    base::Assets,
    deleter::AssetDeleter,
    error::AssetsResult,
    index::{EvictConfig, LruIndex, PinsIndex},
    key::ResourceKey,
};

/// Trait for recording asset bytes for eviction tracking.
#[cfg_attr(test, unimock::unimock(api = ByteRecorderMock))]
pub(crate) trait ByteRecorder: Send + Sync {
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
/// - The decision uses the in-memory snapshots of [`LruIndex`] and [`PinsIndex`]
///   (which are themselves backed by best-effort disk persistence).
/// - Pinned assets are excluded from eviction candidates.
/// - Both `max_assets` and `max_bytes` are soft caps enforced best-effort.
/// - Byte accounting is best-effort and must be explicitly updated via
///   `touch_asset_bytes`; the evictor does NOT walk the filesystem.
/// - When `enabled` is `false`, all operations delegate directly to the inner layer.
#[derive(Clone)]
pub struct EvictAssets<A>
where
    A: Assets,
{
    inner: Arc<A>,
    cfg: EvictConfig,
    seen: Arc<Mutex<HashSet<String>>>,
    cancel: CancellationToken,
    /// Shared LRU index — same instance held by `DiskAssetDeleter` so
    /// LRU bookkeeping and disk-side deletion stay in sync.
    lru: LruIndex,
    /// Shared pins index — same instance used by `LeaseAssets` for
    /// pin/unpin lifecycle and by `DiskAssetDeleter` for full-asset
    /// removal cleanup.
    pins: PinsIndex,
    /// Single canonical removal channel — see [`crate::deleter`].
    deleter: Arc<dyn AssetDeleter>,
}

impl<A: Assets> fmt::Debug for EvictAssets<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvictAssets")
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

impl<A> EvictAssets<A>
where
    A: Assets,
{
    /// Create a new eviction decorator.
    ///
    /// Activation is driven by [`crate::base::Capabilities::EVICT`] on the inner store.
    pub(crate) fn new(
        inner: Arc<A>,
        cfg: EvictConfig,
        cancel: CancellationToken,
        lru: LruIndex,
        pins: PinsIndex,
        deleter: Arc<dyn AssetDeleter>,
    ) -> Self {
        Self {
            inner,
            cfg,
            seen: Arc::new(Mutex::new(HashSet::new())),
            cancel,
            lru,
            pins,
            deleter,
        }
    }

    fn is_active(&self) -> bool {
        self.inner
            .capabilities()
            .contains(crate::base::Capabilities::EVICT)
    }

    /// Record asset size for byte-based eviction.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the LRU index cannot persist the
    /// touch — caller must decide whether the lost durability is
    /// acceptable for its scenario (the [`ByteRecorder`] adapter
    /// downgrades it to a `tracing::warn`).
    pub fn record_asset_bytes(&self, asset_root: &str, bytes: u64) -> AssetsResult<()> {
        if !self.is_active() {
            return Ok(());
        }
        tracing::debug!(asset_root = %asset_root, bytes, "Recording asset bytes");
        self.lru.touch(asset_root, Some(bytes))?;
        tracing::debug!(asset_root = %asset_root, bytes, "Asset bytes recorded");
        Ok(())
    }

    /// Check if byte limit is exceeded and run eviction if needed.
    pub fn check_and_evict_if_over_limit(&self) {
        if !self.is_active() || self.cancel.is_cancelled() || self.cfg.max_bytes.is_none() {
            return;
        }

        let pinned = self.pins.snapshot();
        let total_bytes = self.lru.total_bytes_best_effort();
        tracing::debug!(
            total_bytes,
            max_bytes = ?self.cfg.max_bytes,
            pinned = ?pinned,
            "check_and_evict_if_over_limit"
        );

        let candidates = self.lru.eviction_candidates(&self.cfg, &pinned);

        tracing::debug!(candidates = ?candidates, "Eviction candidates selected");
        for cand in candidates {
            if self.cancel.is_cancelled() {
                break;
            }
            self.evict_one(&pinned, &cand);
        }
    }

    fn evict_one(&self, pinned: &HashSet<String>, cand: &str) {
        if pinned.contains(cand) {
            tracing::debug!(asset_root = %cand, "Skipping pinned asset");
            return;
        }
        // Single canonical removal channel: deleter clears FS,
        // `AvailabilityIndex`, `PinsIndex`, and `LruIndex` together.
        // See [`crate::deleter`].
        log_eviction_outcome(cand, self.deleter.delete_asset(cand));
    }

    fn mark_seen(&self, asset_root: &str) -> bool {
        let mut g = self.seen.lock_sync();
        g.insert(asset_root.to_string())
    }

    fn on_asset_created(&self, asset_root: &str) {
        if self.cancel.is_cancelled() {
            return;
        }

        let pinned = self.pins.snapshot();
        let mut pinned_with_new = pinned.clone();
        pinned_with_new.insert(asset_root.to_string());

        let candidates = self.lru.eviction_candidates(&self.cfg, &pinned_with_new);

        for cand in candidates {
            if self.cancel.is_cancelled() {
                break;
            }
            if pinned.contains(&cand) {
                continue;
            }
            // Same canonical channel — deleter wipes FS and all indexes.
            let _ = self.deleter.delete_asset(&cand);
        }
    }

    fn touch_and_maybe_evict(&self, asset_root: &str, bytes_hint: Option<u64>) {
        if !self.is_active() || self.cancel.is_cancelled() {
            return;
        }

        if !self.mark_seen(asset_root) {
            return;
        }

        match self.lru.touch(asset_root, bytes_hint) {
            Ok(true) => self.on_asset_created(asset_root),
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(asset_root, error = %e, "touch_and_maybe_evict: lru touch failed");
            }
        }
    }
}

impl<A> Assets for EvictAssets<A>
where
    A: Assets,
{
    type Res = A::Res;
    type Context = A::Context;
    type IndexRes = A::IndexRes;

    delegate::delegate! {
        to self.inner {
            fn capabilities(&self) -> crate::base::Capabilities;
            fn root_dir(&self) -> &Path;
            fn asset_root(&self) -> &str;
            fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes>;
            fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes>;
            fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState>;
            fn delete_asset(&self) -> AssetsResult<()>;
            fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()>;
        }
    }

    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        self.touch_and_maybe_evict(self.inner.asset_root(), None);
        self.inner.open_resource_with_ctx(key, ctx)
    }

    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        self.touch_and_maybe_evict(self.inner.asset_root(), None);
        self.inner.acquire_resource_with_ctx(key, ctx)
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

/// Free helper: log the outcome of a deleter-driven eviction.
///
/// Extracted so the calling site stays a one-liner — keeps
/// `evict_one`'s body focused on the policy decision (pin check,
/// dispatch) instead of branching on the result for tracing.
fn log_eviction_outcome(asset_root: &str, result: AssetsResult<()>) {
    match result {
        Ok(()) => tracing::debug!(%asset_root, "Asset deleted successfully"),
        Err(error) => tracing::debug!(%asset_root, %error, "Failed to delete asset"),
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn byte_recorder_mock_api_is_generated() {
        let _ = ByteRecorderMock::record_bytes;
    }
}
