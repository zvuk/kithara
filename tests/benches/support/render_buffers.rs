use std::num::NonZeroU32;

use criterion::{BatchSize, Criterion};
use firewheel::{
    clock::InstantSamples,
    dsp::{buffer::ConstSequentialBuffer, declick::DeclickValues},
    log::{RealtimeLoggerConfig, realtime_logger},
    mask::{ConnectedMask, ConstantMask, SilenceMask},
    node::{
        AudioNodeProcessor, NUM_SCRATCH_BUFFERS, ProcBuffers, ProcExtra, ProcInfo, ProcStore,
        StreamStatus,
    },
};
use kithara::{
    audio::{AudioConfig, mock::TestPcmReader},
    file::{File, FileConfig, FileSrc},
    host::{HostCommand, HostSettingsChange},
    platform::{sync::Arc, time::Duration},
    play::{PlayWorker, PlayWorkerConfig, SessionDuckingMode},
    queue::TrackSource,
    signal::{AudioSpec, OutputContext, SegmentId, SessionEpoch, SessionFrame},
    sync::SpeedCurve,
    warp::{RenderContext, StretchKind},
};
use kithara_command::{Batch, Outcome, ScopedReceipt, Seq, When};
use kithara_dsp::param::{DEFAULT_SMOOTH_SECONDS, MIN_SETTLE_RATIO, SmoothedParam, SmootherConfig};
use kithara_integration_tests::{
    assets_ext::memory_asset_store,
    bufpool_ext::{TestPools, pools},
    mock::PcmDeck,
    offline::OfflinePlayer,
};
use kithara_render::{
    CrossfadeSettings, LaneCommand, LaneProtocol, LaneStart, LaneTask, PcmPacket, PcmReceiver,
    bridge::{DeckMixSettings, DeckPart, Fade, Slot},
    mock::{PacketRing, TestEnds, TestMixer, TestSessionInbox, chunk, mixer_with_shape, send},
    rt::{
        DeckMixer, DeckMixerConfig, StreamShape, install_render_context, publish_render_context,
        track::{PcmConsumer, PlayerResource},
    },
};
use kithara_test_fixtures::integration_fixtures::benchmark_half;
use num_traits::ToPrimitive;

const RATE: u32 = 48_000;
const SEED: usize = 512;

fn rate() -> NonZeroU32 {
    NonZeroU32::new(RATE).expect("fixture rate")
}
fn spec() -> AudioSpec {
    AudioSpec::new(2, rate())
}
fn instant() -> Fade {
    Fade::Crossfade(CrossfadeSettings {
        duration: 0.0,
        ..CrossfadeSettings::default()
    })
}

fn assert_pcm(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    let mismatch =
        actual
            .iter()
            .zip(expected)
            .enumerate()
            .find_map(|(index, (actual, expected))| {
                (actual.to_bits() != expected.to_bits()).then_some((
                    index,
                    actual.to_bits(),
                    expected.to_bits(),
                ))
            });
    assert_eq!(mismatch, None, "complete PCM must match the scalar law");
}

struct DeckFixture {
    mixer: DeckMixer<TestSessionInbox>,
    extra: ProcExtra,
    ends: TestEnds,
    frame: i64,
    _packets: Vec<PacketRing>,
}

impl DeckFixture {
    fn new() -> Self {
        let config = DeckMixerConfig::builder()
            .eq_bands(0)
            .mix(DeckMixSettings::builder().level(0.5).build())
            .build();
        let (TestMixer { mixer, inbox }, ends) = mixer_with_shape(
            config,
            16,
            StreamShape::new(NonZeroU32::new(1024).expect("block"), rate()),
        );
        let mut store = ProcStore::with_capacity(2);
        assert!(store.insert(inbox).is_ok());
        install_render_context(&mut store).expect("context slot");
        let (logger, _) = realtime_logger(RealtimeLoggerConfig::default());
        Self {
            mixer,
            ends,
            frame: 0,
            _packets: Vec::new(),
            extra: ProcExtra {
                logger,
                store,
                scratch_buffers: ConstSequentialBuffer::<f32, NUM_SCRATCH_BUFFERS>::new(1024),
                declick_values: DeclickValues::new(NonZeroU32::new(16).expect("declick")),
            },
        }
    }

