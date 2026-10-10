use std::{
    io::{Read, SeekFrom},
    ops::Range,
};

use kithara_platform::{
    sync::{Arc, Mutex},
    thread,
};
use kithara_stream::{DeferredWake, Source, SourcePhase, Stream};
use kithara_test_utils::kithara;

use super::rebuild::{TestConfig, TestControl, TestSource, TestStream, media_info};
use crate::pipeline::stream::shared::SharedStream;

async fn shared_with_phase(
    phase: SourcePhase,
) -> (SharedStream<TestStream>, Arc<Mutex<Vec<Range<u64>>>>) {
    let source = TestSource::new(Arc::new(TestControl::new(media_info(0))));
    *source.phase_handle().lock() = phase;
    let waits = source.waits_handle();
    let stream = Stream::<TestStream>::new(TestConfig { source })
        .await
        .expect("test stream");
    (SharedStream::new(stream), waits)
}

#[kithara::test(tokio)]
async fn a_parked_playback_wait_arms_its_forward_window_as_demand() {
    let (mut shared, waits) = shared_with_phase(SourcePhase::Waiting).await;
    shared.set_position(1000);
    assert!(
        waits.lock().is_empty(),
        "construction files no playback demand"
    );
    let mut buffer = vec![0; 4096 - 1000];
    let error = shared.read(&mut buffer).expect_err("waiting probe");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(
        waits.lock().clone(),
        vec![1000..4096],
        "the owning reader files its window"
    );
}

#[kithara::test(tokio)]
async fn repeated_parked_polls_coalesce_into_one_probe() {
    let (mut shared, waits) = shared_with_phase(SourcePhase::Waiting).await;
    shared.set_position(1000);
    shared
        .read(&mut vec![0; 4096 - 1000])
        .expect_err("first probe");
    shared.set_position(2000);
    shared
        .read(&mut vec![0; 4096 - 2000])
        .expect_err("latest probe");
    assert_eq!(
        waits.lock().clone(),
        vec![1000..4096, 2000..4096],
        "each owner-thread read files its exact current window"
    );
}

#[kithara::test(tokio)]
async fn a_readiness_poll_answers_while_a_construction_read_holds_the_mutex() {
    let wake = Arc::new(DeferredWake::default());
    let source = TestSource::new(Arc::new(TestControl::new(media_info(0))))
        .with_peer_wake(Arc::clone(&wake));
    *source.phase_handle().lock() = SourcePhase::Waiting;
    let phase = source.phase_handle();
    let park = source.park_handle();
    let probe = source.probe();
    let stream = Stream::<TestStream>::new(TestConfig { source })
        .await
        .expect("test stream");
    let shared = SharedStream::new(stream);
    park.arm();
    let opened = shared.open_initial_reader();
    let gate = opened.construction_gate().expect("construction gate");
    gate.arm();
    let mut reader = opened.into_inner();
    let holder = thread::spawn_named("construction-read-holder", move || {
        reader.read(&mut [0; 64])
    });
    park.wait_entered().await;
    assert!(!matches!(
        probe.phase_at(0..64),
        SourcePhase::Ready | SourcePhase::Eof
    ));
    assert_eq!(probe.phase_at(0..64), SourcePhase::Waiting);
    assert!(shared.abr_handle().is_none());
    assert_eq!(shared.format_change_segment_range().ok(), Some(0..32));
    assert_eq!(
        shared.probe_seek(SeekFrom::Start(64)).expect("probe seek"),
        64
    );
    assert_eq!(shared.position(), 64);
    assert!(wake.flush());
    *phase.lock() = SourcePhase::Eof;
    park.release();
    holder
        .join()
        .expect("reader thread")
        .expect("released read");
}

#[kithara::test(tokio)]
async fn a_ready_playback_poll_files_no_demand() {
    let source = TestSource::new(Arc::new(TestControl::new(media_info(0))));
    let waits = source.waits_handle();
    let probe = source.probe();
    let stream = Stream::<TestStream>::new(TestConfig { source })
        .await
        .expect("test stream");
    let shared = SharedStream::new(stream);
    shared.set_position(1000);
    assert_eq!(probe.phase_at(1000..4096), SourcePhase::Ready);
    assert!(
        waits.lock().is_empty(),
        "a readiness snapshot is not a read demand"
    );
}

#[kithara::test(tokio)]
async fn a_readiness_wait_keeps_the_range_that_parked_the_decoder() {
    let control = Arc::new(TestControl::new(media_info(0)));
    control.enable_byte_map();
    let source = TestSource::new(control).with_peer_wake(Arc::new(DeferredWake::default()));
    *source.phase_handle().lock() = SourcePhase::WaitingDemand;
    let waits = source.waits_handle();
    let probe = source.probe();
    let stream = Stream::<TestStream>::new(TestConfig { source })
        .await
        .expect("test stream");
    let mut shared = SharedStream::new(stream);
    shared.set_position(100);
    assert!(!matches!(
        probe.phase_at(100..627),
        SourcePhase::Ready | SourcePhase::Eof
    ));
    shared.read(&mut [0; 1024]).expect_err("waiting init read");
    let parked_range = waits.lock().pop().expect("read demand");
    assert_eq!(parked_range, 100..627);
    assert_eq!(
        probe.phase_at(parked_range.clone()),
        SourcePhase::WaitingDemand
    );
    shared
        .read(&mut [0; 1024])
        .expect_err("retry same init read");
    assert_eq!(
        waits.lock().as_slice(),
        &[parked_range],
        "the following segment must not enter the demand"
    );
}
