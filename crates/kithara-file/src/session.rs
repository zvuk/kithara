#![forbid(unsafe_code)]

use std::{ops::Range, sync::Arc};

use futures::StreamExt;
use kithara_assets::{AssetResource, AssetStore, ResourceKey};
use kithara_events::{EventBus, FileEvent};
use kithara_net::{Headers, RangeSpec};
use kithara_platform::{
    Mutex,
    time::Duration,
    tokio,
    tokio::{task, time as tokio_time},
};
use kithara_storage::{ResourceExt, ResourceStatus, WaitOutcome};
use kithara_stream::{
    AudioCodec, MediaInfo, ReadOutcome, SourcePhase, StreamError, Timeline,
    dl::{FetchCmd, Peer, PeerHandle, Priority, reject_html_response},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};
use url::Url;

use crate::{coord::FileCoord, error::SourceError};

/// Backpressure pause when download is too far ahead of reader.
const THROTTLE_PAUSE: Duration = Duration::from_millis(10);

// FilePeer — Peer impl for file protocol

pub(crate) struct FilePeer {
    /// Same Arc-clone as the one held by `FileCoord` — reads from the
    /// audio FSM's PLAYING flag route this track's fetches to the
    /// High-priority slot while the listener is actively consuming it.
    timeline: Timeline,
}

impl FilePeer {
    pub(crate) fn new(timeline: Timeline) -> Self {
        Self { timeline }
    }
}

impl Peer for FilePeer {
    /// Priority reflects the audio FSM's decode-activity flag on the
    /// shared `Timeline`. Cheap, lock-free — called by Registry on
    /// every `poll_peers` pass.
    fn priority(&self) -> Priority {
        if self.timeline.is_playing() {
            Priority::High
        } else {
            Priority::Low
        }
    }
}

// FileStreamState — creation helper (unchanged API)

#[derive(Debug, Clone)]
pub(crate) struct FileStreamState {
    pub(crate) res: AssetResource,
    pub(crate) bus: EventBus,
    pub(crate) backend: Arc<AssetStore>,
    pub(crate) key: ResourceKey,
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
        Ok(Self {
            res,
            bus,
            backend: Arc::clone(assets),
            key,
        })
    }
}

// FSM phase

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePhase {
    /// Ready to start the initial full-file download.
    Init,
    /// Download in progress.
    Downloading,
    /// File fully downloaded (or local).
    Complete,
}

// Shared inner state

pub(crate) struct FileInner {
    pub(crate) phase: FilePhase,

    // Resources
    pub(crate) coord: Arc<FileCoord>,
    pub(crate) res: AssetResource,
    pub(crate) bus: EventBus,
    pub(crate) cancel: CancellationToken,

    // Request params
    pub(crate) url: Url,
    pub(crate) headers: Option<Headers>,

    /// Codec discovered from HTTP Content-Type.
    pub(crate) content_type_codec: Option<AudioCodec>,

    /// Owning asset store — needed so a failed download can tear down the
    /// pre-allocated mmap immediately instead of waiting for
    /// `LeaseResource::Drop` to fire at `Stream<File>`-drop time (which is
    /// typically app shutdown because `FileInner.res` holds a clone that
    /// outlives every local transaction).
    pub(crate) backend: Arc<AssetStore>,
    /// Cache key for `backend.remove_resource` on failure.
    pub(crate) key: ResourceKey,
}

impl FileInner {
    /// Mark the resource failed and evict the pre-allocated cache file.
    ///
    /// Call on any terminal download error so the file is gone from disk
    /// before the task returns — without this the clone in `FileInner.res`
    /// keeps the mmap parked in the cache directory for the full lifetime
    /// of the holding `Stream<File>`.
    pub(crate) fn fail_and_evict(&mut self, reason: &str) {
        self.phase = FilePhase::Complete;
        self.res.fail(reason.to_string());
        self.backend.remove_resource(&self.key);
    }
}

// FileSource — implements Source (sync Read+Seek)

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

// Async download tasks