    fn pcm(&mut self, shapes: &[(SegmentId, usize, f32)]) -> Box<PlayerResource> {
        let mut ring = PacketRing::new(spec(), Duration::from_secs(1), 8);
        for &(segment, frames, value) in shapes {
            ring.push(PcmPacket::Chunk(Box::new(chunk(
                spec(),
                segment,
                0,
                0,
                &vec![value; frames * 2],
            ))));
        }
        let resource = PlayerResource::new(
            PcmConsumer::new(ring.receiver.take().expect("receiver")),
            Arc::from("render-buffer-admission"),
            &pools(),
        )
        .expect("prepared resource");
        self._packets.push(ring);
        Box::new(resource)
    }

    fn command(&mut self, commands: Vec<DeckPart>) -> Seq {
        send(&mut self.ends, When::Next, commands)
    }

    fn applied(&mut self, seq: Seq, at: i64) {
        let receipts: Vec<_> = std::iter::from_fn(|| self.ends.ring.receipt()).collect();
        assert!(receipts.iter().any(|receipt| matches!(receipt,
            ScopedReceipt::Scope(_, reply) if reply.seq() == seq && matches!(reply.outcome(), Outcome::Applied { at: frame, .. } if *frame == SessionFrame::new(at)))));
    }

    fn render(&mut self, frames: usize) -> [Vec<f32>; 2] {
        self.extra
            .store
            .try_get_mut::<TestSessionInbox>()
            .expect("inbox")
            .0
            .drain();
        let end = self.frame + i64::try_from(frames).expect("frames");
        publish_render_context(
            &mut self.extra.store,
            RenderContext::new_linear(
                OutputContext::new(
                    SessionFrame::new(self.frame)..SessionFrame::new(end),
                    rate(),
                    SessionEpoch::new(0),
                    None,
                )
                .expect("output"),
                None,
            )
            .expect("context"),
        )
        .expect("publish");
        let info = ProcInfo {
            sample_rate: rate(),
            frames,
            in_silence_mask: SilenceMask::default(),
            out_silence_mask: SilenceMask::default(),
            in_constant_mask: ConstantMask::default(),
            out_constant_mask: ConstantMask::default(),
            in_connected_mask: ConnectedMask::default(),
            out_connected_mask: ConnectedMask::default(),
            total_cpu_seconds_recip: 1.0,
            process_to_playback_delay: None,
            did_just_unbypass: false,
            last_marker_instant: InstantSamples(self.frame),
            sample_rate_recip: f64::from(RATE).recip(),
            clock_samples: InstantSamples(self.frame),
            duration_since_stream_start: Duration::ZERO,
            stream_status: StreamStatus::empty(),
            dropped_frames: 0,
        };
        let (mut left, mut right) = (vec![0.0; frames], vec![0.0; frames]);
        self.mixer.process(
            &info,
            ProcBuffers {
                inputs: &[],
                outputs: &mut [&mut left, &mut right],
            },
            &mut self.extra,
        );
        self.frame = end;
        [left, right]
    }
}

fn seeded(shapes: &[(SegmentId, usize, f32)]) -> DeckFixture {
    let mut fixture = DeckFixture::new();
    let pcm = fixture.pcm(shapes);
    let seq = fixture.command(vec![
        DeckPart::Attach {
            item_id: kithara_events::TrackId::allocate(),
            slot: Slot::new(0),
            pcm,
            segment: SegmentId::FIRST,
        },
        DeckPart::Start {
            slot: Slot::new(0),
            fade: instant(),
        },
    ]);
    for channel in fixture.render(SEED) {
        assert_pcm(&channel, &vec![0.25; SEED]);
    }
    fixture.applied(seq, 0);
    fixture
}

