#![cfg(not(target_arch = "wasm32"))]
use kithara_platform::sync::WaitGate;
use kithara_signal::SegmentId;
use kithara_test_utils::kithara;
use ringbuf::traits::Observer;

use super::super::core::PcmReceiver;
use crate::{
    mock::pcm_fixture::{PcmFixture, chunk},
    worker::PcmPacket,
};

fn pop_blocking(reader: &mut PcmReceiver) -> Option<PcmPacket> {
    reader.wait_for_packet();
    reader.pop()
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::test(hang_timeout_secs(1))]
#[should_panic(expected = "wait_for_packet")]
fn blocking_recv_without_preload_panics_when_no_chunk_arrives() {
    let runtime = kithara_platform::tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("fixture runtime");
    let fixture = runtime.block_on(PcmFixture::new(4, true));
    fixture
        .receiver
        .as_ref()
        .expect("blocking receiver")
        .wait_for_packet();
}

#[kithara::test(native, tokio)]
async fn a_nonblocking_poll_of_an_empty_ring_states_its_demand() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    use kithara_platform::time::Duration;
    use kithara_worker::{
        DispatcherConfig, Event, Observer, Task, TaskConfig, TickResult, Worker, WorkerConfig,
    };

    struct DemandTask(Arc<AtomicUsize>);

    impl Task for DemandTask {
        fn tick(&mut self) -> TickResult {
            self.0.fetch_add(1, Ordering::Relaxed);
            TickResult::Backpressured
        }
    }

    struct DemandObserver {
        parked: mpsc::Sender<()>,
        resume: mpsc::Receiver<()>,
    }

    impl Observer for DemandObserver {
        fn on_event(&mut self, event: Event) {
            if matches!(event, Event::Backpressured(_)) {
                self.parked.send(()).expect("report producer backpressure");
                self.resume.recv().expect("resume producer scheduler");
            }
        }
    }

    let ticks = Arc::new(AtomicUsize::new(0));
    let (parked, observed) = mpsc::channel();
    let (resume, proceed) = mpsc::channel();
    let worker = Worker::new(WorkerConfig::new());
    let dispatcher = worker.dispatcher(
        DispatcherConfig::builder()
            .name("empty-ring-demand")
            .wait_timeout(Duration::from_secs(60))
            .backpressure_poll_interval(Duration::from_secs(60))
            .observer(DemandObserver {
                parked,
                resume: proceed,
            })
            .build(),
    );
    let counter = Arc::clone(&ticks);
    let _task = dispatcher
        .register(TaskConfig::new(), move |_| DemandTask(counter))
        .expect("register demand probe");
    let mut fixture = PcmFixture::with_wake(
        4,
        false,
        crate::worker::scheduler::StreamWake::new(dispatcher.wake_handle()),
    )
    .await;
    let timeout = Duration::from_secs(2);
    observed
        .recv_timeout(timeout)
        .expect("first backpressured pass");
    dispatcher.wake_handle().wake();
    resume.send(()).expect("consume the registration wake");
    observed
        .recv_timeout(timeout)
        .expect("registration wake consumed");
    let before = ticks.load(Ordering::Relaxed);
    let receiver = fixture.receiver.as_mut().expect("receiver");
    assert!(receiver.pop().is_none());
    assert!(receiver.ready.is_none());
    resume.send(()).expect("park after empty poll");
    observed
        .recv_timeout(timeout)
        .expect("empty poll must wake the producer");
    assert_eq!(
        ticks.load(Ordering::Relaxed),
        before + 1,
        "an empty poll schedules exactly one producer pass"
    );
    dispatcher.shutdown();
    resume.send(()).expect("release observer for shutdown");
}

#[kithara::test(native, tokio)]
async fn block_on_underrun_forces_immediate_off_rt_wakes() {
    let wake = kithara_worker::Wake::default();
    let mut fixture = PcmFixture::with_wake(
        4,
        true,
        crate::worker::scheduler::StreamWake::new(wake.clone()),
    )
    .await;
    assert!(fixture.receiver.as_ref().expect("receiver").ready.is_some());
    assert_eq!(kithara_worker::mock::wake_state(&wake), (0, false));
    assert!(fixture.receiver.as_mut().expect("receiver").pop().is_none());
    assert_eq!(kithara_worker::mock::wake_state(&wake), (1, false));
    fixture
        .receiver
        .as_mut()
        .expect("receiver")
        .recycle(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[1.0]))))
        .expect("return fits");
    assert_eq!(kithara_worker::mock::wake_state(&wake), (2, false));
}

