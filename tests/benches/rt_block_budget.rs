#![forbid(unsafe_code)]

use std::{
    hint::black_box,
    num::{NonZeroU32, NonZeroUsize},
    task::{Context, Waker},
};

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
    effects::GainDb,
    file::{File, FileConfig, FileSrc},
    platform::{
        sync::Arc,
        time::{Duration, Instant},
    },
    play::{PlayWorker, PlayWorkerConfig},
    queue::TrackSource,
    signal::{AudioSpec, OutputContext, SegmentId, SessionEpoch, SessionFrame, TransportRevision},
    sync::SpeedCurve,
    warp::{RenderContext, StretchKind},
};
use kithara_command::{
    Batch, ChannelConfig, LevelInbox, Port, ScopeId, ScopedConfig, ScopedInbox, ScopedSender,
    Sender, When, scoped_channel,
};
use kithara_integration_tests::{
    assets_ext::memory_asset_store,
    bufpool_ext::{TestPools, pools},
    mock::PcmDeck,
};
use kithara_render::{
    CrossfadeCurve, CrossfadeSettings, LaneProtocol, LaneStart, LaneTask,
    bridge::{
        DeckEnds, DeckEqChange, DeckPart, DeckProtocol, Fade, SessionInbox, Slot, SlotState,
        scope_channels,
    },
    rt::{
        DeckMixer, DeckMixerConfig, StreamShape, install_render_context, publish_render_context,
        track::{PcmConsumer, PlayerResource},
    },
};
use kithara_test_fixtures::integration_fixtures::benchmark_half;

mod consts {
    pub(super) const BLOCK_FRAMES: u32 = 128;
    pub(super) const CHANNELS: u16 = 2;
    /// Longer than the warm-up and measured blocks together, 22 000 blocks
    /// of 128 frames at 48 kHz (about 58.7 s), so every measured block of
    /// the crossfade lane mixes through the fade law.
    pub(super) const CROSSFADE_SECONDS: f32 = 120.0;
    pub(super) const CROSSFADE_DEPTH: f32 = 0.7;
    pub(super) const CROSSFADE_POSITION: f32 = 0.3;
    pub(super) const MEASURED_BLOCKS: usize = 20_000;
    pub(super) const SAMPLE_RATE: u32 = 48_000;
    pub(super) const TRACK_COUNTS: [usize; 3] = [1, 2, 4];
    pub(super) const TRACK_SECONDS: f64 = 600.0;
    pub(super) const WARMUP_BLOCKS: usize = 2_000;
}

/// What the tracks of a lane mix through on every measured block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lane {
    /// A declick that settles before the warm-up ends: a unity mix.
    Settled,
    /// One equal-power crossfade that spans every warm-up and measured block.
    Crossfade,
    NonFlatEq,
}

impl Lane {
    fn fade(self) -> Fade {
        match self {
            Self::Settled | Self::NonFlatEq => Fade::Declick,
            Self::Crossfade => Fade::Crossfade(
                CrossfadeSettings::new(
                    consts::CROSSFADE_SECONDS,
                    CrossfadeCurve::EqualPower,
                    consts::CROSSFADE_DEPTH,
                    consts::CROSSFADE_POSITION,
                )
                .expect("bench crossfade settings"),
            ),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Settled => "settled",
            Self::Crossfade => "active crossfade",
            Self::NonFlatEq => "non-flat EQ",
        }
    }
}

struct Measurement {
    durations: Vec<Duration>,
    lane: Lane,
    peak: f32,
    tracks: usize,
}

fn non_zero(value: u32, label: &str) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap_or_else(|| panic!("bench {label} must be non-zero"))
}

fn block_frames() -> usize {
    usize::try_from(consts::BLOCK_FRAMES)
        .unwrap_or_else(|_| panic!("bench block frames exceed usize"))
}

fn block_budget() -> Duration {
    Duration::from_secs_f64(f64::from(consts::BLOCK_FRAMES) / f64::from(consts::SAMPLE_RATE))
}

fn spec() -> AudioSpec {
    AudioSpec::new(
        consts::CHANNELS,
        non_zero(consts::SAMPLE_RATE, "sample rate"),
    )
}

