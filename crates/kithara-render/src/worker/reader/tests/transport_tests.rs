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

fn discharge_off_rt(wake: &kithara_worker::Wake) {
    wake.wake();
    assert!(kithara_worker::mock::flush_deferred(wake));
}

fn pop_off_rt(fixture: &mut PcmFixture, wake: &kithara_worker::Wake) -> Option<PcmPacket> {
    let packet = fixture.receiver.as_mut().expect("receiver").pop();
    discharge_off_rt(wake);
    packet
}

#[kithara::test(tokio)]
#[case::nonblocking(false, false)]
#[case::offline(true, false)]
#[case::eof_nonblocking(false, true)]
#[case::eof_offline(true, true)]
async fn callback_consumption_defers_every_wake_even_when_offline_blocking_is_enabled(
    #[case] blocking: bool,
    #[case] end_of_track: bool,
) {
    let wake = kithara_worker::Wake::default();
    let mut fixture = PcmFixture::with_wake(
        4,
        blocking,
        crate::worker::scheduler::StreamWake::new(wake.clone()),
    )
    .await;
    assert!(
        fixture
            .receiver
            .as_ref()
            .expect("receiver")
            .peek()
            .is_none()
    );
    assert_eq!(kithara_worker::mock::wake_state(&wake), (0, true));
    assert!(kithara_worker::mock::flush_deferred(&wake));
    let mut output = chunk(SegmentId::FIRST, if end_of_track { &[] } else { &[1.0] });
    output.meta.end_of_track = end_of_track;
    fixture
        .push(PcmPacket::Chunk(Box::new(output)))
        .expect("forward packet fits");
    let packet = fixture
        .receiver
        .as_mut()
        .expect("receiver")
        .pop()
        .expect("forward packet");
    assert!(matches!(&packet, PcmPacket::Chunk(chunk) if chunk.meta.end_of_track == end_of_track));
    assert_eq!(kithara_worker::mock::wake_state(&wake), (0, true));
    assert!(kithara_worker::mock::flush_deferred(&wake));
    fixture
        .receiver
        .as_mut()
        .expect("receiver")
        .recycle(packet)
        .expect("reverse packet fits");
    assert_eq!(kithara_worker::mock::wake_state(&wake), (0, true));
    assert!(kithara_worker::mock::flush_deferred(&wake));
    assert!(fixture.receiver.as_mut().expect("receiver").pop().is_none());
    assert_eq!(kithara_worker::mock::wake_state(&wake), (0, true));
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::flash(io)]
#[kithara::hang_watchdog]
pub(super) fn receive_notice(receiver: &std::sync::mpsc::Receiver<()>) {
    hang_tick!();
    let mut notice = None;
    hang_park!(|remaining| {
        notice = Some(receiver.recv_timeout(remaining));
    });
    notice
        .expect("bounded observer wait")
        .expect("scheduler notice");
    hang_reset!();
}

