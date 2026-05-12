//! Public `FileSource` and its `Source` trait implementation.
//!
//! `FileSource` is the synchronous Read+Seek-style interface used by the
//! decoder thread. The async download tasks in [`super::download`] mutate the
//! shared `FileInner` lock-free (atomics + `OnceLock`); this file only reads
//! from it.

use std::{num::NonZeroUsize, ops::Range, sync::Arc};

use kithara_assets::{AssetResource, AssetStore, ResourceKey};
use kithara_events::EventBus;
use kithara_net::Headers;
use kithara_platform::{time::Duration, tokio::task};
use kithara_storage::{ResourceExt, WaitOutcome};
use kithara_stream::{
    MediaInfo, ReadOutcome, SegmentDescriptor, SourcePhase, StreamError, Timeline, dl::PeerHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};
use url::Url;

use super::{
    download::{run_full_download, run_range_watcher},
    inner::{FileAssetCtx, FileInner, FilePhase, FileSourceCtx, FileStreamState},
    segments::FileSegmentIndex,
};
use crate::{coord::FileCoord, error::SourceError as FileSourceError};

/// File source: sync Read+Seek access backed by async downloads via
/// [`PeerHandle`].
///
/// Created via [`File::create()`](crate::File). Downloads are driven
/// by spawned async tasks that call [`PeerHandle::execute`].
#[derive(Clone)]
pub struct FileSource {
    /// Shared coordination — held next to `inner` so the hot read paths
    /// don't have to dereference the inner Arc.
    coord: Arc<FileCoord>,
    inner: Arc<FileInner>,
}

impl FileSource {
    /// Try to populate the lazy fragmented-mp4 segment index from the
    /// fully cached file bytes. No-op when the file is still
    /// downloading or the index is already set.
    fn ensure_segment_index(&self) -> Option<&FileSegmentIndex> {
        if let Some(idx) = self.inner.segment_index.get() {
            return Some(idx);
        }
        let total = self.inner.asset.res.len()?;
        if total == 0 {
            return None;
        }
        if !self.inner.asset.res.contains_range(0..total) {
            return None;
        }
        let total_usize = usize::try_from(total).ok()?;
        let mut buf: Box<[u8]> = std::iter::repeat_n(0u8, total_usize).collect();
        self.inner.asset.res.read_at(0, &mut buf).ok()?;
        let index = FileSegmentIndex::try_build(&buf)?;
        let _ = self.inner.segment_index.set(index);
        self.inner.segment_index.get()
    }

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
        Self { coord, inner }
    }

    /// Create a source for a remote file and spawn download tasks.
    ///
    /// Spawns two async tasks:
    /// 1. Full-file download (streaming GET)
    /// 2. Range-request watcher (handles on-demand seeks)
    ///
    /// Both tasks use the provided [`PeerHandle`] for HTTP requests.
    pub(crate) fn remote(
        state: &FileStreamState,
        coord: Arc<FileCoord>,
        cancel: CancellationToken,
        url: Url,
        headers: Option<Headers>,
        look_ahead_bytes: Option<u64>,
        peer: PeerHandle,
    ) -> Self {
        let inner = Arc::new(FileInner::new(
            FileSourceCtx {
                coord: Arc::clone(&coord),
                cancel: cancel.clone(),
                bus: state.bus.clone(),
            },
            FileAssetCtx {
                headers,
                url,
                backend: Arc::clone(&state.backend),
                res: state.res.clone(),
                key: state.key.clone(),
            },
            FilePhase::Init,
        ));

        let dl_inner = Arc::clone(&inner);
        let dl_peer = peer.clone();
        task::spawn(async move {
            run_full_download(dl_inner, dl_peer, look_ahead_bytes).await;
        });

        let rng_inner = Arc::clone(&inner);
        let rng_coord = Arc::clone(&coord);
        task::spawn(async move {
            run_range_watcher(rng_inner, peer, rng_coord, cancel).await;
        });

        Self { coord, inner }
    }
}

impl kithara_stream::Source for FileSource {
    fn as_segment_layout(&self) -> Option<Arc<dyn kithara_stream::SegmentLayout>> {
        self.ensure_segment_index()?;
        Some(Arc::new(FileSegmentLayout {
            inner: Arc::clone(&self.inner),
        }))
    }

    fn demand_range(&self, range: Range<u64>) {
        if self.inner.asset.res.contains_range(range.clone()) {
            return;
        }
        self.coord.request_range(range);
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

        if !self.inner.asset.res.contains_range(range.clone()) {
            debug!(
                range_start = range.start,
                range_end = range.end,
                "file_source::wait_range requesting on-demand download"
            );
            self.coord.request_range(range.clone());
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
    fn init_segment_range(&self) -> Option<Range<u64>> {
        self.segment_index().map(FileSegmentIndex::init_range)
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
