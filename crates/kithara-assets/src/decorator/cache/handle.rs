#![forbid(unsafe_code)]

use std::{fmt, ops::Range, path::Path};

use dashmap::DashSet;
use kithara_platform::sync::Arc;
use kithara_storage::{ResourceStatus, StorageResult, WaitOutcome};

use crate::{
    layout::ResourceKey,
    resource::{RawWriteHandle, ReadSide, WriteSide},
};

/// Writer (Pending) wrapper returned by [`super::CachedAssets`].
pub struct CachedWriter<W> {
    pinned: Arc<DashSet<ResourceKey>>,
    key: ResourceKey,
    inner: W,
}

/// Reader (Ready) wrapper returned by [`super::CachedAssets`]. Cheap to clone.
#[derive(Clone)]
pub struct CachedReader<R> {
    pinned: Arc<DashSet<ResourceKey>>,
    inner: R,
    key: ResourceKey,
}

impl<W: fmt::Debug> fmt::Debug for CachedWriter<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

impl<R: fmt::Debug> fmt::Debug for CachedReader<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

impl<W> CachedWriter<W> {
    pub(super) fn new(pinned: Arc<DashSet<ResourceKey>>, key: ResourceKey, inner: W) -> Self {
        Self { pinned, key, inner }
    }

    /// Pin this resource in the LRU cache so it is never evicted, until
    /// [`CachedReader::release`] is called for the same key.
    pub fn retain(self) -> Self {
        self.pinned.insert(self.key.clone());
        self
    }

    /// Pin this resource in the LRU cache (by-ref, for use inside wrappers).
    pub(crate) fn set_retained(&self) {
        self.pinned.insert(self.key.clone());
    }
}

impl<R> CachedReader<R> {
    pub(super) fn new(pinned: Arc<DashSet<ResourceKey>>, key: ResourceKey, inner: R) -> Self {
        Self { pinned, inner, key }
    }

    /// Unpin this resource, making it eligible for LRU eviction.
    pub fn release(self) -> Self {
        self.pinned.remove(&self.key);
        self
    }

    /// Pin this resource in the LRU cache. It will not be evicted
    /// until [`release`](Self::release) is called for the same key.
    pub fn retain(self) -> Self {
        self.pinned.insert(self.key.clone());
        self
    }

    /// Pin this resource in the LRU cache (by-ref, for use inside wrappers).
    pub(crate) fn set_retained(&self) {
        self.pinned.insert(self.key.clone());
    }
}

impl<W: WriteSide> WriteSide for CachedWriter<W> {
    type Reader = CachedReader<W::Reader>;

    fn commit(self, final_len: Option<u64>) -> StorageResult<CachedReader<W::Reader>> {
        Ok(CachedReader::new(
            Arc::clone(&self.pinned),
            self.key.clone(),
            self.inner.commit(final_len)?,
        ))
    }

    delegate::delegate! {
        to self.inner {
            fn fail(self, reason: String);
            fn raw_write_handle(&self) -> RawWriteHandle;
            fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;
        }
    }

    fn reader(&self) -> CachedReader<W::Reader> {
        CachedReader::new(
            Arc::clone(&self.pinned),
            self.key.clone(),
            self.inner.reader(),
        )
    }
}

impl<R: ReadSide> ReadSide for CachedReader<R> {
    type Writer = CachedWriter<R::Writer>;

    fn reactivate(self) -> StorageResult<CachedWriter<R::Writer>> {
        Ok(CachedWriter::new(
            Arc::clone(&self.pinned),
            self.key.clone(),
            self.inner.reactivate()?,
        ))
    }

    delegate::delegate! {
        to self.inner {
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            fn read_inflight_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome>;
            fn path(&self) -> Option<&Path>;
            fn len(&self) -> Option<u64>;
            fn status(&self) -> ResourceStatus;
            fn contains_range(&self, range: Range<u64>) -> bool;
            fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>>;
        }
    }
}
