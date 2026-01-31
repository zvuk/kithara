#![forbid(unsafe_code)]

use std::{collections::HashSet, fmt::Debug, ops::Range, path::Path, sync::Arc};

use kithara_storage::{ResourceExt, ResourceStatus, StorageResource, StorageResult, WaitOutcome};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::{
    base::Assets, error::AssetsResult, evict::ByteRecorder, index::PinsIndex, key::ResourceKey,
};

/// Decorator that adds "pin (lease) while handle lives" semantics on top of inner [`Assets`].
///
/// ## Normative behavior
/// - Every successful `open_resource()` pins `key.asset_root`.
/// - The pin table is stored as a `HashSet<String>` (unique roots) in memory.
/// - Every pin/unpin operation immediately persists the full table to disk using
///   `inner.open_pins_index_resource(...)` (best-effort).
/// - The pin index resource must be excluded from pinning to avoid recursion.
///
/// This type does **not** do any filesystem/path logic; it uses the inner `Assets` abstraction.
#[derive(Clone)]
pub struct LeaseAssets<A>
where
    A: Assets,
{
    inner: Arc<A>,
    pins: Arc<Mutex<HashSet<String>>>,
    cancel: CancellationToken,
    byte_recorder: Option<Arc<dyn ByteRecorder>>,
}

impl<A> LeaseAssets<A>
where
    A: Assets,
{
    pub fn new(inner: Arc<A>, cancel: CancellationToken) -> Self {
        Self {
            inner,
            pins: Arc::new(Mutex::new(HashSet::new())),
            cancel,
            byte_recorder: None,
        }
    }

    /// Create with byte recorder for eviction tracking.
    pub fn with_byte_recorder(
        inner: Arc<A>,
        cancel: CancellationToken,
        byte_recorder: Arc<dyn ByteRecorder>,
    ) -> Self {
        Self {
            inner,
            pins: Arc::new(Mutex::new(HashSet::new())),
            cancel,
            byte_recorder: Some(byte_recorder),
        }
    }

    pub(crate) fn inner(&self) -> &A {
        &self.inner
    }

    fn open_index(&self) -> AssetsResult<PinsIndex> {
        PinsIndex::open(self.inner())
    }

    fn persist_pins_best_effort(&self, pins: &HashSet<String>) -> AssetsResult<()> {
        let idx = self.open_index()?;
        idx.store(pins)
    }

    fn load_pins_best_effort(&self) -> AssetsResult<HashSet<String>> {
        let idx = self.open_index()?;
        idx.load()
    }

    fn ensure_loaded_best_effort(&self) -> AssetsResult<()> {
        let should_load = { self.pins.lock().is_empty() };

        if !should_load {
            return Ok(());
        }

        let loaded = self.load_pins_best_effort()?;
        let mut guard = self.pins.lock();
        for p in loaded {
            guard.insert(p);
        }
        Ok(())
    }

    fn pin(&self, asset_root: &str) -> AssetsResult<LeaseGuard<A>> {
        self.ensure_loaded_best_effort()?;

        let (snapshot, was_new) = {
            let mut pins = self.pins.lock();
            let was_new = pins.insert(asset_root.to_string());
            (pins.clone(), was_new)
        };

        if was_new {
            let _ = self.persist_pins_best_effort(&snapshot);
        }

        Ok(LeaseGuard {
            inner: Arc::new(LeaseGuardInner {
                owner: self.clone(),
                asset_root: asset_root.to_string(),
                cancel: self.cancel.clone(),
            }),
        })
    }
}

/// Resource wrapper that combines lease guard with byte recording on commit.
#[derive(Clone)]
pub struct LeaseResource<R, L> {
    inner: R,
    _lease: L,
    asset_root: String,
    byte_recorder: Option<Arc<dyn ByteRecorder>>,
}

impl<R, L> Debug for LeaseResource<R, L>
where
    R: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseResource")
            .field("inner", &self.inner)
            .field("asset_root", &self.asset_root)
            .finish()
    }
}

impl<R, L> ResourceExt for LeaseResource<R, L>
where
    R: ResourceExt + Send + Sync + Clone + Debug + 'static,
    L: Send + Sync + Clone + 'static,
{
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()> {
        self.inner.write_at(offset, data)
    }

    fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome> {
        self.inner.wait_range(range)
    }

    fn commit(&self, final_len: Option<u64>) -> StorageResult<()> {
        self.inner.commit(final_len)?;

        // Record bytes if recorder is set (best-effort)
        if let Some(ref recorder) = self.byte_recorder
            && let Ok(metadata) = std::fs::metadata(self.inner.path())
            && metadata.is_file()
        {
            recorder.record_bytes(&self.asset_root, metadata.len());
        }

        Ok(())
    }

    fn fail(&self, reason: String) {
        self.inner.fail(reason);
    }

    fn path(&self) -> &Path {
        self.inner.path()
    }

    fn len(&self) -> Option<u64> {
        self.inner.len()
    }

    fn status(&self) -> ResourceStatus {
        self.inner.status()
    }
}

impl<A> Assets for LeaseAssets<A>
where
    A: Assets,
{
    type Res = LeaseResource<A::Res, LeaseGuard<A>>;
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
        let inner = self.inner.open_resource_with_ctx(key, ctx)?;
        let lease = self.pin(self.inner.asset_root())?;

        Ok(LeaseResource {
            inner,
            _lease: lease,
            asset_root: self.inner.asset_root().to_string(),
            byte_recorder: self.byte_recorder.clone(),
        })
    }

    fn open_pins_index_resource(&self) -> AssetsResult<StorageResource> {
        // Pins index must be opened without pinning to avoid recursion
        self.inner.open_pins_index_resource()
    }

    fn open_lru_index_resource(&self) -> AssetsResult<StorageResource> {
        // LRU index must be opened without pinning
        self.inner.open_lru_index_resource()
    }

    fn delete_asset(&self) -> AssetsResult<()> {
        self.inner.delete_asset()
    }
}

/// RAII guard for a pin.
///
/// Dropping this guard unpins the corresponding `asset_root` and persists the new pin set
/// to disk (best-effort) via the decorator.
///
/// Uses `Arc` internally for reference counting - unpin happens only when the last clone is dropped.
#[derive(Clone)]
pub struct LeaseGuard<A>
where
    A: Assets,
{
    #[allow(dead_code)]
    inner: Arc<LeaseGuardInner<A>>,
}

struct LeaseGuardInner<A>
where
    A: Assets,
{
    owner: LeaseAssets<A>,
    asset_root: String,
    cancel: CancellationToken,
}

impl<A> Drop for LeaseGuardInner<A>
where
    A: Assets,
{
    fn drop(&mut self) {
        if self.cancel.is_cancelled() {
            return;
        }

        tracing::trace!(asset_root = %self.asset_root, "LeaseGuard::drop - removing pin");

        let snapshot = {
            let mut pins = self.owner.pins.lock();
            pins.remove(&self.asset_root);
            pins.clone()
        };

        // Persist to disk (best-effort, sync)
        let _ = self.owner.persist_pins_best_effort(&snapshot);
    }
}
