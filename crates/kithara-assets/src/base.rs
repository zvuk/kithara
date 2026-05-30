#![forbid(unsafe_code)]

use std::{fmt::Debug, hash::Hash, ops::Range, path::Path};

use kithara_storage::{ResourceStatus, StorageResult, WaitOutcome};

use crate::{
    acquisition::{AcquisitionResult, RawWriteHandle, ReadSide, WriteSide},
    error::AssetsResult,
    identity::RequestIdentity,
    key::ResourceKey,
    state::AssetResourceState,
};

/// Base-layer writer over a `kithara_storage::StorageResource`.
///
/// The decrypt-readiness typestate lives one layer up (in `ProcessedWriter` /
/// `ProcessedReader`). This newtype is the **storage** seam: it carries the
/// write capability of a freshly-acquired (uncommitted) resource and consumes
/// itself on [`commit`](WriteSide::commit) into a [`BaseReader`]. It is not
/// `Clone` — a single producer owns the write side.
#[derive(Debug)]
pub struct BaseWriter(kithara_storage::StorageResource);

/// Base-layer reader over a `kithara_storage::StorageResource`.
///
/// Cheap to clone (the underlying `StorageResource` is `Arc`-backed). Reads see
/// whatever the storage layer has committed; the processed/decrypt gate is
/// applied by `ProcessedReader` above.
#[derive(Clone, Debug)]
pub struct BaseReader(kithara_storage::StorageResource);

impl BaseWriter {
    /// Wrap a freshly-acquired (uncommitted) storage resource.
    pub(crate) fn new(inner: kithara_storage::StorageResource) -> Self {
        Self(inner)
    }
}

impl BaseReader {
    /// Wrap a committed (or in-flight shared) storage resource for reading.
    pub(crate) fn new(inner: kithara_storage::StorageResource) -> Self {
        Self(inner)
    }
}

impl WriteSide for BaseWriter {
    type Reader = BaseReader;

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.0.write_at(offset, data)
    }

    fn reader(&self) -> BaseReader {
        BaseReader(self.0.clone())
    }

    fn raw_write_handle(&self) -> RawWriteHandle {
        let storage = self.0.clone();
        RawWriteHandle::new(move |offset, data| storage.write_at(offset, data))
    }

    fn commit(self, final_len: Option<u64>) -> StorageResult<BaseReader> {
        self.0.commit(final_len)?;
        Ok(BaseReader(self.0))
    }

    fn fail(self, reason: String) {
        self.0.fail(reason);
    }
}

impl ReadSide for BaseReader {
    type Writer = BaseWriter;

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        self.0.read_at(offset, buf)
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        self.0.wait_range(range)
    }

    fn contains_range(&self, range: Range<u64>) -> bool {
        self.0.contains_range(range)
    }

    fn len(&self) -> Option<u64> {
        self.0.len()
    }

    fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>> {
        self.0.next_gap(from, limit)
    }

    fn path(&self) -> Option<&Path> {
        self.0.path()
    }

    fn status(&self) -> ResourceStatus {
        self.0.status()
    }

    fn reactivate(self) -> StorageResult<BaseWriter> {
        self.0.reactivate()?;
        Ok(BaseWriter(self.0))
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
/// Acquisition is **phase-typed**: `acquire_resource*` hands back an
/// [`AcquisitionResult`] — a `Pending` [`WriteSide`] writer that must
/// `commit` before any read, or a `Ready` [`ReadSide`] reader when the
/// resource is already committed. `open_resource*` always returns a `Ready`
/// reader. The decrypt-readiness gate is carried in these types, not behind a
/// runtime `is_readable()` probe; storage lifecycle `status()` stays a runtime
/// facade on the reader. See crate `README.md` for the asset / resource /
/// identity model.
pub trait Assets: Clone + Send + Sync + 'static {
    /// Context type for resource processing. Use `()` for no context.
    type Context: Clone + Send + Sync + Hash + Eq + Debug + 'static;
    /// Resource type for index persistence (pins, LRU). Cached and cloned by
    /// the cache decorator; no resource API is invoked on it directly.
    type IndexRes: Clone + Send + Sync + Debug + 'static;
    /// Writer (Pending) phase returned by `acquire_resource*`.
    type ActiveRes: WriteSide<Reader = Self::ReadyRes>;
    /// Reader (Ready) phase returned by `open_resource*` and by the `Ready`
    /// arm of `acquire_resource*`.
    type ReadyRes: ReadSide<Writer = Self::ActiveRes>;

    /// Acquire a resource for mutation (no identity, no context).
    ///
    /// # Errors
    /// Returns `AssetsError` if the resource cannot be opened.
    fn acquire_resource(
        &self,
        key: &ResourceKey,
        identity: Option<&RequestIdentity>,
    ) -> AssetsResult<AcquisitionResult<Self::ActiveRes, Self::ReadyRes>> {
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
    ) -> AssetsResult<AcquisitionResult<Self::ActiveRes, Self::ReadyRes>>;

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
    ) -> AssetsResult<Self::ReadyRes> {
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
    ) -> AssetsResult<Self::ReadyRes>;

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