#[kithara::test(native, tokio)]
async fn an_adopted_mode_still_yields_to_blocking_reads() {
    let wake = kithara_worker::Wake::default();
    let mut fixture = PcmFixture::with_wake(
        4,
        true,
        crate::worker::scheduler::StreamWake::new(wake.clone()),
    )
    .await;
    assert!(fixture.receiver.as_ref().expect("receiver").ready.is_some());
    assert!(
        fixture
            .producer
            .as_ref()
            .expect("producer")
            .ready
            .0
            .is_some()
    );
    assert!(fixture.receiver.as_mut().expect("receiver").pop().is_none());
    assert_eq!(kithara_worker::mock::wake_state(&wake), (1, false));
    let deferred = kithara_worker::Wake::default();
    let mut nonblocking = PcmFixture::with_wake(
        4,
        false,
        crate::worker::scheduler::StreamWake::new(deferred.clone()),
    )
    .await;
    assert!(
        nonblocking
            .receiver
            .as_mut()
            .expect("receiver")
            .pop()
            .is_none()
    );
    nonblocking
        .receiver
        .as_mut()
        .expect("receiver")
        .recycle(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[1.0]))))
        .expect("return fits");
    assert_eq!(kithara_worker::mock::wake_state(&deferred), (0, true));
}

#[kithara::test(native, tokio)]
async fn every_in_flight_packet_fits_the_reverse_ring_without_recycling() {
    let capacity = 4;
    let mut fixture = PcmFixture::new(capacity, true).await;
    for _ in 0..capacity {
        fixture
            .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[1.0]))))
            .expect("fill forward ring");
    }
    let held = fixture
        .receiver
        .as_mut()
        .expect("receiver")
        .pop()
        .expect("held packet");
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[2.0]))))
        .expect("worker fills freed slot before recycling");
    fixture
        .receiver
        .as_mut()
        .expect("receiver")
        .recycle(held)
        .expect("return held packet");
    for _ in 0..capacity {
        let packet = fixture
            .receiver
            .as_mut()
            .expect("receiver")
            .pop()
            .expect("forward packet");
        fixture
            .receiver
            .as_mut()
            .expect("receiver")
            .recycle(packet)
            .expect("every return fits");
    }
    for _ in 0..=capacity {
        assert!(fixture.returned().is_some());
    }
    assert!(fixture.returned().is_none());
}

#[kithara::test(native, tokio)]
async fn explicit_off_rt_mode_is_immediate_without_blocking_reads() {
    let mut fixture = PcmFixture::new(4, false).await;
    assert!(fixture.receiver.as_ref().expect("receiver").ready.is_none());
    assert!(pop_blocking(fixture.receiver.as_mut().expect("receiver")).is_none());
}

#[kithara::test(native, tokio)]
async fn blocking_recv_returns_closed_after_cancel() {
    let mut fixture = PcmFixture::new(4, true).await;
    fixture.close();
    assert!(pop_blocking(fixture.receiver.as_mut().expect("receiver")).is_none());
}

#[kithara::test(native, tokio)]
async fn a_preloaded_read_still_parks_when_underruns_block() {
    let mut fixture = PcmFixture::new(4, true).await;
    let mut receiver = fixture.receiver.take().expect("receiver");
    let writer = kithara_platform::thread::spawn_named("pcm-writer", move || {
        fixture
            .push(PcmPacket::Chunk(Box::new(chunk(
                SegmentId::FIRST,
                &[0.7, 0.8],
            ))))
            .expect("wake blocked reader");
        fixture
    });
    assert!(pop_blocking(&mut receiver).is_some());
    let fixture = writer.join().expect("producer thread");
    assert!(fixture.producer.is_some());
}

#[kithara::test(native, tokio)]
async fn consumer_phase_failed_on_channel_close() {
    let mut fixture = PcmFixture::new(4, true).await;
    fixture.close();
    assert!(pop_blocking(fixture.receiver.as_mut().expect("receiver")).is_none());
    assert!(
        !fixture
            .receiver
            .as_ref()
            .expect("receiver")
            .forward
            .write_is_held()
    );
}

#[kithara::test(native, tokio)]
async fn preloaded_recv_is_nonblocking() {
    let mut fixture = PcmFixture::new(4, false).await;
    assert!(fixture.receiver.as_mut().expect("receiver").pop().is_none());
}

#[kithara::test(native, tokio)]
async fn audio_config_defaults_to_realtime_deferred_consumer_wakes() {
    let fixture = PcmFixture::new(4, false).await;
    assert!(fixture.receiver.as_ref().expect("receiver").ready.is_none());
}