#[kithara::test(tokio)]
#[case::nonblocking(false)]
#[case::offline(true)]
async fn playback_publication_is_deferred_even_for_an_offline_receiver(#[case] blocking: bool) {
    let wake = kithara_worker::Wake::default();
    let mut fixture = PcmFixture::with_wake(
        4,
        blocking,
        crate::worker::scheduler::StreamWake::new(wake.clone()),
    )
    .await;
    let receiver = fixture.receiver.as_mut().expect("receiver");
    receiver.set_playing(true);
    assert_eq!(kithara_worker::mock::wake_state(&wake), (0, true));
    assert!(
        *fixture.producer.as_mut().expect("producer").playing.read(),
        "the worker observes playback on its next scheduled pass"
    );
    fixture
        .receiver
        .as_mut()
        .expect("receiver")
        .set_playing(false);
    assert_eq!(kithara_worker::mock::wake_state(&wake), (0, true));
    assert!(!*fixture.producer.as_mut().expect("producer").playing.read());
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
async fn an_empty_poll_schedules_exactly_one_producer_pass() {
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
        #[kithara::hang_watchdog]
        fn on_event(&mut self, event: Event) {
            if matches!(event, Event::Backpressured(_)) {
                self.parked.send(()).expect("report producer backpressure");
                receive_notice(&self.resume);
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
    receive_notice(&observed);
    dispatcher.wake_handle().wake();
    resume.send(()).expect("consume the registration wake");
    receive_notice(&observed);
    let before = ticks.load(Ordering::Relaxed);
    let receiver = fixture.receiver.as_mut().expect("receiver");
    assert!(receiver.pop().is_none());
    assert!(receiver.ready.is_none());
    resume.send(()).expect("park after empty poll");
    receive_notice(&observed);
    assert_eq!(
        ticks.load(Ordering::Relaxed),
        before + 1,
        "an empty poll schedules exactly one producer pass"
    );
    dispatcher.shutdown();
    resume.send(()).expect("release observer for shutdown");
}

#[kithara::test(tokio)]
async fn a_nonblocking_poll_of_an_empty_ring_states_its_demand() {
    let wake = kithara_worker::Wake::default();
    let mut fixture = PcmFixture::with_wake(
        4,
        false,
        crate::worker::scheduler::StreamWake::new(wake.clone()),
    )
    .await;
    assert!(fixture.receiver.as_mut().expect("receiver").pop().is_none());
    assert!(fixture.receiver.as_ref().expect("receiver").ready.is_none());
    assert_eq!(
        kithara_worker::mock::wake_state(&wake),
        (0, true),
        "an empty poll states producer demand"
    );
}

#[kithara::test(tokio)]
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
    assert!(pop_off_rt(&mut fixture, &wake).is_none());
    assert_eq!(kithara_worker::mock::wake_state(&wake), (1, false));
    fixture
        .receiver
        .as_mut()
        .expect("receiver")
        .recycle(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[1.0]))))
        .expect("return fits");
    assert_eq!(kithara_worker::mock::wake_state(&wake), (1, true));
    wake.wake();
    assert_eq!(kithara_worker::mock::wake_state(&wake), (2, true));
    assert!(kithara_worker::mock::flush_deferred(&wake));
    assert_eq!(kithara_worker::mock::wake_state(&wake), (2, false));
}

#[kithara::test(native, tokio)]
#[case::nonblocking(false)]
#[case::offline(true)]
async fn recycling_pcm_never_wakes_the_producer_inside_the_render_pass(#[case] blocking: bool) {
    #[kithara::rtsan_forbid_blocking]
    fn recycle_in_pass(
        receiver: &mut PcmReceiver,
        wake: &kithara_worker::Wake,
        packets: [PcmPacket; 2],
    ) {
        for packet in packets {
            receiver.recycle(packet).expect("return fits");
            assert_eq!(kithara_worker::mock::wake_state(wake), (0, true));
        }
    }

    let wake = kithara_worker::Wake::default();
    let mut fixture = PcmFixture::with_wake(
        4,
        blocking,
        crate::worker::scheduler::StreamWake::new(wake.clone()),
    )
    .await;
    let receiver = fixture.receiver.as_mut().expect("receiver");
    assert_eq!(receiver.ready.is_some(), blocking);
    assert_eq!(kithara_worker::mock::wake_state(&wake), (0, false));
    let packets = [
        PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[1.0]))),
        PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[2.0]))),
    ];
    recycle_in_pass(receiver, &wake, packets);
    assert_eq!(receiver.reverse.occupied_len(), 2);
    assert_eq!(kithara_worker::mock::wake_state(&wake), (0, true));
    wake.wake();
    assert_eq!(kithara_worker::mock::wake_state(&wake), (1, true));
}

#[kithara::test(tokio)]
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
    assert!(pop_off_rt(&mut fixture, &wake).is_none());
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

#[kithara::test(tokio)]
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

#[kithara::test(tokio)]
async fn explicit_off_rt_mode_is_immediate_without_blocking_reads() {
    let wake = kithara_worker::Wake::default();
    let mut fixture = PcmFixture::with_wake(
        4,
        false,
        crate::worker::scheduler::StreamWake::new(wake.clone()),
    )
    .await;
    assert!(fixture.receiver.as_ref().expect("receiver").ready.is_none());
    assert!(pop_off_rt(&mut fixture, &wake).is_none());
    assert_eq!(kithara_worker::mock::wake_state(&wake), (1, false));
}

#[kithara::test(tokio)]
async fn blocking_recv_returns_closed_after_cancel() {
    let mut fixture = PcmFixture::new(4, true).await;
    fixture.close();
    assert!(pop_blocking(fixture.receiver.as_mut().expect("receiver")).is_none());
}

#[kithara::test(native, tokio)]
async fn a_preloaded_read_still_parks_when_underruns_block() {
    let mut fixture = PcmFixture::new(4, true).await;
    kithara_platform::tokio::task::spawn_sync(move || {
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
    })
    .await
    .expect("off-RT reader phase");
}

