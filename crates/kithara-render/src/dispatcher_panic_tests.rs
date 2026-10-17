use std::num::NonZeroU32;

use kithara_audio::{AudioReadError, AudioSource, Fetch, SeekOutcome, TrackStep};
use kithara_command::{Batch, ChannelConfig, Outcome, Rejection, Sender, When, channel};
use kithara_platform::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use kithara_signal::{AudioChunk, AudioSpec, SegmentId, SessionFrame, SourceSpan};
use kithara_test_utils::kithara;
use ringbuf::{
    HeapRb,
    traits::{Consumer, Split},
};

use super::*;
use crate::{
    PcmPacket, PcmReceiver,
    bridge::{DeckEvent, Fade, PlaybackFault, RtMetrics, Slot},
    rt::track::{PcmConsumer, PlayerResource, PlayerTrack, RtSink},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PanicAt {
    Tick,
    Recycle,
    WarmUp,
    Open,
}

#[derive(Debug)]
struct Source {
    panic_at: Option<PanicAt>,
    armed: Arc<AtomicBool>,
    dropped: Arc<AtomicUsize>,
    panic_on_drop: bool,
    frame: u64,
}

impl AudioSource for Source {
    type Chunk = AudioChunk;

    fn seek(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
        Ok(SeekOutcome::Landed {
            target,
            landed_at: target,
        })
    }

    fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}

    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        None
    }

    fn warm_up(&mut self) {
        assert!(
            !(self.panic_at == Some(PanicAt::WarmUp) && self.armed.load(Ordering::Acquire)),
            "test lane warm-up panic"
        );
    }

    fn prepare_deferred(&mut self) -> Option<AudioSpec> {
        assert!(
            !(self.panic_at == Some(PanicAt::Recycle) && self.armed.load(Ordering::Acquire)),
            "test lane recycle panic"
        );
        None
    }

    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        assert!(
            !(self.panic_at == Some(PanicAt::Tick) && self.armed.load(Ordering::Acquire)),
            "test lane tick panic"
        );
        let mut data = crate::mock::pcm_fixture::chunk(SegmentId::FIRST, &[0.25; 4096]);
        let end = self.frame + u64::from(data.meta.frames);
        data.meta.frame_offset = self.frame;
        data.meta.timestamp = data
            .spec()
            .duration_for(self.frame)
            .expect("source timestamp");
        data.meta.end_timestamp = data.spec().duration_for(end).expect("source end");
        data.meta.source_span = SourceSpan::new(
            self.frame,
            end,
            data.spec().sample_rate,
            u64::from(data.meta.frames),
        );
        self.frame = end;
        TrackStep::Produced(Fetch::data(data))
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::AcqRel);
        assert!(!self.panic_on_drop, "test lane cleanup panic");
    }
}

impl Open for Source {
    type Opened = PcmReceiver;
    type Lane = crate::DecoderNode<Self, crate::test_pools::TestPools>;

    async fn open(
        self,
        _position: Duration,
        _start: LaneStart,
        _inbox: Inbox<LaneProtocol>,
    ) -> OpenResult<Self::Opened, Self::Lane> {
        assert!(
            !(self.panic_at == Some(PanicAt::Open) && self.armed.load(Ordering::Acquire)),
            "test source open panic"
        );
        let spec = AudioSpec::new(1, NonZeroU32::new(48_000).expect("rate"));
        let (lane, pcm, _commands) = crate::worker::terminal_node(self, spec, false);
        Ok((pcm, lane, FrameCount::new(0)))
    }
}

type Loads = DispatcherProtocol<Source>;

fn load(sender: &mut Sender<Loads>, source: Source) -> Seq {
    sender
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![DispatcherCommand::Load(Box::new(LoadRequest {
                    item: source,
                    class: ServiceClass::Warm,
                    position: Duration::ZERO,
                    start: LaneStart {
                        speed: SpeedCurve::Constant(1.0),
                        keylock: false,
                        backend: StretchKind::default(),
                    },
                    inbox: channel(ChannelConfig::builder().build()).1,
                }))],
            },
        )
        .expect("load channel room")
}

