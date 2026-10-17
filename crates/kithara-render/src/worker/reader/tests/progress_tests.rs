use kithara_platform::time::{Duration, WallInstant};
use kithara_test_utils::kithara;
use kithara_worker::{
    DispatcherConfig, Event, Observer, Task, TaskConfig, TickResult, Wake, Worker, WorkerConfig,
};
use ringbuf::traits::{Consumer, Producer};

use super::transport_tests::receive_notice;
use crate::{
    mock::pcm_fixture::{PcmFixture, chunk},
    worker::{PcmPacket, reader::PcmProducer, scheduler::StreamWake},
};

struct ProducerTask {
    producer: PcmProducer,
    started: bool,
    next: usize,
    first_output: std::sync::mpsc::Sender<()>,
}

impl Task for ProducerTask {
    fn tick(&mut self) -> TickResult {
        if !std::mem::replace(&mut self.started, true) {
            return TickResult::Backpressured;
        }
        if let Some(packet) = self.producer.reverse.try_pop() {
            drop(packet);
        }
        if self.next == 32 {
            return TickResult::Backpressured;
        }
        let samples = vec![f32::from(u16::try_from(self.next).expect("packet index")); 480];
        if self
            .producer
            .forward
            .try_push(PcmPacket::Chunk(Box::new(chunk(
                kithara_signal::SegmentId::FIRST,
                &samples,
            ))))
            .is_err()
        {
            return TickResult::Backpressured;
        }
        self.producer.signal();
        self.producer.flush();
        self.next += 1;
        if self.next == 1 {
            self.first_output.send(()).expect("first output notice");
        }
        TickResult::Progress
    }
}

struct DemandObserver {
    wake: std::sync::Arc<std::sync::OnceLock<Wake>>,
    parked: Option<std::sync::mpsc::Sender<()>>,
}

impl Observer for DemandObserver {
    fn on_event(&mut self, event: Event) {
        if matches!(event, Event::Backpressured(_))
            && let Some(parked) = self.parked.take()
        {
            let wake = self.wake.get().expect("dispatcher wake installed");
            let baseline = kithara_worker::mock::wake_state(wake).0;
            parked.send(()).expect("producer is empty and parked");
            wait_for_demand(wake, baseline);
        }
    }
}

#[kithara::flash(io)]
#[kithara::hang_watchdog]
fn wait_for_demand(wake: &Wake, baseline: u64) {
    loop {
        hang_tick!();
        let (signals, pending) = kithara_worker::mock::wake_state(wake);
        if signals > baseline || pending {
            hang_reset!();
            return;
        }
        hang_park!(|remaining: Duration| {
            std::thread::sleep(remaining.min(Duration::from_micros(100)));
        });
    }
}

#[kithara::test(native, tokio)]
#[kithara::hang_watchdog]
#[case::blocking_offline(true)]
#[case::paced_nonblocking(false)]
async fn a_live_producer_serves_consumed_packets_without_underruns(#[case] blocking: bool) {
    let wake_slot = std::sync::Arc::new(std::sync::OnceLock::new());
    let (parked, empty) = std::sync::mpsc::channel();
    let (produced, output) = std::sync::mpsc::channel();
    let worker = Worker::new(WorkerConfig::new());
    let dispatcher = worker.dispatcher(
        DispatcherConfig::builder()
            .name("consumer-progress")
            .observer(DemandObserver {
                wake: std::sync::Arc::clone(&wake_slot),
                parked: Some(parked),
            })
            .build(),
    );
    let wake = dispatcher.wake_handle();
    assert!(wake_slot.set(wake.clone()).is_ok());
    let mut fixture = PcmFixture::with_wake(4, blocking, StreamWake::new(wake.clone())).await;
    let producer = fixture.producer.take().expect("producer owner");
    let _task = dispatcher
        .register(TaskConfig::new(), move |_| ProducerTask {
            producer,
            started: false,
            next: 0,
            first_output: produced,
        })
        .expect("register live producer");
    receive_notice(&empty);
    let baseline = kithara_worker::mock::wake_state(&wake).0;
    let mut receiver = fixture.receiver.take().expect("receiver");
    if blocking {
        receiver = kithara_platform::tokio::task::spawn_sync(move || {
            receiver.wait_for_packet();
            receiver
        })
        .await
        .expect("off-RT packet wait");
    } else {
        assert!(receiver.pop().is_none(), "the first poll states demand");
    }
    receive_notice(&output);
    assert_eq!(
        kithara_worker::mock::wake_state(&wake).0,
        baseline + u64::from(blocking),
        "only the explicit off-RT wait signals the producer immediately"
    );
    let mut underruns = 0;
    let started = WallInstant::now();
    for expected in 0..32 {
        hang_tick!();
        if blocking {
            receiver = kithara_platform::tokio::task::spawn_sync(move || {
                receiver.wait_for_packet();
                receiver
            })
            .await
            .expect("off-RT packet wait");
        }
        match receiver.pop() {
            Some(PcmPacket::Chunk(packet)) => {
                assert_eq!(packet.frames(), 480);
                assert!(packet.samples.iter().all(|sample| {
                    *sample == f32::from(u16::try_from(expected).expect("packet index"))
                }));
                receiver
                    .recycle(PcmPacket::Chunk(packet))
                    .expect("packet returns to producer");
                hang_reset!();
            }
            _ => underruns += 1,
        }
        let _real_io = kithara_platform::flash::real_io();
        kithara_platform::time::sleep(Duration::from_millis(10)).await;
        hang_tick!();
    }
    assert!(started.elapsed() >= Duration::from_millis(320));
    assert_eq!(underruns, 0, "paced callbacks never wait for PCM");
    fixture.receiver = Some(receiver);
    dispatcher.shutdown();
}
