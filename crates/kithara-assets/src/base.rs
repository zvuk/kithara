#![forbid(unsafe_code)]

use std::{fmt::Debug, hash::Hash, path::Path};

use crate::{error::AssetsResult, key::ResourceKey, state::AssetResourceState};

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
/// `kithara-assets` is about *assets* and their *resources*:
/// - an **asset** is a logical unit that may consist of multiple files/resources,
/// - a **resource** is addressed by [`ResourceKey`] and is opened as a unified
///   resource supporting both streaming and atomic access patterns.
///
/// The `Context` associated type allows decorators to pass additional processing
/// context (e.g. encryption info) when opening resources. Use `()` for no context.
///
/// `IndexRes` is the resource type used for internal index persistence (pins, LRU).
/// Disk-backed stores use `MmapResource`; in-memory stores use `MemResource`.
pub trait Assets: Clone + Send + Sync + 'static {
    /// Type returned by `open_resource`. Must be Clone for caching.
    type Res: kithara_storage::ResourceExt + Clone + Send + Sync + Debug + 'static;
    /// Context type for resource processing. Use `()` for no context.
    type Context: Clone + Send + Sync + Hash + Eq + Debug + 'static;
    /// Resource type for index persistence (pins, LRU).
    type IndexRes: kithara_storage::ResourceExt + Clone + Send + Sync + Debug + 'static;

    /// Decorator capabilities supported by this backend.
    ///
    /// Decorators check the relevant [`Capabilities`] bit before activating.
    /// The default returns all capabilities enabled.
    #[must_use]
    fn capabilities(&self) -> Capabilities {
        Capabilities::all()
    }

    /// Open a resource with optional context (main method).
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the resource key is invalid or the underlying
    /// storage cannot be opened (e.g. I/O failure, cancellation).
    fn open_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res>;

    /// Convenience method - open a resource without context.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the resource key is invalid or the underlying
    /// storage cannot be opened.
    fn open_resource(&self, key: &ResourceKey) -> AssetsResult<Self::Res> {
        self.open_resource_with_ctx(key, None)
    }

    /// Acquire a resource for mutation.
    ///
    /// Backends may create the resource when it does not exist yet.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the resource key is invalid or the underlying
    /// storage cannot be opened or created.
    fn acquire_resource_with_ctx(
        &self,
        key: &ResourceKey,
        ctx: Option<Self::Context>,
    ) -> AssetsResult<Self::Res> {
        self.open_resource_with_ctx(key, ctx)
    }

    /// Convenience method - acquire a resource without context.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the resource key is invalid or the underlying
    /// storage cannot be opened or created.
    fn acquire_resource(&self, key: &ResourceKey) -> AssetsResult<Self::Res> {
        self.acquire_resource_with_ctx(key, None)
    }

    /// Inspect the current resource state without creating or mutating it.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the resource key is invalid or the backend
    /// cannot inspect the resource.
    fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState>;

    /// Open the resource used for persisting the pins index.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the index resource cannot be opened (I/O or storage error).
    fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes>;

    /// Open the resource used for persisting the LRU index.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the index resource cannot be opened (I/O or storage error).
    fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes>;

    /// Delete the entire asset (all resources under this store's `asset_root`).
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the asset directory cannot be removed (I/O error
    /// or cancellation).
    fn delete_asset(&self) -> AssetsResult<()>;

    /// Remove a single resource by key.
    ///
    /// Default implementation is a no-op (suitable for disk stores where
    /// resources are managed by filesystem eviction). In-memory stores
    /// override this to free memory.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the resource cannot be removed from the store.
    fn remove_resource(&self, _key: &ResourceKey) -> AssetsResult<()> {
        Ok(())
    }

    /// Return the root directory for disk-backed implementations.
    fn root_dir(&self) -> &Path;

    /// Return the asset root identifier for this store.
    fn asset_root(&self) -> &str;
}
