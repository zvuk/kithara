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
use kithara_stream::{MediaInfo, ReadOutcome, SourcePhase, StreamError, Timeline, dl::PeerHandle};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};
use url::Url;

use super::{
    download::{run_full_download, run_range_watcher},
    inner::{FileInner, FileInnerParams, FilePhase, FileStreamState},
};
use crate::{coord::FileCoord, error::SourceError};

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
    /// Create a source for a local/cached file (no downloads needed).
    pub(crate) fn local(
        res: AssetResource,
        coord: Arc<FileCoord>,
        bus: EventBus,
        backend: Arc<AssetStore>,
        key: ResourceKey,
    ) -> Self {
        let inner = Arc::new(FileInner::new(
            FileInnerParams {
                backend,
                bus,
                key,
                res,
                cancel: CancellationToken::new(),
                coord: Arc::clone(&coord),
                headers: None,
                url: Url::parse("file:///local").expect("valid url"),
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
            FileInnerParams {
                headers,
                url,
                backend: Arc::clone(&state.backend),
                bus: state.bus.clone(),
                cancel: cancel.clone(),
                coord: Arc::clone(&coord),
                key: state.key.clone(),
                res: state.res.clone(),
            },
            FilePhase::Init,
        ));

        // Spawn full-file download task.
        let dl_inner = Arc::clone(&inner);
        let dl_peer = peer.clone();
        task::spawn(async move {
            run_full_download(dl_inner, dl_peer, look_ahead_bytes).await;
        });

        // Spawn range-request watcher task.
        let rng_inner = Arc::clone(&inner);
        let rng_coord = Arc::clone(&coord);
        task::spawn(async move {
            run_range_watcher(rng_inner, peer, rng_coord, cancel).await;
        });

        Self { coord, inner }
    }
}

impl kithara_stream::Source for FileSource {
    type Error = SourceError;

    fn demand_range(&self, range: Range<u64>) {
        if self.inner.res.contains_range(range.clone()) {
            return;
        }
        self.coord.request_range(range);
    }

    fn len(&self) -> Option<u64> {
        self.coord.total_bytes().or_else(|| self.inner.res.len())
    }

    fn media_info(&self) -> Option<MediaInfo> {
        self.inner
            .content_type_codec
            .get()
            .copied()
            .map(|c| MediaInfo::new(Some(c), None))
    }

    fn phase(&self) -> SourcePhase {
        let pos = self.coord.timeline().byte_position();
        self.phase_at(pos..pos.saturating_add(1))
    }

    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        let contains = self.inner.res.contains_range(range.clone());
        if contains {
            return SourcePhase::Ready;
        }

        let res_len = self.inner.res.len();
        let past_eof = self
            .coord
            .total_bytes()
            .or(res_len)
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
    ) -> kithara_stream::StreamResult<ReadOutcome, SourceError> {
        let n = self
            .inner
            .res
            .read_at(offset, buf)
            .map_err(|e| StreamError::Source(SourceError::Storage(e)))?;

        let Some(count) = NonZeroUsize::new(n) else {
            return Ok(ReadOutcome::Eof);
        };

        // Reader-side `FileEvent::ReadProgress` is fired by
        // `FileReaderHooks` from the decoder layer — see
        // `kithara-file/src/session/reader.rs`.
        trace!(offset, bytes = n, "FileSource read complete");

        Ok(ReadOutcome::Bytes(count))
    }

    fn take_reader_hooks(&mut self) -> Option<kithara_stream::SharedHooks> {
        let hooks = super::reader::FileReaderHooks::new(
            self.inner.bus.clone(),
            Arc::clone(&self.coord),
            self.coord.timeline().byte_position_handle(),
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
    ) -> kithara_stream::StreamResult<WaitOutcome, SourceError> {
        // The file backend's `Resource::wait_range` blocks on its own
        // condvar / cancel signals; the source-level `timeout` is a hint
        // that does not gate the inner wait. Both `Some` and `None`
        // collapse to "wait until ready or cancel" here.
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

        // Issue on-demand Range request when data is missing.
        if !self.inner.res.contains_range(range.clone()) {
            debug!(
                range_start = range.start,
                range_end = range.end,
                "file_source::wait_range requesting on-demand download"
            );
            self.coord.request_range(range.clone());
        }

        self.inner
            .res
            .wait_range(range)
            .map_err(|e| StreamError::Source(SourceError::Storage(e)))
    }
}
