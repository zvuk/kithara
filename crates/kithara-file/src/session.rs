#![forbid(unsafe_code)]

use std::{
    collections::VecDeque,
    ops::Range,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
};

use futures::Stream;
use kithara_assets::{AssetResource, AssetStore, ResourceKey};
use kithara_events::{EventBus, FileEvent};
use kithara_net::{Headers, RangeSpec};
use kithara_platform::{Mutex, time::Duration};
use kithara_storage::{ResourceExt, WaitOutcome};
use kithara_stream::{
    AudioCodec, MediaInfo, ReadOutcome, SourcePhase, StreamError,
    dl::{Downloader, FetchCmd, FetchResult, OnConnectFn, Priority, ThrottleFn},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};
use url::Url;

use crate::{coord::FileCoord, error::SourceError};

// ---------------------------------------------------------------------------
// FileStreamState — creation helper (unchanged API)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct FileStreamState {
    pub(crate) res: AssetResource,
    pub(crate) bus: EventBus,
}

impl FileStreamState {
    pub(crate) fn create(
        assets: &Arc<AssetStore>,
        url: &Url,
        bus: Option<EventBus>,
        event_channel_capacity: usize,
    ) -> Result<Self, SourceError> {
        let key = ResourceKey::from_url(url);
        let res = assets.acquire_resource(&key).map_err(SourceError::Assets)?;
        let bus = bus.unwrap_or_else(|| EventBus::new(event_channel_capacity));
        Ok(Self { res, bus })
    }
}

// ---------------------------------------------------------------------------
// FSM phase
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePhase {
    /// Ready to yield the initial full-file download command.
    Init,
    /// Download in progress — waiting for `on_complete`.
    Downloading,
    /// File fully downloaded (or local).
    Complete,
}

// ---------------------------------------------------------------------------
// Shared inner state
// ---------------------------------------------------------------------------

pub(crate) struct FileInner {
    pub(crate) phase: FilePhase,
    pub(crate) queue: VecDeque<FetchCmd>,
    pub(crate) waker: Option<Waker>,

    // Resources
    pub(crate) coord: Arc<FileCoord>,
    pub(crate) res: AssetResource,
    pub(crate) bus: EventBus,
    pub(crate) cancel: CancellationToken,

    // Request params (for building on-demand FetchCmd)
    pub(crate) url: Url,
    pub(crate) headers: Option<Headers>,

    /// Codec discovered from HTTP Content-Type (set by `on_connect`).
    pub(crate) content_type_codec: Option<AudioCodec>,
}

impl FileInner {
    fn take_waker(&mut self) -> Option<Waker> {
        self.waker.take()
    }

