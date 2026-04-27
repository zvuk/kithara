//! Tests for `FileCoord` and `FileSource`.

use std::{num::NonZeroUsize, sync::Arc};

use kithara_assets::{AssetResource, AssetStoreBuilder, ResourceKey};
use kithara_events::{Event, EventBus, FileEvent};
use kithara_platform::time::Duration;
use kithara_storage::{ResourceExt, WaitOutcome};
use kithara_stream::{ReadOutcome, Source, SourcePhase, Timeline};
use kithara_test_utils::kithara;
use tokio_util::sync::CancellationToken;

use super::source::FileSource;
use crate::coord::FileCoord;

fn nz_bytes(n: usize) -> ReadOutcome {
    ReadOutcome::Bytes(NonZeroUsize::new(n).expect("test: byte count must be > 0"))
}

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
        nz_bytes(11)
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
        nz_bytes(7)
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
        nz_bytes(3)
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

    let result = Source::wait_range(&mut source, 50..60, Some(Duration::from_secs(1)));
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
        nz_bytes(2)
    );

    assert_eq!(coord.read_pos(), 0);
    assert_eq!(Source::timeline(&source).byte_position(), 0);

    coord.set_read_pos(5);
    assert_eq!(coord.read_pos(), 5);
    assert_eq!(Source::timeline(&source).byte_position(), 0);
}
