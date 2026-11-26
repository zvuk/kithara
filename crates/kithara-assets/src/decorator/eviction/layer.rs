#![forbid(unsafe_code)]

use std::{collections::HashSet, path::Path};

use dashmap::DashSet;
use kithara_bufpool::HasPool;
use kithara_events::EventBus;
use kithara_platform::{CancelToken, sync::Arc};

use crate::{
    AssetEvent, EvictReason,
    backend::AssetDeleter,
    decorator::{Assets, Capabilities},
    error::AssetsResult,
    index::{EvictConfig, LruIndex, PinsIndex},
    layout::ResourceKey,
    resource::{AcquisitionResult, AssetResourceState, RequestIdentity},
    store::{AssetStoreConfig, AssetStoreConfigOwnerAccess},
};

mod kithara {
    pub(crate) use kithara_test_macros::probe;
}

#[derive(Clone, Default)]
pub(crate) struct EvictionEvents {
    bus: Option<EventBus>,
}

impl EvictionEvents {
    pub(crate) const fn new(bus: Option<EventBus>) -> Self {
        Self { bus }
    }

    fn publish_evicted(&self, published: bool, asset_root: &str, reason: EvictReason) {
        if !published {
            return;
        }
        let Some(bus) = &self.bus else {
            return;
        };
        bus.publish(AssetEvent::Evicted {
            reason,
            asset_root: asset_root.to_string(),
        });
    }

    fn publish_evicted_assets(&self, published: bool, asset_root: &str) {
        self.publish_evicted(published, asset_root, EvictReason::QuotaAssets);
    }

    fn publish_evicted_bytes(&self, published: bool, asset_root: &str) {
        self.publish_evicted(published, asset_root, EvictReason::QuotaBytes);
    }
}

/// Trait for recording asset bytes for eviction tracking.
pub(crate) trait ByteRecorder: Send + Sync {
    /// Record asset bytes and check if eviction is needed.
    fn record_bytes(&self, asset_root: &str, bytes: u64);
}

/// Evict unpinned assets on the first open of each root, between the lease and store layers.
/// Uses in-memory LRU/pin snapshots and best-effort asset/byte caps; resources are not wrapped.
/// Byte accounting requires explicit updates and never walks the filesystem.
/// When disabled, every operation delegates directly to the inner store.
#[derive_where::derive_where(Clone; A: Assets)]
#[derive(derive_more::Debug, kithara_config::ConfigOwner)]
#[debug("EvictAssets {{ .. }}")]
#[config_owner(AssetStoreConfig<S>, config)]
pub struct EvictAssets<A, S>
where
    A: Assets,
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Single canonical removal channel — see [`crate::backend::AssetDeleter`].
    deleter: Arc<dyn AssetDeleter>,
    inner: Arc<A>,
    seen: Arc<DashSet<String>>,
    cancel: CancelToken,
    config: Arc<AssetStoreConfig<S>>,
    events: EvictionEvents,
    /// Shared LRU index — same instance held by `DiskAssetDeleter` so
    /// LRU bookkeeping and disk-side deletion stay in sync.
    lru: LruIndex,
    /// Shared pins index — same instance used by `LeaseAssets` for
    /// pin/unpin lifecycle and by `DiskAssetDeleter` for full-asset
    /// removal cleanup.
    pins: PinsIndex,
}

/// Eviction wiring for [`EvictAssets::new`], separate from the wrapped
/// `inner` store: the retained config, the `cancel` token, the shared `lru` / `pins`
/// indices, and the canonical `deleter`.
pub(crate) struct EvictDeps<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub(crate) deleter: Arc<dyn AssetDeleter>,
    pub(crate) cancel: CancelToken,
    pub(crate) config: Arc<AssetStoreConfig<S>>,
    pub(crate) events: EvictionEvents,
    pub(crate) lru: LruIndex,
    pub(crate) pins: PinsIndex,
}