    /// Build a `FetchCmd` for the full file download (streaming GET).
    fn build_full_download_cmd(
        inner: &Arc<Mutex<Self>>,
        look_ahead_bytes: Option<u64>,
    ) -> FetchCmd {
        let state = inner.lock_sync();
        let url = state.url.clone();
        let headers = state.headers.clone();
        let res = state.res.clone();
        let coord = Arc::clone(&state.coord);
        let bus = state.bus.clone();
        let cb_inner = Arc::clone(inner);
        drop(state);

        let offset = Arc::new(AtomicU64::new(0));

        let writer = {
            let res = res.clone();
            let coord = Arc::clone(&coord);
            let offset = Arc::clone(&offset);
            Box::new(move |chunk: &[u8]| {
                let pos = offset.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                res.write_at(pos, chunk)
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
                coord.set_download_pos(pos + chunk.len() as u64);
                Ok(())
            })
        };

        // Clone coord/inner for closures before on_complete moves the originals.
        let coord_connect = Arc::clone(&coord);
        let inner_connect = Arc::clone(inner);
        let coord_throttle = Arc::clone(&coord);

        let on_complete = Box::new(move |result: FetchResult| {
            let waker = {
                let mut state = cb_inner.lock_sync();
                match result {
                    FetchResult::Ok {
                        bytes_written,
                        ref headers,
                    } => {
                        let total = headers
                            .get("content-length")
                            .or_else(|| headers.get("Content-Length"))
                            .and_then(|v| v.parse::<u64>().ok())
                            .unwrap_or(bytes_written);
                        coord.set_total_bytes(Some(total));

                        if let Err(e) = state.res.commit(Some(bytes_written)) {
                            debug!(?e, "failed to commit file resource");
                        }

                        if let Some(codec) = headers
                            .get("content-type")
                            .or_else(|| headers.get("Content-Type"))
                            .and_then(AudioCodec::from_mime)
                        {
                            state.content_type_codec = Some(codec);
                        }

                        // Discard stale range requests that arrived while
                        // the full download was in flight — the resource is
                        // now committed and cannot accept writes.
                        let _ = state.coord.take_range_request();
                        state.queue.clear();

                        bus.publish(FileEvent::DownloadComplete {
                            total_bytes: bytes_written,
                        });
                        state.phase = FilePhase::Complete;
                    }
                    FetchResult::Err(e) => {
                        state.phase = FilePhase::Complete;
                        // Mark resource failed — unblocks wait_range condvar
                        // so the decoder thread doesn't hang forever.
                        state.res.fail(e.to_string());
                        bus.publish(FileEvent::DownloadError {
                            error: e.to_string(),
                        });
                    }
                }
                state.take_waker()
            };
            if let Some(w) = waker {
                w.wake();
            }
        });

        let on_connect: Option<OnConnectFn> = Some(Box::new(move |headers: &Headers| {
            let len = headers
                .get("content-length")
                .or_else(|| headers.get("Content-Length"))
                .and_then(|v| v.parse::<u64>().ok());
            if let Some(len) = len {
                coord_connect.set_total_bytes(Some(len));
            }
            if let Some(codec) = headers
                .get("content-type")
                .or_else(|| headers.get("Content-Type"))
                .and_then(AudioCodec::from_mime)
            {
                inner_connect.lock_sync().content_type_codec = Some(codec);
            }
        }));

        let throttle: Option<ThrottleFn> = look_ahead_bytes.map(|limit| {
            Box::new(move || {
                let dl = coord_throttle.timeline().download_position();
                let rd = coord_throttle.read_pos();
                dl > 0 && dl.saturating_sub(rd) > limit
            }) as ThrottleFn
        });

        FetchCmd {
            url,
            range: None,
            headers,
            priority: Priority::Normal,
            on_connect,
            writer,
            on_complete,
            throttle,
        }
    }

    /// Build a `FetchCmd` for an on-demand range request (seek).
    fn build_range_cmd(inner: &Arc<Mutex<Self>>, range: Range<u64>) -> FetchCmd {
        let state = inner.lock_sync();
        let url = state.url.clone();
        let headers = state.headers.clone();
        let res = state.res.clone();
        let coord = Arc::clone(&state.coord);
        let bus = state.bus.clone();
        let cb_inner = Arc::clone(inner);
        drop(state);

        let write_start = range.start;
        let offset = Arc::new(AtomicU64::new(write_start));

        let writer = {
            let res = res.clone();
            let coord = Arc::clone(&coord);
            let offset = Arc::clone(&offset);
            Box::new(move |chunk: &[u8]| {
                let pos = offset.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                match res.write_at(pos, chunk) {
                    Ok(()) => {
                        coord.set_download_pos(pos + chunk.len() as u64);
                        Ok(())
                    }
                    Err(e) => {
                        // If the resource was committed (full download finished
                        // while this range request was in flight), silently
                        // succeed — data is already available.
                        if matches!(
                            res.status(),
                            kithara_storage::ResourceStatus::Committed { .. }
                        ) {
                            Ok(())
                        } else {
                            Err(std::io::Error::other(e.to_string()))
                        }
                    }
                }
            })
        };

        let on_complete = Box::new(move |result: FetchResult| {
            let waker = {
                let mut state = cb_inner.lock_sync();
                if let FetchResult::Err(e) = result {
                    // Only fail the resource if it's not already committed
                    // (committed means the full download succeeded).
                    if !matches!(
                        state.res.status(),
                        kithara_storage::ResourceStatus::Committed { .. }
                    ) {
                        state.res.fail(e.to_string());
                        bus.publish(FileEvent::DownloadError {
                            error: e.to_string(),
                        });
                    }
                }
                state.take_waker()
            };
            if let Some(w) = waker {
                w.wake();
            }
        });

        let range_spec = RangeSpec::new(range.start, Some(range.end.saturating_sub(1)));

        FetchCmd {
            url,
            range: Some(range_spec),
            headers,
            priority: Priority::High,
            on_connect: None,
            writer,
            on_complete,
            throttle: None,
        }
    }
}