/// Full-file streaming download.
///
/// Builds a GET command, executes via [`PeerHandle`], streams body
/// chunks to the resource, and commits on completion.
async fn run_full_download(
    inner: Arc<Mutex<FileInner>>,
    peer: PeerHandle,
    look_ahead_bytes: Option<u64>,
) {
    let (url, headers, res, coord, bus, cancel) = {
        let state = inner.lock_sync();
        (
            state.url.clone(),
            state.headers.clone(),
            state.res.clone(),
            Arc::clone(&state.coord),
            state.bus.clone(),
            state.cancel.clone(),
        )
    };

    inner.lock_sync().phase = FilePhase::Downloading;

    let cmd = FetchCmd::get(url)
        .headers(headers)
        .with_validator(reject_html_response);

    let resp = match peer.execute(cmd).await {
        Ok(r) => r,
        Err(e) => {
            inner.lock_sync().fail_and_evict(&e.to_string());
            bus.publish(FileEvent::DownloadError {
                error: e.to_string(),
            });
            return;
        }
    };

    // Process response headers.
    let expected_len = process_response_headers(&inner, &resp.headers, &coord);

    // Stream body to resource.
    let result = stream_body_to_resource(resp.body, &res, &coord, &cancel, look_ahead_bytes).await;

    match result {
        Err(StreamBodyError::Write(e)) => {
            debug!(?e, "write error during full download");
            inner.lock_sync().fail_and_evict(&e.to_string());
            bus.publish(FileEvent::DownloadError {
                error: e.to_string(),
            });
        }
        Err(StreamBodyError::Net(e, written)) => {
            debug!("stream error during full download: {e}");
            let mut state = inner.lock_sync();
            // Only tear down the pre-allocated mmap when nothing was
            // persisted — any written bytes are still useful to the
            // reader and must not be discarded.
            if written == 0 {
                state.fail_and_evict(&e.to_string());
            } else {
                state.phase = FilePhase::Complete;
            }
            drop(state);
            bus.publish(FileEvent::DownloadError {
                error: e.to_string(),
            });
        }
        Err(StreamBodyError::Cancelled) => {}
        Ok(bytes_written) => {
            commit_full_download(&inner, bytes_written, expected_len, &bus);
        }
    }
}

/// Extract content-length and content-type from response headers.
fn process_response_headers(
    inner: &Arc<Mutex<FileInner>>,
    headers: &Headers,
    coord: &Arc<FileCoord>,
) -> Option<u64> {
    let expected_len = headers
        .get("content-length")
        .or_else(|| headers.get("Content-Length"))
        .and_then(|v| v.parse::<u64>().ok());
    if let Some(len) = expected_len {
        coord.set_total_bytes(Some(len));
    }
    if let Some(codec) = headers
        .get("content-type")
        .or_else(|| headers.get("Content-Type"))
        .and_then(AudioCodec::from_mime)
    {
        inner.lock_sync().content_type_codec = Some(codec);
    }
    expected_len
}

enum StreamBodyError {
    Write(kithara_storage::StorageError),
    Net(kithara_net::NetError, u64),
    Cancelled,
}

/// Stream body chunks into a resource, returning total bytes written.
async fn stream_body_to_resource(
    mut body: kithara_stream::dl::BodyStream,
    res: &AssetResource,
    coord: &Arc<FileCoord>,
    cancel: &CancellationToken,
    look_ahead_bytes: Option<u64>,
) -> Result<u64, StreamBodyError> {
    let mut written: u64 = 0;

    while let Some(chunk) = body.next().await {
        match chunk {
            Ok(data) => {
                let pos = written;
                written += data.len() as u64;
                if let Err(e) = res.write_at(pos, &data) {
                    return Err(StreamBodyError::Write(e));
                }
                coord.set_download_pos(written);

                if let Some(limit) = look_ahead_bytes {
                    while written > 0 && written.saturating_sub(coord.read_pos()) > limit {
                        if cancel.is_cancelled() {
                            return Err(StreamBodyError::Cancelled);
                        }
                        tokio_time::sleep(THROTTLE_PAUSE).await;
                    }
                }
            }
            Err(e) => return Err(StreamBodyError::Net(e, written)),
        }
    }

    Ok(written)
}

/// Commit a completed full download.
fn commit_full_download(
    inner: &Arc<Mutex<FileInner>>,
    bytes_written: u64,
    expected_len: Option<u64>,
    bus: &EventBus,
) {
    let expected = expected_len.unwrap_or(bytes_written);
    let mut state = inner.lock_sync();

    if bytes_written >= expected {
        if let Err(e) = state.res.commit(Some(bytes_written)) {
            debug!(?e, "failed to commit file resource");
        }
        let _ = state.coord.take_range_request();
        state.phase = FilePhase::Complete;
        drop(state);
        bus.publish(FileEvent::DownloadComplete {
            total_bytes: bytes_written,
        });
    } else {
        state.phase = FilePhase::Complete;
        drop(state);
        debug!(
            bytes_written,
            expected, "partial download, resource stays active"
        );
        bus.publish(FileEvent::DownloadError {
            error: format!("incomplete: {bytes_written}/{expected} bytes"),
        });
    }
}

