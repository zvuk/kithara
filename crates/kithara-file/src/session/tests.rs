use std::{num::NonZeroUsize, path::PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::{sync::Barrier, thread};

use kithara_assets::{
    AcquisitionResult, AssetReader, AssetResource, AssetSource, AssetStore, ReadSide, ResourceKey,
    StorageBackend, WriteSide,
};
use kithara_config::ConfigOwner;
use kithara_events::{Envelope, EventBus};
#[cfg(not(target_arch = "wasm32"))]
use kithara_platform::CancelScope;
use kithara_platform::{CancelToken, sync::Arc, time::Duration};
use kithara_storage::{StorageError, WaitOutcome};
use kithara_stream::{
    AudioCodec, ContainerFormat, MediaInfo, NotReadyCause, PendingReason, PlayheadState,
    ReadOutcome, SeekState, Source, SourceError as StreamSourceError, SourcePhase, StreamError,
};
use kithara_test_utils::kithara;

use super::source::{FileLocalConfig, FileSource};
use crate::{
    File, FileConfig, FileEvent, FileSrc, TotalBytesSource,
    coord::FileCoord,
    test_pools::{TestPools, pools},
};

type TestFile = File<TestPools>;
type TestReader = AssetReader<TestPools>;
type TestSource = FileSource<TestPools>;
type TestStore = AssetStore<TestPools>;
type TestWriter = kithara_assets::AssetWriter<TestPools>;

fn test_key(store: &TestStore, name: &str) -> ResourceKey {
    let source = AssetSource::Remote {
        url: url::Url::parse("https://example.com/session-test").expect("test URL"),
        discriminator: Some("session-test".to_string()),
    };
    let scope = store.scope::<TestFile>(&source).expect("test scope");
    scope
        .key(&AssetResource::Named {
            namespace: "test".to_string(),
            name: name.to_string(),
        })
        .expect("test resource key")
}

fn nz_bytes(n: usize) -> ReadOutcome {
    ReadOutcome::Bytes(NonZeroUsize::new(n).expect("test: byte count must be > 0"))
}

fn make_coord() -> Arc<FileCoord> {
    Arc::new(FileCoord::new(
        Arc::new(PlayheadState::new()),
        Arc::new(SeekState::new()),
    ))
}

fn local_config() -> Arc<FileConfig<TestPools>> {
    Arc::new(
        FileConfig::for_src(FileSrc::Local(PathBuf::from("/session-test")))
            .store(
                AssetStore::builder(pools())
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .pools(pools())
            .reader_event_capacity(16)
            .build(),
    )
}

fn make_source(reader: TestReader, coord: Arc<FileCoord>, bus: EventBus) -> TestSource {
    make_source_with_cancel(reader, coord, bus, CancelToken::never())
}

fn make_source_with_cancel(
    reader: TestReader,
    coord: Arc<FileCoord>,
    bus: EventBus,
    cancel: CancelToken,
) -> TestSource {
    FileSource::local(
        FileLocalConfig::builder()
            .reader(reader)
            .coord(coord)
            .bus(bus)
            .cancel(cancel)
            .cached_media_info(MediaInfo::from(AudioCodec::Mp3))
            .build(),
        local_config(),
    )
}

#[kithara::test]
fn source_keeps_the_supplied_config_instance() {
    let config = local_config();
    let source = FileSource::local(
        FileLocalConfig::builder()
            .reader(create_committed_resource(b"ID3metadata"))
            .coord(make_coord())
            .bus(EventBus::new(16))
            .cancel(CancelToken::never())
            .build(),
        Arc::clone(&config),
    );

    assert!(std::ptr::eq(source.config(), config.as_ref()));
    assert_eq!(source.config().reader_event_capacity, 16);
}

#[kithara::test]
fn file_source_local_open_publishes_opened_and_size() {
    let reader = create_committed_resource(b"ID3metadata");
    let coord = make_coord();
    let bus = EventBus::new(16);
    let mut rx = bus.subscribe();

    let _source = make_source(reader, coord, bus);

    let opened = rx.try_recv().expect("opened event");
    assert!(matches!(
        opened,
        Envelope {
            event: FileEvent::Opened {
                codec: Some(AudioCodec::Mp3),
                container: Some(ContainerFormat::MpegAudio),
                total_bytes: Some(11),
                cached: true,
            },
            ..
        }
    ));
    let total = rx.try_recv().expect("size event");
    assert!(matches!(
        total,
        Envelope {
            event: FileEvent::TotalBytesResolved {
                total_bytes: 11,
                source: TotalBytesSource::CommittedLen,
            },
            ..
        }
    ));
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

fn create_committed_resource(data: &[u8]) -> TestReader {
    let store = AssetStore::builder(pools())
        .backend(StorageBackend::Memory)
        .cancel(CancelToken::never())
        .build();

    let key = test_key(&store, "test.dat");
    let AcquisitionResult::Pending(writer) = store.acquire_resource(&key, None).unwrap() else {
        panic!("fresh acquire must be Pending");
    };
    writer.write_at(0, data).unwrap();
    writer.commit(Some(data.len() as u64)).unwrap()
}

fn create_active_resource(data: &[u8]) -> (TestReader, TestWriter) {
    create_active_resource_with_cancel(data, CancelToken::never())
}

fn create_active_resource_with_cancel(
    data: &[u8],
    cancel: CancelToken,
) -> (TestReader, TestWriter) {
    let store = AssetStore::builder(pools())
        .backend(StorageBackend::Memory)
        .cancel(cancel)
        .build();

    let key = test_key(&store, "active.dat");
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
fn file_source_read_at_active_gap_reports_pending() {
    let (res, _writer) = create_active_resource(b"hello");
    let coord = make_coord();
    let bus = EventBus::new(16);
    let mut source = make_source(res, coord, bus);

    let mut buf = [0u8; 5];
    let outcome = Source::read_at(&mut source, 5, &mut buf).unwrap();

    assert_eq!(
        outcome,
        ReadOutcome::Pending(PendingReason::NotReady(NotReadyCause::SourcePending))
    );
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
fn file_source_phase_at_known_end_waits_until_active_commit() {
    let data = b"hello";
    let (res, _writer) = create_active_resource(data);
    let coord = make_coord();
    let bus = EventBus::new(16);
    let total = u64::try_from(data.len()).expect("test data length fits u64");
    coord.set_total_bytes(Some(total));
    let source = make_source(res, coord, bus);

    assert_eq!(source.phase_at(total..total + 1), SourcePhase::Waiting);
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
fn file_source_probe_is_ready_on_written_bytes_before_commit() {
    let (reader, _writer) = create_active_resource(b"hello");
    let coord = make_coord();
    coord.set_total_bytes(Some(5));
    let mut source = make_source(reader, coord, EventBus::new(16));

    let result = Source::wait_range(&mut source, 0..5, Some(Duration::from_millis(1)));

    assert_eq!(result.unwrap(), WaitOutcome::Ready);
}

#[kithara::test]
fn file_source_probe_wait_range_surfaces_terminal_storage_failure() {
    let (reader, writer) = create_active_resource(b"");
    writer.fail("network failed".to_string());
    let coord = make_coord();
    coord.set_total_bytes(Some(4));
    let mut source = make_source(reader, coord, EventBus::new(16));

    let result = Source::wait_range(&mut source, 0..4, Some(Duration::from_millis(1)));

    assert!(matches!(
        result,
        Err(StreamError::Source(StreamSourceError::SegmentUnavailable))
    ));
}

#[kithara::test]
fn file_source_probe_wait_range_surfaces_terminal_storage_cancellation() {
    let resource_cancel = CancelToken::never();
    let (reader, _writer) = create_active_resource_with_cancel(b"", resource_cancel.clone());
    let coord = make_coord();
    coord.set_total_bytes(Some(4));
    let mut source = make_source(reader, coord, EventBus::new(16));
    resource_cancel.cancel();

    assert_eq!(Source::phase_at(&source, 0..4), SourcePhase::Cancelled);
    let result = Source::wait_range(&mut source, 0..4, Some(Duration::from_millis(1)));

    assert!(matches!(
        result,
        Err(StreamError::Source(StreamSourceError::Storage(
            StorageError::Cancelled
        )))
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

#[kithara::test(native, timeout(Duration::from_secs(2)))]
fn source_cancel_interrupts_blocked_wait_without_poisoning_asset() {
    let (reader, writer) = create_active_resource(b"");
    let coord = make_coord();
    coord.set_total_bytes(Some(4));
    let scope = CancelScope::new(None);
    let mut source = FileSource::local(
        FileLocalConfig::builder()
            .reader(reader)
            .coord(coord)
            .bus(EventBus::new(16))
            .cancel(scope.token())
            .cached_media_info(MediaInfo::from(AudioCodec::Mp3))
            .build(),
        local_config(),
    );
    let entering_wait = Arc::new(Barrier::new(2));

    let handle = thread::spawn({
        let entering_wait = Arc::clone(&entering_wait);
        move || {
            entering_wait.wait();
            Source::wait_range(&mut source, 0..4, None)
        }
    });

    entering_wait.wait();
    scope.cancel();

    assert!(matches!(
        handle.join().expect("source waiter must not panic"),
        Err(StreamError::Source(StreamSourceError::Storage(
            StorageError::Cancelled
        )))
    ));

    writer.write_at(0, b"done").unwrap();
    let committed = writer.commit(Some(4)).unwrap();
    let mut bytes = [0; 4];
    assert_eq!(committed.read_at(0, &mut bytes).unwrap(), 4);
    assert_eq!(&bytes, b"done");
}

#[kithara::test]
fn pre_cancelled_source_is_terminal_for_ready_eof_and_probe_wait() {
    let cancel = CancelToken::never();
    cancel.cancel();

    let ready_coord = make_coord();
    ready_coord.set_total_bytes(Some(4));
    let mut ready = make_source_with_cancel(
        create_committed_resource(b"done"),
        ready_coord,
        EventBus::new(16),
        cancel.clone(),
    );
    assert_eq!(Source::phase_at(&ready, 0..1), SourcePhase::Cancelled);
    assert!(matches!(
        Source::wait_range(&mut ready, 0..1, Some(Duration::from_millis(1))),
        Err(StreamError::Source(StreamSourceError::Storage(
            StorageError::Cancelled
        )))
    ));
    let mut byte = [0];
    assert!(matches!(
        Source::read_at(&mut ready, 0, &mut byte),
        Err(StreamError::Source(StreamSourceError::Storage(
            StorageError::Cancelled
        )))
    ));

    let eof_coord = make_coord();
    eof_coord.set_total_bytes(Some(4));
    let mut eof = make_source_with_cancel(
        create_committed_resource(b"done"),
        eof_coord,
        EventBus::new(16),
        cancel.clone(),
    );
    assert_eq!(Source::phase_at(&eof, 4..5), SourcePhase::Cancelled);
    assert!(matches!(
        Source::wait_range(&mut eof, 4..5, None),
        Err(StreamError::Source(StreamSourceError::Storage(
            StorageError::Cancelled
        )))
    ));

    let (reader, _writer) = create_active_resource(b"");
    let waiting_coord = make_coord();
    waiting_coord.set_total_bytes(Some(4));
    let mut waiting = make_source_with_cancel(reader, waiting_coord, EventBus::new(16), cancel);
    assert!(matches!(
        Source::wait_range(&mut waiting, 0..4, Some(Duration::from_millis(1))),
        Err(StreamError::Source(StreamSourceError::Storage(
            StorageError::Cancelled
        )))
    ));
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

#[kithara::test]
fn file_magic_looks_past_id3_metadata() {
    let mut bytes = vec![b'I', b'D', b'3', 4, 0, 0, 0, 0, 0, 16];
    bytes.extend_from_slice(&[0; 16]);
    bytes.extend_from_slice(b"fLaC\x80\x00\x00\x22");
    let reader = create_committed_resource(&bytes);
    assert_eq!(
        super::inner::sniff_media_info(&reader),
        Some(MediaInfo::from(AudioCodec::Flac))
    );
}