// ---------------------------------------------------------------------------
// FileSource — implements Source (sync) + Stream<Item = FetchCmd> (async)
// ---------------------------------------------------------------------------

/// File source: sync Read+Seek access AND async download command stream.
///
/// Created via [`File::create()`](crate::File). When shared with a
/// [`Downloader`](kithara_stream::dl::Downloader), clone the source
/// (cheap Arc clone) and register the clone.
#[derive(Clone)]
pub struct FileSource {
    inner: Arc<Mutex<FileInner>>,
    /// Shared coordination (outside Mutex for borrow-free access).
    coord: Arc<FileCoord>,
    /// Codec detected from HTTP Content-Type header.
    content_type_codec: Option<AudioCodec>,
    /// Keep Downloader alive for the lifetime of this source.
    _downloader: Option<Downloader>,
}

impl FileSource {
    /// Create a source for a local/cached file (no downloads needed).
    pub(crate) fn local(res: AssetResource, coord: Arc<FileCoord>, bus: EventBus) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FileInner {
                phase: FilePhase::Complete,
                queue: VecDeque::new(),
                waker: None,
                coord: Arc::clone(&coord),
                res,
                bus,
                cancel: CancellationToken::new(),
                url: Url::parse("file:///local").expect("valid url"),
                headers: None,
                content_type_codec: None,
            })),
            coord,
            content_type_codec: None,
            _downloader: None,
        }
    }

    /// Create a source for a remote file that needs downloading.
    pub(crate) fn remote(
        res: AssetResource,
        coord: Arc<FileCoord>,
        bus: EventBus,
        cancel: CancellationToken,
        url: Url,
        headers: Option<Headers>,
        look_ahead_bytes: Option<u64>,
    ) -> Self {
        let inner = Arc::new(Mutex::new(FileInner {
            phase: FilePhase::Init,
            queue: VecDeque::new(),
            waker: None,
            coord: Arc::clone(&coord),
            res,
            bus,
            cancel,
            url,
            headers,
            content_type_codec: None,
        }));

        // Enqueue the initial full-file download command.
        let cmd = FileInner::build_full_download_cmd(&inner, look_ahead_bytes);
        inner.lock_sync().queue.push_back(cmd);

        Self {
            inner,
            coord,
            content_type_codec: None,
            _downloader: None,
        }
    }

    /// Keep the downloader alive for the lifetime of this source.
    pub(crate) fn with_downloader(mut self, dl: Downloader) -> Self {
        self._downloader = Some(dl);
        self
    }
}

// ---------------------------------------------------------------------------
// Stream<Item = FetchCmd> — yields download commands for the Downloader
// ---------------------------------------------------------------------------

impl Stream for FileSource {
    type Item = FetchCmd;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<FetchCmd>> {
        let mut state = self.inner.lock_sync();
        state.waker = Some(cx.waker().clone());
        if let Some(range) = state.coord.take_range_request()
            && !state.res.contains_range(range.clone())
        {
            state.queue.clear();
            drop(state);
            let cmd = FileInner::build_range_cmd(&self.inner, range);
            return Poll::Ready(Some(cmd));
        }

        if let Some(cmd) = state.queue.pop_front() {
            if state.phase == FilePhase::Init {
                state.phase = FilePhase::Downloading;
            }
            drop(state);
            return Poll::Ready(Some(cmd));
        }

        let cancelled = state.cancel.is_cancelled();
        drop(state);

        if cancelled {
            return Poll::Ready(None);
        }

        Poll::Pending
    }
}

// ---------------------------------------------------------------------------
// Source trait — sync Read+Seek for decoder thread
// ---------------------------------------------------------------------------

