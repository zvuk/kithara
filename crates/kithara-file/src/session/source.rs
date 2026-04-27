//! Public `FileSource` and its `Source` trait implementation.
//!
//! `FileSource` is the synchronous Read+Seek-style interface used by the
//! decoder thread. The async download tasks in [`super::download`] mutate the
//! shared `FileInner` behind `Arc<Mutex<…>>`; this file only reads from it.

use std::{num::NonZeroUsize, ops::Range, sync::Arc};

use kithara_assets::{AssetResource, AssetStore, ResourceKey};
use kithara_events::{EventBus, FileEvent};
use kithara_net::Headers;
use kithara_platform::{Mutex, time::Duration, tokio::task};
use kithara_storage::{ResourceExt, WaitOutcome};
use kithara_stream::{
    AudioCodec, MediaInfo, ReadOutcome, SourcePhase, StreamError, Timeline, dl::PeerHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};
use url::Url;

use super::{
    download::{run_full_download, run_range_watcher},
    inner::{FileInner, FilePhase, FileStreamState},
};
use crate::{coord::FileCoord, error::SourceError};

/// File source: sync Read+Seek access backed by async downloads via
/// [`PeerHandle`].
///
/// Created via [`File::create()`](crate::File). Downloads are driven
/// by spawned async tasks that call [`PeerHandle::execute`].
#[derive(Clone)]
pub struct FileSource {
    inner: Arc<Mutex<FileInner>>,
    /// Shared coordination (outside Mutex for borrow-free access).
    coord: Arc<FileCoord>,
    /// Codec detected from HTTP Content-Type header.
    content_type_codec: Option<AudioCodec>,
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
        Self {
            inner: Arc::new(Mutex::new(FileInner {
                phase: FilePhase::Complete,
                coord: Arc::clone(&coord),
                res,
                bus,
                cancel: CancellationToken::new(),
                url: Url::parse("file:///local").expect("valid url"),
                headers: None,
                content_type_codec: None,
                backend,
                key,
            })),
            coord,
            content_type_codec: None,
        }
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
        let inner = Arc::new(Mutex::new(FileInner {
            phase: FilePhase::Init,
            coord: Arc::clone(&coord),
            res: state.res.clone(),
            bus: state.bus.clone(),
            cancel: cancel.clone(),
            url,
            headers,
            content_type_codec: None,
            backend: Arc::clone(&state.backend),
            key: state.key.clone(),
        }));

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

        Self {
            inner,
            coord,
            content_type_codec: None,
        }
    }
}

impl kithara_stream::Source for FileSource {
    type Error = SourceError;

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

        let state = self.inner.lock_sync();
        if range.start > state.coord.read_pos() {
            state.coord.set_read_pos(range.start);
        }

        // Issue on-demand Range request when data is missing.
        if !state.res.contains_range(range.clone()) {
            debug!(
                range_start = range.start,
                range_end = range.end,
                "file_source::wait_range requesting on-demand download"
            );
            state.coord.request_range(range.clone());
        }

        let res = state.res.clone();
        drop(state);

        res.wait_range(range)
            .map_err(|e| StreamError::Source(SourceError::Storage(e)))
    }

    fn phase(&self) -> SourcePhase {
        let pos = self.coord.timeline().byte_position();
        self.phase_at(pos..pos.saturating_add(1))
    }

    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        let state = self.inner.lock_sync();
        let contains = state.res.contains_range(range.clone());
        let res_len = state.res.len();
        drop(state);

        if contains {
            return SourcePhase::Ready;
        }

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
        let state = self.inner.lock_sync();
        let n = state
            .res
            .read_at(offset, buf)
            .map_err(|e| StreamError::Source(SourceError::Storage(e)))?;

        let Some(count) = NonZeroUsize::new(n) else {
            return Ok(ReadOutcome::Eof);
        };

        let res_len = state.res.len();
        let bus = state.bus.clone();
        drop(state);

        let new_pos = offset.saturating_add(n as u64);
        let total = self.coord.total_bytes().or(res_len);
        bus.publish(FileEvent::ByteProgress {
            position: new_pos,
            total,
        });
        trace!(offset, bytes = n, "FileSource read complete");

        Ok(ReadOutcome::Bytes(count))
    }

    fn len(&self) -> Option<u64> {
        self.coord
            .total_bytes()
            .or_else(|| self.inner.lock_sync().res.len())
    }

    fn media_info(&self) -> Option<MediaInfo> {
        let codec = self
            .content_type_codec
            .or(self.inner.lock_sync().content_type_codec);
        codec.map(|c| MediaInfo::new(Some(c), None))
    }

    fn demand_range(&self, range: Range<u64>) {
        let state = self.inner.lock_sync();
        if state.res.contains_range(range.clone()) {
            return;
        }
        state.coord.request_range(range);
    }
}
