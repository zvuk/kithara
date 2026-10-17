use kithara_render::bridge::SlotState;
use num_traits::ToPrimitive;

use super::*;
use crate::{AdvanceReason, Transition};

const THRESHOLD_ITEM: TrackId = TrackId(1);
const THRESHOLD_NEXT: TrackId = TrackId(2);
const THRESHOLD_BLOCK: usize = 512;

struct ThresholdRig {
    queue: Queue<TestPools>,
    mixer: Option<mock::MixerRig>,
    fixture_dispatcher: kithara_command::Sender<
        kithara_render::DispatcherProtocol<kithara_play::ResourceLoad<TestPools>>,
    >,
    _fixture_driver: kithara_worker::TaskHandle,
    loads: mock::DeckRig<TestPools>,
    source_packets: Option<kithara_render::mock::PacketRing>,
    _dir: TestTempDir,
    source: TrackSource<TestPools>,
    frame: i64,
    prefetches: usize,
    handovers: usize,
    ends: usize,
    order: Vec<&'static str>,
    successor_seq: Option<Seq>,
    target_bound: Option<Option<kithara_play::Bound>>,
    monitoring: bool,
}

impl ThresholdRig {
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn playing(duration: f64, fade: f32, lead: f32, frames: Option<usize>) -> Self {
        use kithara_render::{Dispatched, DispatcherCommand, PcmPacket};
        use kithara_signal::AudioChunk;

        let dir = TestTempDir::new();
        let path = dir.path().join("test.mp3");
        let declared_frames = (duration * f64::from(mock::SAMPLE_RATE.get()))
            .to_usize()
            .expect("fixture declared frame count fits usize");
        let frames = frames.unwrap_or(declared_frames);
        mock::write_pcm_wav(
            &path,
            &vec![0.5; frames * 2],
            AudioSpec::new(2, mock::SAMPLE_RATE),
        )
        .expect("finite constant source");
        let source = TrackSource::Uri(path.to_str().expect("fixture source").to_owned());
        let config = DeckMixerConfig::builder()
            .sample_rate(mock::SAMPLE_RATE)
            .build();
        let shape = kithara_render::rt::StreamShape::new(
            NonZeroU32::new(1024).expect("maximum block"),
            mock::SAMPLE_RATE,
        );
        let prep = ResourcePrep::builder()
            .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
            .block_on_underrun(true)
            .build();
        let (fixture_dispatcher, fixture_inbox) =
            kithara_command::channel(kithara_command::ChannelConfig::builder().build());
        let fixture_driver = prep
            .worker
            .start_dispatcher(fixture_inbox)
            .expect("the real worker owns the threshold source dispatcher");
        let settings = crate::QueueSettings::builder()
            .crossfade(crate::CrossfadeSettings {
                duration: fade,
                ..crate::CrossfadeSettings::default()
            })
            .build();
        let mut rig = Self {
            queue: Queue::new(
                QueueConfig::builder()
                    .prep(prep)
                    .store(
                        AssetStore::builder(pools())
                            .backend(StorageBackend::Memory)
                            .build(),
                    )
                    .mixer(config)
                    .settings(settings)
                    .preload_lead(Duration::from_secs_f32(lead))
                    .action_at_item_end(ActionAtItemEnd::None)
                    .build(),
            ),
            mixer: Some(
                mock::MixerRig::new(config, shape, &pools()).expect("real threshold mixer"),
            ),
            fixture_dispatcher,
            _fixture_driver: fixture_driver,
            loads: mock::DeckRig::new(config).expect("threshold dispatcher"),
            source_packets: None,
            _dir: dir,
            source,
            frame: 0,
            prefetches: 0,
            handovers: 0,
            ends: 0,
            order: Vec::new(),
            successor_seq: None,
            target_bound: None,
            monitoring: false,
        };
        rig.apply(QueueCommand::Append {
            position: kithara_play::Position::ZERO,
            id: THRESHOLD_ITEM,
            source: rig.source.clone(),
        });
        rig.apply(QueueCommand::Select {
            id: THRESHOLD_ITEM,
            transition: Transition::None,
        });
        rig.apply(QueueCommand::Pause { at: When::Next });
        rig.loads.opens.drain();
        let (request, seq) = {
            let mut due = rig.loads.opens.next_due((), 1).expect("initial load");
            let DispatcherCommand::Load(request) = due.commands_mut().pop().expect("load request")
            else {
                panic!("initial dispatcher batch must load");
            };
            assert!(due.commands_mut().is_empty());
            (request, due.defer())
        };
        let mut loaded = {
            let _real_io = kithara_platform::flash::real_io();
            rig.fixture_dispatcher
                .send(
                    When::Next,
                    kithara_command::Batch {
                        basis: Vec::new(),
                        commands: vec![DispatcherCommand::Load(request)],
                    },
                )
                .expect("the actual initial request reaches the worker");
            let mut opening = std::pin::pin!(std::future::poll_fn(|context| {
                rig.fixture_dispatcher.hold(context.waker().clone());
                let receipt = rig.fixture_dispatcher.receipts().next();
                if let Some(receipt) = receipt {
                    rig.fixture_dispatcher.release();
                    let (outcome, _) = receipt.into();
                    let Outcome::Applied {
                        data: Dispatched::Loaded(loaded),
                        ..
                    } = outcome
                    else {
                        panic!("actual queued source must load: {outcome:?}");
                    };
                    std::task::Poll::Ready(loaded)
                } else {
                    std::task::Poll::Pending
                }
            }));
            loop {
                hang_tick!();
                if let Ok(loaded) =
                    kithara_platform::time::timeout(Duration::from_millis(10), opening.as_mut())
                        .await
                {
                    hang_reset!();
                    break loaded;
                }
            }
        };
        if frames != declared_frames {
            let source = loaded
                .opened
                .pcm
                .next_chunk_info()
                .expect("prepared source");
            assert_eq!(usize::try_from(source.frames).ok(), Some(frames));
            let end = source
                .source_span
                .expect("mapped source")
                .position_at(u64::from(source.frames))
                .expect("physical source endpoint");
            let declared = Some(Duration::from_secs_f64(duration));
            let (opened, mut packets) = crate::mock::opened_with_source(
                loaded.opened,
                AudioChunkInfo {
                    media_end: declared,
                    ..source
                },
            );
            assert_eq!(
                opened
                    .pcm
                    .next_chunk_info()
                    .expect("declared source")
                    .media_end,
                declared
            );
            packets.push(PcmPacket::Chunk(Box::new(AudioChunk::new(
                AudioChunkInfo {
                    frames: 0,
                    lane_frame: source.lane_frame + u64::from(source.frames),
                    frame_offset: source.frame_offset + u64::from(source.frames),
                    timestamp: end,
                    source_span: None,
                    end_of_track: true,
                    ..source
                },
                kithara_test_utils::bufpool::sample_buffer(&pools(), &[]),
            ))));
            loaded.opened = opened;
            rig.source_packets = Some(packets);
        }
        rig.loads
            .opens
            .resume(seq, (), ())
            .expect("initial open resumes")
            .apply(Dispatched::Loaded(loaded));
        let receipt = rig
            .loads
            .dispatcher
            .receipts()
            .next()
            .expect("initial open receipt");
        rig.pass(|queue, out| Player::settle(queue, TrackReceipt::Loaded(receipt), out));
        loop {
            hang_tick!();
            rig.read(1).await;
            if rig.queue.current == Some(THRESHOLD_ITEM) {
                hang_reset!();
                break;
            }
            kithara_platform::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(
            rig.mixer
                .as_mut()
                .expect("returned threshold mixer")
                .ends
                .snapshot
                .read()
                .slots[0]
                .position,
            0.0,
            "paused admission must not consume the first read"
        );
        assert_eq!(rig.ends, 0);
        rig.order.clear();
        rig.queue.insert_entry(
            THRESHOLD_NEXT,
            rig.source.clone(),
            kithara_play::Position::ZERO,
            crate::queue::types::Placement::Append,
        );
        rig.apply(QueueCommand::Play { at: When::Next });
        rig.monitoring = true;
        rig.queue.config.action_at_item_end = ActionAtItemEnd::Advance;
        rig
    }

    fn pass<Value>(
        &mut self,
        run: impl FnOnce(&mut Queue<TestPools>, &mut Outbox<'_, TestPools>) -> Value,
    ) -> Value {
        let value = real_outbox(
            &mut self.queue,
            self.mixer.as_mut().expect("returned threshold mixer"),
            &mut self.loads,
            SessionFrame::new(self.frame),
            u32::try_from(THRESHOLD_BLOCK).expect("threshold block fits u32"),
            |queue, out| {
                let (mix, suspended, now, delivery, output, deck) = {
                    let pass = out.pass().expect("host pass");
                    (
                        pass.mix,
                        pass.suspended,
                        pass.now,
                        pass.delivery,
                        *pass.output,
                        pass.deck.clone(),
                    )
                };
                let pass = DeckPass {
                    mix,
                    suspended,
                    now,
                    delivery,
                    output: &output,
                    deck: &deck,
                };
                HostedDeck::tick(queue, pass, out);
                run(queue, out)
            },
        );
        if self.monitoring {
            self.observe();
        }
        value
    }

    fn apply(&mut self, command: QueueCommand<TestPools>) {
        self.pass(|queue, out| Player::apply(queue, command, out))
            .expect("threshold command");
    }

    #[kithara::flash(io)]
    #[kithara::hang_watchdog(timeout = Duration::from_secs(1), ctx = String)]
    async fn seek(&mut self, position: f64) {
        self.read(1).await;
        let segment = self
            .queue
            .current_track()
            .expect("current track")
            .snapshot()
            .mark
            .expect("playing threshold track has a lane mark")
            .lane
            .segment;
        let to = kithara_play::Position::try_from_secs_f64(position).expect("seek position");
        self.apply(QueueCommand::Seek { to });
        loop {
            hang_tick!(format!(
                "threshold seek to {position}: {:?}",
                self.queue.current_track().map(Player::snapshot)
            ));
            self.read(1).await;
            let snapshot = self
                .queue
                .current_track()
                .expect("current track")
                .snapshot();
            if !snapshot.pending_lane
                && matches!(snapshot.status, kithara_play::TrackStatus::Playing { .. })
                && snapshot
                    .mark
                    .is_some_and(|mark| mark.lane.segment != segment)
                && snapshot.position >= to
            {
                hang_reset!();
                break;
            }
            kithara_platform::time::sleep(Duration::from_millis(1)).await;
        }
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn read(&mut self, frames: usize) -> [Vec<f32>; 2] {
        hang_tick!();
        let mut mixer = self.mixer.take().expect("threshold mixer custody");
        let at = SessionFrame::new(self.frame);
        let (mixer, pcm) = kithara_platform::tokio::task::spawn_blocking(move || {
            let mut pcm = [vec![0.0; frames], vec![0.0; frames]];
            let [left, right] = &mut pcm;
            mixer
                .block_frames(at, [left, right])
                .expect("threshold read");
            (mixer, pcm)
        })
        .await
        .expect("threshold render must join");
        self.mixer = Some(mixer);
        while let Some(ScopedReceipt::Scope(_, receipt)) = self
            .mixer
            .as_mut()
            .expect("returned threshold mixer")
            .ring
            .receipt()
        {
            hang_tick!();
            let seq = receipt.seq();
            let (outcome, mut batch) = receipt.into();
            self.pass(|queue, out| {
                Player::settle(
                    queue,
                    TrackReceipt::Deck {
                        seq,
                        outcome: &outcome,
                        batch: &mut batch,
                    },
                    out,
                )
            });
            hang_reset!();
        }
        for event in self
            .mixer
            .as_mut()
            .expect("returned threshold mixer")
            .ends
            .events
            .drain()
            .collect::<Vec<_>>()
        {
            hang_tick!();
            if let kithara_play::DeckEvent::Ended { slot, .. } = event {
                let active = self
                    .queue
                    .active
                    .iter()
                    .find(|active| active.slot == slot)
                    .expect("EOF names a held track");
                assert_eq!(active.item, THRESHOLD_ITEM);
                let source = self.queue.control().track_source(active.item);
                assert_eq!(
                    source.as_ref().and_then(TrackSource::uri),
                    self.source.uri()
                );
                self.ends += 1;
                self.order.push("eof");
            }
            self.pass(|queue, out| Player::settle(queue, TrackReceipt::Event(event), out));
            hang_reset!();
        }
        self.frame += i64::try_from(frames).expect("read frame count fits session clock");
        self.pass(|_, _| ());
        if let Some(packets) = self.source_packets.as_mut() {
            while packets.returned().is_some() {}
        }
        hang_reset!();
        pcm
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    fn observe(&mut self) {
        use kithara_render::{Dispatched, DispatcherCommand};

        self.loads.opens.drain();
        while let Some(due) = self.loads.opens.next_due((), 1) {
            hang_tick!();
            if matches!(
                due.commands().first(),
                Some(DispatcherCommand::SetPriority(_, _))
            ) {
                due.apply(Dispatched::Prioritized);
                hang_reset!();
                continue;
            }
            if matches!(due.commands().first(), Some(DispatcherCommand::Release(_))) {
                due.apply(Dispatched::Released);
                hang_reset!();
                continue;
            }
            due.defer();
            hang_reset!();
        }
        if self.successor_seq.is_none()
            && self
                .queue
                .active
                .iter()
                .map(|active| active.item)
                .chain(self.queue.active.parked_iter().map(|parked| parked.item))
                .any(|item| item == THRESHOLD_NEXT)
        {
            let (_, load) = self
                .queue
                .active
                .iter()
                .map(|active| (active.item, active.load))
                .chain(
                    self.queue
                        .active
                        .parked_iter()
                        .map(|parked| (parked.item, parked.load)),
                )
                .find(|(item, _)| *item == THRESHOLD_NEXT)
                .expect("preload request belongs to the next queue item");
            let seq = load.expect("successor remains unadmitted").seq();
            let due = self
                .loads
                .opens
                .resume(seq, (), ())
                .expect("preload owns its original dispatcher request");
            assert_eq!(due.seq(), seq);
            assert_eq!(due.commands().len(), 1, "one load per request");
            assert!(matches!(
                due.commands().first(),
                Some(DispatcherCommand::Load(_))
            ));
            assert!(
                self.successor_seq.is_none(),
                "no second successor load in this playback cycle"
            );
            self.successor_seq = Some(seq);
            due.defer();
            hang_reset!();
        }
        if let Some(active) = self
            .queue
            .active
            .iter()
            .find(|active| active.item == THRESHOLD_NEXT)
        {
            assert_eq!(
                Some(active.load.expect("successor remains unadmitted").seq()),
                self.successor_seq
            );
            if self.prefetches == 0 {
                self.prefetches += 1;
                self.order.push("prefetch");
            }
        }
        if let Some(target) = self
            .queue
            .target
            .filter(|target| target.to == THRESHOLD_NEXT)
        {
            assert!(target.auto);
            assert_eq!(target.reason, AdvanceReason::NaturalEof);
            if let Some(expected) = self.target_bound {
                assert_eq!(
                    target.bound, expected,
                    "handover must not be requested twice"
                );
            } else {
                self.target_bound = Some(target.bound);
                self.handovers += 1;
                self.order.push("handover");
            }
        }
    }

    #[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    async fn until(&mut self, blocks: usize, eof: bool) {
        for _ in 0..blocks {
            hang_tick!();
            self.read(THRESHOLD_BLOCK).await;
            hang_reset!();
            if if eof {
                self.ends > 0
            } else {
                self.handovers > 0
            } {
                break;
            }
        }
    }
}

#[kithara::test(tokio)]
async fn handover_emits_once_when_position_crosses_fade_threshold() {
    let mut rig = ThresholdRig::playing(10.0, 0.2, 3.5, None).await;
    rig.seek(9.79).await;
    rig.until(4, false).await;
    assert_eq!(rig.handovers, 1);
    assert_eq!(
        rig.ends, 0,
        "threshold-triggered handover should precede EOF"
    );
    rig.read(THRESHOLD_BLOCK).await;
    assert_eq!(
        rig.handovers, 1,
        "handover must not be requested twice in one playback cycle"
    );
}

#[kithara::test(tokio)]
#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn prefetch_fires_before_handover() {
    for (duration, lead, seek) in [(10.0, 2.0, Some(8.5)), (0.5, 5.0, None)] {
        hang_tick!();
        let mut rig = ThresholdRig::playing(duration, 0.0, lead, None).await;
        if let Some(position) = seek {
            rig.seek(position).await;
        }
        rig.read(THRESHOLD_BLOCK).await;
        hang_reset!();
        assert_eq!(
            rig.prefetches, 1,
            "preload must fire inside the prefetch lead window"
        );
        assert_eq!(
            rig.handovers, 0,
            "handover must not fire while pos < dur - fade"
        );
    }
}

#[kithara::test(tokio)]
async fn handover_fires_after_prefetch_when_position_reaches_fade_threshold() {
    let mut rig = ThresholdRig::playing(10.0, 0.2, 2.0, None).await;
    rig.seek(8.5).await;
    rig.read(THRESHOLD_BLOCK).await;
    assert_eq!(rig.prefetches, 1);
    assert_eq!(rig.handovers, 0);
    rig.seek(9.79).await;
    rig.until(4, false).await;
    assert_eq!(rig.handovers, 1);
    assert_eq!(
        rig.order
            .iter()
            .filter(|event| **event != "eof")
            .copied()
            .collect::<Vec<_>>(),
        ["prefetch", "handover"]
    );
}

#[kithara::test(tokio)]
async fn prefetch_and_handover_both_fire_when_thresholds_coincide() {
    let mut rig = ThresholdRig::playing(10.0, 0.2, 0.0, None).await;
    rig.seek(5.0).await;
    rig.read(THRESHOLD_BLOCK).await;
    assert_eq!(rig.prefetches, 0);
    assert_eq!(rig.handovers, 0);
    rig.seek(9.79).await;
    rig.until(4, false).await;
    assert_eq!(rig.prefetches, 1, "prefetch must fire exactly once");
    assert_eq!(rig.handovers, 1, "handover must fire exactly once");
}

#[kithara::test(tokio)]
async fn handover_backstops_eof_when_threshold_was_not_reached_earlier() {
    let mut rig = ThresholdRig::playing(0.01, 0.0, 3.5, None).await;
    rig.read(THRESHOLD_BLOCK).await;
    let slot = &rig
        .mixer
        .as_mut()
        .expect("returned threshold mixer")
        .ends
        .snapshot
        .read()
        .slots[0];
    assert_eq!(
        slot.state,
        SlotState::Ended,
        "first read must be partial at EOF"
    );
    assert_eq!(
        (slot.position * f64::from(mock::SAMPLE_RATE.get()))
            .round()
            .to_usize()
            .expect("EOF position frames fit usize"),
        441,
        "the partial first read consumes the 0.01-second source, not zero-frame EOF"
    );
    assert!(
        slot.position * f64::from(mock::SAMPLE_RATE.get())
            < f64::from(u32::try_from(THRESHOLD_BLOCK).expect("threshold block fits u32"))
    );
    assert_eq!(rig.handovers, 1);
    assert_eq!(rig.ends, 1);
}

#[kithara::test(tokio)]
async fn handover_is_not_duplicated_at_eof_after_early_trigger() {
    let mut rig = ThresholdRig::playing(5.0, 0.2, 3.5, None).await;
    rig.until(600, true).await;
    assert_eq!(rig.handovers, 1);
    assert_eq!(rig.ends, 1);
    assert_eq!(rig.order, ["prefetch", "handover", "eof"]);
}

#[kithara::test(tokio)]
async fn handover_uses_buffered_eof_when_duration_is_overestimated() {
    let mut rig = ThresholdRig::playing(10.0, 0.0, 3.5, Some(900)).await;
    let pcm = rig.read(THRESHOLD_BLOCK).await;
    let observed = rig
        .mixer
        .as_mut()
        .expect("returned threshold mixer")
        .ends
        .snapshot
        .read();
    let slot = &observed.slots[0];
    let source = slot.source.expect("consumed source timing");
    assert_eq!(
        source.segment,
        slot.mark.expect("consumed mark").lane.segment
    );
    let raw_end = source
        .media_origin
        .expect("source media origin")
        .checked_add(source.media_end.expect("source media endpoint"))
        .expect("raw source endpoint");
    let duration = raw_end
        .checked_sub(source.visible_origin.expect("original visible origin"))
        .expect("physical visible span");
    assert_eq!(
        slot.state,
        SlotState::Playing,
        "first 512-frame read must be full"
    );
    assert_eq!(
        (slot.position * f64::from(mock::SAMPLE_RATE.get()))
            .round()
            .to_usize()
            .expect("position frames fit usize"),
        512
    );
    assert_eq!(
        (duration.as_secs_f64() * f64::from(mock::SAMPLE_RATE.get()))
            .round()
            .to_usize()
            .expect("duration frames fit usize"),
        900
    );
    assert_eq!(
        ((raw_end.as_secs_f64() - slot.position) * f64::from(mock::SAMPLE_RATE.get()))
            .round()
            .to_usize()
            .expect("remaining frames fit usize"),
        388
    );
    assert!(duration < Duration::from_secs(10));
    assert_eq!(pcm[0].len(), 512);
    assert!(
        pcm[0][511] > 0.0,
        "the last frame of the first full block is audio"
    );
    assert_eq!(
        rig.handovers, 1,
        "buffered EOF must hand over before the EOF block"
    );
    assert_eq!(rig.ends, 0, "first full block must not emit EOF");
}

fn real_outbox<Value>(
    queue: &mut Queue<TestPools>,
    mixer: &mut mock::MixerRig,
    loads: &mut mock::DeckRig<TestPools>,
    at: SessionFrame,
    frames: u32,
    run: impl FnOnce(&mut Queue<TestPools>, &mut Outbox<'_, TestPools>) -> Value,
) -> Value {
    let shape = kithara_render::rt::StreamShape::new(
        NonZeroU32::new(1024).expect("block size"),
        mock::SAMPLE_RATE,
    );
    let output = mock::output(Some(shape)).get();
    let deck = mixer.ends.snapshot.read().clone();
    let pass = DeckPass {
        mix: DeckMixSettings::default(),
        suspended: false,
        now: at,
        delivery: FrameCount::new(usize::try_from(frames).expect("delivery frames fit usize")),
        output: &output,
        deck: &deck,
    };
    let mut scope = mixer.ring.scope(mixer.scope).expect("real deck scope");
    run(
        queue,
        &mut Outbox::new(&mut scope, &mut loads.dispatcher).in_pass(pass),
    )
}

#[kithara::hang_watchdog(timeout = Duration::from_secs(1))]
fn real_block(
    queue: &mut Queue<TestPools>,
    mixer: &mut mock::MixerRig,
    loads: &mut mock::DeckRig<TestPools>,
    at: i64,
    frames: u32,
) -> [Vec<f32>; 2] {
    let at = SessionFrame::new(at);
    let pcm_frames = usize::try_from(frames).expect("render frames fit usize");
    let mut pcm = [vec![0.0; pcm_frames], vec![0.0; pcm_frames]];
    let [left, right] = &mut pcm;
    mixer
        .block_frames(at, [left, right])
        .expect("real mixer callback");
    while let Some(ScopedReceipt::Scope(_, receipt)) = mixer.ring.receipt() {
        hang_tick!();
        let seq = receipt.seq();
        let (outcome, mut batch) = receipt.into();
        real_outbox(queue, mixer, loads, at, frames, |queue, out| {
            Player::settle(
                queue,
                TrackReceipt::Deck {
                    seq,
                    outcome: &outcome,
                    batch: &mut batch,
                },
                out,
            )
        });
        hang_reset!();
    }
    let events = mixer.ends.events.drain().collect::<Vec<_>>();
    for event in events {
        hang_tick!();
        real_outbox(queue, mixer, loads, at, frames, |queue, out| {
            Player::settle(queue, TrackReceipt::Event(event), out)
        });
        hang_reset!();
    }
    pcm
}

async fn answer_duration(
    queue: &mut Queue<TestPools>,
    rig: &mut mock::DeckRig<TestPools>,
    dir: &TestTempDir,
    duration: f64,
) -> kithara_render::mock::PacketRing {
    let mut loaded = loaded_fixture(queue.worker().expect("fixture worker"), rig, dir).await;
    let source = loaded
        .opened
        .pcm
        .next_chunk_info()
        .expect("prepared fixture source");
    let (opened, packets) = crate::mock::opened_with_source(
        loaded.opened,
        AudioChunkInfo {
            media_end: Some(Duration::from_secs_f64(duration)),
            ..source
        },
    );
    loaded.opened = opened;
    let receipt = rig.open(Ok(loaded)).expect("duration load receipt");
    with_outbox(queue, rig, |queue, out| {
        Player::settle(queue, TrackReceipt::Loaded(receipt), out)
    });
    packets
}

fn block(queue: &mut Queue<TestPools>, rig: &mut mock::DeckRig<TestPools>, at: i64) {
    crate::mock::settle(queue, rig, SessionFrame::new(at));
}

#[kithara::test(tokio)]
async fn a_fade_in_makes_its_track_leading_and_a_preload_does_not() {
    let (mut queue, _, second, mut rig, dir) = pending_selection();
    let leading_source = answer_duration(&mut queue, &mut rig, &dir, 64.0).await;
    crate::mock::finish(&mut queue, &mut rig);
    assert_eq!(queue.control().duration_seconds(), Some(64.0));
    with_outbox(&mut queue, &mut rig, |queue, out| {
        queue.apply_command(
            QueueCommand::Select {
                id: second,
                transition: Transition::None,
            },
            Some(&mock::output(None).get()),
            out,
        )
    })
    .expect("prepare successor");
    let successor_source = answer_duration(&mut queue, &mut rig, &dir, 162.0).await;
    block(&mut queue, &mut rig, 0);
    assert_eq!(
        queue.control().duration_seconds(),
        Some(64.0),
        "preload must not publish the next track duration"
    );
    block(&mut queue, &mut rig, 128);
    block(&mut queue, &mut rig, 256);
    assert_eq!(queue.control().position_seconds(), Some(0.0));
    assert_eq!(queue.control().duration_seconds(), Some(162.0));
    drop((leading_source, successor_source));
}

#[kithara::test(tokio)]
#[case::stitched_in(0.01, Some(SlotState::Playing))]
#[case::still_preloading(60.0, None)]
async fn cancel_preload_unloads_a_successor_only_while_it_preloads(
    #[case] leading_secs: f64,
    #[case] after_cancel: Option<SlotState>,
) {
    let dir = TestTempDir::new();
    let prep = ResourcePrep::builder()
        .worker(PlayWorker::new(PlayWorkerConfig::builder(pools()).build()))
        .build();
    let mut queue = Queue::new(
        QueueConfig::builder()
            .prep(prep)
            .store(
                AssetStore::builder(pools())
                    .backend(StorageBackend::Memory)
                    .build(),
            )
            .build(),
    );
    queue.clock = Some((SessionFrame::new(0), FrameCount::new(1)));
    queue.deck.mixer.sample_rate = mock::SAMPLE_RATE.get();
    let mut rig = mock::DeckRig::new(DeckMixerConfig::default()).expect("load dispatcher");
    let first = TrackId::allocate();
    let second = TrackId::allocate();
    let shape = kithara_render::rt::StreamShape::new(
        NonZeroU32::new(1024).expect("block size"),
        mock::SAMPLE_RATE,
    );
    let mut mixer =
        mock::MixerRig::new(DeckMixerConfig::default(), shape, &pools()).expect("real deck");
    let frames: usize = (leading_secs * f64::from(mock::SAMPLE_RATE.get()))
        .floor()
        .to_usize()
        .expect("leading frame count fits usize");
    let samples = vec![0.5; frames * 2];
    mock::write_pcm_wav(
        &dir.path().join("entry.wav"),
        &samples,
        AudioSpec::new(2, mock::SAMPLE_RATE),
    )
    .expect("leading WAV");
    let successor_dir = TestTempDir::new();
    mock::write_pcm_wav(
        &successor_dir.path().join("entry.wav"),
        &vec![0.5; 8192],
        AudioSpec::new(2, mock::SAMPLE_RATE),
    )
    .expect("successor WAV");
    real_outbox(
        &mut queue,
        &mut mixer,
        &mut rig,
        SessionFrame::new(0),
        1,
        |queue, out| {
            for (id, source_dir) in [(first, &dir), (second, &successor_dir)] {
                Player::apply(
                    queue,
                    QueueCommand::Append {
                        position: kithara_play::Position::ZERO,
                        id,
                        source: TrackSource::Uri(
                            source_dir
                                .path()
                                .join("entry.wav")
                                .to_str()
                                .expect("fixture path")
                                .to_owned(),
                        ),
                    },
                    out,
                )
                .expect("append");
            }
            Player::apply(
                queue,
                QueueCommand::Select {
                    id: first,
                    transition: Transition::None,
                },
                out,
            )
            .expect("select leading");
            Player::apply(queue, QueueCommand::Pause { at: When::Next }, out)
                .expect("prepare paused leading");
        },
    );
    let leading = loaded_fixture(queue.worker().expect("fixture worker"), &mut rig, &dir).await;
    let receipt = rig.open(Ok(leading)).expect("leading load");
    real_outbox(
        &mut queue,
        &mut mixer,
        &mut rig,
        SessionFrame::new(0),
        1,
        |queue, out| Player::settle(queue, TrackReceipt::Loaded(receipt), out),
    );
    real_block(&mut queue, &mut mixer, &mut rig, 0, 1);
    real_block(&mut queue, &mut mixer, &mut rig, 1, 1);
    real_outbox(
        &mut queue,
        &mut mixer,
        &mut rig,
        SessionFrame::new(2),
        1,
        |queue, out| Player::apply(queue, QueueCommand::Play { at: When::Next }, out),
    )
    .expect("start leading");
    real_block(&mut queue, &mut mixer, &mut rig, 2, 1);
    real_outbox(
        &mut queue,
        &mut mixer,
        &mut rig,
        SessionFrame::new(3),
        1,
        |queue, out| {
            queue.request_transition(
                crate::queue::transition::TransitionRequest {
                    id: second,
                    transition: Transition::None,
                    reason: AdvanceReason::NaturalEof,
                    auto: true,
                    playing: true,
                },
                Some(&mock::output(Some(shape)).get()),
                out,
            )
        },
    )
    .expect("gapless successor");
    let successor = loaded_fixture(
        queue.worker().expect("fixture worker"),
        &mut rig,
        &successor_dir,
    )
    .await;
    let receipt = rig.open(Ok(successor)).expect("successor load");
    real_outbox(
        &mut queue,
        &mut mixer,
        &mut rig,
        SessionFrame::new(3),
        1,
        |queue, out| Player::settle(queue, TrackReceipt::Loaded(receipt), out),
    );
    real_block(&mut queue, &mut mixer, &mut rig, 3, 1);
    real_block(&mut queue, &mut mixer, &mut rig, 4, 1);
    let pcm = real_block(&mut queue, &mut mixer, &mut rig, 5, 1024);
    let rendered = pcm.iter().flatten().any(|sample| *sample != 0.0);
    assert!(rendered, "the leading track renders");
    real_outbox(
        &mut queue,
        &mut mixer,
        &mut rig,
        SessionFrame::new(1029),
        1,
        Queue::cancel_target,
    )
    .expect("withdraw target");
    real_block(&mut queue, &mut mixer, &mut rig, 1029, 1);
    assert_eq!(
        queue
            .active
            .iter()
            .find(|active| active.item == second)
            .filter(|active| active.role != Role::Leaving)
            .map(|active| {
                if matches!(
                    active.track.snapshot().status,
                    PlayingStatus::Playing { .. }
                ) {
                    SlotState::Playing
                } else {
                    SlotState::Stopped
                }
            }),
        after_cancel
    );
    assert_eq!(
        queue.current,
        if after_cancel.is_some() {
            Some(second)
        } else {
            Some(first)
        }
    );
}
