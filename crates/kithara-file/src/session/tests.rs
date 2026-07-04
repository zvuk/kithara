use std::{num::NonZeroUsize, sync::Arc};

use kithara_assets::{
    AcquisitionResult, AssetReader, AssetStoreBuilder, StorageBackend, WriteSide,
};
use kithara_events::EventBus;
use kithara_platform::{CancelToken, time::Duration};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    PlayheadState, ReadOutcome, SeekState, Source, SourceError as StreamSourceError, SourcePhase,
    StreamError,
};
use kithara_test_utils::kithara;

use super::source::FileSource;
use crate::coord::FileCoord;

fn nz_bytes(n: usize) -> ReadOutcome {
    ReadOutcome::Bytes(NonZeroUsize::new(n).expect("test: byte count must be > 0"))
}

fn make_coord() -> Arc<FileCoord> {
    Arc::new(FileCoord::new(
        Arc::new(PlayheadState::new()),
        Arc::new(SeekState::new()),
    ))
}

fn make_source(reader: AssetReader, coord: Arc<FileCoord>, bus: EventBus) -> FileSource {
    let backend = Arc::new(
        AssetStoreBuilder::default()
            .cancel(CancelToken::never())
            .build(),
    );
    let key = backend.scope("test").key("test-source");
    FileSource::local(reader, coord, bus, backend, key, CancelToken::never(), None)
}

#[kithara::test]
fn test_file_coord_initial_state() {
    let coord = make_coord();
    assert_eq!(coord.read_pos(), 0);
}

#[kithara::test]
#[case::read(100, true)]
#[case::download(500, false)]
fn test_file_coord_set_and_get_positions(#[case] value: u64, #[case] read_pos: bool) {
    let coord = make_coord();
    if read_pos {
        coord.set_read_pos(value);
        assert_eq!(coord.read_pos(), value);
    } else {
        coord.set_download_pos(value);
        assert_eq!(
            coord.read_pos(),
            0,
            "download position is orthogonal to read position"
        );
    }
}

#[kithara::test]
fn file_coord_total_bytes_roundtrip() {
    let coord = make_coord();
    assert_eq!(coord.total_bytes(), None);
    coord.set_total_bytes(Some(123));
    assert_eq!(coord.total_bytes(), Some(123));
    coord.set_total_bytes(None);
    assert_eq!(coord.total_bytes(), None);
}

fn create_committed_resource(data: &[u8]) -> AssetReader {
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .cancel(CancelToken::never())
        .build();

    let key = store.scope("test").key("test.dat");
    let AcquisitionResult::Pending(writer) = store.acquire_resource(&key, None).unwrap() else {
        panic!("fresh acquire must be Pending");
    };
    writer.write_at(0, data).unwrap();
    writer.commit(Some(data.len() as u64)).unwrap()
}

fn create_active_resource(data: &[u8]) -> (AssetReader, kithara_assets::AssetWriter) {
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Memory)
        .cancel(CancelToken::never())
        .build();

    let key = store.scope("test").key("active.dat");
    let AcquisitionResult::Pending(writer) = store.acquire_resource(&key, None).unwrap() else {
        panic!("fresh acquire must be Pending");
    };
    writer.write_at(0, data).unwrap();
    (writer.reader(), writer)
}