fn adopt_fixture() -> DeckFixture {
    let mut fixture = seeded(&[
        (SegmentId::FIRST, SEED + 7, 0.5),
        (SegmentId::FIRST.next(), 257, 0.25),
    ]);
    fixture.command(vec![DeckPart::Adopt {
        slot: Slot::new(0),
        segment: SegmentId::FIRST.next(),
    }]);
    fixture
}

fn check_adopt(fixture: &mut DeckFixture, output: &[Vec<f32>; 2]) {
    let requested = DeckMixerConfig::default().declick_frames(rate()).get();
    assert!(requested > 7);
    let expected: Vec<_> = (0..257)
        .map(|frame| {
            let progress = frame.min(requested - 1).to_f32().expect("frame")
                / (requested - 1).to_f32().expect("ramp length");
            let incoming = 0.25 * progress;
            let tail = if frame < 7 {
                0.5 * (1.0 - progress)
            } else {
                0.0
            };
            (incoming + tail) * 0.5
        })
        .collect();
    for channel in output {
        assert_pcm(channel, &expected);
    }
    let track = fixture.mixer.track(Slot::new(0)).expect("adopted track");
    assert_eq!(track.segment(), SegmentId::FIRST.next());
    assert_eq!(
        track
            .mark(SessionFrame::new(fixture.frame))
            .expect("mapped cursor")
            .lane
            .frame,
        257
    );
}

fn refill_fixture() -> (DeckFixture, f32) {
    let mut fixture = seeded(&[(SegmentId::FIRST, SEED + 7, 0.5)]);
    let pcm = fixture.pcm(&[(SegmentId::FIRST, 14, 0.25)]);
    let seq = fixture.command(vec![DeckPart::Replace {
        item_id: kithara_events::TrackId::allocate(),
        slot: Slot::new(0),
        pcm,
        segment: SegmentId::FIRST,
    }]);
    let _ = fixture.render(3);
    fixture.applied(seq, i64::try_from(SEED).expect("seed frames"));
    let gain = fixture
        .mixer
        .track(Slot::new(0))
        .expect("replacement")
        .gain();
    assert!(gain > 0.0 && gain < 1.0);
    fixture.command(vec![DeckPart::Detach { slot: Slot::new(0) }]);
    (fixture, gain)
}

fn check_refill(fixture: &mut DeckFixture, gain: f32, output: &[Vec<f32>; 2]) {
    let requested = DeckMixerConfig::default().declick_frames(rate()).get();
    let expected: Vec<_> = (0..16)
        .map(|frame| {
            let previous = if frame < 4 {
                0.5 * (1.0
                    - (frame + 3).to_f32().expect("tail frame")
                        / (DeckMixerConfig::default().evict_fade().get() - 1)
                            .to_f32()
                            .expect("eviction length"))
            } else {
                0.0
            };
            let incoming = if frame < 11 {
                0.25 * (gain
                    * (1.0
                        - frame.to_f32().expect("tail frame")
                            / (requested - 1).to_f32().expect("ramp length")))
            } else {
                0.0
            };
            (previous + incoming) * 0.5
        })
        .collect();
    for channel in output {
        assert_pcm(channel, &expected);
    }
    assert!(fixture.mixer.track(Slot::new(0)).is_none());
    for channel in fixture.render(16) {
        assert_pcm(&channel, &[0.0; 16]);
    }
}

struct JumpFixture {
    receiver: PcmReceiver,
    lane: Box<dyn LaneTask + Send>,
    sender: kithara_command::Sender<LaneProtocol>,
    seq: Seq,
    start: u64,
    _source: PcmDeck,
}

