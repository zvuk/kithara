use std::{num::NonZeroUsize, ops::Range, sync::Arc};

use kithara_assets::{AssetResource, AssetStore, ResourceKey};
use kithara_events::EventBus;
use kithara_platform::time::Duration;
use kithara_storage::{ResourceExt, WaitOutcome};
use kithara_stream::{
    MediaInfo, ReadOutcome, SegmentDescriptor, SourcePhase, StreamError, Timeline, dl::PeerHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::trace;
use url::Url;

use super::{
    inner::{FileAssetCtx, FileInner, FilePhase, FileSourceCtx},
    segments::FileSegmentIndex,
};
use crate::{coord::FileCoord, error::SourceError as FileSourceError};

/// Sync `Source` impl over a shared [`FileInner`].
///
/// All async work — HTTP fetch, body streaming, finalization — is owned
/// by the Downloader through [`FilePeer`](super::FilePeer); `FileSource`
/// just exposes the cached bytes synchronously to the audio worker.
#[derive(Clone)]
pub struct FileSource {
    /// Shared coordination — held next to `inner` so the hot read paths
    /// don't have to dereference the inner Arc.
    coord: Arc<FileCoord>,
    inner: Arc<FileInner>,
    /// Peer registration handle returned by `Downloader::register`.
    /// Held here (mirroring `HlsSource::set_peer_handle`) so the peer
    /// stays registered for the source's lifetime — dropping the last
    /// handle would trigger `PeerInner::drop` and cancel in-flight
    /// fetches. `None` on the `local()` fast path (no Downloader at all).
    peer_handle: Option<PeerHandle>,
}

impl FileSource {
    /// Create a source for a local/cached file (no downloads needed).
    ///
    /// `cancel` is a child of the file config master so a track drop
    /// pulse interrupts any in-flight reads — see
    /// `kithara-play/README.md` "Cancel Hierarchy".
    pub(crate) fn local(
        res: AssetResource,
        coord: Arc<FileCoord>,
        bus: EventBus,
        backend: Arc<AssetStore>,
        key: ResourceKey,
        cancel: CancellationToken,
    ) -> Self {
        let inner = Arc::new(FileInner::new(
            FileSourceCtx {
                cancel,
                bus,
                coord: Arc::clone(&coord),
            },
            FileAssetCtx {
                backend,
                res,
                key,
                headers: None,
                url: Url::parse("file:///local")
                    .expect("BUG: hard-coded literal `file:///local` is a valid URL"),
            },
            FilePhase::Complete,
        ));
        Self {
            coord,
            inner,
            peer_handle: None,
        }
    }

    /// Build a `FileSource` over a pre-constructed [`FileInner`]. The
    /// inner is created up in `stream.rs::Stream<File>::open` and shared
    /// with [`FilePeer`](super::FilePeer); the Downloader owns the fetch
    /// loop, so this constructor does nothing async.
    pub(crate) fn from_inner(inner: Arc<FileInner>, coord: Arc<FileCoord>) -> Self {
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
}

impl kithara_stream::Source for FileSource {
    fn as_segment_layout(&self) -> Option<Arc<dyn kithara_stream::SegmentLayout>> {
        self.inner.segment_index.get()?;
        Some(Arc::new(FileSegmentLayout {
            inner: Arc::clone(&self.inner),
        }))
    }

    fn len(&self) -> Option<u64> {
        let from_coord = self.coord.total_bytes();
        let from_res = self.inner.asset.res.len();
        from_coord.or(from_res)
    }

    fn media_info(&self) -> Option<MediaInfo> {
        self.inner
            .content_type_codec
            .get()
            .copied()
            .map(|c| MediaInfo::new(Some(c), None))
    }

    fn phase(&self) -> SourcePhase {
        let pos = self.coord.position();
        self.phase_at(pos..pos.saturating_add(1))
    }

    fn position(&self) -> u64 {
        self.coord.position()
    }

    fn advance(&self, n: u64) {
        self.coord.advance_position(n);
    }

    fn set_position(&self, pos: u64) {
        self.coord.set_position(pos);
    }

    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        let contains = self.inner.asset.res.contains_range(range.clone());
        if contains {
            return SourcePhase::Ready;
        }

        let from_coord = self.coord.total_bytes();
        let from_res = self.inner.asset.res.len();
        let past_eof = from_coord
            .or(from_res)
            .is_some_and(|total| total > 0 && range.start >= total);

        if self.coord.timeline().is_flushing() {
            return SourcePhase::Seeking;
        }
        if past_eof {
            return SourcePhase::Eof;
        }
        SourcePhase::Waiting
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn read_at(
        &mut self,
        offset: u64,
        buf: &mut [u8],
    ) -> kithara_stream::StreamResult<ReadOutcome> {
        let n = self
            .inner
            .asset
            .res
            .read_at(offset, buf)
            .map_err(|e| StreamError::Source(FileSourceError::Storage(e).into()))?;

        let Some(count) = NonZeroUsize::new(n) else {
            return Ok(ReadOutcome::Eof);
        };

        trace!(offset, bytes = n, "FileSource read complete");

        Ok(ReadOutcome::Bytes(count))
    }

    fn take_reader_hooks(&mut self) -> Option<kithara_stream::SharedHooks> {
        let hooks = super::reader::FileReaderHooks::new(
            self.inner.source.bus.clone(),
            Arc::clone(&self.coord),
            self.coord.timeline().seek_epoch_handle(),
        );
        Some(Arc::new(std::sync::Mutex::new(hooks)))
    }

    fn timeline(&self) -> Timeline {
        self.coord.timeline()
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn wait_range(
        &mut self,
        range: Range<u64>,
        timeout: Option<Duration>,
    ) -> kithara_stream::StreamResult<WaitOutcome> {
        let _ = timeout;

        match self.phase_at(range.clone()) {
            SourcePhase::Seeking => return Ok(WaitOutcome::Interrupted),
            SourcePhase::Eof => return Ok(WaitOutcome::Eof),
            SourcePhase::Ready => return Ok(WaitOutcome::Ready),
            _ => {}
        }

        if range.start > self.coord.read_pos() {
            self.coord.set_read_pos(range.start);
        }

        self.inner
            .asset
            .res
            .wait_range(range)
            .map_err(|e| StreamError::Source(FileSourceError::Storage(e).into()))
    }
}

/// Segment-layout handle for a fully cached fragmented-mp4 file.
///
/// Holds a clone of `FileInner` so the layout survives independently of
/// the original `FileSource` cursor; segment queries hit the lazy
/// `OnceLock<FileSegmentIndex>` populated on first call.
struct FileSegmentLayout {
    inner: Arc<FileInner>,
}

impl FileSegmentLayout {
    fn segment_index(&self) -> Option<&FileSegmentIndex> {
        self.inner.segment_index.get()
    }
}

impl kithara_stream::SegmentLayout for FileSegmentLayout {
    fn init_segment_range(&self) -> Range<u64> {
        self.segment_index()
            .map(FileSegmentIndex::init_range)
            .unwrap_or(0..0)
    }

    fn len(&self) -> Option<u64> {
        self.inner.asset.res.len()
    }

    fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
        self.segment_index()?.segment_after_byte(byte_offset)
    }

    fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
        self.segment_index()?.segment_at_time(t)
    }

    fn segment_count(&self) -> Option<u32> {
        Some(self.segment_index()?.segment_count())
    }
}
