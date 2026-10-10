use std::{num::NonZeroUsize, ops::Range};

use kithara_assets::{AssetReader, ReadSide};
use kithara_bufpool::HasPool;
use kithara_config::Config;
use kithara_download::PeerHandle;
use kithara_events::EventBus;
use kithara_platform::{CancelToken, sync::Arc, time::Duration};
use kithara_storage::{ResourceStatus, StorageError, WaitOutcome};
use kithara_stream::{
    Activity, ActivityWriter, ByteMap, MediaInfo, NotReadyCause, PendingReason, PlayheadRead,
    PlayheadWrite, ReadOutcome, SegmentDescriptor, SourceError as StreamSourceError, SourcePhase,
    SourceProbe, StreamError, StreamResult, WorkerWake,
};
use kithara_test_utils::kithara;
use tracing::trace;

use super::{
    inner::{FileAssetCtx, FileInner, FileSourceCtx, FileTerminalState},
    segments::FileSegmentIndex,
};
use crate::{
    TotalBytesSource,
    config::{FileConfig, FileConfigOwnerAccess},
    coord::FileCoord,
    error::SourceError as FileSourceError,
};

/// Inputs for constructing a local/cached file source.
#[derive(Config)]
#[config(construction)]
pub(crate) struct FileLocalConfig<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    #[config(skip = "transferred to the file source")]
    coord: Arc<FileCoord>,
    #[config(skip = "transferred to the file source")]
    reader: AssetReader<S>,
    #[config(skip = "transferred to the file source")]
    cancel: CancelToken,
    #[config(skip = "transferred to the file source")]
    bus: EventBus,
    #[config(skip = "consumed by the codec probe")]
    cached_media_info: Option<MediaInfo>,
}

/// Sync `Source` impl over a shared [`FileInner`].
///
/// All async work - HTTP fetch, body streaming, finalization - is owned
/// by the Downloader through [`FilePeer`](super::FilePeer); `FileSource`
/// just exposes the cached bytes synchronously to the audio worker.
#[derive(Clone, kithara_config::ConfigOwner)]
#[config_owner(FileConfig<S>, inner.config)]
pub struct FileSource<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Shared coordination - held next to `inner` so the hot read paths
    /// don't have to dereference the inner Arc.
    coord: Arc<FileCoord>,
    inner: Arc<FileInner<S>>,
    /// Peer registration handle returned by `Downloader::register`.
    /// Held here (mirroring `HlsSource::set_peer_handle`) so the peer
    /// stays registered for the source's lifetime - dropping the last
    /// handle would trigger `PeerInner::drop` and cancel in-flight
    /// fetches. `None` on the `local()` fast path (no Downloader at all).
    peer_handle: Option<PeerHandle>,
}