async fn jump_fixture() -> JumpFixture {
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools()).build());
    let source = PcmDeck::new(Box::new(TestPcmReader::with_pcm(
        spec(),
        120.0,
        benchmark_half(),
    )));
    let TrackSource::Uri(path) = source.source() else {
        panic!("PCM URI")
    };
    let audio = AudioConfig::<File<TestPools>>::for_stream(
        FileConfig::for_src(FileSrc::Local(path.into()))
            .store(memory_asset_store())
            .build(),
    )
    .hint("wav".to_owned())
    .build();
    let config = kithara::play::TrackConfig::for_audio(audio).build();
    let (mut sender, inbox) = worker.lane_channel();
    let (mut receiver, lane, _) = worker
        .load(
            config,
            Duration::ZERO,
            LaneStart {
                speed: SpeedCurve::Constant(1.0),
                keylock: false,
                backend: StretchKind::default(),
            },
            inbox,
        )
        .await
        .expect("prepared actual worker");
    let mut start = 0;
    while let Some(packet) = receiver.pop() {
        if let PcmPacket::Chunk(chunk) = &packet {
            start = chunk.meta.lane_frame + u64::try_from(chunk.frames()).expect("preload frames");
        }
        receiver.recycle(packet).expect("recycle preload");
    }
    receiver.set_playing(true);
    let seq = sender
        .send(
            When::Next,
            Batch {
                commands: vec![LaneCommand::Jump { to: Duration::ZERO }],
                basis: Vec::new(),
            },
        )
        .expect("actual Jump command");
    JumpFixture {
        receiver,
        lane: Box::new(lane),
        sender,
        seq,
        start,
        _source: source,
    }
}

impl JumpFixture {
    fn run(&mut self) -> Vec<kithara::signal::AudioChunk> {
        let mut output = Vec::new();
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        for _ in 0..16 {
            let _ = self.lane.poll_commands(&mut context);
            self.lane.recycle();
            let _ = self.lane.tick();
            while let Some(packet) = self.receiver.pop() {
                match packet {
                    PcmPacket::Chunk(chunk) => output.push(*chunk),
                    PcmPacket::Failed { .. } => panic!("Jump source failure"),
                }
            }
            if output.len() >= 2 {
                break;
            }
        }
        output
    }
}

fn check_jump(fixture: &mut JumpFixture, output: &[kithara::signal::AudioChunk]) {
    assert_eq!(output.len(), 2);
    assert!(
        fixture
            .sender
            .receipts()
            .any(|receipt| receipt.seq() == fixture.seq
                && matches!(receipt.outcome(), Outcome::Applied { .. }))
    );
    let frames = DeckMixerConfig::default().declick_frames(rate()).get();
    assert_eq!(output[0].frames(), frames);
    assert!(output[1].frames() >= frames);
    assert_eq!(output[0].meta.lane_frame, fixture.start);
    assert_eq!(
        output[1].meta.lane_frame,
        fixture.start + u64::try_from(frames).expect("jump frames")
    );
    for (up, chunk) in output.iter().enumerate() {
        let expected: Vec<_> = (0..chunk.frames())
            .flat_map(|frame| {
                let progress = (frame.to_f32().expect("jump frame")
                    / frames.to_f32().expect("jump length"))
                .min(1.0);
                let gain = if up == 0 { 1.0 - progress } else { progress };
                [0.5 * gain; 2]
            })
            .collect();
        assert_pcm(&chunk.samples, &expected);
    }
}

async fn prepare_duck(player: &OfflinePlayer) {
    let lead = usize::try_from(player.host().max_block_frames().get()).expect("host quantum");
    let start = player.host().position() + u64::try_from(lead).expect("delivery frames");
    for (offset, mode) in [
        (17, SessionDuckingMode::Soft),
        (300, SessionDuckingMode::Hard),
        (500, SessionDuckingMode::Off),
    ] {
        player
            .host()
            .send(HostCommand::Configure(
                HostSettingsChange::Ducking(mode),
                When::At(SessionFrame::new(
                    i64::try_from(start + offset).expect("ducking frame"),
                )),
            ))
            .await
            .expect("frame-addressed ducking");
    }
    let _ = player.render(lead).await;
    assert_eq!(player.host().position(), start);
}