fn consume_pcm(receiver: &mut PcmReceiver) -> (u64, u64) {
    let packet = receiver.pop().expect("sibling produces actual PCM");
    let PcmPacket::Chunk(chunk) = &packet else {
        panic!("sibling PCM, not a failure");
    };
    assert!(chunk.frames() > 0);
    assert!(chunk.samples.iter().all(|sample| *sample == 0.25));
    let span = chunk.meta.source_span.expect("actual PCM source mapping");
    let range = (span.start(), span.end());
    assert!(range.1 > range.0);
    receiver.recycle(packet).expect("return consumed PCM");
    range
}

fn assert_failed(receiver: PcmReceiver) {
    assert!(receiver.is_closed(), "failed producer closes its PCM ring");
    let spec = receiver.spec();
    let resource = PlayerResource::new(
        PcmConsumer::new(receiver),
        Arc::from("panic.wav"),
        &crate::test_pools::pools(),
    )
    .expect("failed lane resource");
    let mut track = PlayerTrack::builder()
        .item_id(kithara_events::TrackId::allocate())
        .sample_rate(spec.sample_rate)
        .build(Box::new(resource));
    track.start(Fade::Declick);
    let (mut events, mut received) = HeapRb::<DeckEvent>::new(4).split();
    let metrics = RtMetrics::default();
    let slot = Slot::new(0);
    let at = SessionFrame::new(17);
    let mut sink = RtSink::new(&mut events, &metrics, slot, at);
    assert!(track.poll_end(0, &mut 8, &mut sink));
    assert_eq!(
        received.try_pop(),
        Some(DeckEvent::Failed {
            slot,
            at,
            fault: PlaybackFault::Source(kithara_audio::TrackFailureKind::ChannelClosed),
        })
    );
    assert!(!track.poll_end(0, &mut 8, &mut sink));
    assert!(received.try_pop().is_none());
}

fn assert_terminal_commands(
    dispatcher: &mut DispatcherTask<Source>,
    sender: &mut Sender<Loads>,
    dead_lane: LaneId,
    sibling_id: LaneId,
    sibling_pcm: &mut PcmReceiver,
    after_end: u64,
) {
    let priority = sender
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![DispatcherCommand::SetPriority(
                    dead_lane,
                    ServiceClass::Audible,
                )],
            },
        )
        .expect("priority command for the ended lane");
    let release = sender
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands: vec![DispatcherCommand::Release(dead_lane)],
            },
        )
        .expect("release command for the ended lane");
    assert_ne!(dispatcher.tick(), TickResult::Done);
    let answered: Vec<_> = sender.receipts().collect();
    assert_eq!(answered.len(), 2, "both commands for the ended lane settle");
    assert!(answered.iter().any(|receipt| receipt.seq() == priority
        && matches!(
            receipt.outcome(),
            Outcome::Rejected(Rejection::Refused(LoadRefusal::Cancelled))
        )));
    assert!(answered.iter().any(|receipt| receipt.seq() == release
        && matches!(
            receipt.outcome(),
            Outcome::Applied {
                data: Dispatched::Released,
                ..
            }
        )));
    assert_eq!(dispatcher.dispatcher.lanes.len(), 1);
    assert_eq!(dispatcher.dispatcher.lanes[0].id, sibling_id);
    assert_eq!(dispatcher.priority(), Some(ServiceClass::Warm.into()));
    let command_pcm = consume_pcm(sibling_pcm);
    assert!(
        command_pcm.0 >= after_end,
        "terminal commands preserve fresh sibling PCM"
    );
    assert_ne!(dispatcher.tick(), TickResult::Done);
    assert!(
        sender.receipts().next().is_none(),
        "terminal commands settle once"
    );
    let next_pcm = consume_pcm(sibling_pcm);
    assert!(next_pcm.0 >= command_pcm.1);
}