impl<A, S> EvictAssets<A, S>
where
    A: Assets,
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Create a new eviction decorator.
    ///
    /// Activation is driven by [`Capabilities::EVICT`] on the inner store.
    pub(crate) fn new(inner: Arc<A>, deps: EvictDeps<S>) -> Self {
        let EvictDeps {
            config,
            cancel,
            events,
            lru,
            pins,
            deleter,
        } = deps;
        Self {
            inner,
            config,
            cancel,
            events,
            lru,
            pins,
            deleter,
            seen: Arc::new(DashSet::new()),
        }
    }

    fn evict_config(&self) -> EvictConfig {
        EvictConfig {
            max_assets: self.max_assets(),
            max_bytes: self.max_bytes(),
        }
    }

    /// Check if byte limit is exceeded and run eviction if needed.
    #[kithara::probe(pinned = self.pins.snapshot().len())]
    pub fn check_and_evict_if_over_limit(&self) {
        if !self.is_active() || self.cancel.is_cancelled() || self.max_bytes().is_none() {
            return;
        }

        let pinned = self.pins.snapshot();
        let total_bytes = self.lru.total_bytes_best_effort();
        tracing::debug!(
            total_bytes,
            max_bytes = ?self.max_bytes(),
            pinned = ?pinned,
            "check_and_evict_if_over_limit"
        );

        let candidates = self.lru.eviction_candidates(&self.evict_config(), &pinned);

        tracing::debug!(candidates = ?candidates, "Eviction candidates selected");
        for cand in candidates {
            if self.cancel.is_cancelled() {
                break;
            }
            self.evict_one(&pinned, &cand);
        }
    }

    #[kithara::probe(is_pinned = pinned.contains(cand))]
    fn evict_one(&self, pinned: &HashSet<String>, cand: &str) {
        if pinned.contains(cand) {
            tracing::debug!(asset_root = %cand, "Skipping pinned asset");
            return;
        }
        let result = self.deleter.delete_asset(cand);
        self.events.publish_evicted_bytes(result.is_ok(), cand);
        log_eviction_outcome(cand, result);
    }

    fn is_active(&self) -> bool {
        self.inner.capabilities().contains(Capabilities::EVICT)
    }

    fn mark_seen(&self, asset_root: &str) -> bool {
        self.seen.insert(asset_root.to_string())
    }

    fn on_asset_created(&self, asset_root: &str) {
        if self.cancel.is_cancelled() {
            return;
        }

        let pinned = self.pins.snapshot();
        let mut pinned_with_new = pinned.clone();
        pinned_with_new.insert(asset_root.to_string());

        let candidates = self
            .lru
            .eviction_candidates(&self.evict_config(), &pinned_with_new);

        for cand in candidates {
            if self.cancel.is_cancelled() {
                break;
            }
            if pinned.contains(&cand) {
                continue;
            }
            let result = self.deleter.delete_asset(&cand);
            self.events.publish_evicted_assets(result.is_ok(), &cand);
        }
    }

    /// Record bytes without changing recency: per-segment touches would skew eviction order.
    /// Unknown or concurrently evicted roots are ignored.
    pub fn record_asset_bytes(&self, asset_root: &str, bytes: u64) {
        if !self.is_active() {
            return;
        }
        tracing::debug!(asset_root = %asset_root, bytes, "Recording asset bytes");
        self.lru.update_bytes(asset_root, bytes);
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

impl<A, S> Assets for EvictAssets<A, S>
where
    A: Assets,
    S: HasPool<u8> + Send + Sync + 'static,
{
    type ActiveRes = A::ActiveRes;
    type Context = A::Context;
    type ReadyRes = A::ReadyRes;

    #[kithara::probe]
    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<AcquisitionResult<Self::ActiveRes, Self::ReadyRes>> {
        if let Some(asset_root) = key.asset_root() {
            self.touch_and_maybe_evict(asset_root, None);
        }
        self.inner.acquire_resource_with_ctx(key, identity, ctx)
    }

    #[kithara::probe]
    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::ReadyRes> {
        if let Some(asset_root) = key.asset_root() {
            self.touch_and_maybe_evict(asset_root, None);
        }
        self.inner.open_resource_with_ctx(key, identity, ctx)
    }

    delegate::delegate! {
        to self.inner {
            fn capabilities(&self) -> Capabilities;
            fn root_dir(&self) -> &Path;
            fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState>;
            fn delete_asset(&self, asset_root: &str) -> AssetsResult<()>;
            fn remove_resource(&self, key: &ResourceKey) -> AssetsResult<()>;
        }
    }
}

impl<A, S> ByteRecorder for EvictAssets<A, S>
where
    A: Assets,
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn record_bytes(&self, asset_root: &str, bytes: u64) {
        self.record_asset_bytes(asset_root, bytes);
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