#[kithara::test]
fn test_file_source_read_at() {
    let data = b"hello world from kithara";
    let res = create_committed_resource(data);

    let coord = make_coord();
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
fn test_file_source_len() {
    let res = create_committed_resource(b"abc");

    let coord = make_coord();
    let bus = EventBus::new(16);

    coord.set_total_bytes(Some(12345));
    let source = make_source(res, coord, bus);

    assert_eq!(Source::len(&source), Some(12345));
}

#[kithara::test]
#[case::ready_when_range_present(b"hello world", 11, 0..5, SourcePhase::Ready)]
#[case::eof_past_known_length(b"abc", 3, 100..110, SourcePhase::Eof)]
fn file_source_phase_at_range(
    #[case] data: &[u8],
    #[case] total_bytes: u64,
    #[case] range: std::ops::Range<u64>,
    #[case] expected: SourcePhase,
) {
    let res = create_committed_resource(data);
    let coord = make_coord();
    let bus = EventBus::new(16);
    coord.set_total_bytes(Some(total_bytes));
    let source = make_source(res, coord, bus);

    assert_eq!(source.phase_at(range), expected);
}

#[kithara::test]
#[case::seeking_when_data_not_ready(100, 50..60, SourcePhase::Seeking)]
#[case::ready_beats_seeking_when_data_present(11, 0..5, SourcePhase::Ready)]
fn file_source_phase_during_seek(
    #[case] total_bytes: u64,
    #[case] range: std::ops::Range<u64>,
    #[case] expected: SourcePhase,
) {
    let data = b"hello world";
    let res = create_committed_resource(data);
    let coord = make_coord();
    let bus = EventBus::new(16);
    coord.set_total_bytes(Some(total_bytes));
    let seek = coord.seek_control();
    let source = make_source(res, coord, bus);

    let _ = seek.begin(Duration::from_secs(0));

    assert_eq!(source.phase_at(range), expected);
}

#[kithara::test]
#[case::ready_when_current_byte_is_available(0, SourcePhase::Ready)]
#[case::waiting_when_current_byte_is_missing(32, SourcePhase::Waiting)]
#[case::eof_at_end(64, SourcePhase::Eof)]
fn file_source_phase_parameterless(#[case] position: u64, #[case] expected: SourcePhase) {
    let data = [0xABu8; 64];
    let res = create_committed_resource(&data[..16]);
    let coord = make_coord();
    let bus = EventBus::new(16);
    coord.set_total_bytes(Some(data.len() as u64));
    if position > 0 {
        coord.set_position(position);
    }
    let source = make_source(res, coord, bus);

    assert_eq!(Source::phase(&source), expected);
}

#[kithara::test]
fn file_source_wait_range_returns_interrupted_while_flushing() {
    let data = b"hello world from kithara";
    let res = create_committed_resource(data);
    let coord = make_coord();
    let bus = EventBus::new(16);
    coord.set_total_bytes(Some(100));
    let seek = coord.seek_control();
    let mut source = make_source(res, coord, bus);

    let _ = seek.begin(Duration::from_secs(0));

    let result = Source::wait_range(&mut source, 50..60, Some(Duration::from_secs(1)));
    assert_eq!(result.unwrap(), WaitOutcome::Interrupted);
}

#[kithara::test]
fn file_source_probe_wait_range_does_not_block_on_missing_bytes() {
    let (res, _writer) = create_active_resource(b"hello");
    let coord = make_coord();
    let bus = EventBus::new(16);
    coord.set_total_bytes(Some(100));
    let mut source = make_source(res, coord, bus);

    let result = Source::wait_range(&mut source, 0..10, Some(Duration::from_secs(1)));

    assert!(matches!(
        result,
        Err(StreamError::Source(StreamSourceError::WaitBudgetExceeded))
    ));
}

#[kithara::test]
fn file_source_probe_wait_range_clamps_read_ahead_at_known_eof() {
    let data = b"hello";
    let res = create_committed_resource(data);
    let coord = make_coord();
    let bus = EventBus::new(16);
    let total = u64::try_from(data.len()).expect("test data length fits u64");
    coord.set_total_bytes(Some(total));
    let mut source = make_source(res, coord, bus);

    let result = Source::wait_range(&mut source, 0..1024, Some(Duration::from_secs(1)));

    assert_eq!(result.unwrap(), WaitOutcome::Ready);
}

#[kithara::test]
fn file_source_read_at_does_not_advance_timeline_position() {
    let res = create_committed_resource(b"abcdef");

    let coord = make_coord();
    let bus = EventBus::new(16);
    coord.set_total_bytes(Some(6));
    let mut source = make_source(res, Arc::clone(&coord), bus);

    let mut buf = [0u8; 2];
    assert_eq!(
        Source::read_at(&mut source, 0, &mut buf).unwrap(),
        nz_bytes(2)
    );

    assert_eq!(coord.read_pos(), 0);
    assert_eq!(Source::position(&source), 0);

    coord.set_read_pos(5);
    assert_eq!(coord.read_pos(), 5);
    assert_eq!(Source::position(&source), 0);
}