fn check_duck(output: &[f32]) {
    assert_eq!(output.len(), 513 * 2);
    let mut gain = SmoothedParam::new(
        1.0,
        1.0,
        SmootherConfig {
            smooth_seconds: DEFAULT_SMOOTH_SECONDS,
            settle_ratio: MIN_SETTLE_RATIO,
        },
        rate(),
    );
    let mut expected = Vec::new();
    for (frames, mode) in [
        (17, SessionDuckingMode::Off),
        (283, SessionDuckingMode::Soft),
        (200, SessionDuckingMode::Hard),
        (13, SessionDuckingMode::Off),
    ] {
        gain.set_value(mode.volume().amp());
        for _ in 0..frames {
            expected.extend([0.125 * gain.next_smoothed(); 2]);
        }
    }
    gain.settle();
    assert_pcm(output, &expected);
}

pub(super) fn bench(c: &mut Criterion) {
    let runtime = super::make_runtime();
    let mut group = c.benchmark_group("render_buffers");
    let mut adopt = adopt_fixture();
    let output = adopt.render(257);
    check_adopt(&mut adopt, &output);
    group.bench_function("adopt_partial_tail/257", |b| {
        b.iter_batched_ref(
            adopt_fixture,
            |fixture| fixture.render(257),
            BatchSize::PerIteration,
        );
    });
    let (mut refill, gain) = refill_fixture();
    let output = refill.render(16);
    check_refill(&mut refill, gain, &output);
    group.bench_function("slot_refill/16", |b| {
        b.iter_batched_ref(
            refill_fixture,
            |(fixture, _)| fixture.render(16),
            BatchSize::PerIteration,
        );
    });
    let mut jump = runtime.block_on(jump_fixture());
    let output = jump.run();
    check_jump(&mut jump, &output);
    group.bench_function("jump/240", |b| {
        b.iter_batched_ref(
            || runtime.block_on(jump_fixture()),
            JumpFixture::run,
            BatchSize::PerIteration,
        );
    });
    let player = runtime.block_on(super::render_player(SessionDuckingMode::Off));
    runtime.block_on(async {
        prepare_duck(&player).await;
        check_duck(&player.render(513).await);
    });
    runtime.block_on(player.close());
    let player = runtime.block_on(super::render_player(SessionDuckingMode::Off));
    group.bench_function("duck_spans/513", |b| {
        b.iter_batched(
            || runtime.block_on(prepare_duck(&player)),
            |()| runtime.block_on(player.render(513)),
            BatchSize::PerIteration,
        );
    });
    runtime.block_on(player.close());
    group.finish();
}

pub(super) async fn admit_player(player: &OfflinePlayer, label: &str, frames: usize) {
    if label != "settled" && label != "moving" {
        return;
    }
    let mut deck = SmoothedParam::new(0.25, 1.0, SmootherConfig::default(), rate());
    if label == "moving" {
        player
            .set_volume(0.75)
            .await
            .expect("admission target change");
        deck.set_value(0.75 * 0.75);
    }
    let amp = SessionDuckingMode::Soft.volume().amp();
    let mut master = SmoothedParam::new(
        amp,
        1.0,
        SmootherConfig {
            smooth_seconds: DEFAULT_SMOOTH_SECONDS,
            settle_ratio: MIN_SETTLE_RATIO,
        },
        rate(),
    );
    let mut expected = Vec::with_capacity(frames * 2);
    for _ in 0..frames {
        let gain = if label == "moving" {
            deck.next_smoothed()
        } else {
            0.25
        };
        expected.extend([(0.5 * gain) * master.next_smoothed(); 2]);
    }
    deck.settle();
    master.settle();
    assert_pcm(&player.render(frames).await, &expected);
}
