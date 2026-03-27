#![forbid(unsafe_code)]

use std::{ops::Range, sync::Arc};

use kithara_assets::{AssetResource, AssetStore, ResourceKey};
use kithara_events::{EventBus, FileEvent};
use kithara_net::Headers;
use kithara_platform::time::Duration;
use kithara_storage::{ResourceExt, WaitOutcome};
use kithara_stream::{AudioCodec, MediaInfo, ReadOutcome, SourcePhase, StreamError};
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace};
use url::Url;

use crate::{coord::FileCoord, error::SourceError};

#[derive(Debug, Clone)]
pub(crate) struct FileStreamState {
    pub(crate) url: Url,
    pub(crate) cancel: CancellationToken,
    pub(crate) res: AssetResource,
    pub(crate) bus: EventBus,
    pub(crate) headers: Option<Headers>,
}

impl FileStreamState {
    pub(crate) fn create(
        assets: &Arc<AssetStore>,
        url: Url,
        cancel: CancellationToken,
        bus: Option<EventBus>,
        event_channel_capacity: usize,
        headers: Option<Headers>,
    ) -> Result<Arc<Self>, SourceError> {
        let key = ResourceKey::from_url(&url);
        let res = assets.acquire_resource(&key).map_err(SourceError::Assets)?;

        let bus = bus.unwrap_or_else(|| EventBus::new(event_channel_capacity));

        Ok(Arc::new(Self {
            url,
            cancel,
            res,
            bus,
            headers,
        }))
    }

    pub(crate) fn url(&self) -> &Url {
        &self.url
    }

    pub(crate) fn res(&self) -> &AssetResource {
        &self.res
    }

    pub(crate) fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub(crate) fn headers(&self) -> Option<&Headers> {
        self.headers.as_ref()
    }

    pub(crate) fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }
}

/// File source implementing Source trait (sync).
///
/// Wraps storage resource with progress tracking and event emission.
/// Holds an optional [`Backend`](kithara_stream::Backend) to manage the
/// downloader lifecycle: when this source is dropped, the backend is dropped,
/// cancelling the downloader task automatically.
pub struct FileSource {
    coord: Arc<FileCoord>,
    res: AssetResource,
    bus: EventBus,
    /// Codec detected from HTTP Content-Type header.
    content_type_codec: Option<AudioCodec>,
    /// Downloader backend. Dropped with this source, cancelling the downloader.
    _backend: Option<kithara_stream::Backend>,
}

impl FileSource {
    /// Create new file source (no on-demand support — for local files).
    pub(crate) fn new(res: AssetResource, coord: Arc<FileCoord>, bus: EventBus) -> Self {
        Self {
            coord,
            res,
            bus,
            content_type_codec: None,
            _backend: None,
        }
    }

    /// Create new file source with backend (for remote files).
    pub(crate) fn with_backend(
        res: AssetResource,
        coord: Arc<FileCoord>,
        bus: EventBus,
        backend: kithara_stream::Backend,
    ) -> Self {
        Self {
            coord,
            res,
            bus,
            content_type_codec: None,
            _backend: Some(backend),
        }
    }