impl<S> FileSource<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn cancelled_error() -> StreamError {
        StreamError::Source(FileSourceError::Storage(StorageError::Cancelled).into())
    }

    fn ensure_not_cancelled(&self) -> StreamResult<()> {
        if self.inner.source.cancel.is_cancelled() {
            return Err(Self::cancelled_error());
        }
        Ok(())
    }

    fn ensure_realtime_not_terminal(&self) -> StreamResult<()> {
        self.inner.refresh_unmanaged_terminal();
        match self.inner.terminal_state() {
            FileTerminalState::Failed => {
                Err(StreamError::Source(StreamSourceError::SegmentUnavailable))
            }
            FileTerminalState::Cancelled => Err(Self::cancelled_error()),
            FileTerminalState::Active | FileTerminalState::Committed => Ok(()),
        }
    }

    fn ensure_storage_not_terminal(&self) -> StreamResult<()> {
        match self.inner.asset.reader.status() {
            ResourceStatus::Failed(reason) => Err(StreamError::Source(
                FileSourceError::Storage(StorageError::Failed(reason)).into(),
            )),
            ResourceStatus::Cancelled => Err(Self::cancelled_error()),
            ResourceStatus::Active | ResourceStatus::Committed { .. } => Ok(()),
        }
    }

    /// Create a source for a local/cached file (no downloads needed).
    ///
    /// `cancel` is a child of the file config master so a track drop pulse interrupts
    /// any in-flight reads.
    pub(crate) fn local(config: FileLocalConfig<S>, stream_config: Arc<FileConfig<S>>) -> Self {
        let FileLocalConfig {
            reader,
            coord,
            bus,
            cancel,
            cached_media_info,
        } = config;
        let inner = Arc::new(FileInner::new(
            stream_config,
            FileSourceCtx {
                cancel,
                bus,
                coord: Arc::clone(&coord),
            },
            FileAssetCtx { reader },
            true,
            None,
        ));
        if let Some(info) = cached_media_info {
            let _ = inner.content_type_info.set(info);
        }
        let total_bytes = inner.asset.reader.len();
        inner.publish_opened(
            total_bytes,
            true,
            total_bytes.map(|_| TotalBytesSource::CommittedLen),
        );
        Self {
            coord,
            inner,
            peer_handle: None,
        }
    }

    /// Pin the Downloader peer registration to this source's lifetime.
    /// Called once after `Downloader::register`; mirrors
    /// `HlsSource::set_peer_handle`. Without this the handle returned by
    /// `register` drops immediately and `PeerInner::Drop` cancels every
    /// in-flight fetch.
    pub(crate) fn set_peer_handle(&mut self, handle: PeerHandle) {
        self.peer_handle = Some(handle);
    }

    fn update_read_demand(&self, range: &Range<u64>, requested_end: u64) {
        if range.start > self.coord.read_pos() {
            self.coord.set_read_pos(range.start);
            if let Some(lease) = self.inner.resource_lease.as_ref() {
                lease.note_progress();
            }
        }
        if let Some(lease) = self.inner.resource_lease.as_ref() {
            lease.request_until(requested_end);
        }
    }

    /// Build a `FileSource` over a pre-constructed [`FileInner`]. The
    /// inner is created up in `Stream<File<S>>::open` and shared
    /// with [`FilePeer`](super::FilePeer); the Downloader owns the fetch
    /// loop, so this constructor does nothing async.
    pub(crate) const fn with_inner(inner: Arc<FileInner<S>>, coord: Arc<FileCoord>) -> Self {
        Self {
            coord,
            inner,
            peer_handle: None,
        }
    }

    fn zero_read_outcome(&self, offset: u64) -> StreamResult<ReadOutcome> {
        match self.inner.asset.reader.status() {
            ResourceStatus::Active => Ok(ReadOutcome::Pending(PendingReason::NotReady(
                NotReadyCause::SourcePending,
            ))),
            ResourceStatus::Committed {
                final_len: Some(len),
            } if offset < len => Ok(ReadOutcome::Pending(PendingReason::NotReady(
                NotReadyCause::SourcePending,
            ))),
            ResourceStatus::Committed { .. } => Ok(ReadOutcome::Eof),
            ResourceStatus::Failed(reason) => Err(StreamError::Source(
                FileSourceError::Storage(StorageError::Failed(reason)).into(),
            )),
            ResourceStatus::Cancelled => Err(StreamError::Source(
                FileSourceError::Storage(StorageError::Cancelled).into(),
            )),
        }
    }
}

/// Phase snapshots live on the shared inner: it owns the terminal state and
/// the cached-range view, and (per its own contract) is Mutex-free — so the
/// narrow [`SourceProbe`] handle the forbid-blocking audio core polls without
/// the stream's control-plane lock answers from here.
impl<S> FileInner<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub(crate) fn known_len(&self) -> Option<u64> {
        self.source
            .coord
            .total_bytes()
            .or_else(|| self.asset.reader.len())
    }

    pub(crate) fn phase(&self) -> SourcePhase {
        let pos = self.source.coord.position();
        self.phase_at(pos..pos.saturating_add(1))
    }

    pub(crate) fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        if self.source.cancel.is_cancelled() {
            return SourcePhase::Cancelled;
        }
        self.refresh_unmanaged_terminal();
        if self.terminal_state() == FileTerminalState::Cancelled {
            return SourcePhase::Cancelled;
        }
        let Some(readable) = self.readable_part(range) else {
            return match self.terminal_state() {
                FileTerminalState::Committed => SourcePhase::Eof,
                FileTerminalState::Cancelled => SourcePhase::Cancelled,
                FileTerminalState::Active | FileTerminalState::Failed => SourcePhase::Waiting,
            };
        };
        let contains = readable.is_empty() || self.asset.reader.contains_range(readable);
        if contains {
            return SourcePhase::Ready;
        }

        SourcePhase::Waiting
    }

    fn readable_part(&self, range: Range<u64>) -> Option<Range<u64>> {
        let Some(total) = self.known_len() else {
            return Some(range);
        };
        if total > 0 && range.start >= total {
            return None;
        }
        Some(range.start..range.end.min(total))
    }
}

/// Narrow byte-space handle over the shared inner. Wraps the `Arc` because
/// vending the byte map needs an owning handle to the inner's lazy segment
/// index; every answer comes from the inner's Mutex-free state.
struct FileProbe<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    inner: Arc<FileInner<S>>,
}

/// Segment-map handle over the shared inner, vended once the lazy segment
/// index exists — the one body behind `Source::byte_map` and
/// `SourceProbe::byte_map`.
fn file_byte_map<S>(inner: &Arc<FileInner<S>>) -> Option<Arc<dyn ByteMap>>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    inner.segment_index.get()?;
    Some(Arc::new(FileByteMap {
        inner: Arc::clone(inner),
    }))
}