struct BenchInbox(ScopedInbox<DeckProtocol, DeckProtocol>);

impl SessionInbox for BenchInbox {
    fn scope(&mut self, id: ScopeId) -> Option<LevelInbox<'_, DeckProtocol>> {
        self.0.scope(id)
    }
}

struct BenchMixer {
    processor: DeckMixer<BenchInbox>,
    extra: ProcExtra,
    control: ScopedSender<DeckProtocol, DeckProtocol>,
    deck: DeckEnds,
    lanes: Vec<Box<dyn LaneTask + Send>>,
    _lane_senders: Vec<Sender<LaneProtocol>>,
    _sources: Vec<PcmDeck>,
    frame: i64,
}

#[kithara_test_utils::kithara::hang_watchdog]
async fn processor(count: usize, lane: Lane) -> BenchMixer {
    let config = DeckMixerConfig::default();
    let (mut control, inbox) = scoped_channel(
        ScopedConfig::builder()
            .scope(
                ChannelConfig::builder()
                    .targets(config.slots().get())
                    .build(),
            )
            .build(),
    );
    let scope = control
        .open(config.slots().get())
        .expect("bench deck scope");
    let (deck, inputs) = scope_channels(scope, config);
    let pools = pools();
    let shape = StreamShape {
        sample_rate: non_zero(consts::SAMPLE_RATE, "sample rate"),
        max_block_frames: non_zero(consts::BLOCK_FRAMES, "block frames"),
    };
    let processor = DeckMixer::new(inputs, shape, &pools).expect("bench mixer pools");
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let mut sources = Vec::new();
    let mut lanes = Vec::new();
    let mut senders = Vec::new();
    let mut commands = Vec::new();
    for index in 0..count {
        hang_tick!();
        let source = PcmDeck::new(Box::new(TestPcmReader::with_pcm(
            spec(),
            consts::TRACK_SECONDS,
            benchmark_half(),
        )));
        let TrackSource::Uri(path) = source.source() else {
            panic!("PCM deck must be a URI")
        };
        let audio = AudioConfig::<File<TestPools>>::for_stream(
            FileConfig::for_src(FileSrc::Local(path.into()))
                .store(memory_asset_store())
                .build(),
        )
        .hint("wav".to_owned())
        .build();
        let config = kithara::play::TrackConfig::for_audio(audio)
            .preload_chunks(NonZeroUsize::new(16).expect("preload chunks"))
            .audio_buffer_chunks(NonZeroUsize::new(32).expect("ring chunks"))
            .build();
        let (sender, inbox) = worker.lane_channel();
        let start = LaneStart {
            speed: SpeedCurve::Constant(1.0),
            keylock: false,
            backend: StretchKind::default(),
        };
        let (pcm, task, _) = {
            let _real_io = kithara::platform::flash::real_io();
            kithara::platform::time::timeout(
                __hang_detector.remaining(),
                worker.load(config, Duration::ZERO, start, inbox),
            )
            .await
            .unwrap_or_else(|_| {
                hang_tick!();
                panic!("bench PCM lane load exceeded its watchdog budget")
            })
            .expect("bench PCM lane")
        };
        hang_reset!();
        let slot = Slot::new(u16::try_from(index).expect("bench slot"));
        commands.push(DeckPart::Attach {
            item_id: kithara_events::TrackId::allocate(),
            slot,
            pcm: Box::new(
                PlayerResource::new(
                    PcmConsumer::new(pcm),
                    Arc::from(format!("bench-track-{index}")),
                    &pools,
                )
                .expect("bench player resource fits the pool budget"),
            ),
            segment: SegmentId::FIRST,
        });
        commands.push(DeckPart::Start {
            slot,
            fade: lane.fade(),
        });
        sources.push(source);
        lanes.push(Box::new(task) as Box<dyn LaneTask + Send>);
        senders.push(sender);
    }
    if lane == Lane::NonFlatEq {
        for band in 0..config.eq_bands() {
            commands.push(DeckPart::Eq(DeckEqChange::Gain {
                band,
                gain: eq_gain(band),
            }));
        }
    }
    control
        .scope(scope)
        .expect("bench scope")
        .send(
            When::Next,
            Batch {
                basis: Vec::new(),
                commands,
            },
        )
        .expect("bench deck channel capacity");
    control.publish().expect("publish bench commands");
    let mut store = ProcStore::with_capacity(2);
    assert!(
        store.insert(BenchInbox(inbox)).is_ok(),
        "install bench inbox"
    );
    install_render_context(&mut store).expect("install bench render context");
    let (logger, _logger_rx) = realtime_logger(RealtimeLoggerConfig::default());
    BenchMixer {
        processor,
        extra: ProcExtra {
            logger,
            store,
            scratch_buffers: ConstSequentialBuffer::<f32, NUM_SCRATCH_BUFFERS>::new(block_frames()),
            declick_values: DeclickValues::new(non_zero(16, "declick frames")),
        },
        control,
        deck,
        lanes,
        _lane_senders: senders,
        _sources: sources,
        frame: 0,
    }
}