#[kithara::test(native, tokio)]
async fn wake_signal() {
    let mut fixture = PcmFixture::new(2, true).await;
    let ready = fixture
        .receiver
        .as_ref()
        .expect("receiver")
        .ready
        .as_ref()
        .expect("blocking readiness")
        .clone();
    let since = ready.current();
    assert_eq!(ready.current(), since);
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[42.0]))))
        .expect("ring push");
    assert_ne!(ready.current(), since);
}
#[kithara::test(native, tokio)]
async fn wake_skipped_when_parking_in_overflow() {
    let mut fixture = PcmFixture::new(1, true).await;
    let ready = fixture
        .receiver
        .as_ref()
        .expect("receiver")
        .ready
        .as_ref()
        .expect("blocking readiness")
        .clone();
    let since = ready.current();
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[1.0]))))
        .expect("ring push");
    assert_eq!(ready.current(), since + 1);
    let packet = fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[2.0]))))
        .expect_err("full ring retains packet at owner");
    assert_eq!(ready.current(), since + 1);
    assert!(
        matches!(fixture.receiver.as_mut().expect("receiver").pop(), Some(PcmPacket::Chunk(chunk)) if chunk.samples[0] == 1.0)
    );
    fixture.push(packet).expect("admit retained packet");
    assert_eq!(ready.current(), since + 2);
}
#[kithara::test(native, tokio)]
async fn wake_fires_every_ring_push() {
    let mut fixture = PcmFixture::new(2, true).await;
    let ready = fixture
        .receiver
        .as_ref()
        .expect("receiver")
        .ready
        .as_ref()
        .expect("blocking readiness")
        .clone();
    let since = ready.current();
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[1.0]))))
        .expect("first push");
    assert_eq!(ready.current(), since + 1);
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[2.0]))))
        .expect("second push");
    assert_eq!(ready.current(), since + 2);
    assert!(
        matches!(fixture.receiver.as_mut().expect("receiver").pop(), Some(PcmPacket::Chunk(chunk)) if chunk.samples[0] == 1.0)
    );
    assert!(
        matches!(fixture.receiver.as_mut().expect("receiver").pop(), Some(PcmPacket::Chunk(chunk)) if chunk.samples[0] == 2.0)
    );
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[3.0]))))
        .expect("third push");
    assert_eq!(ready.current(), since + 3);
}

#[kithara::test(native, tokio)]
async fn output_wake_is_deferred_until_the_scheduler_shell_flushes() {
    let mut fixture = PcmFixture::new(2, true).await;
    let ready = fixture
        .receiver
        .as_ref()
        .expect("receiver")
        .ready
        .as_ref()
        .expect("readiness")
        .clone();
    let since = ready.current();
    assert_eq!(ready.current(), since);
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[0.1]))))
        .expect("owner-thread admission");
    let delivered = ready.current();
    assert_ne!(delivered, since);
    assert_eq!(ready.current(), delivered);
}
#[kithara::test(native, tokio)]
async fn reader_output_wake_is_deferred_and_coalesced() {
    let mut fixture = PcmFixture::new(2, true).await;
    let ready = fixture
        .receiver
        .as_ref()
        .expect("receiver")
        .ready
        .as_ref()
        .expect("readiness")
        .clone();
    let since = ready.current();
    assert_eq!(ready.current(), since);
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[0.1]))))
        .expect("first owner admission");
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[0.2]))))
        .expect("second owner admission");
    assert_eq!(ready.current(), since + 2);
    assert_eq!(ready.current(), since + 2);
}
#[kithara::test(native, tokio)]
async fn output_available_event_is_coalesced_per_producer_pass() {
    let mut fixture = PcmFixture::new(2, true).await;
    let ready = fixture
        .receiver
        .as_ref()
        .expect("receiver")
        .ready
        .as_ref()
        .expect("readiness")
        .clone();
    let since = ready.current();
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[0.1]))))
        .expect("first push");
    assert_eq!(ready.current(), since + 1);
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[0.2]))))
        .expect("second push");
    assert_eq!(ready.current(), since + 2);
    assert_eq!(ready.current(), since + 2);
    assert!(fixture.receiver.as_mut().expect("receiver").pop().is_some());
    assert!(fixture.receiver.as_mut().expect("receiver").pop().is_some());
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[0.3]))))
        .expect("third push");
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[0.4]))))
        .expect("fourth push");
    assert_eq!(ready.current(), since + 4);
    assert_eq!(ready.current(), since + 4);
}
