#![forbid(unsafe_code)]

use std::{fmt::Debug, hash::Hash, path::Path};

use crate::{
    error::AssetsResult, identity::RequestIdentity, key::ResourceKey, state::AssetResourceState,
};

bitflags::bitflags! {
    /// Decorator capabilities advertised by a base store.
    ///
    /// Decorators check the relevant bit before activating their logic;
    /// when the bit is absent the decorator passes through to the inner layer.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Capabilities: u8 {
        /// In-process LRU handle cache (`CachedAssets`).
        const CACHE      = 0b0001;
        /// LRU eviction with per-asset byte accounting (`EvictAssets`).
        const EVICT      = 0b0010;
        /// RAII pin/lease semantics (`LeaseAssets`).
        const LEASE      = 0b0100;
        /// Chunk-based processing on commit (`ProcessingAssets`).
        const PROCESSING = 0b1000;
    }
}

/// Explicit public contract for the assets abstraction.
///
/// See crate `README.md` for the asset / resource / identity model.
pub trait Assets: Clone + Send + Sync + 'static {
    /// Context type for resource processing. Use `()` for no context.
    type Context: Clone + Send + Sync + Hash + Eq + Debug + 'static;
    /// Resource type for index persistence (pins, LRU).
    type IndexRes: kithara_storage::ResourceExt + Clone + Send + Sync + Debug + 'static;
    /// Type returned by `open_resource`. Must be Clone for caching.
    type Res: kithara_storage::ResourceExt + Clone + Send + Sync + Debug + 'static;

    /// Acquire a resource for mutation (no identity, no context).
    ///
    /// # Errors
    /// Returns `AssetsError` if the resource cannot be opened.
    fn acquire_resource(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
    ) -> AssetsResult<Self::Res> {
        self.acquire_resource_with_ctx(key, identity, None)
    }

    /// Acquire a resource for mutation with optional processing context.
    ///
    /// # Errors
    /// Returns `AssetsError` if the resource cannot be opened.
    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        self.open_resource_with_ctx(key, identity, ctx)
    }

    /// Decorator capabilities supported by this backend.
    #[must_use]
    fn capabilities(&self) -> Capabilities {
        Capabilities::all()
    }

    /// Delete the entire asset (all resources under `asset_root`).
    ///
    /// # Errors
    /// Returns `AssetsError` if the asset directory cannot be removed.
    fn delete_asset(&self, asset_root: &str) -> AssetsResult<()>;

    /// Open the resource used for persisting the LRU index.
    ///
    /// # Errors
    /// Returns `AssetsError` if the index resource cannot be opened.
    fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes>;

    /// Open the resource used for persisting the pins index.
    ///
    /// # Errors
    /// Returns `AssetsError` if the index resource cannot be opened.
    fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes>;

    /// Open a resource for read (no context).
    ///
    /// # Errors
    /// Returns `AssetsError` if the resource cannot be opened.
    fn open_resource(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
    ) -> AssetsResult<Self::Res> {
        self.open_resource_with_ctx(key, identity, None)
    }

    /// Open a resource for read with optional processing context.
    ///
    /// # Errors
    /// Returns `AssetsError` if the resource cannot be opened.
    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res>;

    /// Remove a single resource by `key`.
    ///
    /// # Errors
    /// Returns `AssetsError` if the resource cannot be removed.
    fn remove_resource(&self, _key: &ResourceKey) -> AssetsResult<()> {
        Ok(())
    }

    /// Inspect the current resource state without creating or mutating it.
    ///
    /// # Errors
    /// Returns `AssetsError` if the resource cannot be inspected.
    fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState>;

    /// Return the root directory for disk-backed implementations.
    fn root_dir(&self) -> &Path;
}