fn render_block(mixer: &mut BenchMixer, out_l: &mut [f32], out_r: &mut [f32]) -> Duration {
    let mut context = Context::from_waker(Waker::noop());
    for lane in &mut mixer.lanes {
        let _ = lane.poll_commands(&mut context);
        lane.recycle();
        let _ = lane.tick();
    }
    mixer
        .extra
        .store
        .try_get_mut::<BenchInbox>()
        .expect("bench inbox")
        .0
        .drain();
    let frames = out_l.len();
    let end = mixer.frame + i64::try_from(frames).expect("bench frames fit i64");
    publish_render_context(
        &mut mixer.extra.store,
        RenderContext::new_linear(
            OutputContext::new(
                SessionFrame::new(mixer.frame)..SessionFrame::new(end),
                non_zero(consts::SAMPLE_RATE, "sample rate"),
                SessionEpoch::new(0),
                Some(TransportRevision::first()),
            )
            .expect("bench output range"),
            None,
        )
        .expect("bench linear context"),
    )
    .expect("bench context slot");
    let info = ProcInfo {
        sample_rate: non_zero(consts::SAMPLE_RATE, "sample rate"),
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
        last_marker_instant: InstantSamples(mixer.frame),
        sample_rate_recip: f64::from(consts::SAMPLE_RATE).recip(),
        clock_samples: InstantSamples(mixer.frame),
        duration_since_stream_start: Duration::ZERO,
        stream_status: StreamStatus::empty(),
        dropped_frames: 0,
    };
    let inputs: [&[f32]; 0] = [];
    let mut outputs = [out_l, out_r];
    let buffers = ProcBuffers {
        inputs: &inputs,
        outputs: &mut outputs,
    };
    let start = Instant::now();
    let outcome = mixer.processor.process(&info, buffers, &mut mixer.extra);
    let elapsed = start.elapsed();
    black_box(outcome);
    mixer.frame = end;
    while mixer.control.receipt().is_some() {}
    elapsed
}

fn peak_of(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()))
}

fn eq_gain(band: usize) -> GainDb {
    GainDb::from(if band == 0 { -6.0 } else { 3.0 })
}

fn assert_non_flat_eq(mixer: &mut BenchMixer) {
    let snapshot = mixer.deck.snapshot.read();
    assert!(snapshot.eq.bands() > 1, "the lane must run a crossover");
    for band in 0..snapshot.eq.bands() {
        assert_eq!(snapshot.eq.gain(band), Some(eq_gain(band)));
    }
}

/// Whether every live slot's envelope sits strictly between silence and
/// unity, so its last mixed frame went through the fade law.
fn every_slot_fading(mixer: &mut BenchMixer) -> bool {
    mixer
        .deck
        .snapshot
        .read()
        .slots
        .iter()
        .filter(|slot| slot.state != SlotState::Empty)
        .all(|slot| slot.gain > 0.0 && slot.gain < 1.0)
}