impl kithara_stream::Source for FileSource {
    type Error = SourceError;
    type Topology = ();
    type Layout = ();
    type Coord = Arc<FileCoord>;
    type Demand = Range<u64>;

    fn topology(&self) -> &Self::Topology {
        &()
    }

    fn layout(&self) -> &Self::Layout {
        &()
    }

    fn coord(&self) -> &Self::Coord {
        &self.coord
    }

    #[cfg_attr(feature = "perf", hotpath::measure)]
    fn wait_range(
        &mut self,
        range: Range<u64>,
        timeout: Duration,
    ) -> kithara_stream::StreamResult<WaitOutcome, SourceError> {
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

        // Only issue an on-demand Range request when the initial download
        // has finished. While downloading, data will arrive from the
        // in-flight full GET — issuing a parallel Range request would
        // create two concurrent writers to the same resource.
        let download_done = state.phase == FilePhase::Complete;
        if download_done && !state.res.contains_range(range.clone()) {
            debug!(
                range_start = range.start,
                range_end = range.end,
                "file_source::wait_range requesting on-demand download"
            );
            state.coord.request_range(range.clone());
            if let Some(ref w) = state.waker {
                w.wake_by_ref();
            }
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

        if n == 0 {
            return Ok(ReadOutcome::Data(0));
        }

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

        Ok(ReadOutcome::Data(n))
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
        // Only issue on-demand fetch when the initial download has finished
        // AND the range is not already available. While downloading, data
        // arrives from the in-flight full GET.
        if state.phase != FilePhase::Complete || state.res.contains_range(range.clone()) {
            return;
        }
        state.coord.request_range(range);
        if let Some(ref w) = state.waker {
            w.wake_by_ref();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kithara_assets::{AssetStoreBuilder, ResourceKey};
    use kithara_events::Event;
    use kithara_platform::time::{Duration, sleep, timeout};
    use kithara_stream::{ReadOutcome, Source, Timeline};
    use kithara_test_utils::kithara;
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn make_coord(timeline: Timeline) -> Arc<FileCoord> {
        Arc::new(FileCoord::new(timeline))
    }

    fn make_source(res: AssetResource, coord: Arc<FileCoord>, bus: EventBus) -> FileSource {
        FileSource::local(res, coord, bus)
    }

    // FileCoord

    #[kithara::test]
    fn test_file_coord_initial_state() {
        let coord = FileCoord::new(Timeline::new());
        assert_eq!(coord.read_pos(), 0);
        assert_eq!(coord.timeline().download_position(), 0);
    }

    #[kithara::test]
    #[case::read(100, true)]
    #[case::download(500, false)]
    fn test_file_coord_set_and_get_positions(#[case] value: u64, #[case] read_pos: bool) {
        let coord = FileCoord::new(Timeline::new());
        if read_pos {
            coord.set_read_pos(value);
            assert_eq!(coord.read_pos(), value);
            assert_eq!(coord.timeline().download_position(), 0);
        } else {
            coord.set_download_pos(value);
            assert_eq!(coord.timeline().download_position(), value);
            assert_eq!(coord.timeline().download_position(), value);
            assert_eq!(coord.read_pos(), 0);
        }
    }

    // Demand slot

    #[kithara::test]
    fn file_coord_range_request_starts_empty() {
        let coord = FileCoord::new(Timeline::new());
        assert_eq!(coord.take_range_request(), None);
    }

    #[kithara::test]
    fn file_coord_range_request_replaces_previous() {
        let coord = FileCoord::new(Timeline::new());
        assert!(coord.request_range(0..50));
        assert!(coord.request_range(50..100));
        assert_eq!(coord.take_range_request(), Some(50..100));
        assert_eq!(coord.take_range_request(), None);
    }

    #[kithara::test]
    fn file_coord_range_request_dedupes_identical_request() {
        let coord = FileCoord::new(Timeline::new());
        assert!(coord.request_range(0..50));
        assert!(!coord.request_range(0..50));
        assert_eq!(coord.take_range_request(), Some(0..50));
        assert_eq!(coord.take_range_request(), None);
    }

    #[kithara::test]
    fn file_coord_total_bytes_roundtrip() {
        let coord = FileCoord::new(Timeline::new());
        assert_eq!(coord.total_bytes(), None);
        coord.set_total_bytes(Some(123));
        assert_eq!(coord.total_bytes(), Some(123));
        coord.set_total_bytes(None);
        assert_eq!(coord.total_bytes(), None);
    }

    // FileSource

    fn create_committed_resource(data: &[u8]) -> AssetResource {
        let store = AssetStoreBuilder::new()
            .ephemeral(true)
            .asset_root(Some("test"))
            .cancel(CancellationToken::new())
            .build();

        let key = ResourceKey::new("test.dat");
        let res = store.acquire_resource(&key).unwrap();
        res.write_at(0, data).unwrap();
        res.commit(Some(data.len() as u64)).unwrap();
        res
    }

    #[kithara::test]
    fn test_file_source_read_at() {
        let data = b"hello world from kithara";
        let res = create_committed_resource(data);

        let coord = make_coord(Timeline::new());
        let bus = EventBus::new(16);

        coord.set_total_bytes(Some(data.len() as u64));
        let mut source = make_source(res, Arc::clone(&coord), bus);

        let mut buf = [0u8; 11];
        assert_eq!(
            Source::read_at(&mut source, 0, &mut buf).unwrap(),
            ReadOutcome::Data(11)
        );
        assert_eq!(&buf[..11], b"hello world");
        assert_eq!(
            coord.read_pos(),
            0,
            "read_at must not advance the reader cursor outside Stream::read"
        );

        let mut buf2 = [0u8; 7];
        assert_eq!(
            Source::read_at(&mut source, 6, &mut buf2).unwrap(),
            ReadOutcome::Data(7)
        );
        assert_eq!(&buf2[..7], b"world f");
        assert_eq!(coord.read_pos(), 0);
    }

    #[kithara::test]
    fn file_source_emits_byte_progress_not_playback_truth() {
        let data = b"abcdef";
        let res = create_committed_resource(data);

        let coord = make_coord(Timeline::new());
        let bus = EventBus::new(16);
        let mut events = bus.subscribe();

        coord.set_total_bytes(Some(data.len() as u64));
        let mut source = make_source(res, coord, bus);

        let mut buf = [0u8; 3];
        assert_eq!(
            Source::read_at(&mut source, 0, &mut buf).unwrap(),
            ReadOutcome::Data(3)
        );

        let event = events.try_recv().expect("expected file event");
        match event {
            Event::File(FileEvent::ByteProgress { position, total }) => {
                assert_eq!(position, 3);
                assert_eq!(total, Some(6));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[kithara::test]
    fn test_file_source_len() {
        let res = create_committed_resource(b"abc");

        let coord = make_coord(Timeline::new());
        let bus = EventBus::new(16);

        coord.set_total_bytes(Some(12345));
        let source = make_source(res, coord, bus);

        assert_eq!(Source::len(&source), Some(12345));
    }

    #[kithara::test]
    fn file_source_phase_ready_when_range_present() {
        let data = b"hello world";
        let res = create_committed_resource(data);
        let coord = make_coord(Timeline::new());
        let bus = EventBus::new(16);
        coord.set_total_bytes(Some(data.len() as u64));
        let source = make_source(res, coord, bus);

        assert_eq!(source.phase_at(0..5), SourcePhase::Ready);
    }

    #[kithara::test]
    fn file_source_phase_seeking_when_data_not_ready() {
        let data = b"hello world";
        let res = create_committed_resource(data);
        let timeline = Timeline::new();
        let coord = make_coord(timeline.clone());
        let bus = EventBus::new(16);
        coord.set_total_bytes(Some(100));
        let source = make_source(res, coord, bus);

        let _ = timeline.initiate_seek(Duration::from_secs(0));

        assert_eq!(source.phase_at(50..60), SourcePhase::Seeking);
    }

    #[kithara::test]
    fn file_source_phase_ready_beats_seeking_when_data_present() {
        let data = b"hello world";
        let res = create_committed_resource(data);
        let timeline = Timeline::new();
        let coord = make_coord(timeline.clone());
        let bus = EventBus::new(16);
        coord.set_total_bytes(Some(data.len() as u64));
        let source = make_source(res, coord, bus);

        let _ = timeline.initiate_seek(Duration::from_secs(0));

        assert_eq!(source.phase_at(0..5), SourcePhase::Ready);
    }

    #[kithara::test]
    fn file_source_phase_eof_past_known_length() {
        let data = b"abc";
        let res = create_committed_resource(data);
        let coord = make_coord(Timeline::new());
        let bus = EventBus::new(16);
        coord.set_total_bytes(Some(data.len() as u64));
        let source = make_source(res, coord, bus);

        assert_eq!(source.phase_at(100..110), SourcePhase::Eof);
    }

    #[kithara::test]
    fn file_source_phase_parameterless_ready_when_current_byte_is_available() {
        let data = vec![0xABu8; 64];
        let res = create_committed_resource(&data[..16]);
        let coord = make_coord(Timeline::new());
        let bus = EventBus::new(16);
        coord.set_total_bytes(Some(data.len() as u64));
        let source = make_source(res, coord, bus);

        assert_eq!(Source::phase(&source), SourcePhase::Ready);
    }

    #[kithara::test]
    fn file_source_phase_parameterless_waiting_when_current_byte_is_missing() {
        let data = vec![0xABu8; 64];
        let res = create_committed_resource(&data[..16]);
        let coord = make_coord(Timeline::new());
        let bus = EventBus::new(16);
        coord.set_total_bytes(Some(data.len() as u64));
        coord.timeline().set_byte_position(32);
        let source = make_source(res, coord, bus);

        assert_eq!(Source::phase(&source), SourcePhase::Waiting);
    }

    #[kithara::test]
    fn file_source_phase_parameterless_eof_at_end() {
        let data = b"tiny";
        let res = create_committed_resource(data);
        let coord = make_coord(Timeline::new());
        let bus = EventBus::new(16);
        coord.set_total_bytes(Some(data.len() as u64));
        coord.timeline().set_byte_position(data.len() as u64);
        let source = make_source(res, coord, bus);

        assert_eq!(Source::phase(&source), SourcePhase::Eof);
    }

    #[kithara::test]
    fn file_source_wait_range_returns_interrupted_while_flushing() {
        let data = b"hello world from kithara";
        let res = create_committed_resource(data);
        let timeline = Timeline::new();
        let coord = make_coord(timeline.clone());
        let bus = EventBus::new(16);
        coord.set_total_bytes(Some(100));
        let mut source = make_source(res, coord, bus);

        let _ = timeline.initiate_seek(Duration::from_secs(0));

        let result = Source::wait_range(&mut source, 50..60, Duration::from_secs(1));
        assert_eq!(result.unwrap(), WaitOutcome::Interrupted);
    }

    #[kithara::test]
    fn file_source_demand_range_requests_downloader() {
        let res = create_committed_resource(b"abcdef");
        let coord = make_coord(Timeline::new());
        let bus = EventBus::new(16);
        let source = make_source(res, Arc::clone(&coord), bus);

        assert_eq!(coord.take_range_request(), None);

        Source::demand_range(&source, 512..513);

        assert_eq!(coord.take_range_request(), Some(512..513));
    }

    #[kithara::test]
    fn file_source_read_at_does_not_advance_timeline_position() {
        let res = create_committed_resource(b"abcdef");

        let timeline = Timeline::new();
        let coord = make_coord(timeline.clone());
        let bus = EventBus::new(16);
        coord.set_total_bytes(Some(6));
        let mut source = make_source(res, Arc::clone(&coord), bus);

        let mut buf = [0u8; 2];
        assert_eq!(
            Source::read_at(&mut source, 0, &mut buf).unwrap(),
            ReadOutcome::Data(2)
        );

        assert_eq!(coord.read_pos(), 0);
        assert_eq!(Source::timeline(&source).byte_position(), 0);

        coord.set_read_pos(5);
        assert_eq!(coord.read_pos(), 5);
        assert_eq!(Source::timeline(&source).byte_position(), 0);
    }
}
