use std::{
    fmt::{self, Debug},
    fs,
    ops::Range,
    path::Path,
};

use kithara_platform::sync::Arc;
use kithara_storage::{ResourceStatus, StorageResult, WaitOutcome};

use super::{
    cleanup::WriterCleanup,
    layer::LeaseEvents,
    live::{LiveResource, RemoveFn},
};
use crate::{
    decorator::{ByteRecorder, CachedReader, CachedWriter},
    layout::ResourceKey,
    resource::{AssetResourceState, RawWriteHandle, ReadSide, WriteSide},
};

/// Writer (Pending) phase of a leased resource. Pins the asset while alive,
/// records bytes + updates the live mirror on `commit`, and removes the partial
/// resource if dropped without committing.
pub struct LeaseWriter<W: WriteSide, L> {
    _lease: L,
    byte_recorder: Option<Arc<dyn ByteRecorder>>,
    inner: W,
    cleanup: WriterCleanup,
}

impl<W: WriteSide, L> LeaseWriter<W, L> {
    pub(super) fn new(
        inner: W,
        lease: L,
        byte_recorder: Option<Arc<dyn ByteRecorder>>,
        events: LeaseEvents,
        live: Option<Arc<LiveResource>>,
        remove: Option<RemoveFn>,
        resource_key: Option<ResourceKey>,
    ) -> Self {
        Self {
            _lease: lease,
            byte_recorder,
            inner,
            cleanup: WriterCleanup::new(events, live, remove, resource_key),
        }
    }
}

/// Reader (Ready) phase of a leased resource. Pins the asset while any clone is
/// alive; read-only. Carries the write-side machinery (`byte_recorder`,
/// `remove`) so [`reactivate`](ReadSide::reactivate) can rebuild a full writer.
pub struct LeaseReader<R: ReadSide, L> {
    lease: L,
    byte_recorder: Option<Arc<dyn ByteRecorder>>,
    events: LeaseEvents,
    live: Option<Arc<LiveResource>>,
    remove: Option<RemoveFn>,
    resource_key: Option<ResourceKey>,
    inner: R,
}

impl<R: ReadSide, L> LeaseReader<R, L> {
    pub(super) fn new(
        inner: R,
        lease: L,
        byte_recorder: Option<Arc<dyn ByteRecorder>>,
        events: LeaseEvents,
        live: Option<Arc<LiveResource>>,
        remove: Option<RemoveFn>,
        resource_key: Option<ResourceKey>,
    ) -> Self {
        Self {
            lease,
            byte_recorder,
            events,
            live,
            remove,
            resource_key,
            inner,
        }
    }
}

impl<R, L> Clone for LeaseReader<R, L>
where
    R: ReadSide,
    L: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            lease: self.lease.clone(),
            byte_recorder: self.byte_recorder.clone(),
            events: self.events.clone(),
            remove: self.remove.clone(),
            live: self.live.clone(),
            resource_key: self.resource_key.clone(),
        }
    }
}

impl<W: WriteSide, L> Debug for LeaseWriter<W, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeaseWriter")
            .field("inner", &self.inner)
            .field("key", &self.cleanup.resource_key)
            .finish_non_exhaustive()
    }
}

impl<R: ReadSide, L> Debug for LeaseReader<R, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LeaseReader")
            .field("inner", &self.inner)
            .field("key", &self.resource_key)
            .finish_non_exhaustive()
    }
}

impl<R, L> LeaseReader<CachedReader<R>, L>
where
    R: ReadSide,
{
    /// Pin the underlying cached resource so it is never evicted.
    pub fn retain(self) -> Self {
        self.inner.set_retained();
        self
    }
}

impl<W, L> LeaseWriter<CachedWriter<W>, L>
where
    W: WriteSide,
{
    /// Pin the underlying cached resource so it is never evicted.
    pub fn retain(self) -> Self {
        self.inner.set_retained();
        self
    }
}

impl<W, L> WriteSide for LeaseWriter<W, L>
where
    W: WriteSide,
    L: Send + Sync + Clone + 'static,
{
    type Reader = LeaseReader<W::Reader, L>;

    fn commit(mut self, final_len: Option<u64>) -> StorageResult<LeaseReader<W::Reader, L>> {
        let reader_inner = self.inner.commit(final_len)?;
        if let Some(live) = &self.cleanup.live {
            live.set(AssetResourceState::from(reader_inner.status()));
        }
        if let Some(ref recorder) = self.byte_recorder
            && let Some(asset_root) = self
                .cleanup
                .resource_key
                .as_ref()
                .and_then(ResourceKey::asset_root)
            && let Some(path) = reader_inner.path()
            && let Ok(metadata) = fs::metadata(path)
            && metadata.is_file()
        {
            recorder.record_bytes(asset_root, metadata.len());
        }
        self.cleanup
            .events
            .publish_committed(self.cleanup.resource_key.as_ref(), final_len);
        self.cleanup.disarm();
        Ok(LeaseReader::new(
            reader_inner,
            self._lease.clone(),
            self.byte_recorder.clone(),
            self.cleanup.events.clone(),
            self.cleanup.live.clone(),
            self.cleanup.remove.clone(),
            self.cleanup.resource_key.clone(),
        ))
    }

    fn fail(mut self, reason: String) {
        self.inner.fail(reason.clone());
        if let Some(live) = &self.cleanup.live {
            live.set(AssetResourceState::Failed(reason.clone()));
        }
        self.cleanup
            .events
            .publish_failed(self.cleanup.resource_key.as_ref(), &reason);
        // Explicit failure remains observable through `resource_state`; only
        // silent abandonment removes a partial resource.
        self.cleanup.disarm();
    }

    delegate::delegate! {
        to self.inner {
            fn raw_write_handle(&self) -> RawWriteHandle;
            fn write_at(&self, offset: u64, data: &[u8]) -> StorageResult<()>;
        }
    }

    fn reader(&self) -> LeaseReader<W::Reader, L> {
        LeaseReader::new(
            self.inner.reader(),
            self._lease.clone(),
            self.byte_recorder.clone(),
            self.cleanup.events.clone(),
            self.cleanup.live.clone(),
            self.cleanup.remove.clone(),
            self.cleanup.resource_key.clone(),
        )
    }
}

impl<R, L> ReadSide for LeaseReader<R, L>
where
    R: ReadSide,
    L: Send + Sync + Clone + 'static,
{
    type Writer = LeaseWriter<R::Writer, L>;

    fn reactivate(self) -> StorageResult<LeaseWriter<R::Writer, L>> {
        let writer_inner = self.inner.reactivate()?;
        if let Some(live) = &self.live {
            live.set(AssetResourceState::Active);
        }
        Ok(LeaseWriter::new(
            writer_inner,
            self.lease,
            self.byte_recorder,
            self.events,
            self.live,
            self.remove,
            self.resource_key,
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