impl<S> SourceProbe for FileProbe<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
        file_byte_map(&self.inner)
    }

    delegate::delegate! {
        to self.inner {
            fn phase(&self) -> SourcePhase;
            fn phase_at(&self, range: Range<u64>) -> SourcePhase;
            #[call(known_len)]
            fn len(&self) -> Option<u64>;
        }
        to self.inner.source.coord {
            fn position(&self) -> u64;
            fn set_position(&self, pos: u64);
        }
    }
}

impl<S> kithara_stream::Source for FileSource<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
        file_byte_map(&self.inner)
    }

    fn media_info(&self) -> Option<MediaInfo> {
        self.inner.content_type_info.get().cloned()
    }

    fn probe(&self) -> Arc<dyn SourceProbe> {
        Arc::new(FileProbe {
            inner: Arc::clone(&self.inner),
        })
    }

    #[kithara::measure]
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
        self.ensure_not_cancelled()?;
        let n = self
            .inner
            .asset
            .reader
            .read_at(offset, buf)
            .map_err(|e| StreamError::Source(FileSourceError::Storage(e).into()))?;

        let Some(count) = NonZeroUsize::new(n) else {
            return self.zero_read_outcome(offset);
        };

        trace!(offset, bytes = n, "FileSource read complete");

        Ok(ReadOutcome::Bytes(count))
    }

    fn set_worker_wake(&self, wake: Arc<dyn WorkerWake>) {
        self.inner.set_worker_wake(wake);
        self.inner.arm_reader_waker();
    }

    fn take_reader_event_sink(&mut self) -> Option<kithara_stream::BoxedEventSink> {
        let hooks = super::reader::FileReaderEventSink::new(
            self.inner.source.bus.clone(),
            Arc::clone(&self.coord),
            self.reader_event_capacity(),
        );
        Some(Box::new(hooks))
    }

    #[kithara::measure]
    fn wait_range(
        &mut self,
        range: Range<u64>,
        timeout: Option<Duration>,
    ) -> StreamResult<WaitOutcome> {
        self.ensure_not_cancelled()?;
        if timeout.is_some() {
            self.ensure_realtime_not_terminal()?;
        } else {
            self.ensure_storage_not_terminal()?;
        }
        match self.phase_at(range.clone()) {
            SourcePhase::Cancelled => return Err(Self::cancelled_error()),
            SourcePhase::Eof => return Ok(WaitOutcome::Eof),
            SourcePhase::Ready => return Ok(WaitOutcome::Ready),
            _ => {}
        }

        self.update_read_demand(&range, range.end);

        if timeout.is_some() {
            return Err(StreamError::Source(StreamSourceError::WaitBudgetExceeded));
        }

        self.inner
            .asset
            .reader
            .wait_range_with_cancel(range, &self.inner.source.cancel)
            .map_err(|e| StreamError::Source(FileSourceError::Storage(e).into()))
    }

    delegate::delegate! {
        to self.coord {
            #[call(activity_handle)]
            fn activity(&self) -> Activity;
            fn take_activity_writer(&mut self) -> Option<ActivityWriter>;
            #[call(advance_position)]
            fn advance(&self, n: u64);
            fn playhead_read(&self) -> Arc<dyn PlayheadRead>;
            fn playhead_write(&self) -> Arc<dyn PlayheadWrite>;
            fn position(&self) -> u64;
            fn set_position(&self, pos: u64);
        }
        to self.inner {
            #[call(known_len)]
            fn len(&self) -> Option<u64>;
            fn phase(&self) -> SourcePhase;
            fn phase_at(&self, range: Range<u64>) -> SourcePhase;
        }
    }
}

/// Byte-map handle for a fully cached fragmented-mp4 file.
///
/// Holds a clone of `FileInner` so the layout survives independently of
/// the original `FileSource` cursor; segment queries hit the lazy
/// `OnceLock<FileSegmentIndex>` populated on first call.
struct FileByteMap<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    inner: Arc<FileInner<S>>,
}

impl<S> FileByteMap<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn segment_index(&self) -> Option<&FileSegmentIndex> {
        self.inner.segment_index.get()
    }
}

impl<S> ByteMap for FileByteMap<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn len(&self) -> Option<u64> {
        self.inner.asset.reader.len()
    }

    fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
        self.segment_index()?.segment_after_byte(byte_offset)
    }

    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        self.segment_index()?.segment_at_time(t)
    }

    delegate::delegate! {
        to self {
            #[expr($.map_or(0..0, FileSegmentIndex::init_range))]
            #[call(segment_index)]
            fn init_segment_range(&self) -> Range<u64>;
            #[expr(Some($?.segment_count()))]
            #[call(segment_index)]
            fn segment_count(&self) -> Option<u32>;
        }
    }
}