    /// Set codec detected from HTTP Content-Type header.
    pub(crate) fn with_content_type_codec(mut self, codec: Option<AudioCodec>) -> Self {
        self.content_type_codec = codec;
        self
    }
}

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

        // Fast-path via shared FSM: check for seek, EOF, or already-ready data
        // before touching downloader state or blocking on the resource.
        match self.phase_at(range.clone()) {
            SourcePhase::Seeking => return Ok(WaitOutcome::Interrupted),
            SourcePhase::Eof => return Ok(WaitOutcome::Eof),
            SourcePhase::Ready => return Ok(WaitOutcome::Ready),
            _ => {}
        }

        // Update read position so downloader knows where reader needs data.
        // This prevents backpressure deadlock when symphonia seeks ahead
        // (e.g., SeekFrom::End) while downloader is still near the beginning.
        if range.start > self.coord.read_pos() {
            self.coord.set_read_pos(range.start);
        }

        // If shared state exists, check if on-demand download is needed
        // BEFORE calling res.wait_range() — the resource blocks indefinitely
        // for Active resources when data is not available.
        //
        // Uses non-blocking contains_range() to detect two cases:
        // 1. Forward seek: data not yet downloaded
        // 2. Backward seek: data evicted by ring buffer wrap-around
        //    (WASM MemResource). Without re-download, wait_range() spins
        //    forever because evicted data is removed from the available set.
        //
        // No Committed guard — ring buffer resources lose data after commit
        // when capacity < total, so contains_range() is the only reliable check.
        if !self.res.contains_range(range.clone()) {
            debug!(
                range_start = range.start,
                range_end = range.end,
                "file_source::wait_range requesting on-demand download"
            );
            self.coord.request_range(range.clone());
        }

        // Wait on the resource. If on-demand was requested, the downloader
        // will write data via write_at, which notifies the resource's condvar
        // and unblocks this call.
        self.res
            .wait_range(range)
            .map_err(|e| StreamError::Source(SourceError::Storage(e)))
    }

    fn phase(&self) -> SourcePhase {
        let timeline = self.coord.timeline();
        let pos = timeline.byte_position();
        self.phase_at(pos..pos.saturating_add(1))
    }

    fn phase_at(&self, range: Range<u64>) -> SourcePhase {
        let timeline = self.coord.timeline();
        let past_eof = self
            .coord
            .total_bytes()
            .or_else(|| self.res.len())
            .is_some_and(|total| total > 0 && range.start >= total);
        if self.res.contains_range(range) {
            return SourcePhase::Ready;
        }
        if timeline.is_flushing() {
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
            .res
            .read_at(offset, buf)
            .map_err(|e| StreamError::Source(SourceError::Storage(e)))?;

        if n > 0 {
            let new_pos = offset.saturating_add(n as u64);
            let total = self.coord.total_bytes().or_else(|| self.res.len());

            self.bus.publish(FileEvent::ByteProgress {
                position: new_pos,
                total,
            });

            trace!(offset, bytes = n, "FileSource read complete");
        }

        Ok(ReadOutcome::Data(n))
    }

    fn len(&self) -> Option<u64> {
        self.coord.total_bytes().or_else(|| self.res.len())
    }

    fn media_info(&self) -> Option<MediaInfo> {
        // Pass only codec, not container. Container forces `new_direct`
        // (seek disabled during init) which can hang on streaming sources
        // where the worker blocks waiting for data. Without container,
        // the probe path with seek enabled is used instead.
        self.content_type_codec
            .map(|codec| MediaInfo::new(Some(codec), None))
    }

    fn demand_range(&self, range: Range<u64>) {
        self.coord.request_range(range);
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
        FileSource::new(res, coord, bus)
    }

    // FileCoord

    #[kithara::test]
    fn test_file_coord_initial_state() {
        let coord = FileCoord::new(Timeline::new());
        assert_eq!(coord.read_pos(), 0);
        assert_eq!(coord.download_pos(), 0);
    }

    #[kithara::test]
    #[case::read(100, true)]
    #[case::download(500, false)]
    fn test_file_coord_set_and_get_positions(#[case] value: u64, #[case] read_pos: bool) {
        let coord = FileCoord::new(Timeline::new());
        if read_pos {
            coord.set_read_pos(value);
            assert_eq!(coord.read_pos(), value);
            assert_eq!(coord.download_pos(), 0);
        } else {
            coord.set_download_pos(value);
            assert_eq!(coord.download_pos(), value);
            assert_eq!(coord.timeline().download_position(), value);
            assert_eq!(coord.read_pos(), 0);
        }
    }

    #[kithara::test(tokio)]
    async fn test_file_coord_signal_reader_advanced() {
        let coord = Arc::new(FileCoord::new(Timeline::new()));

        let notified = coord.notified_reader_advance();
        let notified_coord = Arc::clone(&coord);
        timeout(Duration::from_secs(2), async move {
            sleep(Duration::from_millis(10)).await;
            notified_coord.set_read_pos(42);
            notified.await;
        })
        .await
        .unwrap();

        assert_eq!(coord.read_pos(), 42);
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

    /// Helper: create a committed `AssetResource` backed by a file with `data`.
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

        // Read from an offset.
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

        // len() should return the explicit value provided at construction.
        assert_eq!(Source::len(&source), Some(12345));
    }

    // SourcePhase integration tests

    #[kithara::test]
    fn file_source_phase_ready_when_range_present() {
        let data = b"hello world";
        let res = create_committed_resource(data);
        let coord = make_coord(Timeline::new());
        let bus = EventBus::new(16);
        coord.set_total_bytes(Some(data.len() as u64));
        let source = make_source(res, coord, bus);

        assert_eq!(source.phase_at(0..5), SourcePhase::Ready,);
    }

    #[kithara::test]
    fn file_source_phase_seeking_when_data_not_ready() {
        let data = b"hello world";
        let res = create_committed_resource(data);
        let timeline = Timeline::new();
        let coord = make_coord(timeline.clone());
        let bus = EventBus::new(16);
        coord.set_total_bytes(Some(100)); // larger than actual data
        let source = make_source(res, coord, bus);

        // Initiate seek to make timeline flushing.
        let _ = timeline.initiate_seek(Duration::from_secs(0));

        // Range 50..60 is NOT present — seeking wins.
        assert_eq!(source.phase_at(50..60), SourcePhase::Seeking,);
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

        // Initiate seek to make timeline flushing.
        let _ = timeline.initiate_seek(Duration::from_secs(0));

        // Data IS present — Ready wins over Seeking (allows drain).
        assert_eq!(source.phase_at(0..5), SourcePhase::Ready,);
    }

    #[kithara::test]
    fn file_source_phase_eof_past_known_length() {
        let data = b"abc";
        let res = create_committed_resource(data);
        let coord = make_coord(Timeline::new());
        let bus = EventBus::new(16);
        coord.set_total_bytes(Some(data.len() as u64));
        let source = make_source(res, coord, bus);

        assert_eq!(source.phase_at(100..110), SourcePhase::Eof,);
    }

    // Parameterless phase() tests

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
        // Move position to end-of-file.
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
        coord.set_total_bytes(Some(100)); // larger than data
        let mut source = make_source(res, coord, bus);

        // Initiate seek to make timeline flushing.
        let _ = timeline.initiate_seek(Duration::from_secs(0));

        // Range 50..60 is NOT present, so Seeking phase fires (not Ready).
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

        assert_eq!(
            coord.take_range_request(),
            Some(512..513),
            "post-seek WaitingForSource must wake the file downloader via demand_range"
        );
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

        assert_eq!(
            coord.read_pos(),
            0,
            "FileSource read_at must not commit the reader cursor outside Stream::read"
        );
        assert_eq!(Source::timeline(&source).byte_position(), 0);

        coord.set_read_pos(5);
        assert_eq!(coord.read_pos(), 5);
        assert_eq!(Source::timeline(&source).byte_position(), 0);
    }
}