#[kithara::test(tokio)]
#[case::tick_during_preload(PanicAt::Tick, false, false)]
#[case::recycle_during_preload(PanicAt::Recycle, false, false)]
#[case::tick_after_preload(PanicAt::Tick, true, false)]
#[case::recycle_after_preload(PanicAt::Recycle, true, false)]
#[case::tick_and_cleanup(PanicAt::Tick, true, true)]
#[case::warm_up_during_preload(PanicAt::WarmUp, false, false)]
#[case::open_during_preload(PanicAt::Open, false, false)]
async fn a_panicking_lane_preserves_its_siblings_pcm(
    #[case] panic_at: PanicAt,
    #[case] after_preload: bool,
    #[case] panic_on_drop: bool,
) {
    let (mut sender, inbox) = channel::<Loads>(ChannelConfig::builder().build());
    let mut dispatcher = DispatcherTask::new(
        inbox,
        NonZeroUsize::new(2).expect("two lanes"),
        kithara_worker::Wake::default(),
        Some(Handle::current()),
    );
    let armed = Arc::new(AtomicBool::new(!after_preload));
    let failed_drops = Arc::new(AtomicUsize::new(0));
    let sibling_drops = Arc::new(AtomicUsize::new(0));
    let failed = load(
        &mut sender,
        Source {
            panic_at: Some(panic_at),
            armed: armed.clone(),
            dropped: failed_drops.clone(),
            panic_on_drop,
            frame: 0,
        },
    );
    let sibling = load(
        &mut sender,
        Source {
            panic_at: None,
            armed: Arc::new(AtomicBool::new(false)),
            dropped: sibling_drops.clone(),
            panic_on_drop: false,
            frame: 0,
        },
    );
    let mut answered = Vec::new();
    for _ in 0..8 {
        assert_ne!(dispatcher.tick(), TickResult::Done);
        answered.extend(sender.receipts());
        if answered.len() == 2 {
            break;
        }
    }
    assert_eq!(answered.len(), 2, "both loads settle despite a lane panic");
    let mut failed_pcm = None;
    let mut sibling_pcm = None;
    for receipt in answered {
        let seq = receipt.seq();
        let (outcome, _): (Outcome<Loads>, Batch<Loads>) = receipt.into();
        match (seq, outcome) {
            (
                seq,
                Outcome::Applied {
                    data: Dispatched::Loaded(loaded),
                    ..
                },
            ) if seq == sibling => {
                sibling_pcm = Some((loaded.lane, loaded.opened));
            }
            (
                seq,
                Outcome::Applied {
                    data: Dispatched::Loaded(loaded),
                    ..
                },
            ) if seq == failed && after_preload => {
                failed_pcm = Some((loaded.lane, loaded.opened));
            }
            (
                seq,
                Outcome::Rejected(Rejection::Refused(LoadRefusal::Source(
                    kithara_audio::TrackFailureKind::Render,
                ))),
            ) if seq == failed && !after_preload => {}
            (_, outcome) => {
                panic!("panic must preserve the load result and its source failure: {outcome:?}")
            }
        }
    }
    let (sibling_id, mut sibling_pcm) = sibling_pcm.expect("healthy lane loads");
    let before = consume_pcm(&mut sibling_pcm);
    if after_preload {
        consume_pcm(
            &mut failed_pcm
                .as_mut()
                .expect("the failing lane was preloaded")
                .1,
        );
        armed.store(true, Ordering::Release);
    }
    assert_ne!(dispatcher.tick(), TickResult::Done);
    assert_eq!(
        dispatcher.dispatcher.lanes.len(),
        1,
        "only the failed lane is removed"
    );
    assert_eq!(dispatcher.dispatcher.lanes[0].id, sibling_id);
    assert_eq!(dispatcher.priority(), Some(ServiceClass::Warm.into()));
    assert_eq!(
        failed_drops.load(Ordering::Acquire),
        1,
        "failed source is cleaned once"
    );
    assert_eq!(
        sibling_drops.load(Ordering::Acquire),
        0,
        "sibling source stays alive"
    );
    let failed_lane = failed_pcm.map(|(lane, receiver)| {
        assert_failed(receiver);
        lane
    });
    let after = consume_pcm(&mut sibling_pcm);
    assert!(
        after.0 >= before.1,
        "sibling renders fresh source frames after the panic"
    );
    if let Some(lane) = failed_lane {
        assert_terminal_commands(
            &mut dispatcher,
            &mut sender,
            lane,
            sibling_id,
            &mut sibling_pcm,
            after.1,
        );
    }
    dispatcher.on_cancel();
    assert_eq!(dispatcher.priority(), None);
    assert!(sibling_pcm.is_closed());
    assert_eq!(sibling_drops.load(Ordering::Acquire), 1);
    drop(dispatcher);
    assert_eq!(failed_drops.load(Ordering::Acquire), 1);
    assert_eq!(sibling_drops.load(Ordering::Acquire), 1);
}
