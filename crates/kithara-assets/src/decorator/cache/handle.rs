#![forbid(unsafe_code)]

use std::{ops::Range, path::Path};

use dashmap::DashSet;
use kithara_platform::{CancelToken, sync::Arc, tokio::sync::watch};
use kithara_storage::{ResourceStatus, StorageResult, WaitOutcome};

use crate::{
    layout::ResourceKey,
    resource::{RawWriteHandle, ReadSide, WriteSide},
};

pub(super) type EnforceCapacity = Arc<dyn Fn() + Send + Sync>;

/// The "writer live" marker of one generation, held by its writer. Dropping
/// it is the only change it ever carries: every [`WriterWatch`] of the
/// generation then reads the writer as gone, and only then are the waiting
/// readers woken.
pub(super) struct WriterLive {
    _sender: watch::Sender<()>,
}

/// A view of one generation's [`WriterLive`]: the cache entry reads whether
/// the writer is still live, and a reader that joined it waits for its end.
#[derive(Clone)]
pub(super) struct WriterWatch(watch::Receiver<()>);

impl std::fmt::Debug for WriterWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriterWatch")
            .field("live", &self.is_live())
            .finish()
    }
}

impl WriterLive {
    /// A live marker and the watch its generation is seen through.
    pub(super) fn new() -> (Self, WriterWatch) {
        let (sender, watch) = watch::channel(());
        (Self { _sender: sender }, WriterWatch(watch))
    }
}

impl WriterWatch {
    /// The watch of a generation no writer holds.
    pub(super) fn none() -> Self {
        WriterLive::new().1
    }

    /// Whether the generation's writer still holds it.
    pub(super) fn is_live(&self) -> bool {
        self.0.has_changed().is_ok()
    }
}

#[derive(Clone)]
struct CacheHandle<T> {
    pinned: Arc<DashSet<ResourceKey>>,
    enforce_capacity: Option<EnforceCapacity>,
    key: ResourceKey,
    inner: T,
}

/// Writer (Pending) wrapper returned by [`super::CachedAssets`].
#[derive(derive_more::Debug)]
#[debug(bound(W: std::fmt::Debug))]
#[debug("{:?}", handle.inner)]
pub struct CachedWriter<W> {
    handle: CacheHandle<W>,
    _live: WriterLive,
}

/// Reader (Ready) wrapper returned by [`super::CachedAssets`]. Cheap to clone.
#[derive_where::derive_where(Clone; R: Clone)]
#[derive(derive_more::Debug)]
#[debug(bound(R: std::fmt::Debug))]
#[debug("{:?}", handle.inner)]
pub struct CachedReader<R> {
    handle: CacheHandle<R>,
    writer: WriterWatch,
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

impl<W> CachedWriter<W> {
    pub(super) fn new(
        pinned: Arc<DashSet<ResourceKey>>,
        key: ResourceKey,
        inner: W,
        enforce_capacity: Option<EnforceCapacity>,
        live: WriterLive,
    ) -> Self {
        Self {
            handle: CacheHandle::new(pinned, key, inner, enforce_capacity),
            _live: live,
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
        writer: WriterWatch,
    ) -> Self {
        Self {
            handle: CacheHandle::new(pinned, key, inner, enforce_capacity),
            writer,
        }
    }

    /// Wait until the writer this reader joined lets go of the resource,
    /// committed or not; a reader that joined no writer returns at once.
    /// The writer is gone before this wakes, so an acquire after it gets the
    /// stored resource or a fresh writer, never the generation that ended.
    pub async fn writer_released(&self) {
        let mut watch = self.writer.0.clone();
        // The marker never sends: its drop is the one change this awaits.
        let _ = watch.changed().await;
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
        let Self {
            handle:
                CacheHandle {
                    enforce_capacity,
                    pinned,
                    key,
                    inner,
                },
            _live,
        } = self;
        let reader = CachedReader::new(
            pinned,
            key,
            inner.commit(final_len)?,
            enforce_capacity.clone(),
            WriterWatch::none(),
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
            WriterWatch::none(),
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
            WriterLive::new().0,
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