/// Watch for on-demand range requests and spawn range downloads.
///
/// Waits on the coordinator's demand signal. When a range is
/// requested (e.g. by seek), spawns a range download task.
async fn run_range_watcher(
    inner: Arc<Mutex<FileInner>>,
    peer: PeerHandle,
    coord: Arc<FileCoord>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            () = coord.demand_notify().notified() => {}
        }

        while let Some(range) = coord.take_range_request() {
            let res_contains = inner.lock_sync().res.contains_range(range.clone());
            if res_contains {
                continue;
            }
            let task_inner = Arc::clone(&inner);
            let task_peer = peer.clone();
            task::spawn(async move {
                run_range_download(task_inner, task_peer, range).await;
            });
        }
    }
}

/// Download a byte range (on-demand seek fill).
///
/// Replaces the old `build_range_cmd` with its callback machinery.
async fn run_range_download(inner: Arc<Mutex<FileInner>>, peer: PeerHandle, range: Range<u64>) {
    let (url, headers, res, coord, bus) = {
        let state = inner.lock_sync();
        (
            state.url.clone(),
            state.headers.clone(),
            state.res.clone(),
            Arc::clone(&state.coord),
            state.bus.clone(),
        )
    };

    let range_spec = RangeSpec::new(range.start, Some(range.end.saturating_sub(1)));
    let cmd = FetchCmd::get(url)
        .range(Some(range_spec))
        .headers(headers)
        .with_validator(reject_html_response);

    let resp = match peer.execute(cmd).await {
        Ok(r) => r,
        Err(e) => {
            let state = inner.lock_sync();
            if !matches!(state.res.status(), ResourceStatus::Committed { .. }) {
                state.res.fail(e.to_string());
                drop(state);
                bus.publish(FileEvent::DownloadError {
                    error: e.to_string(),
                });
            }
            return;
        }
    };

    let mut written = range.start;
    let mut body = resp.body;

    while let Some(chunk) = body.next().await {
        match chunk {
            Ok(data) => {
                let pos = written;
                written += data.len() as u64;
                match res.write_at(pos, &data) {
                    Ok(()) => {
                        coord.set_download_pos(written);
                    }
                    Err(e) => {
                        // If the resource was committed (full download finished
                        // while this range request was in flight), silently
                        // succeed — data is already available.
                        if !matches!(res.status(), ResourceStatus::Committed { .. }) {
                            debug!(?e, "range write error");
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                let state = inner.lock_sync();
                if !matches!(state.res.status(), ResourceStatus::Committed { .. }) {
                    state.res.fail(e.to_string());
                    drop(state);
                    bus.publish(FileEvent::DownloadError {
                        error: e.to_string(),
                    });
                }
                return;
            }
        }
    }
}

// Source trait — sync Read+Seek for decoder thread

impl kithara_stream::Source for FileSource {
    type Error = SourceError;

    fn timeline(&self) -> Timeline {
        self.coord.timeline()
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
        if state.res.contains_range(range.clone()) {
            return;
        }
        state.coord.request_range(range);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kithara_assets::{AssetStoreBuilder, ResourceKey};
    use kithara_events::Event;
    use kithara_platform::time::Duration;
    use kithara_stream::{ReadOutcome, Source, Timeline};
    use kithara_test_utils::kithara;
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn make_coord(timeline: Timeline) -> Arc<FileCoord> {
        Arc::new(FileCoord::new(timeline))
    }

    fn make_source(res: AssetResource, coord: Arc<FileCoord>, bus: EventBus) -> FileSource {
        let backend = Arc::new(
            AssetStoreBuilder::new()
                .asset_root(None)
                .cancel(CancellationToken::new())
                .build(),
        );
        let key = ResourceKey::new("test-source");
        FileSource::local(res, coord, bus, backend, key)
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
        let data = [0xABu8; 64];
        let res = create_committed_resource(&data[..16]);
        let coord = make_coord(Timeline::new());
        let bus = EventBus::new(16);
        coord.set_total_bytes(Some(data.len() as u64));
        let source = make_source(res, coord, bus);

        assert_eq!(Source::phase(&source), SourcePhase::Ready);
    }

    #[kithara::test]
    fn file_source_phase_parameterless_waiting_when_current_byte_is_missing() {
        let data = [0xABu8; 64];
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
