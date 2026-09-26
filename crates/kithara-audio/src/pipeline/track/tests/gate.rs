use std::{
    io::{Read, SeekFrom},
    ops::Range,
};

use kithara_platform::{
    sync::{Arc, Mutex},
    thread,
};
use kithara_stream::{DeferredWake, SourcePhase, Stream};
use kithara_test_utils::kithara;

use super::rebuild::{TestConfig, TestControl, TestSource, TestStream, media_info};
use crate::pipeline::{
    decode::gate::{ReadinessGate, source_phase_for_wait_context},
    stream::shared::SharedStream,
    track::WaitContext,
};

async fn shared_with_phase(
    phase: SourcePhase,
) -> (SharedStream<TestStream>, Arc<Mutex<Vec<Range<u64>>>>) {
    let source = TestSource::new(Arc::new(TestControl::new(media_info(0))));
    *source.phase_handle().lock() = phase;
    let waits = source.waits_handle();
    let stream = match Stream::<TestStream>::new(TestConfig { source }).await {
        Ok(stream) => stream,
        Err(error) => panic!("test stream construction failed: {error}"),
    };
    (SharedStream::new(stream), waits)
}

/// The playback wait polls the decoder's forward read-ahead window by phase.
/// A poll that files no demand leaves the parked reader invisible to the
/// source: dispatch budgets cover only ranges the source knows a reader
/// waits on (`Source::wait_range`), so the owed window never reaches the
/// polled segments and the fetch queue head sits one past the cap forever
/// (the `phase_continuity` livelock). The filing itself runs off the produce
/// core: `Source::wait_range` takes source-side locks (`DemandState` on
/// file, reader-runtime state on HLS), so the poll arms a wait-free cell
/// and the scheduler shell delivers it.
#[kithara::test(tokio)]
async fn a_parked_playback_wait_arms_its_forward_window_as_demand() {
    let (shared, waits) = shared_with_phase(SourcePhase::Waiting).await;
    shared.set_position(1000);

    let phase = source_phase_for_wait_context(&shared, &WaitContext::Playback);

    assert_eq!(phase, SourcePhase::Waiting);
    assert!(
        waits.lock().is_empty(),
        "the poll itself must not call into `Source::wait_range` — that path \
         locks source state and is off-limits on the produce core"
    );
    shared.flush_demand();
    // The forward window is position..position+read_ahead, clamped to the
    // source length (TestSource reports 4096).
    assert_eq!(
        waits.lock().clone(),
        vec![1000..4096],
        "the shell flush must file the armed window as reader demand"
    );
}

/// Re-arms before a flush coalesce: the shell files one probe for the
/// latest polled window, not one per poll.
#[kithara::test(tokio)]
async fn repeated_parked_polls_coalesce_into_one_probe() {
    let (shared, waits) = shared_with_phase(SourcePhase::Waiting).await;
    shared.set_position(1000);

    let _ = source_phase_for_wait_context(&shared, &WaitContext::Playback);
    shared.set_position(2000);
    let _ = source_phase_for_wait_context(&shared, &WaitContext::Playback);
    shared.flush_demand();

    assert_eq!(
        waits.lock().clone(),
        vec![2000..4096],
        "coalesced flush must file the latest armed window exactly once"
    );
}

/// RT gate polls answer while a construction read holds the control mutex.
/// The real off-RT holder is a construction reader parked inside
/// `Stream::read` → `Source::wait_range(range, None)` with the mutex held;
/// a gate poll that touched that mutex would block the forbid-blocking
/// produce core behind the park (`RTSan`: `sched_yield` in `parking_lot`'s
/// contended acquire). The regression mode is this test hanging on the
/// poll until the harness watchdog fires.
#[kithara::test(tokio)]
async fn a_readiness_poll_answers_while_a_construction_read_holds_the_mutex() {
    let wake = Arc::new(DeferredWake::default());
    let source = TestSource::new(Arc::new(TestControl::new(media_info(0))))
        .with_peer_wake(Arc::clone(&wake));
    *source.phase_handle().lock() = SourcePhase::Waiting;
    let phase = source.phase_handle();
    let park = source.park_handle();
    let stream = match Stream::<TestStream>::new(TestConfig { source }).await {
        Ok(stream) => stream,
        Err(error) => panic!("test stream construction failed: {error}"),
    };
    let shared = SharedStream::new(stream);

    park.arm();
    let opened = shared.open_initial_reader();
    let Some(gate) = opened.construction_gate() else {
        panic!("initial reader must carry a construction gate");
    };
    gate.arm();
    let mut reader = opened.into_inner();
    let holder = thread::spawn_named("construction-read-holder", move || {
        let mut buf = [0u8; 64];
        reader.read(&mut buf)
    });
    park.wait_entered().await;

    assert!(
        !ReadinessGate::new(None).source_is_ready(&shared),
        "a waiting source must poll as not-ready without touching the held control mutex"
    );
    assert_eq!(
        source_phase_for_wait_context(&shared, &WaitContext::Playback),
        SourcePhase::Waiting,
        "the wait-context poll must answer from the probe while the mutex is held"
    );
    assert!(
        shared.abr_handle().is_none(),
        "the fixed-at-open ABR handle must answer (None here) while the mutex is held"
    );
    assert_eq!(
        shared.format_change_segment_range().ok(),
        Some(0..32),
        "the fixed variant-control handle must serve the mock's format range while the mutex is held"
    );
    let pos = match shared.probe_seek(SeekFrom::Start(64)) {
        Ok(pos) => pos,
        Err(error) => {
            panic!("the on-core probe seek must resolve while the mutex is held: {error}")
        }
    };
    assert_eq!(pos, 64);
    assert_eq!(
        shared.position(),
        64,
        "probe_seek must move the probe cursor"
    );
    assert!(
        wake.flush(),
        "probe_seek must arm the peer wake for the shell to flush"
    );

    // Teardown: EOF lets the released construction read return instead of
    // re-parking on the still-waiting phase.
    *phase.lock() = SourcePhase::Eof;
    park.release();
    holder
        .join()
        .expect("holder thread must exit cleanly")
        .expect("released construction read must complete");
}

/// A ready poll is a snapshot, not a wait: it must not arm demand, or the
/// hot produce path would re-file a look-ahead range on every tick.
#[kithara::test(tokio)]
async fn a_ready_playback_poll_files_no_demand() {
    let (shared, waits) = shared_with_phase(SourcePhase::Ready).await;
    shared.set_position(1000);

    let phase = source_phase_for_wait_context(&shared, &WaitContext::Playback);

    assert_eq!(phase, SourcePhase::Ready);
    shared.flush_demand();
    assert!(
        waits.lock().is_empty(),
        "a ready poll must stay a pure snapshot even across a shell flush"
    );
}
