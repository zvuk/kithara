#![forbid(unsafe_code)]

use std::{fmt::Debug, hash::Hash, path::Path};

use crate::{error::AssetsResult, key::ResourceKey};

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

    /// Whether this backend supports eviction decorator semantics.
    ///
    /// Backends that cannot persist or interpret eviction metadata should return `false`.
    #[must_use]
    fn supports_evict(&self) -> bool {
        true
    }

    /// Whether this backend supports lease/pin decorator semantics.
    ///
    /// Backends that cannot persist or interpret pin metadata should return `false`.
    #[must_use]
    fn supports_lease(&self) -> bool {
        true
    }

    /// Whether this backend supports in-process resource/index caching.
    ///
    /// Backends that already provide equivalent reuse semantics or where
    /// caching is undesirable should return `false`.
    #[must_use]
    fn supports_cache(&self) -> bool {
        true
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

    /// Open the resource used for persisting the coverage index.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the index resource cannot be opened (I/O or storage error).
    fn open_coverage_index_resource(&self) -> AssetsResult<Self::IndexRes>;

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
