#![forbid(unsafe_code)]

use std::{fmt, ops::Range, path::Path};

use dashmap::DashSet;
use kithara_platform::{CancelToken, sync::Arc};
use kithara_storage::{ResourceStatus, StorageResult, WaitOutcome};

use crate::{
    layout::ResourceKey,
    resource::{RawWriteHandle, ReadSide, WriteSide},
};

pub(super) type EnforceCapacity = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
struct CacheHandle<T> {
    pinned: Arc<DashSet<ResourceKey>>,
    enforce_capacity: Option<EnforceCapacity>,
    key: ResourceKey,
    inner: T,
}

/// Writer (Pending) wrapper returned by [`super::CachedAssets`].
pub struct CachedWriter<W> {
    handle: CacheHandle<W>,
}

/// Reader (Ready) wrapper returned by [`super::CachedAssets`]. Cheap to clone.
pub struct CachedReader<R> {
    handle: CacheHandle<R>,
}

impl<T> CacheHandle<T> {
    fn new(
        pinned: Arc<DashSet<ResourceKey>>,
        key: ResourceKey,
        inner: T,
        enforce_capacity: Option<EnforceCapacity>,
    ) -> Self {
        Self {
            pinned,
            enforce_capacity,
            key,
            inner,
        }
    }

    fn retain(&self) {
        self.pinned.insert(self.key.clone());
    }
}

impl<R: Clone> Clone for CachedReader<R> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
        }
    }
}

impl<W: fmt::Debug> fmt::Debug for CachedWriter<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.handle.inner.fmt(f)
    }
}

impl<R: fmt::Debug> fmt::Debug for CachedReader<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.handle.inner.fmt(f)
    }
}

impl<W> CachedWriter<W> {
    pub(super) fn new(
        pinned: Arc<DashSet<ResourceKey>>,
        key: ResourceKey,
        inner: W,
        enforce_capacity: Option<EnforceCapacity>,
    ) -> Self {
        Self {
            handle: CacheHandle::new(pinned, key, inner, enforce_capacity),
        }
    }

    /// Pin this resource in the LRU cache so it is never evicted, until
    /// [`CachedReader::release`] is called for the same key.
    pub fn retain(self) -> Self {
        self.handle.retain();
        self
    }

    /// Pin this resource in the LRU cache (by-ref, for use inside wrappers).
    pub(crate) fn set_retained(&self) {
        self.handle.retain();
    }
}

impl<R> CachedReader<R> {
    pub(super) fn new(
        pinned: Arc<DashSet<ResourceKey>>,
        key: ResourceKey,
        inner: R,
        enforce_capacity: Option<EnforceCapacity>,
    ) -> Self {
        Self {
            handle: CacheHandle::new(pinned, key, inner, enforce_capacity),
        }
    }

    /// Unpin this resource, making it eligible for LRU eviction.
    pub fn release(self) -> Self {
        self.handle.pinned.remove(&self.handle.key);
        if let Some(enforce) = &self.handle.enforce_capacity {
            enforce();
        }
        self
    }

    /// Pin this resource in the LRU cache. It will not be evicted
    /// until [`release`](Self::release) is called for the same key.
    pub fn retain(self) -> Self {
        self.handle.retain();
        self
    }

    /// Pin this resource in the LRU cache (by-ref, for use inside wrappers).
    pub(crate) fn set_retained(&self) {
        self.handle.retain();
    }
}

impl<W: WriteSide> WriteSide for CachedWriter<W> {
    type Reader = CachedReader<W::Reader>;

    fn commit(self, final_len: Option<u64>) -> StorageResult<CachedReader<W::Reader>> {
        let CacheHandle {
            enforce_capacity,
            pinned,
            key,
            inner,
        } = self.handle;
        let reader = CachedReader::new(
            pinned,
            key,
            inner.commit(final_len)?,
            enforce_capacity.clone(),
        );
        if let Some(enforce) = enforce_capacity {
            enforce();
        }
        Ok(reader)
    }

    fn reader(&self) -> CachedReader<W::Reader> {
        CachedReader::new(
            Arc::clone(&self.handle.pinned),
            self.handle.key.clone(),
            self.handle.inner.reader(),
            self.handle.enforce_capacity.clone(),
        )
    }

    delegate::delegate! {
        to self.handle.inner {
            fn abandon(self);
            fn fail(self, reason: String);
            fn raw_write_handle(&self) -> RawWriteHandle;
            fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;
        }
    }
}

impl<R: ReadSide> ReadSide for CachedReader<R> {
    type Writer = CachedWriter<R::Writer>;

    fn reactivate(self) -> StorageResult<CachedWriter<R::Writer>> {
        Ok(CachedWriter::new(
            Arc::clone(&self.handle.pinned),
            self.handle.key.clone(),
            self.handle.inner.reactivate()?,
            self.handle.enforce_capacity.clone(),
        ))
    }

    delegate::delegate! {
        to self.handle.inner {
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            fn read_inflight_at(&self, offset: u64, buf: &mut [u8]) -> StorageResult<usize>;
            fn wait_range(&self, range: Range<u64>) -> StorageResult<WaitOutcome>;
            fn wait_range_with_cancel(
                &self,
                range: Range<u64>,
                cancel: &CancelToken,
            ) -> StorageResult<WaitOutcome>;
            fn path(&self) -> Option<&Path>;
            fn len(&self) -> Option<u64>;
            fn status(&self) -> ResourceStatus;
            fn contains_range(&self, range: Range<u64>) -> bool;
            fn next_gap(&self, from: u64, limit: u64) -> Option<Range<u64>>;
        }
    }
}
