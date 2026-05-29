#![forbid(unsafe_code)]

use std::{fmt::Debug, hash::Hash, ops::Range, path::Path};

use kithara_storage::{ResourceStatus, StorageResult, WaitOutcome};

use crate::{error::AssetsResult, key::ResourceKey, state::AssetResourceState};

/// Unified resource-access contract for the asset decorator stack.
///
/// This is the assets-layer facade over a storage resource whose lifecycle
/// phase varies at runtime (active while downloading, committed once written,
/// reactivated on re-fetch) and is observed by concurrent readers. The genuine
/// compile-time write/read typestate lives one layer down in
/// `kithara_storage::Resource<S>`; at the cache layer the resource is a shared,
/// concurrently-observed runtime state, so a `&self` facade with a runtime
/// [`status`](ResourceHandle::status) is the correct representation (mirrors the
/// `CurrentFsm` erasure boundary in the audio worker). Implemented by
/// `StorageResource` and the cache/lease/processing decorator wrappers.
pub trait ResourceHandle: Send + Sync + 'static {
    /// Commit the resource as fully written.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the backend
    /// cannot finalize.
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()>;

    /// Whether the given byte range is fully covered by available data.
    fn contains_range(&self, range: Range<u64>) -> bool;

    /// Mark the resource as failed.
    fn fail(&self, reason: String);

    /// Returns `true` if the resource has been committed with zero length.
    fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// Committed length, if known.
    fn len(&self) -> Option<u64>;

    /// First gap in available data starting at `from`, up to `limit`.
    fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>>;

    /// Backing file path, if any.
    fn path(&self) -> Option<&Path>;

    /// Reactivate a committed resource for continued writing.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled or the backend cannot reopen.
    fn reactivate(&self) -> StorageResult<()>;

    /// Read data at the given offset into `buf`; returns bytes read.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;

    /// Read the entire resource into a caller buffer; returns bytes read.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the read fails.
    fn read_into(&self, buf: &mut Vec<u8>) -> StorageResult<usize> {
        let Some(len) = self.len() else {
            let mut probe = [0u8; 1];
            let _ = self.read_at(0, &mut probe)?;
            return Ok(0);
        };
        if len == 0 {
            buf.clear();
            return Ok(0);
        }
        let len_usize = usize::try_from(len).unwrap_or(usize::MAX);
        buf.resize(len_usize, 0);
        let n = self.read_at(0, buf)?;
        buf.truncate(n);
        Ok(n)
    }

    /// Current runtime lifecycle status.
    fn status(&self) -> ResourceStatus;

    /// Wait until the given byte range is available.
    ///
    /// # Errors
    /// Returns error if the range is invalid, the resource is cancelled, or the
    /// resource has failed.
    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome>;

    /// Write entire contents and commit atomically.
    ///
    /// # Errors
    /// Returns error if the write or commit fails.
    fn write_all(&self, data: &[u8]) -> StorageResult<()> {
        self.write_at(0, data)?;
        self.commit(Some(data.len() as u64))
    }

    /// Write data at the given offset.
    ///
    /// # Errors
    /// Returns error if the resource is cancelled, failed, or the write fails.
    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;
}

impl ResourceHandle for kithara_storage::StorageResource {
    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        Self::commit(self, final_len)
    }
    fn contains_range(&self, range: Range<u64>) -> bool {
        Self::contains_range(self, range)
    }
    fn fail(&self, reason: String) {
        Self::fail(self, reason);
    }
    fn len(&self) -> Option<u64> {
        Self::len(self)
    }
    fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>> {
        Self::next_gap(self, from, limit)
    }
    fn path(&self) -> Option<&Path> {
        Self::path(self)
    }
    fn reactivate(&self) -> StorageResult<()> {
        Self::reactivate(self)
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        Self::read_at(self, offset, buf)
    }
    fn read_into(&self, buf: &mut Vec<u8>) -> StorageResult<usize> {
        Self::read_into(self, buf)
    }
    fn status(&self) -> ResourceStatus {
        Self::status(self)
    }
    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        Self::wait_range(self, range)
    }
    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        Self::write_at(self, offset, data)
    }
}

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
    /// Context type for resource processing. Use `()` for no context.
    type Context: Clone + Send + Sync + Hash + Eq + Debug + 'static;
    /// Resource type for index persistence (pins, LRU). Cached and cloned by
    /// the cache decorator; no resource API is invoked on it directly.
    type IndexRes: Clone + Send + Sync + Debug + 'static;
    /// Type returned by `open_resource`. Must be Clone for caching.
    type Res: ResourceHandle + Clone + Send + Sync + Debug + 'static;

    /// Convenience method - acquire a resource without context.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the resource key is invalid or the underlying
    /// storage cannot be opened or created.
    fn acquire_resource(&self, key: &ResourceKey) -> AssetsResult<Self::Res> {
        self.acquire_resource_with_ctx(key, None)
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

    /// Return the asset root identifier for this store.
    fn asset_root(&self) -> &str;

    /// Decorator capabilities supported by this backend.
    ///
    /// Decorators check the relevant [`Capabilities`] bit before activating.
    /// The default returns all capabilities enabled.
    #[must_use]
    fn capabilities(&self) -> Capabilities {
        Capabilities::all()
    }

    /// Delete the entire asset (all resources under this store's `asset_root`).
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the asset directory cannot be removed (I/O error
    /// or cancellation).
    fn delete_asset(&self) -> AssetsResult<()>;

    /// Open the resource used for persisting the LRU index.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the index resource cannot be opened (I/O or storage error).
    fn open_lru_index_resource(&self) -> AssetsResult<Self::IndexRes>;

    /// Open the resource used for persisting the pins index.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the index resource cannot be opened (I/O or storage error).
    fn open_pins_index_resource(&self) -> AssetsResult<Self::IndexRes>;

    /// Convenience method - open a resource without context.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the resource key is invalid or the underlying
    /// storage cannot be opened.
    fn open_resource(&self, key: &ResourceKey) -> AssetsResult<Self::Res> {
        self.open_resource_with_ctx(key, None)
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

    /// Inspect the current resource state without creating or mutating it.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the resource key is invalid or the backend
    /// cannot inspect the resource.
    fn resource_state(&self, key: &ResourceKey) -> AssetsResult<AssetResourceState>;

    /// Return the root directory for disk-backed implementations.
    fn root_dir(&self) -> &Path;
}