#[kithara::test(tokio)]
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

#[kithara::test(tokio)]
async fn preloaded_recv_is_nonblocking() {
    let mut fixture = PcmFixture::new(4, false).await;
    assert!(fixture.receiver.as_mut().expect("receiver").pop().is_none());
}

#[kithara::test(tokio)]
async fn audio_config_defaults_to_realtime_deferred_consumer_wakes() {
    let fixture = PcmFixture::new(4, false).await;
    assert!(fixture.receiver.as_ref().expect("receiver").ready.is_none());
}

#[kithara::test(tokio)]
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
#[kithara::test(tokio)]
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
#[kithara::test(tokio)]
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

#[kithara::test(tokio)]
async fn output_wake_is_deferred_until_the_scheduler_shell_flushes() {
    let wake = kithara_worker::Wake::default();
    let mut fixture = PcmFixture::with_wake(
        2,
        false,
        crate::worker::scheduler::StreamWake::new(wake.clone()),
    )
    .await;
    assert_eq!(kithara_worker::mock::wake_state(&wake), (0, false));
    assert!(fixture.receiver.as_mut().expect("receiver").pop().is_none());
    assert_eq!(kithara_worker::mock::wake_state(&wake), (0, true));
    assert!(kithara_worker::mock::flush_deferred(&wake));
    assert_eq!(kithara_worker::mock::wake_state(&wake), (0, false));
    assert!(!kithara_worker::mock::flush_deferred(&wake));
    assert_eq!(kithara_worker::mock::wake_state(&wake), (0, false));
}
#[kithara::test(tokio)]
async fn reader_output_wake_is_deferred_and_coalesced() {
    let wake = kithara_worker::Wake::default();
    let mut fixture = PcmFixture::with_wake(
        2,
        false,
        crate::worker::scheduler::StreamWake::new(wake.clone()),
    )
    .await;
    assert!(fixture.receiver.as_mut().expect("receiver").pop().is_none());
    assert!(fixture.receiver.as_mut().expect("receiver").pop().is_none());
    assert_eq!(
        kithara_worker::mock::wake_state(&wake),
        (0, true),
        "reader requests do not signal the gate immediately"
    );
    assert!(kithara_worker::mock::flush_deferred(&wake));
    assert_eq!(kithara_worker::mock::wake_state(&wake), (0, false));
    assert!(
        !kithara_worker::mock::flush_deferred(&wake),
        "a second flush delivers no extra pass"
    );
}
#[kithara::test(tokio)]
#[case::offline(true)]
#[case::realtime_nonblocking(false)]
async fn output_available_event_is_coalesced_per_producer_pass(#[case] blocking: bool) {
    let _real_io = (!blocking).then(kithara_platform::flash::real_io);
    let mut fixture = PcmFixture::new(2, blocking).await;
    let mut events = fixture.events::<kithara_audio::AudioEvent>();
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[0.1]))))
        .expect("first push");
    assert!(events.try_recv().is_err());
    fixture.flush();
    assert!(matches!(
        events.try_recv().map(|envelope| envelope.event),
        Ok(kithara_audio::AudioEvent::OutputAvailable)
    ));
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[0.2]))))
        .expect("second push");
    assert!(events.try_recv().is_err());
    fixture.flush();
    assert!(matches!(
        events.try_recv().map(|envelope| envelope.event),
        Ok(kithara_audio::AudioEvent::OutputAvailable)
    ));
    fixture.flush();
    assert!(events.try_recv().is_err());
    let first = fixture.receiver.as_mut().expect("receiver").pop();
    let second = fixture.receiver.as_mut().expect("receiver").pop();
    assert!(first.is_some());
    assert!(second.is_some());
    let underruns = usize::from(first.is_none()) + usize::from(second.is_none());
    assert_eq!(
        underruns, 0,
        "available PCM never underruns a nonblocking consumer"
    );
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[0.3]))))
        .expect("third push");
    fixture
        .push(PcmPacket::Chunk(Box::new(chunk(SegmentId::FIRST, &[0.4]))))
        .expect("fourth push");
    fixture.flush();
    assert!(matches!(
        events.try_recv().map(|envelope| envelope.event),
        Ok(kithara_audio::AudioEvent::OutputAvailable)
    ));
    assert!(events.try_recv().is_err(), "one event per producer pass");
}