fn measure(tracks: usize, lane: Lane) -> Measurement {
    let runtime = tokio::runtime::Runtime::new().expect("bench runtime");
    let mut processor = runtime.block_on(processor(tracks, lane));
    let _runtime = runtime.enter();

    let frames = block_frames();
    let mut out_l = vec![0.0_f32; frames];
    let mut out_r = vec![0.0_f32; frames];

    for _ in 0..consts::WARMUP_BLOCKS {
        render_block(&mut processor, &mut out_l, &mut out_r);
    }
    if lane == Lane::NonFlatEq {
        assert_non_flat_eq(&mut processor);
        let settled = out_l.clone();
        render_block(&mut processor, &mut out_l, &mut out_r);
        assert_eq!(out_l, settled, "measure only the settled non-flat EQ");
    }

    let fading_before = every_slot_fading(&mut processor);
    let before = processor.deck.snapshot.read().metrics;
    let mut durations = Vec::with_capacity(consts::MEASURED_BLOCKS);
    let mut peak = 0.0_f32;
    for _ in 0..consts::MEASURED_BLOCKS {
        durations.push(render_block(&mut processor, &mut out_l, &mut out_r));
        peak = peak.max(peak_of(&out_l));
        black_box(&out_l);
    }
    let after = processor.deck.snapshot.read().metrics;
    if lane == Lane::NonFlatEq {
        assert_non_flat_eq(&mut processor);
    }
    if lane == Lane::Crossfade {
        assert!(
            fading_before && every_slot_fading(&mut processor),
            "the crossfade must span every measured block"
        );
    }

    let live = processor
        .deck
        .snapshot
        .read()
        .slots
        .iter()
        .filter(|slot| slot.state != SlotState::Empty)
        .count();
    assert_eq!(
        live, tracks,
        "bench measured {live} track(s), not the {tracks} it loaded"
    );
    assert_eq!(
        after.underruns(),
        before.underruns(),
        "an underrun renders zero-fill, so the timing would be of silence"
    );
    assert_eq!(
        after.decode_errors(),
        before.decode_errors(),
        "a decode error skips the mix, so the timing would not be of a mix"
    );
    assert!(peak > 0.0, "measured blocks must carry audio, not silence");

    durations.sort_unstable();
    Measurement {
        durations,
        lane,
        peak,
        tracks,
    }
}

impl Measurement {
    fn percentile(&self, pct: usize) -> Duration {
        let rank = (self.durations.len() * pct).div_ceil(100);
        let idx = rank.saturating_sub(1).min(self.durations.len() - 1);
        self.durations[idx]
    }

    fn max(&self) -> Duration {
        self.percentile(100)
    }
}

fn cell(elapsed: Duration, budget: Duration) -> String {
    let micros = elapsed.as_secs_f64() * 1e6;
    let share = elapsed.as_secs_f64() / budget.as_secs_f64() * 1e2;
    format!("{micros:>7.2} us / {share:>5.2}%")
}

fn main() {
    let budget = block_budget();
    println!(
        "DeckMixer block budget: {:.3} ms ({} frames @ {} Hz, {} ch)",
        budget.as_secs_f64() * 1e3,
        consts::BLOCK_FRAMES,
        consts::SAMPLE_RATE,
        consts::CHANNELS,
    );
    println!(
        "{} blocks measured per lane after {} warm-up blocks",
        consts::MEASURED_BLOCKS,
        consts::WARMUP_BLOCKS,
    );
    println!(
        "{:>16}  {:>7}  {:>22}  {:>22}  {:>22}",
        "lane", "tracks", "p50", "p99", "max"
    );

    for (lane, count) in [Lane::Settled, Lane::Crossfade, Lane::NonFlatEq]
        .into_iter()
        .flat_map(|lane| consts::TRACK_COUNTS.map(|count| (lane, count)))
    {
        let measurement = measure(count, lane);
        println!(
            "{:>16}  {:>7}  {:>22}  {:>22}  {:>22}",
            measurement.lane.label(),
            measurement.tracks,
            cell(measurement.percentile(50), budget),
            cell(measurement.percentile(99), budget),
            cell(measurement.max(), budget),
        );
        black_box(measurement.peak);
    }
}
